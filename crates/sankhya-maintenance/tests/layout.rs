//! Declared clustering: read from configuration, and applied when a partition settles.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_config::Configuration;
use sankhya_maintenance::{
    check_clustering, clustering, clustering_key, plan_compaction, run_compaction,
    CompactionPolicy, FileStat, PartitionState,
};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::Lsn;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

fn config_from(text: &str, dir: &Path) -> Configuration {
    let path = dir.join("application.yaml");
    std::fs::write(&path, text).expect("writing");
    Configuration::load_with(&[path], &BTreeMap::new(), &BTreeMap::new()).expect("loaded")
}

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, false),
    ]))
}

/// Rows whose regions are deliberately interleaved, so sorting is observable.
fn batch(from: i64, rows: usize) -> RecordBatch {
    const REGIONS: [&str; 4] = ["north", "south", "east", "west"];
    let ids: Vec<i64> = (0..rows as i64).map(|i| from + i).collect();
    let regions: Vec<&str> = ids
        .iter()
        .map(|i| REGIONS[(*i as usize) % REGIONS.len()])
        .collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(regions)),
        ],
    )
    .expect("well-formed")
}

// --- declaring it --------------------------------------------------------

#[test]
fn clustering_is_read_from_configuration_under_a_predictable_key() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config_from(
        "table:\n  sales:\n    orders:\n      clustering: region,order_date\n",
        dir.path(),
    );

    assert_eq!(clustering_key("sales", "orders"), "table.sales.orders.clustering");
    assert_eq!(
        clustering(&config, "sales", "orders"),
        vec!["region".to_string(), "order_date".to_string()],
        "most significant column first, as a SORT BY would take it"
    );
}

#[test]
fn a_table_that_declares_nothing_is_not_clustered() {
    // The honest default. An undeclared table is not clustered, rather than clustered on
    // whatever seemed reasonable — the engine cannot tell a meaningful query boundary from a
    // merely low-cardinality column, and guessing costs a sort at every compaction for ever.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config_from("table:\n  sales:\n    other:\n      clustering: x\n", dir.path());
    assert!(clustering(&config, "sales", "orders").is_empty());
}

#[test]
fn a_clustering_on_a_column_the_table_lacks_is_reported() {
    // Every compaction would fail, or — worse, if it were skipped — the table would report
    // itself clustered and not be, which nothing downstream can detect.
    let columns = vec!["id".to_string(), "region".to_string()];
    assert_eq!(check_clustering(&columns, &["region".to_string()]), Ok(()));
    assert_eq!(
        check_clustering(&columns, &["regoin".to_string(), "id".to_string()]),
        Err(vec!["regoin".to_string()]),
        "it names the one that is wrong, not the whole list"
    );
}

// --- applying it ---------------------------------------------------------

/// Compact `files` and return the region column of the result, in file order.
fn compact_and_read(dir: &Path, settled: bool, clustering: &[String]) -> Vec<String> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let mut stats = Vec::new();
    for (index, from) in [0_i64, 1_000].iter().enumerate() {
        let name = format!("part-{index:05}.parquet");
        let report = write_parquet(
            dir,
            &name,
            &batch(*from, 400),
            Lsn::new(index as u64 + 1),
            WriterConfig::default(),
        )
        .expect("written");
        stats.push(FileStat {
            name,
            bytes: report.bytes,
            rows: 400,
            covers_through: Lsn::new(index as u64 + 1),
        });
    }

    let state = PartitionState {
        table: "orders".to_string(),
        partition: String::new(),
        files: stats,
        ticks_since_write: if settled { 100 } else { 0 },
    };
    let policy = CompactionPolicy {
        small_file_bytes: 64 * 1024 * 1024,
        ..CompactionPolicy::default()
    };
    let plan = plan_compaction(&policy, &state).expect("worth compacting");
    let outcome = run_compaction(&plan, dir, "merged.parquet", WriterConfig::default(), clustering)
        .expect("compacted");

    let file = std::fs::File::open(&outcome.output).expect("the merged file");
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("readable")
        .build()
        .expect("built");
    let mut regions = Vec::new();
    for batch in reader {
        let batch = batch.expect("decoded");
        let column = batch
            .column_by_name("region")
            .expect("region")
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("a string column")
            .clone();
        for row in 0..column.len() {
            regions.push(column.value(row).to_string());
        }
    }
    regions
}

#[test]
fn a_settled_partition_is_sorted_by_its_declared_clustering() {
    // The property that makes clustering worth anything: row-group bounds only prune when
    // the rows within a file are ordered by the thing being filtered on. Ordering a table by
    // its date column took TPC-H Q6 from 229 ms to 55 ms.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let regions = compact_and_read(dir.path(), true, &["region".to_string()]);

    assert_eq!(regions.len(), 800);
    let mut sorted = regions.clone();
    sorted.sort();
    assert_eq!(regions, sorted, "the merged file is not ordered by region");
}

#[test]
fn a_partition_still_receiving_writes_is_merged_without_sorting() {
    // Ordering one that is still receiving writes produces a layout that was correct until
    // the next append, for the cost of a full sort on every pass.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let regions = compact_and_read(dir.path(), false, &["region".to_string()]);

    let mut sorted = regions.clone();
    sorted.sort();
    assert_ne!(regions, sorted, "an unsettled partition was sorted anyway");
}

#[test]
fn declaring_no_clustering_merges_without_sorting() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let regions = compact_and_read(dir.path(), true, &[]);
    let mut sorted = regions.clone();
    sorted.sort();
    assert_ne!(regions, sorted, "a table with no declared clustering was sorted");
}
