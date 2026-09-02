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
use sankhya_api_pg::session::{Handler, QueryFailure, QueryResult};
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
            }),
        }
    }

    fn query(&self, sql: &str) -> Result<QueryResult, QueryFailure> {
        if sql.contains("boom") {
            return Err(QueryFailure {
                sqlstate: "42601".to_string(),
                message: "syntax error at or near \"boom\"".to_string(),
                detail: None,
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

    fn visible_tables(&self) -> Vec<CatalogTable> {
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
    async fn read_until(&mut self, tag: u8) -> Vec<(u8, Vec<u8>)> {
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
            let read = self.stream.read(&mut chunk).await.expect("reading");
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
    body.extend_from_slice(b"SELECT id FROM t WHERE note = $1\0");
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
    // The fixture echoes the SQL it was given back through its refusal path only for `boom`,
    // so a successful answer here proves the substituted statement reached the handler.
    assert!(tags(&result).contains(&'C'), "{:?}", tags(&result));
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
