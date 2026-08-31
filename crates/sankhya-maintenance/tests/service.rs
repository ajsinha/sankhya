//! The maintenance thread, and the two kinds of file it removes.
//!
//! Retirement and orphan collection look similar from a distance --- both delete a Parquet
//! file nothing is reading --- and they are different mechanisms protecting against different
//! mistakes. These tests exist to keep them distinguishable:
//!
//! - **Retirement** removes an input a merge *replaced*. The file was live, its replacement
//!   is live now, and the grace period exists because a reader that listed before the merge
//!   may still open what it listed.
//! - **Orphan collection** removes a file the log has *never* named. That happens when a
//!   merge writes its output and then loses the race to commit it: the writer took the
//!   version, maintenance must re-plan, and the file it already wrote is left behind.
//!
//! Nothing was collecting the second kind. One is produced per lost race, and they
//! accumulate for the life of the warehouse.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_clone::{Lineage, Lineages};
use sankhya_maintenance::{Maintainer, MaintenancePolicy, OrphanPolicy};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use std::path::Path;

const SCHEMA: &str = r#"{"type":"struct","fields":[]}"#;

/// A table whose log names `live`, with `stray` also sitting on disk.
fn table_with_a_stray_file(root: &Path, live: &str, stray: &str) {
    std::fs::create_dir_all(root).expect("the table directory");
    commit(root, 0, &create(Metadata::new("t", SCHEMA.to_string(), 0))).expect("creating");

    // Both files exist on disk. Only one of them is in the log, which is the entire
    // difference between a live file and an orphan.
    std::fs::write(root.join(live), vec![b'x'; 512]).expect("the live file");
    std::fs::write(root.join(stray), vec![b'x'; 256]).expect("the stray file");

    commit(
        root,
        1,
        &[Action::Add(AddFile::with_rows(live, 512, 0, 1))],
    )
    .expect("publishing");
}

/// A policy that sweeps on every tick and treats every unreferenced file as old enough.
fn sweeping_at_once() -> MaintenancePolicy {
    MaintenancePolicy {
        orphan_sweep_every: 1,
        // Zero **only here**. The shipping default is a week, and that threshold is the one
        // thing standing between this sweep and a file a committer is in the middle of
        // writing --- so a test that sets it to zero must say why, or it reads as a
        // recommendation.
        orphans: OrphanPolicy { min_age_ticks: 0 },
        ..MaintenancePolicy::default()
    }
}

#[test]
fn a_file_the_log_never_named_is_collected() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    table_with_a_stray_file(root, "part-0000.parquet", "compacted-0007.parquet");

    let mut maintainer = Maintainer::new(sweeping_at_once());
    let report = maintainer.tick(root).expect("a tick");

    assert!(
        !root.join("compacted-0007.parquet").exists(),
        "a merged file whose commit lost the version race is left on disk with nothing \
         naming it, and it is collected --- otherwise every lost race costs a file forever"
    );
    assert!(report.bytes_reclaimed >= 256, "the bytes are reported: {report:?}");
}

#[test]
fn a_file_the_log_names_is_never_collected() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    table_with_a_stray_file(root, "part-0000.parquet", "compacted-0007.parquet");

    let mut maintainer = Maintainer::new(sweeping_at_once());
    maintainer.tick(root).expect("a tick");

    assert!(
        root.join("part-0000.parquet").exists(),
        "the sweep removed a live file, which is data loss and not tidying"
    );
}

#[test]
fn the_log_itself_is_never_swept() {
    // It is not data. A sweep that reached it would delete the table rather than tidy it,
    // and the table would be gone in the one way no backup notices: cleanly.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    table_with_a_stray_file(root, "part-0000.parquet", "compacted-0007.parquet");

    let mut maintainer = Maintainer::new(sweeping_at_once());
    maintainer.tick(root).expect("a tick");

    assert!(
        root.join("_delta_log").is_dir(),
        "the log is not an orphan however little the live set refers to it"
    );
    assert!(
        sankhya_table_delta::live_files(root)
            .expect("the log still replays")
            .files
            .len()
            == 1,
        "the table still reads after a sweep"
    );
}

#[test]
fn a_recent_unreferenced_file_is_left_alone() {
    // The case the age threshold exists for: a file that is not in the live set because the
    // commit naming it has not happened *yet*. From a directory listing that is
    // indistinguishable from garbage, so the sweep must not guess.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    table_with_a_stray_file(root, "part-0000.parquet", "being-written.parquet");

    let mut maintainer = Maintainer::new(MaintenancePolicy {
        orphan_sweep_every: 1,
        // The shipping default: a week.
        orphans: OrphanPolicy::default(),
        ..MaintenancePolicy::default()
    });
    maintainer.tick(root).expect("a tick");

    assert!(
        root.join("being-written.parquet").exists(),
        "a file written seconds ago may be mid-commit, and deleting it would remove data \
         from under the process that just wrote it"
    );
}

#[test]
fn sweeping_is_rarer_than_compacting() {
    // A sweep walks the whole table directory, and what it collects arrives one lost race at
    // a time. Running it every tick would spend the maintenance budget on a listing.
    let policy = MaintenancePolicy::default();
    assert!(
        policy.orphan_sweep_every > 1,
        "the default sweeps every {} tick(s)",
        policy.orphan_sweep_every
    );
}

#[test]
fn compaction_can_be_slowed_without_stopping_it() {
    // Every cadence is a setting, and a setting nobody can observe taking effect is a
    // setting somebody will assume works. Compacting every fourth tick means three ticks in
    // four do no merging --- which is the point, and is also indistinguishable from broken
    // unless it is asserted.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    table_with_a_stray_file(root, "part-0000.parquet", "compacted-0007.parquet");

    let mut maintainer = Maintainer::new(MaintenancePolicy {
        compact_every: 4,
        orphan_sweep_every: 0,
        ..MaintenancePolicy::default()
    });
    for _ in 0..3 {
        let report = maintainer.tick(root).expect("a tick");
        assert!(
            report.merged.is_empty(),
            "a tick between compaction passes must not merge"
        );
    }
}

#[test]
fn a_zero_sweep_interval_disables_sweeping_rather_than_dividing_by_it() {
    // `tick % 0` panics. A setting whose off value crashes the maintenance thread is worse
    // than one that cannot be turned off at all, because it fails at whatever hour the
    // operator set it and takes the thread down with it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    table_with_a_stray_file(root, "part-0000.parquet", "compacted-0007.parquet");

    let mut maintainer = Maintainer::new(MaintenancePolicy {
        orphan_sweep_every: 0,
        orphans: OrphanPolicy { min_age_ticks: 0 },
        ..MaintenancePolicy::default()
    });
    maintainer.tick(root).expect("a tick with sweeping off");

    assert!(
        root.join("compacted-0007.parquet").exists(),
        "sweeping is off, so the orphan stays"
    );
}

#[test]
fn a_new_policy_takes_effect_without_losing_what_is_in_flight() {
    // The hazard in live reconfiguration is not that the new value fails to apply. It is
    // that applying it discards state: a merge waiting out its grace period has already
    // happened, and a maintainer that forgot it would leave those inputs on disk with
    // nothing left that knows to retire them. A reload must not leak files as the price of
    // taking effect.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    table_with_a_stray_file(root, "part-0000.parquet", "compacted-0007.parquet");

    let mut maintainer = Maintainer::new(MaintenancePolicy {
        orphan_sweep_every: 0,
        ..MaintenancePolicy::default()
    });
    maintainer.tick(root).expect("a tick");
    let waiting = maintainer.awaiting_retirement();

    maintainer.reconfigure(sweeping_at_once());
    assert_eq!(
        maintainer.awaiting_retirement(),
        waiting,
        "reconfiguring dropped the retirement queue, which strands every input it held"
    );

    // And the new policy is the one in force: sweeping was off, and now it is not.
    maintainer.tick(root).expect("a tick under the new policy");
    assert!(
        !root.join("compacted-0007.parquet").exists(),
        "the reloaded policy did not take effect on the next tick"
    );
}

#[test]
fn the_running_thread_reports_the_policy_it_is_running_under() {
    // An operator who cannot read back what a reload did has to infer it from behaviour,
    // and the behaviour of maintenance is deliberately quiet.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let handle = sankhya_maintenance::spawn_maintenance(
        vec![dir.path().to_path_buf()],
        MaintenancePolicy::default(),
    );
    assert_eq!(handle.policy().compact_every, 1);

    handle.reconfigure(MaintenancePolicy {
        compact_every: 9,
        ..MaintenancePolicy::default()
    });
    assert_eq!(
        handle.policy().compact_every,
        9,
        "the handle must report what it was last given, or a reload cannot be confirmed"
    );
    handle.stop();
}

/// A table whose version 1 names `first`, and whose version 2 replaces it with `second`.
///
/// Both files stay on disk. After version 2 the log's *live* set names only `second`, so
/// `first` is exactly what an origin's sweeper sees as debris — and exactly what a clone taken
/// at version 1 still reads.
fn table_that_moved_on(root: &Path, first: &str, second: &str) {
    std::fs::create_dir_all(root).expect("the table directory");
    commit(root, 0, &create(Metadata::new("t", SCHEMA.to_string(), 0))).expect("creating");

    std::fs::write(root.join(first), vec![b'x'; 512]).expect("the first file");
    commit(root, 1, &[Action::Add(AddFile::with_rows(first, 512, 0, 1))]).expect("v1");

    std::fs::write(root.join(second), vec![b'x'; 512]).expect("the second file");
    commit(
        root,
        2,
        &[
            Action::Remove(sankhya_table_delta::RemoveFile::rewritten(first, 0)),
            Action::Add(AddFile::with_rows(second, 512, 0, 1)),
        ],
    )
    .expect("v2");
}

#[test]
fn a_file_only_a_clone_still_names_is_not_swept_from_its_origin() {
    // The failure ADR-0016 exists to prevent, at the exact place it would happen. From the
    // origin's point of view a file only the clone still names is indistinguishable from
    // debris: on disk, not in the live set, older than the threshold. Nothing would fail and
    // no query would error; the clone would simply be missing rows the next time anybody read
    // that range of it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("entries");
    table_that_moved_on(&root, "part-0000.parquet", "part-0001.parquet");

    let mut clones = Lineages::new();
    clones.record("staging", Lineage::new("entries", 1, 0));

    let mut maintainer = Maintainer::new(sweeping_at_once()).among(clones);
    maintainer.tick(&root).expect("a tick");

    assert!(
        root.join("part-0000.parquet").exists(),
        "the origin swept a file its clone is the only remaining reader of"
    );
    assert!(root.join("part-0001.parquet").exists(), "and the live file is untouched");
}

#[test]
fn the_same_file_is_swept_when_nothing_was_cloned_from_the_table() {
    // The control, and the point of it: without it the test above would pass just as happily
    // against a maintainer that had stopped sweeping altogether.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("entries");
    table_that_moved_on(&root, "part-0000.parquet", "part-0001.parquet");

    let mut maintainer = Maintainer::new(sweeping_at_once());
    maintainer.tick(&root).expect("a tick");

    assert!(
        !root.join("part-0000.parquet").exists(),
        "a superseded file nobody reads is debris, and a warehouse with no clones must still \
         reclaim it"
    );
}

#[test]
fn a_clone_taken_at_a_later_version_does_not_pin_what_that_version_had_dropped() {
    // The pin is a version, not the table. A clone taken at version 2 reads `part-0001`, and
    // `part-0000` is no more reachable from it than from the origin — so keeping it would be
    // keeping a file on the strength of a clone that never read it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("entries");
    table_that_moved_on(&root, "part-0000.parquet", "part-0001.parquet");

    let mut clones = Lineages::new();
    clones.record("staging", Lineage::new("entries", 2, 0));

    let mut maintainer = Maintainer::new(sweeping_at_once()).among(clones);
    maintainer.tick(&root).expect("a tick");

    assert!(
        !root.join("part-0000.parquet").exists(),
        "version 2 does not name it, so a clone of version 2 does not read it"
    );
    assert!(root.join("part-0001.parquet").exists());
}

#[test]
fn a_clone_of_a_different_table_pins_nothing_here() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("entries");
    table_that_moved_on(&root, "part-0000.parquet", "part-0001.parquet");

    let mut clones = Lineages::new();
    clones.record("scratch", Lineage::new("somewhere-else", 1, 0));

    let mut maintainer = Maintainer::new(sweeping_at_once()).among(clones);
    maintainer.tick(&root).expect("a tick");

    assert!(!root.join("part-0000.parquet").exists());
}
