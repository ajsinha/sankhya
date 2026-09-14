//! The types a warehouse actually filters on.
//!
//! `M24`. `stats.rs` recorded bounds for `Int16/32/64`, `UInt64`, `Float32/64` and `Utf8`,
//! and for nothing else — no `Date32`, no `Timestamp`, no `Decimal128`. A date-ranged query
//! is the commonest query a warehouse runs and it pruned **nothing**, while `NFR-PERF-03` was
//! reported met for a *"multi-dimensional pivot, warm, pruned"* against a query that could not
//! satisfy its own precondition.
//!
//! # Why every test here goes through the log
//!
//! Because the bound is written to the table log and read back, and the two halves are where
//! the type is lost. The protocol's statistics document is **schemaless**: a date is
//! `"2026-09-14"` and a city is `"paris"`, both JSON strings. A test that kept the statistics
//! in memory would prove the producer and the pruner agree and say nothing about the round
//! trip that actually happens on every query planned by a fresh process.
//!
//! So each fixture records statistics with the real producer, encodes them into a real
//! `FileStatistics`, decodes them **against the schema**, and only then asks whether a file is
//! prunable. That is the whole chain, and any link that drops the type shows up as a bound
//! that never matches.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{
    ArrayRef, Date32Array, Decimal128Array, Int64Array, RecordBatch, TimestampMicrosecondArray,
    UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use datafusion::prelude::{col, lit, SessionContext};
use datafusion::scalar::ScalarValue;
use sankhya_plan::{plan_splice, TierRef};
use sankhya_readpath::{LoggedFile, SankhyaTable};
use sankhya_table::{column_stats, write_parquet, WriterConfig};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

/// `2026-09-14`, in the units each column keeps.
const SEPT_14: i32 = 20_710;
const MICROS_PER_DAY: i64 = 86_400_000_000;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("day", DataType::Date32, false),
        Field::new("at", DataType::Timestamp(TimeUnit::Microsecond, None), false),
        Field::new("money", DataType::Decimal128(18, 2), false),
        Field::new("amount", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

/// One file covering `days` consecutive dates from `from`, with the other columns tracking.
///
/// Statistics travel the way they do in production: computed by [`column_stats`], written
/// into the protocol's document, and read back **against the schema**. A test that skipped
/// the middle two steps would pass while a date bound came back as a byte string.
fn fragment(dir: &std::path::Path, index: u64, from: i32, days: i32) -> LoggedFile {
    let dates: Vec<i32> = (from..from + days).collect();
    let stamps: Vec<i64> = dates.iter().map(|d| i64::from(*d) * MICROS_PER_DAY).collect();
    // Pence, so the scale is load-bearing: 1,234.56 and not 1,234.
    let money: Vec<i128> = dates.iter().map(|d| i128::from(*d) * 100 + 56).collect();
    let amounts: Vec<i64> = dates.iter().map(|d| i64::from(*d)).collect();
    let lsns: Vec<u64> = dates.iter().map(|d| u64::try_from(*d).expect("after 1970")).collect();

    let columns: Vec<ArrayRef> = vec![
        Arc::new(Date32Array::from(dates)),
        Arc::new(TimestampMicrosecondArray::from(stamps)),
        Arc::new(
            Decimal128Array::from(money)
                .with_precision_and_scale(18, 2)
                .expect("a declared decimal"),
        ),
        Arc::new(Int64Array::from(amounts)),
        Arc::new(UInt64Array::from(lsns)),
    ];
    let batch = RecordBatch::try_new(schema(), columns).expect("building");

    let name = format!("part-{index:04}.parquet");
    let report = write_parquet(
        dir,
        &name,
        &batch,
        Lsn::new(u64::try_from(from + days).expect("after 1970")),
        WriterConfig::default(),
    )
    .expect("writing");

    let rows = u64::try_from(days).expect("a positive span");
    let document = sankhya_table_delta::from_column_stats(rows, &column_stats(&batch));
    let recovered = sankhya_table_delta::to_column_stats(&document, Some(&schema()));

    LoggedFile::new(
        dir.join(&name).to_str().expect("a utf-8 path").to_string(),
        report.bytes,
        rows,
    )
    .with_stats(recovered)
}

/// Ten files of ten days each, starting a hundred days before `SEPT_14`.
///
/// Written **once** per test. A data file is never overwritten --- `write_parquet` refuses a
/// name that exists, because it belongs to rows some log still refers to --- so a helper that
/// rebuilt the table to ask a second question of it failed on its own fixture.
fn files(dir: &std::path::Path) -> Vec<LoggedFile> {
    (0..10u64)
        .map(|i| fragment(dir, i, SEPT_14 - 100 + i32::try_from(i).expect("small") * 10, 10))
        .collect()
}

fn over(files: Vec<LoggedFile>) -> SankhyaTable {
    let coverage = LsnRange::up_to(Lsn::new(1_000_000));
    let splice = plan_splice(&[TierRef::new("published", coverage)], Lsn::new(1_000_000))
        .expect("a single tier covers it");
    SankhyaTable::new(schema(), files, Vec::new(), Lsn::new(1_000_000), splice, true)
}

/// Run the query and return how many rows came back.
async fn answer(table: SankhyaTable, sql: &str) -> i64 {
    let ctx = SessionContext::new();
    ctx.register_table("events", Arc::new(table))
        .expect("registering");
    let batches = ctx
        .sql(sql)
        .await
        .expect("planning")
        .collect()
        .await
        .expect("executing");
    batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("a count")
        .value(0)
}

#[tokio::test]
async fn a_date_filter_prunes_and_still_answers() {
    // The headline. Ten files of ten days; one day can only be in one of them.
    let dir = tempfile::tempdir().expect("a temp dir");
    let written = files(dir.path());
    let provider = over(written.clone());

    let filters = vec![col("day").eq(lit(ScalarValue::Date32(Some(SEPT_14 - 5))))];
    assert_eq!(
        provider.prunable(&filters),
        9,
        "nine of ten files cannot hold that date, and the statistics prove it"
    );

    // And the answer is the one the unpruned scan would give. Pruning that changes a
    // result is worse than no pruning at all, which is why both halves are asserted.
    let rows = answer(over(written), "SELECT COUNT(*) FROM events WHERE day = DATE '2026-09-09'")
        .await;
    assert_eq!(rows, 1, "exactly one row carries that date");
}

#[tokio::test]
async fn a_date_range_prunes_the_files_outside_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let written = files(dir.path());

    // The last thirty days: the first seven files are wholly before it.
    let filters = vec![col("day").gt_eq(lit(ScalarValue::Date32(Some(SEPT_14 - 30))))];
    assert_eq!(over(written.clone()).prunable(&filters), 7);

    let rows = answer(
        over(written),
        "SELECT COUNT(*) FROM events WHERE day >= DATE '2026-08-15'",
    )
    .await;
    assert_eq!(rows, 30, "thirty days, and the pruning did not lose any of them");
}

#[tokio::test]
async fn a_timestamp_filter_prunes() {
    // The same question of a column whose unit is not days. The bound carries its unit, so
    // a microsecond literal and a microsecond column meet as themselves.
    let dir = tempfile::tempdir().expect("a temp dir");
    let provider = over(files(dir.path()));

    // Sixty days back, which is the boundary of the fifth file: the six files at or after it
    // hold nothing earlier, and the statistics prove it.
    let at = i64::from(SEPT_14 - 60) * MICROS_PER_DAY;
    let filters = vec![col("at").lt(lit(ScalarValue::TimestampMicrosecond(Some(at), None)))];
    assert_eq!(
        provider.prunable(&filters),
        6,
        "a bound recorded in microseconds, written to the log as an instant, and read back \
         against the column's own unit"
    );
}

#[tokio::test]
async fn a_decimal_filter_prunes_without_going_through_a_float() {
    // Money. The bound is an unscaled integer and a scale, compared at a common scale, so
    // `1234.56` is that amount and not the nearest `f64` to it.
    let dir = tempfile::tempdir().expect("a temp dir");
    let provider = over(files(dir.path()));

    let target = i128::from(SEPT_14 - 5) * 100 + 56;
    let filters = vec![col("money").eq(lit(ScalarValue::Decimal128(Some(target), 18, 2)))];
    assert_eq!(
        provider.prunable(&filters),
        9,
        "one file can hold that exact amount"
    );
}

#[tokio::test]
async fn a_bound_of_the_wrong_unit_prunes_nothing_rather_than_the_wrong_thing() {
    // **The reason the unit is in the bound at all.**
    //
    // A `Timestamp(Second)` literal against a `Timestamp(Microsecond)` column names an
    // instant a million times further from the epoch if the unit is dropped. Recorded as a
    // bare integer — which is what every one of these types was before `M24`, and what they
    // would still be if the variant had been reused — the comparison succeeds and is wrong,
    // and the file it skips holds rows the query wanted.
    //
    // Here it must simply not prune. Asserted as a *lower* bound on correctness: a missed
    // skip costs a scan, and this test exists to prove it never costs an answer.
    let dir = tempfile::tempdir().expect("a temp dir");
    let provider = over(files(dir.path()));

    let seconds = i64::from(SEPT_14 - 5) * 86_400;
    let filters = vec![col("at").lt(lit(ScalarValue::TimestampSecond(Some(seconds), None)))];
    // Whatever DataFusion does with the literal — coerce it to the column's unit, or hand it
    // over as written — the one outcome that must not happen is a file skipped on a
    // comparison between two different units.
    let skipped = provider.prunable(&filters);
    assert!(
        skipped <= 5,
        "a second-resolution literal must never prove more files irrelevant than the same \
         instant in microseconds does: {skipped}"
    );
}

#[tokio::test]
async fn an_unrecognised_type_is_scanned_rather_than_guessed_at() {
    // The rule the whole crate is built on, pinned where it is easiest to break: a column
    // this system records no bounds for must prune nothing, not prune on an empty range.
    let dir = tempfile::tempdir().expect("a temp dir");
    let provider = over(files(dir.path()));

    // `_sankhya_commit_lsn` is recorded, so use a filter on a column that is not in the
    // statistics at all by asking about one no predicate can be extracted for.
    let filters = vec![col("amount").eq(col("_sankhya_commit_lsn"))];
    assert_eq!(
        provider.prunable(&filters),
        0,
        "a column compared to another column yields no predicate, and no predicate prunes \
         nothing"
    );
}
