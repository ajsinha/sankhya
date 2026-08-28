//! Where published files actually land.
//!
//! # The test that was missing
//!
//! `the_partition_path_is_what_an_external_engine_expects` asserted that
//! `DateAxis::partition_path` formats a string. It publishes nothing, so it held while every
//! table declared `partitionColumns: ["sank_data_date"]` and wrote every file flat with an
//! empty `partitionValues` — a column that existed in the metadata and in no location the
//! Delta protocol defines. An external engine reads that as null for every row.
//!
//! These tests assert against the **files and the log**, because that is what Spark and
//! Trino read, and a formatter cannot be wrong about it in a way they would notice.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Date32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_publish::{publish_table, Publication};
use sankhya_schema::{Granularity, DATA_DATE_COLUMN};
use std::path::Path;
use std::sync::Arc;

/// 2024-03-01 and 2024-03-02, days since the Unix epoch.
const FIRST_OF_MARCH: i32 = 19_783;
const SECOND_OF_MARCH: i32 = 19_784;
/// 2024-04-01, a different month and so a different partition at month granularity.
const FIRST_OF_APRIL: i32 = 19_814;

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, false),
        Field::new("order_date", DataType::Date32, false),
    ]))
}

fn batch(dates: Vec<i32>) -> RecordBatch {
    let n = dates.len() as i64;
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from((0..n).collect::<Vec<i64>>())),
            Arc::new(StringArray::from(
                (0..n).map(|i| format!("row-{i}")).collect::<Vec<String>>(),
            )),
            Arc::new(Date32Array::from(dates)),
        ],
    )
    .expect("well-formed")
}

/// Every file under a directory, relative to it.
fn files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, out);
        } else if let Ok(relative) = path.strip_prefix(base) {
            out.push(relative.display().to_string());
        }
    }
}

fn log_of(root: &Path, version: u64) -> String {
    std::fs::read_to_string(root.join(format!("_delta_log/{version:020}.json")))
        .unwrap_or_default()
}

// --- the layout an external engine reads --------------------------------

#[test]
fn a_published_file_lands_in_its_date_partition() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("orders");
    let publication = Publication::external(&root, "orders").dated_by("order_date");
    publish_table(&publication, &schema(), &[batch(vec![FIRST_OF_MARCH])]).expect("published");

    let on_disk = files(&root);
    assert!(
        on_disk
            .iter()
            .any(|f| f.starts_with("sank_data_date=2024-03-01/") && f.ends_with(".parquet")),
        "no file in a partition directory: {on_disk:?}"
    );
    assert!(
        !on_disk.iter().any(|f| f.starts_with("part-")),
        "a file was written flat at the table root: {on_disk:?}"
    );
}

#[test]
fn the_add_action_carries_the_partition_value() {
    // Declared in the metadata *and* supplied per file. A table declaring a partition column
    // whose files carry no value for it is malformed: the column reads null for every row in
    // an external engine, and nothing prunes.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("orders");
    let publication = Publication::external(&root, "orders").dated_by("order_date");
    publish_table(&publication, &schema(), &[batch(vec![FIRST_OF_MARCH])]).expect("published");

    let log = log_of(&root, 1);
    assert!(
        log.contains(r#""partitionValues":{"sank_data_date":"2024-03-01"}"#),
        "the add action has no partition value: {log}"
    );
    assert!(
        log.contains(r#""path":"sank_data_date=2024-03-01/part-00000.parquet""#),
        "the path is not relative to the table root: {log}"
    );

    // And the metadata still declares the column, so the two agree.
    assert!(log_of(&root, 0).contains(DATA_DATE_COLUMN));
}

#[test]
fn one_batch_of_two_dates_becomes_two_files_in_one_commit() {
    // Writing them to one file puts a date in a partition it does not belong to, and every
    // pruning query then reads the wrong set. Committing them separately lets a reader see
    // half a batch.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("orders");
    let publication = Publication::external(&root, "orders").dated_by("order_date");
    let published = publish_table(
        &publication,
        &schema(),
        &[batch(vec![FIRST_OF_MARCH, SECOND_OF_MARCH, FIRST_OF_MARCH])],
    )
    .expect("published");

    assert_eq!(published.len(), 2, "{published:#?}");
    let on_disk = files(&root);
    assert!(on_disk.iter().any(|f| f.starts_with("sank_data_date=2024-03-01/")));
    assert!(on_disk.iter().any(|f| f.starts_with("sank_data_date=2024-03-02/")));

    // One commit, both adds.
    let log = log_of(&root, 1);
    assert_eq!(log.matches("\"add\"").count(), 2, "{log}");
    assert!(log_of(&root, 2).is_empty(), "a second commit was written");
}

#[test]
fn rows_are_placed_in_the_partition_their_own_date_names() {
    // The property that makes pruning correct. Two rows of the first, one of the second.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("orders");
    let publication = Publication::external(&root, "orders").dated_by("order_date");
    let published = publish_table(
        &publication,
        &schema(),
        &[batch(vec![FIRST_OF_MARCH, SECOND_OF_MARCH, FIRST_OF_MARCH])],
    )
    .expect("published");

    let first = published
        .iter()
        .find(|p| p.file.contains("2024-03-01"))
        .expect("a partition for the first");
    let second = published
        .iter()
        .find(|p| p.file.contains("2024-03-02"))
        .expect("a partition for the second");
    assert_eq!(first.rows, 2);
    assert_eq!(second.rows, 1);
}

#[test]
fn a_coarser_granularity_gathers_the_days_of_a_month() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("orders");
    let publication = Publication::external(&root, "orders")
        .dated_by("order_date")
        .partitioned_by(Granularity::Month);
    let published = publish_table(
        &publication,
        &schema(),
        &[batch(vec![FIRST_OF_MARCH, SECOND_OF_MARCH, FIRST_OF_APRIL])],
    )
    .expect("published");

    assert_eq!(published.len(), 2, "March and April: {published:#?}");
    let on_disk = files(&root);
    assert!(
        on_disk.iter().any(|f| f.starts_with("sank_data_date=2024-03/")),
        "{on_disk:?}"
    );
    assert!(
        on_disk.iter().any(|f| f.starts_with("sank_data_date=2024-04/")),
        "{on_disk:?}"
    );
}

// --- refusals -----------------------------------------------------------

#[test]
fn a_null_date_is_refused_rather_than_filed_under_today() {
    // A per-row fallback makes the column mean "when it happened" in some rows and "when we
    // received it" in others, inseparably and for ever.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("orders");
    let publication = Publication::external(&root, "orders").dated_by("order_date");

    let nullable = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, false),
        Field::new("order_date", DataType::Date32, true),
    ]));
    let with_null = RecordBatch::try_new(
        Arc::clone(&nullable),
        vec![
            Arc::new(Int64Array::from(vec![0_i64, 1])),
            Arc::new(StringArray::from(vec!["a", "b"])),
            Arc::new(Date32Array::from(vec![Some(FIRST_OF_MARCH), None])),
        ],
    )
    .expect("well-formed");

    let refused = publish_table(&publication, &nullable, &[with_null])
        .expect_err("filed a row with no date");
    assert!(
        refused.to_string().contains("row 1"),
        "the refusal names the row: {refused}"
    );
}

#[test]
fn an_ingest_dated_table_still_partitions() {
    // The axis that says out loud it means arrival. Every row of a batch arrived together,
    // so they share one partition — but there is still a partition, and still a value.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("arrivals");
    let publication = Publication::external(&root, "arrivals");
    publish_table(&publication, &schema(), &[batch(vec![FIRST_OF_MARCH])]).expect("published");

    let on_disk = files(&root);
    assert!(
        on_disk
            .iter()
            .any(|f| f.starts_with(&format!("{DATA_DATE_COLUMN}="))),
        "an ingest-dated table wrote flat: {on_disk:?}"
    );
    assert!(log_of(&root, 1).contains(r#""sank_data_date":"#), "{}", log_of(&root, 1));
}
