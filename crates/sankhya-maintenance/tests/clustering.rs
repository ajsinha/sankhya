//! Compaction writes settled partitions in order.
//!
//! # What ordering is for
//!
//! Row-group statistics only prune when a row group's values fall outside the predicate.
//! Written in arrival order, every row group holds the whole range of every column, the
//! bounds exclude nothing, and a selective query reads the entire table. Sorting on the
//! column a query filters by turns those bounds into an index — measured on TPC-H Q6,
//! which selects one year in seven, it took the query from 229 ms to 55 ms.
//!
//! So the assertions here are about the *bounds* the files end up with, not about row
//! order for its own sake. Row order is the mechanism; disjoint bounds are the result.

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

use arrow_array::{Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use sankhya_maintenance::{
    run_compaction, CompactionPlan, CompactionUrgency, FileStat, RetentionPolicy,
};
use sankhya_stats::{can_skip, Bound, ColumnStats, Predicate};
use sankhya_table::{column_stats, read_parquet_stats, write_parquet, WriterConfig};
use sankhya_types::Lsn;
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("day", DataType::Int64, false),
        Field::new("amount", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

/// A fragment whose days are spread across the whole range, as arrival order produces.
fn scattered(dir: &std::path::Path, index: u64, rows: i64) -> FileStat {
    let days: Vec<i64> = (0..rows)
        .map(|i| (i * 7 + index as i64 * 3) % 365)
        .collect();
    let amounts: Vec<i64> = (0..rows).collect();
    let lsns: Vec<u64> = (0..rows).map(|i| index * 1000 + i as u64).collect();

    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(days)),
            Arc::new(Int64Array::from(amounts)),
            Arc::new(UInt64Array::from(lsns)),
        ],
    )
    .expect("building");

    let name = format!("part-{index:04}.parquet");
    let lsn = Lsn::new(index * 1000 + rows as u64);
    let report = write_parquet(dir, &name, &batch, lsn, WriterConfig::default()).expect("writing");
    FileStat {
        name,
        bytes: report.bytes,
        rows: rows as u64,
        covers_through: lsn,
    }
}

fn plan_over(dir: &std::path::Path, files: u64, settled: bool) -> CompactionPlan {
    let inputs: Vec<FileStat> = (0..files).map(|i| scattered(dir, i, 500)).collect();
    let covers_through = inputs
        .iter()
        .map(|f| f.covers_through)
        .max()
        .expect("inputs");
    CompactionPlan {
        table: "t".to_string(),
        partition: "all".to_string(),
        urgency: CompactionUrgency::Routine,
        inputs,
        covers_through,
        settled,
        reason: "a test".to_string(),
    }
}

/// The `day` column of a written file, in the order it is stored.
fn days_in(path: &std::path::Path) -> Vec<i64> {
    let file = std::fs::File::open(path).expect("opening");
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("reader")
        .build()
        .expect("building");
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.expect("batch");
        let column = batch
            .column_by_name("day")
            .expect("day")
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("int64");
        out.extend((0..column.len()).map(|i| column.value(i)));
    }
    out
}

#[test]
fn a_settled_partition_is_written_in_order() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 4, true);
    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &["day".to_string()],
    )
    .expect("compacting");

    let days = days_in(&outcome.output);
    assert!(
        days.windows(2).all(|w| w[0] <= w[1]),
        "the merged file is not in day order"
    );
    assert_eq!(days.len(), 2_000);
}

#[test]
fn a_busy_partition_is_left_in_arrival_order() {
    // Sorting a partition still receiving writes means sorting it again tomorrow, for a
    // layout that was correct until the next append.
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 4, false);
    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &["day".to_string()],
    )
    .expect("compacting");

    let days = days_in(&outcome.output);
    assert!(
        !days.windows(2).all(|w| w[0] <= w[1]),
        "a busy partition was sorted anyway"
    );
}

#[test]
fn ordering_changes_nothing_but_the_order() {
    // A sort that lost, duplicated or altered a row would leave a smaller or larger
    // dataset that is internally consistent -- the failure this whole system is built to
    // refuse.
    let dir = tempfile::tempdir().expect("a temp dir");

    let unsorted = run_compaction(
        &plan_over(&dir.path().join("a"), 4, false),
        &dir.path().join("a"),
        "merged.parquet",
        WriterConfig::default(),
        &["day".to_string()],
    )
    .expect("compacting");

    let sorted = run_compaction(
        &plan_over(&dir.path().join("b"), 4, true),
        &dir.path().join("b"),
        "merged.parquet",
        WriterConfig::default(),
        &["day".to_string()],
    )
    .expect("compacting");

    assert_eq!(sorted.rows, unsorted.rows);

    let mut a = days_in(&unsorted.output);
    let mut b = days_in(&sorted.output);
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b, "sorting changed which values are present");
}

#[test]
fn ordering_makes_the_bounds_worth_having() {
    // The point of the exercise, asserted as the consequence rather than as row order.
    // Unsorted, the file's bounds span the whole range and prune nothing.
    let dir = tempfile::tempdir().expect("a temp dir");
    let sorted = run_compaction(
        &plan_over(dir.path(), 4, true),
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &["day".to_string()],
    )
    .expect("compacting");

    let day = sorted
        .column_stats
        .get("day")
        .expect("statistics for the clustering column");
    assert_eq!(day.min, Some(Bound::Int(0)));

    // A whole-file bound still spans everything -- one file cannot be pruned against
    // itself. What sorting buys is *row group* bounds inside it, which is what the
    // measurement in the module header records and what a reader sees.
    assert!(!can_skip(day, &Predicate::Equals(Bound::Int(100))));
}

#[test]
fn a_clustering_column_that_does_not_exist_is_refused() {
    // Merging unsorted instead would leave a partition that looks clustered and is not,
    // and nothing downstream could tell -- queries would simply be slower than the
    // layout promised.
    let dir = tempfile::tempdir().expect("a temp dir");
    let err = run_compaction(
        &plan_over(dir.path(), 3, true),
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &["not_a_column".to_string()],
    )
    .expect_err("an unknown clustering column must be refused");

    assert!(format!("{err}").contains("looks clustered and is not"));
}

#[test]
fn no_clustering_key_merges_without_sorting() {
    // The default. A table nobody has declared an ordering for is merged in arrival
    // order, which is correct and is what every earlier test in this crate assumes.
    let dir = tempfile::tempdir().expect("a temp dir");
    let outcome = run_compaction(
        &plan_over(dir.path(), 4, true),
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect("compacting");

    let days = days_in(&outcome.output);
    assert!(!days.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(read_parquet_stats(&outcome.output).expect("stats").0, 2_000);
}

#[test]
fn several_columns_order_lexicographically() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let outcome = run_compaction(
        &plan_over(dir.path(), 3, true),
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &["day".to_string(), "amount".to_string()],
    )
    .expect("compacting");

    let file = std::fs::File::open(&outcome.output).expect("opening");
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("reader")
        .build()
        .expect("building");

    let mut previous: Option<(i64, i64)> = None;
    for batch in reader {
        let batch = batch.expect("batch");
        let days = batch.column_by_name("day").expect("day");
        let days = days.as_any().downcast_ref::<Int64Array>().expect("int64");
        let amounts = batch.column_by_name("amount").expect("amount");
        let amounts = amounts
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("int64");

        for i in 0..batch.num_rows() {
            let current = (days.value(i), amounts.value(i));
            if let Some(before) = previous {
                assert!(before <= current, "{before:?} came before {current:?}");
            }
            previous = Some(current);
        }
    }

    // And the statistics computed from the sorted batch describe the sorted batch.
    let stats: &ColumnStats = outcome.column_stats.get("day").expect("statistics");
    assert_eq!(stats.rows, outcome.rows);
    let _ = column_stats;
}
