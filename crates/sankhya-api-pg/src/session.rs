//! One client connection, from the startup packet to the last byte.
//!
//! # The shape of a connection
//!
//! A connection is a state machine with three states and no way to skip one. A client that
//! sends a query before authenticating gets an error rather than an answer, and the state
//! machine is what makes that structural rather than a check somebody remembered to write.
//!
//! # Why the handler is a trait
//!
//! Everything here is protocol. What a query *means* is somebody else's problem --- the
//! catalogue, the planner, the policy component. Keeping that behind a trait means this
//! module can be tested by driving bytes in and reading bytes out, with no engine at all,
//! which is what makes the protocol tests above possible.

use std::collections::BTreeMap;
use crate::catalog::{answer, recognise, startup_parameters, CatalogTable};
use crate::message::{
    decode, decode_startup, encode, BackendMessage, DecodeError, FieldDescription, FrontendMessage,
};
use bytes::BytesMut;

/// What a connection has got through so far.
///
/// A client cannot reach `Ready` without passing through `Authenticating`, and cannot run a
/// query before `Ready`. The compiler does not enforce that, but the single `advance`
/// function does, and it is the only place the state changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Waiting for the startup packet.
    Startup,
    /// The client asked for TLS, the door has it, and `S` has been written.
    ///
    /// Nothing more can be decoded from this connection until a handshake happens, which
    /// needs a socket — so the state machine stops here and the caller that owns the socket
    /// takes over. Keeping the *decision* here and only the *handshake* out there is what
    /// stops there being two places that know what an `SSLRequest` is.
    Handshaking,
    /// Startup received; waiting for credentials.
    Authenticating,
    /// Authenticated and ready for statements.
    Ready,
    /// The client said goodbye, or we are refusing to continue.
    Closed,
}

/// What a query produced.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QueryResult {
    /// The columns.
    pub fields: Vec<FieldDescription>,
    /// The rows, as text.
    pub rows: Vec<Vec<Option<String>>>,
    /// The completion tag, such as `SELECT 3`.
    pub tag: String,
}

/// What went wrong running a query.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QueryFailure {
    /// The five-character SQLSTATE, which is what the client branches on.
    pub sqlstate: String,
    /// What happened.
    pub message: String,
    /// What to do about it.
    pub detail: Option<String>,
}

/// Whatever actually answers queries.
pub trait Handler: Send + Sync {
    /// Authenticate a client. `None` for the parameters means the startup packet had none.
    ///
    /// Returning `Err` refuses the connection with a message the client will show.
    fn authenticate(
        &self,
        parameters: &[(String, String)],
        password: Option<&[u8]>,
    ) -> Result<(), QueryFailure>;

    /// Whether this client must send a password.
    ///
    /// Separate from `authenticate` because the server has to decide *before* it can ask,
    /// and asking a client that needs no password is a round trip nobody needs.
    fn requires_password(&self, _parameters: &[(String, String)]) -> bool {
        true
    }

    /// Run a statement.
    fn query(&self, sql: &str) -> Result<QueryResult, QueryFailure>;

    /// Whether this handler recognises the statement as one of its own.
    ///
    /// # Why the handler gets to say so first
    ///
    /// [`crate::catalog::recognise`] is deliberately lenient — it answers `SHOW <anything>`
    /// as a session setting, because tools generate settings queries in a dozen spellings and
    /// matching them literally works for one client and fails for the next.
    ///
    /// That leniency swallows statements a *server* defines. `SHOW FEEDS` was answered here
    /// as a setting called `feeds`, with one empty value, and never reached the handler that
    /// implements it. Nothing below this layer could notice: the handler's own tests call it
    /// directly, so the surface worked everywhere except over the wire, which is the only
    /// place anybody uses it.
    ///
    /// So a handler may claim a statement, and a claimed statement bypasses the shortcut. The
    /// precedence is the right way round — the thing that *defines* a statement decides
    /// before the thing that guesses at one — and it is a default of `false`, so a handler
    /// that claims nothing behaves exactly as before.
    fn claims(&self, _sql: &str) -> bool {
        false
    }

    /// The tables this client may see, for catalogue answers.
    ///
    /// Already filtered by policy when this is called. A catalogue that listed tables the
    /// caller cannot read would disclose their existence, which is the leak the policy
    /// component refuses to permit anywhere else.
    fn visible_tables(&self) -> Vec<CatalogTable>;

    /// This server's version, for `version()` and `server_version`.
    fn server_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// The schema `current_schema()` reports.
    fn current_schema(&self) -> String {
        "public".to_string()
    }

    /// A connection has been accepted.
    ///
    /// Paired with [`Handler::connection_closed`], and the pairing is the whole contract: a
    /// gauge incremented on accept and decremented on any path *except* the one a panicking
    /// or short-circuiting connection takes will climb forever and read as a leak that is
    /// not there. The listener calls this on accept and the close on every exit from
    /// `serve`, including the error paths.
    ///
    /// Defaulted to nothing, so a handler that does not care about connection lifecycle ---
    /// every test handler --- is unaffected.
    fn connection_opened(&self) {}

    /// A connection has finished, however it finished.
    fn connection_closed(&self) {}
}

/// One connection's protocol state.
#[derive(Debug)]
pub struct Connection {
    phase: Phase,
    parameters: Vec<(String, String)>,
    process_id: i32,
    secret: i32,
    encrypts: bool,
    insists: bool,
    /// Prepared statements by name; the empty name is the unnamed one.
    ///
    /// # Why these are held at all
    ///
    /// They were not, and the consequence was that the **extended query protocol did not
    /// work**. `Parse` was acknowledged and its SQL discarded, `Bind` was acknowledged,
    /// `Describe` answered `NoData`, and `Execute` had no arm at all --- so it fell to the
    /// out-of-phase catch-all, which refuses with `08P01` and **closes the connection**.
    ///
    /// Three cheerful acknowledgements and then a dead socket. Every mainstream driver ---
    /// JDBC, psycopg, pgx, npgsql, ODBC --- uses this path by default, against a door whose
    /// whole purpose is that ordinary PostgreSQL clients work.
    statements: BTreeMap<String, String>,
    /// Bound portals by name, each carrying the statement it will run.
    portals: BTreeMap<String, Portal>,
}

/// A bound portal: a statement with its parameters substituted, and its answer once run.
#[derive(Clone, Debug, Default)]
struct Portal {
    /// The statement to run, parameters already in place.
    sql: String,
    /// What it answered, once something has asked.
    ///
    /// # Why the answer is cached rather than re-run
    ///
    /// A client sends `Describe` and then `Execute`, and both need the result: `Describe` needs
    /// the column names, `Execute` needs the rows. Running the statement twice would answer
    /// from two different snapshots of the warehouse, so the column list could describe rows
    /// that are not the ones sent.
    ///
    /// This does mean the statement runs at `Describe` time, which is earlier than PostgreSQL
    /// would run it. That is a real difference and it is the honest one available here: the
    /// alternative is planning without executing, which needs a planner this layer does not
    /// have and must not acquire --- a second planner would disagree with the first.
    answered: Option<Result<QueryResult, QueryFailure>>,
}

impl Connection {
    /// A connection that has not yet received its startup packet.
    ///
    /// `process_id` and `secret` are what a cancellation request on a second connection
    /// will present. They must be unguessable: anyone who can guess them can cancel
    /// somebody else's query.
    #[must_use]
    pub fn new(process_id: i32, secret: i32) -> Self {
        Self {
            phase: Phase::Startup,
            parameters: Vec::new(),
            process_id,
            secret,
            encrypts: false,
            insists: false,
            statements: BTreeMap::new(),
            portals: BTreeMap::new(),
        }
    }

    /// This connection is on a door that can encrypt.
    ///
    /// `insisting` makes it a door that will not serve a client which never asked.
    #[must_use]
    pub const fn on_an_encrypted_door(mut self, insisting: bool) -> Self {
        self.encrypts = true;
        self.insists = insisting;
        self
    }

    /// What state this connection is in.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// The startup parameters the client sent.
    #[must_use]
    pub fn parameters(&self) -> &[(String, String)] {
        &self.parameters
    }

    /// The value of one startup parameter.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Consume as many complete messages as `input` holds, producing bytes to send back.
    ///
    /// Returns how many bytes of `input` were consumed. A caller drives this in a loop,
    /// keeping whatever is left over --- which is what makes the connection handle a client
    /// that pipelines, or a network that splits a message across two reads.
    pub fn advance(&mut self, input: &[u8], handler: &dyn Handler, output: &mut BytesMut) -> usize {
        let mut consumed = 0usize;
        loop {
            let rest = input.get(consumed..).unwrap_or(&[]);
            if rest.is_empty() || self.phase == Phase::Closed {
                return consumed;
            }

            let decoded = if self.phase == Phase::Startup {
                decode_startup(rest)
            } else {
                decode(rest)
            };

            match decoded {
                Err(DecodeError::Incomplete) => return consumed,
                Err(error) => {
                    // A malformed message desynchronises the stream: everything after it is
                    // at an unknown offset. Continuing would be guessing, so the connection
                    // ends rather than producing plausible nonsense.
                    self.fail(&error.to_string(), "08P01", output);
                    return input.len();
                }
                Ok((message, used)) => {
                    consumed = consumed.saturating_add(used);
                    self.handle(message, handler, output);
                }
            }
        }
    }

    /// Act on one decoded message.
    fn handle(&mut self, message: FrontendMessage, handler: &dyn Handler, output: &mut BytesMut) {
        match (self.phase, message) {
            (Phase::Startup, FrontendMessage::SslRequest) => {
                // A single byte, outside the ordinary framing: 'N' declines, 'S' accepts.
                // Every client knows how to proceed after an 'N', which is why declining is
                // an answer rather than a silence.
                if self.encrypts {
                    output.extend_from_slice(b"S");
                    self.phase = Phase::Handshaking;
                } else {
                    output.extend_from_slice(b"N");
                }
            }
            (Phase::Startup, FrontendMessage::GssEncRequest) => {
                // Answered rather than ignored. `psql` with `gssencmode=prefer` — the
                // default on several distributions — asks this before anything else, and a
                // server that says nothing leaves it waiting on somebody's timeout.
                output.extend_from_slice(b"N");
            }
            (Phase::Startup, FrontendMessage::CancelRequest { .. }) => {
                // Cancellation arrives on its own connection and gets no reply at all —
                // the server acts and closes. Replying would be a protocol violation.
                self.phase = Phase::Closed;
            }
            (Phase::Startup, FrontendMessage::Startup { parameters }) if self.insists => {
                // The client reached its startup message without ever asking about TLS, so
                // this connection is in the clear and about to carry a password.
                //
                // Refused here rather than after authentication, and with a message rather
                // than a closed socket: a silent close is reported by every client as a
                // network fault, and the operator goes to look at the network. `28000` and
                // a sentence naming the cause is the difference between changing one
                // connection string and reading a packet capture.
                self.parameters = parameters;
                encode(
                    &BackendMessage::ErrorResponse {
                        sqlstate: "28000".to_owned(),
                        message: "this server requires TLS and this connection is not \
                                  encrypted"
                            .to_owned(),
                        detail: Some(
                            "connect with sslmode=require or stronger. The refusal is \
                             before authentication because a password sent to discover \
                             this would already have crossed the wire in the clear"
                                .to_owned(),
                        ),
                    },
                    output,
                );
                self.phase = Phase::Closed;
            }
            (Phase::Startup, FrontendMessage::Startup { parameters }) => {
                self.parameters = parameters;
                if handler.requires_password(&self.parameters) {
                    self.phase = Phase::Authenticating;
                    encode(&BackendMessage::AuthenticationCleartextPassword, output);
                } else {
                    match handler.authenticate(&self.parameters, None) {
                        Ok(()) => self.become_ready(handler, output),
                        Err(failure) => self.refuse(&failure, output),
                    }
                }
            }
            (Phase::Authenticating, FrontendMessage::Password { body }) => {
                match handler.authenticate(&self.parameters, Some(&body)) {
                    Ok(()) => self.become_ready(handler, output),
                    Err(failure) => self.refuse(&failure, output),
                }
            }
            (Phase::Ready, FrontendMessage::Query { sql }) => {
                self.run(&sql, handler, output);
                encode(&BackendMessage::ReadyForQuery { status: b'I' }, output);
            }
            (Phase::Ready, FrontendMessage::Parse { name, sql }) => {
                self.statements.insert(name, sql);
                encode(&BackendMessage::ParseComplete, output);
            }
            (Phase::Ready, FrontendMessage::Bind { portal, statement, parameters }) => {
                match self.statements.get(&statement) {
                    None => self.complain(
                        &format!("there is no prepared statement called `{statement}`"),
                        // `26000`, invalid_sql_statement_name --- what a driver branches on
                        // to re-prepare rather than to reconnect.
                        "26000",
                        output,
                    ),
                    Some(sql) => {
                        let sql = substitute(sql, &parameters);
                        self.portals.insert(portal, Portal { sql, answered: None });
                        encode(&BackendMessage::BindComplete, output);
                    }
                }
            }
            (Phase::Ready, FrontendMessage::Describe { kind, name }) => {
                self.describe(kind, &name, handler, output);
            }
            (Phase::Ready, FrontendMessage::Execute { portal, max_rows }) => {
                self.execute_portal(&portal, max_rows, handler, output);
            }
            (Phase::Ready, FrontendMessage::Close { kind, name }) => {
                // Both are named because a client that closes one and finds the other still
                // there has a leak it cannot see.
                if kind == b'S' {
                    self.statements.remove(&name);
                } else {
                    self.portals.remove(&name);
                }
                encode(&BackendMessage::CloseComplete, output);
            }
            (Phase::Ready, FrontendMessage::Sync) => {
                encode(&BackendMessage::ReadyForQuery { status: b'I' }, output);
            }
            (_, FrontendMessage::Terminate) => {
                self.phase = Phase::Closed;
            }
            (phase, message) => {
                // A message that does not belong in this phase. The commonest real case is
                // a query before authentication, and answering it would be the whole point
                // of authentication gone.
                self.fail(
                    &format!("a {message:?} is not valid while {phase:?}"),
                    "08P01",
                    output,
                );
            }
        }
    }

    /// Finish authentication and tell the client everything it needs.
    fn become_ready(&mut self, handler: &dyn Handler, output: &mut BytesMut) {
        self.phase = Phase::Ready;
        encode(&BackendMessage::AuthenticationOk, output);
        for (name, value) in startup_parameters(&handler.server_version()) {
            encode(&BackendMessage::ParameterStatus { name, value }, output);
        }
        encode(
            &BackendMessage::BackendKeyData {
                process_id: self.process_id,
                secret: self.secret,
            },
            output,
        );
        encode(&BackendMessage::ReadyForQuery { status: b'I' }, output);
    }

    /// Run one statement, answering from the catalogue where that is what was asked.
    fn run(&self, sql: &str, handler: &dyn Handler, output: &mut BytesMut) {
        if sql.trim().is_empty() {
            encode(&BackendMessage::EmptyQueryResponse, output);
            return;
        }

        // Catalogue queries are answered here rather than reaching the engine. They refer
        // to tables that do not exist in it, so passing them through would produce a
        // "no such table" error for a query the client considers routine.
        if let Some(catalogue) = recognise(sql).filter(|_| !handler.claims(sql)) {
            let result = answer(
                &catalogue,
                &handler.server_version(),
                &handler.current_schema(),
                &handler.visible_tables(),
            );
            let rows = result.rows.len();
            encode(
                &BackendMessage::RowDescription {
                    fields: result.fields,
                },
                output,
            );
            for row in result.rows {
                encode(
                    &BackendMessage::DataRow {
                        values: row_bytes(&row),
                    },
                    output,
                );
            }
            encode(
                &BackendMessage::CommandComplete {
                    tag: format!("SELECT {rows}"),
                },
                output,
            );
            return;
        }

        match handler.query(sql) {
            Ok(result) => {
                encode(
                    &BackendMessage::RowDescription {
                        fields: result.fields,
                    },
                    output,
                );
                for row in result.rows {
                    encode(
                        &BackendMessage::DataRow {
                            values: row_bytes(&row),
                        },
                        output,
                    );
                }
                encode(&BackendMessage::CommandComplete { tag: result.tag }, output);
            }
            Err(failure) => {
                encode(
                    &BackendMessage::ErrorResponse {
                        sqlstate: failure.sqlstate,
                        message: failure.message,
                        detail: failure.detail,
                    },
                    output,
                );
            }
        }
    }

    /// Answer what a prepared statement or portal looks like.
    ///
    /// A statement gets its parameter list and the shape of its rows; a portal gets the shape
    /// alone. The shape comes from **running** the portal and caching the answer --- see
    /// [`Portal::answered`] for why running early is the honest option here.
    fn describe(&mut self, kind: u8, name: &str, handler: &dyn Handler, output: &mut BytesMut) {
        if kind == b'S' {
            // No parameter types are inferred: nothing here plans, so nothing here knows them.
            // An empty list means "none declared", which every driver accepts and which is
            // true --- the values arrive as text and are substituted as text.
            encode(&BackendMessage::ParameterDescription { type_oids: Vec::new() }, output);
        }
        let portal = if kind == b'S' {
            // Describing a *statement* names a statement, not a portal. There is no bound
            // portal to run, so the shape is not knowable without one --- and `NoData` is the
            // protocol's way of saying so.
            self.statements.get(name).map(|sql| Portal {
                sql: sql.clone(),
                answered: None,
            })
        } else {
            self.portals.get(name).cloned()
        };
        let Some(portal) = portal else {
            if kind == b'S' {
                encode(&BackendMessage::NoData, output);
            } else {
                self.complain(
                    &format!("there is no portal called `{name}`"),
                    // `34000`, invalid_cursor_name.
                    "34000",
                    output,
                );
            }
            return;
        };
        if kind == b'S' {
            encode(&BackendMessage::NoData, output);
            return;
        }

        let answered = self.answer_of(name, &portal, handler);
        match answered {
            Ok(result) if result.fields.is_empty() => {
                encode(&BackendMessage::NoData, output);
            }
            Ok(result) => {
                encode(&BackendMessage::RowDescription { fields: result.fields }, output);
            }
            Err(failure) => self.report(&failure, output),
        }
    }

    /// Run a bound portal and send its rows.
    fn execute_portal(
        &mut self,
        name: &str,
        max_rows: i32,
        handler: &dyn Handler,
        output: &mut BytesMut,
    ) {
        let Some(portal) = self.portals.get(name).cloned() else {
            self.complain(&format!("there is no portal called `{name}`"), "34000", output);
            return;
        };
        if portal.sql.trim().is_empty() {
            encode(&BackendMessage::EmptyQueryResponse, output);
            return;
        }
        match self.answer_of(name, &portal, handler) {
            Err(failure) => self.report(&failure, output),
            Ok(result) => {
                // `max_rows` of zero means "all of them". A positive bound is honoured and
                // answered with `PortalSuspended` rather than `CommandComplete`, because a
                // client that asked for the first ten rows and was told the statement was
                // complete would never ask for the eleventh.
                let bound = usize::try_from(max_rows).unwrap_or(0);
                let sending = if bound == 0 { result.rows.len() } else { bound.min(result.rows.len()) };
                for row in result.rows.iter().take(sending) {
                    encode(&BackendMessage::DataRow { values: row_bytes(row) }, output);
                }
                if bound > 0 && sending < result.rows.len() {
                    encode(&BackendMessage::PortalSuspended, output);
                } else {
                    encode(&BackendMessage::CommandComplete { tag: result.tag }, output);
                }
            }
        }
    }

    /// What a portal answers, run once and remembered.
    fn answer_of(
        &mut self,
        name: &str,
        portal: &Portal,
        handler: &dyn Handler,
    ) -> Result<QueryResult, QueryFailure> {
        if let Some(answered) = portal.answered.clone() {
            return answered;
        }
        let answered = self.answer_now(&portal.sql, handler);
        if let Some(held) = self.portals.get_mut(name) {
            held.answered = Some(answered.clone());
        }
        answered
    }

    /// Run one statement, from the catalogue where that is what was asked.
    ///
    /// The same decision [`Self::run`] makes, as a value rather than as bytes, because the
    /// extended protocol sends the shape and the rows in separate messages.
    fn answer_now(
        &self,
        sql: &str,
        handler: &dyn Handler,
    ) -> Result<QueryResult, QueryFailure> {
        if let Some(catalogue) = recognise(sql).filter(|_| !handler.claims(sql)) {
            let result = answer(
                &catalogue,
                &handler.server_version(),
                &handler.current_schema(),
                &handler.visible_tables(),
            );
            let rows = result.rows.len();
            return Ok(QueryResult {
                fields: result.fields,
                rows: result.rows,
                tag: format!("SELECT {rows}"),
            });
        }
        handler.query(sql)
    }

    /// Complain about a statement without closing the connection.
    ///
    /// The distinction [`Self::fail`] does not make: a message that arrives in the wrong
    /// *phase* leaves the session unusable, but a `Bind` naming a statement nobody prepared is
    /// an ordinary mistake. Closing the socket for it is what made the extended protocol
    /// unusable in the first place, and repeating that here would be the same defect in a
    /// smaller costume.
    fn complain(&self, message: &str, sqlstate: &str, output: &mut BytesMut) {
        self.report(
            &QueryFailure {
                sqlstate: sqlstate.to_string(),
                message: message.to_string(),
                detail: None,
            },
            output,
        );
    }

    /// Send a refusal without closing the connection.
    ///
    /// Unlike [`Self::fail`], which is for a message that arrived in the wrong phase and
    /// leaves the session unusable. A statement that fails is not a protocol violation, and a
    /// client that has to reconnect after every mistyped query is one nobody can use.
    fn report(&self, failure: &QueryFailure, output: &mut BytesMut) {
        encode(
            &BackendMessage::ErrorResponse {
                sqlstate: failure.sqlstate.clone(),
                message: failure.message.clone(),
                detail: failure.detail.clone(),
            },
            output,
        );
    }

    /// Refuse the connection.
    fn refuse(&mut self, failure: &QueryFailure, output: &mut BytesMut) {
        encode(
            &BackendMessage::ErrorResponse {
                sqlstate: failure.sqlstate.clone(),
                message: failure.message.clone(),
                detail: failure.detail.clone(),
            },
            output,
        );
        self.phase = Phase::Closed;
    }

    /// End the connection with a protocol error.
    fn fail(&mut self, message: &str, sqlstate: &str, output: &mut BytesMut) {
        encode(
            &BackendMessage::ErrorResponse {
                sqlstate: sqlstate.to_string(),
                message: message.to_string(),
                detail: None,
            },
            output,
        );
        self.phase = Phase::Closed;
    }
}

/// Render a row's text values as the bytes a `DataRow` carries.
///
/// `None` stays `None` all the way to the wire, where it becomes a length of -1. Turning it
/// into an empty string here would be the same wrong answer one layer earlier.
fn row_bytes(row: &[Option<String>]) -> Vec<Option<Vec<u8>>> {
    row.iter()
        .map(|value| value.as_ref().map(|text| text.as_bytes().to_vec()))
        .collect()
}

/// Put a portal's parameter values into its statement.
///
/// # Why this is textual, and what that costs
///
/// The simple-query door has no parameter binding, so a value has to reach the engine inside
/// the statement. Values arrive as text --- this server declares no parameter types, so every
/// client sends text --- and are quoted as SQL literals here.
///
/// `NULL` is written as the keyword and not as `''`. A parameter that is absent and one that
/// is the empty string are different values, and a binding that conflated them would produce a
/// wrong answer rather than a formatting slip.
///
/// **This is not a substitute for real binding**, and the difference is worth naming: a
/// literal is re-parsed by the engine, so it must be escaped correctly, and it is escaped
/// correctly here by doubling quotes --- the standard SQL escape, which the engine reads back
/// as one quote. When the engine grows typed parameter binding, this goes away and the values
/// stop passing through the parser at all.
fn substitute(sql: &str, parameters: &[Option<Vec<u8>>]) -> String {
    if parameters.is_empty() {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len());
    let mut characters = sql.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '$' {
            out.push(character);
            // A literal in the statement is copied through untouched, so that a `$1` inside
            // one stays text rather than becoming a placeholder.
            if character == '\'' {
                while let Some(inside) = characters.next() {
                    out.push(inside);
                    if inside == '\'' && characters.peek() != Some(&'\'') {
                        break;
                    }
                    if inside == '\'' {
                        if let Some(escaped) = characters.next() {
                            out.push(escaped);
                        }
                    }
                }
            }
            continue;
        }
        let mut digits = String::new();
        while characters.peek().is_some_and(char::is_ascii_digit) {
            if let Some(digit) = characters.next() {
                digits.push(digit);
            }
        }
        // `$` followed by something that is not a number is not a placeholder.
        let Ok(index) = digits.parse::<usize>() else {
            out.push('$');
            out.push_str(&digits);
            continue;
        };
        match index.checked_sub(1).and_then(|at| parameters.get(at)) {
            // A placeholder with no value bound to it is left as it was, so the engine
            // reports it rather than this layer inventing a value for it.
            None => {
                out.push('$');
                out.push_str(&digits);
            }
            Some(None) => out.push_str("NULL"),
            Some(Some(bytes)) => {
                let text = String::from_utf8_lossy(bytes);
                out.push('\'');
                out.push_str(&text.replace('\'', "''"));
                out.push('\'');
            }
        }
    }
    out
}
