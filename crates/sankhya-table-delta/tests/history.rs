//! Reading a table's history out of its log.
//!
//! # Why these are unit tests and not only a server test
//!
//! `SHOW HISTORY OF` is a *reading* of the log, and the two readings that matter most are the
//! ones a server test cannot easily stage: a commit that touched no file, and a compaction.
//! Both were wrong when this was written --- a metadata commit reported as having happened in
//! 1970, and a compaction reported as a data change --- and both are the same failure, a
//! summary that is confidently wrong rather than silent.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_table_delta::history::{describe, history};
use sankhya_table_delta::{commit, create, Action, AddFile, FileStatistics, Metadata, RemoveFile};

const SCHEMA: &str = r#"{"type":"struct","fields":[]}"#;

fn table() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("creating");
    dir
}

#[test]
fn the_creating_commit_names_no_files_and_carries_no_time() {
    // The creating commit touches no file, so there is nothing to take a time from. Reporting
    // zero would put every table's creation on the first of January 1970 and present it as a
    // fact somebody could act on.
    let dir = table();
    let changes = history(dir.path()).expect("a history");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].version, 0);
    assert_eq!(changes[0].at, None, "a time nobody recorded is not zero");
    assert!(changes[0].declared_schema);
    assert_eq!(describe(&changes[0]), "created");
}

#[test]
fn an_append_is_an_append_and_declares_that_it_changed_data() {
    let dir = table();
    commit(dir.path(), 1, &[Action::Add(AddFile::new("part-0000.parquet", 100, 1_700))])
        .expect("appending");

    let changes = history(dir.path()).expect("a history");
    let appended = &changes[1];
    assert_eq!(describe(appended), "appended");
    assert_eq!(appended.added, 1);
    assert_eq!(appended.removed, 0);
    assert_eq!(appended.bytes_added, 100);
    assert_eq!(appended.at, Some(1_700), "taken from the file it added");
    assert!(appended.changed_data);
}

#[test]
fn a_compaction_is_named_a_compaction_and_declares_that_no_rows_changed() {
    // The reading this whole column exists for. A compaction replaces files and changes not one
    // row --- and calling that a change tells somebody their table moved when maintenance ran,
    // which is both false and the fastest way to make them stop trusting the column.
    let dir = table();
    commit(
        dir.path(),
        1,
        &[
            Action::Add(AddFile::new("part-0000.parquet", 100, 1)),
            Action::Add(AddFile::new("part-0001.parquet", 100, 2)),
        ],
    )
    .expect("appending");
    let statistics = FileStatistics::default();
    commit(
        dir.path(),
        2,
        &[
            Action::Remove(RemoveFile::rewritten("part-0000.parquet", 3)),
            Action::Remove(RemoveFile::rewritten("part-0001.parquet", 3)),
            Action::Add(AddFile::rewritten("part-merged.parquet", 200, 3, &statistics)),
        ],
    )
    .expect("compacting");

    let changes = history(dir.path()).expect("a history");
    let compacted = &changes[2];
    assert!(
        !compacted.changed_data,
        "both halves of a compaction must declare that no rows changed"
    );
    assert_eq!(describe(compacted), "compacted");
    assert_eq!(compacted.added, 1);
    assert_eq!(compacted.removed, 2);
}

#[test]
fn a_rewrite_that_did_change_rows_is_not_called_a_compaction() {
    // The distinction is the writer's declaration, not the shape of the commit. A merge that
    // replaced files *and* changed rows has the same file counts as a compaction, and reporting
    // it as one would hide a real change behind a word that means "nothing happened".
    let dir = table();
    commit(dir.path(), 1, &[Action::Add(AddFile::new("part-0000.parquet", 100, 1))])
        .expect("appending");
    commit(
        dir.path(),
        2,
        &[
            Action::Remove(RemoveFile::deleted("part-0000.parquet", 3)),
            Action::Add(AddFile::new("part-0001.parquet", 90, 3)),
        ],
    )
    .expect("rewriting");

    let changes = history(dir.path()).expect("a history");
    assert!(changes[2].changed_data);
    assert_eq!(describe(&changes[2]), "rewritten");
}

#[test]
fn a_commit_is_one_row_however_many_actions_it_holds() {
    // A reader thinks of a commit as one event. Rendering an action per row would report a
    // compaction of a thousand files as a thousand versions.
    let dir = table();
    let files: Vec<Action> = (0..8)
        .map(|i| Action::Add(AddFile::new(format!("part-{i:04}.parquet"), 10, i)))
        .collect();
    commit(dir.path(), 1, &files).expect("appending");

    let changes = history(dir.path()).expect("a history");
    assert_eq!(changes.len(), 2, "two commits, whatever they contained");
    assert_eq!(changes[1].added, 8);
    assert_eq!(changes[1].bytes_added, 80);
    assert_eq!(changes[1].at, Some(7), "the newest file the commit touched");
}

#[test]
fn an_unreadable_log_is_reported_rather_than_summarised_as_no_history() {
    // "This table has no history" and "I could not read its history" lead to opposite actions,
    // and only one of them is recoverable.
    let dir = table();
    std::fs::write(dir.path().join("_delta_log").join("00000000000000000001.json"), "{ not json")
        .expect("corrupting");
    assert!(history(dir.path()).is_err());
}

#[test]
fn a_compaction_between_two_versions_contributes_no_rows_and_is_still_reported() {
    // **The reason `M20` was deferred, and the reason it could be built.** The log records
    // file-level adds and removes, so a compaction --- which rewrites files and changes no row
    // --- would show as a total replacement. A diff that reported it as a change would be worse
    // than no diff, because it looks like an answer.
    //
    // Both halves are asserted, and the second is the one that is easy to leave out: filtering
    // the compaction out of the arithmetic is correct, and *silence* about it is a second
    // misleading answer. A reader told nothing changed over a range in which every file was
    // rewritten is left wondering why the storage looks nothing like it did.
    let dir = table();
    let statistics = FileStatistics::new(100);
    commit(
        dir.path(),
        1,
        &[
            Action::Add(AddFile::with_statistics("part-0000.parquet", 100, 1, &statistics)),
            Action::Add(AddFile::with_statistics("part-0001.parquet", 100, 2, &statistics)),
        ],
    )
    .expect("appending");
    let merged = FileStatistics::new(200);
    commit(
        dir.path(),
        2,
        &[
            Action::Remove(RemoveFile::rewritten("part-0000.parquet", 3)),
            Action::Remove(RemoveFile::rewritten("part-0001.parquet", 3)),
            Action::Add(AddFile::rewritten("part-merged.parquet", 200, 3, &merged)),
        ],
    )
    .expect("compacting");

    let over_the_compaction =
        sankhya_table_delta::difference(dir.path(), 1, 2).expect("a difference");
    assert_eq!(
        over_the_compaction.commits, 0,
        "no commit in this range changed a row, and the compaction must not be counted as one"
    );
    assert_eq!(over_the_compaction.rows_added, 0, "a compaction adds no rows");
    assert_eq!(over_the_compaction.rows_removed, 0, "and removes none");
    assert_eq!(
        over_the_compaction.files_added, 0,
        "and its file counts stay out of the arithmetic too --- reporting one file added is the \
         same wrong answer one level down"
    );
    assert_eq!(
        over_the_compaction.compactions, 1,
        "and it is *named*, because a number withheld to avoid confusing somebody is a number \
         they will need and will then get somewhere less careful"
    );

    // And a range that spans the append still counts the append, which is what makes the
    // assertions above about the compaction rather than about an empty implementation.
    let including_the_append =
        sankhya_table_delta::difference(dir.path(), 0, 2).expect("a difference");
    assert_eq!(including_the_append.commits, 1, "the append, and only the append");
    assert_eq!(including_the_append.rows_added, 200, "two files of a hundred");
    assert_eq!(including_the_append.compactions, 1, "with the compaction still named beside it");
}

#[test]
fn rows_removed_are_the_rows_that_were_in_the_file() {
    // A removal names a path and says nothing about what was in it, so the count is looked up
    // from the commit that added it. Without that, `rows_removed` would be zero for every
    // deletion and a diff would report a table emptying as no change at all.
    let dir = table();
    let hundred = FileStatistics::new(100);
    commit(
        dir.path(),
        1,
        &[Action::Add(AddFile::with_statistics("part-0000.parquet", 100, 1, &hundred))],
    )
    .expect("appending");
    commit(
        dir.path(),
        2,
        &[Action::Remove(RemoveFile::deleted("part-0000.parquet", 3))],
    )
    .expect("deleting");

    let difference = sankhya_table_delta::difference(dir.path(), 1, 2).expect("a difference");
    assert_eq!(difference.commits, 1);
    assert_eq!(difference.files_removed, 1);
    assert_eq!(
        difference.rows_removed, 100,
        "the rows the file held, from the commit that added it --- not zero, which is what a \
         removal action says on its own"
    );
    assert_eq!(difference.rows_added, 0);
}
