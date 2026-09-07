//! A real client, over a real socket, speaking the real protocol.
//!
//! Everything else in this crate tests bytes in isolation. This drives an actual TCP
//! connection through the whole handshake and out the other side, because the failures that
//! matter most in a protocol are *sequencing* failures — a message that is individually
//! correct and arrives in the wrong place.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_api_pg::catalog::{CatalogColumn, CatalogTable};
use sankhya_api_pg::listener::PgListener;
use sankhya_api_pg::message::{oid, FieldDescription, PROTOCOL_VERSION};
use sankhya_api_pg::session::{Caller, Handler, QueryFailure, QueryResult};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A handler that answers one query and refuses one password.
#[derive(Debug)]
struct Fixture {
    password: Option<&'static str>,
    /// A statement this handler defines itself, which the catalogue must not answer for it.
    claims: Option<&'static str>,
}

impl Fixture {
    /// The ordinary fixture: no password, and no statements of its own.
    fn open() -> Self {
        Self { password: None, claims: None }
    }
}

impl Handler for Fixture {
    fn claims(&self, sql: &str) -> bool {
        self.claims.is_some_and(|claimed| sql.trim() == claimed)
    }

    fn requires_password(&self, _parameters: &[(String, String)]) -> bool {
        self.password.is_some()
    }

    fn authenticate(
        &self,
        _parameters: &[(String, String)],
        password: Option<&[u8]>,
    ) -> Result<(), QueryFailure> {
        match (self.password, password) {
            (None, _) => Ok(()),
            (Some(expected), Some(offered)) if offered == expected.as_bytes() => Ok(()),
            _ => Err(QueryFailure {
                // The SQLSTATE a driver branches on to decide whether to prompt again.
                sqlstate: "28P01".to_string(),
                message: "password authentication failed".to_string(),
                detail: None,
                subjects: Vec::new(),
            }),
        }
    }

    fn query(&self, sql: &str, caller: &Caller<'_>) -> Result<QueryResult, QueryFailure> {
        // What the *session* is holding, so a test can assert that a `SET` took effect rather
        // than only that it was acknowledged. Without this the two are indistinguishable from
        // outside, which is how `CLI-06` lasted: the extended protocol answered `SET SNAPSHOT`
        // with a success tag and never recorded it.
        //
        // Not spelled `SHOW`: the catalogue recognises those and answers them before the
        // handler is asked, so the probe would report the catalogue's idea of a setting rather
        // than the session's.
        if let Some(name) = sql.strip_prefix("peek setting ") {
            return Ok(QueryResult {
                fields: vec![FieldDescription::text("value", oid::TEXT, -1)],
                rows: vec![vec![caller.setting(name.trim()).map(ToOwned::to_owned)]],
                tag: "SELECT 1".to_string(),
            });
        }
        // The statement is echoed back as a row, so a test can assert **what reached the
        // handler**. Without it, a bound parameter that never arrived is invisible: the
        // fixture answers the same two rows either way, and the test passes while the
        // parameter is dropped on the floor.
        if sql.contains("echo") {
            return Ok(QueryResult {
                fields: vec![FieldDescription::text("sql", oid::TEXT, -1)],
                rows: vec![vec![Some(sql.to_string())]],
                tag: "SELECT 1".to_string(),
            });
        }
        if sql.contains("boom") {
            return Err(QueryFailure {
                sqlstate: "42601".to_string(),
                message: "syntax error at or near \"boom\"".to_string(),
                detail: None,
                subjects: Vec::new(),
            });
        }
        Ok(QueryResult {
            fields: vec![
                FieldDescription::text("id", oid::INT8, 8),
                FieldDescription::text("note", oid::TEXT, -1),
            ],
            rows: vec![
                vec![Some("1".to_string()), Some("first".to_string())],
                vec![Some("2".to_string()), None],
            ],
            tag: "SELECT 2".to_string(),
        })
    }

    fn visible_tables(&self, _caller: &Caller<'_>) -> Vec<CatalogTable> {
        vec![CatalogTable {
            schema: "sales".to_string(),
            name: "orders".to_string(),
            columns: vec![CatalogColumn {
                name: "id".to_string(),
                type_name: "int8".to_string(),
                type_oid: oid::INT8,
                nullable: false,
            }],
        }]
    }
}

/// A minimal client: enough protocol to prove the server speaks it.
struct Client {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl Client {
    async fn connect(address: std::net::SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(address).await.expect("connecting"),
            buffer: Vec::new(),
        }
    }

    async fn startup(&mut self, user: &str) {
        let mut body = Vec::new();
        for (key, value) in [("user", user), ("database", "acme")] {
            body.extend_from_slice(key.as_bytes());
            body.push(0);
            body.extend_from_slice(value.as_bytes());
            body.push(0);
        }
        body.push(0);

        let mut packet = Vec::new();
        packet.extend_from_slice(&i32::try_from(body.len() + 8).unwrap_or(0).to_be_bytes());
        packet.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        packet.extend_from_slice(&body);
        self.send_raw(&packet).await;
    }

    async fn send(&mut self, tag: u8, body: &[u8]) {
        let mut packet = vec![tag];
        packet.extend_from_slice(&i32::try_from(body.len() + 4).unwrap_or(0).to_be_bytes());
        packet.extend_from_slice(body);
        self.send_raw(&packet).await;
    }

    async fn send_raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.expect("writing");
        self.stream.flush().await.expect("flushing");
    }

    async fn query(&mut self, sql: &str) {
        let mut body = sql.as_bytes().to_vec();
        body.push(0);
        self.send(b'Q', &body).await;
    }

    /// Read until a message with `tag` arrives, returning every message seen.
    ///
    /// Reading to a marker rather than a fixed count, because the number of messages the
    /// server sends is not something a client should have to predict.
    /// Read messages until one carries `tag`, the connection closes, or the deadline passes.
    ///
    /// # Why there is a deadline
    ///
    /// There was not, and it cost twenty minutes to find out. A mutation that removed the
    /// contract check meant no `ErrorResponse` ever arrived, and a test waiting for one
    /// **hung** --- so the mutation audit stalled instead of reporting a survivor, and the
    /// build would have stalled with it.
    ///
    /// A hang is strictly worse than a failure: it takes the run with it and reports nothing.
    /// This repository has been bitten by exactly this three times before, which is why its
    /// rule is that every wait goes through one bounded helper rather than a timeout somebody
    /// has to remember at each call site.
    async fn read_until(&mut self, tag: u8) -> Vec<(u8, Vec<u8>)> {
        // Generous: the server does no I/O to answer any of these, so anything approaching
        // this is a hang rather than a slow machine.
        const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
        let until = std::time::Instant::now() + DEADLINE;
        let mut messages = Vec::new();
        let mut chunk = vec![0u8; 4096];
        loop {
            // Drain whatever is already buffered before reading more.
            while self.buffer.len() >= 5 {
                let kind = self.buffer[0];
                let length = i32::from_be_bytes([
                    self.buffer[1],
                    self.buffer[2],
                    self.buffer[3],
                    self.buffer[4],
                ]);
                let total = usize::try_from(length).unwrap_or(0) + 1;
                if self.buffer.len() < total {
                    break;
                }
                let body = self.buffer[5..total].to_vec();
                self.buffer.drain(..total);
                messages.push((kind, body));
                if kind == tag {
                    return messages;
                }
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            assert!(
                !left.is_zero(),
                "no `{}` arrived within {}s; the messages seen were {:?}",
                tag as char,
                DEADLINE.as_secs(),
                messages.iter().map(|(kind, _)| *kind as char).collect::<Vec<char>>()
            );
            let read = match tokio::time::timeout(left, self.stream.read(&mut chunk)).await {
                Ok(read) => read.expect("reading"),
                Err(_) => panic!(
                    "no `{}` arrived within {}s; the messages seen were {:?}",
                    tag as char,
                    DEADLINE.as_secs(),
                    messages.iter().map(|(kind, _)| *kind as char).collect::<Vec<char>>()
                ),
            };
            if read == 0 {
                return messages;
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

/// Start a server on an ephemeral port and return its address.
async fn start(handler: Fixture) -> std::net::SocketAddr {
    let listener = PgListener::bind("127.0.0.1:0").await.expect("binding");
    let address = listener.local_addr().expect("an address");
    let handler: Arc<dyn Handler> = Arc::new(handler);
    tokio::spawn(async move {
        // One connection per test is enough, and serving exactly one means the task ends
        // rather than leaking.
        listener.accept_one(handler).await.ok();
    });
    address
}

fn tags(messages: &[(u8, Vec<u8>)]) -> Vec<char> {
    messages.iter().map(|(tag, _)| *tag as char).collect()
}

#[tokio::test]
async fn a_client_connects_authenticates_and_runs_a_query() {
    // The whole handshake, over a real socket. Individually-correct messages arriving in
    // the wrong order is the failure mode that only an end-to-end test finds.
    let address = start(Fixture { password: Some("hunter2"), claims: None }).await;
    let mut client = Client::connect(address).await;

    client.startup("ana").await;
    let auth = client.read_until(b'R').await;
    assert_eq!(tags(&auth), vec!['R'], "the server asks for a password");

    let mut password = b"hunter2".to_vec();
    password.push(0);
    client.send(b'p', &password).await;

    let ready = client.read_until(b'Z').await;
    let seen = tags(&ready);
    assert!(seen.contains(&'R'), "authentication ok: {seen:?}");
    assert!(seen.contains(&'S'), "parameter status: {seen:?}");
    assert!(seen.contains(&'K'), "backend key data: {seen:?}");
    assert_eq!(seen.last(), Some(&'Z'), "ready for query comes last");

    client.query("SELECT id, note FROM t").await;
    let result = client.read_until(b'Z').await;
    let seen = tags(&result);
    assert_eq!(
        seen,
        vec!['T', 'D', 'D', 'C', 'Z'],
        "description, two rows, completion, ready"
    );

    let (_, complete) = result.iter().find(|(t, _)| *t == b'C').expect("a tag");
    assert!(String::from_utf8_lossy(complete).starts_with("SELECT 2"));
}

#[tokio::test]
async fn a_null_survives_the_round_trip_as_a_null() {
    // The second fixture row has a null. A client that receives an empty string instead has
    // been told something false about the data.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    client.query("SELECT id, note FROM t").await;
    let result = client.read_until(b'Z').await;
    let rows: Vec<&Vec<u8>> = result
        .iter()
        .filter(|(t, _)| *t == b'D')
        .map(|(_, body)| body)
        .collect();
    assert_eq!(rows.len(), 2);

    // The second row's second value: after the column count (2), the first value's length
    // and bytes, then the second value's length.
    let second = rows.get(1).expect("a second row");
    let first_length = i32::from_be_bytes([second[2], second[3], second[4], second[5]]);
    let at = 6 + usize::try_from(first_length).unwrap_or(0);
    let second_length =
        i32::from_be_bytes([second[at], second[at + 1], second[at + 2], second[at + 3]]);
    assert_eq!(second_length, -1, "a null must arrive as -1, not as 0");
}

#[tokio::test]
async fn a_query_before_authentication_is_refused() {
    // The whole point of authentication. The state machine makes this structural rather
    // than a check somebody remembered to write.
    let address = start(Fixture { password: Some("hunter2"), claims: None }).await;
    let mut client = Client::connect(address).await;

    client.startup("ana").await;
    client.read_until(b'R').await;
    client.query("SELECT 1").await;

    let response = client.read_until(b'E').await;
    let (_, body) = response.iter().find(|(t, _)| *t == b'E').expect("an error");
    let text = String::from_utf8_lossy(body);
    assert!(text.contains("Authenticating"), "{text}");
}

#[tokio::test]
async fn a_wrong_password_is_refused_with_the_sqlstate_drivers_branch_on() {
    let address = start(Fixture { password: Some("hunter2"), claims: None }).await;
    let mut client = Client::connect(address).await;

    client.startup("ana").await;
    client.read_until(b'R').await;
    let mut wrong = b"letmein".to_vec();
    wrong.push(0);
    client.send(b'p', &wrong).await;

    let response = client.read_until(b'E').await;
    let (_, body) = response.iter().find(|(t, _)| *t == b'E').expect("an error");
    let text = String::from_utf8_lossy(body);
    assert!(
        text.contains("28P01"),
        "the SQLSTATE a driver checks: {text}"
    );
    assert!(text.contains("password authentication failed"));
}

#[tokio::test]
async fn a_catalogue_query_is_answered_without_reaching_the_engine() {
    // These refer to tables the engine does not have, so passing them through would
    // produce "no such table" for a query the client considers routine.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    client.query("SELECT version()").await;
    let result = client.read_until(b'Z').await;
    let (_, row) = result.iter().find(|(t, _)| *t == b'D').expect("a row");
    let text = String::from_utf8_lossy(row);
    assert!(text.contains("PostgreSQL 17.0"), "{text}");
    assert!(text.contains("SANKHYA"), "{text}");
}

#[tokio::test]
async fn a_failing_query_leaves_the_connection_usable() {
    // An error is not a disconnection. A client that has to reconnect after every mistyped
    // query is one nobody can use interactively.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    client.query("SELECT boom").await;
    let failed = client.read_until(b'Z').await;
    assert!(tags(&failed).contains(&'E'));
    assert_eq!(tags(&failed).last(), Some(&'Z'), "and still ready");

    client.query("SELECT id, note FROM t").await;
    let recovered = client.read_until(b'Z').await;
    assert!(
        tags(&recovered).contains(&'T'),
        "the next query works: {:?}",
        tags(&recovered)
    );
}

#[tokio::test]
async fn a_tls_request_is_declined_and_the_client_may_continue() {
    // 'N' means "no TLS available, carry on in the clear". Every client knows how to
    // proceed after it; a silent close looks like a crash.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;

    let mut packet = Vec::new();
    packet.extend_from_slice(&8i32.to_be_bytes());
    packet.extend_from_slice(&80_877_103i32.to_be_bytes());
    client.send_raw(&packet).await;

    let mut one = [0u8; 1];
    client.stream.read_exact(&mut one).await.expect("a reply");
    assert_eq!(one[0], b'N');

    // And the connection is still usable for the real startup packet.
    client.startup("ana").await;
    let ready = client.read_until(b'Z').await;
    assert_eq!(tags(&ready).last(), Some(&'Z'));
}

#[tokio::test]
async fn an_empty_query_gets_the_empty_response_rather_than_an_error() {
    // Clients send these — a trailing semicolon, a comment-only statement. Treating one as
    // an error makes a script fail on a blank line.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    client.query("").await;
    let result = client.read_until(b'Z').await;
    assert!(tags(&result).contains(&'I'), "{:?}", tags(&result));
    assert!(!tags(&result).contains(&'E'));
}

#[tokio::test]
async fn a_message_split_across_two_writes_is_reassembled() {
    // A network splits wherever it likes. A server that assumed one read equals one message
    // works on a loopback and fails in production.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    let sql = "SELECT id, note FROM t";
    let mut body = sql.as_bytes().to_vec();
    body.push(0);
    let mut packet = vec![b'Q'];
    packet.extend_from_slice(&i32::try_from(body.len() + 4).unwrap_or(0).to_be_bytes());
    packet.extend_from_slice(&body);

    let (head, tail) = packet.split_at(3);
    client.send_raw(head).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    client.send_raw(tail).await;

    let result = client.read_until(b'Z').await;
    assert!(tags(&result).contains(&'T'), "{:?}", tags(&result));
}

#[tokio::test]
async fn two_pipelined_queries_are_both_answered() {
    // A client may send without waiting. A server that read one message per read would
    // leave the second sitting in its buffer forever.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    let mut both = Vec::new();
    for sql in ["SELECT id, note FROM t", "SELECT id, note FROM t"] {
        let mut body = sql.as_bytes().to_vec();
        body.push(0);
        both.push(b'Q');
        both.extend_from_slice(&i32::try_from(body.len() + 4).unwrap_or(0).to_be_bytes());
        both.extend_from_slice(&body);
    }
    client.send_raw(&both).await;

    let first = client.read_until(b'Z').await;
    assert!(tags(&first).contains(&'T'));
    let second = client.read_until(b'Z').await;
    assert!(tags(&second).contains(&'T'), "the second was not answered");
}

#[tokio::test]
async fn a_terminate_closes_the_connection() {
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    client.send(b'X', &[]).await;
    let mut chunk = [0u8; 16];
    let read = client.stream.read(&mut chunk).await.expect("reading");
    assert_eq!(read, 0, "the server closes without further messages");
}

#[tokio::test]
async fn a_statement_the_handler_claims_is_not_answered_from_the_catalogue() {
    // The defect this exists for: `recognise` answers `SHOW <anything>` as a session setting,
    // because tools spell settings queries a dozen ways and matching them literally works for
    // one client and breaks the next. That leniency swallowed `SHOW FEEDS` — a statement the
    // *server* defines — and answered it with one empty value.
    //
    // Nothing below this layer could see it. The handler's own tests call the handler, so the
    // surface worked everywhere except over a socket, which is the only place it is used.
    let address = start(Fixture { password: None, claims: Some("SHOW FEEDS") }).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    client.query("SHOW FEEDS").await;
    let result = client.read_until(b'Z').await;
    let (_, row) = result.iter().find(|(t, _)| *t == b'D').expect("a row");
    // The fixture's own two-column answer, not the catalogue's one empty setting.
    assert!(
        String::from_utf8_lossy(row).contains("first"),
        "the catalogue answered a statement the handler claimed: {:?}",
        String::from_utf8_lossy(row)
    );
}

#[tokio::test]
async fn a_setting_the_handler_does_not_claim_is_still_answered_from_the_catalogue() {
    // The other half, and the reason the fix is a claim rather than a narrower prefix match.
    // `SHOW server_version_num` is sent by catalogue-browsing clients on connection and must
    // keep being answered here — a handler that claims one `SHOW` has not claimed them all.
    let address = start(Fixture { password: None, claims: Some("SHOW FEEDS") }).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    client.query("SHOW server_version_num").await;
    let result = client.read_until(b'Z').await;
    let (_, row) = result.iter().find(|(t, _)| *t == b'D').expect("a row");
    assert!(
        !String::from_utf8_lossy(row).contains("first"),
        "the handler was asked about a setting it did not claim"
    );
}

#[tokio::test]
async fn a_parsed_and_bound_statement_runs_and_returns_its_rows() {
    // The defect this exists for, and the worst one for real clients: `Parse` was acked and
    // its SQL **discarded**, `Bind` was acked, `Describe` answered `NoData`, and `Execute` had
    // no arm at all --- so it fell to the out-of-phase catch-all, which refuses with `08P01`
    // and closes the connection.
    //
    // Three cheerful acknowledgements and then a dead socket. Every mainstream driver ---
    // JDBC, psycopg, pgx, npgsql, ODBC --- uses this path by default, against a door whose
    // whole purpose is that ordinary PostgreSQL clients work.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    // Parse (unnamed), Bind (no parameters), Describe the portal, Execute, Sync.
    let mut body = b"\0".to_vec();
    body.extend_from_slice(b"SELECT id, note FROM t\0");
    body.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'P', &body).await;

    let mut bind = b"\0\0".to_vec();
    bind.extend_from_slice(&0i16.to_be_bytes()); // no format codes
    bind.extend_from_slice(&0i16.to_be_bytes()); // no parameters
    bind.extend_from_slice(&0i16.to_be_bytes()); // no result formats
    client.send(b'B', &bind).await;

    client.send(b'D', b"P\0").await;
    let mut execute = b"\0".to_vec();
    execute.extend_from_slice(&0i32.to_be_bytes());
    client.send(b'E', &execute).await;
    client.send(b'S', &[]).await;

    let result = client.read_until(b'Z').await;
    let seen = tags(&result);
    assert!(seen.contains(&'1'), "no ParseComplete: {seen:?}");
    assert!(seen.contains(&'2'), "no BindComplete: {seen:?}");
    assert!(seen.contains(&'T'), "no RowDescription: {seen:?}");
    assert_eq!(
        result.iter().filter(|(tag, _)| *tag == b'D').count(),
        2,
        "the fixture's two rows did not arrive: {seen:?}"
    );
    assert!(seen.contains(&'C'), "no CommandComplete: {seen:?}");
    assert!(!seen.contains(&'E'), "it was refused: {seen:?}");

    // And the connection is still usable, which the old behaviour destroyed.
    client.query("SELECT id, note FROM t").await;
    assert!(tags(&client.read_until(b'Z').await).contains(&'T'));
}

#[tokio::test]
async fn a_bound_parameter_reaches_the_statement() {
    // `Bind`'s parameter values were decoded and thrown away, which was survivable only while
    // nothing ever ran a portal. The moment `Execute` did anything, a parameterised query
    // would have run without its parameters --- silently, and with a plausible answer.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    let mut body = b"\0".to_vec();
    body.extend_from_slice(b"SELECT echo FROM t WHERE note = $1\0");
    body.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'P', &body).await;

    let mut bind = b"\0\0".to_vec();
    bind.extend_from_slice(&0i16.to_be_bytes());
    bind.extend_from_slice(&1i16.to_be_bytes());
    bind.extend_from_slice(&5i32.to_be_bytes());
    bind.extend_from_slice(b"first");
    bind.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'B', &bind).await;

    let mut execute = b"\0".to_vec();
    execute.extend_from_slice(&0i32.to_be_bytes());
    client.send(b'E', &execute).await;
    client.send(b'S', &[]).await;

    let result = client.read_until(b'Z').await;
    let (_, row) = result.iter().find(|(tag, _)| *tag == b'D').expect("a row");
    let echoed = String::from_utf8_lossy(row);
    assert!(
        echoed.contains("'first'"),
        "the bound parameter never reached the handler: {echoed}"
    );
    assert!(
        !echoed.contains("$1"),
        "the placeholder was sent unsubstituted: {echoed}"
    );
}

#[tokio::test]
async fn a_null_parameter_binds_as_null_and_not_as_the_empty_string() {
    // Two different values. A parameter that is absent and one that is the empty string mean
    // different things, and a binding that conflated them would produce a wrong answer rather
    // than a formatting slip.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    let mut body = b"\0".to_vec();
    body.extend_from_slice(b"SELECT echo FROM t WHERE note = $1\0");
    body.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'P', &body).await;

    let mut bind = b"\0\0".to_vec();
    bind.extend_from_slice(&0i16.to_be_bytes());
    bind.extend_from_slice(&1i16.to_be_bytes());
    bind.extend_from_slice(&(-1i32).to_be_bytes()); // SQL NULL
    bind.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'B', &bind).await;

    let mut execute = b"\0".to_vec();
    execute.extend_from_slice(&0i32.to_be_bytes());
    client.send(b'E', &execute).await;
    client.send(b'S', &[]).await;

    let result = client.read_until(b'Z').await;
    let (_, row) = result.iter().find(|(tag, _)| *tag == b'D').expect("a row");
    let echoed = String::from_utf8_lossy(row);
    assert!(echoed.contains("NULL"), "a null bound as something else: {echoed}");
    assert!(!echoed.contains("''"), "a null bound as the empty string: {echoed}");
}

#[tokio::test]
async fn executing_a_portal_nobody_bound_is_refused_without_closing_the_connection() {
    // A statement that fails is not a protocol violation. The old behaviour treated every
    // extended-protocol message as out-of-phase and closed the socket, so one mistake cost the
    // connection --- and a client that must reconnect after every mistake is unusable.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    let mut execute = b"nosuch\0".to_vec();
    execute.extend_from_slice(&0i32.to_be_bytes());
    client.send(b'E', &execute).await;
    client.send(b'S', &[]).await;

    let result = client.read_until(b'Z').await;
    assert!(tags(&result).contains(&'E'), "it was not refused: {:?}", tags(&result));

    client.query("SELECT id, note FROM t").await;
    assert!(
        tags(&client.read_until(b'Z').await).contains(&'T'),
        "the connection did not survive the refusal"
    );
}

#[tokio::test]
async fn a_client_speaking_a_contract_this_server_does_not_is_refused_at_connection() {
    // `ADR-0017` Decision 5. A binding is installed independently of the server --- a package
    // index, a container image and a deployment all move at their own pace --- so the two will
    // disagree, and the only question is where.
    //
    // Unchecked, it surfaces eleven calls later as a field that is missing. Checked here, it
    // surfaces where somebody can act, naming both versions.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;

    let mut body = 196_608i32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0ana\0");
    body.extend_from_slice(b"sankhya_contract\0999\0\0");
    let mut startup = Vec::new();
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    client.send_raw(&startup).await;

    let result = client.read_until(b'E').await;
    let (_, said) = result.iter().find(|(tag, _)| *tag == b'E').expect("a refusal");
    let text = String::from_utf8_lossy(said);
    assert!(text.contains("08004"), "the code for a declined connection: {text}");
    assert!(text.contains("999"), "the refusal names the client's version: {text}");
    assert!(text.contains("client=999"), "and carries both as data: {text}");
    assert!(text.contains("server=1"), "{text}");
}

#[tokio::test]
async fn a_client_that_claims_no_contract_is_served_as_a_generic_driver() {
    // The other half, and the one that matters more often. `psql`, JDBC and every ordinary
    // PostgreSQL driver send no contract version because they have none --- refusing them
    // would refuse the whole ecosystem this door exists for.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;

    let result = client.read_until(b'Z').await;
    assert!(
        !tags(&result).contains(&'E'),
        "a client with no contract was refused: {:?}",
        tags(&result)
    );
}

#[tokio::test]
async fn a_client_speaking_this_server_s_contract_is_served() {
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;

    let mut body = 196_608i32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0ana\0");
    body.extend_from_slice(b"sankhya_contract\01\0\0");
    let mut startup = Vec::new();
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    client.send_raw(&startup).await;

    let result = client.read_until(b'Z').await;
    assert!(!tags(&result).contains(&'E'), "{:?}", tags(&result));

    // And the server announced its own, so a client can tell what it is talking to before it
    // asks for anything.
    let announced: String = result
        .iter()
        .filter(|(tag, _)| *tag == b'S')
        .map(|(_, body)| String::from_utf8_lossy(body).into_owned())
        .collect();
    assert!(announced.contains("sankhya_contract"), "{announced}");
}

#[tokio::test]
async fn a_contract_that_is_not_a_number_is_refused_rather_than_ignored() {
    // Ignoring it would serve a client whose declaration nobody read --- which is the same as
    // having no handshake, with the appearance of one.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;

    let mut body = 196_608i32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0ana\0");
    body.extend_from_slice(b"sankhya_contract\0soon\0\0");
    let mut startup = Vec::new();
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    client.send_raw(&startup).await;

    let result = client.read_until(b'E').await;
    let (_, said) = result.iter().find(|(tag, _)| *tag == b'E').expect("a refusal");
    assert!(String::from_utf8_lossy(said).contains("08004"));
}

/// Run one statement through Parse/Bind/Execute, the way every mainstream driver does.
async fn extended(client: &mut Client, sql: &str) -> Vec<(u8, Vec<u8>)> {
    let mut body = b"\0".to_vec();
    body.extend_from_slice(sql.as_bytes());
    body.push(0);
    body.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'P', &body).await;

    let mut bind = b"\0\0".to_vec();
    bind.extend_from_slice(&0i16.to_be_bytes());
    bind.extend_from_slice(&0i16.to_be_bytes());
    bind.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'B', &bind).await;

    let mut execute = b"\0".to_vec();
    execute.extend_from_slice(&0i32.to_be_bytes());
    client.send(b'E', &execute).await;
    client.send(b'S', &[]).await;
    client.read_until(b'Z').await
}

/// The single text value a `DataRow` carries, or `None` for a null.
fn only_value(messages: &[(u8, Vec<u8>)]) -> Option<String> {
    let (_, body) = messages.iter().find(|(tag, _)| *tag == b'D')?;
    // Columns (2 bytes), then the first column's length (4) and its bytes.
    let length = i32::from_be_bytes([body[2], body[3], body[4], body[5]]);
    if length < 0 {
        return None;
    }
    let end = 6 + usize::try_from(length).unwrap_or(0);
    Some(String::from_utf8_lossy(&body[6..end]).into_owned())
}

#[tokio::test]
async fn a_setting_changed_on_the_extended_protocol_takes_effect() {
    // `CLI-06`. `remember_setting` was called only from the simple-`Query` arm, so a client
    // using Parse/Bind/Execute got a success tag, the handler validated the value, and every
    // subsequent statement in that session ran as though nothing had been set.
    //
    // pgjdbc, psycopg3, asyncpg and SQLAlchemy all use the extended protocol by default, so
    // this was the path almost every real client takes — and there is no symptom: the reply is
    // a `CommandComplete` either way.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    let acknowledged = extended(&mut client, "SET SNAPSHOT = 'eod'").await;
    assert!(
        tags(&acknowledged).contains(&'C'),
        "the SET was not acknowledged at all: {:?}",
        tags(&acknowledged)
    );

    // The assertion the acknowledgement cannot make. Asked through the *simple* protocol, so
    // a failure here is about what the session remembers rather than about how it is read.
    client.query("peek setting snapshot").await;
    let answered = client.read_until(b'Z').await;
    assert_eq!(
        only_value(&answered).as_deref(),
        Some("eod"),
        "the session did not remember a setting the extended protocol acknowledged"
    );
}

#[tokio::test]
async fn a_setting_written_without_spaces_is_read_as_a_setting() {
    // `CLI-07`. Both the protocol layer and the server took the second whitespace-delimited
    // word as the setting's name, so `SET SNAPSHOT='eod'` named a setting called
    // `snapshot='eod'` with an empty value — and fell through to the arm that accepts any
    // `SET` as a no-op. No validation, no effect, no symptom.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    client.query("SET SNAPSHOT='eod'").await;
    client.read_until(b'Z').await;

    client.query("peek setting snapshot").await;
    let answered = client.read_until(b'Z').await;
    assert_eq!(
        only_value(&answered).as_deref(),
        Some("eod"),
        "a setting written without spaces was acknowledged and did nothing"
    );

    // And the version form, which broke one token further along: the table name came out as
    // `sales.orders=2`, so every table would have been the same setting.
    client.query("SET VERSION OF sales.orders=2").await;
    client.read_until(b'Z').await;
    client.query("peek setting version of sales.orders").await;
    let answered = client.read_until(b'Z').await;
    assert_eq!(only_value(&answered).as_deref(), Some("2"));
}

#[tokio::test]
async fn a_refused_setting_leaves_the_session_unchanged_on_the_extended_protocol() {
    // The rule the simple path already held, now that the extended path records anything at
    // all: storing before asking is how a refusal comes to have taken effect.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    let refused = extended(&mut client, "SET SNAPSHOT = 'boom'").await;
    assert!(tags(&refused).contains(&'E'), "the fixture must refuse this: {:?}", tags(&refused));

    client.query("peek setting snapshot").await;
    let answered = client.read_until(b'Z').await;
    assert_eq!(
        only_value(&answered),
        None,
        "a setting the handler refused was remembered anyway"
    );
}

#[tokio::test]
async fn the_catalogue_split_holds_on_the_extended_protocol_too() {
    // The same decision as the two tests above, at the path almost every real driver takes.
    //
    // `run` and `answer_now` each make it, and both were covered by one entry naming text that
    // occurs in both — so whichever came first in the file was mutated and the other was tested
    // by nothing. Every driver that binds parameters — pgjdbc, psycopg3, asyncpg, SQLAlchemy —
    // reaches the second one and not the first.
    let address = start(Fixture { password: None, claims: Some("SHOW FEEDS") }).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    // The whole row rather than its first column: the handler's answer is two columns and the
    // word that identifies it is in the second.
    let whole = |messages: &[(u8, Vec<u8>)]| {
        messages
            .iter()
            .find(|(tag, _)| *tag == b'D')
            .map(|(_, body)| String::from_utf8_lossy(body).into_owned())
            .unwrap_or_default()
    };

    // Claimed by the handler: the catalogue must not answer it.
    let claimed = whole(&extended(&mut client, "SHOW FEEDS").await);
    assert!(
        claimed.contains("first"),
        "the catalogue answered a statement the handler claimed: {claimed:?}"
    );

    // Not claimed: the catalogue must still answer it, or a client browsing on connection gets
    // "no such table" for a query it considers routine.
    let ordinary = whole(&extended(&mut client, "SHOW server_version_num").await);
    assert!(
        !ordinary.contains("first"),
        "the handler was asked a statement the catalogue owns: {ordinary:?}"
    );
    assert!(!ordinary.is_empty(), "the catalogue answered nothing");
}

#[tokio::test]
async fn a_suspended_portal_resumes_where_it_stopped() {
    // A portal is a cursor, and it had no position.
    //
    // `Execute` sent the first `max_rows` rows and answered `PortalSuspended`; the next
    // `Execute` sent **the same rows again**, and said `PortalSuspended` again. A client
    // streaming a large result with a fetch size --- `setFetchSize` in JDBC, a server-side
    // cursor in psycopg --- looped forever over its first page. Nothing errored, and every
    // individual message was correct.
    let address = start(Fixture::open()).await;
    let mut client = Client::connect(address).await;
    client.startup("ana").await;
    client.read_until(b'Z').await;

    let mut parse = b"\0".to_vec();
    parse.extend_from_slice(b"SELECT id, note FROM t\0");
    parse.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'P', &parse).await;

    let mut bind = b"\0\0".to_vec();
    bind.extend_from_slice(&0i16.to_be_bytes());
    bind.extend_from_slice(&0i16.to_be_bytes());
    bind.extend_from_slice(&0i16.to_be_bytes());
    client.send(b'B', &bind).await;

    // One row at a time, over a result of two.
    let mut fetch = || {
        let mut execute = b"\0".to_vec();
        execute.extend_from_slice(&1i32.to_be_bytes());
        execute
    };

    client.send(b'E', &fetch()).await;
    client.send(b'S', &[]).await;
    let first = client.read_until(b'Z').await;
    let first_rows: Vec<String> = first
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, body)| String::from_utf8_lossy(body).to_string())
        .collect();
    assert_eq!(first_rows.len(), 1, "one row was asked for: {:?}", tags(&first));
    assert!(tags(&first).contains(&'s'), "there is more, so it suspends: {:?}", tags(&first));

    client.send(b'E', &fetch()).await;
    client.send(b'S', &[]).await;
    let second = client.read_until(b'Z').await;
    let second_rows: Vec<String> = second
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, body)| String::from_utf8_lossy(body).to_string())
        .collect();
    assert_eq!(second_rows.len(), 1, "the second page holds the second row");
    assert_ne!(
        first_rows[0], second_rows[0],
        "the portal resent its first row instead of advancing --- this is the infinite loop"
    );
    assert!(
        tags(&second).contains(&'C'),
        "the result is exhausted, so it completes rather than suspending: {:?}",
        tags(&second)
    );
}
