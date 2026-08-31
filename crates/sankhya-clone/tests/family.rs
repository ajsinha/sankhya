//! Which tables a reclamation decision has to consult instead of one.
//!
//! # The set that matters, and the wider one that is tempting
//!
//! For a sweep of table `T`'s root, the tables that can still name a file there are `T` and
//! everything cloned from `T`, transitively. Ancestors are not in that set: a clone's inherited
//! files live under the **origin's** root, and the origin cannot name a file the clone wrote
//! after the split because it never heard of it.
//!
//! "The whole family" is the intuitive answer and it is wider than necessary. Wider is safe
//! here — it keeps files — but a set that is wider than its justification is one nobody can
//! reason about later, so the two are separate operations with separate names.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_clone::family::{Cycle, Lineages};
use sankhya_clone::lineage::Lineage;
use std::collections::BTreeSet;

const T0: i64 = 1_700_000_000_000_000;

fn named(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// `entries` cloned to `staging`, `staging` cloned to `scratch`, and an unrelated `other`.
fn tree() -> Lineages {
    let mut lineages = Lineages::new();
    lineages.record("staging", Lineage::new("entries", 40, T0));
    lineages.record("scratch", Lineage::new("staging", 3, T0));
    lineages.record("sibling", Lineage::new("entries", 41, T0));
    lineages
}

#[test]
fn a_warehouse_with_no_clones_changes_no_reclamation_decision() {
    // The fast path every existing deployment takes, and the reason this costs nothing until
    // somebody clones something.
    let lineages = Lineages::new();
    assert!(lineages.is_empty());
    assert_eq!(lineages.readers_of("entries").unwrap(), named(&["entries"]));
    assert!(lineages.dependents("entries").unwrap().is_empty());
}

#[test]
fn a_sweep_must_consult_every_clone_beneath_the_table_transitively() {
    // `scratch` is a clone of a clone. Its files are under `staging`'s root and `entries`'s, so
    // both sweepers have to know about it.
    assert_eq!(
        tree().readers_of("entries").unwrap(),
        named(&["entries", "staging", "scratch", "sibling"])
    );
    assert_eq!(tree().readers_of("staging").unwrap(), named(&["staging", "scratch"]));
    assert_eq!(tree().readers_of("scratch").unwrap(), named(&["scratch"]));
}

#[test]
fn ancestors_are_not_readers_of_a_clones_own_files() {
    // The narrowing that needs an argument: `entries` cannot name a file `staging` wrote after
    // the split, because `entries` never heard of it. Including ancestors would be safe and
    // wider than its justification.
    let readers = tree().readers_of("staging").unwrap();
    assert!(!readers.contains("entries"), "an origin does not read its clone's later files");
    assert!(!readers.contains("sibling"), "nor does a sibling");
}

#[test]
fn the_family_is_the_wider_set_and_is_reachable_from_any_member() {
    // For "may this be dropped" and "what must a backup include", the whole tree matters, and
    // asking from a leaf must give the same answer as asking from the root.
    let whole = named(&["entries", "staging", "scratch", "sibling"]);
    for member in ["entries", "staging", "scratch", "sibling"] {
        assert_eq!(tree().family(member).unwrap(), whole, "asked from {member}");
    }
}

#[test]
fn the_chain_of_origins_is_nearest_first() {
    assert_eq!(tree().ancestors("scratch").unwrap(), vec!["staging", "entries"]);
    assert_eq!(tree().ancestors("staging").unwrap(), vec!["entries"]);
    assert!(tree().ancestors("entries").unwrap().is_empty());
}

#[test]
fn dependents_names_what_removing_a_table_would_break() {
    // The refusal `ADR-0016` added: dropping an origin a clone still references is the deletion
    // the whole design exists to prevent, arriving through the front door.
    assert_eq!(tree().dependents("entries").unwrap(), named(&["staging", "scratch", "sibling"]));
    assert_eq!(tree().dependents("staging").unwrap(), named(&["scratch"]));
    assert!(tree().dependents("scratch").unwrap().is_empty(), "a leaf breaks nothing");
}

#[test]
fn a_lineage_cycle_is_refused_rather_than_walked() {
    // It cannot arise from cloning --- an origin exists before its clone --- so it means a
    // table's properties were edited. A resolver that trusted the construction would hang, and
    // a sweep that hangs stops every reclamation in the warehouse until somebody notices.
    let mut looped = Lineages::new();
    looped.record("a", Lineage::new("b", 1, T0));
    looped.record("b", Lineage::new("a", 1, T0));

    assert_eq!(looped.ancestors("a"), Err(Cycle { at: "a".to_string() }));
    assert!(looped.readers_of("a").is_err());
    assert!(looped.family("a").is_err());
}

#[test]
fn a_table_pointing_at_itself_is_a_cycle_too() {
    let mut looped = Lineages::new();
    looped.record("a", Lineage::new("a", 1, T0));
    assert_eq!(looped.ancestors("a"), Err(Cycle { at: "a".to_string() }));
}

#[test]
fn a_cycle_elsewhere_does_not_refuse_an_unrelated_table() {
    // A corrupt record is a reason to refuse decisions about the tables it touches, not to stop
    // reclaiming everywhere. The failure is contained to the family it is in.
    let mut lineages = tree();
    lineages.record("x", Lineage::new("y", 1, T0));
    lineages.record("y", Lineage::new("x", 1, T0));

    assert!(lineages.readers_of("entries").is_ok());
    assert!(lineages.readers_of("x").is_err());
}

#[test]
fn the_refusal_says_it_cannot_have_come_from_cloning() {
    let said = Cycle { at: "a".to_string() }.to_string();
    assert!(said.contains("properties were edited"), "{said}");
    assert!(said.contains("hang"), "{said}");
}

#[test]
fn a_clone_of_a_table_that_was_never_recorded_is_still_a_reader() {
    // The origin need not be known to this map for the clone to name its files. A sweep of an
    // origin that has no lineage entry of its own still has to see what was cloned from it.
    let mut lineages = Lineages::new();
    lineages.record("staging", Lineage::new("entries", 40, T0));
    assert_eq!(lineages.of("entries"), None, "the origin is not itself a clone");
    assert_eq!(lineages.readers_of("entries").unwrap(), named(&["entries", "staging"]));
}

#[test]
fn a_sweep_keeps_the_versions_direct_clones_still_read() {
    // `ADR-0016`'s Decision 1a: a clone records an origin and a version rather than naming the
    // origin's files, so the sweeper's question is about its own log — which versions of me does
    // somebody still read?
    let mut lineages = Lineages::new();
    lineages.record("staging", Lineage::new("entries", 40, T0));
    lineages.record("sibling", Lineage::new("entries", 41, T0));
    lineages.record("also", Lineage::new("entries", 40, T0));

    assert_eq!(lineages.pinned_versions("entries"), BTreeSet::from([40, 41]));
}

#[test]
fn transitivity_does_not_compound_into_the_root() {
    // The simplification Decision 1a buys, and the one most likely to be got wrong the other
    // way. `scratch` is a clone of `staging`, so it pins a version of *staging* — staging's own
    // files are protected by staging's sweeper, and staging's dependence on `entries` is
    // expressed by staging's own pin of version 40.
    assert_eq!(tree().pinned_versions("entries"), BTreeSet::from([40, 41]));
    assert_eq!(tree().pinned_versions("staging"), BTreeSet::from([3]));
    assert!(tree().pinned_versions("scratch").is_empty(), "nothing is cloned from a leaf");
}

#[test]
fn a_table_nobody_cloned_pins_no_version_at_all() {
    // Every table that exists. The sweep then does exactly what it does today, which is the
    // property that makes this affordable.
    assert!(Lineages::new().pinned_versions("entries").is_empty());
    assert!(tree().pinned_versions("unrelated").is_empty());
}
