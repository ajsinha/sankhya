//! A real query, stopped.
//!
//! Every other cancellation test asserts a decision. This one asserts an *outcome*: a
//! query planned and executed by the engine, over real Parquet, that returns an error
//! instead of an answer because it ran out of time.

use arrow_array::{Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use datafusion::catalog::TableProvider;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::prelude::SessionContext;
use sankhya_governor::{Budget, Cancel, Deadline, Stopped};
use sankhya_readpath::{resolve, BudgetedExec, Clock};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use sankhya_types::{Lsn, LsnRange};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const DELTA_SCHEMA: &str = r#"{"type":"struct","fields":[{"name":"amount","type":"long","nullable":false,"metadata":{}},{"name":"_sankhya_commit_lsn","type":"long","nullable":false,"metadata":{}}]}"#;

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

/// A table of `files` fragments, enough that a scan produces several batches.
fn publish(root: &std::path::Path, files: u64, per: u64) -> u64 {
    commit(root, 0, &create(Metadata::new("t", DELTA_SCHEMA, 0))).expect("creating");
    let mut adds = Vec::new();
    for i in 0..files {
        let name = format!("part-{i:04}.parquet");
        let from = i * per;
        let report = write_parquet(
            root,
            &name,
            &rows(from, from + per),
            Lsn::new(from + per),
            WriterConfig::default(),
        )
        .expect("publishing");
        adds.push(Action::Add(AddFile::with_rows(name, report.bytes, 0, per)));
    }
    commit(root, 1, &adds).expect("publishing");
    files * per
}

/// A clock that advances one tick per reading, so "after three batches" is exact.
fn counting_clock() -> (Clock, Arc<AtomicU64>) {
    let ticks = Arc::new(AtomicU64::new(0));
    let handle = Arc::clone(&ticks);
    let clock: Clock = Arc::new(move || handle.fetch_add(1, Ordering::SeqCst));
    (clock, ticks)
}

async fn run(
    root: &std::path::Path,
    total: u64,
    budget: Budget,
    clock: Clock,
) -> Result<usize, String> {
    run_with_partitions(root, total, budget, clock, 1).await
}

async fn run_with_partitions(
    root: &std::path::Path,
    total: u64,
    budget: Budget,
    clock: Clock,
    partitions: usize,
) -> Result<usize, String> {
    let provider = resolve(
        schema(),
        root,
        Some(LsnRange::up_to(Lsn::new(total))),
        None,
        Lsn::new(total),
    )
    .expect("resolving");

    let ctx = SessionContext::new_with_config(
        datafusion::prelude::SessionConfig::new().with_target_partitions(partitions),
    );
    let state = ctx.state();
    let plan = provider
        .scan(&state, None, &[], None)
        .await
        .expect("planning");

    let budgeted: Arc<dyn ExecutionPlan> = Arc::new(BudgetedExec::new(plan, budget, clock));
    match datafusion::physical_plan::collect(budgeted, ctx.task_ctx()).await {
        Ok(batches) => Ok(batches.iter().map(arrow_array::RecordBatch::num_rows).sum()),
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::test]
async fn a_query_within_its_deadline_returns_its_answer() {
    // The node must be invisible when the budget holds. A cancellation mechanism that
    // costs correctness when it does not fire is worse than none.
    let dir = tempfile::tempdir().expect("a temp dir");
    let total = publish(dir.path(), 8, 500);
    let (clock, _) = counting_clock();

    let budget = Budget::new(Deadline::never(), Cancel::new(), 1);
    let rows = run(dir.path(), total, budget, clock)
        .await
        .expect("a query with no deadline must finish");

    assert_eq!(rows as u64, total);
}

#[tokio::test]
async fn a_query_that_runs_out_of_time_fails_rather_than_returning_less() {
    // The outcome that matters. Ending the stream quietly would hand the caller a
    // partial result that looks complete, which is the one thing worse than failing.
    let dir = tempfile::tempdir().expect("a temp dir");
    let total = publish(dir.path(), 8, 500);
    let (clock, _) = counting_clock();

    // The clock advances once per batch, so a deadline of two stops on the third.
    let budget = Budget::new(Deadline::at(2), Cancel::new(), 1);
    let error = run(dir.path(), total, budget, clock)
        .await
        .expect_err("the deadline must stop it");

    assert!(error.contains("too late to be useful"), "{error}");
}

#[tokio::test]
async fn a_cancelled_query_fails_and_says_retrying_is_pointless() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let total = publish(dir.path(), 8, 500);
    let (clock, _) = counting_clock();

    let cancel = Cancel::new();
    cancel.cancel();
    let budget = Budget::new(Deadline::never(), cancel, 1);

    let error = run(dir.path(), total, budget, clock)
        .await
        .expect_err("a cancelled query must stop");
    assert!(error.contains("cancelled"), "{error}");
    assert!(!Stopped::Cancelled.retryable());
}

#[tokio::test]
async fn the_query_stops_within_one_batch_of_its_deadline() {
    // The bound, stated as a number and then checked. Single-partition, because the
    // bound is *per partition* and this is about the per-partition part; the next test is
    // about how it scales.
    let dir = tempfile::tempdir().expect("a temp dir");
    let total = publish(dir.path(), 40, 200);
    let (clock, ticks) = counting_clock();

    const DEADLINE: u64 = 3;
    let budget = Budget::new(Deadline::at(DEADLINE), Cancel::new(), 1);
    let error = run(dir.path(), total, budget, clock)
        .await
        .expect_err("stopped");
    assert!(error.contains("too late"), "{error}");

    // One reading per batch that crossed the node, and it must have stopped on the first
    // reading at or past the deadline rather than several later.
    let readings = ticks.load(Ordering::SeqCst);
    assert!(
        readings <= DEADLINE + 1,
        "the clock was read {readings} times for a deadline of {DEADLINE}; the query \
         kept going past the point it should have stopped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bound_is_per_partition_and_scales_with_parallelism() {
    // Each partition runs its own stream and observes the deadline for itself, so a plan
    // with N partitions can have N batches in flight when it passes.
    //
    // "One batch" would be the tidier claim and it would be wrong. The node cannot
    // interrupt an operator that is mid-batch, and cannot make one partition stop
    // another: the shared token propagates a *cancellation*, but a deadline is a fact
    // each partition reads for itself.
    //
    // Worth asserting rather than assuming, because the number is proportional to
    // parallelism -- which is chosen for throughput, and this is what it costs on the
    // other side. It was also how the property was discovered: partitioning the scan
    // broke the test above, and the test was right to break.
    const PARTITIONS: usize = 8;
    const DEADLINE: u64 = 3;

    let dir = tempfile::tempdir().expect("a temp dir");
    let total = publish(dir.path(), 40, 200);
    let (clock, ticks) = counting_clock();

    let budget = Budget::new(Deadline::at(DEADLINE), Cancel::new(), 1);
    let error = run_with_partitions(dir.path(), total, budget, clock, PARTITIONS)
        .await
        .expect_err("stopped");
    assert!(error.contains("too late"), "{error}");

    let readings = ticks.load(Ordering::SeqCst) as usize;
    assert!(
        readings <= DEADLINE as usize + PARTITIONS + 1,
        "the clock was read {readings} times for a deadline of {DEADLINE} across \
         {PARTITIONS} partitions; the bound is one batch per partition, not more"
    );
    assert!(
        readings > DEADLINE as usize + 1,
        "only {readings} readings across {PARTITIONS} partitions; the plan did not run \
         in parallel, so this test is not measuring what it claims"
    );
}

#[tokio::test]
async fn a_deadline_that_has_not_passed_does_not_stop_anything() {
    // A deadline generous enough to finish must not stop the query on the last batch by
    // an off-by-one.
    let dir = tempfile::tempdir().expect("a temp dir");
    let total = publish(dir.path(), 4, 250);
    let (clock, _) = counting_clock();

    let budget = Budget::new(Deadline::at(10_000), Cancel::new(), 1);
    let rows = run(dir.path(), total, budget, clock)
        .await
        .expect("finishes");
    assert_eq!(rows as u64, total);
}
