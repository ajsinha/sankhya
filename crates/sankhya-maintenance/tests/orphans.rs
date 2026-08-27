//! Sweeping up, and everything it refuses to sweep.
//!
//! The interesting assertions are the refusals. Reclaiming a file too late costs disk;
//! reclaiming one too early costs data that was never recorded as lost, or a query that
//! fails on a file that is not there.

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

use sankhya_maintenance::{plan_orphan_cleanup, sweep, FileOnDisk, OrphanPolicy};
use std::collections::BTreeSet;

fn on_disk(entries: &[(&str, u64)]) -> Vec<FileOnDisk> {
    entries
        .iter()
        .map(|(name, age)| FileOnDisk {
            name: (*name).to_string(),
            bytes: 1_000,
            age_ticks: *age,
        })
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_string()).collect()
}

fn policy() -> OrphanPolicy {
    OrphanPolicy { min_age_ticks: 100 }
}

#[test]
fn an_old_unreferenced_file_is_reclaimed() {
    // What the sweep exists for: a compaction wrote its output and the process died
    // before committing it, so the file is complete, correct, and referenced by nothing.
    let plan = plan_orphan_cleanup(
        &on_disk(&[("live.parquet", 500), ("orphan.parquet", 500)]),
        &set(&["live.parquet"]),
        &set(&["live.parquet"]),
        &policy(),
    );

    assert_eq!(plan.remove, vec!["orphan.parquet"]);
    assert_eq!(plan.bytes_reclaimable, 1_000);
}

#[test]
fn a_recent_unreferenced_file_is_left_alone() {
    // The defence that matters. An uncommitted file and an orphan are indistinguishable
    // -- both on disk, both in no log -- and the only thing separating them is time.
    let plan = plan_orphan_cleanup(
        &on_disk(&[("just-written.parquet", 3)]),
        &BTreeSet::new(),
        &BTreeSet::new(),
        &policy(),
    );

    assert!(plan.remove.is_empty());
    assert_eq!(plan.retained.len(), 1);
    assert!(plan.retained[0].1.contains("indistinguishable"));
}

#[test]
fn the_threshold_boundary_is_inclusive() {
    // A file exactly at the threshold is old enough. Excluding it would leave every
    // orphan needing one more tick than the policy says, which is a policy nobody
    // configured.
    let plan = plan_orphan_cleanup(
        &on_disk(&[("orphan.parquet", 100)]),
        &BTreeSet::new(),
        &BTreeSet::new(),
        &policy(),
    );
    assert_eq!(plan.remove, vec!["orphan.parquet"]);
}

#[test]
fn a_file_a_retained_snapshot_reaches_is_kept_however_old() {
    // Compaction's inputs are exactly this shape: superseded, absent from the live set,
    // and still the correct answer for a query pinned before the merge. Sweeping them on
    // age alone breaks time travel silently, and only for queries that reach back far
    // enough to notice.
    let plan = plan_orphan_cleanup(
        &on_disk(&[("superseded.parquet", 1_000_000)]),
        &set(&["merged.parquet"]),
        &set(&["merged.parquet", "superseded.parquet"]),
        &policy(),
    );

    assert!(plan.remove.is_empty());
    assert!(plan.retained[0].1.contains("time travel"));
}

#[test]
fn the_log_is_never_swept() {
    // The log is not data. Sweeping it deletes the table.
    let plan = plan_orphan_cleanup(
        &on_disk(&[
            ("_delta_log", 1_000_000),
            ("_last_checkpoint", 1_000_000),
            ("_delta_log/00000000000000000000.json", 1_000_000),
        ]),
        &BTreeSet::new(),
        &BTreeSet::new(),
        &policy(),
    );

    assert!(plan.remove.is_empty(), "{:?}", plan.remove);
    assert!(plan.retained.is_empty(), "the log is not even a candidate");
}

#[test]
fn a_live_file_is_never_a_candidate_at_any_age() {
    let plan = plan_orphan_cleanup(
        &on_disk(&[("live.parquet", u64::MAX)]),
        &set(&["live.parquet"]),
        &BTreeSet::new(),
        &policy(),
    );
    assert!(plan.remove.is_empty());
    assert!(plan.retained.is_empty(), "a live file is not a candidate");
}

#[test]
fn the_default_threshold_is_far_beyond_any_commit() {
    // Erring high costs storage; erring low costs data. A default that could plausibly
    // be shorter than a stalled commit is the wrong kind of wrong.
    let default = OrphanPolicy::default();
    assert!(
        default.min_age_ticks >= 24 * 60 * 60,
        "a default measured in minutes would race a slow commit"
    );
}

#[test]
fn sweeping_removes_what_the_plan_named_and_nothing_else() {
    let dir = tempfile::tempdir().expect("a temp dir");
    for name in ["live.parquet", "orphan.parquet", "keep.parquet"] {
        std::fs::write(dir.path().join(name), b"xxxxx").expect("writing");
    }

    let plan = plan_orphan_cleanup(
        &on_disk(&[
            ("live.parquet", 500),
            ("orphan.parquet", 500),
            ("keep.parquet", 5),
        ]),
        &set(&["live.parquet"]),
        &BTreeSet::new(),
        &policy(),
    );
    let report = sweep(&plan, dir.path());

    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.bytes_reclaimed, 5);
    assert!(dir.path().join("live.parquet").exists());
    assert!(dir.path().join("keep.parquet").exists());
    assert!(!dir.path().join("orphan.parquet").exists());
}

#[test]
fn a_file_already_gone_counts_as_removed() {
    // Two sweeps overlapping, or a manual cleanup in between. Already gone is the
    // desired state, not a failure.
    let dir = tempfile::tempdir().expect("a temp dir");
    let plan = plan_orphan_cleanup(
        &on_disk(&[("vanished.parquet", 500)]),
        &BTreeSet::new(),
        &BTreeSet::new(),
        &policy(),
    );

    let report = sweep(&plan, dir.path());
    assert_eq!(report.removed.len(), 1);
    assert!(report.failed.is_empty());
}

#[test]
fn one_undeletable_file_does_not_stop_the_sweep() {
    // A directory cannot be removed with remove_file, which stands in here for any
    // per-file failure. The rest must still be reclaimed.
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir(dir.path().join("stubborn.parquet")).expect("creating a directory");
    std::fs::write(dir.path().join("ordinary.parquet"), b"xx").expect("writing");

    let plan = plan_orphan_cleanup(
        &on_disk(&[("stubborn.parquet", 500), ("ordinary.parquet", 500)]),
        &BTreeSet::new(),
        &BTreeSet::new(),
        &policy(),
    );
    let report = sweep(&plan, dir.path());

    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.removed.len(), 1);
    assert!(!dir.path().join("ordinary.parquet").exists());
}
