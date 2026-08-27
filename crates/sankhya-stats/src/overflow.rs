//! Deciding, before a query runs, whether summing a column can overflow.
//!
//! # Why this is worth predicting rather than detecting
//!
//! The analytical engine does not detect it. A sum of 64-bit integers that exceeds the
//! range wraps and returns a large negative number where the answer is a large positive
//! one; a sum of decimals past 38 digits loses exactness and returns something close to
//! the right answer. Neither raises anything. The transactional tier, asked the same
//! question, widens its accumulator and gives the exact figure.
//!
//! So the same query against the two tiers can disagree, in the direction that matters,
//! with nothing to indicate it. Detecting it afterwards is not on offer — by then the
//! number is already in a report.
//!
//! # Why bounds are enough
//!
//! A column's maximum magnitude and its row count bound the sum: nothing can exceed
//! `max(|min|, |max|) × rows`. That is a gross over-estimate for real data, and being an
//! over-estimate is exactly right here — it can say *this cannot overflow* and be
//! certain, or *this might* and be wrong in the safe direction.
//!
//! **The asymmetry is the design.** A false alarm costs a refused query that would have
//! been fine; a missed one costs a wrong number nobody notices. Where the bounds are
//! unknown, the answer is "might", because unknown is not "small".

use crate::column::{Bound, ColumnStats};
use std::fmt;

/// What summing a column might do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SumRisk {
    /// The bounds prove the total fits.
    Safe,
    /// The bounds do not prove it fits.
    ///
    /// Not a prediction that it *will* overflow — an admission that nothing here rules
    /// it out.
    Possible { widest_total: Option<i128> },
    /// Nothing is known about the column's range.
    Unknown,
}

impl SumRisk {
    /// Whether a query summing this column should be allowed to run unexamined.
    #[must_use]
    pub const fn provably_safe(self) -> bool {
        matches!(self, Self::Safe)
    }
}

impl fmt::Display for SumRisk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safe => f.write_str("the total cannot leave the range of the type"),
            Self::Possible { widest_total } => match widest_total {
                Some(total) => write!(
                    f,
                    "the total could reach {total}, which does not fit; the analytical \
                     engine would wrap or lose exactness rather than refuse, and the \
                     transactional tier would give a different answer"
                ),
                None => f.write_str(
                    "the total could exceed what a 128-bit accumulator holds, so nothing \
                     here rules out an overflow",
                ),
            },
            Self::Unknown => f.write_str(
                "the column has no recorded bounds, and unknown is not the same as small",
            ),
        }
    }
}

/// Whether summing an integer column can leave the range of a 64-bit integer.
///
/// Uses a 128-bit accumulator for the estimate, so the estimate itself cannot overflow
/// while calculating whether the sum would — which would be a comical way to get this
/// wrong.
#[must_use]
pub fn integer_sum_risk(stats: &ColumnStats) -> SumRisk {
    let rows = i128::from(stats.rows.saturating_sub(stats.nulls));
    if rows == 0 {
        return SumRisk::Safe;
    }

    let (Some(Bound::Int(min)), Some(Bound::Int(max))) = (&stats.min, &stats.max) else {
        return SumRisk::Unknown;
    };

    // The widest the total can be in either direction. Both ends matter: a column of
    // large negatives overflows just as readily as one of large positives.
    let widest = i128::from(*min)
        .saturating_mul(rows)
        .abs()
        .max(i128::from(*max).saturating_mul(rows).abs());

    if widest <= i128::from(i64::MAX) {
        return SumRisk::Safe;
    }
    SumRisk::Possible {
        widest_total: Some(widest),
    }
}

/// Whether summing a decimal column can exceed `digits` significant digits.
///
/// The analytical engine's decimal is fixed at 38 digits; past that it stops being
/// exact, which for a type chosen *because* money must be exact is the wrong kind of
/// wrong.
#[must_use]
pub fn decimal_sum_risk(stats: &ColumnStats, digits: u32) -> SumRisk {
    let rows = i128::from(stats.rows.saturating_sub(stats.nulls));
    if rows == 0 {
        return SumRisk::Safe;
    }

    let (Some(Bound::Int(min)), Some(Bound::Int(max))) = (&stats.min, &stats.max) else {
        return SumRisk::Unknown;
    };

    let widest = i128::from(*min)
        .saturating_mul(rows)
        .abs()
        .max(i128::from(*max).saturating_mul(rows).abs());

    // 10^digits - 1 is the largest value with that many digits. Above 38 the limit
    // exceeds what an i128 holds, at which point the accumulator is the constraint
    // rather than the declared precision.
    let Some(limit) = ten_to(digits) else {
        return SumRisk::Possible { widest_total: None };
    };

    if widest < limit {
        return SumRisk::Safe;
    }
    SumRisk::Possible {
        widest_total: Some(widest),
    }
}

/// `10^n`, or `None` when it does not fit in an `i128`.
fn ten_to(n: u32) -> Option<i128> {
    let mut value: i128 = 1;
    for _ in 0..n {
        value = value.checked_mul(10)?;
    }
    Some(value)
}
