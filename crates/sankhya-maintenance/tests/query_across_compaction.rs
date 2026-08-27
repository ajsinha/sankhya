//! A query answered before, during and after a compaction returns the same thing.
//!
//! This is the join between the two halves of the system: maintenance rewrites the
//! files underneath, and the read path must not notice. It is also where the cost of
//! getting the file set wrong becomes visible as a wrong number rather than as an
//! abstract argument.

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
use datafusion::prelude::{ParquetReadOptions, SessionContext};
use sankhya_maintenance::{
    apply, execute_tick, plan_tick, CompactionPolicy, DriverPolicy, FileStat, PartitionState,
    SystemState,
};
use sankhya_readpath::{register_spliced, PublishedTier, TierSet};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

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

async fn measure(ctx: &SessionContext, name: &str) -> (i64, i64) {
    let batches = ctx
        .sql(&format!("SELECT COUNT(*) c, SUM(amount) s FROM {name}"))
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

fn path_of(dir: &std::path::Path, name: &str) -> String {
    dir.join(name).to_str().expect("a utf-8 path").to_string()
}

#[tokio::test]
async fn a_query_is_unaffected_by_compaction_running_underneath_it() {
    const FRAGMENTS: u64 = 20;
    const PER: u64 = 100;
    const TOTAL: u64 = FRAGMENTS * PER;

    let dir = tempfile::tempdir().expect("a temp dir");

    // Twenty fragments, as continuous capture leaves them.
    let mut live: Vec<FileStat> = (0..FRAGMENTS)
        .map(|i| {
            let name = format!("part-{i:04}.parquet");
            let from = i * PER;
            let report = write_parquet(
                dir.path(),
                &name,
                &rows(from, from + PER),
                Lsn::new(from + PER),
                WriterConfig::default(),
            )
            .expect("writing");
            FileStat {
                name,
                bytes: report.bytes,
                rows: PER,
                covers_through: Lsn::new(from + PER),
            }
        })
        .collect();

    let tier_from = |live: &[FileStat]| {
        PublishedTier::new(
            live.iter().map(|f| path_of(dir.path(), &f.name)).collect(),
            LsnRange::up_to(Lsn::new(TOTAL)),
        )
    };

    // Before.
    let before_tier = tier_from(&live);
    let ctx = SessionContext::new();
    register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&before_tier), None),
        Lsn::new(TOTAL),
    )
    .await
    .expect("splicing");
    let before = measure(&ctx, "orders").await;
    assert_eq!(
        before,
        (i64::try_from(TOTAL).expect("small"), triangular(TOTAL))
    );

    // Compact.
    let policy = DriverPolicy {
        compaction: CompactionPolicy {
            small_file_bytes: 1024 * 1024,
            routine_file_count: 4,
            ..CompactionPolicy::default()
        },
        ..DriverPolicy::default()
    };
    let partition = PartitionState {
        table: "sales.orders".to_string(),
        partition: "all".to_string(),
        files: live.clone(),
        ticks_since_write: 100,
    };
    let plan = plan_tick(
        &[partition],
        &policy,
        &SystemState {
            in_maintenance_window: false,
            queries_running: 0,
            duty_cycle_ticks_remaining: 10_000,
        },
    );
    assert!(!plan.run.is_empty(), "the fixture must actually compact");

    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    apply(&mut live, &report);

    // After. Fewer files, same answer -- and the inputs are still on disk.
    assert!(live.len() < usize::try_from(FRAGMENTS).expect("small"));
    let after_tier = tier_from(&live);
    let ctx = SessionContext::new();
    register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&after_tier), None),
        Lsn::new(TOTAL),
    )
    .await
    .expect("splicing");
    let after = measure(&ctx, "orders").await;

    assert_eq!(after, before, "compaction changed what the query returns");
}

#[tokio::test]
async fn reading_the_directory_instead_of_the_live_set_gives_a_wrong_answer() {
    // The failure this design prevents, produced deliberately so the cost is a number
    // rather than an argument. Nothing is wrong on disk: compaction only ever adds, and
    // the inputs are meant to still be there. It is the *reader* that is wrong.
    let dir = tempfile::tempdir().expect("a temp dir");
    const FRAGMENTS: u64 = 8;
    const PER: u64 = 100;
    const TOTAL: u64 = FRAGMENTS * PER;

    let mut live: Vec<FileStat> = (0..FRAGMENTS)
        .map(|i| {
            let name = format!("part-{i:04}.parquet");
            let from = i * PER;
            let report = write_parquet(
                dir.path(),
                &name,
                &rows(from, from + PER),
                Lsn::new(from + PER),
                WriterConfig::default(),
            )
            .expect("writing");
            FileStat {
                name,
                bytes: report.bytes,
                rows: PER,
                covers_through: Lsn::new(from + PER),
            }
        })
        .collect();

    let policy = DriverPolicy {
        compaction: CompactionPolicy {
            small_file_bytes: 1024 * 1024,
            routine_file_count: 4,
            ..CompactionPolicy::default()
        },
        ..DriverPolicy::default()
    };
    let partition = PartitionState {
        table: "sales.orders".to_string(),
        partition: "all".to_string(),
        files: live.clone(),
        ticks_since_write: 100,
    };
    let plan = plan_tick(
        &[partition],
        &policy,
        &SystemState {
            in_maintenance_window: false,
            queries_running: 0,
            duty_cycle_ticks_remaining: 10_000,
        },
    );
    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");
    let merged_rows: u64 = report.merged.iter().map(|o| o.rows).sum();
    apply(&mut live, &report);

    // The live set is right.
    let live_total: u64 = live.iter().map(|f| f.rows).sum();
    assert_eq!(live_total, TOTAL);

    // The directory is not.
    let ctx = SessionContext::new();
    ctx.register_parquet(
        "listed",
        dir.path().to_str().expect("a utf-8 path"),
        ParquetReadOptions::default(),
    )
    .await
    .expect("registering the directory");
    let (count, _) = measure(&ctx, "listed").await;

    assert_eq!(
        u64::try_from(count).expect("small"),
        TOTAL + merged_rows,
        "reading the directory must count the merged rows twice, or this test is not \
         demonstrating the hazard it claims to"
    );
}
