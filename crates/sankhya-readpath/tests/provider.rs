//! The provider answers, and plans without touching a file.
//!
//! Two claims are worth testing separately. That the answers are right is the obvious
//! one. That **planning costs nothing per file** is the one the whole metadata-only
//! design exists for, and it is invisible in a correctness test — a provider that opens
//! every Parquet footer returns exactly the same rows.

use arrow_array::{Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_readpath::{resolve, ReadError};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use sankhya_table_memory::{ArrivalBuffer, MemoryBudget};
use sankhya_types::{Lsn, LsnRange};
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

fn triangular(n: u64) -> i64 {
    i64::try_from(n * (n + 1) / 2).expect("small")
}

fn range(from: u64, to: u64) -> LsnRange {
    LsnRange::new(Lsn::new(from), Lsn::new(to)).expect("a non-empty range")
}

/// A table of `files` published fragments of `per` rows each.
fn publish(root: &std::path::Path, files: u64, per: u64) {
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
}

async fn measure(table: Arc<sankhya_readpath::SankhyaTable>, sql: &str) -> (i64, i64) {
    let ctx = SessionContext::new();
    ctx.register_table("orders", table).expect("registering");
    let batches = ctx
        .sql(sql)
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

const AGGREGATE: &str = "SELECT COUNT(*) c, SUM(amount) s FROM orders";

#[tokio::test]
async fn the_provider_answers_from_published_files() {
    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 10, 100);

    let table = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(1_000))),
        None,
        Lsn::new(1_000),
    )
    .expect("resolving");

    assert_eq!(table.splice().tier_names(), vec!["published"]);
    let (count, sum) = measure(Arc::new(table), AGGREGATE).await;
    assert_eq!(count, 1_000);
    assert_eq!(sum, triangular(1_000));
}

#[tokio::test]
async fn the_provider_splices_memory_and_files() {
    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 7, 100);

    let mut arrival = ArrivalBuffer::new("arrival", schema(), MemoryBudget::default());
    arrival.append(rows(0, 1_000), range(0, 1_000));
    arrival.note_durable(Lsn::new(700));

    let table = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(700))),
        Some(&arrival),
        Lsn::new(1_000),
    )
    .expect("resolving");

    assert_eq!(table.splice().tier_names(), vec!["published", "arrival"]);
    let (count, sum) = measure(Arc::new(table), AGGREGATE).await;
    assert_eq!(count, 1_000, "the overlap was counted twice");
    assert_eq!(sum, triangular(1_000));
}

#[tokio::test]
async fn a_pinned_read_stops_at_its_target() {
    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 10, 100);

    let table = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(1_000))),
        None,
        Lsn::new(350),
    )
    .expect("resolving");

    let (count, sum) = measure(Arc::new(table), AGGREGATE).await;
    assert_eq!(count, 350);
    assert_eq!(sum, triangular(350));
}

#[tokio::test]
async fn statistics_come_from_the_log_and_are_exact_when_nothing_is_filtered() {
    use datafusion::catalog::TableProvider;
    use datafusion::common::stats::Precision;

    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 10, 100);

    let table = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(1_000))),
        None,
        Lsn::new(1_000),
    )
    .expect("resolving");

    let stats = table.statistics().expect("the log knows the row count");
    assert_eq!(stats.num_rows, Precision::Exact(1_000));
}

#[tokio::test]
async fn statistics_are_inexact_when_the_target_can_filter() {
    use datafusion::catalog::TableProvider;
    use datafusion::common::stats::Precision;

    // Pinned below what the tiers hold, so the declared count is an upper bound.
    // Reporting it as exact would let the optimizer order joins on a number that is
    // simply wrong -- a slow plan chosen confidently, which is harder to notice than a
    // slow plan chosen for want of information.
    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 10, 100);

    let table = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(1_000))),
        None,
        Lsn::new(350),
    )
    .expect("resolving");

    assert_eq!(
        table.statistics().expect("statistics").num_rows,
        Precision::Inexact(1_000)
    );
}

#[tokio::test]
async fn a_gap_is_refused_at_planning_time() {
    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 3, 100);

    let mut arrival = ArrivalBuffer::new("arrival", schema(), MemoryBudget::default());
    arrival.append(rows(700, 1_000), range(700, 1_000));

    let err = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(300))),
        Some(&arrival),
        Lsn::new(1_000),
    )
    .expect_err("positions 301..=700 are held by nothing");

    assert!(matches!(err, ReadError::Splice(_)), "{err}");
}

#[tokio::test]
async fn a_file_without_a_row_count_is_refused() {
    // Treating an absent count as zero would tell the optimizer the table is empty,
    // which produces a wrong plan rather than a slow one.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    commit(root, 0, &create(Metadata::new("t", DELTA_SCHEMA, 0))).expect("creating");
    let report = write_parquet(
        root,
        "part-0000.parquet",
        &rows(0, 100),
        Lsn::new(100),
        WriterConfig::default(),
    )
    .expect("publishing");
    commit(
        root,
        1,
        &[Action::Add(AddFile::new(
            "part-0000.parquet",
            report.bytes,
            0,
        ))],
    )
    .expect("committing without statistics");

    let err = resolve(
        schema(),
        root,
        Some(LsnRange::up_to(Lsn::new(100))),
        None,
        Lsn::new(100),
    )
    .expect_err("a file with no row count cannot be planned against");

    assert!(format!("{err}").contains("without a row count"));
}

#[tokio::test]
async fn an_unreadable_log_is_refused_rather_than_guessed_around() {
    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 3, 100);
    std::fs::write(
        dir.path()
            .join("_delta_log")
            .join("00000000000000000002.json"),
        "{not json}\n",
    )
    .expect("corrupting the log");

    let err = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(300))),
        None,
        Lsn::new(300),
    )
    .expect_err("a malformed log must refuse");

    assert!(matches!(err, ReadError::Log(_)), "{err}");
}

#[tokio::test]
async fn the_provider_carries_its_provenance() {
    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 7, 100);
    let mut arrival = ArrivalBuffer::new("arrival", schema(), MemoryBudget::default());
    arrival.append(rows(0, 1_000), range(0, 1_000));
    arrival.note_durable(Lsn::new(700));

    let table = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(700))),
        Some(&arrival),
        Lsn::new(1_000),
    )
    .expect("resolving");

    let provenance = table.splice().provenance();
    assert_eq!(provenance[0].0, "published");
    assert_eq!(provenance[0].1.end_inclusive(), Lsn::new(700));
    assert_eq!(provenance[1].0, "arrival");
    assert_eq!(provenance[1].1.start_exclusive(), Lsn::new(700));
}

/// What metadata-only coupling is worth, measured rather than argued.
///
/// Run with `cargo test -p sankhya-readpath --test provider --release -- --ignored
/// --nocapture measure`.
///
/// The claim is that **planning does no file I/O**, because statistics come from the
/// table log rather than from Parquet footers. That predicts planning latency roughly
/// flat in file count, against a listing-based path that grows with it.
///
/// This is invisible in every correctness test above: a provider that opens every footer
/// returns exactly the same rows.
#[tokio::test]
#[ignore = "a measurement, not an assertion"]
async fn measure_planning_cost_against_file_count() {
    use datafusion::prelude::ParquetReadOptions;

    for files in [50u64, 200, 800] {
        let dir = tempfile::tempdir().expect("a temp dir");
        publish(dir.path(), files, 50);
        let end = files * 50;

        // The provider: file list and row counts from the log.
        let mut provider_best = std::time::Duration::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let table = resolve(
                schema(),
                dir.path(),
                Some(LsnRange::up_to(Lsn::new(end))),
                None,
                Lsn::new(end),
            )
            .expect("resolving");
            let ctx = SessionContext::new();
            ctx.register_table("orders", Arc::new(table))
                .expect("registering");
            let _ = ctx
                .sql(AGGREGATE)
                .await
                .expect("planning")
                .create_physical_plan()
                .await
                .expect("physical plan");
            provider_best = provider_best.min(start.elapsed());
        }

        // The listing path: directory scan plus a footer read per file.
        let mut listing_best = std::time::Duration::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let ctx = SessionContext::new();
            ctx.register_parquet(
                "orders",
                dir.path().to_str().expect("a utf-8 path"),
                ParquetReadOptions::default(),
            )
            .await
            .expect("registering");
            let _ = ctx
                .sql(AGGREGATE)
                .await
                .expect("planning")
                .create_physical_plan()
                .await
                .expect("physical plan");
            listing_best = listing_best.min(start.elapsed());
        }

        println!(
            "{files:>4} files: provider {:>9.2?}   listing {:>9.2?}   {:.1}x",
            provider_best,
            listing_best,
            listing_best.as_secs_f64() / provider_best.as_secs_f64()
        );
    }
}
