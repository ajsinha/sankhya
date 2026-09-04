//! Reduction that gives the same answer however the work was divided.
//!
//! The failure this prevents is a figure that will not tie out and nobody can explain:
//! too small to notice in testing, too large to reconcile in production.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use proptest::prelude::*;
use sankhya_math::{combine_partials, deterministic_sum};

/// Values of widely different magnitudes, where naive summation loses the small ones.
fn awkward() -> Vec<f64> {
    let mut out = vec![1e16, -1e16];
    out.extend((1..=1_000).map(f64::from));
    out.push(1e-8);
    out
}

#[test]
fn a_naive_sum_depends_on_order_and_this_one_does_not() {
    // The premise, checked rather than assumed. If the naive sums agreed, the fixture
    // would not be demonstrating the problem and the assertion below would be empty.
    let forward = awkward();
    let backward: Vec<f64> = forward.iter().rev().copied().collect();

    let naive_forward: f64 = forward.iter().sum();
    let naive_backward: f64 = backward.iter().sum();
    assert_ne!(
        naive_forward, naive_backward,
        "the fixture must actually expose order dependence"
    );

    assert_eq!(
        deterministic_sum(&forward),
        deterministic_sum(&backward),
        "the same values in a different order gave a different total"
    );
}

#[test]
fn the_canonical_sum_is_also_the_accurate_one() {
    // 1e16 and -1e16 cancel exactly; everything else should survive. A naive
    // left-to-right sum loses the small terms into the large running total.
    let values = awkward();
    let expected = (1..=1_000).map(f64::from).sum::<f64>() + 1e-8;
    let ours = deterministic_sum(&values);

    assert!(
        (ours - expected).abs() < 1e-6,
        "expected about {expected}, got {ours}"
    );
}

#[test]
fn partial_sums_combine_regardless_of_which_worker_finished_first() {
    // What a parallel reduction actually does: each worker sums its share, and the
    // results arrive in whatever order the scheduler produced them.
    let values = awkward();
    let shares: Vec<Vec<f64>> = values.chunks(97).map(<[f64]>::to_vec).collect();

    let mut partials: Vec<f64> = shares.iter().map(|s| deterministic_sum(s)).collect();
    let in_order = combine_partials(&partials);

    partials.reverse();
    let reversed = combine_partials(&partials);

    partials.rotate_left(3);
    let rotated = combine_partials(&partials);

    assert_eq!(in_order, reversed);
    assert_eq!(in_order, rotated);
}

#[test]
fn a_different_partitioning_gives_the_same_total() {
    // The case that matters operationally: the same query on a machine with a different
    // core count partitions differently.
    let values = awkward();
    let mut answers = Vec::new();
    for width in [1usize, 3, 16, 97, 500, 1_003] {
        let partials: Vec<f64> = values.chunks(width).map(deterministic_sum).collect();
        answers.push(combine_partials(&partials));
    }

    // Different partitionings of a compensated sum need not agree bit for bit, because
    // each partial is itself rounded. They must agree to within rounding of the total,
    // which is the guarantee actually on offer -- claiming bit-identity across
    // partitionings would be a claim this implementation does not deliver.
    let first = answers[0];
    for answer in &answers {
        assert!(
            (answer - first).abs() < 1e-6,
            "partitioning changed the total: {first} against {answer}"
        );
    }
}

#[test]
fn a_total_that_cancels_is_exact_rather_than_nearly_right() {
    // `COR-08`, as the four inputs the audit ran against the shipped binary.
    //
    // The fast path scales every term so the accumulator's last bit sits a hundred binary
    // places under the **largest term**, and concluded from that that truncation could not
    // move the answer. What is returned is the **total**, and cancellation makes the total
    // arbitrarily smaller than the largest term --- so the margin was about 48 bits of
    // cancellation, not 100, and past it the answer was quietly approximate.
    //
    // These are not exotic. A cumulative position that nets out, a hedge against its
    // underlying, a reconciliation of two large sides: all of them cancel.
    for (values, expected, was) in [
        (vec![1e13, -1e13, 0.01], 0.01, "9.999999999999995e-3"),
        (vec![1e18, -1e18, 0.01], 0.01, "9.999999999763531e-3"),
        (vec![1e30, -1e30, 1e-5], 1e-5, "0"),
        (vec![1.0, -1.0, 1e-25], 1e-25, "9.999995265034156e-26"),
    ] {
        let total = deterministic_sum(&values);
        assert_eq!(
            total, expected,
            "{values:?} summed to {total:e} rather than {expected:e}; before the fix it was \
             {was}, which is the figure that will not tie out and nobody can explain"
        );
    }
}

#[test]
fn a_cancelled_total_is_the_same_however_the_work_was_divided() {
    // The property the fix must not cost. The exact route is reached only by declining the
    // fast one, and a fallback that depended on ordering would trade a wrong answer for two
    // different answers.
    let values = vec![1e18, 0.01, -1e18, 7.0, -7.0, 1e-9];
    let baseline = deterministic_sum(&values);

    let mut rotated = values.clone();
    for _ in 0..values.len() {
        rotated.rotate_left(1);
        assert_eq!(
            deterministic_sum(&rotated),
            baseline,
            "the order changed the total: {rotated:?}"
        );
    }

    let mut reversed = values;
    reversed.reverse();
    assert_eq!(deterministic_sum(&reversed), baseline);
}

#[test]
fn an_ordinary_sum_still_takes_the_fast_path() {
    // The guard that makes the fix safe declines when the total has cancelled below the
    // truncation floor. It must not decline on data that has not cancelled, or every sum in
    // the system pays for an expansion it does not need.
    //
    // Checked through the answer rather than by inspecting which route ran: a hundred
    // thousand terms of the same sign cannot cancel, and the exact total is known.
    let values: Vec<f64> = (1..=100_000).map(f64::from).collect();
    let expected = 100_000.0 * 100_001.0 / 2.0;
    assert_eq!(deterministic_sum(&values), expected);
}

#[test]
fn an_empty_sum_is_zero() {
    assert_eq!(deterministic_sum(&[]), 0.0);
}

#[test]
fn a_non_finite_value_propagates_rather_than_being_reordered() {
    // Ordering by magnitude is meaningless once an infinity is involved, and the result
    // is non-finite whatever order is chosen. Passing it through unchanged is honest;
    // silently dropping it would turn an invalid input into a plausible total.
    assert!(deterministic_sum(&[1.0, f64::INFINITY, 2.0]).is_infinite());
    assert!(deterministic_sum(&[1.0, f64::NAN, 2.0]).is_nan());
}

proptest! {
    /// However the values are permuted, the total is identical.
    #[test]
    fn any_permutation_gives_an_identical_total(
        values in prop::collection::vec(-1e12f64..1e12, 1..80),
        swaps in prop::collection::vec((0usize..80, 0usize..80), 0..40),
    ) {
        let baseline = deterministic_sum(&values);

        let mut permuted = values.clone();
        for (a, b) in swaps {
            if a < permuted.len() && b < permuted.len() {
                permuted.swap(a, b);
            }
        }

        prop_assert_eq!(deterministic_sum(&permuted), baseline);
    }

    /// A duplicate is not a permutation: the total must change.
    ///
    /// Guards against an implementation that achieved order-independence by
    /// accidentally deduplicating.
    #[test]
    fn adding_a_value_changes_the_total(
        values in prop::collection::vec(1.0f64..1e6, 1..40),
    ) {
        let baseline = deterministic_sum(&values);
        let mut extended = values.clone();
        extended.push(values[0]);
        prop_assert_ne!(deterministic_sum(&extended), baseline);
    }
}
