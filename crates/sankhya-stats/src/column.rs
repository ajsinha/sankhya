//! Per-column statistics, and what it means to merge them.

use crate::sketch::DistinctSketch;
use std::fmt;

/// A comparable bound on a column's values.
///
/// Comparison is only ever within a variant. Two bounds of different variants are
/// **incomparable**, not coerced — a comparison across types is where a bound quietly
/// stops meaning what it says, and the cost of that is a skipped file rather than a
/// slow one.
#[derive(Clone, PartialEq, Debug)]
pub enum Bound {
    Int(i64),
    Float(f64),
    /// Lexicographic, matching how the column is sorted.
    Bytes(Vec<u8>),
}

impl Bound {
    /// Order, or `None` when the two are not comparable.
    ///
    /// A NaN float is incomparable with everything including itself, which is the
    /// correct answer and is what stops it being used as a bound.
    #[must_use]
    pub fn compare(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => Some(a.cmp(b)),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b),
            (Self::Bytes(a), Self::Bytes(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }

    fn min_of(a: &Self, b: &Self) -> Option<Self> {
        match a.compare(b)? {
            std::cmp::Ordering::Greater => Some(b.clone()),
            _ => Some(a.clone()),
        }
    }

    fn max_of(a: &Self, b: &Self) -> Option<Self> {
        match a.compare(b)? {
            std::cmp::Ordering::Less => Some(b.clone()),
            _ => Some(a.clone()),
        }
    }
}

/// Why two statistics could not be merged.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MergeError {
    /// The bounds are of different types.
    ///
    /// Merging them would require choosing an ordering across types, and the resulting
    /// bound would not be a bound on anything. Refusing leaves the caller to drop the
    /// bounds, which costs a scan rather than an answer.
    IncomparableBounds,
}

impl fmt::Display for MergeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "the bounds are of different types, so a merged bound would not bound \
             anything; drop the bounds instead, which costs a scan rather than an answer",
        )
    }
}

impl std::error::Error for MergeError {}

/// What is known about one column of one file.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ColumnStats {
    pub rows: u64,
    pub nulls: u64,
    /// Absent when unknown, and unknown is not the same as unbounded.
    ///
    /// An absent bound means "this may match anything", so a file with absent bounds is
    /// always read. A caller that fills them in with a default has turned a missing
    /// statistic into a wrong one.
    pub min: Option<Bound>,
    pub max: Option<Bound>,
    pub distinct: DistinctSketch,
    /// Total bytes across values, for average-width estimation.
    pub total_width: u64,
}

impl ColumnStats {
    #[must_use]
    pub fn null_fraction(&self) -> f64 {
        if self.rows == 0 {
            return 0.0;
        }
        self.nulls as f64 / self.rows as f64
    }

    #[must_use]
    pub fn average_width(&self) -> f64 {
        let present = self.rows.saturating_sub(self.nulls);
        if present == 0 {
            return 0.0;
        }
        self.total_width as f64 / present as f64
    }

    /// The estimated distinct-value count.
    ///
    /// Approximate, and never usable for pruning. See the crate documentation.
    #[must_use]
    pub fn distinct_estimate(&self) -> u64 {
        self.distinct.estimate()
    }

    /// Whether the column holds any non-null value.
    ///
    /// This, rather than the row count, is what decides whether a side contributes to a
    /// merged bound: bounds describe non-null values, so a file that is empty *or*
    /// entirely null bounds nothing and merging it must leave the other side's bounds
    /// alone.
    #[must_use]
    pub const fn has_values(&self) -> bool {
        self.rows > self.nulls
    }

    /// Whether every value in the column is null.
    ///
    /// Worth its own accessor because it is the one case where absent bounds are
    /// informative rather than merely unknown.
    #[must_use]
    pub fn all_null(&self) -> bool {
        self.rows > 0 && self.nulls == self.rows
    }

    /// Record one value.
    pub fn observe(&mut self, value: Option<&[u8]>, bound: Option<Bound>) {
        self.rows = self.rows.saturating_add(1);
        let Some(bytes) = value else {
            self.nulls = self.nulls.saturating_add(1);
            return;
        };
        self.total_width = self
            .total_width
            .saturating_add(bytes.len().try_into().unwrap_or(u64::MAX));
        self.distinct.add(bytes);

        let Some(bound) = bound else { return };
        // A bound that cannot be compared is not recorded. Recording it would make every
        // later merge incomparable, losing the bounds for the whole partition.
        if matches!(&bound, Bound::Float(f) if f.is_nan()) {
            return;
        }
        self.min = match self.min.take() {
            None => Some(bound.clone()),
            Some(existing) => Bound::min_of(&existing, &bound).or(Some(existing)),
        };
        self.max = match self.max.take() {
            None => Some(bound.clone()),
            Some(existing) => Bound::max_of(&existing, &bound).or(Some(existing)),
        };
    }

    /// Combine with statistics for another file.
    ///
    /// Exact for counts and bounds; the distinct sketch merges exactly too, in the sense
    /// that merging gives the same registers as sketching the union. This is what makes
    /// statistics maintainable at compaction: the merged file's statistics are the merge
    /// of its inputs', with no value re-read.
    ///
    /// # Errors
    ///
    /// Returns [`MergeError::IncomparableBounds`] when the two carry bounds of different
    /// types. The counts would merge fine, but silently dropping only the bounds would
    /// leave a caller believing it had merged everything.
    pub fn merge(&mut self, other: &Self) -> Result<(), MergeError> {
        if let (Some(a), Some(b)) = (&self.min, &other.min) {
            if a.compare(b).is_none() {
                return Err(MergeError::IncomparableBounds);
            }
        }
        if let (Some(a), Some(b)) = (&self.max, &other.max) {
            if a.compare(b).is_none() {
                return Err(MergeError::IncomparableBounds);
            }
        }

        // Captured before the counts change, since the counts are what define it.
        let had_values = self.has_values();

        self.rows = self.rows.saturating_add(other.rows);
        self.nulls = self.nulls.saturating_add(other.nulls);
        self.total_width = self.total_width.saturating_add(other.total_width);
        self.distinct.merge(&other.distinct);

        // A side with no non-null values bounds nothing, so merging it is the identity
        // on bounds. Treating it as "unbounded" instead would drop perfectly good bounds
        // every time compaction merged into a fresh accumulator, which is safe and
        // needlessly slow.
        //
        // Where both sides do hold values, an absent bound on either makes the merged
        // bound absent: the union is only bounded if both halves are, and inheriting one
        // side's bound would claim a limit the other side may exceed. That is the one
        // way a merge can produce a skip that hides a row.
        let mine = self.min.take();
        let theirs = other.min.clone();
        self.min = if !other.has_values() {
            mine
        } else if !had_values {
            theirs
        } else {
            match (mine, theirs) {
                (Some(a), Some(b)) => Bound::min_of(&a, &b),
                _ => None,
            }
        };

        let mine = self.max.take();
        let theirs = other.max.clone();
        self.max = if !other.has_values() {
            mine
        } else if !had_values {
            theirs
        } else {
            match (mine, theirs) {
                (Some(a), Some(b)) => Bound::max_of(&a, &b),
                _ => None,
            }
        };

        Ok(())
    }
}
