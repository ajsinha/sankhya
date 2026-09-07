//! Encoding tests.
//!
//! The refusals are the point. Every "unparseable" case here is one a more permissive
//! encoder would have turned into a null — producing a row that is present, a query
//! that succeeds, and one column silently empty. Row counts would still reconcile.

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

use arrow_array::{Array, Decimal128Array, Int64Array, StringArray, TimestampMicrosecondArray};
use proptest::prelude::*;
use sankhya_cdc_apply::{Mutation, Op, Row};
use sankhya_schema::{Field, LogicalSchema, LogicalType, Precision};
use sankhya_table::{encode_batch, EncodeError};
use sankhya_types::Lsn;

fn schema(fields: Vec<(&str, LogicalType, bool)>) -> LogicalSchema {
    LogicalSchema::new(
        fields
            .into_iter()
            .map(|(name, logical, nullable)| Field {
                name: name.into(),
                logical,
                nullable,
                is_key: false,
            })
            .collect(),
    )
}

fn row(values: Vec<Option<&str>>, lsn: u64) -> Mutation {
    Mutation {
        relation_id: 1,
        op: Op::Insert,
        row: Row {
            values: values.into_iter().map(|v| v.map(str::to_string)).collect(),
        },
        commit_lsn: Lsn::new(lsn),
    }
}

fn dec(digits: u8, scale: u8) -> LogicalType {
    LogicalType::Decimal(Precision::new(digits, scale).expect("valid precision"))
}

#[test]
fn a_batch_carries_provenance_columns() {
    let s = schema(vec![("id", LogicalType::Int64, false)]);
    let batch =
        encode_batch(&s, &[row(vec![Some("1")], 100), row(vec![Some("2")], 200)]).expect("encodes");

    assert_eq!(batch.num_rows(), 2);
    assert_eq!(
        batch.num_columns(),
        4,
        "one declared column plus three provenance columns"
    );

    let arrow_schema = batch.schema();
    let names: Vec<&str> = arrow_schema
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    assert_eq!(
        names,
        [
            "id",
            "_sankhya_commit_lsn",
            "_sankhya_commit_ts",
            "_sankhya_op"
        ]
    );
}

#[test]
fn the_commit_position_travels_with_every_row() {
    // Provenance in the data itself is what makes the applied position recoverable
    // from the table's own history rather than from external state that could drift.
    let s = schema(vec![("id", LogicalType::Int64, false)]);
    let batch = encode_batch(
        &s,
        &[row(vec![Some("1")], 4242), row(vec![Some("2")], 4242)],
    )
    .expect("encodes");
    let lsn = batch
        .column(1)
        .as_any()
        .downcast_ref::<arrow_array::UInt64Array>()
        .expect("a position column");
    assert_eq!(lsn.value(0), 4242);
    assert_eq!(lsn.value(1), 4242);
}

#[test]
fn a_commit_time_nothing_knows_is_null_rather_than_the_epoch() {
    // A `Mutation` carries a commit **position** and no commit time: the time is on the
    // transaction's `BEGIN` in a replication stream, and nothing in this build reads one
    // (`ING-00`). The column was declared non-null, so the writer had to supply something,
    // and it supplied `0` --- every captured row stamped `1970-01-01T00:00:00Z`.
    //
    // That is worse than a null in the way that matters here: a reader cannot tell it from a
    // real instant, and `WHERE _sankhya_commit_ts > <anything>` silently excludes every row
    // while looking like a filter that found nothing. `ING-09`.
    let s = schema(vec![("id", LogicalType::Int64, false)]);
    let batch = encode_batch(&s, &[row(vec![Some("1")], 7)]).expect("encodes");
    let ts = batch
        .column(2)
        .as_any()
        .downcast_ref::<arrow_array::TimestampMicrosecondArray>()
        .expect("a timestamp column");
    assert!(ts.is_null(0), "the epoch is a wrong instant, not a missing one");
}

#[test]
fn decimals_are_exact_and_never_pass_through_floating_point() {
    // A value that f64 cannot represent exactly. Going via floating point would
    // introduce error in the one type whose entire purpose is not to have any.
    let s = schema(vec![("amount", dec(18, 4), false)]);
    let batch = encode_batch(
        &s,
        &[
            row(vec![Some("12345678901234.5678")], 1),
            row(vec![Some("-0.0001")], 2),
            row(vec![Some("0")], 3),
        ],
    )
    .expect("encodes");

    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .expect("a decimal column");
    assert_eq!(column.value(0), 123_456_789_012_345_678_i128);
    assert_eq!(column.value(1), -1);
    assert_eq!(column.value(2), 0);
}

#[test]
fn a_decimal_with_more_precision_than_declared_is_refused_not_rounded() {
    // Silently dropping a digit is precisely the corruption this type exists to
    // prevent, and the resulting value would look entirely plausible.
    let s = schema(vec![("amount", dec(10, 2), false)]);
    let err = encode_batch(&s, &[row(vec![Some("1.239")], 1)]).expect_err("must refuse");
    assert!(matches!(err, EncodeError::Unparseable { .. }), "{err:?}");
}

#[test]
fn trailing_zeros_beyond_the_declared_scale_are_accepted() {
    // They carry no information, so refusing them would be pedantry rather than safety.
    let s = schema(vec![("amount", dec(10, 2), false)]);
    let batch = encode_batch(&s, &[row(vec![Some("1.2300")], 1)]).expect("encodes");
    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .expect("decimal");
    assert_eq!(column.value(0), 123);
}

#[test]
fn an_unparseable_value_is_an_error_not_a_null() {
    // The central rule. A null here would turn a parsing defect into missing data:
    // the row present, the query successful, one column silently empty — and row
    // counts would still reconcile.
    let s = schema(vec![("id", LogicalType::Int64, true)]);
    let err = encode_batch(&s, &[row(vec![Some("not-a-number")], 1)]).expect_err("must refuse");
    let EncodeError::Unparseable { column, value, .. } = err else {
        panic!("expected unparseable, got {err:?}");
    };
    assert_eq!(column, "id");
    assert_eq!(value, "not-a-number");
}

#[test]
fn a_null_in_a_not_null_column_is_refused() {
    let s = schema(vec![("id", LogicalType::Int64, false)]);
    let err = encode_batch(&s, &[row(vec![None], 1)]).expect_err("must refuse");
    assert!(matches!(err, EncodeError::UnexpectedNull { .. }));
}

#[test]
fn nulls_are_preserved_where_the_column_permits_them() {
    let s = schema(vec![("label", LogicalType::Utf8, true)]);
    let batch =
        encode_batch(&s, &[row(vec![None], 1), row(vec![Some("present")], 2)]).expect("encodes");
    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("strings");
    assert!(column.is_null(0));
    assert_eq!(column.value(1), "present");
}

#[test]
fn zoned_timestamps_keep_their_zone() {
    // Dropping the zone here is how an entire column silently shifts by hours later.
    let s = schema(vec![("at", LogicalType::TimestampUtc, false)]);
    let batch = encode_batch(&s, &[row(vec![Some("2025-01-01 00:00:00+00")], 1)]).expect("encodes");
    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .expect("timestamps");
    assert_eq!(
        column.value(0),
        1_735_689_600_000_000,
        "2025-01-01T00:00:00Z in microseconds"
    );

    let arrow_schema = batch.schema();
    let field = arrow_schema.field(0);
    assert!(
        format!("{:?}", field.data_type()).contains("UTC"),
        "the zone must survive encoding: {:?}",
        field.data_type()
    );
}

#[test]
fn dates_and_timestamps_agree_on_the_epoch() {
    let s = schema(vec![
        ("d", LogicalType::Date, false),
        ("t", LogicalType::TimestampUtc, false),
    ]);
    let batch = encode_batch(
        &s,
        &[row(
            vec![Some("1970-01-01"), Some("1970-01-01 00:00:00+00")],
            1,
        )],
    )
    .expect("encodes");
    let d = batch
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Date32Array>()
        .expect("dates");
    let t = batch
        .column(1)
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .expect("timestamps");
    assert_eq!(d.value(0), 0);
    assert_eq!(t.value(0), 0);
}

#[test]
fn a_zone_offset_is_applied_not_discarded() {
    // Regression: an earlier version stripped the offset and treated the wall time as
    // UTC, shifting an entire column by the offset. Every value stayed internally
    // consistent, so nothing looked wrong until it was compared against the source.
    let s = schema(vec![("at", LogicalType::TimestampUtc, false)]);
    let batch = encode_batch(
        &s,
        &[
            row(vec![Some("2025-01-01 00:00:00+00")], 1),
            row(vec![Some("2024-12-31 20:00:00-04")], 2), // the same instant
            row(vec![Some("2025-01-01 05:30:00+05:30")], 3), // and again
            row(vec![Some("2025-01-01T00:00:00Z")], 4),   // and again
        ],
    )
    .expect("encodes");

    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .expect("timestamps");

    let expected = 1_735_689_600_000_000i64; // 2025-01-01T00:00:00Z
    for i in 0..4 {
        assert_eq!(
            column.value(i),
            expected,
            "row {i} should be the same instant; the offset must be applied, not dropped"
        );
    }
}

#[test]
fn an_absent_offset_is_treated_as_the_value_it_states() {
    // An unzoned timestamp carries no offset to apply, so it must not be shifted.
    let s = schema(vec![("at", LogicalType::TimestampLocal, false)]);
    let batch = encode_batch(&s, &[row(vec![Some("2025-01-01 00:00:00")], 1)]).expect("encodes");
    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .expect("timestamps");
    assert_eq!(column.value(0), 1_735_689_600_000_000);
}

#[test]
fn an_implausible_offset_is_refused() {
    let s = schema(vec![("at", LogicalType::TimestampUtc, false)]);
    assert!(encode_batch(&s, &[row(vec![Some("2025-01-01 00:00:00+99")], 1)]).is_err());
}

#[test]
fn sub_second_precision_survives() {
    let s = schema(vec![("at", LogicalType::TimestampUtc, false)]);
    let batch =
        encode_batch(&s, &[row(vec![Some("2025-01-01 00:00:00.123456+00")], 1)]).expect("encodes");
    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .expect("timestamps");
    assert_eq!(column.value(0), 1_735_689_600_123_456);
}

#[test]
fn the_operation_is_recorded_per_row() {
    let s = schema(vec![("id", LogicalType::Int64, false)]);
    let mut insert = row(vec![Some("1")], 1);
    insert.op = Op::Insert;
    let mut update = row(vec![Some("2")], 2);
    update.op = Op::Update;
    let mut delete = row(vec![Some("3")], 3);
    delete.op = Op::Delete;

    let batch = encode_batch(&s, &[insert, update, delete]).expect("encodes");
    let ops = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("strings");
    assert_eq!((ops.value(0), ops.value(1), ops.value(2)), ("I", "U", "D"));
}

#[test]
fn integers_round_trip_at_the_boundaries() {
    let s = schema(vec![("v", LogicalType::Int64, false)]);
    let batch = encode_batch(
        &s,
        &[
            row(vec![Some(&i64::MAX.to_string())], 1),
            row(vec![Some(&i64::MIN.to_string())], 2),
        ],
    )
    .expect("encodes");
    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("ints");
    assert_eq!(column.value(0), i64::MAX);
    assert_eq!(column.value(1), i64::MIN);
}

proptest! {
    /// Any decimal the column can hold round-trips exactly.
    #[test]
    fn decimals_round_trip(units in -1_000_000_000_000i128..1_000_000_000_000, scale in 0u8..=6) {
        let s = schema(vec![("amount", dec(38, scale), false)]);
        // Render the value the way the source would.
        let divisor = 10i128.pow(u32::from(scale));
        let text = if scale == 0 {
            units.to_string()
        } else {
            let sign = if units < 0 { "-" } else { "" };
            let magnitude = units.unsigned_abs();
            format!("{sign}{}.{:0width$}", magnitude / divisor.unsigned_abs(),
                    magnitude % divisor.unsigned_abs(), width = usize::from(scale))
        };

        let batch = encode_batch(&s, &[row(vec![Some(&text)], 1)]);
        let Ok(batch) = batch else {
            prop_assert!(false, "{text} should encode at scale {scale}");
            return Ok(());
        };
        let column = batch.column(0).as_any().downcast_ref::<Decimal128Array>().expect("decimal");
        prop_assert_eq!(column.value(0), units, "round trip failed for {}", text);
    }

    /// Encoding never panics, whatever text arrives.
    #[test]
    fn encoding_never_panics(values in prop::collection::vec(".{0,40}", 0..8)) {
        let s = schema(vec![
            ("a", LogicalType::Int64, true),
            ("b", dec(18, 4), true),
            ("c", LogicalType::TimestampUtc, true),
        ]);
        let rows: Vec<Mutation> = values
            .iter()
            .map(|v| row(vec![Some(v.as_str()), Some(v.as_str()), Some(v.as_str())], 1))
            .collect();
        let _ = encode_batch(&s, &rows);
    }
}

#[test]
fn a_float_too_large_for_float32_is_refused_rather_than_encoded_as_infinity() {
    // `ING-07`, by the capture route. `str::parse::<f32>()` returns `Ok(inf)` for a value too
    // large to represent rather than an error, so a number the source sent as finite arrived
    // in the table as an infinity — silently, and after every other narrowing check had said
    // the value was fine.
    //
    // The feed path had the same defect at its own narrowing, and both are refusals now: an
    // infinity is not a large number, and a column declared `float32` reporting one is
    // reporting something the source never said.
    let s = schema(vec![("value", LogicalType::Float32, false)]);
    let refused = encode_batch(&s, &[row(vec![Some("1e308")], 100)])
        .expect_err("an infinity was encoded");
    // `Unparseable`, which is the right shape: the value does not parse *as a float32*. It
    // parses as an infinity, and an infinity is not what was written down.
    assert!(
        matches!(&refused, EncodeError::Unparseable { logical, value, .. }
            if logical.contains("float32") && value == "1e308"),
        "refused for the wrong reason: {refused:?}"
    );

    // A value that fits is still encoded, or the check has cost the column its range.
    encode_batch(&s, &[row(vec![Some("1.5")], 100)]).expect("an ordinary float was refused");

    // And an infinity the source *actually sent* is carried through rather than refused: it
    // is what the source said, and dropping it would be the reverse mistake.
    encode_batch(&s, &[row(vec![Some("inf")], 100)]).expect("an explicit infinity was refused");
}
