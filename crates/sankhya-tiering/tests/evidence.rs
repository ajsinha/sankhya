//! What an archive can prove about itself, years after everyone has left.
//!
//! # The requirement is about what is *not* needed to read it
//!
//! `FR-TIER-35`: a signed evidence pack per archive, **generatable years later from the
//! write-once manifest alone**. Not from the registry, which lives in a database that may not
//! exist; not from this software, which may not build; not from a key server, an object
//! catalogue or a runbook.
//!
//! The question being answered is *"here is an archive and a marker; what is this, where did it
//! come from, who authorised it, and is it intact?"* — asked by somebody who was not there,
//! about a system nobody still runs.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_tiering::evidence::{hmac_sha256, Pack, Seal, Unreadable};
use sankhya_tiering::policy::Retention;
use sankhya_tiering::registry::Range;
use sankhya_tiering::verify::Hash;
use sankhya_tiering::ArchiveEntry;

const T0: i64 = 1_700_000_000_000_000;

fn entry() -> ArchiveEntry {
    ArchiveEntry {
        table: "entries".to_string(),
        range: Range::new(0, 100),
        archive: "s3://archive/entries/0-100".to_string(),
        snapshot: "snap-1".to_string(),
        rows: 1_000,
        keys: Hash::from_bytes([7; 32]),
        columns: Vec::new(),
        archived_at: T0,
        retention: Retention::new("7-year statutory record retention", 2557),
        legal_hold: false,
        attribution: vec![
            ("principal".to_string(), "alex".to_string()),
            ("change_reference".to_string(), "CHG-4471".to_string()),
        ],
    }
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn a_pack_is_built_from_the_marker_and_nothing_else() {
    let pack = Pack::from_marker(&entry().marker()).unwrap();

    assert_eq!(pack.get("table"), Some("entries"));
    assert_eq!(pack.get("range"), Some("[0, 100)"));
    assert_eq!(pack.get("archive"), Some("s3://archive/entries/0-100"));
    assert_eq!(pack.get("snapshot"), Some("snap-1"));
    assert_eq!(pack.get("rows"), Some("1000"));
    assert_eq!(pack.get("keys"), Some(Hash::from_bytes([7; 32]).to_string().as_str()));
}

#[test]
fn a_retention_basis_is_a_sentence_and_survives_being_one() {
    // The reason the marker is one `key=value` per line rather than one line: a value containing
    // spaces needs no escaping when the separator is a newline, and a format with no escaping is
    // a format with nothing to get wrong years later.
    let pack = Pack::from_marker(&entry().marker()).unwrap();
    assert_eq!(pack.get("retention_basis"), Some("7-year statutory record retention"));
    assert_eq!(pack.get("retention_days"), Some("2557"));
}

#[test]
fn the_pack_still_names_who_authorised_the_purge() {
    // "The scheduler did it" is not an acceptable audit answer, and neither is a pack that has
    // forgotten who asked.
    let pack = Pack::from_marker(&entry().marker()).unwrap();
    let attribution = pack.attribution();
    assert!(attribution.contains(&("principal", "alex")));
    assert!(attribution.contains(&("change_reference", "CHG-4471")));
}

#[test]
fn a_key_this_version_has_never_heard_of_is_ignored() {
    // A reader written today must not fail on a marker written by a later version. That is the
    // only way "generatable years later" survives contact with a schema change.
    let mut marker = entry().marker();
    marker.push_str("\nsomething_added_in_2031=whatever");

    let pack = Pack::from_marker(&marker).unwrap();
    assert_eq!(pack.get("something_added_in_2031"), Some("whatever"));
    assert_eq!(pack.get("table"), Some("entries"));
}

#[test]
fn blank_lines_and_lines_without_a_separator_are_skipped() {
    let marker = format!("{}\n\n   \na line with no equals sign\n", entry().marker());
    assert!(Pack::from_marker(&marker).is_ok());
}

#[test]
fn a_value_containing_an_equals_sign_keeps_all_of_it() {
    // The first `=` is the separator, so a query string or a base64 tail in an archive location
    // arrives intact.
    let pack = Pack::from_marker(
        "table=t\nrange=[0, 1)\narchive=s3://b/k?a=1&b=2\nsnapshot=s\nrows=1\nkeys=ab",
    )
    .unwrap();
    assert_eq!(pack.get("archive"), Some("s3://b/k?a=1&b=2"));
}

#[test]
fn a_marker_missing_required_keys_names_every_one_of_them() {
    // Somebody holding a damaged marker wants to know how damaged.
    let refusal = Pack::from_marker("table=entries\nrows=10").expect_err("four are missing");
    assert_eq!(
        refusal,
        Unreadable::Missing {
            keys: vec![
                "range".to_string(),
                "archive".to_string(),
                "snapshot".to_string(),
                "keys".to_string()
            ]
        }
    );
    assert!(refusal.to_string().contains("is not evidence"));
}

#[test]
fn a_seal_verifies_against_the_pack_it_was_taken_over() {
    let pack = Pack::from_marker(&entry().marker()).unwrap();
    let seal = pack.seal(b"an operator's key");

    assert!(pack.sealed_by(b"an operator's key", &seal));
    assert!(!pack.sealed_by(b"a different key", &seal));
}

#[test]
fn altering_one_character_of_the_evidence_breaks_the_seal() {
    let pack = Pack::from_marker(&entry().marker()).unwrap();
    let seal = pack.seal(b"an operator's key");

    let tampered =
        Pack::from_marker(&entry().marker().replace("rows=1000", "rows=1001")).unwrap();
    assert!(!tampered.sealed_by(b"an operator's key", &seal));
}

#[test]
fn a_marker_with_its_lines_rearranged_is_the_same_evidence() {
    // The seal is a function of the content rather than of the order the marker happened to be
    // written in — otherwise re-emitting a marker would invalidate its own seal.
    let marker = entry().marker();
    let mut lines: Vec<&str> = marker.lines().collect();
    lines.reverse();

    let forwards = Pack::from_marker(&marker).unwrap();
    let backwards = Pack::from_marker(&lines.join("\n")).unwrap();

    assert_eq!(forwards.canonical(), backwards.canonical());
    assert_eq!(forwards.seal(b"k").to_bytes(), backwards.seal(b"k").to_bytes());
}

#[test]
fn a_seal_survives_a_round_trip_through_its_bytes() {
    let pack = Pack::from_marker(&entry().marker()).unwrap();
    let seal = pack.seal(b"k");
    assert!(pack.sealed_by(b"k", &Seal::from_bytes(seal.to_bytes())));
    assert_eq!(seal.to_string().len(), 64, "thirty-two bytes as hexadecimal");
}

#[test]
fn hmac_matches_the_published_vectors() {
    // `RFC 4231`. The way to make an implementation of a standard construction trustworthy is
    // not care but published test vectors, and these are the reason writing it out here rather
    // than adding an unreviewed dependency is defensible.

    // Case 1: a twenty-byte key.
    assert_eq!(
        hex(hmac_sha256(&[0x0b; 20], b"Hi There")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );

    // Case 2: a key shorter than the block.
    assert_eq!(
        hex(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );

    // Case 3: a longer message.
    assert_eq!(
        hex(hmac_sha256(&[0xaa; 20], &[0xdd; 50])),
        "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
    );

    // Case 6: **a key longer than the block**, which is the branch implementations get wrong —
    // it has to be hashed first, not truncated.
    assert_eq!(
        hex(hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

#[test]
fn hmac_is_not_a_bare_hash_of_the_key_and_message() {
    // The mistake this rules out is the one that looks identical in a unit test that only checks
    // "different inputs, different outputs": concatenation is not a message authentication code,
    // because a length-extension attack forges from it.
    use sha2::{Digest, Sha256};
    let mut naive = Sha256::new();
    naive.update(b"key");
    naive.update(b"message");
    let naive: [u8; 32] = naive.finalize().into();

    assert_ne!(hmac_sha256(b"key", b"message"), naive);
}

#[test]
fn a_pack_prints_itself_as_the_marker_it_came_from() {
    // So that a person holding a pack can put it back where they found it, and the round trip is
    // what makes "generatable from the manifest alone" checkable rather than asserted.
    let pack = Pack::from_marker(&entry().marker()).unwrap();
    let again = Pack::from_marker(&pack.to_string()).unwrap();
    assert_eq!(pack, again);
}
