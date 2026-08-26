//! Splice-planner tests.
//!
//! These assert the property the whole read path rests on: a query is answered from a
//! set of tiers that covers the requested span **exactly once**. Both failure modes —
//! counting a row twice and losing one — return a plausible number, so neither would
//! be caught by inspection.

use proptest::prelude::*;
use sankhya_plan::{SpliceError, TierRef, is_exact_cover, plan_splice};
use sankhya_types::{Lsn, LsnRange};

fn tier(name: &'static str, from: u64, to: u64) -> TierRef {
    TierRef::new(name, LsnRange::new(Lsn::new(from), Lsn::new(to)).expect("valid range"))
}

#[test]
fn a_single_tier_covering_everything_is_used_alone() {
    let tiers = [tier("published", 0, 1000)];
    let splice = plan_splice(&tiers, Lsn::new(1000)).expect("plans");
    assert_eq!(splice.tier_names(), ["published"]);
    assert!(is_exact_cover(&splice.tiers, Lsn::new(1000)));
}

#[test]
fn published_and_buffer_abut_exactly() {
    // The canonical arrangement: committed data up to a point, in-memory beyond it.
    let tiers = [tier("published", 0, 900), tier("buffer", 900, 1000)];
    let splice = plan_splice(&tiers, Lsn::new(1000)).expect("plans");
    assert_eq!(splice.tier_names(), ["published", "buffer"]);

    // The seam is exact: neither tier claims position 900 twice, and nothing falls
    // between them.
    let ranges: Vec<_> = splice.tiers.iter().map(|t| t.coverage).collect();
    assert!(ranges[0].abuts(ranges[1]));
    assert!(!ranges[0].overlaps(ranges[1]));
}

#[test]
fn a_gap_refuses_the_query_and_names_the_missing_span() {
    // The alternative — returning what is available — is a silently short answer, and
    // nothing about it looks wrong.
    let tiers = [tier("published", 0, 400), tier("buffer", 700, 1000)];
    let err = plan_splice(&tiers, Lsn::new(1000)).expect_err("must refuse");
    let SpliceError::CoverageGap { from, to } = err else {
        panic!("expected a coverage gap, got {err:?}");
    };
    assert_eq!(from, Lsn::new(400));
    assert_eq!(to, Lsn::new(700), "the operator needs the exact missing span");
}

#[test]
fn requesting_beyond_the_frontier_is_refused() {
    let tiers = [tier("published", 0, 500)];
    let err = plan_splice(&tiers, Lsn::new(900)).expect_err("must refuse");
    let SpliceError::BeyondFrontier { requested, available } = err else {
        panic!("expected beyond-frontier, got {err:?}");
    };
    assert_eq!(requested, Lsn::new(900));
    assert_eq!(available, Lsn::new(500));
}

#[test]
fn overlapping_tiers_are_trimmed_rather_than_double_counted() {
    // Tiers legitimately overlap — a buffer often still holds what was just published.
    // The planner must produce a non-overlapping cover regardless.
    let tiers = [tier("published", 0, 800), tier("buffer", 600, 1000)];
    let splice = plan_splice(&tiers, Lsn::new(1000)).expect("plans");

    assert!(is_exact_cover(&splice.tiers, Lsn::new(1000)));
    for pair in splice.tiers.windows(2) {
        assert!(
            !pair[0].coverage.overlaps(pair[1].coverage),
            "selected tiers must never overlap: {:?}",
            splice.tiers
        );
    }
}

#[test]
fn the_shortest_cover_is_preferred() {
    // Fewer tiers means fewer scans. Given a choice, the planner takes the tier that
    // reaches furthest.
    let tiers = [
        tier("small_a", 0, 100),
        tier("small_b", 100, 200),
        tier("wide", 0, 900),
        tier("buffer", 900, 1000),
    ];
    let splice = plan_splice(&tiers, Lsn::new(1000)).expect("plans");
    assert_eq!(splice.tier_names(), ["wide", "buffer"]);
}

#[test]
fn tier_order_does_not_matter() {
    let forward = [tier("published", 0, 900), tier("buffer", 900, 1000)];
    let reversed = [tier("buffer", 900, 1000), tier("published", 0, 900)];
    let a = plan_splice(&forward, Lsn::new(1000)).expect("plans");
    let b = plan_splice(&reversed, Lsn::new(1000)).expect("plans");
    assert_eq!(a.tier_names(), b.tier_names());
}

#[test]
fn a_partial_target_trims_the_last_tier() {
    // Reading as of an earlier position must not drag in data beyond it.
    let tiers = [tier("published", 0, 900), tier("buffer", 900, 1000)];
    let splice = plan_splice(&tiers, Lsn::new(950)).expect("plans");
    assert_eq!(splice.target, Lsn::new(950));
    assert_eq!(
        splice.tiers.last().expect("a tier").coverage.end_inclusive(),
        Lsn::new(950),
        "the final tier must be trimmed to the target"
    );
    assert!(is_exact_cover(&splice.tiers, Lsn::new(950)));
}

#[test]
fn an_empty_target_needs_no_tiers() {
    assert!(plan_splice(&[], Lsn::ZERO).expect("plans").tiers.is_empty());
}

#[test]
fn no_tiers_at_all_is_refused_rather_than_answered_empty() {
    let err = plan_splice(&[], Lsn::new(1)).expect_err("must refuse");
    assert!(matches!(err, SpliceError::BeyondFrontier { .. }));
}

#[test]
fn provenance_reports_every_tier_and_its_span() {
    let tiers = [tier("published", 0, 900), tier("buffer", 900, 1000)];
    let splice = plan_splice(&tiers, Lsn::new(1000)).expect("plans");
    let provenance = splice.provenance();
    assert_eq!(provenance.len(), 2);
    assert_eq!(provenance[0].0, "published");
    assert_eq!(provenance[1].1.end_inclusive(), Lsn::new(1000));
}

proptest! {
    /// Whenever the planner succeeds, its result is an exact cover.
    ///
    /// This is the property that matters: not that the planner is clever, but that it
    /// never returns something unsound.
    #[test]
    fn success_always_yields_an_exact_cover(
        spans in prop::collection::vec((0u64..500, 1u64..200), 1..8),
        target_raw in 1u64..700,
    ) {
        let owned: Vec<(u64, u64)> = spans.iter().map(|(s, l)| (*s, s + l)).collect();
        let tiers: Vec<TierRef> = owned
            .iter()
            .map(|(from, to)| {
                TierRef::new("t", LsnRange::new(Lsn::new(*from), Lsn::new(*to)).expect("valid"))
            })
            .collect();
        let target = Lsn::new(target_raw);

        if let Ok(splice) = plan_splice(&tiers, target) {
            prop_assert!(
                is_exact_cover(&splice.tiers, target),
                "planner returned an unsound cover: {:?}", splice.tiers
            );
            // And never any overlap between consecutive selections.
            for pair in splice.tiers.windows(2) {
                prop_assert!(!pair[0].coverage.overlaps(pair[1].coverage));
            }
        }
    }

    /// A contiguous chain always plans, and always uses the whole chain's span.
    #[test]
    fn contiguous_chains_always_plan(lengths in prop::collection::vec(1u64..100, 1..10)) {
        let mut bounds = Vec::new();
        let mut at = 0u64;
        for len in &lengths {
            bounds.push((at, at + len));
            at += len;
        }
        let tiers: Vec<TierRef> = bounds
            .iter()
            .map(|(f, t)| TierRef::new("t", LsnRange::new(Lsn::new(*f), Lsn::new(*t)).expect("valid")))
            .collect();

        let splice = plan_splice(&tiers, Lsn::new(at));
        prop_assert!(splice.is_ok(), "a contiguous chain must always plan");
        if let Ok(s) = splice {
            prop_assert!(is_exact_cover(&s.tiers, Lsn::new(at)));
        }
    }

    /// Planning never panics, whatever coverage it is handed.
    #[test]
    fn planning_never_panics(
        spans in prop::collection::vec((any::<u32>(), any::<u32>()), 0..12),
        target in any::<u32>(),
    ) {
        let tiers: Vec<TierRef> = spans
            .iter()
            .filter_map(|(a, b)| {
                let (lo, hi) = if a <= b { (*a, *b) } else { (*b, *a) };
                LsnRange::new(Lsn::new(u64::from(lo)), Lsn::new(u64::from(hi)))
                    .map(|r| TierRef::new("t", r))
            })
            .collect();
        let _ = plan_splice(&tiers, Lsn::new(u64::from(target)));
    }
}
