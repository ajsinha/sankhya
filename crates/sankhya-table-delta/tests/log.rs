//! Replaying the log.
//!
//! The interesting cases are the ones where order matters, because replay is the only
//! thing standing between a correct file set and one that double-counts.

use sankhya_table_delta::{
    commit, create, live_files, Action, AddFile, CommitError, Metadata, RemoveFile,
};

const SCHEMA: &str = r#"{"type":"struct","fields":[]}"#;

fn add(name: &str) -> Action {
    Action::Add(AddFile::new(name, 100, 0))
}

fn remove(name: &str) -> Action {
    Action::Remove(RemoveFile::rewritten(name, 1))
}

fn root() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp dir")
}

#[test]
fn a_table_with_no_commits_has_no_files_and_no_version() {
    let dir = root();
    let live = live_files(dir.path()).expect("reading");
    assert!(live.files.is_empty());
    assert_eq!(live.version, None);
}

#[test]
fn commits_replay_in_version_order_not_write_order() {
    // Written out of order on purpose. The protocol's zero-padded twenty-digit naming
    // exists so lexical order is version order; a replay that trusted directory order
    // would be at the mercy of the filesystem.
    let dir = root();
    commit(dir.path(), 2, &[remove("a.parquet")]).expect("commit 2");
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");
    commit(dir.path(), 1, &[add("a.parquet"), add("b.parquet")]).expect("commit 1");

    let live = live_files(dir.path()).expect("reading");
    assert_eq!(live.paths(), vec!["b.parquet"]);
    assert_eq!(live.version, Some(2));
}

#[test]
fn a_file_removed_and_added_again_is_live() {
    // Reachable through an ordinary compaction followed by a rebase, so this is a real
    // sequence rather than a curiosity.
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");
    commit(dir.path(), 1, &[add("a.parquet")]).expect("commit 1");
    commit(dir.path(), 2, &[remove("a.parquet")]).expect("commit 2");
    commit(dir.path(), 3, &[add("a.parquet")]).expect("commit 3");

    assert_eq!(
        live_files(dir.path()).expect("reading").paths(),
        vec!["a.parquet"]
    );
}

#[test]
fn a_file_added_twice_is_live_once() {
    // The failure this prevents is arithmetic: a duplicated entry counts every row in
    // that file twice, in every query, silently.
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");
    commit(dir.path(), 1, &[add("a.parquet")]).expect("commit 1");
    commit(dir.path(), 2, &[add("a.parquet")]).expect("commit 2");

    let live = live_files(dir.path()).expect("reading");
    assert_eq!(live.paths(), vec!["a.parquet"]);
    assert_eq!(live.total_bytes(), 100);
}

#[test]
fn committing_a_taken_version_is_refused() {
    // The whole of the protocol's concurrency control. The loser must re-read and
    // rebase, because its decisions were made against a table state that no longer
    // exists -- and for a compaction those decisions are exactly "which files to merge".
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");

    let err = commit(dir.path(), 0, &[add("a.parquet")]).expect_err("a taken version");
    assert!(matches!(err, CommitError::VersionTaken(0)));
    assert!(format!("{err}").contains("rebase"));

    // The original commit is untouched.
    assert!(live_files(dir.path()).expect("reading").files.is_empty());
}

#[test]
fn a_commit_is_never_observed_half_written() {
    // Staged under a temporary name and renamed, so a reader either sees the whole
    // commit or none of it. A partial commit file is a log state the protocol has no
    // way to describe.
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");

    let entries: Vec<String> = std::fs::read_dir(dir.path().join("_delta_log"))
        .expect("listing")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();

    assert_eq!(entries, vec!["00000000000000000000.json"]);
}

#[test]
fn a_malformed_log_is_reported_rather_than_skipped() {
    // A line this crate could not have written means the log is not what it appears to
    // be. Skipping it would silently drop files from the table.
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");
    std::fs::write(
        dir.path()
            .join("_delta_log")
            .join("00000000000000000001.json"),
        "{\"add\": not json}\n",
    )
    .expect("writing a bad commit");

    let err = live_files(dir.path()).expect_err("a malformed log");
    assert!(matches!(err, CommitError::Malformed { version: 1, .. }));
}

#[test]
fn the_live_set_totals_only_live_files() {
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");
    commit(
        dir.path(),
        1,
        &[add("a.parquet"), add("b.parquet"), add("c.parquet")],
    )
    .expect("commit 1");
    commit(
        dir.path(),
        2,
        &[
            Action::Add(AddFile::new("merged.parquet", 300, 1)),
            remove("a.parquet"),
            remove("b.parquet"),
            remove("c.parquet"),
        ],
    )
    .expect("commit 2");

    let live = live_files(dir.path()).expect("reading");
    assert_eq!(live.paths(), vec!["merged.parquet"]);
    assert_eq!(live.total_bytes(), 300);
}
