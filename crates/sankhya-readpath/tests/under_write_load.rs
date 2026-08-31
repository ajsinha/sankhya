//! Are readers blocked by writers?
//!
//! # The criterion, and why it is stated as latency rather than as correctness
//!
//! Exit criterion 5 and [ADR-0013](../../../docs/adr/0013-concurrency-and-data-safety.md)'s
//! C2: *read latency is flat under write load.* Every other property in that document can be
//! delivered by one lock over the warehouse. This one cannot --- a global lock fails it
//! however carefully it is written, which is exactly why the criterion is phrased this way.
//!
//! # What the reader here is
//!
//! `resolve_cached` is the normal entry point: the table log resolved to a live file set,
//! spliced against the arrival tier, which is what every query plan does before it reads a
//! byte. It is also where the choke points ADR-0013 names actually live. Timing a full scan
//! instead would measure Parquet decoding, which no writer contends with.
//!
//! # The control is in the run
//!
//! A ratio needs a floor, and a floor copied from another machine is a number about that
//! machine. So each measurement is taken twice: once against the shipping path, and once
//! with the reader and the writers sharing one mutex --- the global serialization point the
//! criterion exists to forbid, applied to the real code. The threshold then comes from two
//! measured states rather than from taste.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_precision_loss
)]

use arrow_array::{Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use sankhya_publish::Publication;
use sankhya_readpath::resolve_cached;
use sankhya_table_delta::LogCache;
use sankhya_testkit::capacity::Window;
use sankhya_testkit::Until;
use sankhya_types::{Lsn, LsnRange};
use std::sync::{Arc, Barrier, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long each measured arm runs. Long enough for a few hundred resolutions, short enough
/// that four arms and their warm-up finish in a couple of seconds.
const WINDOW: Duration = Duration::from_millis(300);

/// Writers hammering *other* tables while the reader works.
const WRITERS: usize = 4;

/// Rows in a writer's batch. Real Parquet work, not a token file.
const ROWS: u64 = 2_000;

/// Held for the length of any test in this file: two throughput measurements sharing a
/// machine measure each other.
static MEASURING: Mutex<()> = Mutex::new(());

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

/// A table of `files` published fragments, written through the shipping write path.
fn published(root: &std::path::Path, name: &str, files: u64) -> Publication {
    let publication = Publication::external(root.join(name), name);
    publication.create(&schema()).expect("creating");
    for file in 0..files {
        let from = file * ROWS;
        publication
            .append(
                file + 1,
                &format!("part-{file:04}.parquet"),
                &rows(from, from + ROWS),
                Lsn::new(from + ROWS),
            )
            .expect("publishing");
    }
    publication
}

/// What a reader managed in one window.
struct Read {
    resolutions: usize,
    worst: Duration,
    p99: Duration,
}

impl Read {
    fn from(mut samples: Vec<Duration>) -> Self {
        samples.sort_unstable();
        let resolutions = samples.len();
        let worst = samples.last().copied().unwrap_or_default();
        // The index below the top percent, so a single scheduling hiccup does not become the
        // headline figure while a systematically blocked reader still shows up.
        let at = resolutions.saturating_mul(99) / 100;
        let p99 = samples.get(at.min(resolutions.saturating_sub(1))).copied().unwrap_or_default();
        Self { resolutions, worst, p99 }
    }
}

/// Resolve one table for `WINDOW`, recording how long each resolution took.
///
/// `through`, when given, is taken around the resolution --- the shape where a reader shares
/// a lock with the writers, which is what criterion 5 forbids.
fn read_for(root: &std::path::Path, through: Option<&Mutex<()>>) -> Read {
    let cache = LogCache::new();
    let schema = schema();
    let mut samples = Vec::new();
    let started = Instant::now();
    while started.elapsed() < WINDOW {
        let at = Instant::now();
        {
            let _serialized =
                through.map(|lock| lock.lock().unwrap_or_else(PoisonError::into_inner));
            resolve_cached(
                Arc::clone(&schema),
                root,
                Some(LsnRange::up_to(Lsn::new(u64::MAX))),
                None,
                Lsn::new(ROWS * 4),
                &cache,
            )
            .expect("the table resolves");
        }
        samples.push(at.elapsed());
    }
    Read::from(samples)
}

/// The same, with `WRITERS` writers publishing continuously to tables of their own.
fn read_under_write_load(root: &std::path::Path, through: Option<&Mutex<()>>) -> Read {
    let writers: Vec<Publication> = (0..WRITERS)
        .map(|writer| published(root, &format!("w{writer}"), 1))
        .collect();
    let batch = rows(0, ROWS);
    let until = Until::new();
    let gate = Barrier::new(WRITERS + 1);

    std::thread::scope(|scope| {
        for publication in &writers {
            let running = until.handle();
            let gate = &gate;
            let batch = &batch;
            scope.spawn(move || {
                gate.wait();
                let mut version = 2;
                while running.keep_going() {
                    let _serialized =
                        through.map(|lock| lock.lock().unwrap_or_else(PoisonError::into_inner));
                    publication
                        .append(
                            version,
                            &format!("part-{version:04}.parquet"),
                            batch,
                            Lsn::new(version * ROWS),
                        )
                        .expect("publishing");
                    version += 1;
                }
            });
        }
        gate.wait();
        let read = read_for(&root.join("readable"), through);
        // Stopped before the scope joins, and `Until` would stop them anyway on the way out
        // --- the difference between a test that fails and a test that hangs.
        until.stop();
        read
    })
}

fn arm() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

#[test]
#[ignore = "a throughput measurement: run alone by `check-concurrency`, because a measurement taken while `cargo test --workspace` saturates the machine describes the machine"]
fn read_latency_is_flat_under_write_load() {
    let _measuring = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    // Skipped loudly and by name, on two counts. With fewer cores than participants the reader
    // loses throughput to the scheduler rather than to a lock, and this would measure that;
    // with the cores present but already busy --- a full `cargo test --workspace` --- it would
    // measure the same thing while appearing to have enough. This test has failed that way.
    let Some(window) = Window::open("read_latency_is_flat_under_write_load", WRITERS + 2) else {
        return;
    };

    // Warm up, so the first arm does not pay for a cold page cache and report the read path
    // as slower than it is.
    let warm = arm();
    published(warm.path(), "readable", 8);
    let _ = read_for(&warm.path().join("readable"), None);

    let quiet = arm();
    published(quiet.path(), "readable", 8);
    let idle = read_for(&quiet.path().join("readable"), None);

    let busy = arm();
    published(busy.path(), "readable", 8);
    let loaded = read_under_write_load(busy.path(), None);

    let lock = Mutex::new(());
    let quiet_locked = arm();
    published(quiet_locked.path(), "readable", 8);
    let idle_locked = read_for(&quiet_locked.path().join("readable"), Some(&lock));

    let busy_locked = arm();
    published(busy_locked.path(), "readable", 8);
    let loaded_locked = read_under_write_load(busy_locked.path(), Some(&lock));

    let flat = loaded.resolutions as f64 / idle.resolutions as f64;
    let serialized = loaded_locked.resolutions as f64 / idle_locked.resolutions as f64;
    println!(
        "C2: {} resolutions idle, {} under {WRITERS} writers ({flat:.2}x), p99 {:?} -> {:?}, \
         worst {:?} -> {:?}; sharing one lock with the writers {serialized:.2}x, p99 {:?}",
        idle.resolutions,
        loaded.resolutions,
        idle.p99,
        loaded.p99,
        idle.worst,
        loaded.worst,
        loaded_locked.p99
    );

    // The machine has to have *stayed* quiet. A measurement that began on an idle machine and
    // finished on a saturated one describes the machine rather than the read path.
    if !window.held() {
        return;
    }

    // Measured across repeated runs on a twenty-four core machine: the free reader holds
    // 0.59 to 0.80 of its idle rate, and a reader sharing one lock with the writers holds
    // 0.00 to 0.07. Four tenths sits below everything the free path produced and far above
    // everything the serialized one did.
    //
    // It is deliberately not 1.0. Four writers encoding Parquet on the same filesystem cost
    // a reader something real --- page cache, directory metadata, memory bandwidth. What
    // this rules out is a reader *waiting* for a writer, and the control arm is what tells
    // those two apart.
    assert!(
        flat >= 0.4,
        "reads fell to {flat:.2} of their idle rate under {WRITERS} writers \
         ({} -> {} resolutions). Readers are supposed to be unaware of writers",
        idle.resolutions,
        loaded.resolutions
    );
    // The criterion is stated as latency, so it is also asserted as latency. A reader that
    // waits behind a writer's Parquet encode shows it here first: idle p99 is around a
    // fifth of a millisecond, and behind a shared lock it has been measured in seconds.
    assert!(
        loaded.p99 <= idle.p99 * 4,
        "the reader's p99 went from {:?} idle to {:?} under {WRITERS} writers",
        idle.p99,
        loaded.p99
    );
    // The control is what gives the threshold above its meaning. If a reader sharing a lock
    // with the writers scored as well as one that does not, this measurement could not see
    // the difference it exists to see.
    assert!(
        flat >= serialized * 5.0,
        "the free reader held {flat:.2} of its idle rate and a reader sharing one lock with \
         the writers held {serialized:.2} --- too close for this to distinguish them"
    );
}

#[test]
fn a_reader_of_the_table_being_written_never_waits_for_the_writer() {
    // The honest half. A reader of a table somebody is *appending to* must do more work as
    // commits arrive --- it has a longer log to replay --- so its rate is not expected to be
    // flat. What must hold is that it is never blocked, never refused, and never shown a
    // version that goes backwards.
    let _measuring = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = arm();
    let publication = published(dir.path(), "hot", 1);
    let batch = rows(0, ROWS);
    let until = Until::new();
    let gate = Barrier::new(2);

    let (resolutions, worst) = std::thread::scope(|scope| {
        let running = until.handle();
        let writing = {
            let publication = &publication;
            let batch = &batch;
            let gate = &gate;
            scope.spawn(move || {
                gate.wait();
                let mut version = 2;
                while running.keep_going() {
                    publication
                        .append(
                            version,
                            &format!("part-{version:04}.parquet"),
                            batch,
                            Lsn::new(version * ROWS),
                        )
                        .expect("publishing");
                    version += 1;
                }
                version - 2
            })
        };

        gate.wait();
        let cache = LogCache::new();
        let schema = schema();
        let mut resolutions = 0usize;
        let mut worst = Duration::ZERO;
        let mut seen = 0u64;
        let started = Instant::now();
        while started.elapsed() < WINDOW {
            let at = Instant::now();
            let table = resolve_cached(
                Arc::clone(&schema),
                &publication.root,
                Some(LsnRange::up_to(Lsn::new(u64::MAX))),
                None,
                Lsn::new(u64::MAX),
                &cache,
            )
            .expect("a table being written still resolves");
            worst = worst.max(at.elapsed());
            // Rows that went backwards would mean a reader had been shown a state older
            // than one it had already resolved --- the log read non-monotonically under an
            // appending writer, which is the failure a cache probing for a newer version
            // would produce if it ever answered from a stale entry.
            let declared = table.declared_rows();
            assert!(
                declared >= seen,
                "a reader saw {declared} rows after seeing {seen}: the log went backwards \
                 under a writer"
            );
            seen = declared;
            resolutions += 1;
        }
        until.stop();
        let committed = writing.join().expect("no panic");
        assert!(
            committed > 0,
            "the writer committed nothing, so this measured an idle table"
        );
        (resolutions, worst)
    });

    println!(
        "C2: {resolutions} resolutions of a table under continuous append, worst {worst:?}"
    );
    assert!(
        resolutions > 0,
        "a reader of a table being written made no progress at all"
    );
}
