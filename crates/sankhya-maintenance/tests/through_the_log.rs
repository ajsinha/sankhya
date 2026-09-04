//! The whole storage loop, with the log as the only source of truth.
//!
//! Files are published, compacted and committed; the reader is told nothing except
//! where the table root is. Nothing in this test passes a file list from the writer to
//! the reader, which is the point — that is the coupling the log removes.

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
use datafusion::prelude::SessionContext;
use sankhya_maintenance::{
    apply, commit_tick, execute_tick, plan_tick, CompactionPolicy, DriverPolicy, FileStat,
    PartitionState, SystemState,
};
use sankhya_readpath::{register_spliced, PublishedTier, TierSet};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, live_files, Action, AddFile, Metadata};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

const SCHEMA: &str = r#"{"type":"struct","fields":[{"name":"amount","type":"long","nullable":false,"metadata":{}}]}"#;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("amount", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

fn rows(from: u64, to: u64) -> RecordBatch {
    let lsns: Vec<u64> = (from + 1..=to).collect();
    let amounts: Vec<i64> = lsns
        .iter()
        .map(|l| i64::try_from(*l).expect("small"))
        .collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(amounts)),
            Arc::new(UInt64Array::from(lsns)),
        ],
    )
    .expect("building")
}

fn triangular(n: u64) -> i64 {
    i64::try_from(n * (n + 1) / 2).expect("small")
}

async fn measure(root: &std::path::Path, target: u64) -> (i64, i64) {
    // The reader is given the table root and nothing else.
    let tier =
        PublishedTier::from_log(root, LsnRange::up_to(Lsn::new(target))).expect("reading the log");

    let ctx = SessionContext::new();
    register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&tier), None),
        Lsn::new(target),
    )
    .await
    .expect("splicing");

    let batches = ctx
        .sql("SELECT COUNT(*) c, SUM(amount) s FROM orders")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("executing");
    let b = &batches[0];
    (
        b.column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("count")
            .value(0),
        b.column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("sum")
            .value(0),
    )
}

#[tokio::test]
async fn publish_compact_commit_query() {
    const FRAGMENTS: u64 = 24;
    const PER: u64 = 250;
    const TOTAL: u64 = FRAGMENTS * PER;

    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();

    // Create the table.
    commit(root, 0, &create(Metadata::new("orders", SCHEMA, 0))).expect("creating");

    // Publish, as continuous capture does: many small files, each committed.
    let mut live: Vec<FileStat> = Vec::new();
    let mut adds = Vec::new();
    for i in 0..FRAGMENTS {
        let name = format!("part-{i:04}.parquet");
        let from = i * PER;
        let report = write_parquet(
            root,
            &name,
            &rows(from, from + PER),
            Lsn::new(from + PER),
            WriterConfig::default(),
        )
        .expect("publishing");
        adds.push(Action::Add(AddFile::with_rows(
            name.clone(),
            report.bytes,
            0,
            PER,
        )));
        live.push(FileStat {
            name,
            bytes: report.bytes,
            rows: PER,
            covers_through: Lsn::new(from + PER),
        });
    }
    commit(root, 1, &adds).expect("publishing to the log");

    let before = measure(root, TOTAL).await;
    assert_eq!(
        before,
        (i64::try_from(TOTAL).expect("small"), triangular(TOTAL))
    );
    assert_eq!(live_files(root).expect("log").files.len(), 24);

    // Compact, tick by tick, committing each one.
    let policy = DriverPolicy {
        compaction: CompactionPolicy {
            small_file_bytes: 1024 * 1024,
            routine_file_count: 4,
            ..CompactionPolicy::default()
        },
        ..DriverPolicy::default()
    };
    let state = SystemState {
        in_maintenance_window: false,
        queries_running: 0,
        duty_cycle_ticks_remaining: 10_000,
    };

    let mut version = 1u64;
    let mut ticks = 0u64;
    loop {
        ticks += 1;
        assert!(ticks < 50, "the loop did not converge");

        // The driver's input is the log, not a listing and not a value threaded from
        // the writer.
        let logged = live_files(root).expect("log");
        let files: Vec<FileStat> = logged
            .files
            .iter()
            .map(|f| {
                let existing = live
                    .iter()
                    .find(|l| l.name == f.path)
                    .map_or(Lsn::new(TOTAL), |l| l.covers_through);
                FileStat {
                    name: f.path.clone(),
                    bytes: f.size,
                    // The log's own count. Falling back to zero would let a plan claim
                    // it was merging nothing and then merge everything, which the
                    // merge's own row-count check would catch -- loudly, but only after
                    // the work was done.
                    rows: f.rows().expect("every file in this log declares its rows"),
                    covers_through: existing,
                }
            })
            .collect();

        let partition = PartitionState {
            table: "orders".to_string(),
            partition: "all".to_string(),
            files,
            ticks_since_write: 100,
        };
        let plan = plan_tick(&[partition], &policy, &state);
        if plan.run.is_empty() {
            break;
        }

        let report = execute_tick(&plan, root, ticks, WriterConfig::default()).expect("ticking");
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        version += 1;
        commit_tick(root, version, &report, i64::try_from(ticks).expect("small"))
            .expect("committing the tick");
        apply(&mut live, &report, root);
    }

    // Fewer live files, same answer -- and every superseded file still on disk.
    let after_log = live_files(root).expect("log");
    assert!(
        after_log.files.len() < 24,
        "24 live files became {}",
        after_log.files.len()
    );
    assert_eq!(after_log.version, Some(version));

    let after = measure(root, TOTAL).await;
    assert_eq!(after, before, "compaction changed what the query returns");

    let on_disk = std::fs::read_dir(root)
        .expect("listing")
        .filter(|e| {
            e.as_ref()
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".parquet")
        })
        .count();
    assert!(
        on_disk > after_log.files.len(),
        "the superseded files should still be present: {on_disk} on disk, {} live",
        after_log.files.len()
    );
}

#[tokio::test]
async fn a_tick_commits_atomically() {
    // One version per tick, holding every add and every remove. Splitting them would
    // publish a state in which the same rows are live twice -- briefly, and briefly is
    // enough for a reader to see it.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    commit(root, 0, &create(Metadata::new("orders", SCHEMA, 0))).expect("creating");

    let mut adds = Vec::new();
    let mut files = Vec::new();
    for i in 0..8u64 {
        let name = format!("part-{i:04}.parquet");
        let report = write_parquet(
            root,
            &name,
            &rows(i * 100, i * 100 + 100),
            Lsn::new(i * 100 + 100),
            WriterConfig::default(),
        )
        .expect("publishing");
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
            covers_through: Lsn::new(i * 100 + 100),
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
            duty_cycle_ticks_remaining: 10_000,
        },
    );
    let report = execute_tick(&plan, root, 1, WriterConfig::default()).expect("ticking");
    let merged_inputs = report.merged[0].inputs_retained.len();

    commit_tick(root, 2, &report, 1).expect("committing");

    let log_dir = root.join("_delta_log");
    let versions = std::fs::read_dir(&log_dir).expect("listing").count();
    assert_eq!(versions, 3, "the tick must be exactly one new version");

    let live = live_files(root).expect("log");
    assert_eq!(live.files.len(), 8 - merged_inputs + 1);
    assert_eq!(live.version, Some(2));
}

#[tokio::test]
async fn a_tick_that_loses_the_version_race_is_refused() {
    // Two coordinators, one log. The loser must rebase, because its decisions were
    // exactly which files to merge and those were made against a state that no longer
    // exists.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    commit(root, 0, &create(Metadata::new("orders", SCHEMA, 0))).expect("creating");
    // A real action, because an action-less commit is refused: an empty body and a body
    // truncated to nothing are the same bytes, and replaying one as written drops every file
    // the missing lines named. What this test needs is only that version 1 is taken.
    commit(
        root,
        1,
        &[Action::Add(AddFile::new("someone-else.parquet", 1, 0))],
    )
    .expect("someone else's commit");

    let report = sankhya_maintenance::TickReport::default();
    let err = commit_tick(root, 1, &report, 0).expect_err("the version is taken");
    assert!(format!("{err}").contains("rebase"));
}

#[tokio::test]
async fn the_driver_publishes_a_compaction_as_a_rewrite_not_a_deletion() {
    // Asserted on what the driver actually commits, not on the constructor it could
    // have called. A removal marked as a data change tells a reader streaming changes
    // that every compacted row was deleted and re-inserted -- a flood of spurious
    // changes proportional to how well maintenance is working.
    //
    // This case exists because a mutation swapping the driver's `rewritten` for
    // `deleted` survived a test that checked only the two constructors in isolation.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    commit(root, 0, &create(Metadata::new("orders", SCHEMA, 0))).expect("creating");

    let mut adds = Vec::new();
    let mut files = Vec::new();
    for i in 0..8u64 {
        let name = format!("part-{i:04}.parquet");
        let report = write_parquet(
            root,
            &name,
            &rows(i * 100, i * 100 + 100),
            Lsn::new(i * 100 + 100),
            WriterConfig::default(),
        )
        .expect("publishing");
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
            covers_through: Lsn::new(i * 100 + 100),
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
            duty_cycle_ticks_remaining: 10_000,
        },
    );
    let report = execute_tick(&plan, root, 1, WriterConfig::default()).expect("ticking");
    commit_tick(root, 2, &report, 1).expect("committing");

    let actions = sankhya_table_delta::read_actions(root).expect("reading the log");
    let removals: Vec<_> = actions
        .iter()
        .filter_map(|(v, a)| match a {
            Action::Remove(r) if *v == 2 => Some(r),
            _ => None,
        })
        .collect();

    assert!(!removals.is_empty(), "the tick must have removed something");
    for removal in &removals {
        assert!(
            !removal.data_change,
            "{} was published as a deletion; compaction does not change rows",
            removal.path
        );
    }

    // And the add is not a spurious insertion either -- it genuinely adds a file, so
    // dataChange is true there and that is correct.
    let added: Vec<_> = actions
        .iter()
        .filter_map(|(v, a)| match a {
            Action::Add(f) if *v == 2 => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(added.len(), 1);
}
