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
