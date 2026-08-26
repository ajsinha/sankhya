//! Deciding whether a file can be skipped.
//!
//! Every function here answers one question — *can this file be skipped?* — and answers
//! **no** whenever it is not certain. See the crate documentation for why that asymmetry
//! is the whole design: a needless read costs time, and a wrong skip costs an answer
//! that nothing downstream can detect.

use crate::column::{Bound, ColumnStats};
use std::cmp::Ordering;

/// A predicate on one column.
#[derive(Clone, PartialEq, Debug)]
pub enum Predicate {
    Equals(Bound),
    LessThan(Bound),
    LessOrEqual(Bound),
    GreaterThan(Bound),
    GreaterOrEqual(Bound),
    /// `column BETWEEN low AND high`, inclusive.
    Between {
        low: Bound,
        high: Bound,
    },
    IsNull,
    IsNotNull,
}

/// Whether no row in a file with these statistics can satisfy `predicate`.
///
/// `true` means the file is provably irrelevant and may be skipped. `false` means either
/// that it may match or that the statistics do not establish otherwise, and those two
/// cases are deliberately not distinguished — a caller that could tell them apart would
/// eventually be tempted to act on the difference.
#[must_use]
pub fn can_skip(stats: &ColumnStats, predicate: &Predicate) -> bool {
    // An empty file matches nothing at all.
    if stats.rows == 0 {
        return true;
    }

    match predicate {
        Predicate::IsNull => stats.nulls == 0,
        Predicate::IsNotNull => stats.all_null(),

        // A file of nothing but nulls satisfies no comparison, whatever its bounds say.
        _ if stats.all_null() => true,

        Predicate::Equals(target) => below_min(stats, target) || above_max(stats, target),
        // column < target: skippable only if every value is at or above the target.
        Predicate::LessThan(target) => matches!(
            compare_min(stats, target),
            Some(Ordering::Greater | Ordering::Equal)
        ),
        Predicate::LessOrEqual(target) => {
            matches!(compare_min(stats, target), Some(Ordering::Greater))
        }
        Predicate::GreaterThan(target) => matches!(
            compare_max(stats, target),
            Some(Ordering::Less | Ordering::Equal)
        ),
        Predicate::GreaterOrEqual(target) => {
            matches!(compare_max(stats, target), Some(Ordering::Less))
        }
        Predicate::Between { low, high } => above_max(stats, low) || below_min(stats, high),
    }
}

/// `min` against `target`, or `None` when the comparison cannot be made.
fn compare_min(stats: &ColumnStats, target: &Bound) -> Option<Ordering> {
    stats.min.as_ref()?.compare(target)
}

fn compare_max(stats: &ColumnStats, target: &Bound) -> Option<Ordering> {
    stats.max.as_ref()?.compare(target)
}

/// Whether the target is strictly below everything in the file.
fn below_min(stats: &ColumnStats, target: &Bound) -> bool {
    matches!(compare_min(stats, target), Some(Ordering::Greater))
}

/// Whether the target is strictly above everything in the file.
fn above_max(stats: &ColumnStats, target: &Bound) -> bool {
    matches!(compare_max(stats, target), Some(Ordering::Less))
}
