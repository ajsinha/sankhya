//! Exact summation: the property a canonical order does not give you.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::indexing_slicing
)]

use proptest::prelude::*;
use sankhya_math::{deterministic_sum, Exact};

#[test]
fn a_canonical_order_is_reproducible_and_still_not_associative() {
    // The distinction this type exists for, stated as a test rather than a comment.
    // `deterministic_sum` gives the same answer for the same values in any order — that is
    // reproducibility. It does not give the same answer for the same values *grouped*
    // differently, and a cube rolls up in groups.
    let values = [1.0, 1e16, -1e16, 1e-16];

    let flat = deterministic_sum(&values);
    let grouped = deterministic_sum(&[
        deterministic_sum(&values[..2]),
        deterministic_sum(&values[2..]),
    ]);
    assert_ne!(flat.to_bits(), grouped.to_bits(), "if these agree, pick harder values");

    // Exactly, both ways, identical.
    let mut left = Exact::of(&values[..2]);
    left.combine(&Exact::of(&values[2..]));
    assert_eq!(Exact::of(&values).to_f64().to_bits(), left.to_f64().to_bits());
}

#[test]
fn combining_is_associative_and_commutative() {
    let a = Exact::of(&[1e17, 3.0]);
    let b = Exact::of(&[-1e17, 0.5]);
    let c = Exact::of(&[7.25, -2.75]);

    let mut left = a.clone();
    left.combine(&b);
    left.combine(&c);

    let mut right = c.clone();
    right.combine(&a);
    right.combine(&b);

    assert_eq!(left.to_f64().to_bits(), right.to_f64().to_bits());
}

#[test]
fn an_exact_sum_of_nothing_is_zero() {
    assert_eq!(Exact::zero().to_f64(), 0.0);
    assert!(Exact::zero().components().is_empty());
    assert!(Exact::zero().is_exact());
}

#[test]
fn cancelling_values_leave_no_components_behind() {
    // Or an expansion grows without bound over a long roll-up, and the cost stops being
    // "a few doubles".
    let mut exact = Exact::zero();
    for value in [1e20, 1.0, -1e20, -1.0] {
        exact.add(value);
    }
    assert_eq!(exact.to_f64(), 0.0);
    assert!(
        exact.components().is_empty(),
        "a zero component is not a component: {:?}",
        exact.components()
    );
}

#[test]
fn a_non_finite_value_taints_the_sum_rather_than_being_hidden() {
    // There is no exact representation to preserve, and reporting a finite total for data
    // that has none is worse than propagating the infinity.
    let mut exact = Exact::of(&[1.0, 2.0]);
    exact.add(f64::INFINITY);
    assert!(!exact.is_exact());
    assert!(exact.to_f64().is_infinite());

    let mut with_nan = Exact::of(&[1.0]);
    with_nan.add(f64::NAN);
    assert!(with_nan.to_f64().is_nan());
}

#[test]
fn a_tainted_sum_stays_tainted_when_combined() {
    let mut clean = Exact::of(&[1.0]);
    let mut tainted = Exact::zero();
    tainted.add(f64::INFINITY);
    clean.combine(&tainted);
    assert!(!clean.is_exact());
    assert!(clean.to_f64().is_infinite());
}

#[test]
fn components_sum_to_the_total_so_a_cuboid_can_store_them_unrounded() {
    // This is what a materialised cuboid holds. Storing `to_f64()` instead rounds at every
    // level of a roll-up, and the fast path then disagrees with the slow one.
    let values = [1e16, 1.0, 2.0, -1e16, 0.25];
    let exact = Exact::of(&values);
    let restored = Exact::of(exact.components());
    assert_eq!(restored.to_f64().to_bits(), exact.to_f64().to_bits());
}

proptest! {
    /// However the values are split into groups, the exact total is the same.
    #[test]
    fn any_grouping_gives_identical_bits(
        values in prop::collection::vec(-1e18f64..1e18, 1..30),
        split in 0usize..30
    ) {
        let at = split.min(values.len());
        let mut grouped = Exact::of(&values[..at]);
        grouped.combine(&Exact::of(&values[at..]));
        prop_assert_eq!(
            Exact::of(&values).to_f64().to_bits(),
            grouped.to_f64().to_bits()
        );
    }

    /// Adding in any order gives identical bits.
    #[test]
    fn any_order_gives_identical_bits(
        values in prop::collection::vec(-1e18f64..1e18, 1..30),
        rotation in 0usize..30
    ) {
        let at = rotation % values.len();
        let rotated: Vec<f64> = values[at..].iter().chain(values[..at].iter()).copied().collect();
        prop_assert_eq!(
            Exact::of(&values).to_f64().to_bits(),
            Exact::of(&rotated).to_f64().to_bits()
        );
    }

    /// An expansion never grows past one component per value.
    #[test]
    fn an_expansion_stays_bounded(values in prop::collection::vec(-1e18f64..1e18, 1..60)) {
        let exact = Exact::of(&values);
        prop_assert!(
            exact.components().len() <= values.len(),
            "{} components for {} values",
            exact.components().len(),
            values.len()
        );
    }
}

// --- the fixed-point route ------------------------------------------------
//
// `deterministic_sum` takes the fixed-point route first and falls back to the sorted one. If
// the two ever disagree, every figure this system produces moves --- so these hold the
// equivalence directly rather than trusting that the callers would have noticed.

/// A small deterministic generator, so a failing case is reproducible from its seed rather
/// than from "it failed on Tuesday".
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }
}

/// The sorted sum, reached directly, whatever `deterministic_sum` chooses to do.
fn sorted_sum(values: &[f64]) -> f64 {
    // Every path but the fixed-point one, obtained by making the fixed point decline: an extra
    // non-finite term would change the answer, so instead the values are summed through the
    // canonical order by construction here.
    let mut ordered: Vec<f64> = values.to_vec();
    ordered.sort_by(|a, b| {
        a.abs()
            .partial_cmp(&b.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut sum = 0.0f64;
    let mut compensation = 0.0f64;
    for value in ordered {
        let t = sum + value;
        if sum.abs() >= value.abs() {
            compensation += (sum - t) + value;
        } else {
            compensation += (value - t) + sum;
        }
        sum = t;
    }
    sum + compensation
}

#[test]
fn the_fixed_point_route_agrees_with_the_sorted_one_bit_for_bit() {
    // Twenty thousand vectors spanning forty orders of magnitude. Not a demonstration that
    // it is usually right: a disagreement anywhere here is a figure that changes, and the
    // whole reason the fixed point is allowed to run first is that it cannot produce one.
    let mut seeded = Seeded(0x5adf_aced);
    let mut checked = 0u32;

    for _ in 0..20_000 {
        let n = 1 + (seeded.next() % 64) as usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let spread = (seeded.next() % 40) as i32 - 20;
        let values: Vec<f64> = (0..n)
            .map(|_| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let decade = (seeded.next() % 8) as i32 + spread;
                (seeded.unit() - 0.5) * 2.0 * 10f64.powi(decade)
            })
            .collect();

        if let Some(exact) = sankhya_math::exact_sum(&values) {
            assert_eq!(
                exact,
                sorted_sum(&values),
                "the two routes disagree on {values:?}"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 19_000,
        "the fixed point declined too often to have been tested: {checked}"
    );
}

#[test]
fn the_fixed_point_total_is_a_function_of_the_multiset() {
    // The property the sort exists to buy, obtained here by construction: integer addition is
    // associative and commutative, so permuting the input cannot move the total.
    let mut seeded = Seeded(0x0f1c_e550);

    for _ in 0..5_000 {
        let n = 2 + (seeded.next() % 32) as usize;
        let values: Vec<f64> = (0..n)
            .map(|_| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let decade = (seeded.next() % 30) as i32 - 15;
                (seeded.unit() - 0.5) * 2.0 * 10f64.powi(decade)
            })
            .collect();

        let Some(forwards) = sankhya_math::exact_sum(&values) else {
            continue;
        };
        let mut backwards = values.clone();
        backwards.reverse();
        assert_eq!(sankhya_math::exact_sum(&backwards), Some(forwards));

        // And a rotation, which reversal alone would not catch: reversing a palindrome is
        // the identity, and a lane-based accumulator survives that while failing a rotation.
        let mut rotated = values.clone();
        rotated.rotate_left(1 + (seeded.next() % (n as u64 - 1)) as usize);
        assert_eq!(sankhya_math::exact_sum(&rotated), Some(forwards));
    }
}

#[test]
fn the_case_that_defeats_a_lane_parallel_sum() {
    // `1e16, 1, -1e16, 1` repeated: huge terms that cancel and small ones that must survive.
    // Eight fixed lanes with per-lane compensation --- ten to fifteen times faster, and the
    // reason it is not in this crate --- returns 5 here, and 0 when the input is reversed.
    let mut values: Vec<f64> = Vec::new();
    for _ in 0..9 {
        values.extend_from_slice(&[1e16, 1.0, -1e16, 1.0]);
    }

    assert_eq!(sankhya_math::exact_sum(&values), Some(18.0));
    assert_eq!(sankhya_math::deterministic_sum(&values), 18.0);

    let mut reversed = values.clone();
    reversed.reverse();
    assert_eq!(sankhya_math::deterministic_sum(&reversed), 18.0);

    // And the naive sum, so the size of what is being defended is on the record.
    let naive: f64 = values.iter().sum();
    assert!((naive - 18.0).abs() > 10.0, "the naive sum was not wrong: {naive}");
}

#[test]
fn the_fixed_point_declines_rather_than_approximating() {
    // The one thing it must never do is return a number it is not sure of. A non-finite term
    // has no fixed-point image, and a magnitude near the top of the range would overflow the
    // accumulator --- both decline, and `deterministic_sum` then sorts.
    assert_eq!(sankhya_math::exact_sum(&[1.0, f64::NAN]), None);
    assert_eq!(sankhya_math::exact_sum(&[f64::INFINITY]), None);
    assert_eq!(sankhya_math::exact_sum(&[f64::NEG_INFINITY, 1.0]), None);

    // And `deterministic_sum` still answers those, by the route that has no such limit.
    assert!(sankhya_math::deterministic_sum(&[1e300, 1.0]).is_finite());

    // A term a hundred binary places below the largest is **not** a decline, and this is the
    // case that says why. `1e300 + 1.0` is `1e300` in `f64`: the small term cannot move the
    // result, so dropping it in the accumulator is not an approximation --- it is the
    // correctly-rounded answer, reached by a shorter road.
    assert_eq!(sankhya_math::exact_sum(&[1e300, 1.0]), Some(1e300));
    assert_eq!(1e300 + 1.0, 1e300, "the premise of the paragraph above");
    assert_eq!(
        sankhya_math::exact_sum(&[1e300, 1.0]),
        Some(sorted_sum(&[1e300, 1.0])),
        "and the sorted route agrees, which is the property that matters"
    );
}

#[test]
fn a_total_of_nothing_and_a_total_of_zeroes_are_both_positive_zero() {
    // `-0.0 + -0.0` is `-0.0` in floating point, and a total reported as negative zero is a
    // difference somebody asks about and nobody can explain.
    assert_eq!(sankhya_math::exact_sum(&[]), Some(0.0));
    assert_eq!(sankhya_math::exact_sum(&[0.0, -0.0]), Some(0.0));
    assert!(sankhya_math::exact_sum(&[-0.0, -0.0]).is_some_and(|t| t == 0.0 && t.is_sign_positive()));
}

#[test]
fn an_infinity_taints_an_exact_sum_rather_than_becoming_a_nan_inside_it() {
    // An expansion holds a sum as non-overlapping doubles, and `two_sum` on an infinity
    // produces a `NaN` component --- so an expansion that admits one stops being an
    // expansion of anything. The variant exists to notice that and hand the ordinary IEEE
    // result back instead.
    //
    // This mutation survived the audit before this test: nothing asserted what an expansion
    // does with a value it cannot represent, so the branch that decides was covered by
    // nothing at all.
    let mut exact = sankhya_math::Exact::zero();
    exact.add(1e300);
    exact.add(f64::INFINITY);
    exact.add(-1e300);

    assert!(!exact.is_exact(), "an expansion holding an infinity called itself exact");
    let total = exact.to_f64();
    assert!(
        total.is_infinite() && total.is_sign_positive(),
        "an infinity became {total} inside the expansion rather than propagating"
    );

    // A NaN propagates as a NaN, which is the other thing that must not turn into a number.
    let mut with_nan = sankhya_math::Exact::zero();
    with_nan.add(5.0);
    with_nan.add(f64::NAN);
    assert!(with_nan.to_f64().is_nan(), "a NaN was absorbed into a finite total");

    // And combining a tainted expansion into a clean one taints it too, or a roll-up would
    // launder the infinity one level up.
    let mut clean = sankhya_math::Exact::of(&[1.0, 2.0]);
    clean.combine(&exact);
    assert!(!clean.is_exact(), "combining a tainted expansion left the result exact");
}
