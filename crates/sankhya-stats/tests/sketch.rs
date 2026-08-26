//! The distinct-value estimate: accurate enough for the one thing it is for.

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

use sankhya_stats::DistinctSketch;

fn sketch_of(range: std::ops::Range<u64>) -> DistinctSketch {
    let mut s = DistinctSketch::new();
    for i in range {
        s.add(&i.to_be_bytes());
    }
    s
}

fn error(estimate: u64, actual: u64) -> f64 {
    (estimate as f64 - actual as f64).abs() / actual as f64
}

#[test]
fn the_estimate_is_accurate_enough_to_order_a_join() {
    // The tolerance is what the *use* requires, not what the algorithm can achieve. This
    // number picks which side of a join to build: 2% wrong picks the same side, and a
    // factor of a thousand does not. Nobody reports this figure.
    for actual in [10u64, 100, 1_000, 10_000, 100_000] {
        let estimate = sketch_of(0..actual).estimate();
        let e = error(estimate, actual);
        assert!(
            e < 0.05,
            "{actual} distinct values estimated as {estimate}, {:.1}% out",
            e * 100.0
        );
    }
}

#[test]
fn a_handful_of_values_is_not_reported_as_hundreds() {
    // Without the small-cardinality correction a nearly-empty sketch reports a few
    // hundred distinct values for a column holding three -- which is exactly the
    // magnitude of error that changes a join decision, in the direction that hurts.
    for actual in [1u64, 2, 3, 5, 8] {
        let estimate = sketch_of(0..actual).estimate();
        assert!(
            estimate <= actual + 2,
            "{actual} distinct values estimated as {estimate}"
        );
    }
}

#[test]
fn repeats_do_not_inflate_the_estimate() {
    let mut s = DistinctSketch::new();
    for _ in 0..1_000 {
        for i in 0..50u64 {
            s.add(&i.to_be_bytes());
        }
    }
    assert!(error(s.estimate(), 50) < 0.1, "{}", s.estimate());
}

#[test]
fn merging_matches_sketching_the_union() {
    // The property that makes statistics maintainable at compaction: the merged file's
    // sketch is the merge of its inputs', with no value re-read. Exactly equal, not
    // approximately -- the registers are a maximum, so merging loses nothing.
    let mut left = sketch_of(0..5_000);
    let right = sketch_of(2_500..7_500);
    left.merge(&right);

    let direct = sketch_of(0..7_500);
    assert_eq!(
        left, direct,
        "merging is not the same as sketching the union"
    );
}

#[test]
fn merging_is_order_independent() {
    let a = sketch_of(0..1_000);
    let b = sketch_of(500..1_500);
    let c = sketch_of(1_200..2_000);

    let mut forward = a.clone();
    forward.merge(&b);
    forward.merge(&c);

    let mut backward = c.clone();
    backward.merge(&b);
    backward.merge(&a);

    assert_eq!(forward, backward);
}

#[test]
fn the_estimate_is_reproducible() {
    // Two nodes must agree about a plan. A hash seeded per process makes that impossible
    // and the disagreement looks like a bug in the optimizer.
    assert_eq!(
        sketch_of(0..10_000).estimate(),
        sketch_of(0..10_000).estimate()
    );

    let mut reversed = DistinctSketch::new();
    for i in (0..10_000u64).rev() {
        reversed.add(&i.to_be_bytes());
    }
    assert_eq!(reversed, sketch_of(0..10_000));
}

#[test]
fn an_empty_sketch_estimates_nothing() {
    let s = DistinctSketch::new();
    assert!(s.is_empty());
    assert_eq!(s.estimate(), 0);
}

#[test]
fn strings_and_integers_do_not_collide_systematically() {
    // A hash with poor avalanche biases the register index, and the symptom is an
    // estimate that is fine for one column shape and badly wrong for another.
    let mut s = DistinctSketch::new();
    for i in 0..5_000u64 {
        s.add(format!("customer-{i:08}").as_bytes());
    }
    assert!(error(s.estimate(), 5_000) < 0.05, "{}", s.estimate());
}
