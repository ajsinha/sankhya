//! A filtered total, and the trap that makes completeness read a hundred percent forever.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use proptest::prelude::*;
use sankhya_cube::complete::{Assessed, Completeness, Threshold};

// --- the trap -----------------------------------------------------------

#[test]
fn completeness_counted_from_what_survived_is_always_complete() {
    // The whole feature can be built and still read 100% forever. A withheld row leaves no
    // trace — a cell whose every row was removed is simply absent, indistinguishable from
    // one that never had data — so counting what arrived and dividing by what arrived gives
    // one, always.
    //
    // This test states the trap rather than testing the fix: the withheld count has to come
    // from the filter, which is why it is a parameter and not something derived here.
    let survived = 600_u64;
    let derived_from_output = Completeness::of(survived, 0);
    assert!(derived_from_output.is_complete(), "and it is wrong");

    let told_by_the_filter = Completeness::of(survived, 400);
    assert!(!told_by_the_filter.is_complete());
    assert_eq!(told_by_the_filter.fraction(), Some(0.6));
}

// --- an empty aggregate is not a complete one ---------------------------

#[test]
fn an_aggregate_over_nothing_has_no_completeness_rather_than_full_completeness() {
    // Rounding it up to 1.0 is how an empty result passes a threshold: the query returns
    // nothing, reports itself complete, and reconciles against a real figure as a zero.
    let nothing = Completeness::of(0, 0);
    assert_eq!(nothing.fraction(), None);
    assert!(!nothing.is_complete());
    assert!(!Threshold::COMPLETE.met_by(&nothing));

    let assessed = Assessed::new(0.0_f64, nothing);
    let refused = assessed.meeting(&Threshold::COMPLETE).expect_err("passed on nothing");
    assert!(
        refused.to_string().contains("no input at all"),
        "the message distinguishes empty from complete: {refused}"
    );
}

#[test]
fn everything_withheld_is_not_complete_and_is_not_empty() {
    let all_gone = Completeness::of(0, 500);
    assert_eq!(all_gone.fraction(), Some(0.0));
    assert!(!all_gone.is_complete());
    assert_eq!(all_gone.considered(), 500);
}

// --- the threshold ------------------------------------------------------

#[test]
fn a_total_below_the_threshold_is_refused_rather_than_returned() {
    // FR-QUERY-13: it fails rather than returning a flattering result. A caller who wants
    // the partial figure asks for it by name; one who does not is not handed it by default.
    let assessed = Assessed::new(4_182_900.0_f64, Completeness::of(600, 400));
    let threshold = Threshold::at_least(0.95).expect("a fraction");

    let refused = assessed.meeting(&threshold).expect_err("returned a partial total");
    assert_eq!(refused.withheld, 400);
    assert_eq!(refused.seen, Some(0.6));

    // The partial value is reachable, under a name that makes reading it a decision.
    assert_eq!(*assessed.regardless(), 4_182_900.0);
}

#[test]
fn a_total_meeting_the_threshold_is_returned() {
    let assessed = Assessed::new(42.0_f64, Completeness::of(990, 10));
    let threshold = Threshold::at_least(0.95).expect("a fraction");
    assert_eq!(assessed.meeting(&threshold), Ok(&42.0));
}

#[test]
fn a_threshold_exactly_met_passes() {
    // A boundary that rejects at equality would refuse a complete aggregate against
    // `COMPLETE`, and "the query failed" looks exactly like a genuine policy shortfall.
    let assessed = Assessed::new(1.0_f64, Completeness::complete(10));
    assert!(assessed.meeting(&Threshold::COMPLETE).is_ok());

    let half = Assessed::new(1.0_f64, Completeness::of(5, 5));
    assert!(half.meeting(&Threshold::at_least(0.5).expect("a fraction")).is_ok());
}

#[test]
fn a_threshold_that_is_not_a_fraction_is_refused_at_construction() {
    // 1.5 refuses everything and NaN compares false against everything, so both silently
    // reject every aggregate — and a rejection is indistinguishable from a real breach, so
    // nobody would question it.
    for bad in [1.5, -0.1, f64::NAN, f64::INFINITY] {
        assert!(Threshold::at_least(bad).is_err(), "{bad} was accepted as a threshold");
    }
    for good in [0.0, 0.5, 1.0] {
        assert!(Threshold::at_least(good).is_ok());
    }
}

// --- combining ----------------------------------------------------------

#[test]
fn combining_adds_counts_rather_than_averaging_fractions() {
    // A mean of fractions weights a cell of three rows the same as one of three million, so
    // a roll-up over one heavily filtered small cell and one complete large one reports
    // about half — a figure that is wrong in the direction of refusing good data.
    let tiny_and_filtered = Completeness::of(1, 2);
    let large_and_complete = Completeness::complete(999_997);

    let combined = tiny_and_filtered.and(large_and_complete);
    assert_eq!(combined.contributed(), 999_998);
    assert_eq!(combined.withheld(), 2);
    assert!(
        combined.fraction().expect("some input") > 0.999,
        "counts, not an average of 0.33 and 1.0"
    );
}

#[test]
fn combining_is_order_independent_and_associative() {
    let a = Completeness::of(3, 1);
    let b = Completeness::of(5, 2);
    let c = Completeness::of(0, 7);
    assert_eq!(a.and(b), b.and(a));
    assert_eq!(a.and(b).and(c), a.and(b.and(c)));
}

#[test]
fn a_complete_aggregate_stays_complete_when_combined_with_another() {
    assert!(Completeness::complete(4).and(Completeness::complete(6)).is_complete());
    assert!(!Completeness::complete(4).and(Completeness::of(6, 1)).is_complete());
}

// --- the value and its completeness travel together ---------------------

#[test]
fn mapping_a_value_keeps_its_completeness() {
    // Or the completeness is lost at the first formatting step, which is where it matters.
    let assessed = Assessed::new(3.0_f64, Completeness::of(6, 4));
    let rendered = assessed.map(|v| format!("{v:.2}"));
    assert_eq!(rendered.completeness().withheld(), 4);
    assert_eq!(rendered.regardless(), "3.00");
}

proptest! {
    /// Completeness is the contributed share of everything considered, never above one.
    #[test]
    fn a_fraction_is_always_between_zero_and_one(
        contributed in 0u64..1_000_000,
        withheld in 0u64..1_000_000
    ) {
        let completeness = Completeness::of(contributed, withheld);
        match completeness.fraction() {
            None => {
                prop_assert_eq!(contributed, 0);
                prop_assert_eq!(withheld, 0);
            }
            Some(fraction) => {
                prop_assert!((0.0..=1.0).contains(&fraction), "{fraction}");
                prop_assert_eq!(fraction >= 1.0, withheld == 0);
            }
        }
    }

    /// A threshold is met exactly when the fraction reaches it.
    #[test]
    fn the_threshold_is_a_simple_comparison_with_no_gaps(
        contributed in 1u64..1000,
        withheld in 0u64..1000,
        required in 0.0f64..=1.0
    ) {
        let completeness = Completeness::of(contributed, withheld);
        let threshold = Threshold::at_least(required).expect("in range");
        let seen = completeness.fraction().expect("some input");
        prop_assert_eq!(threshold.met_by(&completeness), seen >= required);
    }
}
