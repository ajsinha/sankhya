//! The seven ways a clone operation is refused, and one test that they are all still there.
//!
//! # Why they are worth reading together
//!
//! Each is the same failure arriving from a different direction — silent data loss in a table
//! nobody was touching, found when somebody reads a clone months later. `ADR-0016` named four
//! and building the list found three more, which is the argument for enumerating them in one
//! place: a list nobody can read end to end is a list nobody can check.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_clone::action::{clone_table, lineage_of};
use sankhya_clone::lineage::Lineage;
use sankhya_clone::refuse::{may_clone, may_drop, may_purge, may_read_as_of, Origin, Refused, Request};
use sankhya_clone::Lineages;
use sankhya_table_delta::Action;

const T0: i64 = 1_700_000_000_000_000;
const DAY: i64 = 86_400 * 1_000_000;

fn request() -> Request {
    Request {
        table: "staging".to_string(),
        tenant: "acme".to_string(),
        origin: "entries".to_string(),
        origin_tenant: "acme".to_string(),
        version: 40,
    }
}

fn healthy() -> Origin {
    Origin {
        latest_version: 52,
        earliest_retained_version: 12,
        purge_in_flight: false,
        schema_evolving: false,
    }
}

#[test]
fn an_ordinary_clone_is_allowed() {
    assert_eq!(may_clone(&request(), &healthy()), Ok(()));
}

#[test]
fn a_clone_whose_origin_is_being_purged_is_refused() {
    let purging = Origin { purge_in_flight: true, ..healthy() };
    let refusal = may_clone(&request(), &purging).expect_err("a purge is in flight");
    assert_eq!(refusal, Refused::OriginBeingPurged { origin: "entries".to_string() });
    assert!(refusal.to_string().contains("being detached"));
}

#[test]
fn a_clone_across_tenants_is_refused_before_anything_else_is_asked() {
    // Checked first because it is the one refusal that is never a timing problem and never
    // becomes true by waiting. A clone is a reference rather than a copy, so allowing it would
    // be a way to read another tenant's bytes without a grant.
    let across = Request { origin_tenant: "widgets".to_string(), ..request() };
    let broken_too = Origin { purge_in_flight: true, schema_evolving: true, ..healthy() };

    let refusal = may_clone(&across, &broken_too).expect_err("another tenant's table");
    assert_eq!(
        refusal,
        Refused::AcrossTenants {
            tenant: "acme".to_string(),
            origin_tenant: "widgets".to_string()
        }
    );
    assert!(refusal.to_string().contains("without a grant"));
}

#[test]
fn a_clone_of_a_table_mid_schema_evolution_is_refused() {
    let evolving = Origin { schema_evolving: true, ..healthy() };
    let refusal = may_clone(&request(), &evolving).expect_err("mid-evolution");
    assert_eq!(refusal, Refused::SchemaEvolving { origin: "entries".to_string() });
    assert!(refusal.to_string().contains("belong to neither"));
}

#[test]
fn a_version_the_origin_never_had_is_refused() {
    let ahead = Request { version: 99, ..request() };
    assert_eq!(
        may_clone(&ahead, &healthy()),
        Err(Refused::NoSuchVersion {
            origin: "entries".to_string(),
            wanted: 99,
            latest: 52
        })
    );
}

#[test]
fn a_version_the_origin_can_no_longer_reconstruct_is_refused_differently() {
    // The refusal ADR-0016 added, and the distinction that earns it its own variant: "never had
    // it" is a typo, and "had it and retired the files" is a clone that would be an empty table
    // wearing the name of a full one.
    let old = Request { version: 3, ..request() };
    let refusal = may_clone(&old, &healthy()).expect_err("retired");
    assert_eq!(
        refusal,
        Refused::VersionRetired {
            origin: "entries".to_string(),
            wanted: 3,
            earliest: 12
        }
    );
    assert!(refusal.to_string().contains("empty table wearing the name"));
}

#[test]
fn the_boundaries_of_the_retained_window_are_inside_it() {
    for version in [12, 52] {
        assert_eq!(may_clone(&Request { version, ..request() }, &healthy()), Ok(()));
    }
    assert!(may_clone(&Request { version: 11, ..request() }, &healthy()).is_err());
    assert!(may_clone(&Request { version: 53, ..request() }, &healthy()).is_err());
}

#[test]
fn dropping_an_origin_a_clone_still_reads_is_refused_and_names_them() {
    // The deletion this whole design exists to prevent, arriving through the front door.
    let mut clones = Lineages::new();
    clones.record("staging", Lineage::new("entries", 40, T0));
    clones.record("scratch", Lineage::new("staging", 3, T0));

    let refusal = may_drop("entries", &clones).expect_err("two clones read it");
    let Refused::StillRead { by, .. } = &refusal else { panic!("still read") };
    assert_eq!(by, &["scratch".to_string(), "staging".to_string()]);
    assert!(refusal.to_string().contains("materialise them first"));
}

#[test]
fn dropping_a_leaf_clone_is_allowed() {
    let mut clones = Lineages::new();
    clones.record("staging", Lineage::new("entries", 40, T0));
    assert_eq!(may_drop("staging", &clones), Ok(()));
}

#[test]
fn purging_a_table_a_clone_reads_is_refused_for_the_same_reason() {
    // A different operation refused on the same ground: M9's purge detaches and drops a
    // partition, and a clone naming those files would be reading data the registry says was
    // archived and the source says is gone.
    let mut clones = Lineages::new();
    clones.record("staging", Lineage::new("entries", 40, T0));
    assert!(may_purge("entries", &clones).is_err());
    assert_eq!(may_purge("untouched", &clones), Ok(()));
}

#[test]
fn a_tangled_lineage_refuses_the_drop_rather_than_guessing() {
    let mut looped = Lineages::new();
    looped.record("a", Lineage::new("b", 1, T0));
    looped.record("b", Lineage::new("a", 1, T0));

    let refusal = may_drop("a", &looped).expect_err("tangled");
    assert!(matches!(refusal, Refused::Tangled { .. }));
    assert!(refusal.to_string().contains("not a question with an answer"));
}

#[test]
fn reading_a_clone_from_before_it_existed_is_refused() {
    // Answering from the origin's history would give the clone a past it never had, and the
    // origin is queryable directly by anybody who wants the real one.
    let lineage = Lineage::new("entries", 40, T0);
    let refusal = may_read_as_of("staging", T0 - DAY, &lineage).expect_err("before it existed");
    assert!(matches!(refusal, Refused::BeforeTheClone { .. }));
    assert!(refusal.to_string().contains("a past it never had"));

    assert_eq!(may_read_as_of("staging", T0, &lineage), Ok(()));
    assert_eq!(may_read_as_of("staging", T0 + DAY, &lineage), Ok(()));
}

#[test]
fn creating_a_clone_commits_a_table_with_lineage_and_no_files() {
    // Decision 1a, as the thing that is actually written. There is nothing to add: the rows it
    // starts with are the origin's and stay where they are, which is what makes the clone
    // constant-time and the reclamation question answerable.
    let lineage = Lineage::new("entries", 40, T0);
    let actions = clone_table("staging", r#"{"type":"struct","fields":[]}"#, T0, &lineage);

    assert!(
        !actions.iter().any(|action| matches!(action, Action::Add(_))),
        "a clone that added files would be a copy"
    );
    assert_eq!(lineage_of(&actions), Some(Ok(lineage)));
}

#[test]
fn an_ordinary_table_creation_carries_no_lineage() {
    use sankhya_table_delta::{create, Metadata};
    let actions = create(Metadata::new("entries", r#"{"type":"struct","fields":[]}"#, T0));
    assert_eq!(lineage_of(&actions), None);
}

#[test]
fn every_refusal_says_what_to_do_or_why_there_is_nothing_to_do() {
    // The list, walked. A refusal that only says no leaves an operator to guess, and guessing
    // is what each of these exists to prevent.
    let refusals = [
        Refused::OriginBeingPurged { origin: "entries".to_string() },
        Refused::AcrossTenants {
            tenant: "acme".to_string(),
            origin_tenant: "widgets".to_string(),
        },
        Refused::SchemaEvolving { origin: "entries".to_string() },
        Refused::NoSuchVersion { origin: "entries".to_string(), wanted: 99, latest: 52 },
        Refused::VersionRetired { origin: "entries".to_string(), wanted: 3, earliest: 12 },
        Refused::StillRead { table: "entries".to_string(), by: vec!["staging".to_string()] },
        Refused::BeforeTheClone {
            table: "staging".to_string(),
            wanted: 0,
            cloned_at: T0,
            origin: "entries".to_string(),
        },
        Refused::Tangled { at: "a".to_string() },
    ];
    assert_eq!(refusals.len(), 8, "seven from the ADR, plus a tangled lineage");

    for refusal in &refusals {
        let said = refusal.to_string();
        // A floor rather than a measure. Sixty characters is about where a message stops being
        // able to state the fact and stop --- this caught `NoSuchVersion` saying only "has no
        // version 99; its newest is 52", which is true and leaves an operator to guess whether
        // waiting would help.
        assert!(said.len() > 60, "`{said}` states a fact without explaining it");
        assert!(
            said.contains("entries")
                || said.contains("staging")
                || said.contains("acme")
                || said.contains('`'),
            "`{said}` does not name what it is about"
        );
    }
}
