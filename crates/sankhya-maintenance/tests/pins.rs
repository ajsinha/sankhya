//! Who is allowed to delete a file, and what happens when that cannot be established.
//!
//! Reclamation asks one question --- *does anything still read this?* --- and the audit found
//! four independent ways for the answer to come back **no** when the truth was yes. None of
//! them was a race. Each fired on a schedule, deleted data somebody was still reading, and
//! reported nothing.
//!
//! - `COR-01` --- a clone's pin is recorded as `schema.table` and the sweeper asked for
//!   `table`, so the comparison was false in every deployment.
//! - `COR-03` --- the orphan sweep honoured clone pins and not snapshot pins, and a pinned
//!   file is *guaranteed* to reach the sweep's age threshold because retirement correctly
//!   refuses it for ever.
//! - `OPS-13` --- a pin that could not be read contributed nothing, which is indistinguishable
//!   from a pin that protects nothing.
//! - `OPS-14` --- the leaked-lease backstop was written as an `and`, so it was not a backstop.
//!
//! Each test here is written in the naming and the directory layout the server actually uses,
//! because the shape `COR-01` hid in was a test built at the warehouse root.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use sankhya_clone::{Lineage, Lineages};
use sankhya_leases::Leases;
use sankhya_maintenance::{Maintainer, MaintenancePolicy, OrphanPolicy, StillReading};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata, RemoveFile};
use sankhya_types::Lsn;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SCHEMA: &str = r#"{"type":"struct","fields":[{"name":"id","type":"long","nullable":false,"metadata":{}}]}"#;

fn arrow_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]))
}

/// A table where the server puts one: `<warehouse>/<schema>/<table>`.
///
/// The layout is the test. `discover` only ever walks two levels, so the qualified name is the
/// only name anything records --- and a fixture that builds its table one level up is a fixture
/// in which the naming defect cannot appear.
fn a_table_that_moved_on(warehouse: &Path, schema: &str, table: &str) -> PathBuf {
    let root = warehouse.join(schema).join(table);
    std::fs::create_dir_all(&root).expect("the table directory");
    commit(&root, 0, &create(Metadata::new(table, SCHEMA.to_string(), 0))).expect("creating");

    for name in ["part-0000.parquet", "part-0001.parquet"] {
        std::fs::write(root.join(name), vec![b'x'; 512]).expect("a data file");
    }
    commit(
        &root,
        1,
        &[Action::Add(AddFile::with_rows("part-0000.parquet", 512, 0, 1))],
    )
    .expect("v1");
    commit(
        &root,
        2,
        &[
            Action::Remove(RemoveFile::rewritten("part-0000.parquet", 0)),
            Action::Add(AddFile::with_rows("part-0001.parquet", 512, 0, 1)),
        ],
    )
    .expect("v2");
    root
}

/// Sweeping orphans every tick, with the age threshold off.
///
/// Zero **only here**. The shipping default is a week, and it is the one thing standing
/// between this sweep and a file a committer is in the middle of writing --- so a test that
/// sets it to zero says why, or it reads as a recommendation.
fn sweeping_at_once() -> MaintenancePolicy {
    MaintenancePolicy {
        orphan_sweep_every: 1,
        orphans: OrphanPolicy { min_age_ticks: 0 },
        ..MaintenancePolicy::default()
    }
}

// --- COR-01 -----------------------------------------------------------------------------

#[test]
fn a_clone_pin_recorded_as_schema_dot_table_reaches_the_sweeper() {
    // The defect, at production's naming. `clones.record` stores what a person typed and what
    // the catalogue resolves --- `sales.entries`. The sweeper has a directory, and a directory
    // knows only its leaf. `origin == table` was therefore false for every clone in every
    // warehouse, the pin was invisible, the file aged past the threshold, and the clone read
    // short with no error at all.
    let home = tempfile::tempdir().expect("a temporary directory");
    let root = a_table_that_moved_on(home.path(), "sales", "entries");

    let mut clones = Lineages::new();
    clones.record("sales.staging", Lineage::new("sales.entries", 1, 0));

    let mut maintainer = Maintainer::new(sweeping_at_once()).among(clones);
    maintainer.tick(&root).expect("a tick");

    assert!(
        root.join("part-0000.parquet").exists(),
        "the origin swept a file only its clone still reads --- the qualified name the clone \
         was recorded under never reached the sweeper"
    );
}

#[test]
fn a_clone_of_a_different_schemas_table_of_the_same_name_pins_nothing() {
    // The control that keeps the fix from being "match on the leaf and hope". `hr.entries` and
    // `sales.entries` are different tables; a clone of one must not hold the other's files, or
    // the fix has traded a deletion bug for a warehouse that never reclaims.
    let home = tempfile::tempdir().expect("a temporary directory");
    let root = a_table_that_moved_on(home.path(), "sales", "entries");

    let mut clones = Lineages::new();
    clones.record("hr.staging", Lineage::new("hr.entries", 1, 0));

    let mut maintainer = Maintainer::new(sweeping_at_once()).among(clones);
    maintainer.tick(&root).expect("a tick");

    assert!(
        !root.join("part-0000.parquet").exists(),
        "a clone of `hr.entries` held a file belonging to `sales.entries`"
    );
}

// --- COR-03 -----------------------------------------------------------------------------

#[test]
fn the_orphan_sweep_honours_a_snapshot_pin() {
    // Retirement unioned clone pins and snapshot pins under a comment reading *"two rules
    // disagree eventually, and the one that loses deletes a file somebody is reading"*. This
    // sweep was the second rule, and it read only half the union.
    //
    // Deterministic rather than racy: retirement correctly declines a pinned file for ever,
    // which **guarantees** it crosses this sweep's age threshold. Every snapshot older than
    // the threshold lost its files, on schedule.
    let home = tempfile::tempdir().expect("a temporary directory");
    let root = a_table_that_moved_on(home.path(), "sales", "entries");

    let pinned = std::collections::BTreeMap::from([(root.clone(), vec![1_u64])]);
    let mut maintainer = Maintainer::new(sweeping_at_once()).pinning(pinned);
    maintainer.tick(&root).expect("a tick");

    assert!(
        root.join("part-0000.parquet").exists(),
        "a snapshot pins version 1, version 1 names this file, and the orphan sweep deleted it"
    );
}

#[test]
fn the_orphan_sweep_still_collects_what_no_snapshot_pins() {
    // The control. Without it the test above passes just as happily against a sweeper that
    // has stopped sweeping.
    let home = tempfile::tempdir().expect("a temporary directory");
    let root = a_table_that_moved_on(home.path(), "sales", "entries");

    let pinned = std::collections::BTreeMap::from([(root.clone(), vec![2_u64])]);
    let mut maintainer = Maintainer::new(sweeping_at_once()).pinning(pinned);
    maintainer.tick(&root).expect("a tick");

    assert!(
        !root.join("part-0000.parquet").exists(),
        "version 2 does not name it, so a snapshot of version 2 is not a reason to keep it"
    );
}

// --- OPS-13 -----------------------------------------------------------------------------

#[test]
fn a_pin_that_could_not_be_read_stops_reclamation_and_says_so() {
    // The unsafe direction, stated as a test. A snapshot document that will not parse used to
    // contribute nothing --- and contributing nothing is what a table with no snapshots also
    // does, so the sweeper reclaimed exactly the files whose protection it had failed to read.
    let home = tempfile::tempdir().expect("a temporary directory");
    let root = a_table_that_moved_on(home.path(), "sales", "entries");

    let mut maintainer = Maintainer::new(sweeping_at_once());
    maintainer.told(
        &StillReading {
            unreadable: vec!["snapshots/nightly.json: unexpected end of input".to_string()],
            ..StillReading::default()
        },
        &root,
    );
    let report = maintainer.tick(&root).expect("a tick");

    assert!(
        root.join("part-0000.parquet").exists(),
        "reclamation ran while the pin set was unknown"
    );
    assert!(
        report.declined.iter().any(|why| why.contains("nightly.json")),
        "it declined silently, which from disk usage alone is indistinguishable from broken \
         --- {:?}",
        report.declined
    );
}

#[test]
fn a_pin_picture_with_no_holes_reclaims_as_usual() {
    // The control: `told` with nothing unreadable must not become a permanent brake.
    let home = tempfile::tempdir().expect("a temporary directory");
    let root = a_table_that_moved_on(home.path(), "sales", "entries");

    let mut maintainer = Maintainer::new(sweeping_at_once());
    maintainer.told(&StillReading::default(), &root);
    let report = maintainer.tick(&root).expect("a tick");

    assert!(report.declined.is_empty(), "{:?}", report.declined);
    assert!(
        !root.join("part-0000.parquet").exists(),
        "nothing pins this file and nothing failed to be read; it is debris"
    );
}

#[test]
fn the_maintenance_thread_counts_the_ticks_it_declined() {
    // A log line answers *"did this happen?"*. An operator looking at a warehouse that is not
    // shrinking needs *"is it still happening?"*, and reading a log to find out that nothing
    // is being deleted is how `OPS-13`'s discarded complaints stayed discarded.
    let home = tempfile::tempdir().expect("a temporary directory");
    let root = a_table_that_moved_on(home.path(), "sales", "entries");

    let reading = Arc::new(std::sync::Mutex::new(StillReading {
        unreadable: vec!["snapshots/nightly.json: unexpected end of input".to_string()],
        ..StillReading::default()
    }));
    let mut policy = sweeping_at_once();
    policy.interval = std::time::Duration::from_millis(10);
    let handle = sankhya_maintenance::spawn_maintenance_watching_pins(
        vec![root.clone()],
        policy,
        None,
        Arc::clone(&reading),
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while handle.declined() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let declined = handle.declined();
    handle.stop();

    assert!(declined > 0, "the thread declined to reclaim and counted nothing");
    assert!(
        root.join("part-0000.parquet").exists(),
        "and it declined in the direction that keeps files"
    );
}

// --- OPS-14 -----------------------------------------------------------------------------

/// Eight small files in one table, which compaction merges into one.
fn a_compactable_table(root: &Path) {
    commit(root, 0, &create(Metadata::new("orders", SCHEMA.to_string(), 0))).expect("creating");
    let mut adds = Vec::new();
    for i in 0..8_u64 {
        let name = format!("part-{i:04}.parquet");
        let batch = RecordBatch::try_new(
            arrow_schema(),
            vec![Arc::new(Int64Array::from((0..100_i64).collect::<Vec<i64>>()))],
        )
        .expect("a batch");
        let report = write_parquet(
            root,
            &name,
            &batch,
            Lsn::new(i * 100 + 100),
            WriterConfig::default(),
        )
        .expect("writing");
        adds.push(Action::Add(AddFile::with_rows(name, report.bytes, 0, 100)));
    }
    commit(root, 1, &adds).expect("publishing");
}

fn retiring_next_tick(leak_ticks: u64) -> MaintenancePolicy {
    let mut policy = MaintenancePolicy {
        compact_every: 1,
        orphan_sweep_every: 0,
        orphans: OrphanPolicy { min_age_ticks: 0 },
        ..MaintenancePolicy::default()
    };
    policy.retention.grace_ticks = 1;
    policy.retention.leak_ticks = leak_ticks;
    policy
}

#[test]
fn a_leaked_lease_does_not_stop_reclamation_for_ever() {
    // The comment above `retire_due` promised a backstop *"for the case where a lease is
    // leaked and never released, because a registry with a leak and no backstop reclaims
    // nothing for ever"*, and then wrote `old_enough && unreachable`. Under an `and` a leaked
    // lease held every merge input on disk for ever and every entry in `pending` in memory for
    // ever, growing by one per merge --- both copies of every compacted partition, and no
    // report saying why.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    a_compactable_table(root);

    let leases = Arc::new(Leases::new());
    // Never dropped. That is the leak: a reader that announced itself and whose exit was
    // never announced, which is what a panicking handler or a dropped connection leaves.
    let _leaked = leases.pin();
    let mut maintainer = Maintainer::new(retiring_next_tick(4)).watching(Arc::clone(&leases));

    assert!(!maintainer.tick(root).expect("a tick").merged.is_empty());

    let mut fired = Vec::new();
    for _ in 0..8 {
        fired.extend(maintainer.tick(root).expect("a tick").presumed_leaked);
    }

    assert!(
        !root.join("part-0000.parquet").exists(),
        "one leaked lease kept both copies of the partition on disk for ever"
    );
    assert_eq!(
        maintainer.awaiting_retirement(),
        0,
        "and kept the queue that names them in memory for ever"
    );
    assert!(
        fired.iter().any(|why| why.contains("backstop")),
        "the backstop fired without saying it had --- a backstop firing means a lease leaked, \
         which is a defect nothing else would surface: {fired:?}"
    );
}

#[test]
fn a_reader_inside_the_backstop_is_still_waited_for() {
    // The control, and the one that matters. A backstop that fires on a live reader is a file
    // vanishing mid-scan, which is worse than the disk it was meant to save.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    a_compactable_table(root);

    let leases = Arc::new(Leases::new());
    let reader = leases.pin();
    let mut maintainer = Maintainer::new(retiring_next_tick(1_000)).watching(Arc::clone(&leases));

    assert!(!maintainer.tick(root).expect("a tick").merged.is_empty());
    for _ in 0..8 {
        let report = maintainer.tick(root).expect("a tick");
        assert!(report.presumed_leaked.is_empty(), "{:?}", report.presumed_leaked);
    }

    assert!(
        root.join("part-0000.parquet").exists(),
        "a reader well inside the backstop had its file deleted underneath it"
    );
    drop(reader);
    assert!(
        !maintainer.tick(root).expect("a tick").files_removed.is_empty(),
        "and once it left, the inputs were still collectable"
    );
}
