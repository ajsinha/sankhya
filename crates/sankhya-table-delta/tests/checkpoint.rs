//! Checkpoints, and the fact that nothing depends on one.
//!
//! The property running through all of this: a checkpoint is *derived*. It contains
//! exactly what replay produces, so a reader that cannot find one, cannot parse one, or
//! simply distrusts one gets the same answer more slowly. That is what makes it safe to
//! write by hand — the worst a bad checkpoint can do is be ignored.

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

use sankhya_table_delta::{
    commit, create, latest_checkpoint, latest_metadata, live_files, read_checkpoint,
    write_checkpoint, Action, AddFile, CommitError, Metadata, RemoveFile,
};

const SCHEMA: &str = r#"{"type":"struct","fields":[]}"#;

fn root() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn metadata() -> Metadata {
    Metadata::new("t", SCHEMA, 0)
}

/// A table of `files` live files across two commits.
fn build(dir: &std::path::Path, files: u64) {
    let mut actions = create(metadata());
    for i in 0..files {
        actions.push(Action::Add(AddFile::with_rows(
            format!("part-{i:08}.parquet"),
            100,
            0,
            50,
        )));
    }
    commit(dir, 0, &actions).expect("commit 0");
}

#[test]
fn a_checkpoint_records_exactly_the_live_files() {
    let dir = root();
    build(dir.path(), 12);

    let live = live_files(dir.path()).expect("reading");
    let report = write_checkpoint(dir.path(), &live, &metadata(), 1, 2).expect("checkpointing");

    assert_eq!(report.version, 0);
    assert_eq!(report.actions, 14, "protocol, metadata, twelve files");

    let recorded = read_checkpoint(dir.path(), 0).expect("reading it back");
    assert_eq!(recorded.len(), 12);
    assert_eq!(recorded[0].path, "part-00000000.parquet");
    assert_eq!(recorded[0].rows(), Some(50));
}

#[test]
fn a_checkpoint_omits_files_that_were_removed() {
    // A checkpoint is the reconciled state, not the history. A file added and later
    // removed is simply absent -- which is the protocol's own design and is why a
    // checkpoint can be smaller than the log it replaces.
    let dir = root();
    build(dir.path(), 6);
    commit(
        dir.path(),
        1,
        &[
            Action::Add(AddFile::with_rows("merged.parquet", 600, 1, 300)),
            Action::Remove(RemoveFile::rewritten("part-00000000.parquet", 1)),
            Action::Remove(RemoveFile::rewritten("part-00000001.parquet", 1)),
        ],
    )
    .expect("compacting");

    let live = live_files(dir.path()).expect("reading");
    write_checkpoint(dir.path(), &live, &metadata(), 1, 2).expect("checkpointing");

    let recorded = read_checkpoint(dir.path(), 1).expect("reading it back");
    let paths: Vec<&str> = recorded.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths.len(), 5);
    assert!(!paths.contains(&"part-00000000.parquet"));
    assert!(paths.contains(&"merged.parquet"));
}

#[test]
fn a_reader_starts_from_the_checkpoint_and_applies_what_came_after() {
    let dir = root();
    build(dir.path(), 4);

    let live = live_files(dir.path()).expect("reading");
    write_checkpoint(dir.path(), &live, &metadata(), 1, 2).expect("checkpointing");

    commit(
        dir.path(),
        1,
        &[Action::Add(AddFile::with_rows("later.parquet", 100, 0, 50))],
    )
    .expect("committing");

    let after = live_files(dir.path()).expect("reading");
    assert_eq!(after.files.len(), 5);
    assert_eq!(after.version, Some(1));
    assert!(after.paths().contains(&"later.parquet"));
}

#[test]
fn the_answer_is_the_same_with_and_without_a_checkpoint() {
    // The property that makes a checkpoint an optimisation rather than a second source
    // of truth.
    let dir = root();
    build(dir.path(), 8);
    commit(
        dir.path(),
        1,
        &[
            Action::Add(AddFile::with_rows("merged.parquet", 800, 1, 400)),
            Action::Remove(RemoveFile::rewritten("part-00000003.parquet", 1)),
        ],
    )
    .expect("compacting");

    let without = live_files(dir.path()).expect("reading");
    write_checkpoint(dir.path(), &without, &metadata(), 1, 2).expect("checkpointing");
    let with = live_files(dir.path()).expect("reading");

    assert_eq!(with, without);
}

#[test]
fn a_missing_checkpoint_file_is_ignored_rather_than_trusted() {
    // A pointer to a checkpoint that is not there sends every reader down a path that
    // fails, where no pointer at all costs them only a replay.
    let dir = root();
    build(dir.path(), 5);
    let live = live_files(dir.path()).expect("reading");
    write_checkpoint(dir.path(), &live, &metadata(), 1, 2).expect("checkpointing");

    assert_eq!(latest_checkpoint(dir.path()), Some(0));
    std::fs::remove_file(
        dir.path()
            .join("_delta_log")
            .join("00000000000000000000.checkpoint.parquet"),
    )
    .expect("removing the checkpoint");

    assert_eq!(latest_checkpoint(dir.path()), None);
    assert_eq!(live_files(dir.path()).expect("reading"), live);
}

#[test]
fn an_unreadable_pointer_is_ignored_rather_than_reported() {
    let dir = root();
    build(dir.path(), 3);
    let live = live_files(dir.path()).expect("reading");
    write_checkpoint(dir.path(), &live, &metadata(), 1, 2).expect("checkpointing");

    std::fs::write(
        dir.path().join("_delta_log").join("_last_checkpoint"),
        "{not json",
    )
    .expect("corrupting the pointer");

    assert_eq!(latest_checkpoint(dir.path()), None);
    assert_eq!(
        live_files(dir.path()).expect("reading"),
        live,
        "a corrupt pointer must cost a replay, not an answer"
    );
}

#[test]
fn a_checkpoint_from_a_previous_table_at_the_same_path_is_ignored() {
    // A table dropped and recreated leaves a pointer to a version the new log may not
    // have. Trusting it would serve files from a table that no longer exists.
    let dir = root();
    build(dir.path(), 6);
    for v in 1..=4u64 {
        commit(
            dir.path(),
            v,
            &[Action::Add(AddFile::with_rows(
                format!("extra-{v}.parquet"),
                100,
                0,
                50,
            ))],
        )
        .expect("committing");
    }
    let live = live_files(dir.path()).expect("reading");
    write_checkpoint(dir.path(), &live, &metadata(), 1, 2).expect("checkpointing");
    assert_eq!(latest_checkpoint(dir.path()), Some(4));

    // Rebuild the log shorter, leaving the old checkpoint and pointer behind.
    for v in 1..=4u64 {
        std::fs::remove_file(dir.path().join("_delta_log").join(format!("{v:020}.json")))
            .expect("removing");
    }

    assert_eq!(
        latest_checkpoint(dir.path()),
        None,
        "a checkpoint claiming a version the log does not have must be ignored"
    );
    assert_eq!(live_files(dir.path()).expect("reading").version, Some(0));
}

#[test]
fn checkpointing_a_table_with_no_commits_is_refused() {
    let dir = root();
    let live = live_files(dir.path()).expect("reading");
    assert!(write_checkpoint(dir.path(), &live, &metadata(), 1, 2).is_err());
}

#[test]
fn a_checkpoint_is_never_observed_half_written() {
    // Staged and renamed like a commit. A partial checkpoint would not even be
    // detectably partial -- Parquet without a footer simply fails to open, which is the
    // good case; a truncated row group is worse.
    let dir = root();
    build(dir.path(), 4);
    let live = live_files(dir.path()).expect("reading");
    write_checkpoint(dir.path(), &live, &metadata(), 1, 2).expect("checkpointing");

    let leftovers: Vec<String> = std::fs::read_dir(dir.path().join("_delta_log"))
        .expect("listing")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// What a checkpoint saves a cold reader.
///
/// Run with `cargo test -p sankhya-table-delta --test checkpoint --release -- --ignored
/// --nocapture measure`.
///
/// This is the number that matters to an external engine, which has no cache and starts
/// cold every time.
#[test]
#[ignore = "a measurement, not an assertion"]
fn measure_what_a_checkpoint_saves_a_cold_reader() {
    for commits in [1_000u64, 10_000, 50_000] {
        let dir = root();
        commit(dir.path(), 0, &create(metadata())).expect("commit 0");
        for v in 1..=commits {
            commit(
                dir.path(),
                v,
                &[Action::Add(AddFile::with_rows(
                    format!("part-{v:08}.parquet"),
                    100,
                    0,
                    50,
                ))],
            )
            .expect("committing");
        }

        let mut without = std::time::Duration::MAX;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let live = live_files(dir.path()).expect("replaying");
            assert_eq!(live.files.len() as u64, commits);
            without = without.min(t.elapsed());
        }

        let live = live_files(dir.path()).expect("reading");
        let report = write_checkpoint(dir.path(), &live, &metadata(), 1, 2).expect("checkpointing");

        let mut with = std::time::Duration::MAX;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let live = live_files(dir.path()).expect("reading");
            assert_eq!(live.files.len() as u64, commits);
            with = with.min(t.elapsed());
        }

        println!(
            "{commits:>6} commits: replay {without:>9.2?}   from checkpoint {with:>9.2?}   \
             {:.0}x   ({} KiB)",
            without.as_secs_f64() / with.as_secs_f64(),
            report.bytes / 1024
        );
    }
}

#[test]
fn a_checkpoint_declaring_a_reader_version_we_cannot_honour_is_refused() {
    // The bypass. `FMT-02`'s ceiling is enforced in `read_actions_after`, which reads JSON
    // commits — and `live_files` starts from a checkpoint and then reads only the commits
    // **after** it. A reader-version bump made before the checkpoint was therefore parsed by
    // nothing, because the protocol lives on a row whose `add` is null and the reader skipped
    // exactly those rows.
    //
    // So a table an external engine had upgraded and then checkpointed was **refused by a
    // query and served by a compaction** — the state `log.rs` warns against by name. The
    // compaction is the damaging half: it reads the raw Parquet, ignores the deletion vectors
    // a version 3 table depends on, and commits the result. Logically deleted rows come back,
    // into a file every external reader will now believe, written by a process reporting
    // success.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    commit(root, 0, &create(Metadata::new("t", SCHEMA.to_string(), 0))).expect("creating");
    commit(
        root,
        1,
        &[Action::Add(AddFile::with_rows("part-0.parquet", 512, 0, 1))],
    )
    .expect("publishing");

    let live = live_files(root).expect("a readable table");
    let metadata = latest_metadata(root).expect("readable").expect("a table has metadata");

    // A checkpoint another engine could write: this writer only ever emits reader version 1,
    // and the versions are parameters precisely because the file is an open format.
    write_checkpoint(root, &live, &metadata, 3, 7).expect("an upgraded checkpoint");

    // And the commit that would ordinarily carry the bump, written *before* the checkpoint ---
    // which is the real sequence: an external engine enables deletion vectors at commit N and
    // checkpoints at M >= N. `advance` reads only commits after M, so this one is never
    // parsed, and the checkpoint is the only remaining record of the declaration.
    assert!(
        latest_checkpoint(root).is_some(),
        "the checkpoint must be the starting point for this to test anything"
    );

    match live_files(root) {
        Err(CommitError::Unsupported { required, supported, .. }) => {
            assert_eq!(required, 3);
            assert_eq!(supported, 1);
        }
        Err(other) => panic!("refused for the wrong reason: {other}"),
        Ok(live) => panic!(
            "a checkpoint declaring reader version 3 was read, and the table served whole \
             with {} file(s) --- its deleted rows would come back through compaction",
            live.files.len()
        ),
    }
}
