//! Moving a whole table to the published tier without the table going away.
//!
//! # Why the name is the requirement
//!
//! `FR-TIER-21` is short and easy to underrate: *"a table that vanishes breaks every downstream
//! tool and saved query"*. A table nobody has written to for four years is still named in
//! dashboards, in a report somebody runs each quarter, in a view three other views are built on,
//! and in the query a person pastes from a wiki page. Dropping the name turns one storage
//! decision into a morning of unrelated failures in places nobody connected to tiering.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_tiering::migrate::{migrate, Incomplete};
use sankhya_tiering::policy::Retention;
use sankhya_tiering::registry::{Range, Registry};
use sankhya_tiering::unify::mutable;
use sankhya_tiering::verify::Hash;
use sankhya_tiering::ArchiveEntry;

const T0: i64 = 1_700_000_000_000_000;
const DOMAIN: Range = Range::new(0, 300);

fn archived(from: i64, until: i64) -> ArchiveEntry {
    ArchiveEntry {
        table: "entries".to_string(),
        range: Range::new(from, until),
        archive: format!("s3://archive/entries/{from}-{until}"),
        snapshot: "snap-1".to_string(),
        rows: 1_000,
        keys: Hash::from_bytes([4; 32]),
        columns: Vec::new(),
        archived_at: T0,
        retention: Retention::new("7-year statutory record retention", 2557),
        legal_hold: false,
        attribution: Vec::new(),
    }
}

#[test]
fn a_wholly_archived_table_keeps_its_name() {
    let registry =
        Registry::from_entries(vec![archived(0, 100), archived(100, 300)]).unwrap();

    let cold = migrate(&registry, "entries", DOMAIN, T0).unwrap();

    assert_eq!(cold.name(), "entries", "the same name, which is the requirement");
    assert_eq!(cold.domain(), DOMAIN);
    assert_eq!(cold.archives().len(), 2);
    assert!(!cold.writable(), "cold and read-only");
}

#[test]
fn a_partly_archived_table_is_refused_with_every_hole() {
    // The trap the requirement creates. The table stays visible --- that is the point --- and a
    // visible table whose archive covers four of its five years answers four years of questions
    // without mentioning the fifth.
    let registry = Registry::from_entries(vec![archived(0, 100), archived(200, 300)]).unwrap();

    let refusal = migrate(&registry, "entries", DOMAIN, T0).expect_err("a hole at 100..200");

    assert_eq!(
        refusal,
        Incomplete::NotWhollyArchived {
            table: "entries".to_string(),
            gaps: vec![Range::new(100, 200)]
        }
    );
    assert!(refusal.to_string().contains("without mentioning the rest"));
}

#[test]
fn every_hole_is_named_rather_than_the_first() {
    let registry = Registry::from_entries(vec![archived(100, 200)]).unwrap();
    let refusal = migrate(&registry, "entries", DOMAIN, T0).expect_err("two holes");

    let Incomplete::NotWhollyArchived { gaps, .. } = refusal else { panic!("a coverage gap") };
    assert_eq!(gaps, vec![Range::new(0, 100), Range::new(200, 300)]);
}

#[test]
fn a_table_with_nothing_archived_is_refused_rather_than_migrated_empty() {
    let registry = Registry::new();
    let refusal = migrate(&registry, "entries", DOMAIN, T0).expect_err("nothing is archived");
    assert!(matches!(refusal, Incomplete::NotWhollyArchived { .. }));
}

#[test]
fn a_domain_covering_nothing_is_refused_rather_than_trivially_satisfied() {
    // "Wholly archived" over an empty domain is a claim about no rows, and an empty registry
    // would satisfy it. That is a migration of a table nobody checked.
    let registry = Registry::new();
    let refusal = migrate(&registry, "entries", Range::new(100, 100), T0)
        .expect_err("an empty domain");
    assert_eq!(refusal, Incomplete::EmptyDomain { table: "entries".to_string() });
    assert!(refusal.to_string().contains("empty registry"));
}

#[test]
fn another_tables_archives_do_not_migrate_this_one() {
    let mut registry = Registry::new();
    registry
        .record(ArchiveEntry { table: "others".to_string(), ..archived(0, 300) })
        .unwrap();

    let refusal = migrate(&registry, "entries", DOMAIN, T0).expect_err("wrong table");
    assert!(matches!(refusal, Incomplete::NotWhollyArchived { .. }));
}

#[test]
fn a_migrated_table_refuses_mutations_across_its_whole_domain() {
    // Cold and read-only is not a flag somebody reads; every key of the domain is inside an
    // archived range, so `FR-TIER-18`'s typed refusal applies at every point of it.
    let registry =
        Registry::from_entries(vec![archived(0, 100), archived(100, 300)]).unwrap();
    let cold = migrate(&registry, "entries", DOMAIN, T0).unwrap();

    for at in [cold.domain().from, 150, cold.domain().until - 1] {
        let refusal = mutable(&registry, "entries", at).expect_err("archived at {at}");
        assert!(refusal.to_string().contains("compensating entry"));
    }
}

#[test]
fn the_description_says_what_it_is_without_needing_the_requirement_open() {
    let registry = Registry::from_entries(vec![archived(0, 300)]).unwrap();
    let said = migrate(&registry, "entries", DOMAIN, T0).unwrap().to_string();

    assert!(said.contains("`entries`"), "{said}");
    assert!(said.contains("cold and read-only"), "{said}");
    assert!(said.contains("[0, 300)"), "{said}");
}
