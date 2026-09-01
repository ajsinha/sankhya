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
}

impl Connection {
    /// A connection that has not yet received its startup packet.
    ///
    /// `process_id` and `secret` are what a cancellation request on a second connection
    /// will present. They must be unguessable: anyone who can guess them can cancel
    /// somebody else's query.
    #[must_use]
    pub const fn new(process_id: i32, secret: i32) -> Self {
        Self {
            phase: Phase::Startup,
            parameters: Vec::new(),
            process_id,
            secret,
            encrypts: false,
            insists: false,
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
            (Phase::Ready, FrontendMessage::Parse { .. }) => {
                encode(&BackendMessage::ParseComplete, output);
            }
            (Phase::Ready, FrontendMessage::Bind { .. }) => {
                encode(&BackendMessage::BindComplete, output);
            }
            (Phase::Ready, FrontendMessage::Describe { .. }) => {
                encode(&BackendMessage::NoData, output);
            }
            (Phase::Ready, FrontendMessage::Close { .. }) => {
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
        if let Some(catalogue) = recognise(sql) {
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
