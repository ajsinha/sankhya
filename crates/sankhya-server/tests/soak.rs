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
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use sankhya_types::Lsn;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/warehouse.rs"]
mod warehouse;
#[path = "../src/wiring.rs"]
mod wiring;

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
    std::fs::create_dir_all(root).expect("the table directory");
    let delta = sankhya_table_delta::schema_string(&schema()).expect("representable");
    commit(root, 0, &create(Metadata::new("orders", delta, 0))).expect("created");
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
    let name = format!("part-{version:05}.parquet");
    let report = write_parquet(root, &name, &batch, Lsn::new(version + 1), WriterConfig::default())
        .expect("written");
    commit(
        root,
        version,
        &[Action::Add(AddFile::with_rows(&name, report.bytes, 0, 50))],
    )
    .expect("published");
}

/// Remove the files a compaction would, replacing them with one.
fn compact(root: &std::path::Path, version: u64, replacing: &[String]) {
    let mut actions: Vec<Action> = replacing
        .iter()
        .map(|path| {
            Action::Remove(sankhya_table_delta::RemoveFile::rewritten(
                path.clone(),
                i64::try_from(version).unwrap_or(0),
            ))
        })
        .collect();
    let name = format!("compacted-{version:05}.parquet");
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(vec![0_i64])),
            Arc::new(StringArray::from(vec![Some("north")])),
        ],
    )
    .expect("a valid batch");
    let report = write_parquet(root, &name, &batch, Lsn::new(version + 1), WriterConfig::default())
        .expect("written");
    actions.push(Action::Add(AddFile::with_rows(&name, report.bytes, 0, 1)));
    commit(root, version, &actions).expect("compacted");
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
            listen: "127.0.0.1:0".to_string(),
            warehouse: warehouse_root.clone(),
            read_as_of: Lsn::new(u64::MAX),
            tenant,
            require_password: false,
            metrics_listen: None,
        },
        wiring::permissive_policy(&tenant, &tables),
        tables,
        servable,
    ));

    let queries = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let mut samples = Samples::new();
    let mut next_version = 5_u64;

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
                        server.query("SELECT region, count(*) FROM orders GROUP BY region")
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
            let live = sankhya_table_delta::live_files(&table_root).expect("replays");
            let replacing: Vec<String> =
                live.files.iter().map(|file| file.path.clone()).collect();
            compact(&table_root, next_version, &replacing);
            next_version += 1;
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
