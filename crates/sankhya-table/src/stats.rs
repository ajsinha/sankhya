//! Computing statistics from a batch that is already in hand.
//!
//! # Why compaction is the right place
//!
//! Statistics cost a pass over the data, and compaction has already paid for one. Any
//! other schedule pays it twice — once to compact and once to analyse — for a result
//! that is identical.
//!
//! It also means statistics arrive without anyone asking. The alternative is an analysis
//! command someone has to run, which on a system that onboards tables automatically from
//! a replication stream means the tables nobody thought about are exactly the ones with
//! no statistics.
//!
//! # What is computed cheaply and what is not
//!
//! Bounds come from Arrow's vectorised aggregate kernels and null counts from the array
//! metadata, so both are close to free. The distinct-value sketch has to hash every
//! value and is genuinely a pass over the data.
//!
//! That split was measured rather than assumed, and the first version of this module got
//! it badly wrong: it computed bounds in a scalar loop, which cost 29% of the merge on
//! its own — five times what the sketch costs. The claim in this comment was written
//! before the measurement and was false when written.
//!
//! The sketch is computed anyway, because a cardinality estimate is the one statistic
//! neither the file format nor the table log carries, and it is what join ordering needs
//! on schemas where nobody has run an analysis command.
//!
//! # What is skipped, and why that is safe
//!
//! A column whose type has no exact [`Bound`] gets **no bounds at all** rather than
//! approximate ones. An absent bound means the file is always read, so the cost of not
//! recognising a type is a scan. Recognising one incorrectly would cost an answer.

use arrow_array::cast::AsArray;
use arrow_array::types::{Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, UInt64Type};
use arrow_array::{Array, RecordBatch};
use arrow_schema::DataType;
use sankhya_stats::{Bound, ColumnStats};
use std::collections::BTreeMap;

/// Per-column statistics for a batch.
///
/// Columns whose type is not recognised still get row and null counts — those are always
/// correct and always useful — but no bounds and no sketch.
#[must_use]
pub fn column_stats(batch: &RecordBatch) -> BTreeMap<String, ColumnStats> {
    let mut out = BTreeMap::new();

    for (index, field) in batch.schema().fields().iter().enumerate() {
        let array = batch.column(index);
        let mut stats = ColumnStats {
            rows: u64::try_from(array.len()).unwrap_or(u64::MAX),
            nulls: u64::try_from(array.null_count()).unwrap_or(u64::MAX),
            ..ColumnStats::default()
        };

        observe(array.as_ref(), field.data_type(), &mut stats);
        out.insert(field.name().clone(), stats);
    }

    out
}

/// Fill in bounds, widths and the sketch for a recognised type.
fn observe(array: &dyn Array, data_type: &DataType, stats: &mut ColumnStats) {
    macro_rules! primitive {
        ($arrow_type:ty, $to_bound:expr, $width:expr) => {{
            let typed = array.as_primitive::<$arrow_type>();
            let to_bound = $to_bound;

            // Bounds from the vectorised kernel rather than a loop. The kernel skips
            // nulls and returns `None` for an all-null array, which is the same thing
            // an absent bound means.
            //
            // The kernel *propagates* NaN, though, and that is a trap. Refusing the NaN
            // here and keeping whatever bound was already recorded produces a bound
            // narrower than the truth — which is the one direction that skips a file it
            // should have read. So a NaN result invalidates the fast path entirely and
            // the column is measured again, skipping NaNs.
            let lo = arrow::compute::kernels::aggregate::min(typed).map(to_bound);
            let hi = arrow::compute::kernels::aggregate::max(typed).map(to_bound);

            if is_nan(lo.as_ref()) || is_nan(hi.as_ref()) {
                for i in 0..typed.len() {
                    if typed.is_null(i) {
                        continue;
                    }
                    widen(stats, to_bound(typed.value(i)));
                }
            } else {
                if let Some(bound) = lo {
                    widen(stats, bound);
                }
                if let Some(bound) = hi {
                    widen(stats, bound);
                }
            }

            // Fixed-width, so the total is arithmetic rather than a sum.
            let present = u64::try_from(typed.len() - typed.null_count()).unwrap_or(0);
            stats.total_width = present.saturating_mul($width);

            // The sketch is the one part that genuinely needs every value.
            for i in 0..typed.len() {
                if typed.is_null(i) {
                    continue;
                }
                stats.distinct.add(&typed.value(i).to_ne_bytes());
            }
        }};
    }

    match data_type {
        DataType::Int16 => primitive!(Int16Type, |v| Bound::Int(i64::from(v)), 2),
        DataType::Int32 => primitive!(Int32Type, |v| Bound::Int(i64::from(v)), 4),
        DataType::Int64 => primitive!(Int64Type, Bound::Int, 8),
        DataType::UInt64 => primitive!(
            UInt64Type,
            |v: u64| Bound::Int(i64::try_from(v).unwrap_or(i64::MAX)),
            8
        ),
        DataType::Float32 => primitive!(Float32Type, |v| Bound::Float(f64::from(v)), 4),
        DataType::Float64 => primitive!(Float64Type, Bound::Float, 8),
        DataType::Utf8 => {
            let typed = array.as_string::<i32>();
            if let Some(v) = arrow::compute::kernels::aggregate::min_string(typed) {
                widen(stats, Bound::Bytes(v.as_bytes().to_vec()));
            }
            if let Some(v) = arrow::compute::kernels::aggregate::max_string(typed) {
                widen(stats, Bound::Bytes(v.as_bytes().to_vec()));
            }
            // Variable width, so this one does have to be summed -- but the offsets
            // give it without touching the values.
            let offsets = typed.offsets();
            let bytes = offsets
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_sub(offsets.first().copied().unwrap_or(0));
            stats.total_width = u64::try_from(bytes).unwrap_or(0);

            for i in 0..typed.len() {
                if typed.is_null(i) {
                    continue;
                }
                stats.distinct.add(typed.value(i).as_bytes());
            }
        }
        // Deliberately unhandled: no bounds, no sketch, and the file is always read.
        // Adding a type here is an optimisation; getting one wrong is a lost row.
        _ => {}
    }
}

/// Whether a bound is a NaN, and therefore useless.
fn is_nan(bound: Option<&Bound>) -> bool {
    matches!(bound, Some(Bound::Float(f)) if f.is_nan())
}

/// Extend the bounds to include `bound`.
///
/// Never narrows. A value that cannot be compared against the existing bound — a NaN,
/// most obviously — leaves the bounds untouched rather than replacing them, because a
/// bound nothing can be compared against makes every later merge incomparable and loses
/// the bounds for the whole partition.
fn widen(stats: &mut ColumnStats, bound: Bound) {
    if matches!(&bound, Bound::Float(f) if f.is_nan()) {
        return;
    }

    stats.min = match stats.min.take() {
        None => Some(bound.clone()),
        Some(existing) => match existing.compare(&bound) {
            Some(std::cmp::Ordering::Greater) => Some(bound.clone()),
            _ => Some(existing),
        },
    };
    stats.max = match stats.max.take() {
        None => Some(bound.clone()),
        Some(existing) => match existing.compare(&bound) {
            Some(std::cmp::Ordering::Less) => Some(bound),
            _ => Some(existing),
        },
    };
}
