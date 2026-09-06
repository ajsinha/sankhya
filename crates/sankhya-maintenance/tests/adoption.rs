//! Maintenance finds the tables that were not there when it started.
//!
//! `OPS-10`, `OPS-11`. The table list was a `Vec<PathBuf>` taken once at startup, so a table
//! created afterwards was **never maintained** --- its log grew and its small files were
//! never compacted, for the life of the process. Nothing said so, because the aggregate
//! reclaimed-bytes figure kept rising from the tables that *were* being maintained, and a
//! warehouse that is shrinking looks like a warehouse being looked after.
//!
//! And a tick that failed was discarded with `Err(_) => continue`, so a table whose
//! compaction failed every thirty seconds for ever was invisible from a log and a dashboard
//! alike.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_maintenance::{MaintenancePolicy, StillReading};
use sankhya_table_delta::{commit, create, Metadata};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SCHEMA: &str = r#"{"type":"struct","fields":[]}"#;

/// An empty but valid table at `root`.
fn a_table(root: &Path) {
    std::fs::create_dir_all(root).expect("the table directory");
    commit(root, 0, &create(Metadata::new("t", SCHEMA.to_string(), 0))).expect("creating");
}

/// A policy that ticks as fast as the thread allows.
fn brisk() -> MaintenancePolicy {
    MaintenancePolicy {
        interval: Duration::from_millis(50),
        ..MaintenancePolicy::default()
    }
}

/// Wait until `condition` holds, or give up and say what it was waiting for.
///
/// Bounded, because a test that waits forever for something that never happens takes the
/// build with it and reports nothing --- the failure this repository has already met three
/// times through the mutation catalogue.
fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("waited ten seconds and {what} never happened");
}

#[test]
fn a_table_created_after_the_thread_started_is_maintained() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    a_table(&warehouse.join("sales").join("orders"));

    let handle = sankhya_maintenance::spawn_maintenance_over_warehouse(
        warehouse.clone(),
        brisk(),
        None,
        Arc::new(Mutex::new(StillReading::default())),
    );

    until("the first table was adopted", || handle.maintaining() == 1);

    // The table that did not exist when the thread started. Before this, it was maintained
    // by nobody, for ever, and the only symptom was a log that never got compacted.
    a_table(&warehouse.join("sales").join("returns"));
    until("the new table was adopted", || handle.maintaining() == 2);

    // And a table that goes away stops being ticked, rather than failing on every tick for
    // the rest of the process.
    std::fs::remove_dir_all(warehouse.join("sales").join("returns")).expect("dropping");
    until("the dropped table was released", || handle.maintaining() == 1);

    handle.stop();
}

#[test]
fn a_fixed_list_is_still_a_fixed_list() {
    // The other half of the same property, and the reason `Maintaining` is an enum rather
    // than a flag: a test that wants to maintain one table and nothing else still can, and
    // the two behaviours are named rather than inferred.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    let orders = warehouse.join("sales").join("orders");
    a_table(&orders);

    let handle = sankhya_maintenance::spawn_maintenance_watching_pins(
        vec![orders],
        brisk(),
        None,
        Arc::new(Mutex::new(StillReading::default())),
    );
    until("the listed table was adopted", || handle.maintaining() == 1);

    a_table(&warehouse.join("sales").join("returns"));
    // Given ten ticks to notice, and it must not.
    let ticks = handle.ticks();
    until("ten more ticks ran", || handle.ticks() > ticks + 10);
    assert_eq!(
        handle.maintaining(),
        1,
        "a thread given an explicit list must maintain that list and nothing else"
    );

    handle.stop();
}

#[test]
fn a_warehouse_nobody_can_list_is_reported_rather_than_read_as_empty() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    a_table(&warehouse.join("sales").join("orders"));

    let (found, unlisted) = sankhya_maintenance::tables_under_reporting(&warehouse);
    assert_eq!(found.len(), 1);
    assert!(unlisted.is_empty(), "a readable warehouse has nothing to report");

    // A warehouse that is not there at all is silent: one is created on first use.
    let (found, unlisted) =
        sankhya_maintenance::tables_under_reporting(&dir.path().join("never-created"));
    assert!(found.is_empty() && unlisted.is_empty());

    let schema = warehouse.join("sales");
    let mut permissions = std::fs::metadata(&schema).expect("it exists").permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&schema, permissions).expect("setting permissions");
    assert!(
        std::fs::read_dir(&schema).is_err(),
        "still readable after chmod 000 --- this test cannot run as root"
    );

    let (found, unlisted) = sankhya_maintenance::tables_under_reporting(&warehouse);

    let mut permissions = std::fs::metadata(&schema).expect("it exists").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&schema, permissions).ok();

    assert!(found.is_empty(), "nothing under it could be listed");
    assert_eq!(
        unlisted.len(),
        1,
        "a maintenance thread with nothing to maintain looks exactly like a warehouse that \
         needs none, so it must say which directory it could not read: {unlisted:?}"
    );
}

#[test]
fn a_tick_that_fails_is_counted_rather_than_discarded() {
    // `OPS-11`, verbatim: the tick's error was thrown away with `Err(_) => continue`, so a
    // table whose compaction failed on every tick failed silently for ever --- and the
    // aggregate reclaimed-bytes figure kept rising from the other tables, so the warehouse
    // looked healthy.
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    let orders = warehouse.join("sales").join("orders");
    a_table(&orders);

    let handle = sankhya_maintenance::spawn_maintenance_over_warehouse(
        warehouse.clone(),
        brisk(),
        None,
        Arc::new(Mutex::new(StillReading::default())),
    );
    until("the table was adopted", || handle.maintaining() == 1);

    // Ticks are running and none of them is failing, so the count below is about the log and
    // not about a thread that fails on everything.
    let ticks = handle.ticks();
    until("some ticks ran cleanly", || handle.ticks() > ticks + 3);
    assert_eq!(handle.failed(), 0, "a healthy table must not report failures");

    // The log is what every tick reads. Made unreadable while the thread runs, which is what
    // a permissions change or a half-mounted export does to a live warehouse.
    let log = orders.join("_delta_log");
    let mut permissions = std::fs::metadata(&log).expect("it exists").permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&log, permissions).expect("setting permissions");
    let unreadable = std::fs::read_dir(&log).is_err();

    let counted = if unreadable {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if handle.failed() > 0 || Instant::now() >= deadline {
                break handle.failed();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    } else {
        0
    };

    let mut permissions = std::fs::metadata(&log).expect("it exists").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&log, permissions).ok();
    handle.stop();

    assert!(
        unreadable,
        "the log is still readable after chmod 000 --- this test cannot run as root"
    );
    assert!(
        counted > 0,
        "a table whose maintenance cannot read its log must be counted as failing, not \
         skipped in silence"
    );
}

#[test]
fn maintenance_writes_the_checkpoints_that_bound_replay() {
    // `OPS-21`. `checkpoint_if_due` was called from tests and from nothing else, so every
    // log replay in the system --- at startup, on every statement's freshness probe, in
    // `doctor` --- ran from version zero: one `exists()`, one read and one JSON parse per
    // commit, per table, for the life of the warehouse.
    //
    // The reason recorded for not wiring it was that `checkpoint_if_due` needs the table's
    // `Metadata` and the log crate had no reader. That reasoning was right and it expired.
    use sankhya_table_delta::{latest_checkpoint, read_checkpoint, Action, AddFile};

    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    let orders = warehouse.join("sales").join("orders");
    a_table(&orders);

    // Past the interval, so one is due. Each commit is a version the next replay would
    // otherwise have to read.
    for version in 1..=(sankhya_maintenance::CHECKPOINT_INTERVAL + 2) {
        std::fs::write(orders.join(format!("part-{version}.parquet")), vec![b'x'; 64])
            .expect("a data file");
        commit(
            &orders,
            version,
            &[Action::Add(AddFile::with_rows(
                &format!("part-{version}.parquet"),
                64,
                0,
                1,
            ))],
        )
        .expect("publishing");
    }
    assert!(
        latest_checkpoint(&orders).is_none(),
        "nothing has checkpointed it yet"
    );

    let handle = sankhya_maintenance::spawn_maintenance_over_warehouse(
        warehouse.clone(),
        brisk(),
        None,
        Arc::new(Mutex::new(StillReading::default())),
    );
    until("a checkpoint was written", || {
        latest_checkpoint(&orders).is_some()
    });
    handle.stop();

    // And it summarises the log rather than being an empty file that satisfies a check: the
    // live set it holds is the live set a replay produces.
    let version = latest_checkpoint(&orders).expect("a checkpoint");
    let from_checkpoint = read_checkpoint(&orders, version).expect("readable");
    let by_replay = sankhya_table_delta::live_files(&orders).expect("replayable");
    assert_eq!(
        from_checkpoint.len(),
        by_replay.files.len(),
        "a checkpoint that does not agree with replay is worse than none"
    );
}
