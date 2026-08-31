//! The archival registry: what it will not record, what it cannot answer for, and what it
//! refuses to let anybody expire.
//!
//! # The failures these are about
//!
//! A registry that admits two entries for the same rows has made a choice at query time that
//! nobody can explain. A registry that reports a range as covered when part of it is not has
//! turned a missing archive into a short answer. And a registry that lets a snapshot expire
//! while an entry still points at it has turned *"the archive is provably the data"* into
//! *"the archive is what we have"*.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_schema::{LogicalType, Precision};
use sankhya_tiering::policy::{has_ordinal, Retention};
use sankhya_tiering::registry::{Conflict, Range, Registry, Rejected};
use sankhya_tiering::verify::Hash;
use sankhya_tiering::ArchiveEntry;

const DAY: i64 = 86_400 * 1_000_000;
const T0: i64 = 1_700_000_000_000_000;

fn entry(table: &str, from: i64, until: i64, snapshot: &str) -> ArchiveEntry {
    ArchiveEntry {
        table: table.to_string(),
        range: Range::new(from, until),
        archive: format!("s3://archive/{table}/{from}-{until}"),
        snapshot: snapshot.to_string(),
        rows: 1_000,
        keys: Hash::from_bytes([7; 32]),
        columns: Vec::new(),
        archived_at: T0,
        retention: Retention::new("7-year statutory record retention", 2557),
        legal_hold: false,
        attribution: vec![("principal".to_string(), "alex".to_string())],
    }
}

#[test]
fn two_entries_claiming_the_same_rows_are_refused_where_somebody_can_explain_it() {
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();

    let clash = registry.record(entry("entries", 50, 150, "snap-2"));
    assert_eq!(
        clash,
        Err(Rejected::Overlaps {
            offered: Range::new(50, 150),
            existing: Range::new(0, 100)
        })
    );
    assert_eq!(registry.entries().len(), 1, "the ambiguity was never created");
}

#[test]
fn adjacent_ranges_do_not_overlap_because_the_bound_is_exclusive() {
    // With inclusive bounds these two either share a day or leave a hole on one, and which
    // happened depends on whoever wrote the second entry.
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();
    registry.record(entry("entries", 100, 200, "snap-2")).unwrap();
    assert_eq!(registry.entries().len(), 2);
}

#[test]
fn the_same_range_of_a_different_table_is_not_a_clash() {
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();
    registry.record(entry("others", 0, 100, "snap-1")).unwrap();
    assert_eq!(registry.entries().len(), 2);
}

#[test]
fn an_archive_of_nothing_is_not_a_record() {
    let mut registry = Registry::new();
    assert_eq!(
        registry.record(entry("entries", 100, 100, "snap-1")),
        Err(Rejected::EmptyRange { offered: Range::new(100, 100) })
    );
}

#[test]
fn a_fully_covered_range_has_no_gaps() {
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();
    registry.record(entry("entries", 100, 200, "snap-2")).unwrap();

    let coverage = registry.coverage("entries", Range::new(0, 200));
    assert!(coverage.is_complete(), "{:?}", coverage.gaps);
    assert_eq!(coverage.archives.len(), 2);
}

#[test]
fn a_hole_between_two_archives_is_reported_rather_than_assumed_hot() {
    // `FR-TIER-17`: an uncovered range intersecting the predicate is a coverage-gap error. A
    // query that quietly returns fewer rows than exist is the failure tiering is most able to
    // cause and least able to detect.
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 50, "snap-1")).unwrap();
    registry.record(entry("entries", 80, 200, "snap-2")).unwrap();

    let coverage = registry.coverage("entries", Range::new(0, 200));
    assert_eq!(coverage.gaps, vec![Range::new(50, 80)]);
}

#[test]
fn a_hole_at_each_end_is_reported_too() {
    let mut registry = Registry::new();
    registry.record(entry("entries", 40, 60, "snap-1")).unwrap();

    let coverage = registry.coverage("entries", Range::new(0, 100));
    assert_eq!(coverage.gaps, vec![Range::new(0, 40), Range::new(60, 100)]);
}

#[test]
fn a_range_no_entry_claims_at_all_is_one_gap() {
    let registry = Registry::new();
    let coverage = registry.coverage("entries", Range::new(0, 100));
    assert_eq!(coverage.gaps, vec![Range::new(0, 100)]);
    assert!(coverage.archives.is_empty());
}

#[test]
fn another_tables_archives_do_not_cover_this_one() {
    let mut registry = Registry::new();
    registry.record(entry("others", 0, 100, "snap-1")).unwrap();

    let coverage = registry.coverage("entries", Range::new(0, 100));
    assert_eq!(coverage.gaps, vec![Range::new(0, 100)]);
}

#[test]
fn a_snapshot_an_entry_still_needs_cannot_be_expired() {
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();

    let pins = registry.pins(T0 + DAY);
    let refusal = pins.may_expire("snap-1").expect_err("still needed");
    assert!(refusal.to_string().contains("snap-1"));
    assert!(pins.may_expire("snap-unrelated").is_ok());
}

#[test]
fn the_pin_lifts_when_the_retention_basis_lapses() {
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();

    let after = T0 + 2558 * DAY;
    assert!(registry.pins(after).is_empty(), "2557 days is the whole basis");
    assert!(registry.pins(after).may_expire("snap-1").is_ok());
}

#[test]
fn a_legal_hold_outlives_the_retention_basis() {
    // A hold with an end date is a retention basis. The ones that matter do not have one.
    let mut held = entry("entries", 0, 100, "snap-1");
    held.legal_hold = true;
    let mut registry = Registry::new();
    registry.record(held).unwrap();

    let long_after = T0 + 20_000 * DAY;
    assert_eq!(registry.pins(long_after).len(), 1);
}

#[test]
fn a_range_in_both_tiers_is_a_conflict_and_the_table_stops_being_servable() {
    // `FR-TIER-23`: refuse rather than serve a plausible wrong answer.
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();

    let reconciliation = registry.reconcile(&[("entries".to_string(), Range::new(50, 150))]);
    assert_eq!(
        reconciliation.conflicts,
        vec![Conflict::InBothTiers {
            table: "entries".to_string(),
            cold: Range::new(0, 100),
            hot: Range::new(50, 150)
        }]
    );
    assert!(reconciliation.servable("entries").is_none());
    assert!(reconciliation.refused().contains("entries"));
}

#[test]
fn an_unaffected_table_is_still_servable() {
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();
    registry.record(entry("others", 0, 100, "snap-1")).unwrap();

    let reconciliation = registry.reconcile(&[("entries".to_string(), Range::new(50, 150))]);
    let servable = reconciliation.servable("others").expect("nothing said about it");
    assert_eq!(servable.table(), "others");
}

#[test]
fn a_hot_range_that_does_not_touch_an_archive_is_not_a_conflict() {
    let mut registry = Registry::new();
    registry.record(entry("entries", 0, 100, "snap-1")).unwrap();

    let reconciliation = registry.reconcile(&[("entries".to_string(), Range::new(100, 200))]);
    assert!(reconciliation.conflicts.is_empty());
    assert!(reconciliation.servable("entries").is_some());
}

#[test]
fn a_restore_that_lost_an_entry_reports_it_before_serving() {
    // `FR-TIER-24`. An entry that vanished across a restore is a range the system now believes
    // was never archived, and the first evidence would be a query answering from a source that
    // no longer holds it.
    let before = Registry::from_entries(vec![
        entry("entries", 0, 100, "snap-1"),
        entry("entries", 100, 200, "snap-2"),
    ])
    .unwrap();
    let after = Registry::from_entries(vec![entry("entries", 0, 100, "snap-1")]).unwrap();

    let delta = after.delta(&before);
    assert_eq!(delta.removed.len(), 1);
    assert_eq!(delta.removed[0].range, Range::new(100, 200));
    assert!(delta.added.is_empty());
    assert!(delta.to_string().contains("1 removed"));
}

#[test]
fn an_entry_whose_facts_changed_is_neither_added_nor_removed() {
    let before = Registry::from_entries(vec![entry("entries", 0, 100, "snap-1")]).unwrap();
    let mut moved = entry("entries", 0, 100, "snap-1");
    moved.archive = "s3://somewhere-else/entries".to_string();
    let after = Registry::from_entries(vec![moved]).unwrap();

    let delta = after.delta(&before);
    assert_eq!(delta.changed.len(), 1);
    assert!(delta.added.is_empty() && delta.removed.is_empty());
}

#[test]
fn two_identical_registries_have_no_delta() {
    let one = Registry::from_entries(vec![entry("entries", 0, 100, "snap-1")]).unwrap();
    let other = Registry::from_entries(vec![entry("entries", 0, 100, "snap-1")]).unwrap();
    assert!(one.delta(&other).is_empty());
    assert_eq!(one.delta(&other).to_string(), "the archival registry is unchanged");
}

#[test]
fn a_restore_of_an_ambiguous_registry_fails_rather_than_serving_from_it() {
    let ambiguous = Registry::from_entries(vec![
        entry("entries", 0, 100, "snap-1"),
        entry("entries", 50, 150, "snap-2"),
    ]);
    assert!(ambiguous.is_err());
}

#[test]
fn the_marker_names_what_a_reader_years_later_would_need() {
    // The reader of this is a person with a copy of an object store and no build of this
    // software, so it is one line of text rather than a format needing a parser.
    let marker = entry("entries", 0, 100, "snap-1").marker();
    for expected in [
        "table=entries",
        "range=[0, 100)",
        "snapshot=snap-1",
        "rows=1000",
        "principal=alex",
        "legal_hold=false",
    ] {
        assert!(marker.contains(expected), "`{expected}` missing from `{marker}`");
    }
    assert!(marker.contains(&Hash::from_bytes([7; 32]).to_string()));
}

#[test]
fn every_logical_type_has_a_decided_answer_about_being_a_tiering_key() {
    // The other half of the exhaustive match: `has_ordinal` refuses to compile when a logical
    // type is added, and this pins what was decided so a later change to admit one for
    // convenience has to argue with a test.
    for logical in [
        LogicalType::Int16,
        LogicalType::Int32,
        LogicalType::Int64,
        LogicalType::TimestampUtc,
        LogicalType::TimestampLocal,
        LogicalType::Date,
        LogicalType::Time,
        LogicalType::Decimal(Precision { digits: 38, scale: 9 }),
    ] {
        assert!(has_ordinal(&logical), "{logical:?} orders");
    }
    for logical in [
        LogicalType::Boolean,
        LogicalType::Utf8,
        LogicalType::Binary,
        LogicalType::Uuid,
        LogicalType::Json,
        LogicalType::Float32,
        LogicalType::Float64,
    ] {
        assert!(!has_ordinal(&logical), "{logical:?} does not");
    }
}
