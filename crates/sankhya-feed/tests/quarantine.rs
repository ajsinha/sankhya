//! What is kept when a record does not fit, and what makes it replayable.
//!
//! # The property these are protecting
//!
//! A quarantine is only worth having if a person can act on it. That means the record whole
//! rather than an error message, a code they can count without parsing prose, and the identity
//! of the configuration that refused it --- because a declaration changes, and *"why did this
//! fail in March"* is otherwise unanswerable.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Array, StringArray};
use sankhya_feed::bind::Unfit;
use sankhya_feed::declare::{
    Column, DateFrom, Declaration, Microbatch, Missing, Quarantine, Unknown,
};
use sankhya_feed::quarantine::{batch, code, fingerprint, schema, Refused, TABLE};

fn declaration() -> Declaration {
    Declaration {
        name: "orders".to_owned(),
        from: "/spool".to_owned(),
        schema: "sales".to_owned(),
        table: "orders".to_owned(),
        columns: vec![Column {
            name: "id".to_owned(),
            from: None,
            written_type: "int64".to_owned(),
            nullable: false,
            missing: Missing::Refuse,
        }],
        date: Some(DateFrom::Ingest),
        unknown: Unknown::Refuse,
        microbatch: Microbatch::default(),
        quarantine: Quarantine::default(),
    }
}

fn refused(payload: &str) -> Refused {
    Refused {
        feed: "orders".to_owned(),
        source: "/spool/2026-08-31.json".to_owned(),
        position: 41,
        arrived_at: 1_756_000_000_000_000,
        reason_code: "wrong-kind",
        reason: "`id` reads a whole number and this document has a string".to_owned(),
        declaration: fingerprint(&declaration()),
        payload: payload.to_owned(),
        data_date: 20_697,
    }
}

#[test]
fn the_record_is_kept_exactly_as_it_arrived() {
    // Whole, and unmodified. A record reduced to its error message cannot be replayed, and
    // replay is the only actual remedy for a source that was briefly wrong.
    let awkward = r#"{"id": "42", "note": "a \"quoted\" string, and a comma,"}"#;
    let assembled = batch(&[refused(awkward)]).expect("a batch");

    let payloads = assembled
        .column_by_name("payload")
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
        .expect("a payload column");
    assert_eq!(payloads.value(0), awkward);
}

#[test]
fn every_refusal_has_a_stable_code_that_is_not_its_sentence() {
    // A client counting kinds must not be counting substrings of prose. The moment it does,
    // the sentence becomes an API nobody meant to publish and nobody may reword.
    let kinds = [
        (Unfit::NotADocument, "not-a-document"),
        (
            Unfit::MissingKey { column: "id".to_owned(), key: "id".to_owned() },
            "missing-key",
        ),
        (Unfit::UnknownKey { key: "grew".to_owned() }, "unknown-key"),
        (
            Unfit::WrongKind {
                column: "id".to_owned(),
                wanted: "a whole number",
                found: "a string",
                because: "because",
            },
            "wrong-kind",
        ),
        (
            Unfit::OutOfRange {
                column: "id".to_owned(),
                wanted: "a whole number",
                value: "1e40".to_owned(),
            },
            "out-of-range",
        ),
        (Unfit::NullIntoNotNull { column: "id".to_owned() }, "null-into-not-null"),
    ];
    for (unfit, expected) in kinds {
        assert_eq!(code(&unfit), expected, "{unfit:?}");
    }
}

#[test]
fn the_schema_is_the_same_for_every_feed_and_carries_the_date_axis() {
    // Fixed on purpose. A per-feed quarantine schema would have to change whenever a feed
    // did — a migration triggered by the very edit that most needs somewhere to put the
    // records it has started refusing.
    let quarantine = schema();
    let fields: Vec<&str> = quarantine.fields().iter().map(|f| f.name().as_str()).collect();

    assert_eq!(
        fields,
        vec![
            "feed",
            "source",
            "position",
            "arrived_at",
            "reason_code",
            "reason",
            "declaration",
            "payload",
            sankhya_schema::DATA_DATE_COLUMN,
        ]
    );
    assert!(quarantine.fields().iter().all(|field| !field.is_nullable()));
    assert!(TABLE.starts_with("sank_"), "reserved, so a feed cannot land on it");
}

#[test]
fn the_fingerprint_moves_when_the_declaration_means_something_different() {
    let base = fingerprint(&declaration());

    // A type change, a nullability change, and a change of what a missing key means are each
    // a different meaning for the same column.
    let mut retyped = declaration();
    retyped.columns[0].written_type = "int32".to_owned();
    assert_ne!(fingerprint(&retyped), base);

    let mut nullable = declaration();
    nullable.columns[0].nullable = true;
    assert_ne!(fingerprint(&nullable), base);

    let mut forgiving = declaration();
    forgiving.columns[0].missing = Missing::Null;
    assert_ne!(fingerprint(&forgiving), base);

    let mut widened = declaration();
    widened.unknown = Unknown::Ignore;
    assert_ne!(fingerprint(&widened), base);

    let mut dated = declaration();
    dated.date = Some(DateFrom::Column { name: "id".to_owned() });
    assert_ne!(fingerprint(&dated), base);
}

#[test]
fn the_fingerprint_ignores_what_does_not_change_the_meaning() {
    // Where the files are read from is an operational detail: moving a spool directory does
    // not make the records refused before the move any less attributable.
    let mut moved = declaration();
    moved.from = "/mnt/elsewhere".to_owned();
    assert_eq!(fingerprint(&moved), fingerprint(&declaration()));

    // Nor does how often a batch closes.
    let mut faster = declaration();
    faster.microbatch = Microbatch { rows: 1, seconds: 1 };
    assert_eq!(fingerprint(&faster), fingerprint(&declaration()));
}

#[test]
fn two_columns_cannot_be_rearranged_into_the_same_fingerprint() {
    // Length-prefixed for this: `ab` reading `c` must not hash the same as `a` reading `bc`,
    // which is what a plain concatenation would do.
    let mut first = declaration();
    first.columns[0].name = "ab".to_owned();
    first.columns[0].from = Some("c".to_owned());

    let mut second = declaration();
    second.columns[0].name = "a".to_owned();
    second.columns[0].from = Some("bc".to_owned());

    assert_ne!(fingerprint(&first), fingerprint(&second));
}
