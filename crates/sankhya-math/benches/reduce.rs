//! What the fixed-point route is worth against the sorted one it replaced.
//!
//! # Why this exists
//!
//! `PERF-01`, `PERF-02`. `ADR-0020` Decision 3 published three tables --- the reduction alone
//! at **1.5× / 2.0× / 2.7×**, a per-kernel table, and *"10 to 15 times faster"* --- and none of
//! them exists as code anywhere in this repository's history. The same decision's own rule
//! reads *"every claim about speed carries its number… this repository does not ship claims"*,
//! which is exactly what let those numbers stand: a figure in prose reads as measured.
//!
//! What is measured here is the thing the decision actually changed: `deterministic_sum` takes
//! the fixed-point route when a common scale exists, and falls back to sorting by magnitude and
//! summing an exact expansion when it does not. The comparison is those two routes over the
//! same data, and the arms consume their results so neither can be deleted.
//!
//! # The fallback is measured on input that reaches it
//!
//! `exact_sum` declines when the values cannot share a fixed-point scale --- which is what the
//! fallback is for. Timing the fallback on data the fast route would have taken measures a
//! branch nobody reaches, so the sorted arm is given a spread wide enough to make the
//! fixed-point route decline, and the fixed-point arm gets data it accepts. **The two arms
//! therefore do not sum the same numbers**, and the ratio is a cost-of-route comparison rather
//! than a speedup on one input --- which is what the published tables implied and could not
//! have measured either.

// A benchmark chooses all of its own data, exactly as a test does. The workspace denies
// `unwrap`, `expect`, `panic` and unchecked indexing because a *server* must not do those
// things to data it did not choose; a benchmark that cannot fail loudly on a fixture it built
// itself would report a number about the wrong thing.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use sankhya_math::{deterministic_sum, exact_sum};

/// Values that share a scale, so the fixed-point route takes them.
fn scaled(n: usize) -> Vec<f64> {
    #[allow(clippy::cast_precision_loss)]
    (0..n).map(|i| (i % 1000) as f64 * 0.125).collect()
}

/// Values that cancel far enough to exhaust the fixed margin, so the sorted expansion runs.
///
/// Large terms that annihilate, with small non-integral ones eighty binary places below them.
/// The small terms truncate at the accumulator's scale, the large ones cancel to nearly
/// nothing, and `exact_sum` declines rather than returning something nearly right --- which is
/// precisely the input the fallback exists for. A first version of this used a wide range of
/// powers of two, which the fixed-point route takes without difficulty; the assertion below
/// caught it, and it would have been a benchmark timing a branch nobody reaches.
fn cancelling(n: usize) -> Vec<f64> {
    let big = 2.0f64.powi(80);
    (0..n)
        .map(|i| match i % 4 {
            0 => big,
            1 => 0.1,
            2 => -big,
            _ => 0.3,
        })
        .collect()
}

/// What determinism costs against a sum that does not promise it.
///
/// # Why the absolute price and not only the ratio
///
/// `PERF-07`. The documentation stated that the fixed-point reduction is faster **than this
/// project's own previous code**, and said nothing else --- so a reader came away believing
/// the kernels had got fast. They had got *faster than they were*. Against an ordinary
/// `iter().sum()` the guarantee is expensive, and that exchange is defensible only if the price
/// is on the page beside it: `exact_sum` accumulates into an `i128`, which **cannot be
/// autovectorised**, and walks the values more than once.
///
/// The naive arm is `iter().sum::<f64>()` --- the thing a reader would have written, and the
/// thing every other engine does. It is not order-independent and that is exactly the point:
/// this measures what order-independence costs, not which implementation is better.
fn what_the_guarantee_costs(c: &mut Criterion) {
    let mut group = c.benchmark_group("determinism-price");
    for n in [8usize, 64, 512, 4096] {
        let values = scaled(n);
        // The fast route, so the comparison is against the path a well-behaved input takes
        // rather than against the fallback.
        assert!(
            exact_sum(&values).is_some(),
            "the priced arm must take the fixed-point route"
        );
        group.bench_with_input(BenchmarkId::new("ordinary-sum", n), &n, |b, _| {
            b.iter(|| black_box(black_box(&values).iter().sum::<f64>()));
        });
        group.bench_with_input(BenchmarkId::new("deterministic", n), &n, |b, _| {
            b.iter(|| black_box(deterministic_sum(black_box(&values))));
        });
    }
    group.finish();
}

fn the_two_routes(c: &mut Criterion) {
    let mut group = c.benchmark_group("deterministic-sum");
    for n in [64usize, 512, 4096] {
        let fast = scaled(n);
        let slow = cancelling(n);
        // Not vacuous: the arms must actually be taking the routes they are named after.
        assert!(
            exact_sum(&fast).is_some(),
            "the fixed-point arm must reach the fixed-point route"
        );
        assert!(
            exact_sum(&slow).is_none(),
            "the fallback arm must reach the fallback"
        );

        group.bench_with_input(BenchmarkId::new("fixed-point", n), &n, |b, _| {
            b.iter(|| black_box(deterministic_sum(black_box(&fast))));
        });
        group.bench_with_input(BenchmarkId::new("sorted-expansion", n), &n, |b, _| {
            b.iter(|| black_box(deterministic_sum(black_box(&slow))));
        });
    }
    group.finish();
}

criterion_group!(benches, the_two_routes, what_the_guarantee_costs);
criterion_main!(benches);
