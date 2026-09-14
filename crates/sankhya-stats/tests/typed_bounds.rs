//! A bound that knows what it counts.
//!
//! `M24` gave dates, instants and decimals their own variants rather than folding them into
//! `Bound::Int`. Everything here is a consequence of that choice, and every test is a case
//! where the folded representation would have compared two numbers that mean different things
//! and skipped a file holding rows.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use proptest::prelude::*;
use sankhya_stats::{can_skip, Bound, ColumnStats, Predicate, TimeUnit};
use std::cmp::Ordering;

fn stamp(value: i64, unit: TimeUnit) -> Bound {
    Bound::Timestamp { value, unit }
}

#[test]
fn the_same_instant_in_two_units_compares_equal() {
    // The property the unit exists for. One second past the epoch is one second past the
    // epoch however it is counted, and a bound that forgot its unit would call the
    // nanosecond form a billion times larger.
    let forms = [
        stamp(1, TimeUnit::Second),
        stamp(1_000, TimeUnit::Millisecond),
        stamp(1_000_000, TimeUnit::Microsecond),
        stamp(1_000_000_000, TimeUnit::Nanosecond),
    ];
    for a in &forms {
        for b in &forms {
            assert_eq!(a.compare(b), Some(Ordering::Equal), "{a:?} against {b:?}");
        }
    }
}

#[test]
fn an_instant_before_the_epoch_still_orders() {
    // Euclidean division rather than truncating, which is the difference between
    // `-1500ms` sorting before `-1s` and sorting after it.
    let earlier = stamp(-1_500, TimeUnit::Millisecond);
    let later = stamp(-1, TimeUnit::Second);
    assert_eq!(earlier.compare(&later), Some(Ordering::Less));
    assert_eq!(later.compare(&earlier), Some(Ordering::Greater));
}

#[test]
fn a_second_bound_past_the_nanosecond_range_is_still_comparable() {
    // The reason instants are split into seconds and sub-second nanoseconds rather than
    // normalised to nanoseconds: `i64` nanoseconds run out in 2262, and a contract ending
    // in 2300 is an ordinary thing for a warehouse to hold. Normalising would overflow and
    // produce a bound that is not a bound.
    let far = stamp(10_413_792_000, TimeUnit::Second); // 2300-01-01
    let near = stamp(0, TimeUnit::Nanosecond);
    assert_eq!(far.compare(&near), Some(Ordering::Greater));
}

#[test]
fn a_date_is_not_an_integer_and_says_so() {
    // 20,710 days is 2026-09-14 and 20,710 of anything else is not. Comparing them would be
    // the one thing this type exists to refuse.
    assert_eq!(Bound::Date(20_710).compare(&Bound::Int(20_710)), None);
    assert_eq!(Bound::Date(20_710).compare(&stamp(20_710, TimeUnit::Second)), None);
}

#[test]
fn decimals_compare_at_a_common_scale() {
    // `1.5` at scale 1 and `1.50` at scale 2 are the same amount. Comparing the unscaled
    // integers --- 15 against 150 --- is off by a factor of ten, and it is off in the
    // direction that makes a maximum look ten times larger than it is.
    let one_and_a_half = Bound::Decimal { unscaled: 15, scale: 1 };
    let also = Bound::Decimal { unscaled: 150, scale: 2 };
    assert_eq!(one_and_a_half.compare(&also), Some(Ordering::Equal));

    let more = Bound::Decimal { unscaled: 151, scale: 2 };
    assert_eq!(one_and_a_half.compare(&more), Some(Ordering::Less));
}

#[test]
fn a_decimal_comparison_that_would_overflow_refuses() {
    // `None` costs a scan. Saturating would cost an answer: a bound pinned at `i128::MAX`
    // is larger than every value in the file, and a `>` predicate then skips it.
    let huge = Bound::Decimal { unscaled: i128::MAX, scale: 0 };
    let fine = Bound::Decimal { unscaled: 1, scale: 30 };
    assert_eq!(huge.compare(&fine), None);
}

#[test]
fn a_date_predicate_skips_only_what_it_proves() {
    let mut stats = ColumnStats::default();
    for day in 20_700..20_710 {
        stats.observe(Some(&i32::to_be_bytes(day)), Some(Bound::Date(day)));
    }

    assert!(can_skip(&stats, &Predicate::LessThan(Bound::Date(20_700))));
    assert!(can_skip(&stats, &Predicate::GreaterThan(Bound::Date(20_709))));
    assert!(!can_skip(&stats, &Predicate::Equals(Bound::Date(20_705))));
    // And an integer predicate against a date column proves nothing at all, so the file is
    // read. A missed skip costs a scan; this is the case where taking it would cost rows.
    assert!(!can_skip(&stats, &Predicate::GreaterThan(Bound::Int(20_709))));
}

proptest! {
    /// Never skip a file that holds a matching instant.
    ///
    /// The one property in this crate that separates an optimisation from a wrong answer,
    /// asked of the mixed-unit case where it is easiest to break.
    #[test]
    fn an_instant_in_the_file_is_never_skipped(
        values in proptest::collection::vec(-4_000_000_000i64..4_000_000_000, 1..30),
        probe in -4_000_000_000i64..4_000_000_000,
    ) {
        let mut stats = ColumnStats::default();
        for value in &values {
            // Recorded in milliseconds.
            let millis = value.saturating_mul(1_000);
            stats.observe(Some(&millis.to_be_bytes()), Some(stamp(millis, TimeUnit::Millisecond)));
        }
        // Asked in seconds, which is the mismatch a folded bound gets wrong.
        let asked = stamp(probe, TimeUnit::Second);
        let present = values.contains(&probe);
        if present {
            prop_assert!(!can_skip(&stats, &Predicate::Equals(asked)));
        }
    }
}
