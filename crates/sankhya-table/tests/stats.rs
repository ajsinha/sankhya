//! Statistics computed from a batch.
//!
//! Everything here is about bounds being *correct*, because a bound that is wrong in the
//! narrowing direction causes a file to be skipped that should have been read — and that
//! loss is silent.

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

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_stats::{can_skip, Bound, Predicate};
use sankhya_table::column_stats;
use std::sync::Arc;

fn ints(values: Vec<Option<i64>>) -> RecordBatch {
    RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, true)])),
        vec![Arc::new(Int64Array::from(values))],
    )
    .expect("building")
}

fn floats(values: Vec<Option<f64>>) -> RecordBatch {
    RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("f", DataType::Float64, true)])),
        vec![Arc::new(Float64Array::from(values))],
    )
    .expect("building")
}

fn strings(values: Vec<Option<&str>>) -> RecordBatch {
    RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("s", DataType::Utf8, true)])),
        vec![Arc::new(StringArray::from(values))],
    )
    .expect("building")
}

#[test]
fn bounds_are_the_true_extremes() {
    // Deliberately unsorted and starting with neither extreme, so an implementation that
    // took the first and last values, or that assigned min and max the wrong way round,
    // fails here.
    let stats = column_stats(&ints(vec![
        Some(50),
        Some(-3),
        Some(900),
        Some(7),
        Some(-1_000),
        Some(12),
    ]));
    let n = &stats["n"];

    assert_eq!(n.min, Some(Bound::Int(-1_000)));
    assert_eq!(n.max, Some(Bound::Int(900)));
    assert_eq!(n.rows, 6);
    assert_eq!(n.nulls, 0);
}

#[test]
fn the_bounds_actually_prune_correctly() {
    // The consequence, rather than the value. A file holding -1000..=900 must be skipped
    // for anything outside that and read for anything inside it.
    let stats = column_stats(&ints(vec![Some(-1_000), Some(0), Some(900)]));
    let n = &stats["n"];

    assert!(can_skip(n, &Predicate::GreaterThan(Bound::Int(900))));
    assert!(can_skip(n, &Predicate::LessThan(Bound::Int(-1_000))));
    assert!(!can_skip(n, &Predicate::Equals(Bound::Int(0))));
    assert!(!can_skip(n, &Predicate::Equals(Bound::Int(-1_000))));
    assert!(!can_skip(n, &Predicate::Equals(Bound::Int(900))));
}

#[test]
fn nulls_are_counted_and_excluded_from_bounds() {
    let stats = column_stats(&ints(vec![None, Some(5), None, Some(9), None]));
    let n = &stats["n"];

    assert_eq!(n.rows, 5);
    assert_eq!(n.nulls, 3);
    assert_eq!(n.min, Some(Bound::Int(5)));
    assert_eq!(n.max, Some(Bound::Int(9)));
    assert!((n.null_fraction() - 0.6).abs() < 1e-9);
    assert!(!can_skip(n, &Predicate::IsNull));
}

#[test]
fn an_all_null_column_has_no_bounds_and_skips_every_comparison() {
    let stats = column_stats(&ints(vec![None, None]));
    let n = &stats["n"];

    assert_eq!(n.min, None);
    assert_eq!(n.max, None);
    assert!(n.all_null());
    assert!(can_skip(n, &Predicate::Equals(Bound::Int(0))));
    assert!(!can_skip(n, &Predicate::IsNull));
}

#[test]
fn a_nan_never_becomes_a_bound() {
    // A NaN bound is incomparable with everything, so it would make every later merge
    // incomparable and lose the bounds for the whole partition -- and while it stood, no
    // file would ever be skipped on that column again.
    let stats = column_stats(&floats(vec![
        Some(f64::NAN),
        Some(2.5),
        Some(-1.5),
        Some(f64::NAN),
    ]));
    let f = &stats["f"];

    assert_eq!(f.min, Some(Bound::Float(-1.5)));
    assert_eq!(f.max, Some(Bound::Float(2.5)));
    assert!(can_skip(f, &Predicate::GreaterThan(Bound::Float(2.5))));
}

#[test]
fn string_bounds_are_lexicographic() {
    let stats = column_stats(&strings(vec![
        Some("mango"),
        Some("apple"),
        Some("zebra"),
        Some("Apple"),
    ]));
    let s = &stats["s"];

    // Uppercase sorts before lowercase in byte order, which is what the column is
    // actually sorted by.
    assert_eq!(s.min, Some(Bound::Bytes(b"Apple".to_vec())));
    assert_eq!(s.max, Some(Bound::Bytes(b"zebra".to_vec())));
}

#[test]
fn widths_are_measured_without_touching_the_values() {
    let stats = column_stats(&strings(vec![Some("ab"), None, Some("cdef")]));
    let s = &stats["s"];
    assert_eq!(s.total_width, 6);
    assert_eq!(s.average_width(), 3.0);

    let stats = column_stats(&ints(vec![Some(1), Some(2), None]));
    let n = &stats["n"];
    assert_eq!(n.total_width, 16);
}

#[test]
fn the_sketch_counts_distinct_values_not_rows() {
    let mut values = Vec::new();
    for _ in 0..200 {
        for i in 0..30i64 {
            values.push(Some(i));
        }
    }
    let stats = column_stats(&ints(values));
    let n = &stats["n"];

    assert_eq!(n.rows, 6_000);
    let estimate = n.distinct_estimate();
    assert!(
        (25..=35).contains(&estimate),
        "6,000 rows of 30 distinct values estimated as {estimate}"
    );
}

#[test]
fn an_unrecognised_type_still_gets_counts_but_no_bounds() {
    // Counts are always correct and always useful. Bounds are absent rather than
    // guessed, so the file is always read -- which costs a scan, not an answer.
    let schema = Arc::new(Schema::new(vec![Field::new("b", DataType::Boolean, true)]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(arrow_array::BooleanArray::from(vec![
            Some(true),
            None,
            Some(false),
        ]))],
    )
    .expect("building");

    let stats = column_stats(&batch);
    let b = &stats["b"];

    assert_eq!(b.rows, 3);
    assert_eq!(b.nulls, 1);
    assert_eq!(b.min, None);
    assert!(!can_skip(b, &Predicate::Equals(Bound::Int(1))));
}

#[test]
fn every_column_gets_an_entry() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("a", DataType::Int64, false),
        Field::new("b", DataType::Utf8, false),
        Field::new("c", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1i64])),
            Arc::new(StringArray::from(vec!["x"])),
            Arc::new(Float64Array::from(vec![1.5f64])),
        ],
    )
    .expect("building");

    let stats = column_stats(&batch);
    assert_eq!(stats.len(), 3);
    assert!(stats.contains_key("a"));
    assert!(stats.contains_key("b"));
    assert!(stats.contains_key("c"));
}

#[test]
fn a_nan_never_narrows_a_bound_into_skipping_a_real_row() {
    // The failure this guards, stated as the consequence rather than the value.
    //
    // Arrow's aggregate kernels propagate NaN, so `max` over a column containing one
    // returns NaN. Refusing that NaN and keeping whatever bound was recorded so far
    // leaves a maximum *below* the true maximum — and a file holding 2.5 is then skipped
    // for `f > 0`, silently, because its statistics claim it tops out at -1.5.
    //
    // That was the behaviour before this test existed.
    let stats = column_stats(&floats(vec![Some(-1.5), Some(2.5), Some(f64::NAN)]));
    let f = &stats["f"];

    assert!(
        !can_skip(f, &Predicate::GreaterThan(Bound::Float(0.0))),
        "the file holds 2.5 and would have been skipped: bounds are {:?}..{:?}",
        f.min,
        f.max
    );
    assert!(
        !can_skip(f, &Predicate::Equals(Bound::Float(2.5))),
        "the file holds exactly this value"
    );
}

#[test]
fn a_column_of_only_nans_has_no_bounds_at_all() {
    // No orderable value means no bound, which means the file is always read. Recording
    // any bound here would be recording a fiction.
    let stats = column_stats(&floats(vec![Some(f64::NAN), Some(f64::NAN)]));
    let f = &stats["f"];

    assert_eq!(f.min, None);
    assert_eq!(f.max, None);
    assert_eq!(f.rows, 2);
    assert!(!can_skip(f, &Predicate::Equals(Bound::Float(1.0))));
}

#[test]
fn infinities_are_ordered_normally() {
    // Infinity is orderable, unlike NaN, so it is a perfectly good bound.
    let stats = column_stats(&floats(vec![
        Some(f64::NEG_INFINITY),
        Some(0.0),
        Some(f64::INFINITY),
    ]));
    let f = &stats["f"];

    assert_eq!(f.min, Some(Bound::Float(f64::NEG_INFINITY)));
    assert_eq!(f.max, Some(Bound::Float(f64::INFINITY)));
    assert!(!can_skip(f, &Predicate::GreaterThan(Bound::Float(1e300))));
}

// --- the types a warehouse filters on (`M24`) -------------------------------------------

#[test]
fn a_date_column_gets_bounds_and_they_prune() {
    // Until `M24` this type was in the unhandled list: no bounds, every file read, and a
    // date-ranged query --- which is most of them --- pruned nothing at all.
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("d", DataType::Date32, true)])),
        vec![Arc::new(arrow_array::Date32Array::from(vec![
            Some(20_700),
            Some(20_705),
            Some(20_710),
        ]))],
    )
    .expect("building");

    let stats = &column_stats(&batch)["d"];
    assert_eq!(stats.min, Some(Bound::Date(20_700)));
    assert_eq!(stats.max, Some(Bound::Date(20_710)));
    assert!(can_skip(stats, &Predicate::LessThan(Bound::Date(20_700))));
    assert!(!can_skip(stats, &Predicate::Equals(Bound::Date(20_705))));
    // And an integer of the same magnitude proves nothing, because a day count and a
    // number are not the same kind of thing.
    assert!(!can_skip(stats, &Predicate::LessThan(Bound::Int(20_700))));
}

#[test]
fn a_timestamp_column_records_the_unit_it_counts_in() {
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new(
            "t",
            DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, None),
            true,
        )])),
        vec![Arc::new(arrow_array::TimestampMillisecondArray::from(vec![
            Some(1_000),
            Some(9_000),
        ]))],
    )
    .expect("building");

    let stats = &column_stats(&batch)["t"];
    assert_eq!(
        stats.min,
        Some(Bound::Timestamp {
            value: 1_000,
            unit: sankhya_stats::TimeUnit::Millisecond
        })
    );
    // One second past the epoch, asked in seconds, is the same instant --- so a file whose
    // earliest value *is* that instant cannot be skipped by `< it`... but it can by `<`
    // anything earlier. Both directions, because the unit is what makes either true.
    assert!(can_skip(
        stats,
        &Predicate::LessThan(Bound::Timestamp {
            value: 1,
            unit: sankhya_stats::TimeUnit::Second
        })
    ));
    assert!(!can_skip(
        stats,
        &Predicate::LessThan(Bound::Timestamp {
            value: 5,
            unit: sankhya_stats::TimeUnit::Second
        })
    ));
}

#[test]
fn a_decimal_column_keeps_its_scale() {
    // Money. `1234.56` is an unscaled 123,456 at scale 2, and a bound that dropped the
    // scale would be a hundred times the amount.
    let values = arrow_array::Decimal128Array::from(vec![Some(123_456_i128), Some(999_999)])
        .with_precision_and_scale(18, 2)
        .expect("a declared decimal");
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new(
            "m",
            DataType::Decimal128(18, 2),
            true,
        )])),
        vec![Arc::new(values)],
    )
    .expect("building");

    let stats = &column_stats(&batch)["m"];
    assert_eq!(
        stats.min,
        Some(Bound::Decimal {
            unscaled: 123_456,
            scale: 2
        })
    );
    // The same amount written at a different scale is the same amount.
    assert!(can_skip(
        stats,
        &Predicate::LessThan(Bound::Decimal {
            unscaled: 12_345_6,
            scale: 2
        })
    ));
    assert!(!can_skip(
        stats,
        &Predicate::LessThan(Bound::Decimal {
            unscaled: 1_234_570,
            scale: 3
        })
    ));
}

#[test]
fn a_u64_past_the_signed_range_leaves_the_column_unbounded() {
    // Bounds are kept in a signed space, and a `u64` past `i64::MAX` saturates to it --- so
    // the file would claim a maximum that a value it holds exceeds, and
    // `n > 9223372036854775807` would skip it. The rows that predicate hides are real.
    //
    // Unbounded instead, which costs a scan of a column nothing in this warehouse writes.
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("n", DataType::UInt64, true)])),
        vec![Arc::new(arrow_array::UInt64Array::from(vec![
            Some(1_u64),
            Some(u64::MAX),
        ]))],
    )
    .expect("building");

    let stats = &column_stats(&batch)["n"];
    assert_eq!(stats.min, None, "a saturated maximum invalidates both bounds");
    assert_eq!(stats.max, None);
    assert!(!can_skip(stats, &Predicate::GreaterThan(Bound::Int(i64::MAX))));

    // And a column that stays inside the range keeps its bounds, so the guard above is not
    // simply switching the type off.
    let ordinary = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("n", DataType::UInt64, true)])),
        vec![Arc::new(arrow_array::UInt64Array::from(vec![
            Some(1_u64),
            Some(9_u64),
        ]))],
    )
    .expect("building");
    assert_eq!(column_stats(&ordinary)["n"].max, Some(Bound::Int(9)));
}
