//! The wire protocol, byte for byte.
//!
//! Protocol bugs do not fail loudly. A message framed four bytes wrong leaves the client
//! reading the next one from the wrong offset, and what it eventually reports is unrelated
//! to what went wrong. So these tests check bytes rather than behaviour: the length field,
//! the null terminator, the -1 that means SQL null.

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

use bytes::BytesMut;
use sankhya_api_pg::message::{
    decode, decode_startup, encode, oid, BackendMessage, DecodeError, FieldDescription,
    FrontendMessage, CANCEL_REQUEST_CODE, PROTOCOL_VERSION, SSL_REQUEST_CODE,
};

fn frame(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&i32::try_from(body.len() + 4).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn cstring(text: &str) -> Vec<u8> {
    let mut out = text.as_bytes().to_vec();
    out.push(0);
    out
}

// --- the startup handshake ------------------------------------------------

#[test]
fn a_startup_packet_carries_the_connection_parameters() {
    let mut body = Vec::new();
    body.extend_from_slice(&cstring("user"));
    body.extend_from_slice(&cstring("ana"));
    body.extend_from_slice(&cstring("database"));
    body.extend_from_slice(&cstring("acme"));
    body.push(0);

    let mut packet = Vec::new();
    packet.extend_from_slice(&i32::try_from(body.len() + 8).unwrap_or(0).to_be_bytes());
    packet.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    packet.extend_from_slice(&body);

    let (message, consumed) = decode_startup(&packet).expect("a valid startup packet");
    assert_eq!(consumed, packet.len());
    let FrontendMessage::Startup { parameters } = message else {
        panic!("expected a startup message");
    };
    assert_eq!(
        parameters,
        vec![
            ("user".to_string(), "ana".to_string()),
            ("database".to_string(), "acme".to_string()),
        ]
    );
}

#[test]
fn a_tls_request_is_recognised_before_anything_else() {
    let mut packet = Vec::new();
    packet.extend_from_slice(&8i32.to_be_bytes());
    packet.extend_from_slice(&SSL_REQUEST_CODE.to_be_bytes());

    let (message, consumed) = decode_startup(&packet).expect("a valid request");
    assert_eq!(message, FrontendMessage::SslRequest);
    assert_eq!(consumed, 8);
}

#[test]
fn a_cancellation_request_carries_the_key_the_server_issued() {
    let mut packet = Vec::new();
    packet.extend_from_slice(&16i32.to_be_bytes());
    packet.extend_from_slice(&CANCEL_REQUEST_CODE.to_be_bytes());
    packet.extend_from_slice(&4_242i32.to_be_bytes());
    packet.extend_from_slice(&99i32.to_be_bytes());

    let (message, _) = decode_startup(&packet).expect("a valid request");
    assert_eq!(
        message,
        FrontendMessage::CancelRequest {
            process_id: 4_242,
            secret: 99
        }
    );
}

#[test]
fn an_unsupported_protocol_version_is_named_in_the_refusal() {
    let mut packet = Vec::new();
    packet.extend_from_slice(&8i32.to_be_bytes());
    packet.extend_from_slice(&131_072i32.to_be_bytes());

    let Err(error) = decode_startup(&packet) else {
        panic!("protocol 2.0 must be refused");
    };
    assert_eq!(error, DecodeError::UnsupportedVersion { version: 131_072 });
    assert!(error.to_string().contains("3.0"));
}

// --- ordinary messages ----------------------------------------------------

#[test]
fn a_simple_query_is_read_back_exactly() {
    let packet = frame(b'Q', &cstring("SELECT 1"));
    let (message, consumed) = decode(&packet).expect("a valid query");
    assert_eq!(
        message,
        FrontendMessage::Query {
            sql: "SELECT 1".to_string()
        }
    );
    assert_eq!(consumed, packet.len());
}

#[test]
fn the_declared_length_excludes_the_type_byte_and_includes_itself() {
    // The single most common mistake in an implementation of this protocol, and it does not
    // fail loudly: the client reads the next message from the wrong offset and reports
    // something unrelated.
    let sql = cstring("SELECT 1");
    let packet = frame(b'Q', &sql);

    let declared = i32::from_be_bytes([packet[1], packet[2], packet[3], packet[4]]);
    assert_eq!(usize::try_from(declared).unwrap_or(0), sql.len() + 4);
    assert_eq!(packet.len(), usize::try_from(declared).unwrap_or(0) + 1);
}

#[test]
fn two_messages_in_one_buffer_are_read_one_at_a_time() {
    // A client may pipeline. A decoder that consumed the whole buffer would lose the second.
    let mut buffer = frame(b'Q', &cstring("SELECT 1"));
    buffer.extend_from_slice(&frame(b'S', &[]));

    let (first, consumed) = decode(&buffer).expect("the first message");
    assert!(matches!(first, FrontendMessage::Query { .. }));
    let (second, _) = decode(&buffer[consumed..]).expect("the second message");
    assert_eq!(second, FrontendMessage::Sync);
}

#[test]
fn a_partial_message_asks_for_more_rather_than_failing() {
    // A streaming decoder sees this constantly, so it is separate from every real failure —
    // a caller must not be able to conflate "wait" with "give up".
    let packet = frame(b'Q', &cstring("SELECT 1"));
    for cut in 0..packet.len() {
        assert_eq!(
            decode(&packet[..cut]),
            Err(DecodeError::Incomplete),
            "a buffer of {cut} bytes should ask for more"
        );
    }
    assert!(decode(&packet).is_ok());
}

#[test]
fn a_length_that_cannot_be_right_is_refused_rather_than_trusted() {
    let mut packet = vec![b'Q'];
    packet.extend_from_slice(&1i32.to_be_bytes());
    packet.extend_from_slice(b"junk");

    let Err(error) = decode(&packet) else {
        panic!("a length of 1 must be refused");
    };
    assert_eq!(error, DecodeError::BadLength { declared: 1 });
    assert!(error.to_string().contains("off by four"));
}

#[test]
fn an_unterminated_string_is_refused() {
    assert_eq!(
        decode(&frame(b'Q', b"SELECT 1")),
        Err(DecodeError::BadString)
    );
}

#[test]
fn a_message_type_this_server_does_not_implement_names_itself() {
    let Err(error) = decode(&frame(b'Z', &[])) else {
        panic!("an unimplemented type must be refused");
    };
    assert_eq!(error, DecodeError::UnknownType { tag: b'Z' });
    assert!(error.to_string().contains('Z'));
}

#[test]
fn the_extended_query_messages_round_trip() {
    let mut parse_body = cstring("stmt1");
    parse_body.extend_from_slice(&cstring("SELECT $1"));
    let (parsed, _) = decode(&frame(b'P', &parse_body)).expect("parse");
    assert_eq!(
        parsed,
        FrontendMessage::Parse {
            name: "stmt1".to_string(),
            sql: "SELECT $1".to_string()
        }
    );

    let mut execute_body = cstring("portal1");
    execute_body.extend_from_slice(&100i32.to_be_bytes());
    let (executed, _) = decode(&frame(b'E', &execute_body)).expect("execute");
    assert_eq!(
        executed,
        FrontendMessage::Execute {
            portal: "portal1".to_string(),
            max_rows: 100
        }
    );

    let mut describe_body = vec![b'S'];
    describe_body.extend_from_slice(&cstring("stmt1"));
    let (described, _) = decode(&frame(b'D', &describe_body)).expect("describe");
    assert_eq!(
        described,
        FrontendMessage::Describe {
            kind: b'S',
            name: "stmt1".to_string()
        }
    );
}

#[test]
fn a_password_message_drops_the_terminator_but_keeps_the_bytes() {
    // The body is not necessarily text — a SCRAM exchange is binary — so it is kept as
    // bytes rather than decoded as a string.
    let mut body = b"hunter2".to_vec();
    body.push(0);
    let (message, _) = decode(&frame(b'p', &body)).expect("a password message");
    assert_eq!(
        message,
        FrontendMessage::Password {
            body: b"hunter2".to_vec()
        }
    );
}

#[test]
fn a_terminate_message_needs_no_body() {
    let (message, consumed) = decode(&frame(b'X', &[])).expect("terminate");
    assert_eq!(message, FrontendMessage::Terminate);
    assert_eq!(consumed, 5);
}

// --- what the server writes -----------------------------------------------

fn framing_of(message: &BackendMessage) -> (u8, i32, usize) {
    let mut out = BytesMut::new();
    encode(message, &mut out);
    (
        out[0],
        i32::from_be_bytes([out[1], out[2], out[3], out[4]]),
        out.len(),
    )
}

#[test]
fn every_backend_message_frames_its_own_length_correctly() {
    // Computed after the body is written rather than predicted before it: a predicted
    // length is a second place the size is decided, and the two drift.
    let messages = [
        BackendMessage::AuthenticationOk,
        BackendMessage::ReadyForQuery { status: b'I' },
        BackendMessage::CommandComplete {
            tag: "SELECT 3".to_string(),
        },
        BackendMessage::ParseComplete,
        BackendMessage::NoData,
        BackendMessage::ParameterStatus {
            name: "server_version".to_string(),
            value: "17.0".to_string(),
        },
    ];
    for message in &messages {
        let (_, declared, total) = framing_of(message);
        assert_eq!(
            usize::try_from(declared).unwrap_or(0) + 1,
            total,
            "{message:?} is framed wrong"
        );
    }
}

#[test]
fn a_null_value_is_minus_one_and_not_an_empty_string() {
    // A zero-length value is the empty string, which is a different thing. Conflating them
    // is a wrong answer rather than a formatting slip.
    let mut out = BytesMut::new();
    encode(
        &BackendMessage::DataRow {
            values: vec![None, Some(Vec::new()), Some(b"x".to_vec())],
        },
        &mut out,
    );

    assert_eq!(out[0], b'D');
    assert_eq!(i16::from_be_bytes([out[5], out[6]]), 3);
    assert_eq!(
        i32::from_be_bytes([out[7], out[8], out[9], out[10]]),
        -1,
        "SQL null is -1"
    );
    assert_eq!(
        i32::from_be_bytes([out[11], out[12], out[13], out[14]]),
        0,
        "the empty string is zero, not -1"
    );
}

#[test]
fn a_row_description_uses_real_postgresql_type_oids() {
    // A client receiving an OID it does not recognise renders the value as an opaque
    // string, so an invented OID turns every integer column into text with no error raised.
    let mut out = BytesMut::new();
    encode(
        &BackendMessage::RowDescription {
            fields: vec![
                FieldDescription::text("id", oid::INT8, 8),
                FieldDescription::text("name", oid::TEXT, -1),
            ],
        },
        &mut out,
    );

    assert_eq!(out[0], b'T');
    assert_eq!(i16::from_be_bytes([out[5], out[6]]), 2);
    // "id\0" is three bytes, then table oid (4) and column number (2).
    let type_at = 7 + 3 + 6;
    assert_eq!(
        i32::from_be_bytes([
            out[type_at],
            out[type_at + 1],
            out[type_at + 2],
            out[type_at + 3]
        ]),
        20,
        "int8 is OID 20 in PostgreSQL's own catalogue"
    );
}

#[test]
fn an_error_response_carries_both_severity_fields() {
    // The localised field and the always-English one. A client reading only the localised
    // field, on a server running in another locale, gets nothing it can branch on.
    let mut out = BytesMut::new();
    encode(
        &BackendMessage::ErrorResponse {
            sqlstate: "42501".to_string(),
            message: "permission denied".to_string(),
            detail: Some("ask an administrator".to_string()),
        },
        &mut out,
    );

    assert_eq!(out[0], b'E');
    let body = &out[5..];
    assert!(body.contains(&b'S'));
    assert!(body.contains(&b'V'));
    assert!(body.contains(&b'C'));
    let text = String::from_utf8_lossy(body);
    assert!(text.contains("42501"));
    assert!(text.contains("permission denied"));
    assert_eq!(
        body.last(),
        Some(&0),
        "the field list ends with a zero byte"
    );
}

#[test]
fn a_data_row_reads_back_the_way_a_client_parses_it() {
    let mut out = BytesMut::new();
    let values = vec![Some(b"41".to_vec()), None, Some(b"hello".to_vec())];
    encode(
        &BackendMessage::DataRow {
            values: values.clone(),
        },
        &mut out,
    );

    let declared = i32::from_be_bytes([out[1], out[2], out[3], out[4]]);
    let mut at = 5usize;
    let count = i16::from_be_bytes([out[at], out[at + 1]]);
    at += 2;
    let mut read = Vec::new();
    for _ in 0..count {
        let length = i32::from_be_bytes([out[at], out[at + 1], out[at + 2], out[at + 3]]);
        at += 4;
        if length < 0 {
            read.push(None);
        } else {
            let n = usize::try_from(length).unwrap_or(0);
            read.push(Some(out[at..at + n].to_vec()));
            at += n;
        }
    }
    assert_eq!(read, values);
    assert_eq!(
        at,
        usize::try_from(declared).unwrap_or(0) + 1,
        "and nothing is left over"
    );
}

#[test]
fn a_ready_for_query_reports_the_transaction_state() {
    // Clients use this to decide whether to send a rollback. Reporting idle inside a failed
    // transaction leaves the client believing it can continue.
    for status in [b'I', b'T', b'E'] {
        let mut out = BytesMut::new();
        encode(&BackendMessage::ReadyForQuery { status }, &mut out);
        assert_eq!(out[0], b'Z');
        assert_eq!(out[5], status);
        assert_eq!(out.len(), 6);
    }
}

#[test]
fn an_empty_message_is_five_bytes() {
    // Tag plus a length of four, covering only itself. A common off-by-one produces four
    // bytes here and desynchronises everything after it.
    let mut out = BytesMut::new();
    encode(&BackendMessage::ParseComplete, &mut out);
    assert_eq!(out.len(), 5);
    assert_eq!(i32::from_be_bytes([out[1], out[2], out[3], out[4]]), 4);
}
