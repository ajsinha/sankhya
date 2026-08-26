//! Statistics survive a restart, and other engines can read them.
//!
//! The point of putting bounds in the table log rather than keeping them in this process
//! is that a query planned by a fresh process must prune exactly as one planned by a warm
//! one — and that a reader which is not SANKHYA can prune at all.

use arrow_array::{Int64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::{col, lit};
use sankhya_maintenance::{
    commit_tick, execute_tick, plan_tick, CompactionPolicy, DriverPolicy, FileStat, PartitionState,
    SystemState,
};
use sankhya_readpath::resolve;
use sankhya_stats::Bound;
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, live_files, Action, AddFile, Metadata};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

const DELTA_SCHEMA: &str = r#"{"type":"struct","fields":[{"name":"amount","type":"long","nullable":false,"metadata":{}},{"name":"label","type":"string","nullable":false,"metadata":{}},{"name":"_sankhya_commit_lsn","type":"long","nullable":false,"metadata":{}}]}"#;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("amount", DataType::Int64, false),
        Field::new("label", DataType::Utf8, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

fn batch(from: i64, to: i64) -> RecordBatch {
    let amounts: Vec<i64> = (from..to).collect();
    let labels: Vec<String> = amounts.iter().map(|a| format!("row-{a:06}")).collect();
    let lsns: Vec<u64> = amounts
        .iter()
        .map(|a| u64::try_from(*a).expect("small"))
        .collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(amounts)),
            Arc::new(StringArray::from(labels)),
            Arc::new(UInt64Array::from(lsns)),
        ],
    )
    .expect("building")
}

/// Publish eight fragments and compact them, committing everything.
fn build(root: &std::path::Path) -> u64 {
    commit(root, 0, &create(Metadata::new("orders", DELTA_SCHEMA, 0))).expect("creating");

    let mut adds = Vec::new();
    let mut files = Vec::new();
    for i in 0..8i64 {
        let from = i * 100 + 1;
        let name = format!("part-{i:04}.parquet");
        let lsn = Lsn::new(u64::try_from(from + 99).expect("small"));
        let report = write_parquet(
            root,
            &name,
            &batch(from, from + 100),
            lsn,
            WriterConfig::default(),
        )
        .expect("writing");
        adds.push(Action::Add(AddFile::with_rows(
            name.clone(),
            report.bytes,
            0,
            100,
        )));
        files.push(FileStat {
            name,
            bytes: report.bytes,
            rows: 100,
            covers_through: lsn,
        });
    }
    commit(root, 1, &adds).expect("publishing");

    let policy = DriverPolicy {
        compaction: CompactionPolicy {
            small_file_bytes: 1024 * 1024,
            routine_file_count: 4,
            ..CompactionPolicy::default()
        },
        ..DriverPolicy::default()
    };
    let plan = plan_tick(
        &[PartitionState {
            table: "orders".to_string(),
            partition: "all".to_string(),
            files,
            ticks_since_write: 100,
        }],
        &policy,
        &SystemState {
            in_maintenance_window: false,
            queries_running: 0,
            duty_cycle_ticks_remaining: 100_000,
        },
    );
    let report = execute_tick(&plan, root, 1, WriterConfig::default()).expect("ticking");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    commit_tick(root, 2, &report, 1).expect("committing");

    800
}

#[tokio::test]
async fn the_log_carries_the_bounds_compaction_computed() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let total = build(dir.path());

    // Read the log as a fresh process would.
    let live = live_files(dir.path()).expect("log");
    let merged = live
        .files
        .iter()
        .find(|f| f.path.starts_with("compacted-"))
        .expect("the merged file is live");

    let stats = merged.statistics().expect("the merge committed statistics");
    assert!(stats.num_records > 0);

    assert_eq!(
        stats.min_values.get("amount"),
        Some(&serde_json::Value::from(1i64))
    );
    assert_eq!(stats.null_count.get("amount"), Some(&0));
    // String bounds survive as strings, not as byte arrays.
    assert_eq!(
        stats.min_values.get("label"),
        Some(&serde_json::Value::from("row-000001"))
    );

    let _ = total;
}

#[tokio::test]
async fn a_fresh_process_prunes_exactly_as_a_warm_one_would() {
    // The whole reason for persisting them. Nothing here carries state from the
    // compaction: the provider is built from the table root alone.
    let dir = tempfile::tempdir().expect("a temp dir");
    let total = build(dir.path());

    let provider = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(total))),
        None,
        Lsn::new(total),
    )
    .expect("resolving");

    // Outside the merged file's range.
    assert!(
        provider.prunable(&[col("amount").gt(lit(100_000i64))]) > 0,
        "the bounds did not survive the restart"
    );
    // Inside it.
    assert_eq!(provider.prunable(&[col("amount").eq(lit(42i64))]), 0);
}

#[tokio::test]
async fn statistics_round_trip_through_the_log_unchanged() {
    let dir = tempfile::tempdir().expect("a temp dir");
    build(dir.path());

    let live = live_files(dir.path()).expect("log");
    let merged = live
        .files
        .iter()
        .find(|f| f.path.starts_with("compacted-"))
        .expect("merged");

    let recovered = sankhya_table_delta::to_column_stats(&merged.statistics().expect("statistics"));

    let amount = recovered.get("amount").expect("amount is catalogued");
    assert_eq!(amount.min, Some(Bound::Int(1)));
    assert_eq!(amount.nulls, 0);
    assert!(amount.rows > 0);

    // The cardinality sketch is not in the protocol and cannot come back. A caller must
    // not read the resulting zero as "no distinct values".
    assert_eq!(amount.distinct_estimate(), 0);
}

#[tokio::test]
async fn maintenance_checkpoints_a_log_once_it_has_grown() {
    // Checkpointing is a maintenance job rather than part of committing, because a
    // checkpoint holds exactly what replay produces: failing to write one is a missed
    // optimisation, and a commit that could fail for that reason would be worse than the
    // problem.
    use sankhya_maintenance::{checkpoint_if_due, CHECKPOINT_INTERVAL};
    use sankhya_table_delta::latest_checkpoint;

    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    let metadata = Metadata::new("orders", DELTA_SCHEMA, 0);
    commit(root, 0, &create(metadata.clone())).expect("creating");

    // Nothing due yet.
    assert!(checkpoint_if_due(root, &metadata, CHECKPOINT_INTERVAL)
        .expect("checking")
        .is_none());
    assert_eq!(latest_checkpoint(root), None);

    for v in 1..CHECKPOINT_INTERVAL {
        commit(
            root,
            v,
            &[Action::Add(AddFile::with_rows(
                format!("part-{v:04}.parquet"),
                100,
                0,
                50,
            ))],
        )
        .expect("committing");
    }
    assert!(
        checkpoint_if_due(root, &metadata, CHECKPOINT_INTERVAL)
            .expect("checking")
            .is_none(),
        "one commit short should not be due"
    );

    commit(
        root,
        CHECKPOINT_INTERVAL,
        &[Action::Add(AddFile::with_rows("last.parquet", 100, 0, 50))],
    )
    .expect("committing");

    let report = checkpoint_if_due(root, &metadata, CHECKPOINT_INTERVAL)
        .expect("checking")
        .expect("a checkpoint is due");
    assert_eq!(report.version, CHECKPOINT_INTERVAL);
    assert_eq!(latest_checkpoint(root), Some(CHECKPOINT_INTERVAL));

    // And immediately after, nothing is due again.
    assert!(checkpoint_if_due(root, &metadata, CHECKPOINT_INTERVAL)
        .expect("checking")
        .is_none());

    // The table reads the same either way.
    let live = live_files(root).expect("reading");
    assert_eq!(live.files.len() as u64, CHECKPOINT_INTERVAL);
    assert_eq!(live.version, Some(CHECKPOINT_INTERVAL));
}
