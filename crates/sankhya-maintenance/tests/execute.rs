//! Compaction runs; retirement refuses to run early.
//!
//! The interesting assertions here are the negative ones. A compaction that merges
//! correctly but retires its inputs a moment too early is *worse* than one that never
//! compacts at all: it turns a performance problem into a query that fails on a
//! vanished file.

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

use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use sankhya_maintenance::{
    retire_inputs, run_compaction, CompactionPlan, CompactionUrgency, FileStat, RetentionPolicy,
};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::Lsn;
use std::collections::BTreeSet;
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]))
}

fn write_fragment(dir: &std::path::Path, name: &str, start: i64, rows: i64, lsn: u64) -> FileStat {
    let batch = RecordBatch::try_new(
        schema(),
        vec![Arc::new(Int64Array::from(
            (start..start + rows).collect::<Vec<_>>(),
        ))],
    )
    .expect("building a batch");
    let report =
        write_parquet(dir, name, &batch, Lsn::new(lsn), WriterConfig::default()).expect("writing");
    FileStat {
        name: name.to_string(),
        bytes: report.bytes,
        rows: u64::try_from(rows).expect("a sane row count"),
        covers_through: Lsn::new(lsn),
    }
}

fn plan_over(dir: &std::path::Path, count: i64) -> CompactionPlan {
    let inputs: Vec<FileStat> = (0..count)
        .map(|i| {
            write_fragment(
                dir,
                &format!("part-{i:03}.parquet"),
                i * 100,
                100,
                2000 + u64::try_from(i).expect("a sane index"),
            )
        })
        .collect();
    let covers_through = inputs
        .iter()
        .map(|f| f.covers_through)
        .max()
        .expect("at least one input");
    CompactionPlan {
        table: "sales.orders".to_string(),
        partition: "dt=2026-08-25".to_string(),
        urgency: CompactionUrgency::Routine,
        inputs,
        covers_through,
        settled: true,
        reason: "a test".to_string(),
    }
}

#[test]
fn a_plan_executes_and_leaves_its_inputs_alone() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 6);

    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect("running the plan");

    assert_eq!(outcome.rows, 600);
    assert_eq!(outcome.covers_through, plan.covers_through);
    for file in &plan.inputs {
        assert!(
            dir.path().join(&file.name).exists(),
            "{} was removed by the merge itself",
            file.name
        );
    }
}

#[test]
fn retirement_waits_out_the_grace_period() {
    // The case that matters. A reader that listed files a moment before the merge is
    // entitled to open them, and it has no way to tell us it is doing so.
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 4);
    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect("running the plan");

    let policy = RetentionPolicy {
        grace_ticks: 24,
        verify_replacement: true,
    };
    let retirement = retire_inputs(&outcome, &BTreeSet::new(), 23, &policy).expect("retiring");

    assert!(retirement.removed.is_empty());
    assert_eq!(retirement.retained.len(), 4);
    assert!(retirement.retained[0].1.contains("grace period"));
    for file in &plan.inputs {
        assert!(dir.path().join(&file.name).exists());
    }
}

#[test]
fn retirement_proceeds_once_the_grace_period_has_passed() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 4);
    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect("running the plan");

    let retirement = retire_inputs(&outcome, &BTreeSet::new(), 24, &RetentionPolicy::default())
        .expect("retiring");

    assert_eq!(retirement.removed.len(), 4);
    assert!(retirement.bytes_reclaimed > 0);
    for file in &plan.inputs {
        assert!(
            !dir.path().join(&file.name).exists(),
            "{} survived retirement",
            file.name
        );
    }
    // The replacement is untouched.
    assert!(outcome.output.exists());
}

#[test]
fn a_pinned_snapshot_keeps_its_files() {
    // Time travel and long-running sessions both pin a position. A file a pinned
    // snapshot may resolve to cannot be removed however old it is.
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 3);
    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect("running the plan");

    let mut referenced = BTreeSet::new();
    referenced.insert(Lsn::new(2001));

    let retirement = retire_inputs(&outcome, &referenced, 10_000, &RetentionPolicy::default())
        .expect("retiring");

    assert!(retirement.removed.is_empty());
    assert!(retirement.retained[0].1.contains("snapshot"));
    for file in &plan.inputs {
        assert!(dir.path().join(&file.name).exists());
    }
}

#[test]
fn a_snapshot_past_the_merge_does_not_block_retirement() {
    // Only snapshots that could resolve to the old files matter. One pinned beyond the
    // merged coverage reads the replacement.
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 3);
    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect("running the plan");

    let mut referenced = BTreeSet::new();
    referenced.insert(Lsn::new(9_999));

    let retirement = retire_inputs(&outcome, &referenced, 10_000, &RetentionPolicy::default())
        .expect("retiring");

    assert_eq!(retirement.removed.len(), 3);
}

#[test]
fn a_missing_replacement_stops_retirement_entirely() {
    // If the merge output has gone, removing its inputs would destroy the data. This is
    // the one case that is an error rather than a retained file.
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 3);
    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect("running the plan");

    std::fs::remove_file(&outcome.output).expect("removing the replacement");

    let err = retire_inputs(
        &outcome,
        &BTreeSet::new(),
        10_000,
        &RetentionPolicy::default(),
    )
    .expect_err("a missing replacement must stop retirement");

    assert!(format!("{err}").contains("could not be verified"));
    for file in &plan.inputs {
        assert!(
            dir.path().join(&file.name).exists(),
            "{} was removed despite a missing replacement",
            file.name
        );
    }
}

#[test]
fn a_truncated_replacement_stops_retirement_entirely() {
    // Verified, not assumed. The merge may have succeeded hours before retirement runs.
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_over(dir.path(), 3);
    let outcome = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect("running the plan");

    // Replace the output with a valid but shorter file.
    let short = RecordBatch::try_new(schema(), vec![Arc::new(Int64Array::from(vec![1i64, 2, 3]))])
        .expect("building");
    std::fs::remove_file(&outcome.output).expect("removing");
    write_parquet(
        dir.path(),
        "merged.parquet",
        &short,
        Lsn::new(2002),
        WriterConfig::default(),
    )
    .expect("writing a short replacement");

    let err = retire_inputs(
        &outcome,
        &BTreeSet::new(),
        10_000,
        &RetentionPolicy::default(),
    )
    .expect_err("a short replacement must stop retirement");

    assert!(format!("{err}").contains("holds 3 rows"));
    for file in &plan.inputs {
        assert!(dir.path().join(&file.name).exists());
    }
}

#[test]
fn a_stale_plan_is_reported_rather_than_absorbed() {
    // The plan is computed from a listing. If the files changed underneath it, the
    // merge is still correct but the scheduler was acting on stale statistics — worth
    // surfacing rather than silently accepting.
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut plan = plan_over(dir.path(), 3);
    plan.inputs[0].rows = 999_999;

    let err = run_compaction(
        &plan,
        dir.path(),
        "merged.parquet",
        WriterConfig::default(),
        &[],
    )
    .expect_err("a stale plan should be reported");

    assert!(format!("{err}").contains("expected"));
}
