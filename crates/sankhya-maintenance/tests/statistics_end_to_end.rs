//! Compaction computes statistics; the provider prunes on them.
//!
//! Each half has been tested alone. This is the test that the halves meet — that what
//! compaction produces is what the provider can use, without a translation step in
//! between that nobody wrote.

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
use datafusion::prelude::{col, lit, SessionContext};
use sankhya_maintenance::{
    execute_tick, plan_tick, CompactionPolicy, DriverPolicy, FileStat, PartitionState, SystemState,
};
use sankhya_plan::{plan_splice, TierRef};
use sankhya_readpath::{LoggedFile, SankhyaTable};
use sankhya_stats::Bound;
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

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
            Arc::new(arrow_array::StringArray::from(labels)),
            Arc::new(UInt64Array::from(lsns)),
        ],
    )
    .expect("building")
}

fn quiet() -> SystemState {
    SystemState {
        in_maintenance_window: false,
        queries_running: 0,
        duty_cycle_ticks_remaining: 100_000,
    }
}

fn policy() -> DriverPolicy {
    DriverPolicy {
        compaction: CompactionPolicy {
            small_file_bytes: 1024 * 1024,
            routine_file_count: 4,
            ..CompactionPolicy::default()
        },
        ..DriverPolicy::default()
    }
}

#[tokio::test]
async fn compaction_produces_statistics_the_provider_can_prune_on() {
    let dir = tempfile::tempdir().expect("a temp dir");

    // Eight fragments, amounts 1..801.
    let files: Vec<FileStat> = (0..8i64)
        .map(|i| {
            let from = i * 100 + 1;
            let name = format!("part-{i:04}.parquet");
            let lsn = Lsn::new(u64::try_from(from + 99).expect("small"));
            let report = write_parquet(
                dir.path(),
                &name,
                &batch(from, from + 100),
                lsn,
                WriterConfig::default(),
            )
            .expect("writing");
            FileStat {
                name,
                bytes: report.bytes,
                rows: 100,
                covers_through: lsn,
            }
        })
        .collect();

    let plan = plan_tick(
        &[PartitionState {
            table: "sales.orders".to_string(),
            partition: "all".to_string(),
            files,
            ticks_since_write: 100,
        }],
        &policy(),
        &quiet(),
    );
    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let outcome = &report.merged[0];

    // The statistics came out of the merge, not out of a second pass.
    let amount = outcome
        .column_stats
        .get("amount")
        .expect("the merged file has statistics for amount");
    assert_eq!(amount.rows, outcome.rows);
    assert_eq!(amount.nulls, 0);
    assert_eq!(amount.min, Some(Bound::Int(1)));
    assert_eq!(
        amount.max,
        Some(Bound::Int(i64::try_from(outcome.rows).expect("small")))
    );

    // A string column is catalogued too, with lexicographic bounds.
    let label = outcome
        .column_stats
        .get("label")
        .expect("the merged file has statistics for label");
    assert_eq!(label.min, Some(Bound::Bytes(b"row-000001".to_vec())));
    assert!(label.distinct_estimate() > 0);

    // And the provider prunes on exactly those statistics, with no translation between.
    let merged = LoggedFile::new(
        outcome.output.to_str().expect("a utf-8 path").to_string(),
        outcome.bytes,
        outcome.rows,
    )
    .with_stats(outcome.column_stats.clone());

    let coverage = LsnRange::up_to(outcome.covers_through);
    let splice = plan_splice(
        &[TierRef::new("published", coverage)],
        outcome.covers_through,
    )
    .expect("a single tier");
    let provider = SankhyaTable::new(
        schema(),
        vec![merged],
        Vec::new(),
        outcome.covers_through,
        splice,
        true,
    );

    // Outside the merged file's range: provably irrelevant.
    assert_eq!(provider.prunable(&[col("amount").gt(lit(100_000i64))]), 1);
    // Inside it: must be read.
    assert_eq!(provider.prunable(&[col("amount").eq(lit(42i64))]), 0);

    // The answer is still right.
    let ctx = SessionContext::new();
    ctx.register_table("orders", Arc::new(provider))
        .expect("registering");
    let batches = ctx
        .sql("SELECT COUNT(*) c, SUM(amount) s FROM orders WHERE amount > 100000")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("executing");
    let b = &batches[0];
    assert_eq!(
        b.column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("count")
            .value(0),
        0
    );
}

#[tokio::test]
async fn statistics_survive_repeated_compaction() {
    // Compaction merges its own outputs over time. Statistics computed from a merged
    // file must still bound its contents -- a defect here would appear only on the
    // second pass, which is a place nobody looks.
    let dir = tempfile::tempdir().expect("a temp dir");

    let mut names = Vec::new();
    let mut live: Vec<FileStat> = (0..12i64)
        .map(|i| {
            let from = i * 50 + 1;
            let name = format!("part-{i:04}.parquet");
            let lsn = Lsn::new(u64::try_from(from + 49).expect("small"));
            let report = write_parquet(
                dir.path(),
                &name,
                &batch(from, from + 50),
                lsn,
                WriterConfig::default(),
            )
            .expect("writing");
            names.push(name.clone());
            FileStat {
                name,
                bytes: report.bytes,
                rows: 50,
                covers_through: lsn,
            }
        })
        .collect();

    let mut last_max = None;
    for tick in 1..=3u64 {
        let plan = plan_tick(
            &[PartitionState {
                table: "sales.orders".to_string(),
                partition: "all".to_string(),
                files: live.clone(),
                ticks_since_write: 100,
            }],
            &policy(),
            &quiet(),
        );
        if plan.run.is_empty() {
            break;
        }
        let report =
            execute_tick(&plan, dir.path(), tick, WriterConfig::default()).expect("ticking");
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let outcome = &report.merged[0];
        let amount = outcome.column_stats.get("amount").expect("statistics");

        // Whatever has been merged so far, the bounds still contain it.
        assert_eq!(amount.min, Some(Bound::Int(1)));
        assert!(amount.rows > 0);
        last_max = amount.max.clone();

        sankhya_maintenance::apply(&mut live, &report, dir.path());
    }

    assert!(last_max.is_some(), "no compaction ran");
}
