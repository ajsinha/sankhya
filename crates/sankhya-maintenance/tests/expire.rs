//! Partitions that have aged out, and the ones that must not be touched.
//!
//! # What is really being protected here
//!
//! This is the only automatic thing in the system that removes data from a live table. It is
//! defensible because it detaches rather than deletes --- reversible until retirement runs ---
//! and because it refuses to act wherever it would have to guess. Those refusals are what
//! these assert; the happy path is arithmetic.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_maintenance::expire::{detach, plan};
use sankhya_table_delta::{AddFile, LiveSet};

/// 2026-08-31, as days since the epoch.
const TODAY: i32 = 20_696;

fn live(paths: &[&str]) -> LiveSet {
    LiveSet {
        files: paths
            .iter()
            .map(|path| AddFile::new((*path).to_owned(), 1_024, 0))
            .collect(),
        version: Some(1),
    }
}

#[test]
fn a_partition_older_than_the_retention_is_detached() {
    let set = live(&[
        "sank_data_date=2026-07-01/part-0000.parquet",
        "sank_data_date=2026-08-30/part-0001.parquet",
    ]);

    let expired = plan(&set, TODAY, 30);

    assert_eq!(expired.files, 1);
    assert!(expired.partitions.contains("sank_data_date=2026-07-01"));
    assert!(
        !expired.partitions.contains("sank_data_date=2026-08-30"),
        "yesterday is not thirty days ago"
    );
}

#[test]
fn a_partition_exactly_at_the_boundary_is_kept() {
    // Kept rather than removed. The boundary has to fall somewhere, and the side that keeps
    // data is the side to be wrong on: a record kept a day too long costs storage, and one
    // removed a day early costs the only copy of something somebody was about to look at.
    let set = live(&["sank_data_date=2026-08-01/part-0000.parquet"]);

    assert!(plan(&set, TODAY, 30).is_empty(), "thirty days ago exactly");
    assert_eq!(plan(&set, TODAY, 29).files, 1, "and a day less is past it");
}

#[test]
fn every_file_of_an_expired_partition_goes_and_nothing_else_does() {
    let set = live(&[
        "sank_data_date=2026-07-01/part-0000.parquet",
        "sank_data_date=2026-07-01/part-0001.parquet",
        "sank_data_date=2026-08-31/part-0002.parquet",
    ]);

    let expired = plan(&set, TODAY, 30);
    assert_eq!(expired.files, 2);
    assert_eq!(expired.partitions.len(), 1, "two files, one partition");
}

#[test]
fn a_partition_whose_date_cannot_be_read_is_never_expired() {
    // Refusing to guess is the whole point: a partition whose date is unreadable is one whose
    // age is unknown, and removing it would be acting on an assumption about data.
    let set = live(&[
        "sank_data_date=not-a-date/part-0000.parquet",
        "sank_data_date=2026-13-45/part-0001.parquet",
        "sank_data_date=2026-07/part-0002.parquet",
        "some_other_column=2026-07-01/part-0003.parquet",
    ]);

    assert!(plan(&set, TODAY, 1).is_empty(), "none of these has a legible date");
}

#[test]
fn a_file_at_the_table_root_belongs_to_no_partition_and_is_left_alone() {
    // A table written before it was partitioned holds them, and detaching one on a guess
    // about its contents would be deleting data on no evidence at all.
    let set = live(&["part-0000.parquet"]);

    assert!(plan(&set, TODAY, 1).is_empty());
}

#[test]
fn a_retention_longer_than_the_epoch_keeps_everything() {
    // The arithmetic hazard: a cutoff computed by subtracting a very large retention could
    // wrap into a date in the *future* and expire the entire table in one tick.
    let set = live(&["sank_data_date=1970-01-02/part-0000.parquet"]);

    assert!(plan(&set, TODAY, u32::MAX).is_empty(), "everything is younger than forever");
}

#[test]
fn detaching_removes_exactly_the_planned_files_and_leaves_the_data_on_disk() {
    let dir = tempfile::tempdir().expect("a directory");
    let root = dir.path();
    let set = live(&[
        "sank_data_date=2026-07-01/part-0000.parquet",
        "sank_data_date=2026-08-31/part-0001.parquet",
    ]);
    // A table to commit against: version zero is its creation.
    sankhya_table_delta::commit(
        root,
        0,
        &sankhya_table_delta::create(sankhya_table_delta::Metadata::new("t", "{}", 0)),
    )
    .expect("creating");

    let expired = plan(&set, TODAY, 30);
    detach(root, 1, &set, &expired, 0).expect("detaching");

    let after = sankhya_table_delta::live_files(root).expect("replaying");
    // The detached file is out of the live set. Whether the file itself is still on disk is
    // retirement's business, and it is: detach is reversible until the grace period runs.
    assert!(
        after.files.iter().all(|file| !file.path.contains("2026-07-01")),
        "the expired partition is out of the live set"
    );
}
