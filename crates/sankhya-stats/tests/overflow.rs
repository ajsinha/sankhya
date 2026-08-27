//! Predicting an overflow the analytical engine would not report.
//!
//! Every assertion here is about which direction the estimate errs in. A false alarm
//! costs a refused query that would have been fine; a missed one costs a wrong number
//! nobody notices, which is the failure this exists to prevent.

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

use sankhya_stats::{decimal_sum_risk, integer_sum_risk, Bound, ColumnStats, SumRisk};

fn column(min: i64, max: i64, rows: u64, nulls: u64) -> ColumnStats {
    ColumnStats {
        rows,
        nulls,
        min: Some(Bound::Int(min)),
        max: Some(Bound::Int(max)),
        ..ColumnStats::default()
    }
}

#[test]
fn an_ordinary_column_is_provably_safe() {
    // The common case has to come out clean, or the check is a permanent alarm and gets
    // switched off.
    let risk = integer_sum_risk(&column(0, 1_000_000, 1_000_000, 0));
    assert_eq!(risk, SumRisk::Safe);
    assert!(risk.provably_safe());
}

#[test]
fn a_column_that_can_overflow_is_flagged() {
    // A billion rows of a billion each is 10^18, inside the range; ten billion each is
    // not.
    let risk = integer_sum_risk(&column(0, 10_000_000_000, 1_000_000_000, 0));
    assert!(matches!(risk, SumRisk::Possible { .. }));
    assert!(!risk.provably_safe());
    assert!(format!("{risk}").contains("wrap or lose exactness"));
}

#[test]
fn large_negatives_overflow_as_readily_as_large_positives() {
    // Checking only the maximum would clear a column of large negative values, and a
    // sum of losses is exactly the figure somebody cares about.
    let risk = integer_sum_risk(&column(-10_000_000_000, 0, 1_000_000_000, 0));
    assert!(matches!(risk, SumRisk::Possible { .. }));
}

#[test]
fn a_column_with_no_bounds_is_not_assumed_small() {
    // Unknown is not the same as small. Treating an absent bound as safe would turn a
    // missing statistic into a silent wrong answer, which is the whole thing this is
    // meant to stop.
    let mut stats = column(0, 1, 10, 0);
    stats.min = None;
    assert_eq!(integer_sum_risk(&stats), SumRisk::Unknown);
    assert!(!integer_sum_risk(&stats).provably_safe());
}

#[test]
fn a_column_of_only_nulls_cannot_overflow() {
    // Nothing to add.
    assert_eq!(
        integer_sum_risk(&column(0, i64::MAX, 1_000, 1_000)),
        SumRisk::Safe
    );
}

#[test]
fn nulls_are_excluded_from_the_row_count() {
    // Counting them would inflate the estimate and raise alarms on columns that are
    // mostly empty -- which is a common shape and would make the check useless there.
    let mostly_null = column(0, i64::MAX / 4, 1_000_000, 999_999);
    assert_eq!(integer_sum_risk(&mostly_null), SumRisk::Safe);

    let all_present = column(0, i64::MAX / 4, 1_000_000, 0);
    assert!(matches!(
        integer_sum_risk(&all_present),
        SumRisk::Possible { .. }
    ));
}

#[test]
fn the_estimate_does_not_overflow_while_deciding_whether_the_sum_would() {
    // A comical way to get this wrong. The accumulator is twice the width of the type it
    // is reasoning about, and saturates besides.
    let extreme = column(i64::MIN, i64::MAX, u64::MAX, 0);
    assert!(matches!(
        integer_sum_risk(&extreme),
        SumRisk::Possible { .. }
    ));
}

#[test]
fn the_boundary_is_where_the_type_ends() {
    // Exactly at the maximum fits; one more does not.
    assert_eq!(integer_sum_risk(&column(0, i64::MAX, 1, 0)), SumRisk::Safe);
    assert!(matches!(
        integer_sum_risk(&column(0, i64::MAX, 2, 0)),
        SumRisk::Possible { .. }
    ));
}

#[test]
fn a_decimal_within_its_declared_precision_is_safe() {
    // A hundred thousand rows of at most a million: eleven digits, well inside 38.
    assert_eq!(
        decimal_sum_risk(&column(0, 1_000_000, 100_000, 0), 38),
        SumRisk::Safe
    );
}

#[test]
fn a_decimal_that_can_exceed_its_precision_is_flagged() {
    // The case that produced a wrong number rather than an error: a total past 38
    // digits comes back close to right instead of being refused. For a type chosen
    // because money must be exact, close is the wrong kind of wrong.
    //
    // It takes the extremes of both ranges to get there, and that is the finding rather
    // than an awkward fixture: with bounds held as 64-bit integers, values cap at about
    // nineteen digits, and nineteen digits summed over even a huge row count barely
    // reaches thirty-eight. See the note below on what that does and does not cover.
    let huge = column(i64::MAX, i64::MAX, u64::MAX, 0);
    assert!(matches!(
        decimal_sum_risk(&huge, 38),
        SumRisk::Possible { .. }
    ));
}

#[test]
fn a_decimal_wider_than_a_64_bit_bound_cannot_be_checked_at_all() {
    // The limitation, asserted so it is not mistaken for coverage.
    //
    // Bounds are held as 64-bit integers, so a decimal column whose values exceed about
    // nineteen digits has no representable bound — the statistics record nothing, and
    // this check answers "unknown" rather than "safe". That is the right answer and it
    // is not a useful one: the columns most likely to overflow a 38-digit decimal are
    // exactly the ones whose bounds cannot be recorded.
    //
    // Widening `Bound` to 128 bits would fix it. Until then, a caller must treat
    // `Unknown` on a decimal column as a real possibility rather than a formality.
    let mut wide = column(0, 0, 1_000_000, 0);
    wide.min = None;
    wide.max = None;

    assert_eq!(decimal_sum_risk(&wide, 38), SumRisk::Unknown);
    assert!(!decimal_sum_risk(&wide, 38).provably_safe());
}

#[test]
fn a_narrower_declared_precision_is_easier_to_exceed() {
    // The same column against different declarations. A decimal(9,0) column overflows
    // far sooner than a decimal(38,0) one, and the check has to reflect what was
    // declared rather than what the accumulator could hold.
    let stats = column(0, 1_000_000, 10_000, 0);
    assert_eq!(decimal_sum_risk(&stats, 38), SumRisk::Safe);
    assert!(matches!(
        decimal_sum_risk(&stats, 9),
        SumRisk::Possible { .. }
    ));
}

#[test]
fn an_unrepresentable_precision_is_reported_as_unbounded_rather_than_safe() {
    // Past 38 digits the accumulator, not the declaration, is the constraint. Saying
    // "safe" there would be answering a question that was not asked.
    let stats = column(0, 1_000_000, 10_000, 0);
    let risk = decimal_sum_risk(&stats, 40);
    assert_eq!(risk, SumRisk::Possible { widest_total: None });
    assert!(format!("{risk}").contains("128-bit"));
}
