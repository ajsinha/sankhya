//! A partition predicate, and whether naming one does anything.
//!
//! `NFR-PERF-03` says *multi-dimensional pivot, warm, **pruned***, with a stated precondition
//! that a partition predicate be present — and every document in this repository has said,
//! correctly, that no query could satisfy it. The reason given was that the read path prunes
//! by file statistics rather than by partition value, and that partition values are dropped on
//! the way into the scan.
//!
//! That reason was complete only while the partition column had no statistics. It is a
//! `Date32`, stamped into the data by `sankhya-publish` so an external reader sees it
//! natively — and `Date32` was one of the types `stats.rs` did not recognise. So the column
//! existed, the predicate parsed, and the bound that would have made it prune was never
//! recorded.
//!
//! `M24a` records it. These tests exist to find out what that changed, because the answer
//! decides whether `M24b` is plumbing or a correction, and a claim about pruning that nobody
//! measured is how the objective got into this state in the first place.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Date32Array, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use datafusion::prelude::{col, lit};
use datafusion::scalar::ScalarValue;
use sankhya_publish::{publish_table, Publication};
use sankhya_readpath::resolve;
use sankhya_schema::DATA_DATE_COLUMN;
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

/// 2024-03-01 and the four days after it.
const FIRST_OF_MARCH: i32 = 19_783;

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("order_date", DataType::Date32, false),
    ]))
}

fn batch(day: i32, from: i64) -> RecordBatch {
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from((from..from + 10).collect::<Vec<i64>>())),
            Arc::new(Date32Array::from(vec![day; 10])),
        ],
    )
    .expect("well-formed")
}

/// Five days, one partition each, published through the product's own writer.
fn published(root: &std::path::Path) -> sankhya_readpath::SankhyaTable {
    let publication = Publication::external(root, "orders").dated_by("order_date");
    let batches: Vec<RecordBatch> = (0..5)
        .map(|i| batch(FIRST_OF_MARCH + i, i64::from(i) * 10))
        .collect();
    publish_table(&publication, &schema(), &batches).expect("published");

    // The schema as written, which is the declared one plus the stamped date column.
    let live = sankhya_table_delta::live_files(root).expect("the log");
    let first = root.join(&live.files[0].path);
    let file = std::fs::File::open(&first).expect("opening");
    let written = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("reader")
        .schema()
        .clone();

    let target = Lsn::new(1_000);
    resolve(written, root, Some(LsnRange::up_to(target)), None, target).expect("resolving")
}

#[test]
fn a_partition_predicate_prunes() {
    // **The objective's precondition, measured rather than asserted about.**
    //
    // Five daily partitions, one file each. A predicate naming the partition column on one
    // day must prove the other four irrelevant — and this is the strongest case pruning has,
    // because every row in a partition carries the same date, so the file's bounds are a
    // single point.
    //
    // Before `M24a` the answer here was **zero**: `sank_data_date` is a `Date32` and
    // `stats.rs` recorded no bounds for that type, so the column was stamped, declared,
    // supplied per file in the log — and pruned nothing.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let table = published(&dir.path().join("orders"));

    let filters = vec![col(DATA_DATE_COLUMN).eq(lit(ScalarValue::Date32(Some(FIRST_OF_MARCH))))];
    assert_eq!(
        table.prunable(&filters),
        4,
        "four of five partitions cannot hold that date, and the catalogue proves it without \
         opening a footer"
    );
}

#[test]
fn a_partition_range_prunes_what_falls_outside_it() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let table = published(&dir.path().join("orders"));

    let filters =
        vec![col(DATA_DATE_COLUMN).gt_eq(lit(ScalarValue::Date32(Some(FIRST_OF_MARCH + 3))))];
    assert_eq!(table.prunable(&filters), 3, "the first three days are wholly before it");
}

#[test]
fn the_ordinary_date_column_prunes_too_and_the_two_agree() {
    // `order_date` is the column the user wrote; `sank_data_date` is the one the partitioning
    // derived from it. Over this data they hold the same value in every row, so a predicate
    // on either must prove the same files irrelevant.
    //
    // They are different mechanisms in the sense that matters to the objective — one is a
    // partition column and one is not — and identical in the sense that matters to a query:
    // both are bounds in the catalogue, and neither opens a file to be used.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let table = published(&dir.path().join("orders"));

    let by_partition =
        vec![col(DATA_DATE_COLUMN).eq(lit(ScalarValue::Date32(Some(FIRST_OF_MARCH + 2))))];
    let by_column = vec![col("order_date").eq(lit(ScalarValue::Date32(Some(FIRST_OF_MARCH + 2))))];
    assert_eq!(table.prunable(&by_partition), 4);
    assert_eq!(table.prunable(&by_column), 4);
}
