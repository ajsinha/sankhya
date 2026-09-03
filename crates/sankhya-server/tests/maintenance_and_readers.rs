//! What happens to a running reader when the warehouse maintains itself underneath it.
//!
//! These two facts were each correct alone and were not put together:
//!
//! - A server resolves its table providers **once**, in `start()`. `resolve_with` reads the
//!   log, builds a file list, and the provider holds it. This is deliberate and documented on
//!   `Settings::read_as_of`: the server runs no ingest, so the warehouse it serves does not
//!   move.
//! - As of 2026-08-28 the server **starts a maintenance thread**, which compacts files and
//!   then retires the inputs a merge replaced once their grace period has run.
//!
//! The warehouse now moves. A provider fixed at boot is a listing that never refreshes, and
//! retirement's grace period protects a reader that listed *recently* — not one that listed
//! at startup and has been serving from it for hours.
//!
//! Reproduced here first, then fixed: a provider whose table has moved is resolved again
//! before it is registered, so a running server tracks the warehouse it is maintaining. The
//! test below is what caught it, and stays as the thing that would catch it coming back.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_api_pg::session::Caller;
use sankhya_api_pg::session::Handler;
use sankhya_authz::policy::{Action, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Role, TenantId};
use sankhya_maintenance::{Maintainer, MaintenancePolicy, RetentionPolicy};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::Arc;

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
#[path = "../src/feeds.rs"]
mod feeds;
#[path = "../src/driver.rs"]
mod driver;
#[path = "../src/snapshots.rs"]
mod snapshots;
#[path = "../src/wiring.rs"]
mod wiring;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("amount", DataType::Float64, false),
    ]))
}

fn batch(from: i64) -> RecordBatch {
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(vec![from, from + 1])),
            Arc::new(StringArray::from(vec!["north", "south"])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
        ],
    )
    .expect("a valid batch")
}

/// A table of several small files, which is what compaction is for.
fn table_with_small_files() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("sales").join("orders");
    let publication = Publication::external(&root, "orders");
    publication.create(&schema()).expect("creating");
    for file in 0..6u64 {
        publication
            .append(
                file + 1,
                &format!("part-{file:04}.parquet"),
                &batch(i64::try_from(file).unwrap_or(0) * 10),
                Lsn::new(file + 1),
            )
            .expect("publishing");
    }
    (dir, root)
}

/// How many rows a provider resolved *now* can read.
async fn rows_through_a_fresh_provider(root: &std::path::Path) -> usize {
    let table = sankhya_readpath::resolve(
        schema(),
        root,
        sankhya_types::LsnRange::new(Lsn::new(0), Lsn::new(u64::MAX)),
        None,
        Lsn::new(u64::MAX),
    )
    .expect("resolving");
    let context = SessionContext::new();
    context
        .register_table("orders", Arc::new(table))
        .expect("registering");
    let batches = context
        .sql("SELECT * FROM orders")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("executing");
    batches.iter().map(RecordBatch::num_rows).sum()
}

/// A maintainer that retires immediately, so a grace period does not hide the interaction.
fn retires_at_once() -> MaintenancePolicy {
    MaintenancePolicy {
        retention: RetentionPolicy {
            // Zero **only here**. The shipping default is 24 ticks and exists precisely to
            // protect a reader that listed before the merge. Setting it to zero is how this
            // test reaches, in one tick, the state a real deployment reaches after twelve
            // minutes.
            grace_ticks: 0,
            ..RetentionPolicy::default()
        },
        orphan_sweep_every: 0,
        ..MaintenancePolicy::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_provider_resolved_before_maintenance_cannot_read_after_retirement() {
    // The finding. A file list captured at boot survives compaction --- the merged file is
    // new and the inputs are still on disk --- and does **not** survive retirement, which is
    // the step that deletes them.
    let (_dir, root) = table_with_small_files();

    // A provider resolved now, exactly as `start()` resolves one.
    let before = sankhya_readpath::resolve(
        schema(),
        &root,
        sankhya_types::LsnRange::new(Lsn::new(0), Lsn::new(u64::MAX)),
        None,
        Lsn::new(u64::MAX),
    )
    .expect("resolving at startup");

    let mut maintainer = Maintainer::new(retires_at_once());
    // Two ticks: the first merges, the second retires what the first replaced.
    maintainer.tick(&root).expect("a merging tick");
    maintainer.tick(&root).expect("a retiring tick");

    let context = SessionContext::new();
    context
        .register_table("orders", Arc::new(before))
        .expect("registering");
    let outcome = context
        .sql("SELECT * FROM orders")
        .await
        .expect("planning")
        .collect()
        .await;

    // The provider held from before maintenance names files retirement has deleted, so it
    // fails --- which is the defect, and the reason `Server` re-resolves before registering.
    assert!(
        outcome.is_err(),
        "a file list captured before retirement should no longer be readable; if this starts \
         passing, retirement declined to remove anything and the test is no longer exercising \
         what it claims to"
    );

    // And a provider resolved *now* reads the table perfectly well. That is the whole fix:
    // not that retirement is wrong to delete, but that a reader must not hold a listing
    // across it.
    assert_eq!(
        rows_through_a_fresh_provider(&root).await,
        12,
        "six files of two rows, merged, still hold every row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_keeps_reading_across_its_own_maintenance() {
    // The fix, from the outside. A server resolves a table once at boot; maintenance then
    // merges its files and deletes the ones it replaced; and the next query must answer,
    // not fail on a path nobody asked about.
    //
    // Before the fix this failed with a missing-file error roughly twelve minutes into any
    // deployment with maintenance enabled --- one grace period after the first merge.
    let (dir, root) = table_with_small_files();

    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture opens: {refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "the fixture reads: {unreadable:?}");

    let tenant = TenantId::from_uuid(uuid::Uuid::from_u128(1));
    let policy = PolicySet::new().with(Rule::grant(
        tenant,
        Role::new("reader"),
        TableRef::new("sales", "orders"),
        Action::Read,
    ));
    let server = wiring::Server::with_tables(
        wiring::Settings {
            listen: "127.0.0.1:0".to_string(),
            warehouse: dir.path().to_path_buf(),
            read_as_of: Lsn::new(u64::MAX),
            tenant,
            cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
            flight_listen: None,
            maintenance: None,
            require_password: false,
            metrics_listen: None,
            transport_security: None,
        },
        policy,
        warehouse::describe(&found),
        servable,
    );
    server
        .authenticate(&[("user".to_string(), "ana".to_string())], Some(b"x"))
        .expect("authenticated");

    let before = server.query("SELECT * FROM orders", &Caller::new(&anyone())).expect("reads at first");
    assert_eq!(before.rows.len(), 12);

    // The warehouse maintains itself underneath the running server.
    let mut maintainer = Maintainer::new(retires_at_once());
    maintainer.tick(&root).expect("a merging tick");
    maintainer.tick(&root).expect("a retiring tick");

    let after = server
        .query("SELECT * FROM orders", &Caller::new(&anyone()))
        .expect("reads after its own maintenance");
    assert_eq!(
        after.rows.len(),
        12,
        "every row survives a merge; the file list must be re-read, not the rows re-counted"
    );
}

// --- leases: retirement waits for readers, not for a count of ticks ------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_statement_pins_the_warehouse_for_as_long_as_it_runs() {
    // The property the whole registry exists for, checked at the level where it matters: a
    // statement running against this server is announced, so a sweeper consulting the same
    // registry cannot conclude the warehouse is idle.
    //
    // Checked through the server's own registry rather than by racing a real deletion, because
    // a test that has to *win* a race to observe a property fails to observe it whenever it
    // loses --- which is a flaky test asserting a safety guarantee, the worst kind.
    let (dir, _root) = table_with_small_files();
    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture opens: {refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "the fixture reads: {unreadable:?}");

    let tenant = TenantId::from_uuid(uuid::Uuid::from_u128(1));
    let policy = PolicySet::new().with(Rule::grant(
        tenant,
        Role::new("reader"),
        TableRef::new("sales", "orders"),
        Action::Read,
    ));
    let server = wiring::Server::with_tables(
        wiring::Settings {
            listen: "127.0.0.1:0".to_string(),
            warehouse: dir.path().to_path_buf(),
            read_as_of: Lsn::new(u64::MAX),
            tenant,
            cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
            flight_listen: None,
            maintenance: None,
            require_password: false,
            metrics_listen: None,
            transport_security: None,
        },
        policy,
        warehouse::describe(&found),
        servable,
    );

    server
        .authenticate(&[("user".to_string(), "ana".to_string())], Some(b"x"))
        .expect("authenticated");

    let leases = server.leases();
    let idle = leases.mark();
    assert!(
        leases.drained(idle),
        "no statement is running, so nothing is holding the warehouse"
    );

    // Real statements, observed from outside while they run.
    //
    // Pinning by hand here would test the registry and not the server: a version of this test
    // that called `leases.pin()` itself passed with the pin removed from `run_statement`
    // altogether, which is the whole behaviour it was named for.
    let server = std::sync::Arc::new(server);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let querying = {
        let server = std::sync::Arc::clone(&server);
        let stop = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                server.query("SELECT * FROM orders", &Caller::new(&anyone())).expect("reads");
            }
        })
    };

    let mut saw_a_reader = false;
    for _ in 0..20_000 {
        let during = leases.mark();
        if !leases.drained(during) {
            saw_a_reader = true;
            break;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    querying.join().expect("no panic");

    assert!(
        saw_a_reader,
        "statements ran continuously and the registry never reported a reader inside"
    );

    let after = leases.mark();
    assert!(
        leases.drained(after),
        "and the warehouse drains once the statements stop"
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
