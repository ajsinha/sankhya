//! What a clone records, and what a table that is not one answers.
//!
//! # The distinction these are mostly about
//!
//! *Not a clone* and *a clone whose lineage cannot be read* are different answers, and
//! collapsing them is how the whole mechanism fails silently: a table whose lineage is
//! unreadable is one its origin's sweeper cannot know about, and answering "not a clone" there
//! is answering *"nothing else reads these files"* on no evidence at all.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_clone::lineage::{Lineage, Malformed, CLONED_AT, ORIGIN, VERSION};
use std::collections::BTreeMap;

const T0: i64 = 1_700_000_000_000_000;

#[test]
fn a_lineage_survives_a_round_trip_through_table_properties() {
    let lineage = Lineage::new("entries", 40, T0);
    let properties = lineage.to_properties();

    assert_eq!(properties.get(ORIGIN).map(String::as_str), Some("entries"));
    assert_eq!(properties.get(VERSION).map(String::as_str), Some("40"));
    assert_eq!(properties.get(CLONED_AT).map(String::as_str), Some(T0.to_string().as_str()));

    assert_eq!(Lineage::from_properties(&properties), Some(Ok(lineage)));
}

#[test]
fn an_ordinary_table_is_not_a_clone_and_that_is_not_a_failure() {
    // Every table that exists today answers this way, which is the reason the whole mechanism
    // costs nothing until somebody clones something.
    assert_eq!(Lineage::from_properties(&BTreeMap::new()), None);

    let unrelated = BTreeMap::from([
        ("delta.appendOnly".to_string(), "true".to_string()),
        ("sankhya.something.else".to_string(), "1".to_string()),
    ]);
    assert_eq!(Lineage::from_properties(&unrelated), None);
}

#[test]
fn a_table_claiming_to_be_a_clone_with_no_origin_is_refused_not_ignored() {
    // The distinction the whole mechanism rests on. Returning `None` here would say "nothing
    // else reads these files", which is the sentence that deletes a clone's data.
    let broken = BTreeMap::from([(VERSION.to_string(), "40".to_string())]);
    assert_eq!(Lineage::from_properties(&broken), Some(Err(Malformed::NoOrigin)));
}

#[test]
fn an_origin_with_no_version_is_refused() {
    // Without the version the lineage says only that two tables are related, which is not
    // enough to answer what the clone should contain.
    let broken = BTreeMap::from([(ORIGIN.to_string(), "entries".to_string())]);
    assert_eq!(Lineage::from_properties(&broken), Some(Err(Malformed::NoVersion)));
}

#[test]
fn a_version_that_is_not_a_version_is_refused_and_quoted_back() {
    let broken = BTreeMap::from([
        (ORIGIN.to_string(), "entries".to_string()),
        (VERSION.to_string(), "yesterday".to_string()),
    ]);
    assert_eq!(
        Lineage::from_properties(&broken),
        Some(Err(Malformed::UnreadableVersion { found: "yesterday".to_string() }))
    );
}

#[test]
fn an_origin_that_is_only_whitespace_is_no_origin() {
    let broken = BTreeMap::from([
        (ORIGIN.to_string(), "   ".to_string()),
        (VERSION.to_string(), "40".to_string()),
    ]);
    assert_eq!(Lineage::from_properties(&broken), Some(Err(Malformed::NoOrigin)));
}

#[test]
fn a_missing_timestamp_does_not_make_a_lineage_unreadable() {
    // The origin and the version are what a reclamation decision needs. When it was taken is
    // provenance, and refusing the whole record for want of it would turn a cosmetic gap into
    // a table whose origin cannot sweep.
    let terse = BTreeMap::from([
        (ORIGIN.to_string(), "entries".to_string()),
        (VERSION.to_string(), "40".to_string()),
    ]);
    let lineage = Lineage::from_properties(&terse).unwrap().unwrap();
    assert_eq!(lineage.origin, "entries");
    assert_eq!(lineage.version, 40);
    assert_eq!(lineage.cloned_at, 0);
}

#[test]
fn a_refusal_says_why_it_is_not_treated_as_an_ordinary_table() {
    let said = Malformed::NoOrigin.to_string();
    assert!(said.contains("not therefore treated as an ordinary table"), "{said}");
    assert!(said.contains("sweeper"), "{said}");
}

#[test]
fn the_properties_are_namespaced_so_a_foreign_reader_ignores_them() {
    // The open-storage claim is kept by writing only what the format defines. A vendor prefix
    // in `configuration` is a table property; a new log action would be a bet that every reader
    // ignores what it does not recognise.
    for key in Lineage::new("entries", 1, T0).to_properties().keys() {
        assert!(key.starts_with("sankhya.clone."), "`{key}` is not namespaced");
    }
}

#[test]
fn a_lineage_says_what_it_is_in_a_sentence() {
    assert_eq!(
        Lineage::new("entries", 40, T0).to_string(),
        "cloned from `entries` at version 40"
    );
}
