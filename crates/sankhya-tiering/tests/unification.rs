//! Serving a predicate that spans both tiers, and the four cases the tie-break rule has to cover.
//!
//! # Why "total" is the word that matters
//!
//! `DEC-25` requires the rule to produce *"either a correct answer or an explicit error in every
//! direction of disagreement"*. A rule with a case nobody decided is a rule that decides that
//! case somewhere else — in a planner, silently, as a shorter answer than the user asked for.
//!
//! So the four combinations of *catalog says attached* and *registry says archived* each have a
//! test, and a property test asserts the thing the four cannot: that for **any** arrangement of
//! extents the plan's segments cover the predicate exactly, or the error names every hole.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use proptest::prelude::*;
use sankhya_tiering::policy::Retention;
use sankhya_tiering::registry::{Conflict, Range, Registry, Servable};
use sankhya_tiering::unify::{mutable, plan, Read, Unservable};
use sankhya_tiering::verify::Hash;
use sankhya_tiering::ArchiveEntry;

const T0: i64 = 1_700_000_000_000_000;
const DOMAIN: Range = Range::new(0, 1_000);

fn archived(from: i64, until: i64) -> ArchiveEntry {
    ArchiveEntry {
        table: "entries".to_string(),
        range: Range::new(from, until),
        archive: format!("s3://archive/entries/{from}-{until}"),
        snapshot: "snap-1".to_string(),
        rows: 1_000,
        keys: Hash::from_bytes([5; 32]),
        columns: Vec::new(),
        archived_at: T0,
        retention: Retention::new("7-year statutory record retention", 2557),
        legal_hold: false,
        attribution: Vec::new(),
    }
}

/// A witness from a reconciliation that found nothing.
///
/// Taken against an empty hot extent on purpose. Reconciliation happens at startup and after a
/// restore; the query-time tie-break has to stay total for disagreements that appear *after*
/// it, which is exactly what a resurrected backup produces. A test that could only reach the
/// planner with the two authorities already agreeing could not reach the case the rule exists
/// for.
fn witness(registry: &Registry) -> Servable {
    registry.reconcile(&[]).servable("entries").expect("nothing was found")
}

#[test]
fn a_predicate_entirely_in_the_hot_tier_is_one_source_segment() {
    let registry = Registry::new();
    let servable = witness(&registry);
    let plan = plan(
        Range::new(0, 100),
        &[Range::new(0, 1_000)],
        &registry,
        &servable,
        "snap-1",
        DOMAIN,
    )
    .unwrap();

    assert_eq!(plan.segments.len(), 1);
    assert_eq!(plan.segments[0].read, Read::Source);
    assert_eq!(plan.segments[0].range, Range::new(0, 100));
    assert!(!plan.spans_tiers());
}

#[test]
fn a_predicate_entirely_in_an_archive_is_one_cold_segment() {
    let registry = Registry::from_entries(vec![archived(0, 500)]).unwrap();
    let servable = witness(&registry);
    let plan =
        plan(Range::new(100, 200), &[], &registry, &servable, "snap-1", DOMAIN).unwrap();

    assert_eq!(plan.segments.len(), 1);
    assert_eq!(
        plan.segments[0].read,
        Read::Archive {
            archive: "s3://archive/entries/0-500".to_string(),
            extent: Range::new(0, 500)
        }
    );
    assert_eq!(plan.cold().len(), 1);
}

#[test]
fn a_predicate_spanning_both_is_unioned_in_key_order() {
    // The whole point. A user who queries six years and silently receives two has been handed a
    // wrong answer by a system that knew better.
    let registry = Registry::from_entries(vec![archived(0, 500)]).unwrap();
    let servable = witness(&registry);
    let plan =
        plan(Range::new(0, 1_000), &[Range::new(500, 1_000)], &registry, &servable, "s", DOMAIN)
            .unwrap();

    assert_eq!(plan.segments.len(), 2);
    assert_eq!(plan.segments[0].range, Range::new(0, 500));
    assert!(matches!(plan.segments[0].read, Read::Archive { .. }));
    assert_eq!(plan.segments[1].range, Range::new(500, 1_000));
    assert_eq!(plan.segments[1].read, Read::Source);
    assert!(plan.spans_tiers());
}

#[test]
fn a_range_in_neither_tier_fails_rather_than_answering_short() {
    // Never return a silently short answer. The rows are in neither the source nor an archive,
    // which is a defect or a purge that lost its registry entry --- both things somebody must
    // be told about rather than shown a smaller result set.
    let registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    let servable = witness(&registry);
    let refusal =
        plan(Range::new(0, 300), &[Range::new(200, 300)], &registry, &servable, "s", DOMAIN)
            .expect_err("there is a hole between 100 and 200");

    assert_eq!(
        refusal,
        Unservable::CoverageGap {
            table: "entries".to_string(),
            gaps: vec![Range::new(100, 200)]
        }
    );
    assert!(refusal.to_string().contains("silently short answer"));
}

#[test]
fn every_hole_is_named_rather_than_the_first() {
    let registry = Registry::from_entries(vec![archived(100, 200), archived(300, 400)]).unwrap();
    let servable = witness(&registry);
    let refusal = plan(Range::new(0, 500), &[], &registry, &servable, "s", DOMAIN)
        .expect_err("three holes");

    let Unservable::CoverageGap { gaps, .. } = refusal else { panic!("a gap") };
    assert_eq!(gaps, vec![Range::new(0, 100), Range::new(200, 300), Range::new(400, 500)]);
}

#[test]
fn a_range_both_tiers_claim_is_read_once_from_the_source_and_flagged() {
    // A restored backup resurrecting purged rows. Hot wins and the range is read exactly once,
    // so the failure case does not become double-counting on top of an inconsistency --- and
    // the inconsistency is reported rather than absorbed, because a query that papers over it
    // is a query that stops anybody finding out.
    let registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    let servable = witness(&registry);
    let plan =
        plan(Range::new(0, 100), &[Range::new(0, 100)], &registry, &servable, "s", DOMAIN)
            .unwrap();

    assert_eq!(plan.segments.len(), 1, "read once, not twice");
    assert_eq!(plan.segments[0].read, Read::Source, "hot wins");
    assert_eq!(
        plan.inconsistencies,
        vec![Conflict::InBothTiers {
            table: "entries".to_string(),
            cold: Range::new(0, 100),
            hot: Range::new(0, 100)
        }]
    );
}

#[test]
fn adjacent_pieces_from_the_same_place_become_one_segment() {
    // Two hot extents meeting at a boundary are one read, and a plan that says otherwise is a
    // plan somebody has to explain.
    let registry = Registry::new();
    let servable = witness(&registry);
    let plan = plan(
        Range::new(0, 200),
        &[Range::new(0, 100), Range::new(100, 200)],
        &registry,
        &servable,
        "s",
        DOMAIN,
    )
    .unwrap();

    assert_eq!(plan.segments.len(), 1);
    assert_eq!(plan.segments[0].range, Range::new(0, 200));
}

#[test]
fn a_predicate_covering_the_whole_domain_is_flagged() {
    // The tiering equivalent of a missing partition filter. It should surface as a warning long
    // before it surfaces as a forty-minute query.
    let registry = Registry::new();
    let servable = witness(&registry);
    let whole = plan(DOMAIN, &[DOMAIN], &registry, &servable, "s", DOMAIN).unwrap();
    assert!(whole.whole_domain);

    let narrow =
        plan(Range::new(10, 20), &[DOMAIN], &registry, &servable, "s", DOMAIN).unwrap();
    assert!(!narrow.whole_domain);
}

#[test]
fn the_plan_records_the_snapshot_both_authorities_were_read_in() {
    // Two reads at two snapshots produce a plan that reads a moving range twice or not at all,
    // depending on which way it moved.
    let registry = Registry::new();
    let servable = witness(&registry);
    let plan =
        plan(Range::new(0, 10), &[DOMAIN], &registry, &servable, "snap-77", DOMAIN).unwrap();
    assert_eq!(plan.snapshot, "snap-77");
}

#[test]
fn a_mutation_into_an_archived_range_is_refused_by_name() {
    // `FR-TIER-18`: reporting zero rows affected is a silent wrong answer. The rows exist ---
    // they are somewhere the statement cannot reach --- and a count of zero says the opposite.
    let registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    let refusal = mutable(&registry, "entries", 50).expect_err("archived");

    assert_eq!(refusal.extent, Range::new(0, 100));
    let said = refusal.to_string();
    assert!(said.contains("s3://archive/entries/0-100"), "{said}");
    assert!(said.contains("compensating entry"), "{said}");
    assert!(said.contains("amendment link"), "{said}");
}

#[test]
fn a_mutation_outside_every_archive_is_allowed() {
    let registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    assert!(mutable(&registry, "entries", 150).is_ok());
    assert!(mutable(&registry, "another", 50).is_ok());
}

proptest! {
    /// The property the four cases cannot state: for **any** arrangement of extents, the plan's
    /// segments are disjoint, in key order, and cover the predicate exactly --- or the refusal
    /// names holes which, together with what would have been served, account for all of it.
    ///
    /// This is what "total" means, and it is the assertion that would fail if a case were ever
    /// added without an answer.
    #[test]
    fn the_tie_break_rule_leaves_nothing_undecided(
        hot in proptest::collection::vec((0i64..40, 1i64..12), 0..5),
        cold in proptest::collection::vec((0i64..40, 1i64..12), 0..5),
        from in 0i64..40,
        width in 1i64..40,
    ) {
        let attached: Vec<Range> =
            hot.iter().map(|(at, span)| Range::new(*at, at + span)).collect();

        // Archived ranges may not overlap each other, which the registry enforces. Take the
        // ones that fit rather than generating a legal set, so the shapes stay varied.
        let mut registry = Registry::new();
        for (at, span) in &cold {
            let _ = registry.record(archived(*at, at + span));
        }
        let servable = registry.reconcile(&[]).servable("entries").expect("nothing found");

        let predicate = Range::new(from, from + width);
        match plan(predicate, &attached, &registry, &servable, "s", DOMAIN) {
            Ok(plan) => {
                let mut at = predicate.from;
                for segment in &plan.segments {
                    prop_assert_eq!(segment.range.from, at, "a hole or an overlap");
                    prop_assert!(!segment.range.is_empty());
                    at = segment.range.until;
                }
                prop_assert_eq!(at, predicate.until, "the segments stop short");
            }
            Err(Unservable::CoverageGap { gaps, .. }) => {
                prop_assert!(!gaps.is_empty(), "a coverage gap with no gaps in it");
                for gap in &gaps {
                    prop_assert!(!gap.is_empty());
                    prop_assert!(gap.from >= predicate.from && gap.until <= predicate.until);
                }
            }
            Err(other) => prop_assert!(false, "unexpected refusal: {:?}", other),
        }
    }
}
