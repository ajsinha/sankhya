//! Skipping a file must never lose a row.
//!
//! The property below is the only one that really matters here. Everything else in this
//! crate is an optimisation; this is the line between an optimisation and a wrong
//! answer, and the wrong answer is undetectable — the query returns fewer rows and
//! nothing about the result says so.

use proptest::prelude::*;
use sankhya_stats::{can_skip, Bound, ColumnStats, Predicate};

fn stats_over(values: &[Option<i64>]) -> ColumnStats {
    let mut stats = ColumnStats::default();
    for value in values {
        match value {
            Some(v) => stats.observe(Some(&v.to_be_bytes()), Some(Bound::Int(*v))),
            None => stats.observe(None, None),
        }
    }
    stats
}

/// Whether any value in the file actually satisfies the predicate.
fn any_matches(values: &[Option<i64>], predicate: &Predicate) -> bool {
    values.iter().any(|value| match (value, predicate) {
        (None, Predicate::IsNull) => true,
        (None, _) => false,
        (Some(_), Predicate::IsNull) => false,
        (Some(_), Predicate::IsNotNull) => true,
        (Some(v), Predicate::Equals(Bound::Int(t))) => v == t,
        (Some(v), Predicate::LessThan(Bound::Int(t))) => v < t,
        (Some(v), Predicate::LessOrEqual(Bound::Int(t))) => v <= t,
        (Some(v), Predicate::GreaterThan(Bound::Int(t))) => v > t,
        (Some(v), Predicate::GreaterOrEqual(Bound::Int(t))) => v >= t,
        (
            Some(v),
            Predicate::Between {
                low: Bound::Int(lo),
                high: Bound::Int(hi),
            },
        ) => v >= lo && v <= hi,
        _ => false,
    })
}

fn any_predicate() -> impl Strategy<Value = Predicate> {
    prop_oneof![
        (-30i64..30).prop_map(|t| Predicate::Equals(Bound::Int(t))),
        (-30i64..30).prop_map(|t| Predicate::LessThan(Bound::Int(t))),
        (-30i64..30).prop_map(|t| Predicate::LessOrEqual(Bound::Int(t))),
        (-30i64..30).prop_map(|t| Predicate::GreaterThan(Bound::Int(t))),
        (-30i64..30).prop_map(|t| Predicate::GreaterOrEqual(Bound::Int(t))),
        (-30i64..30, -30i64..30).prop_map(|(a, b)| Predicate::Between {
            low: Bound::Int(a.min(b)),
            high: Bound::Int(a.max(b)),
        }),
        Just(Predicate::IsNull),
        Just(Predicate::IsNotNull),
    ]
}

proptest! {
    /// A skip is never wrong.
    ///
    /// The converse is deliberately *not* asserted: skipping less than it could is a
    /// missed optimisation, and an implementation that never skipped anything would be
    /// slow and correct. Only one direction is a defect.
    #[test]
    fn skipping_never_hides_a_matching_row(
        values in prop::collection::vec(prop::option::of(-30i64..30), 0..40),
        predicate in any_predicate(),
    ) {
        let stats = stats_over(&values);
        if can_skip(&stats, &predicate) {
            prop_assert!(
                !any_matches(&values, &predicate),
                "skipped a file containing a match: {:?} against {:?}",
                values,
                predicate
            );
        }
    }

    /// Merged statistics are still safe to skip on.
    ///
    /// Compaction merges statistics rather than recomputing them, so a merge that
    /// narrowed a bound would produce a file that is skipped when it should be read —
    /// and the defect would appear only after compaction ran, on data that was correct
    /// when it was written.
    #[test]
    fn merged_statistics_are_still_safe(
        left in prop::collection::vec(prop::option::of(-30i64..30), 0..25),
        right in prop::collection::vec(prop::option::of(-30i64..30), 0..25),
        predicate in any_predicate(),
    ) {
        let mut merged = stats_over(&left);
        merged.merge(&stats_over(&right)).expect("both are integers");

        let mut union = left.clone();
        union.extend(right.iter().copied());

        if can_skip(&merged, &predicate) {
            prop_assert!(!any_matches(&union, &predicate));
        }
    }

    /// Merging is exact for counts.
    #[test]
    fn merging_conserves_counts(
        left in prop::collection::vec(prop::option::of(-30i64..30), 0..25),
        right in prop::collection::vec(prop::option::of(-30i64..30), 0..25),
    ) {
        let mut merged = stats_over(&left);
        merged.merge(&stats_over(&right)).expect("both are integers");

        let mut union = left.clone();
        union.extend(right.iter().copied());
        let direct = stats_over(&union);

        prop_assert_eq!(merged.rows, direct.rows);
        prop_assert_eq!(merged.nulls, direct.nulls);
        prop_assert_eq!(merged.min, direct.min);
        prop_assert_eq!(merged.max, direct.max);
    }
}

#[test]
fn a_file_outside_the_range_is_skipped() {
    // The optimisation actually happening. Without this the property above would be
    // satisfied by an implementation that never skips anything.
    let stats = stats_over(&[Some(10), Some(20), Some(30)]);

    assert!(can_skip(&stats, &Predicate::Equals(Bound::Int(5))));
    assert!(can_skip(&stats, &Predicate::Equals(Bound::Int(35))));
    assert!(can_skip(&stats, &Predicate::LessThan(Bound::Int(10))));
    assert!(can_skip(&stats, &Predicate::GreaterThan(Bound::Int(30))));
    assert!(can_skip(
        &stats,
        &Predicate::Between {
            low: Bound::Int(40),
            high: Bound::Int(50)
        }
    ));

    assert!(!can_skip(&stats, &Predicate::Equals(Bound::Int(20))));
    assert!(!can_skip(&stats, &Predicate::LessThan(Bound::Int(11))));
}

#[test]
fn an_absent_bound_means_read_the_file() {
    // Unknown is not unbounded. A caller that filled in a default here would have turned
    // a missing statistic into a wrong one.
    let mut stats = ColumnStats::default();
    stats.observe(Some(b"x"), None);

    assert!(stats.min.is_none());
    assert!(!can_skip(&stats, &Predicate::Equals(Bound::Int(999))));
}

#[test]
fn a_bound_of_the_wrong_type_never_skips() {
    // Comparing across types is where a bound quietly stops meaning what it says. The
    // comparison is refused, so the file is read.
    let stats = stats_over(&[Some(10), Some(20)]);
    assert!(!can_skip(
        &stats,
        &Predicate::Equals(Bound::Bytes(b"hello".to_vec()))
    ));
    assert!(!can_skip(&stats, &Predicate::LessThan(Bound::Float(1.0))));
}

#[test]
fn a_file_with_no_nulls_skips_an_is_null() {
    let stats = stats_over(&[Some(1), Some(2)]);
    assert!(can_skip(&stats, &Predicate::IsNull));
    assert!(!can_skip(&stats, &Predicate::IsNotNull));
}

#[test]
fn a_file_of_only_nulls_skips_everything_else() {
    let stats = stats_over(&[None, None, None]);
    assert!(!can_skip(&stats, &Predicate::IsNull));
    assert!(can_skip(&stats, &Predicate::IsNotNull));
    assert!(can_skip(&stats, &Predicate::Equals(Bound::Int(1))));
    assert!(can_skip(&stats, &Predicate::GreaterThan(Bound::Int(-999))));
}

#[test]
fn an_empty_file_is_always_skippable() {
    let stats = ColumnStats::default();
    assert!(can_skip(&stats, &Predicate::IsNull));
    assert!(can_skip(&stats, &Predicate::Equals(Bound::Int(1))));
}

#[test]
fn a_nan_is_never_recorded_as_a_bound() {
    // A NaN bound is incomparable with everything, so recording one would make every
    // later merge incomparable and lose the bounds for the whole partition.
    let mut stats = ColumnStats::default();
    stats.observe(Some(&1.0f64.to_be_bytes()), Some(Bound::Float(1.0)));
    stats.observe(Some(&f64::NAN.to_be_bytes()), Some(Bound::Float(f64::NAN)));
    stats.observe(Some(&3.0f64.to_be_bytes()), Some(Bound::Float(3.0)));

    assert_eq!(stats.min, Some(Bound::Float(1.0)));
    assert_eq!(stats.max, Some(Bound::Float(3.0)));
    assert_eq!(stats.rows, 3);
}

#[test]
fn a_nan_arriving_first_does_not_become_the_bound_forever() {
    // The case the ordinary NaN test misses. With a real value first, the comparison
    // against NaN fails and the existing bound is kept, so the guard looks redundant.
    // With NaN *first* there is no existing bound to keep: it is adopted, and every
    // later comparison against it fails, so it stays as the bound for the whole
    // partition and no file is ever skipped again.
    let mut stats = ColumnStats::default();
    stats.observe(Some(&f64::NAN.to_be_bytes()), Some(Bound::Float(f64::NAN)));
    stats.observe(Some(&1.0f64.to_be_bytes()), Some(Bound::Float(1.0)));
    stats.observe(Some(&3.0f64.to_be_bytes()), Some(Bound::Float(3.0)));

    assert_eq!(stats.min, Some(Bound::Float(1.0)));
    assert_eq!(stats.max, Some(Bound::Float(3.0)));

    // And the bounds still work, which is the consequence that matters.
    assert!(can_skip(&stats, &Predicate::GreaterThan(Bound::Float(3.0))));
}

#[test]
fn merging_incomparable_bounds_is_refused() {
    // The counts would merge fine. Dropping only the bounds silently would leave the
    // caller believing it had merged everything.
    let mut ints = stats_over(&[Some(1)]);
    let mut bytes = ColumnStats::default();
    bytes.observe(Some(b"a"), Some(Bound::Bytes(b"a".to_vec())));

    assert!(ints.merge(&bytes).is_err());
}

#[test]
fn merging_with_an_unbounded_file_drops_the_bounds() {
    // The union is only bounded if both halves are. Inheriting one side's bound would
    // claim a limit the other side may exceed -- which is the one way a merge can
    // produce a skip that hides a row.
    let mut known = stats_over(&[Some(10), Some(20)]);
    let mut unknown = ColumnStats::default();
    unknown.observe(Some(b"x"), None);

    known.merge(&unknown).expect("no bound conflict");

    assert!(known.min.is_none());
    assert!(known.max.is_none());
    assert!(!can_skip(&known, &Predicate::Equals(Bound::Int(999))));
}
