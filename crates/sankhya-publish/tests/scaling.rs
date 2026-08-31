//! Does the write path scale with writers, and does contention on one table stay bounded?
//!
//! # Why these are measurements and not assertions
//!
//! Every safety property in [ADR-0013](../../../docs/adr/0013-concurrency-and-data-safety.md)
//! can be satisfied by one lock over the warehouse. That design loses no commit, shows no
//! partial file and deletes nothing a reader holds --- and it is the outcome the concurrency
//! criteria exist to forbid. So C1 and C3 cannot be shown by asserting that the code is
//! correct; they have to be shown by measuring what it does under load.
//!
//! # The control is in the run
//!
//! A throughput threshold copied from somebody else's machine measures that machine. Each
//! test here measures the shipping path **and** the same workload serialized through one
//! mutex, in the same run on the same hardware, and compares the two. That is what stops the
//! failure this repository has already shipped twice: a contention test whose threshold sat
//! *below* the contended figure, so it passed with the defect restored.
//!
//! The control mutex is not a fake of anything. It is a global serialization point, applied
//! to the real write path, which is exactly the shape C1 forbids.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_precision_loss
)]

use arrow_array::{Date32Array, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_publish::Publication;
use sankhya_table_delta::live_files;
use sankhya_testkit::capacity::can_measure;
use sankhya_testkit::Hammer;
use sankhya_types::Lsn;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex, PoisonError};
use std::time::Instant;

const FIRST_DAY: i32 = 19_723;

/// Rows per commit. Large enough that a commit is real work --- a Parquet file written,
/// statistics computed, a log entry claimed --- and small enough that a measurement arm
/// finishes in a second or so.
const ROWS: usize = 64;

/// Commits per writer in a measured arm. Enough that an arm runs for a tenth of a second
/// or more --- a window of a few milliseconds measures the scheduler's mood.
const COMMITS: usize = 200;

/// Held for the length of any test in this file.
///
/// The harness runs tests in a binary concurrently, and two throughput measurements sharing
/// a machine measure each other. Serializing them is not tidiness: without it the free arm
/// of one test competes with the serialized arm of another and the ratio is meaningless.
static MEASURING: Mutex<()> = Mutex::new(());

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("event_date", DataType::Date32, false),
    ]))
}

fn batch(from: i64, rows: usize) -> RecordBatch {
    let ids: Vec<i64> = (0..rows as i64).map(|i| from + i).collect();
    let dates: Vec<i32> = ids.iter().map(|_| FIRST_DAY).collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(Date32Array::from(dates)),
        ],
    )
    .expect("well-formed")
}

fn a_table(root: &Path, name: &str) -> Publication {
    let publication = Publication::external(root.join(name), name).dated_by("event_date");
    publication.create(&schema()).expect("created");
    publication
}

/// Commits per second, from `writers` writers each publishing to a table of its own.
///
/// `through`, when given, is taken around every publish. That is the global serialization
/// point, and it is what the free path is compared against.
fn commits_per_second(root: &Path, writers: usize, through: Option<&Mutex<()>>) -> f64 {
    let publications: Vec<Publication> = (0..writers)
        .map(|writer| a_table(root, &format!("t{writer}")))
        .collect();
    let payload = batch(0, ROWS);
    let gate = Barrier::new(writers.saturating_add(1));

    let elapsed = std::thread::scope(|scope| {
        let handles: Vec<_> = publications
            .iter()
            .map(|publication| {
                let gate = &gate;
                let payload = &payload;
                scope.spawn(move || {
                    gate.wait();
                    for round in 0..COMMITS {
                        let version = round as u64 + 1;
                        let name = format!("part-{round:05}.parquet");
                        // The guard covers the publish and nothing else, so the control
                        // serializes the write path rather than the test's bookkeeping.
                        let _serialized = through
                            .map(|lock| lock.lock().unwrap_or_else(PoisonError::into_inner));
                        publication
                            .append_rebasing(version, 4, &name, payload, Lsn::new(version))
                            .expect("committed");
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

    (writers * COMMITS) as f64 / elapsed.as_secs_f64()
}

/// Commits per second, from `writers` writers all publishing to **one** table.
///
/// The contended shape. Writing the files still parallelizes; claiming a version does not,
/// and must not --- that is the contention the protocol is supposed to have.
fn contended_commits_per_second(root: &Path, writers: usize) -> f64 {
    let publication = a_table(root, "one");
    let payload = batch(0, ROWS);
    let gate = Barrier::new(writers.saturating_add(1));

    let elapsed = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..writers)
            .map(|writer| {
                let gate = &gate;
                let payload = &payload;
                let publication = &publication;
                scope.spawn(move || {
                    gate.wait();
                    for round in 0..COMMITS {
                        let name = format!("w{writer:02}-{round:05}.parquet");
                        publication
                            .append_rebasing(
                                publication.next_version(),
                                // Sized from the measured distribution rather than from a
                                // fairness argument. At eight writers a rebase count has a
                                // median of three and a longest run past fifty, and the tail
                                // is what a budget has to cover; see `REBASE_BUDGET`.
                                writers * 32,
                                &name,
                                payload,
                                Lsn::new(round as u64 + 1),
                            )
                            .expect("committed after rebasing");
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

    (writers * COMMITS) as f64 / elapsed.as_secs_f64()
}

/// One arm's temporary directory, so no arm inherits another's page cache or file count.
/// The scaling this file asks the write path to demonstrate.
///
/// One constant, used both by the assertion and by the check that the machine could have
/// satisfied it. Two numbers would drift, and the drift would be invisible: a skip threshold
/// below the assertion produces a test that runs precisely when it cannot pass.
const FLOOR: f64 = 3.0;

fn arm() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

#[test]
fn writers_to_different_tables_scale_with_their_count() {
    // Exit criterion 4, and ADR-0013's C1. The property is not "commits succeed" --- they
    // succeed under a global lock too. It is that adding writers adds throughput.
    let _measuring = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    // A machine with three cores cannot distinguish a scaling write path from a serialized
    // one, and one whose cores are already spoken for cannot either --- under a full
    // `cargo test --workspace` a free arm that fails to outrun one writer says nothing about
    // the write path. Both are asked here, and either one skips loudly and by name.
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    if !can_measure("writers_to_different_tables_scale_with_their_count", 4) {
        return;
    }
    let writers = cores.min(8);
    if !can_measure("writers_to_different_tables_scale_with_their_count", writers) {
        return;
    }

    // Warm up. The first arm otherwise pays for cold page cache and lazily built Parquet
    // machinery, and would report the write path as slower than it is.
    let warm = arm();
    let _ = commits_per_second(warm.path(), 2, None);

    let solo = arm();
    let one = commits_per_second(solo.path(), 1, None);
    let parallel = arm();
    let many = commits_per_second(parallel.path(), writers, None);

    let lock = Mutex::new(());
    let solo_locked = arm();
    let one_locked = commits_per_second(solo_locked.path(), 1, Some(&lock));
    let parallel_locked = arm();
    let many_locked = commits_per_second(parallel_locked.path(), writers, Some(&lock));

    let scales = many / one;
    let serialized = many_locked / one_locked;
    println!(
        "C1: {writers} writers on {writers} tables --- free {one:.0} -> {many:.0} commits/s \
         ({scales:.2}x); behind one lock {one_locked:.0} -> {many_locked:.0} ({serialized:.2}x)"
    );

    // Three, from three measured states rather than from taste. Free, this path scales
    // between 4.4x and 5.9x across runs. With one lock over the whole publish it scales
    // 0.9x to 1.2x. With a lock over **only** the commit --- the narrower defect, and the
    // likelier one --- it still scales 1.87x, because encoding Parquet is untouched and is
    // most of a publish. A floor of two would have passed that. The commit path has its own
    // measurement in `sankhya-table-delta`, where the encoding does not mask it.
    assert!(
        scales >= FLOOR,
        "{writers} writers on {writers} tables managed {scales:.2}x the throughput of one \
         writer ({one:.0} -> {many:.0} commits/s). Throughput that does not grow with writers \
         is a global serialization point, whatever the code looks like"
    );
    // The control is what makes the number above mean something. If the serialized arm
    // scaled as well as the free one, this test could not tell the two apart and its
    // threshold would be taste rather than measurement.
    assert!(
        scales >= serialized * 2.0,
        "the free path scaled {scales:.2}x and the same workload behind one warehouse lock \
         scaled {serialized:.2}x --- too close for this measurement to distinguish them"
    );
}

#[test]
fn contention_on_one_table_is_bounded_and_loses_nothing() {
    // Exit criterion 6, and ADR-0013's C3. Every writer aims at the same version, because
    // each asks for the next free one before any of them commits.
    let _measuring = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = arm();
    let publication = a_table(dir.path(), "contested");
    let payload = batch(0, 64);

    const WRITERS: usize = 16;
    // Generously bounded, and bounded. A writer that loses every race until it is last
    // needs one attempt per writer ahead of it, and losing is not evenly spread: the
    // chance of losing `a` rounds in a row is (1-1/w)^a, so a ceiling of four per writer
    // refuses about one writer in sixty. The ceiling is here to catch a loop that does not
    // terminate, not to be reached, so it is set far above what the race needs and the
    // assertion below is on what the race actually cost.
    const ATTEMPTS: usize = WRITERS * 16;

    /// What a fair race should cost the unluckiest writer, with room for scheduling.
    const EXPECTED_WORST: usize = WRITERS * 4;

    let started = Instant::now();
    let hammered = Hammer::new().workers(WRITERS).run(|writer, jitter| {
        jitter.pause();
        publication.append_rebasing(
            publication.next_version(),
            ATTEMPTS,
            &format!("w{writer:02}.parquet"),
            &payload,
            Lsn::new(writer as u64 + 1),
        )
    });
    let elapsed = started.elapsed();

    let mut versions = BTreeSet::new();
    let mut worst = 0usize;
    for (writer, outcome) in hammered.outcomes.iter().enumerate() {
        let rebased = outcome.as_ref().unwrap_or_else(|error| {
            panic!(
                "writer {writer} could not commit under contention: {error} --- {}",
                hammered.replay_with()
            )
        });
        assert!(
            versions.insert(rebased.version),
            "two writers were told they committed version {} --- {}",
            rebased.version,
            hammered.replay_with()
        );
        worst = worst.max(rebased.retries);
    }

    assert_eq!(
        versions.len(),
        WRITERS,
        "{WRITERS} writers produced {} distinct versions --- {}",
        versions.len(),
        hammered.replay_with()
    );

    // Bounded, stated as the number rather than as the absence of a hang. A retry count at
    // the ceiling means the next writer would have been refused, which is the difference
    // between degradation and failure.
    assert!(
        worst < EXPECTED_WORST,
        "the worst writer needed {worst} rebases where a fair race among {WRITERS} writers \
         should cost fewer than {EXPECTED_WORST} --- {}",
        hammered.replay_with()
    );

    // Nothing was lost. A writer told it committed and whose file is not in the live set is
    // the silent loss this milestone opened with, arriving through a different door.
    let live = live_files(&publication.root).expect("the table resolves");
    let published: BTreeSet<String> = live
        .files
        .iter()
        .map(|file| {
            file.path
                .rsplit('/')
                .next()
                .unwrap_or(&file.path)
                .to_string()
        })
        .collect();
    for writer in 0..WRITERS {
        let name = format!("w{writer:02}.parquet");
        assert!(
            published.contains(&name),
            "writer {writer} was told it committed and {name} is not in the live set --- {}",
            hammered.replay_with()
        );
    }

    println!(
        "C3: {WRITERS} writers on one table, all committed in {elapsed:?}; worst rebase count \
         {worst}, against {EXPECTED_WORST} expected and a ceiling of {ATTEMPTS}"
    );
}

#[test]
fn a_loser_under_real_contention_is_told_rather_than_left_to_hang() {
    // The same shape with no room to rebase. What matters is not that writers fail --- they
    // must, with one attempt each and one free version between them --- but that a failure
    // says it is contention. A pipeline told "the table is busy" retries; a pipeline told
    // nothing waits for ever.
    let _measuring = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = arm();
    let publication = a_table(dir.path(), "crowded");
    let payload = batch(0, 64);

    const WRITERS: usize = 16;
    let hammered = Hammer::new().workers(WRITERS).run(|writer, jitter| {
        jitter.pause();
        publication.append_rebasing(
            publication.next_version(),
            1,
            &format!("w{writer:02}.parquet"),
            &payload,
            Lsn::new(writer as u64 + 1),
        )
    });

    let refused: Vec<String> = hammered
        .outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().err().map(ToString::to_string))
        .collect();
    assert!(
        !refused.is_empty(),
        "sixteen writers with one attempt each all committed, so this measured nothing --- {}",
        hammered.replay_with()
    );
    for detail in &refused {
        assert!(
            detail.contains("faster than this writer can follow"),
            "a writer that lost a race was told {detail:?} instead of that the table is \
             contended --- {}",
            hammered.replay_with()
        );
    }
}

#[test]
fn contention_on_one_table_degrades_rather_than_collapsing() {
    // The other half of exit criterion 6. Bounded retries say a contended table finishes;
    // this says it finishes faster than one writer would have, which is what separates
    // *degradation* from a queue with extra steps.
    let _measuring = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    if !can_measure("contention_on_one_table_degrades_rather_than_collapsing", 4) {
        return;
    }
    let writers = cores.min(8);
    if !can_measure("contention_on_one_table_degrades_rather_than_collapsing", writers) {
        return;
    }

    let warm = arm();
    let _ = commits_per_second(warm.path(), 2, None);

    let solo = arm();
    let one = commits_per_second(solo.path(), 1, None);
    let spread = arm();
    let uncontended = commits_per_second(spread.path(), writers, None);
    let shared = arm();
    let contended = contended_commits_per_second(shared.path(), writers);

    println!(
        "C3: {writers} writers --- one writer {one:.0}, {writers} tables {uncontended:.0}, \
         one table {contended:.0} commits/s ({:.0}% of the uncontended rate)",
        contended / uncontended * 100.0
    );

    // The claim, stated as the thing that would be false if rebasing collapsed: writers
    // sharing a table still beat a single writer. If every rebase cost more than it saved,
    // this would sit below one writer's rate and contention would be a net loss.
    assert!(
        contended > one,
        "{writers} writers on one table managed {contended:.0} commits/s against one \
         writer's {one:.0} --- rebase-and-retry cost more than the parallelism gained"
    );
}
