//! The provider answers, and plans without touching a file.
//!
//! Two claims are worth testing separately. That the answers are right is the obvious
//! one. That **planning costs nothing per file** is the one the whole metadata-only
//! design exists for, and it is invisible in a correctness test — a provider that opens
//! every Parquet footer returns exactly the same rows.

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
use sankhya_readpath::{resolve, resolve_cached, ReadError};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_publish::Publication;
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
    // Through `Publication`, not by assembling the log. A read test that builds its own add
    // actions asserts against a layout it wrote itself, so it keeps passing when the write
    // path changes underneath it --- which is the one thing a read test is for.
    let publication = Publication::external(root, "t");
    publication.create(&schema()).expect("creating");
    for i in 0..files {
        let from = i * per;
        publication
            .append(
                i + 1,
                &format!("part-{i:04}.parquet"),
                &rows(from, from + per),
                Lsn::new(from + per),
            )
            .expect("publishing");
    }
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
    // The invalid state comes from the crate that owns the log, not from this test: a test
    // that assembles a broken log by hand is doing storage work.
    sankhya_table_delta::malformed::create_table(root, "t", DELTA_SCHEMA).expect("creating");
    let report = write_parquet(
        root,
        "part-0000.parquet",
        &rows(0, 100),
        Lsn::new(100),
        WriterConfig::default(),
    )
    .expect("publishing");
    sankhya_table_delta::malformed::add_without_row_count(root, 1, "part-0000.parquet", report.bytes)
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

#[tokio::test]
async fn a_predicate_the_provider_does_not_evaluate_is_still_applied() {
    // This provider selects files; it does not evaluate predicates. Telling the engine
    // otherwise -- claiming a filter is handled *exactly* -- gives it permission to drop
    // the filter from the plan entirely, and the query silently returns every row.
    //
    // Nothing else in this file has a WHERE clause, so nothing else could notice.
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

    let (count, sum) = measure(
        Arc::new(table),
        "SELECT COUNT(*) c, SUM(amount) s FROM orders WHERE amount > 900",
    )
    .await;

    assert_eq!(count, 100, "the predicate was dropped from the plan");
    assert_eq!(sum, triangular(1_000) - triangular(900));
}

#[tokio::test]
async fn a_predicate_and_a_pinned_target_compose() {
    // Two filters from different places -- one the caller wrote, one the read position
    // implies -- must both survive into the plan.
    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 10, 100);

    let table = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(1_000))),
        None,
        Lsn::new(500),
    )
    .expect("resolving");

    let (count, sum) = measure(
        Arc::new(table),
        "SELECT COUNT(*) c, SUM(amount) s FROM orders WHERE amount > 400",
    )
    .await;

    assert_eq!(count, 100, "rows 401..=500 and no others");
    assert_eq!(sum, triangular(500) - triangular(400));
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

#[tokio::test]
async fn a_cached_resolve_answers_identically_to_an_uncached_one() {
    // The cache is a performance decision and must never be a correctness one, so the
    // two paths are compared directly rather than each checked against expectations.
    use sankhya_table_delta::LogCache;

    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 10, 100);
    let cache = LogCache::new();

    for target in [100u64, 550, 1_000] {
        let plain = resolve(
            schema(),
            dir.path(),
            Some(LsnRange::up_to(Lsn::new(1_000))),
            None,
            Lsn::new(target),
        )
        .expect("resolving");

        let cached = resolve_cached(
            schema(),
            dir.path(),
            Some(LsnRange::up_to(Lsn::new(1_000))),
            None,
            Lsn::new(target),
            &cache,
        )
        .expect("resolving");

        assert_eq!(plain.declared_rows(), cached.declared_rows());
        assert_eq!(plain.splice().tier_names(), cached.splice().tier_names());
        assert_eq!(
            measure(Arc::new(plain), AGGREGATE).await,
            measure(Arc::new(cached), AGGREGATE).await,
            "the cached plan answered differently at target {target}"
        );
    }
}

#[tokio::test]
async fn a_cached_resolve_sees_a_commit_made_after_it_warmed() {
    // A cache that misses a commit serves a file set missing rows, and nothing about the
    // result says so.
    use sankhya_table_delta::LogCache;

    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 4, 100);
    let cache = LogCache::new();

    let before = resolve_cached(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(400))),
        None,
        Lsn::new(400),
        &cache,
    )
    .expect("resolving");
    assert_eq!(before.declared_rows(), 400);

    // Another file, committed after the cache warmed.
    // Through the writer, at whatever version is free --- the fixture takes one version per
    // file now, so a hard-coded 2 is a version somebody else already used.
    let publication = Publication::external(dir.path(), "t");
    publication
        .append(
            publication.next_version(),
            "part-0004.parquet",
            &rows(400, 500),
            Lsn::new(500),
        )
        .expect("committing");

    let after = resolve_cached(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(500))),
        None,
        Lsn::new(500),
        &cache,
    )
    .expect("resolving");

    assert_eq!(after.declared_rows(), 500, "the cache missed a commit");
    let (count, sum) = measure(Arc::new(after), AGGREGATE).await;
    assert_eq!(count, 500);
    assert_eq!(sum, triangular(500));
}

/// A scan of many files runs on more than one partition.
///
/// This guards a defect that was found by measurement rather than by a test: the
/// provider handed the engine a single file group, a single group is a single
/// partition, and everything above it could then only redistribute batches that had
/// been read serially. The answers were identical and the query used one core.
///
/// It is asserted here rather than left to a benchmark because it is invisible in a
/// result. Nothing about the rows returned says how many threads produced them, so
/// without this the regression would come back silently — as it originally arrived.
///
/// The provider deliberately does *not* group the files itself. It passes them as one
/// group and lets the engine split them by byte range, which balances on size rather
/// than on file count and matches what the engine does for its own listing tables.
/// Grouping them by hand first was measurably worse, because the engine then had to
/// repartition an already-unbalanced arrangement. So the assertion is on the outcome —
/// the scan is parallel — not on the mechanism that produces it.
#[tokio::test]
async fn a_scan_of_many_files_is_parallel() {
    use datafusion::physical_plan::{ExecutionPlan, ExecutionPlanProperties};

    let dir = tempfile::tempdir().expect("a temp dir");
    publish(dir.path(), 32, 100);

    let table = resolve(
        schema(),
        dir.path(),
        Some(LsnRange::up_to(Lsn::new(3_200))),
        None,
        Lsn::new(3_200),
    )
    .expect("resolving");

    // Fixed rather than inherited from the machine, so the assertion means the same
    // thing on a build agent with two cores as on a workstation with ninety-six.
    let ctx = SessionContext::new_with_config(
        datafusion::prelude::SessionConfig::new().with_target_partitions(8),
    );
    ctx.register_table("orders", Arc::new(table))
        .expect("registering");

    let plan = ctx
        .sql("SELECT SUM(amount) FROM orders")
        .await
        .expect("planning")
        .create_physical_plan()
        .await
        .expect("a physical plan");

    // Walk to the scan itself. Partition counts above it prove nothing: a repartition
    // can manufacture eight partitions from one serial reader, which is exactly the
    // shape the original defect had.
    fn scan_partitions(plan: &Arc<dyn ExecutionPlan>) -> Option<usize> {
        if plan.name() == "DataSourceExec" {
            return Some(plan.output_partitioning().partition_count());
        }
        plan.children()
            .into_iter()
            .find_map(|child| scan_partitions(&Arc::clone(child)))
    }

    let partitions = scan_partitions(&plan).expect("a scan in the plan");
    assert!(
        partitions > 1,
        "the scan reads 32 files on {partitions} partition(s); it should use more than \
         one, or every core above it waits on one thread"
    );
}
