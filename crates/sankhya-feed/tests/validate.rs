//! What a declaration must be before it is a feed.
//!
//! # Why every fault, and not the first
//!
//! A validator that stops at the first fault turns fixing a configuration into a sequence of
//! builds, and somewhere around the fourth the person stops fixing rules and starts removing
//! them. That is the failure this is written to prevent, so it is asserted directly rather
//! than assumed.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_feed::declare::{Column, Declaration, Microbatch, Missing, Quarantine, Unknown};
use sankhya_feed::validate::{validate, Fault};

/// A declaration that is entirely fine, for a test to spoil one part of.
fn sound() -> Declaration {
    Declaration {
        name: "orders".to_owned(),
        from: "/var/spool/orders".to_owned(),
        schema: "sales".to_owned(),
        table: "orders".to_owned(),
        columns: vec![
            Column {
                name: "id".to_owned(),
                from: None,
                written_type: "int64".to_owned(),
                nullable: false,
                missing: Missing::Refuse,
            },
            Column {
                name: "amount".to_owned(),
                from: Some("total".to_owned()),
                written_type: "decimal(18,2)".to_owned(),
                nullable: false,
                missing: Missing::Refuse,
            },
        ],
        unknown: Unknown::Refuse,
        microbatch: Microbatch::default(),
        quarantine: Quarantine::default(),
    }
}

#[test]
fn a_sound_declaration_becomes_a_feed() {
    let feed = validate(sound()).expect("a sound declaration");

    assert_eq!(feed.name(), "orders");
    assert_eq!(feed.columns().len(), 2);
    // The key defaults to the column's name, and is the declared one when there is one.
    assert_eq!(feed.columns()[0].key, "id");
    assert_eq!(feed.columns()[1].key, "total");
}

#[test]
fn the_defaults_are_the_careful_ones() {
    let feed = validate(sound()).expect("a sound declaration");

    assert_eq!(feed.unknown(), Unknown::Refuse, "a key nobody claimed is news");
    assert_eq!(feed.columns()[0].missing, Missing::Refuse, "a missing key is not a null");
    assert!(feed.quarantine().retain_days > 0, "a quarantine expires");
    assert!(feed.microbatch().rows > 0 && feed.microbatch().seconds > 0);
}

#[test]
fn every_failing_rule_is_reported_and_not_just_the_first() {
    let mut declaration = sound();
    declaration.name = String::new();
    declaration.quarantine.retain_days = 0;
    declaration.quarantine.stop_above = 1.0;
    declaration.microbatch.rows = 0;
    declaration.columns[1].written_type = "int".to_owned();

    let faults = validate(declaration).expect_err("five things are wrong");

    assert_eq!(faults.len(), 5, "one fault per broken rule: {faults:?}");
    assert!(faults.contains(&Fault::Empty { field: "name" }));
    assert!(faults.contains(&Fault::QuarantineForever));
    assert!(faults.contains(&Fault::UnusableStopRate { rate: 1.0 }));
    assert!(faults.contains(&Fault::UnboundedBatch { bound: "rows" }));
    assert!(faults.contains(&Fault::UnknownType {
        name: "amount".to_owned(),
        written: "int".to_owned(),
    }));
}

#[test]
fn an_unwritable_type_says_what_the_types_are() {
    let mut declaration = sound();
    declaration.columns[0].written_type = "bigint".to_owned();

    let faults = validate(declaration).expect_err("bigint is not a type here");
    let said = faults[0].to_string();

    // Naming the alternatives, because a refusal that only says "no" leaves the operator
    // guessing at spellings — and one of their guesses will be a word this accepts for
    // something else.
    assert!(said.contains("`bigint`"), "{said}");
    assert!(said.contains("int64"), "{said}");
    assert!(said.contains("decimal(digits,scale)"), "{said}");
}

#[test]
fn two_columns_reading_one_key_is_one_fault_naming_both() {
    let mut declaration = sound();
    declaration.columns[1].from = Some("id".to_owned());

    let faults = validate(declaration).expect_err("both read `id`");

    // One fault rather than a pair of them, and it names every column involved: three
    // columns on one key should be one sentence, not three.
    assert_eq!(
        faults,
        vec![Fault::DuplicateKey {
            key: "id".to_owned(),
            columns: vec!["id".to_owned(), "amount".to_owned()],
        }]
    );
}

#[test]
fn a_repeated_column_name_is_refused() {
    let mut declaration = sound();
    declaration.columns[1].name = "id".to_owned();
    declaration.columns[1].from = Some("total".to_owned());

    let faults = validate(declaration).expect_err("two columns called `id`");
    assert!(faults.contains(&Fault::DuplicateColumn { name: "id".to_owned() }));
}

#[test]
fn filling_a_not_null_column_with_null_is_refused() {
    // The mistake is quiet: the column is declared `NOT NULL`, the feed is told a missing key
    // means null, and every document lacking the key becomes a row that cannot be written —
    // discovered at publication rather than at configuration.
    let mut declaration = sound();
    declaration.columns[0].missing = Missing::Null;

    let faults = validate(declaration).expect_err("null into a not-null column");
    assert!(faults.contains(&Fault::NullIntoNotNull { name: "id".to_owned() }));
}

#[test]
fn a_nullable_column_may_say_a_missing_key_means_null() {
    let mut declaration = sound();
    declaration.columns[0].missing = Missing::Null;
    declaration.columns[0].nullable = true;

    let feed = validate(declaration).expect("said by name, on a column that accepts it");
    assert_eq!(feed.columns()[0].missing, Missing::Null);
}

#[test]
fn a_stop_rate_that_cannot_fire_or_fires_on_everything_is_refused() {
    for rate in [0.0, 1.0, -0.5, 2.0] {
        let mut declaration = sound();
        declaration.quarantine.stop_above = rate;
        let faults = validate(declaration).expect_err("not a usable rate");
        assert!(
            faults.contains(&Fault::UnusableStopRate { rate }),
            "{rate} should be refused: {faults:?}"
        );
    }
}

#[test]
fn a_feed_with_no_columns_is_refused() {
    let mut declaration = sound();
    declaration.columns.clear();

    let faults = validate(declaration).expect_err("no columns");
    assert!(faults.contains(&Fault::NoColumns));
    assert!(
        faults[0].to_string().contains("evidence that something ran"),
        "the refusal says what the mistake produces"
    );
}
