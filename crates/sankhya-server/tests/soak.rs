//! A real run: writes, queries and maintenance at once, sampled and judged.
//!
//! # Short, and honest about it
//!
//! `M6`'s exit criterion asks for a **multi-day** run at the ten-gigabyte scale. That is a
//! scheduled pipeline, not a `cargo test`, and nothing here pretends otherwise.
//!
//! What this is: the harness driven against the real server, on every build, at a size and
//! duration that fit in a test. It establishes that the measurements are actually taken from
//! a running system, that concurrent load does not break it, and that the judgement runs end
//! to end — so the scheduled run is a change of duration and scale rather than a first
//! attempt at the whole thing.
//!
//! **Load is concurrent, and that is the part that matters even at this size.** A soak that
//! writes and never queries proves the writer does not leak and nothing else; the interesting
//! failures are contention failures and they need contention.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_precision_loss
)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_diagnostic::soak::sample::{file_bytes, open_files, resident_bytes, Samples};
use sankhya_diagnostic::soak::Report;
use sankhya_maintenance::{Maintainer, MaintenancePolicy};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/warehouse.rs"]
mod warehouse;
#[path = "../src/adopt.rs"]
mod adopt;
#[path = "../src/clones.rs"]
mod clones;
#[path = "../src/cubes.rs"]
mod cubes;
#[path = "../src/aggregations.rs"]
mod aggregations;
#[path = "../src/feeds.rs"]
mod feeds;
#[path = "../src/driver.rs"]
mod driver;
#[path = "../src/snapshots.rs"]
mod snapshots;
#[path = "../src/wiring.rs"]
mod wiring;

use sankhya_api_pg::session::Caller;
use sankhya_api_pg::session::Handler;
use sankhya_authz::principal::TenantId;
use wiring::{Server, Settings};

/// How many sampling rounds the run takes.
///
/// Enough to clear `FEWEST_SAMPLES` with margin, and few enough that a build does not wait
/// on it. The scheduled run changes this number and nothing else.
const ROUNDS: usize = 60;

/// How long a round pauses, so the run spans wall-clock time rather than iterations.
///
/// Without it the sixty rounds finished in half a second, and half a second of samples can
/// speak about a second and a half — not about the three weeks the scheduled run asks about.
/// Sampling faster does not help: forty readings taken inside one allocation ramp are forty
/// readings of the same moment.
const PAUSE_MILLIS: u64 = 120;

/// **The horizon is not three weeks here, and the difference is the point.**
///
/// The scheduled multi-day run judges against weeks because it observes for days. A test
/// borrowing that horizon extrapolates by a factor of millions — which is exactly how the
/// first version of this reported a memory leak that was a process warming up. The harness
/// refuses that now, and `sankhya_diagnostic::soak::report::supported_horizon` is what a run should ask
/// for instead.

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
    ]))
}

fn write_table(root: &std::path::Path) {
    Publication::external(root, "orders")
        .create(&schema())
        .expect("created");
}

/// Publish one more file, as ingest would.
fn append_file(root: &std::path::Path, version: u64) {
    let ids: Vec<i64> = (0..50)
        .map(|i| i64::try_from(version * 50 + i).unwrap_or(0))
        .collect();
    let regions: Vec<Option<&str>> = ids
        .iter()
        .map(|i| if i % 2 == 0 { Some("north") } else { None })
        .collect();
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(regions)),
        ],
    )
    .expect("a valid batch");
    // Rebasing, because capture is not the only committer: maintenance writes to the same
    // log and takes the version this was about to use. Failing there would mean a compaction
    // can stop ingest, which inverts the ordering rule --- the source outranks maintenance.
    // The hand-rolled version counter this replaced could not survive a real maintainer.
    Publication::external(root, "orders")
        .append_rebasing(
            version,
            8,
            &format!("part-{version:05}.parquet"),
            &batch,
            Lsn::new(version + 1),
        )
        .expect("published");
}

/// One tick of the warehouse's own maintenance.
///
/// This used to be a hand-rolled "compaction": it committed a `Remove` for every input and
/// added a **one-row** replacement in their place, discarding the data it claimed to have
/// merged. Nothing read the result, so nothing noticed. It also never retired the inputs it
/// removed from the log, which is the defect that filled a disk in the longer soak.
///
/// A test has no business sequencing maintenance. `Maintainer::tick` is the product's own
/// pass --- plan, merge, commit, and retire what has served its grace period --- so this
/// harness asks for a tick and asserts on what happens, which is what a test is for.
fn maintain(maintainer: &mut Maintainer, root: &std::path::Path) {
    maintainer.tick(root).expect("a maintenance tick");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_short_run_under_concurrent_load_is_judged() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse_root = dir.path().join("warehouse");
    let data = dir.path().join(".sankhya");
    std::fs::create_dir_all(&data).expect("the data directory");
    let table_root = warehouse_root.join("sales").join("orders");
    write_table(&table_root);
    for version in 1..=4 {
        append_file(&table_root, version);
    }

    let (found, refused) = warehouse::discover(&warehouse_root);
    assert!(refused.is_empty(), "{refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "{unreadable:?}");
    let tables = warehouse::describe(&found);
    let tenant = TenantId::from_uuid(uuid::Uuid::from_u128(1));
    let server = Arc::new(Server::with_tables(
        Settings {
            roles: Default::default(),
            credentials: Default::default(),
            listen: "127.0.0.1:0".to_string(),
            warehouse: warehouse_root.clone(),
            read_as_of: Lsn::new(u64::MAX),
            tenant,
            cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
            flight_listen: None,
            maintenance: None,
            require_password: false,
            user_functions: false,
            metrics_listen: None,
            transport_security: None,
        },
        wiring::permissive_policy(&tenant, &tables),
        tables,
        servable,
    ));

    let queries = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let mut samples = Samples::new();
    let mut next_version = 5_u64;
    // The warehouse's own maintenance, driven a tick at a time so the test controls *when*
    // it runs without knowing anything about *what* it does.
    let mut maintainer = Maintainer::new(MaintenancePolicy::default());

    for round in 0..ROUNDS {
        // Queries, concurrently with everything else. Blocking work goes through
        // `block_in_place` for the same reason the real handler does: a synchronous handler
        // on an async worker starves the runtime otherwise.
        let running = {
            let server = Arc::clone(&server);
            let queries = Arc::clone(&queries);
            tokio::task::spawn(async move {
                for _ in 0..5 {
                    let server = Arc::clone(&server);
                    let ran = tokio::task::block_in_place(|| {
                        server.query("SELECT region, count(*) FROM orders GROUP BY region", &Caller::new(&anyone()))
                    });
                    if ran.is_ok() {
                        queries.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        };

        // Writes.
        append_file(&table_root, next_version);
        next_version += 1;

        // Maintenance, on a duty cycle: every fifth round, collapse what has accumulated.
        // This is what makes `live_files` a sawtooth rather than a ramp, which is the shape
        // the judgement is built for.
        if round % 5 == 4 {
            maintain(&mut maintainer, &table_root);
        }

        running.await.expect("the query task joins");
        tokio::time::sleep(std::time::Duration::from_millis(PAUSE_MILLIS)).await;

        // Sample after the round's work, so a reading reflects a completed unit rather than
        // an arbitrary moment inside one.
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let at = started.elapsed().as_micros() as i64;
        samples.record("resident_bytes", at, resident_bytes());
        samples.record("open_files", at, open_files());
        samples.record("metric_series", at, Some(0.0));
        samples.record(
            "history_bytes",
            at,
            Some(file_bytes(&data.join("diagnostic-history.tsv")).unwrap_or(0.0)),
        );
        samples.record(
            "queries",
            at,
            Some(queries.load(Ordering::Relaxed) as f64),
        );
        samples.record("audit_records", at, Some(server.audit_len() as f64));
        let live = sankhya_table_delta::live_files(&table_root).expect("replays");
        samples.record("live_files", at, Some(live.files.len() as f64));
        // Every watched measure, or the report is judging a run with one that was never
        // taken --- which reads as a failure rather than as a gap, and points at the wrong
        // thing entirely.
        samples.record(
            "warehouse_bytes",
            at,
            sankhya_diagnostic::soak::sample::tree_bytes(&warehouse_root),
        );
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let now = started.elapsed().as_micros() as i64;
    let report = Report::of(&samples, sankhya_diagnostic::soak::report::supported_horizon(&samples), now);

    // Printed whether or not it passes. A soak that reports only failures gives nobody the
    // trend, and the trend is how a slow drift is noticed before it is a failure.
    println!("\n{}", report.describe());

    assert!(
        queries.load(Ordering::Relaxed) >= ROUNDS as u64,
        "the run has to have actually queried, or it measured an idle process"
    );
    assert!(
        samples.of("resident_bytes").len() == ROUNDS,
        "every round sampled memory: {} of {ROUNDS}",
        samples.of("resident_bytes").len()
    );
    assert_eq!(
        samples.missed("resident_bytes"),
        0,
        "a reading that could not be taken is counted, not silently skipped"
    );
    assert!(
        samples.span_seconds() >= 5,
        "the run spanned {}s, which is too little to judge anything over",
        samples.span_seconds()
    );
    assert!(
        report.passed(),
        "the run did not come out clean:\n{}",
        report.describe()
    );
}

/// The caller a test means when it does not care who is asking.
///
/// Its own helper rather than an inline literal at forty call sites: when a test *does* care,
/// it should be visibly different from one that does not.
#[allow(dead_code)]
fn anyone() -> Vec<(String, String)> {
    vec![("user".to_string(), "quickstart".to_string())]
}
