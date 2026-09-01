//! The PostgreSQL wire protocol, encoded and decoded by hand.
//!
//! # Why by hand
//!
//! `FR-API-02` calls this the highest-adoption-value surface in the product, and the reason
//! is that every reporting tool, notebook, driver and ORM in a very large ecosystem already
//! speaks it. That value comes entirely from behaving *exactly* as those clients expect, and
//! a library that gets ninety percent of the protocol right leaves the remaining ten percent
//! as bugs nobody can diagnose --- a tool that connects, runs one query, and then hangs.
//!
//! Writing it out means every field is a decision this repository made and can test.
//!
//! # The shapes that matter
//!
//! Messages are length-prefixed. Every one after the startup packet begins with a single
//! type byte, then a big-endian `i32` length **that includes itself but not the type byte**.
//! That off-by-four is the single most common mistake in an implementation of this protocol,
//! and it does not fail loudly: the client reads the next message from the wrong offset and
//! reports something unrelated.

use bytes::{Buf, BufMut, BytesMut};

/// The protocol version this server speaks: 3.0, as `(3 << 16) | 0`.
pub const PROTOCOL_VERSION: i32 = 196_608;

/// The magic number a client sends to request TLS before anything else.
pub const SSL_REQUEST_CODE: i32 = 80_877_103;

/// The magic number for a cancellation request on a second connection.
pub const CANCEL_REQUEST_CODE: i32 = 80_877_102;

/// The magic number a client sends to request GSSAPI encryption.
///
/// Decoded so it can be declined. `psql` with `gssencmode=prefer` — the default on several
/// distributions — sends this before anything else, and an unrecognised code is a decode
/// error, which would make the most ordinary client on Linux fail to connect for a reason
/// that reads like corruption.
pub const GSS_REQUEST_CODE: i32 = 80_877_104;

/// What a client sent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FrontendMessage {
    /// The first message: protocol version and connection parameters.
    Startup {
        /// The parameters, `user` and `database` among them.
        parameters: Vec<(String, String)>,
    },
    /// A request to negotiate TLS before the startup packet.
    SslRequest,
    /// A request to negotiate GSSAPI encryption before the startup packet.
    GssEncRequest,
    /// A request to cancel work on another connection.
    CancelRequest {
        /// Which backend.
        process_id: i32,
        /// The secret it issued.
        secret: i32,
    },
    /// A password, in whatever form was asked for.
    Password {
        /// The bytes, which are not necessarily text.
        body: Vec<u8>,
    },
    /// A statement to run immediately.
    Query {
        /// The SQL.
        sql: String,
    },
    /// Prepare a statement.
    Parse {
        /// The name to give it, or empty for the unnamed one.
        name: String,
        /// The SQL.
        sql: String,
    },
    /// Bind parameters to a prepared statement.
    Bind {
        /// The portal to create.
        portal: String,
        /// The statement to bind.
        statement: String,
    },
    /// Ask what a prepared statement or portal looks like.
    Describe {
        /// `S` for a statement, `P` for a portal.
        kind: u8,
        /// Which one.
        name: String,
    },
    /// Run a bound portal.
    Execute {
        /// Which portal.
        portal: String,
        /// How many rows to return, or zero for all of them.
        max_rows: i32,
    },
    /// Finish the current extended-query sequence.
    Sync,
    /// Discard a prepared statement or portal.
    Close {
        /// `S` or `P`.
        kind: u8,
        /// Which one.
        name: String,
    },
    /// The client is going away.
    Terminate,
}

/// What the server sends back.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BackendMessage {
    /// Authentication succeeded and nothing further is needed.
    AuthenticationOk,
    /// The server wants a password in the clear.
    AuthenticationCleartextPassword,
    /// A runtime parameter the client should record.
    ParameterStatus {
        /// Its name.
        name: String,
        /// Its value.
        value: String,
    },
    /// The identity a cancellation request will need.
    BackendKeyData {
        /// This connection's identifier.
        process_id: i32,
        /// The secret that authorises cancelling it.
        secret: i32,
    },
    /// The server is ready for the next statement.
    ReadyForQuery {
        /// `I` idle, `T` in a transaction, `E` in a failed transaction.
        status: u8,
    },
    /// The shape of the rows that follow.
    RowDescription {
        /// One entry per column.
        fields: Vec<FieldDescription>,
    },
    /// One row.
    DataRow {
        /// One value per column; `None` is SQL null, which is not the empty string.
        values: Vec<Option<Vec<u8>>>,
    },
    /// A statement finished.
    CommandComplete {
        /// The tag, such as `SELECT 3`.
        tag: String,
    },
    /// The statement returned no rows and produced no tag-worthy result.
    EmptyQueryResponse,
    /// Something went wrong.
    ErrorResponse {
        /// The five-character SQLSTATE.
        sqlstate: String,
        /// What happened.
        message: String,
        /// What to do about it.
        detail: Option<String>,
    },
    /// A statement was prepared.
    ParseComplete,
    /// A portal was bound.
    BindComplete,
    /// A statement or portal was discarded.
    CloseComplete,
    /// A portal has more rows than the requested limit.
    PortalSuspended,
    /// A described statement takes no parameters, or these.
    ParameterDescription {
        /// The type OID of each parameter.
        type_oids: Vec<i32>,
    },
    /// A described statement returns nothing.
    NoData,
}

/// One column of a result.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FieldDescription {
    /// The column's name.
    pub name: String,
    /// The PostgreSQL type OID.
    ///
    /// A real OID, not an invented one. Every client maps these to its own types, and an
    /// unknown OID is usually rendered as an opaque string --- which looks like the query
    /// returning text where it returned a number.
    pub type_oid: i32,
    /// The type's width, or `-1` for variable.
    pub type_size: i16,
    /// `0` for text, `1` for binary.
    pub format: i16,
}

impl FieldDescription {
    /// A text-format column of the given type.
    #[must_use]
    pub fn text(name: impl Into<String>, type_oid: i32, type_size: i16) -> Self {
        Self {
            name: name.into(),
            type_oid,
            type_size,
            format: 0,
        }
    }
}

/// The type OIDs this server uses.
///
/// Taken from PostgreSQL's own catalogue rather than assigned here. A client receiving an
/// OID it does not recognise falls back to treating the value as an opaque string, so an
/// invented OID turns every integer column into text without any error being raised.
pub mod oid {
    /// `bool`.
    pub const BOOL: i32 = 16;
    /// `bytea`.
    pub const BYTEA: i32 = 17;
    /// `int8`.
    pub const INT8: i32 = 20;
    /// `int2`.
    pub const INT2: i32 = 21;
    /// `int4`.
    pub const INT4: i32 = 23;
    /// `text`.
    pub const TEXT: i32 = 25;
    /// `oid`.
    pub const OID: i32 = 26;
    /// `float4`.
    pub const FLOAT4: i32 = 700;
    /// `float8`.
    pub const FLOAT8: i32 = 701;
    /// `varchar`.
    pub const VARCHAR: i32 = 1043;
    /// `date`.
    pub const DATE: i32 = 1082;
    /// `timestamp`.
    pub const TIMESTAMP: i32 = 1114;
    /// `timestamptz`.
    pub const TIMESTAMPTZ: i32 = 1184;
    /// `numeric`.
    pub const NUMERIC: i32 = 1700;
}

/// Why a message could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// Not enough bytes yet. Read more and try again.
    ///
    /// Not an error in the ordinary sense --- a streaming decoder sees this constantly. It
    /// is separate from every real failure so a caller cannot conflate "wait" with "give up".
    Incomplete,
    /// The message declares a length that cannot be right.
    BadLength {
        /// What it declared.
        declared: i32,
    },
    /// A message type this server does not implement.
    UnknownType {
        /// The type byte.
        tag: u8,
    },
    /// A string was not valid UTF-8, or was not terminated.
    BadString,
    /// The startup packet asked for a protocol version this server does not speak.
    UnsupportedVersion {
        /// What was asked for.
        version: i32,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incomplete => f.write_str("the message is not complete yet"),
            Self::BadLength { declared } => write!(
                f,
                "a message declared a length of {declared}, which cannot be right. The \
                 length includes itself and excludes the type byte; an implementation that \
                 has this off by four does not fail loudly, it reads the next message from \
                 the wrong offset"
            ),
            Self::UnknownType { tag } => write!(
                f,
                "message type '{}' is not implemented by this server",
                *tag as char
            ),
            Self::BadString => f.write_str("a string was unterminated or not valid UTF-8"),
            Self::UnsupportedVersion { version } => write!(
                f,
                "protocol version {version} is not supported; this server speaks 3.0 \
                 ({PROTOCOL_VERSION})"
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Read a startup packet, which has no type byte.
///
/// Returns the message and how many bytes it consumed, so a caller driving a stream knows
/// how far to advance.
pub fn decode_startup(buffer: &[u8]) -> Result<(FrontendMessage, usize), DecodeError> {
    if buffer.len() < 8 {
        return Err(DecodeError::Incomplete);
    }
    let mut cursor = buffer;
    let length = cursor.get_i32();
    if length < 8 || length > 10_000 {
        return Err(DecodeError::BadLength { declared: length });
    }
    let length = usize::try_from(length).unwrap_or(0);
    if buffer.len() < length {
        return Err(DecodeError::Incomplete);
    }
    let code = cursor.get_i32();

    match code {
        SSL_REQUEST_CODE => Ok((FrontendMessage::SslRequest, length)),
        GSS_REQUEST_CODE => Ok((FrontendMessage::GssEncRequest, length)),
        CANCEL_REQUEST_CODE => {
            if cursor.remaining() < 8 {
                return Err(DecodeError::Incomplete);
            }
            Ok((
                FrontendMessage::CancelRequest {
                    process_id: cursor.get_i32(),
                    secret: cursor.get_i32(),
                },
                length,
            ))
        }
        PROTOCOL_VERSION => {
            let body = buffer.get(8..length).ok_or(DecodeError::Incomplete)?;
            let mut parameters = Vec::new();
            let mut rest = body;
            loop {
                let (key, remainder) = read_cstring(rest)?;
                if key.is_empty() {
                    break;
                }
                let (value, remainder) = read_cstring(remainder)?;
                parameters.push((key, value));
                rest = remainder;
            }
            Ok((FrontendMessage::Startup { parameters }, length))
        }
        other => Err(DecodeError::UnsupportedVersion { version: other }),
    }
}

/// Read one ordinary message.
///
/// Returns the message and how many bytes it consumed.
pub fn decode(buffer: &[u8]) -> Result<(FrontendMessage, usize), DecodeError> {
    if buffer.len() < 5 {
        return Err(DecodeError::Incomplete);
    }
    let tag = *buffer.first().ok_or(DecodeError::Incomplete)?;
    let mut header = buffer.get(1..5).ok_or(DecodeError::Incomplete)?;
    let length = header.get_i32();
    // The length covers itself and the body, but not the type byte. This is the off-by-four
    // that quietly desynchronises a connection.
    if length < 4 {
        return Err(DecodeError::BadLength { declared: length });
    }
    let total = usize::try_from(length).unwrap_or(0).saturating_add(1);
    if buffer.len() < total {
        return Err(DecodeError::Incomplete);
    }
    let body = buffer.get(5..total).ok_or(DecodeError::Incomplete)?;

    let message = match tag {
        b'p' => FrontendMessage::Password {
            body: body.strip_suffix(&[0]).unwrap_or(body).to_vec(),
        },
        b'Q' => FrontendMessage::Query {
            sql: read_cstring(body)?.0,
        },
        b'P' => {
            let (name, rest) = read_cstring(body)?;
            let (sql, _) = read_cstring(rest)?;
            FrontendMessage::Parse { name, sql }
        }
        b'B' => {
            let (portal, rest) = read_cstring(body)?;
            let (statement, _) = read_cstring(rest)?;
            FrontendMessage::Bind { portal, statement }
        }
        b'D' => {
            let kind = *body.first().ok_or(DecodeError::Incomplete)?;
            let (name, _) = read_cstring(body.get(1..).unwrap_or(&[]))?;
            FrontendMessage::Describe { kind, name }
        }
        b'E' => {
            let (portal, rest) = read_cstring(body)?;
            let mut tail = rest;
            let max_rows = if tail.remaining() >= 4 {
                tail.get_i32()
            } else {
                0
            };
            FrontendMessage::Execute { portal, max_rows }
        }
        b'S' => FrontendMessage::Sync,
        b'C' => {
            let kind = *body.first().ok_or(DecodeError::Incomplete)?;
            let (name, _) = read_cstring(body.get(1..).unwrap_or(&[]))?;
            FrontendMessage::Close { kind, name }
        }
        b'X' => FrontendMessage::Terminate,
        other => return Err(DecodeError::UnknownType { tag: other }),
    };
    Ok((message, total))
}

/// Read a null-terminated string, returning it and what follows.
fn read_cstring(bytes: &[u8]) -> Result<(String, &[u8]), DecodeError> {
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or(DecodeError::BadString)?;
    let text = std::str::from_utf8(bytes.get(..end).unwrap_or(&[]))
        .map_err(|_| DecodeError::BadString)?
        .to_string();
    Ok((text, bytes.get(end + 1..).unwrap_or(&[])))
}

/// Write a message onto the wire.
pub fn encode(message: &BackendMessage, out: &mut BytesMut) {
    match message {
        BackendMessage::AuthenticationOk => framed(out, b'R', |body| body.put_i32(0)),
        BackendMessage::AuthenticationCleartextPassword => {
            framed(out, b'R', |body| body.put_i32(3));
        }
        BackendMessage::ParameterStatus { name, value } => framed(out, b'S', |body| {
            put_cstring(body, name);
            put_cstring(body, value);
        }),
        BackendMessage::BackendKeyData { process_id, secret } => framed(out, b'K', |body| {
            body.put_i32(*process_id);
            body.put_i32(*secret);
        }),
        BackendMessage::ReadyForQuery { status } => framed(out, b'Z', |body| body.put_u8(*status)),
        BackendMessage::RowDescription { fields } => framed(out, b'T', |body| {
            body.put_i16(i16::try_from(fields.len()).unwrap_or(i16::MAX));
            for field in fields {
                put_cstring(body, &field.name);
                // Table OID and column number: zero means "not a plain column of a table",
                // which is honest for a computed result and is what clients expect there.
                body.put_i32(0);
                body.put_i16(0);
                body.put_i32(field.type_oid);
                body.put_i16(field.type_size);
                // Type modifier: -1 for "none".
                body.put_i32(-1);
                body.put_i16(field.format);
            }
        }),
        BackendMessage::DataRow { values } => framed(out, b'D', |body| {
            body.put_i16(i16::try_from(values.len()).unwrap_or(i16::MAX));
            for value in values {
                match value {
                    // -1 length is SQL null. A zero-length value is the empty string, which
                    // is a different thing, and conflating them is a wrong answer rather
                    // than a formatting slip.
                    None => body.put_i32(-1),
                    Some(bytes) => {
                        body.put_i32(i32::try_from(bytes.len()).unwrap_or(0));
                        body.put_slice(bytes);
                    }
                }
            }
        }),
        BackendMessage::CommandComplete { tag } => framed(out, b'C', |body| put_cstring(body, tag)),
        BackendMessage::EmptyQueryResponse => framed(out, b'I', |_| {}),
        BackendMessage::ErrorResponse {
            sqlstate,
            message,
            detail,
        } => framed(out, b'E', |body| {
            // Severity, twice: the localised field and the always-English one. A client
            // reading only the localised field on a server in another locale gets nothing
            // it can branch on.
            body.put_u8(b'S');
            put_cstring(body, "ERROR");
            body.put_u8(b'V');
            put_cstring(body, "ERROR");
            body.put_u8(b'C');
            put_cstring(body, sqlstate);
            body.put_u8(b'M');
            put_cstring(body, message);
            if let Some(detail) = detail {
                body.put_u8(b'D');
                put_cstring(body, detail);
            }
            body.put_u8(0);
        }),
        BackendMessage::ParseComplete => framed(out, b'1', |_| {}),
        BackendMessage::BindComplete => framed(out, b'2', |_| {}),
        BackendMessage::CloseComplete => framed(out, b'3', |_| {}),
        BackendMessage::PortalSuspended => framed(out, b's', |_| {}),
        BackendMessage::ParameterDescription { type_oids } => framed(out, b't', |body| {
            body.put_i16(i16::try_from(type_oids.len()).unwrap_or(i16::MAX));
            for oid in type_oids {
                body.put_i32(*oid);
            }
        }),
        BackendMessage::NoData => framed(out, b'n', |_| {}),
    }
}

/// Write a tagged, length-prefixed message.
///
/// The length is computed after the body is written rather than predicted before it. A
/// predicted length is a second place the message's size is decided, and the two drift.
fn framed(out: &mut BytesMut, tag: u8, write_body: impl FnOnce(&mut BytesMut)) {
    let mut body = BytesMut::new();
    write_body(&mut body);
    out.put_u8(tag);
    // The length covers itself and the body, and excludes the tag.
    out.put_i32(i32::try_from(body.len().saturating_add(4)).unwrap_or(i32::MAX));
    out.put_slice(&body);
}

/// Write a null-terminated string.
fn put_cstring(out: &mut BytesMut, text: &str) {
    out.put_slice(text.as_bytes());
    out.put_u8(0);
}
