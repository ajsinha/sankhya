//! Does the commit path serialize the warehouse?
//!
//! # The criterion this answers
//!
//! Exit criterion 4, and [ADR-0013](../../../docs/adr/0013-concurrency-and-data-safety.md)'s
//! C1: *writers to different tables do not contend.* `ARCHITECTURE.md` has carried the same
//! sentence as a standing note since M0 --- *keep the commit path per-table, never globally
//! serialized* --- and M8 is where it stops being a note and becomes something measured.
//!
//! # Why it is measured here rather than through a publish
//!
//! `sankhya-publish` has the same measurement end to end, and it is the one that describes
//! what a writer experiences. But most of a publish is encoding Parquet, which parallelizes
//! whatever the log does, and that dilutes the signal: a global lock over **just** the commit
//! still leaves an end-to-end publish scaling nearly twice, because the encoding is untouched.
//!
//! The claim is about the commit path, so the measurement is of the commit path.
//!
//! # The control is in the run
//!
//! Every arm is measured twice --- once as the code stands, once with the same commits taken
//! through one mutex. That is the global serialization point the criterion forbids, applied
//! to the real function, and it is what turns the threshold below from taste into a figure
//! sitting between two measured states. A test whose threshold cannot tell the two apart is
//! the failure this repository has already shipped: a contention assertion set *below* the
//! contended figure, which passed with the defect restored.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_precision_loss
)]

use sankhya_testkit::capacity::can_measure;
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use std::path::Path;
use std::sync::{Barrier, Mutex, PoisonError};
use std::time::Instant;

const SCHEMA: &str = r#"{"type":"struct","fields":[{"name":"id","type":"long","nullable":false,"metadata":{}}]}"#;

/// Commits per writer in a measured arm.
const COMMITS: u64 = 600;

/// Held for the length of any test in this file: two throughput measurements sharing a
/// machine measure each other, and the harness runs a binary's tests concurrently.
static MEASURING: Mutex<()> = Mutex::new(());

fn a_table(root: &Path, name: &str) -> std::path::PathBuf {
    let table = root.join(name);
    std::fs::create_dir_all(&table).expect("the directory");
    commit(
        &table,
        0,
        &create(Metadata::new(name, SCHEMA.to_string(), 0)),
    )
    .expect("creating");
    table
}

/// Commits per second, from `writers` writers each committing to a table of its own.
///
/// `through`, when given, is taken around every commit --- one lock over the warehouse,
/// which is the design the criterion exists to forbid.
fn commits_per_second(root: &Path, writers: usize, through: Option<&Mutex<()>>) -> f64 {
    let tables: Vec<std::path::PathBuf> = (0..writers)
        .map(|writer| a_table(root, &format!("t{writer}")))
        .collect();
    let gate = Barrier::new(writers.saturating_add(1));

    let elapsed = std::thread::scope(|scope| {
        let handles: Vec<_> = tables
            .iter()
            .map(|table| {
                let gate = &gate;
                scope.spawn(move || {
                    // Built before the clock starts. Serializing an action is the writer's
                    // work, not the log's, and timing it here would dilute what this
                    // measures in exactly the way the module comment warns about.
                    let actions =
                        [Action::Add(AddFile::with_rows("part.parquet", 128, 0, 10))];
                    gate.wait();
                    for version in 1..=COMMITS {
                        let _serialized = through
                            .map(|lock| lock.lock().unwrap_or_else(PoisonError::into_inner));
                        commit(table, version, &actions).expect("committed");
                    }
                })
            })
            .collect();
        gate.wait();
        let started = Instant::now();
        for handle in handles {
            handle.join().expect("no panic");
        }
        started.elapsed()
    });

    (writers as u64 * COMMITS) as f64 / elapsed.as_secs_f64()
}

/// One arm's temporary directory, so no arm inherits another's page cache or file count.
fn arm() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

#[test]
fn commits_to_different_tables_do_not_contend() {
    let _measuring = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    // Skipped loudly and by name. Three cores cannot distinguish a commit path that scales
    // from one that does not, and neither can four cores somebody else is already using --- a
    // pass or a failure there would describe the machine.
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    if !can_measure("commits_to_different_tables_do_not_contend", 4) {
        return;
    }
    let writers = cores.min(8);
    if !can_measure("commits_to_different_tables_do_not_contend", writers) {
        return;
    }

    // Warm up, so the first arm does not pay for a cold page cache.
    let warm = arm();
    let _ = commits_per_second(warm.path(), 2, None);

    let solo = arm();
    let one = commits_per_second(solo.path(), 1, None);
    let spread = arm();
    let many = commits_per_second(spread.path(), writers, None);

    let lock = Mutex::new(());
    let solo_locked = arm();
    let one_locked = commits_per_second(solo_locked.path(), 1, Some(&lock));
    let spread_locked = arm();
    let many_locked = commits_per_second(spread_locked.path(), writers, Some(&lock));

    let scales = many / one;
    let serialized = many_locked / one_locked;
    println!(
        "C1: {writers} tables --- free {one:.0} -> {many:.0} commits/s ({scales:.2}x); \
         behind one warehouse lock {one_locked:.0} -> {many_locked:.0} ({serialized:.2}x)"
    );

    assert!(
        scales >= 3.0,
        "{writers} writers on {writers} tables managed {scales:.2}x one writer's throughput \
         ({one:.0} -> {many:.0} commits/s). A commit path that does not scale with tables is \
         a global serialization point, whatever the code looks like"
    );
    assert!(
        scales >= serialized * 2.0,
        "the free commit path scaled {scales:.2}x and the same commits behind one lock scaled \
         {serialized:.2}x --- too close for this measurement to distinguish them"
    );
}
