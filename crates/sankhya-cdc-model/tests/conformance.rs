//! Conformance against a real PostgreSQL replication stream.
//!
//! The unit tests in `decode.rs` feed the decoder bytes produced by a helper in the
//! same file, which proves it is self-consistent but not that it is *correct*. This
//! test decodes a stream captured from an actual PostgreSQL 17.11 server, so the
//! encoder and the decoder have genuinely independent origins.
//!
//! The fixture was produced by:
//!   - `wal_level = logical`, publication `FOR ALL TABLES`, protocol version 4
//!   - insert with a 12 KB out-of-line value, a second plain insert
//!   - an update that deliberately does **not** touch the large value, which is what
//!     makes the source withhold it as unchanged
//!   - a delete
//!
//! # A trap worth recording
//!
//! The fixture is captured through `pg_logical_slot_peek_binary_changes`, which
//! returns **one row per message** with exact bytes. It is *not* captured with
//! `pg_recvlogical -f`: that tool appends a newline separator after each message,
//! because it is built for textual output plugins. On a binary stream those newlines
//! are indistinguishable from payload and corrupt it. The first attempt at this
//! fixture failed for exactly that reason, at the byte immediately after the first
//! `Begin` — and the decoder was right while the capture was wrong.
//!
//! The fixture is stored length-framed (a big-endian `u32` length before each
//! message) so the test can assert per-message boundaries independently of the
//! decoder's own idea of where a message ends. Reusing the decoder to find the
//! boundaries would make the boundary assertions circular.
//!
//! Regenerate with `crates/sankhya-cdc-model/tests/fixtures/regenerate.sh`.

use sankhya_cdc_model::{Decoder, Message, ReplicaIdentity, TupleValue};

const REAL_STREAM: &[u8] = include_bytes!("fixtures/real-pgoutput-v4.bin");

/// Split the length-framed fixture into individual messages.
///
/// The framing comes from the capture, not from the protocol, so it gives an
/// independent ground truth for where each message begins and ends.
fn framed_messages(mut bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while bytes.len() >= 4 {
        let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let body = &bytes[4..4 + len];
        out.push(body);
        bytes = &bytes[4 + len..];
    }
    assert!(bytes.is_empty(), "fixture framing is inconsistent");
    out
}

/// Decode every message, asserting the decoder consumes each one **exactly**.
///
/// Consuming too little would desynchronise a live stream; consuming too much would
/// silently swallow the next message. Both are caught here because the expected
/// length comes from the capture rather than from the decoder.
fn decode_stream(bytes: &[u8]) -> Vec<Message> {
    let decoder = Decoder::new();
    framed_messages(bytes)
        .into_iter()
        .enumerate()
        .map(|(i, body)| match decoder.decode_prefix(body) {
            Ok((message, consumed)) => {
                assert_eq!(
                    consumed,
                    body.len(),
                    "message {i} (tag {:?}): decoder consumed {consumed} of {} bytes; \
                     a live stream would desynchronise here",
                    body.first().map(|b| *b as char),
                    body.len()
                );
                message
            }
            Err(e) => panic!(
                "message {i} (tag {:?}) failed to decode: {e}\nbytes: {:02x?}",
                body.first().map(|b| *b as char),
                &body[..body.len().min(48)]
            ),
        })
        .collect()
}

#[test]
fn decodes_a_real_stream_end_to_end() {
    let messages = decode_stream(REAL_STREAM);
    assert!(!messages.is_empty(), "fixture decoded to nothing");

    // Every transaction that begins must also end. If the decoder mis-sized any
    // message the stream would desynchronise and this balance would break.
    let begins = messages
        .iter()
        .filter(|m| matches!(m, Message::Begin { .. }))
        .count();
    let commits = messages.iter().filter(|m| m.seals_transaction()).count();
    assert_eq!(
        begins, commits,
        "unbalanced transactions: {begins} begins, {commits} commits"
    );
    assert_eq!(
        begins, 4,
        "the fixture contains four autocommitted statements"
    );

    // Exactly the shape the fixture script produces.
    let inserts = messages
        .iter()
        .filter(|m| matches!(m, Message::Insert { .. }))
        .count();
    let updates = messages
        .iter()
        .filter(|m| matches!(m, Message::Update { .. }))
        .count();
    let deletes = messages
        .iter()
        .filter(|m| matches!(m, Message::Delete { .. }))
        .count();
    assert_eq!((inserts, updates, deletes), (2, 1, 1));
}

#[test]
fn real_relation_metadata_matches_the_source_schema() {
    let messages = decode_stream(REAL_STREAM);
    let relation = messages
        .iter()
        .find_map(|m| match m {
            Message::Relation(r) if r.name == "device_readings" => Some(r.clone()),
            _ => None,
        })
        .expect("the stream describes device_readings");

    assert_eq!(relation.namespace, "public");
    assert_eq!(relation.replica_identity, ReplicaIdentity::Default);
    assert!(relation.replica_identity.identifies_rows());

    let names: Vec<&str> = relation.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["id", "device_id", "reading", "payload", "observed_at"]
    );

    // Only the primary key participates in identity under the default setting.
    let keys: Vec<&str> = relation
        .columns
        .iter()
        .filter(|c| c.is_key)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(keys, ["id"], "only the primary key should be a key column");

    // Type identifiers as PostgreSQL assigns them: int8, text, numeric, text, timestamptz.
    let oids: Vec<u32> = relation.columns.iter().map(|c| c.type_oid).collect();
    assert_eq!(oids, [20, 25, 1700, 25, 1184]);

    // numeric(12,4) carries its precision and scale in the modifier; the others do not.
    let reading = &relation.columns[2];
    assert!(
        reading.type_modifier > 0,
        "numeric(12,4) should carry a type modifier, got {}",
        reading.type_modifier
    );
}

#[test]
fn real_update_withholds_the_unchanged_large_value() {
    // This is the assertion the whole fixture exists for. The update changed only
    // `reading`, so PostgreSQL transmits the 12 KB `payload` as an unchanged marker
    // rather than as data. A decoder that folded that into a null would silently
    // destroy the column, and the resulting row would look entirely correct.
    let messages = decode_stream(REAL_STREAM);
    let update = messages
        .iter()
        .find_map(|m| match m {
            Message::Update { new, .. } => Some(new.clone()),
            _ => None,
        })
        .expect("the stream contains an update");

    assert!(
        update.has_unchanged(),
        "the update should withhold the untouched large value; values: {:?}",
        update
            .values
            .iter()
            .map(std::mem::discriminant)
            .collect::<Vec<_>>()
    );

    let payload = &update.values[3];
    assert_eq!(*payload, TupleValue::Unchanged);
    assert!(
        !payload.is_present(),
        "an unchanged value carries nothing writable"
    );
    assert_ne!(
        *payload,
        TupleValue::Null,
        "unchanged must never be confused with null"
    );
}

#[test]
fn real_delete_carries_only_the_key() {
    let messages = decode_stream(REAL_STREAM);
    let (old, key_only) = messages
        .iter()
        .find_map(|m| match m {
            Message::Delete { old, key_only, .. } => Some((old.clone(), *key_only)),
            _ => None,
        })
        .expect("the stream contains a delete");

    assert!(
        key_only,
        "under the default replica identity a delete sends only the key"
    );
    // Non-key columns are null in a key-only image — present but empty, which is
    // different again from unchanged.
    assert!(
        matches!(old.values[0], TupleValue::Text(_)),
        "the key must be present"
    );
}

#[test]
fn real_insert_carries_every_column() {
    let messages = decode_stream(REAL_STREAM);
    let insert = messages
        .iter()
        .find_map(|m| match m {
            Message::Insert { new, .. } => Some(new.clone()),
            _ => None,
        })
        .expect("the stream contains an insert");

    assert_eq!(insert.values.len(), 5);
    assert!(
        !insert.has_unchanged(),
        "an insert has no previous version to withhold"
    );
    assert!(insert.values.iter().all(TupleValue::is_present));
}

#[test]
fn every_prefix_of_the_real_stream_is_handled_without_panic() {
    // Real streams arrive in fragments. Every partial read must yield either a
    // message or a clean error — never a panic and never a mis-parse.
    let decoder = Decoder::new();
    for body in framed_messages(REAL_STREAM) {
        for cut in 0..body.len() {
            let _ = decoder.decode_prefix(&body[..cut]);
        }
    }
}

#[test]
fn corrupting_any_single_byte_never_panics() {
    // Bit-flip resilience over a realistic message shape, which random bytes rarely
    // reach. A panic in the capture loop is an availability incident in the source
    // database, so this is checked against real structure rather than noise alone.
    let decoder = Decoder::new();
    for body in framed_messages(REAL_STREAM) {
        for i in 0..body.len().min(1024) {
            let mut corrupted = body.to_vec();
            corrupted[i] ^= 0xFF;
            let _ = decoder.decode_prefix(&corrupted);
        }
    }
}
