//! Publishing when something else is committing to the same log.
//!
//! These properties moved here with the logic. They were unit tests over a closure inside
//! the ingest pipeline; the closure is gone, because rebasing is publication behaviour and
//! `sankhya-publish` is the one library for that. Testing them against real logs rather than
//! a stubbed committer is a gain: a stub agrees with whatever the test believes the log
//! does, and the whole point is what the log actually does.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Date32Array, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_publish::Publication;
use sankhya_table_delta::{commit, live_files, Action, AddFile};
use sankhya_types::Lsn;
use std::sync::Arc;

const FIRST_DAY: i32 = 19_723;

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

fn published_at(root: &std::path::Path) -> Option<u64> {
    live_files(root).ok().and_then(|set| set.version)
}

#[test]
fn a_free_version_commits_without_rebasing() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    let rebased = publication
        .append_rebasing(1, 4, "a.parquet", &batch(0, 10), Lsn::new(1))
        .expect("committed");
    assert_eq!(rebased.version, 1);
    assert_eq!(rebased.retries, 0);
}

#[test]
fn a_version_taken_by_another_committer_moves_on_and_reports_the_retry() {
    // The ordinary case: maintenance committed between two publishes. Failing here would
    // mean a compaction can stop capture, which inverts the ordering rule — the source
    // outranks maintenance, always.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    // Something else takes version 1 first.
    commit(
        &root,
        1,
        &[Action::Add(AddFile::with_rows("elsewhere.parquet", 1, 0, 1))],
    )
    .expect("the other committer");

    let rebased = publication
        .append_rebasing(1, 4, "a.parquet", &batch(0, 10), Lsn::new(1))
        .expect("committed after a rebase");
    assert_eq!(rebased.version, 2, "it took the next free version");
    assert_eq!(rebased.retries, 1);
    assert_eq!(published_at(&root), Some(2));
}

#[test]
fn the_files_are_written_once_however_many_rebases_it_takes() {
    // Nothing about a file depends on the version, so rewriting per attempt would multiply
    // the work by the contention.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");
    for version in 1..4 {
        commit(
            &root,
            version,
            &[Action::Add(AddFile::with_rows("elsewhere.parquet", 1, 0, 1))],
        )
        .expect("the other committer");
    }

    let rebased = publication
        .append_rebasing(1, 8, "a.parquet", &batch(0, 10), Lsn::new(1))
        .expect("committed");
    assert!(rebased.retries >= 1, "{rebased:?}");

    // One partition, one file, regardless of how many versions were tried.
    let files: Vec<_> = std::fs::read_dir(root.join("sank_data_date=2024-01-01"))
        .expect("the partition directory")
        .flatten()
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
}

#[test]
fn a_runaway_committer_gives_a_diagnosable_failure_rather_than_a_hang() {
    // The bound. Without it this loops for ever, and a pipeline that appears to hang is far
    // harder to diagnose than one that says what it could not do.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");
    for version in 1..6 {
        commit(
            &root,
            version,
            &[Action::Add(AddFile::with_rows("elsewhere.parquet", 1, 0, 1))],
        )
        .expect("the other committer");
    }

    // One attempt against a version that is taken, and no room to move.
    let refused = publication
        .append_rebasing(1, 1, "a.parquet", &batch(0, 10), Lsn::new(1))
        .expect_err("it must give up rather than loop");
    assert!(
        format!("{refused}").contains("faster than this writer can follow"),
        "{refused}"
    );
}

#[test]
fn a_failure_that_is_not_a_version_race_is_not_retried() {
    // Rebasing helps with contention and nothing else. Retrying an unwritable log sixteen
    // times turns one error into sixteen and reports the last — and the caller is told the
    // table is contended when in fact the disk refused it.
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    // The log directory refuses writes for a reason no later version fixes.
    let log = root.join("_delta_log");
    let mut permissions = std::fs::metadata(&log).expect("the log directory").permissions();
    permissions.set_mode(0o500);
    std::fs::set_permissions(&log, permissions).expect("making it read-only");

    let refused = publication
        .append_rebasing(1, 16, "a.parquet", &batch(0, 10), Lsn::new(1))
        .expect_err("an unwritable log must fail");

    // Restore before asserting, so a failure here does not leave an undeletable directory.
    let mut permissions = std::fs::metadata(&log).expect("the log directory").permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&log, permissions).expect("restoring");

    assert!(
        !format!("{refused}").contains("faster than this writer can follow"),
        "an unwritable log was reported as contention after sixteen retries: {refused}"
    );
}
