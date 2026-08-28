//! Deliberately invalid logs, for the tests that must prove they are refused.
//!
//! # Why this is product code and not test code
//!
//! A reader that rejects a malformed log needs a malformed log to be pointed at, and the
//! writers cannot produce one --- that is what makes them writers. Every test that needed
//! such a state therefore built the log by hand: metadata, add actions, versions, all
//! assembled in the test file.
//!
//! That is how a test comes to know the storage layout, and knowing it is how it comes to
//! encode the *old* one. The rule in this repository is now absolute: **no server-side
//! functionality in test code, ever.** A test that needs a capability gets it from the crate
//! that owns the capability, in one line.
//!
//! So the ability to write a broken log lives here, beside the code that writes correct
//! ones, named for exactly how it is broken. Each function documents the defect it injects
//! and the reader behaviour it exists to prove, so a reader of the test can see what is being
//! tested without reconstructing the Delta protocol in their head.
//!
//! Nothing here is reachable from a running server: these functions write states the product
//! refuses to produce, and they exist so that refusal can be tested.

use crate::log::{commit, create, Action, AddFile, CommitError, Metadata};
use std::path::Path;

/// Create a table whose log declares `schema`, without writing any data.
///
/// # Errors
///
/// Returns the commit error if version zero is already taken. On success, the version written.
pub fn create_table(
    table_root: &Path,
    name: &str,
    schema: &str,
) -> Result<u64, CommitError> {
    commit(
        table_root,
        0,
        &create(Metadata::new(name, schema.to_string(), 0)),
    )
}

/// Commit an add action carrying **no row count**.
///
/// # The defect this injects
///
/// A `numRecords` a reader cannot see. Treating an absent count as zero tells the optimizer
/// the table is empty, which produces a *wrong* plan rather than a slow one --- so the read
/// path must refuse the file rather than assume a number for it.
///
/// # Errors
///
/// Returns the commit error if `version` is already taken.
pub fn add_without_row_count(
    table_root: &Path,
    version: u64,
    file_name: &str,
    bytes: u64,
) -> Result<u64, CommitError> {
    commit(
        table_root,
        version,
        &[Action::Add(AddFile::new(file_name, bytes, 0))],
    )
}

/// Commit an add action for a file that is **not on disk**.
///
/// # The defect this injects
///
/// A log entry pointing at nothing. It is what an interrupted writer would leave if it
/// committed before its data landed, and the read path must fail loudly rather than return
/// short results --- a query missing a file silently is indistinguishable from a query
/// against less data.
///
/// # Errors
///
/// Returns the commit error if `version` is already taken.
pub fn add_naming_a_missing_file(
    table_root: &Path,
    version: u64,
    file_name: &str,
    rows: u64,
) -> Result<u64, CommitError> {
    commit(
        table_root,
        version,
        &[Action::Add(AddFile::with_rows(file_name, 1, 0, rows))],
    )
}

/// Create a table whose log names `files` Parquet files that were never written.
///
/// # The defect this injects
///
/// A log describing a warehouse that does not exist on disk. It is the cheap way to build a
/// table with hundreds of live files for the diagnostics that read *only* the log --- a
/// compaction-debt reading counts what the log says is live, and writing nine hundred real
/// files to test it would take minutes to prove something the log alone decides.
///
/// It is still a malformed table, and it lives here so that a test using it says so.
///
/// # Errors
///
/// Returns the commit error if version zero is already taken.
pub fn table_naming_missing_files(
    table_root: &Path,
    name: &str,
    schema: &str,
    files: usize,
) -> Result<u64, CommitError> {
    let mut actions = create(Metadata::new(name, schema.to_string(), 0));
    for i in 0..files {
        actions.push(Action::Add(AddFile::new(
            format!("part-{i:05}.parquet"),
            1_024,
            0,
        )));
    }
    commit(table_root, 0, &actions)
}

/// Commit adds for a range of files that were never written.
///
/// As [`table_naming_missing_files`], for a table that already exists: the arrivals half of
/// a compaction-debt reading, where what matters is how fast the log grows.
///
/// # Errors
///
/// Returns the commit error if `version` is already taken.
pub fn adds_naming_missing_files(
    table_root: &Path,
    version: u64,
    range: std::ops::Range<usize>,
) -> Result<u64, CommitError> {
    let actions: Vec<Action> = range
        .map(|i| {
            Action::Add(AddFile::new(format!("part-{i:05}.parquet"), 1_024, 0))
        })
        .collect();
    commit(table_root, version, &actions)
}

/// Record, in the log alone, that a range of never-written files became one.
///
/// # The defect this injects
///
/// The *shape* a compaction leaves in the log --- a remove per input and one add --- over
/// files that never existed. Real maintenance cannot produce it here, because there is
/// nothing on disk for it to merge; and a diagnostic that reads live-file counts out of the
/// log does not need there to be.
///
/// # Errors
///
/// Returns the commit error if `version` is already taken.
pub fn replace_missing_files_with_one(
    table_root: &Path,
    version: u64,
    replacing: std::ops::Range<usize>,
    replacement: &str,
    bytes: u64,
) -> Result<u64, CommitError> {
    let mut actions: Vec<Action> = replacing
        .map(|i| {
            Action::Remove(crate::log::RemoveFile::rewritten(
                format!("part-{i:05}.parquet"),
                i64::try_from(version).unwrap_or(0),
            ))
        })
        .collect();
    actions.push(Action::Add(AddFile::new(replacement, bytes, 1)));
    commit(table_root, version, &actions)
}
