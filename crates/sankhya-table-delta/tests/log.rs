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

/// How replay cost grows with commit count.
///
/// Run with `cargo test -p sankhya-table-delta --test log --release -- --ignored
/// --nocapture measure`.
///
/// Every query plan replays the log from the first commit. That is fine at hundreds of
/// commits and the question is what it looks like at the counts a table actually reaches:
/// a table committing every ten seconds passes 8,000 commits in a day.
#[test]
#[ignore = "a measurement, not an assertion"]
fn measure_how_replay_grows_with_commit_count() {
    for commits in [100u64, 1_000, 10_000, 50_000] {
        let dir = root();
        commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");

        for v in 1..=commits {
            commit(dir.path(), v, &[add(&format!("part-{v:08}.parquet"))]).expect("committing");
        }

        let mut best = std::time::Duration::MAX;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let live = live_files(dir.path()).expect("reading");
            std::hint::black_box(&live);
            best = best.min(t.elapsed());
        }

        println!("{commits:>6} commits: replay {best:>10.2?}");
    }
}

#[test]
fn replay_scales_linearly_with_the_number_of_files() {
    // A regression guard for an algorithmic property rather than a speed.
    //
    // The first implementation searched the accumulated file list for every add and
    // filtered it for every remove, which is quadratic. It was invisible at the scale of
    // every other test here and cost 1.96 seconds per query plan at fifty thousand
    // commits -- which a table committing every ten seconds reaches inside a week.
    //
    // Two choices make this guard actually work, and the first version of it had neither:
    //
    // The actions go into a *few large commits* rather than many small ones. The
    // quadratic term is per action, not per commit, so spreading them over thousands of
    // files buries it under linear file I/O -- which is what happened, and the guard
    // passed against the very implementation it was written to catch.
    //
    // And the assertion is on the *ratio* between two sizes rather than on elapsed time,
    // so it means the same thing on a slow machine as on a fast one. Linear growth
    // roughly quadruples for four times the work; quadratic growth multiplies by
    // sixteen.
    fn replay_of(files: u64) -> std::time::Duration {
        let dir = root();
        commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");

        // Ten commits, however many files, so file I/O stays constant across sizes and
        // only the replay itself grows.
        let per_commit = files / 10;
        for v in 1..=10u64 {
            let actions: Vec<Action> = (0..per_commit)
                .map(|i| add(&format!("part-{:08}.parquet", v * per_commit + i)))
                .collect();
            commit(dir.path(), v, &actions).expect("committing");
        }

        let mut best = std::time::Duration::MAX;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let live = live_files(dir.path()).expect("reading");
            assert_eq!(live.files.len() as u64, files);
            best = best.min(t.elapsed());
        }
        best
    }

    let small = replay_of(10_000);
    let large = replay_of(40_000);

    let ratio = large.as_secs_f64() / small.as_secs_f64();
    assert!(
        ratio < 9.0,
        "replay grew {ratio:.1}x for 4x the files ({small:?} then {large:?}); linear \
         growth is about 4x and quadratic about 16x"
    );
}
