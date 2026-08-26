//! Property tests for the core vocabulary.
//!
//! These are the highest-value tests in the crate: they assert the invariants that
//! everything above depends on, over thousands of randomized inputs rather than a
//! handful of chosen examples.

use proptest::prelude::*;
use sankhya_types::{Fixed, FixedError, Lsn, LsnRange, Scale, Timestamp, ValidityInterval};

fn scale() -> impl Strategy<Value = Scale> {
    (0u8..=12).prop_filter_map("valid scale", Scale::new)
}

proptest! {
    /// Summation is order-independent.
    ///
    /// This is the property that decides the representation. It **holds** for exact
    /// fixed-point and **fails** for binary floating point, because floating-point
    /// addition is not associative. In a parallel engine the partition and completion
    /// order varies between runs, so a float-based sum returns different answers for
    /// the same query — which is disqualifying anywhere results must be reproducible.
    #[test]
    fn sum_is_order_independent(s in scale(), mut values in prop::collection::vec(-1_000_000i128..1_000_000, 0..64)) {
        let as_fixed: Vec<Fixed> = values.iter().map(|u| Fixed::from_units(*u, s)).collect();
        let forward = Fixed::sum(as_fixed.iter().copied(), s);

        values.reverse();
        let reversed: Vec<Fixed> = values.iter().map(|u| Fixed::from_units(*u, s)).collect();
        let backward = Fixed::sum(reversed.iter().copied(), s);

        prop_assert_eq!(forward, backward);
    }

    /// Addition never wraps silently; it either succeeds or reports overflow.
    #[test]
    fn addition_never_wraps(s in scale(), a in any::<i128>(), b in any::<i128>()) {
        let result = Fixed::from_units(a, s).add(Fixed::from_units(b, s));
        match a.checked_add(b) {
            Some(expected) => prop_assert_eq!(result.map(Fixed::units), Ok(expected)),
            None => prop_assert_eq!(result.unwrap_err(), FixedError::Overflow),
        }
    }

    /// Mixing scales is refused rather than silently coerced.
    #[test]
    fn scale_mismatch_is_refused(a in 0u8..=8, b in 0u8..=8, x in any::<i64>(), y in any::<i64>()) {
        prop_assume!(a != b);
        let (Some(sa), Some(sb)) = (Scale::new(a), Scale::new(b)) else {
            return Ok(());
        };
        let result = Fixed::from_units(i128::from(x), sa).add(Fixed::from_units(i128::from(y), sb));
        let refused = matches!(result, Err(FixedError::ScaleMismatch { .. }));
        prop_assert!(refused, "mixing scales must be refused");
    }

    /// Widening then narrowing round-trips exactly.
    #[test]
    fn rescale_round_trips(s in 0u8..=6, extra in 1u8..=6, units in -1_000_000i128..1_000_000) {
        let (Some(from), Some(to)) = (Scale::new(s), Scale::new(s + extra)) else {
            return Ok(());
        };
        let original = Fixed::from_units(units, from);
        let widened = original.rescale(to).expect("widening fits");
        prop_assert_eq!(widened.rescale(from), Ok(original));
    }

    /// Narrowing that would discard a non-zero digit is refused, never truncated.
    #[test]
    fn narrowing_refuses_precision_loss(units in 1i128..1_000_000) {
        let (Some(from), Some(to)) = (Scale::new(4), Scale::new(2)) else {
            return Ok(());
        };
        let value = Fixed::from_units(units * 100 + 1, from); // trailing digit is non-zero
        let refused = matches!(value.rescale(to), Err(FixedError::PrecisionLoss { .. }));
        prop_assert!(refused, "narrowing must not truncate");
    }

    /// Adjacent coverage intervals abut exactly: no gap, no overlap.
    ///
    /// This is the invariant the read-path splice rests on. If it can be violated,
    /// a tiered query can double-count or lose rows.
    #[test]
    fn abutting_ranges_never_overlap(a in 0u64..1_000_000, b in 0u64..1_000_000, c in 0u64..1_000_000) {
        let mut points = [a, b, c];
        points.sort_unstable();
        let [lo, mid, hi] = points;
        let (Some(first), Some(second)) = (
            LsnRange::new(Lsn::new(lo), Lsn::new(mid)),
            LsnRange::new(Lsn::new(mid), Lsn::new(hi)),
        ) else {
            return Ok(());
        };
        prop_assert!(first.abuts(second));
        prop_assert!(!first.overlaps(second));
    }

    /// Every position in a covered span lands in exactly one of two abutting tiers.
    #[test]
    fn abutting_ranges_cover_without_duplication(mid in 1u64..10_000, hi_extra in 1u64..10_000, probe in 0u64..20_000) {
        let hi = mid + hi_extra;
        let (Some(lower), Some(upper)) = (
            LsnRange::new(Lsn::ZERO, Lsn::new(mid)),
            LsnRange::new(Lsn::new(mid), Lsn::new(hi)),
        ) else {
            return Ok(());
        };
        let p = Lsn::new(probe);
        let in_lower = lower.contains(p);
        let in_upper = upper.contains(p);
        prop_assert!(!(in_lower && in_upper), "position {p} counted twice");
        if probe > 0 && probe <= hi {
            prop_assert!(in_lower || in_upper, "position {p} fell in no tier");
        }
    }

    /// The textual log-position form round-trips.
    #[test]
    fn lsn_text_round_trips(raw in any::<u64>()) {
        let lsn = Lsn::new(raw);
        prop_assert_eq!(Lsn::parse(&lsn.to_string()), Some(lsn));
    }

    /// An inverted validity interval is rejected at construction.
    #[test]
    fn inverted_validity_is_rejected(from in 1i64..1_000_000, back in 1i64..1_000_000) {
        let interval = ValidityInterval::new(
            Timestamp::from_micros(from),
            Some(Timestamp::from_micros(from - back)),
        );
        prop_assert!(interval.is_none());
    }
}

/// Malformed wire input yields `None` rather than panicking. The decoder parses bytes
/// from a network socket, so malformed input is expected rather than exceptional.
#[test]
fn malformed_lsn_text_is_rejected() {
    for bad in ["", "/", "X/", "/Y", "nonsense", "1/2/3", "ZZ/GG"] {
        assert_eq!(Lsn::parse(bad), None, "{bad:?} should not parse");
    }
}

/// Demonstrates the failure this type exists to prevent.
#[test]
fn floating_point_would_not_be_order_independent() {
    let values: Vec<f64> = (1..=1000).map(|i| 1.0 / f64::from(i)).collect();
    let forward: f64 = values.iter().sum();
    let backward: f64 = values.iter().rev().sum();
    assert_ne!(
        forward.to_bits(),
        backward.to_bits(),
        "floating-point summation happened to be order-independent here; \
         the fixed-point guarantee is still the one we rely on"
    );

    let scale = Scale::new(6).expect("scale 6 is valid");
    let exact: Vec<Fixed> = (1..=1000)
        .map(|i| Fixed::from_units(i128::from(i), scale))
        .collect();
    let ef: Fixed = Fixed::sum(exact.iter().copied(), scale).expect("no overflow");
    let eb: Fixed = Fixed::sum(exact.iter().rev().copied(), scale).expect("no overflow");
    assert_eq!(ef, eb, "fixed-point summation must be order-independent");
}
