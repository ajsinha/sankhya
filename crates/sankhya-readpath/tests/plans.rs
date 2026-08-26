//! The shape of the plans this system produces.
//!
//! # Why the shape and not the text
//!
//! A byte-exact plan snapshot fails on every cosmetic change upstream — a renamed metric,
//! a reordered attribute — and the reflex is to regenerate it without reading. After the
//! third regeneration nobody is reading them at all, and the snapshot that was meant to
//! catch a lost optimisation catches nothing.
//!
//! So what is pinned is the sequence of operators. That is stable across cosmetic change
//! and moves exactly when the plan actually changes, which is the event worth a failing
//! test.
//!
//! # Why properties are asserted as well
//!
//! A shape says what the plan is; it does not say whether the plan is any good. The
//! assertions below are about the optimisations this system depends on and that fail
//! *silently* when they stop working — a query with pruning switched off is not wrong,
//! only slower, and slower by an amount nobody notices until the table is large.

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
use datafusion::catalog::TableProvider;
use datafusion::physical_plan::{displayable, ExecutionPlan};
use datafusion::prelude::{col, lit, SessionContext};
use sankhya_plan::{plan_splice, TierRef};
use sankhya_readpath::{LoggedFile, SankhyaTable};
use sankhya_stats::{Bound, ColumnStats};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::{Lsn, LsnRange};
use std::collections::BTreeMap;
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("amount", DataType::Int64, false),
        Field::new("bucket", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

fn fragment(dir: &std::path::Path, index: u64, from: i64, to: i64) -> LoggedFile {
    let amounts: Vec<i64> = (from..to).collect();
    let buckets: Vec<i64> = amounts.iter().map(|a| a % 7).collect();
    let lsns: Vec<u64> = amounts
        .iter()
        .map(|a| u64::try_from(*a).expect("small"))
        .collect();
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(amounts.clone())),
            Arc::new(Int64Array::from(buckets)),
            Arc::new(UInt64Array::from(lsns)),
        ],
    )
    .expect("building");

    let name = format!("part-{index:04}.parquet");
    let report = write_parquet(
        dir,
        &name,
        &batch,
        Lsn::new(u64::try_from(to - 1).expect("small")),
        WriterConfig::default(),
    )
    .expect("writing");

    let mut stats = ColumnStats::default();
    for amount in &amounts {
        stats.observe(Some(&amount.to_be_bytes()), Some(Bound::Int(*amount)));
    }
    let mut catalogue = BTreeMap::new();
    catalogue.insert("amount".to_string(), stats);

    LoggedFile::new(
        dir.join(&name).to_str().expect("a utf-8 path").to_string(),
        report.bytes,
        u64::try_from(to - from).expect("small"),
    )
    .with_stats(catalogue)
}

fn provider(dir: &std::path::Path) -> SankhyaTable {
    provider_with(dir, true)
}

/// `nothing_past_target` is the provider's `exact_counts`: whether the tiers hold anything
/// beyond the read position. When they do not, the target filter can remove nothing and is
/// left out of the plan entirely — along with the column it would have read.
fn provider_with(dir: &std::path::Path, nothing_past_target: bool) -> SankhyaTable {
    let files: Vec<LoggedFile> = (0..6u64)
        .map(|i| {
            let from = i64::try_from(i).expect("small") * 100 + 1;
            fragment(dir, i, from, from + 100)
        })
        .collect();
    let coverage = LsnRange::up_to(Lsn::new(600));
    let splice =
        plan_splice(&[TierRef::new("published", coverage)], Lsn::new(600)).expect("a single tier");
    SankhyaTable::new(
        schema(),
        files,
        Vec::new(),
        Lsn::new(600),
        splice,
        nothing_past_target,
    )
}

/// The operators in a plan, outermost first.
///
/// Names only. Attributes carry metrics, partition counts and formatting that move for
/// reasons unrelated to whether the plan is the same plan.
fn shape(plan: &Arc<dyn ExecutionPlan>) -> Vec<String> {
    displayable(plan.as_ref())
        .indent(false)
        .to_string()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let trimmed = line.trim_start();
            trimmed
                .split([':', ' '])
                .next()
                .unwrap_or(trimmed)
                .to_string()
        })
        .collect()
}

async fn plan_for(ctx: &SessionContext, table: SankhyaTable, sql: &str) -> Arc<dyn ExecutionPlan> {
    ctx.register_table("orders", Arc::new(table))
        .expect("registering");
    ctx.sql(sql)
        .await
        .expect("planning")
        .create_physical_plan()
        .await
        .expect("physical plan")
}

#[tokio::test]
async fn a_scan_has_the_shape_it_has_always_had() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = SessionContext::new();
    let plan = plan_for(&ctx, provider(dir.path()), "SELECT amount FROM orders").await;

    // Reading at the latest position: nothing in any tier is past the target, so the
    // filter would remove nothing and is left out — along with the column it would have
    // read. That is the shape most queries get, because most queries read the latest
    // data.
    assert_eq!(
        shape(&plan),
        vec!["DataSourceExec".to_string()],
        "the plan for a plain scan changed"
    );
}

#[tokio::test]
async fn an_aggregate_keeps_its_two_phase_shape() {
    // Partial then final. Losing the partial phase turns a distributed aggregation into
    // one that ships every row to one place, which is correct and enormously slower.
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = SessionContext::new();
    let plan = plan_for(
        &ctx,
        provider(dir.path()),
        "SELECT bucket, SUM(amount) FROM orders GROUP BY bucket",
    )
    .await;

    let shape = shape(&plan);
    assert!(
        shape.iter().filter(|op| *op == "AggregateExec").count() >= 2,
        "the aggregate is no longer two-phase: {shape:?}"
    );
    assert_eq!(shape.last().map(String::as_str), Some("DataSourceExec"));
}

#[tokio::test]
async fn a_projection_does_not_read_columns_nobody_asked_for() {
    // Reading every column of every file is the single largest avoidable cost in a
    // columnar scan, and a plan that does it is not wrong -- only slower, by an amount
    // that grows with the table.
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = SessionContext::new();
    let plan = plan_for(&ctx, provider(dir.path()), "SELECT amount FROM orders").await;

    let text = displayable(plan.as_ref()).indent(false).to_string();
    assert!(
        !text.contains("bucket"),
        "the scan reads a column the query never mentions:\n{text}"
    );
}

#[tokio::test]
async fn a_pinned_read_enforces_its_position_inside_the_plan() {
    // When the tiers *do* hold rows past the target, the filter appears — and inside the
    // plan rather than above it, where a buffering operator would have consumed
    // everything before the filter ever ran.
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = SessionContext::new();
    let plan = plan_for(
        &ctx,
        provider_with(dir.path(), false),
        "SELECT amount FROM orders",
    )
    .await;

    let text = displayable(plan.as_ref()).indent(false).to_string();
    assert!(
        text.contains("_sankhya_commit_lsn"),
        "no filter on the read position appears in the plan:\n{text}"
    );
    assert!(
        shape(&plan).contains(&"FilterExec".to_string()),
        "the position is not enforced by a filter: {:?}",
        shape(&plan)
    );
}

#[tokio::test]
async fn a_read_at_the_latest_position_pays_for_no_filter_at_all() {
    // The optimisation, and the reason it is worth having. Enforcing the position costs a
    // column read and a predicate *per table*, so it compounds with join arity — measured
    // at 39% on a six-way join. Paying that on a query reading the latest data, which is
    // most queries, to support reading an older one is the wrong way round.
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = SessionContext::new();
    let plan = plan_for(&ctx, provider(dir.path()), "SELECT amount FROM orders").await;

    let text = displayable(plan.as_ref()).indent(false).to_string();
    assert!(
        !text.contains("_sankhya_commit_lsn"),
        "the position column is still being read when nothing can be filtered:\n{text}"
    );
    assert!(
        !shape(&plan).contains(&"FilterExec".to_string()),
        "a filter that can remove nothing is still in the plan: {:?}",
        shape(&plan)
    );
}

#[tokio::test]
async fn pruning_removes_files_from_the_plan_itself() {
    // The catalogue's whole benefit. A file the statistics prove irrelevant is not named
    // in the plan, so its footer is never opened -- which is invisible in a result and
    // visible here.
    let dir = tempfile::tempdir().expect("a temp dir");
    let table = provider(dir.path());
    let pruned = table.prunable(&[col("amount").eq(lit(150i64))]);
    assert_eq!(pruned, 5, "five of six files cannot hold the value");

    let ctx = SessionContext::new();
    let plan = plan_for(&ctx, table, "SELECT amount FROM orders WHERE amount = 150").await;
    let text = displayable(plan.as_ref()).indent(false).to_string();

    // Exactly one file survives, and it is the one holding 101..=200. Naming *which*
    // rather than counting matters: a plan that pruned the wrong five would pass a count.
    assert!(
        text.contains("part-0001.parquet"),
        "the surviving file is not the one holding the value:\n{text}"
    );
    let named = (0..6)
        .filter(|i| text.contains(&format!("part-{i:04}.parquet")))
        .count();
    assert_eq!(named, 1, "the plan names {named} files:\n{text}");
}

#[tokio::test]
async fn a_query_with_no_predicate_prunes_nothing() {
    // The other direction, so the test above cannot be satisfied by a plan that names no
    // files at all.
    //
    // Asserted on the provider rather than on the plan text, because the plan display
    // *truncates* a long file list with an ellipsis — counting names there reports five
    // of six for a plan that reads all six, which is a property of the formatter and not
    // of the plan.
    let dir = tempfile::tempdir().expect("a temp dir");
    let table = provider(dir.path());
    assert_eq!(table.prunable(&[]), 0);

    let ctx = SessionContext::new();
    let plan = plan_for(&ctx, table, "SELECT amount FROM orders").await;
    let text = displayable(plan.as_ref()).indent(false).to_string();
    assert!(
        text.contains("part-0000.parquet") && text.contains("part-0001.parquet"),
        "the plan reads fewer files than it should:\n{text}"
    );
}
