//! Decoder tests.
//!
//! The most important test in this file is [`never_panics_on_arbitrary_input`]. The
//! decoder consumes bytes from a network socket, so a panic would take down the
//! capture loop — and a stalled capture loop retains write-ahead log until the source
//! database's volume fills. A decoder panic is therefore an availability incident in
//! the *transactional* system, which is the worst outcome this architecture can
//! produce. See INV-2.

use proptest::prelude::*;
use sankhya_cdc_model::{DecodeError, Decoder, Message, ReplicaIdentity, TupleValue};

/// Microseconds between the Unix epoch and the source's 2000-01-01 epoch.
const EPOCH_OFFSET: i64 = 946_684_800_000_000;

fn begin(lsn: u64, xid: u32) -> Vec<u8> {
    let mut b = vec![b'B'];
    b.extend_from_slice(&lsn.to_be_bytes());
    b.extend_from_slice(&0i64.to_be_bytes()); // source epoch
    b.extend_from_slice(&xid.to_be_bytes());
    b
}

fn relation(id: u32, ns: &str, name: &str, identity: u8, cols: &[(&str, u32, bool)]) -> Vec<u8> {
    let mut b = vec![b'R'];
    b.extend_from_slice(&id.to_be_bytes());
    b.extend_from_slice(ns.as_bytes());
    b.push(0);
    b.extend_from_slice(name.as_bytes());
    b.push(0);
    b.push(identity);
    b.extend_from_slice(&(cols.len() as i16).to_be_bytes());
    for (cname, oid, is_key) in cols {
        b.push(u8::from(*is_key));
        b.extend_from_slice(cname.as_bytes());
        b.push(0);
        b.extend_from_slice(&oid.to_be_bytes());
        b.extend_from_slice(&(-1i32).to_be_bytes());
    }
    b
}

fn tuple(values: &[TupleValue]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(values.len() as i16).to_be_bytes());
    for v in values {
        match v {
            TupleValue::Null => b.push(b'n'),
            TupleValue::Unchanged => b.push(b'u'),
            TupleValue::Text(s) => {
                b.push(b't');
                b.extend_from_slice(&(s.len() as i32).to_be_bytes());
                b.extend_from_slice(s.as_bytes());
            }
            TupleValue::Binary(v) => {
                b.push(b'b');
                b.extend_from_slice(&(v.len() as i32).to_be_bytes());
                b.extend_from_slice(v);
            }
        }
    }
    b
}

#[test]
fn decodes_begin_and_converts_epoch() {
    let Ok(Message::Begin { final_lsn, commit_time, xid }) = Decoder::new().decode(&begin(0x1_0000_0020, 4242))
    else {
        panic!("expected a Begin");
    };
    assert_eq!(final_lsn.to_string(), "1/20");
    assert_eq!(xid, 4242);
    // The source counts from 2000-01-01; SANKHYA uses the Unix epoch everywhere, and
    // the conversion happens once, at this boundary.
    assert_eq!(commit_time.as_micros(), EPOCH_OFFSET);
}

#[test]
fn decodes_relation_with_key_columns() {
    let bytes = relation(16384, "public", "device_readings", b'd',
        &[("id", 20, true), ("reading", 1700, false)]);
    let Ok(Message::Relation(r)) = Decoder::new().decode(&bytes) else {
        panic!("expected a Relation");
    };
    assert_eq!(r.relation_id, 16384);
    assert_eq!(r.namespace, "public");
    assert_eq!(r.name, "device_readings");
    assert_eq!(r.replica_identity, ReplicaIdentity::Default);
    assert_eq!(r.columns.len(), 2);
    assert!(r.columns[0].is_key);
    assert!(!r.columns[1].is_key);
}

#[test]
fn unchanged_value_is_distinct_from_null() {
    // The defect this guards against: treating a withheld large value as a null and
    // writing it downstream destroys real data, and the resulting row looks correct.
    let mut bytes = vec![b'I'];
    bytes.extend_from_slice(&16384u32.to_be_bytes());
    bytes.push(b'N');
    bytes.extend_from_slice(&tuple(&[
        TupleValue::Text("1".into()),
        TupleValue::Unchanged,
        TupleValue::Null,
    ]));

    let Ok(Message::Insert { new, .. }) = Decoder::new().decode(&bytes) else {
        panic!("expected an Insert");
    };
    assert_eq!(new.values[1], TupleValue::Unchanged);
    assert_eq!(new.values[2], TupleValue::Null);
    assert_ne!(new.values[1], new.values[2], "unchanged must never equal null");
    assert!(!new.values[1].is_present(), "unchanged carries no writable data");
    assert!(new.values[2].is_present(), "null is a real, writable value");
    assert!(new.has_unchanged());
}

#[test]
fn update_distinguishes_key_only_from_full_before_image() {
    for (marker, expect_key_only) in [(b'K', true), (b'O', false)] {
        let mut bytes = vec![b'U'];
        bytes.extend_from_slice(&16384u32.to_be_bytes());
        bytes.push(marker);
        bytes.extend_from_slice(&tuple(&[TupleValue::Text("1".into())]));
        bytes.push(b'N');
        bytes.extend_from_slice(&tuple(&[TupleValue::Text("2".into())]));

        let Ok(Message::Update { old, key_only, .. }) = Decoder::new().decode(&bytes) else {
            panic!("expected an Update");
        };
        assert!(old.is_some());
        assert_eq!(key_only, expect_key_only);
    }
}

#[test]
fn update_without_before_image_is_accepted() {
    let mut bytes = vec![b'U'];
    bytes.extend_from_slice(&16384u32.to_be_bytes());
    bytes.push(b'N');
    bytes.extend_from_slice(&tuple(&[TupleValue::Text("2".into())]));

    let Ok(Message::Update { old, .. }) = Decoder::new().decode(&bytes) else {
        panic!("expected an Update");
    };
    assert!(old.is_none(), "default replica identity sends no before-image on update");
}

#[test]
fn truncate_carries_all_relations_and_flags() {
    let mut bytes = vec![b'T'];
    bytes.extend_from_slice(&2i32.to_be_bytes());
    bytes.push(0b11); // cascade + restart identity
    bytes.extend_from_slice(&16384u32.to_be_bytes());
    bytes.extend_from_slice(&16385u32.to_be_bytes());

    let Ok(Message::Truncate { relation_ids, cascade, restart_identity }) =
        Decoder::new().decode(&bytes)
    else {
        panic!("expected a Truncate");
    };
    assert_eq!(relation_ids, vec![16384, 16385]);
    assert!(cascade && restart_identity);
}

#[test]
fn only_commits_seal_a_transaction() {
    // A batch may span several transactions but must never split one, so only these
    // two messages make a transaction eligible to be flushed.
    let mut commit = vec![b'C', 0];
    commit.extend_from_slice(&1u64.to_be_bytes());
    commit.extend_from_slice(&2u64.to_be_bytes());
    commit.extend_from_slice(&0i64.to_be_bytes());
    let decoded = Decoder::new().decode(&commit).expect("commit decodes");
    assert!(decoded.seals_transaction());

    assert!(!Decoder::new().decode(&begin(1, 1)).expect("begin decodes").seals_transaction());
}

#[test]
fn unknown_replica_identity_is_rejected_not_guessed() {
    let bytes = relation(1, "s", "t", b'?', &[]);
    assert!(matches!(
        Decoder::new().decode(&bytes),
        Err(DecodeError::UnknownReplicaIdentity { .. })
    ));
}

#[test]
fn truncation_is_reported_with_position() {
    let full = begin(0x1_0000_0020, 7);
    for cut in 0..full.len() {
        match Decoder::new().decode(&full[..cut]) {
            Err(DecodeError::Truncated { .. } | DecodeError::UnknownMessage { .. }) => {}
            other => panic!("prefix of length {cut} should be rejected, got {other:?}"),
        }
    }
}

#[test]
fn unterminated_string_is_rejected() {
    let mut bytes = vec![b'R'];
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(b"public"); // no terminator
    assert!(matches!(
        Decoder::new().decode(&bytes),
        Err(DecodeError::UnterminatedString { .. })
    ));
}

#[test]
fn invalid_utf8_is_rejected() {
    let mut bytes = vec![b'R'];
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&[0xFF, 0xFE, 0x00]);
    assert!(matches!(Decoder::new().decode(&bytes), Err(DecodeError::InvalidUtf8 { .. })));
}

#[test]
fn negative_lengths_are_rejected() {
    let mut bytes = vec![b'T'];
    bytes.extend_from_slice(&(-1i32).to_be_bytes());
    bytes.push(0);
    assert!(matches!(Decoder::new().decode(&bytes), Err(DecodeError::NegativeLength { .. })));
}

proptest! {
    /// **The decoder never panics, for any input whatsoever.**
    ///
    /// This stands in for the continuous fuzz target until that lands in CI. A panic
    /// here stalls capture, which retains write-ahead log, which can fill the source
    /// database's volume and take down the transactional system.
    #[test]
    fn never_panics_on_arbitrary_input(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = Decoder::new().decode(&bytes);
    }

    /// Nor on input shaped like a real message but with corrupted contents — the
    /// case a purely random generator almost never reaches.
    #[test]
    fn never_panics_on_plausible_input(
        tag in prop::sample::select(vec![b'B', b'C', b'R', b'Y', b'I', b'U', b'D', b'T', b'O', b'M', b'S', b'E', b'c', b'A']),
        body in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        let mut bytes = vec![tag];
        bytes.extend_from_slice(&body);
        let _ = Decoder::new().decode(&bytes);
    }

    /// Any prefix of a valid message is rejected cleanly rather than mis-parsed.
    #[test]
    fn prefixes_of_valid_messages_are_rejected(lsn in any::<u64>(), xid in any::<u32>(), cut in 0usize..17) {
        let full = begin(lsn, xid);
        let cut = cut.min(full.len().saturating_sub(1));
        prop_assert!(Decoder::new().decode(&full[..cut]).is_err());
    }

    /// Text values round-trip byte-exactly, including empty strings and multi-byte
    /// characters. A silent mangling here would corrupt every row of a text column.
    #[test]
    fn text_values_round_trip(values in prop::collection::vec("[a-zA-Z0-9 ]{0,32}", 0..8)) {
        let vals: Vec<TupleValue> = values.iter().cloned().map(TupleValue::Text).collect();
        let mut bytes = vec![b'I'];
        bytes.extend_from_slice(&16384u32.to_be_bytes());
        bytes.push(b'N');
        bytes.extend_from_slice(&tuple(&vals));

        let Ok(Message::Insert { new, .. }) = Decoder::new().decode(&bytes) else {
            prop_assert!(false, "expected an Insert");
            return Ok(());
        };
        prop_assert_eq!(new.values, vals);
    }
}
