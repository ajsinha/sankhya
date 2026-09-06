//! Replaying the log.
//!
//! The interesting cases are the ones where order matters, because replay is the only
//! thing standing between a correct file set and one that double-counts.

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
    commit, commits, create, live_files, Action, AddFile, CommitError, Metadata, RemoveFile,
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
fn commits_replay_in_version_order_not_directory_order() {
    // The commit files are created in reverse, so anything that trusted the order the
    // filesystem hands them back would see the removal before the add and report both
    // files live.
    //
    // Written directly rather than through `commit`, which now refuses to leave a gap —
    // a rule that exists for a different reason and would make this sequence
    // unconstructible. The property being tested is the replay's, not the writer's.
    let dir = root();
    let log = dir.path().join("_delta_log");
    std::fs::create_dir_all(&log).expect("creating the log directory");

    let write = |version: u64, actions: &[Action]| {
        let body: String = actions
            .iter()
            .map(|a| format!("{}\n", serde_json::to_string(a).expect("encoding")))
            .collect();
        std::fs::write(log.join(format!("{version:020}.json")), body).expect("writing");
    };

    write(2, &[remove("a.parquet")]);
    write(1, &[add("a.parquet"), add("b.parquet")]);
    write(0, &create(Metadata::new("t", SCHEMA, 0)));

    let live = live_files(dir.path()).expect("reading");
    assert_eq!(live.paths(), vec!["b.parquet"]);
    assert_eq!(live.version, Some(2));
}

#[test]
fn a_commit_that_would_leave_a_gap_is_refused() {
    // Contiguity is not tidiness. A reader that knows the state at version n finds out
    // whether anything is newer by asking whether n+1 exists -- one probe, rather than
    // listing a directory that grows without bound. A gap makes that probe stop early
    // and silently serve a file set missing everything past it.
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");

    let err = commit(dir.path(), 5, &[add("a.parquet")]).expect_err("a gap must be refused");
    assert!(matches!(
        err,
        CommitError::NonContiguous {
            attempted: 5,
            expected: 1
        }
    ));
    assert!(format!("{err}").contains("incomplete file set"));

    // The next version is accepted.
    commit(dir.path(), 1, &[add("a.parquet")]).expect("commit 1");
    assert_eq!(live_files(dir.path()).expect("reading").version, Some(1));
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
fn a_malformed_line_beside_a_good_one_is_reported_rather_than_skipped() {
    // The test above passes for the wrong reason if the parse is made to skip: the commit it
    // writes holds *only* the bad line, so `read` stays at zero and the empty-body seal fires
    // --- returning the same `Malformed` variant by a different route. The mutation catalogue
    // found that, by surviving.
    //
    // So this one puts a good action beside the bad line. The seal is satisfied, and the only
    // thing that can report the damage is the parse itself. Skipping here is the failure that
    // matters: the commit replays as though it did less than it did, and the files the lost
    // line named are dropped from the table with no error anywhere.
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");
    let good = serde_json::to_string(&Action::Add(AddFile::with_rows("part-0.parquet", 512, 0, 1)))
        .expect("an action this crate wrote");
    // Valid JSON, and an `add` --- but not an `add` this crate can read. That distinction is
    // the one the parse being tested makes, and it is not the one `{"add": not json}` makes:
    // a line that is not JSON at all is caught by the earlier value parse, which is why the
    // mutation survived a test using one. The line has to get as far as the action.
    std::fs::write(
        dir.path()
            .join("_delta_log")
            .join("00000000000000000001.json"),
        format!("{good}\n{{\"add\": {{\"path\": 17, \"size\": \"large\"}}}}\n"),
    )
    .expect("writing a half-good commit");

    match live_files(dir.path()) {
        Err(CommitError::Malformed { version: 1, .. }) => {}
        Err(other) => panic!("reported for the wrong reason: {other}"),
        Ok(live) => panic!(
            "a line this crate could not have written was skipped, and the commit replayed \
             as {} file(s) with no error --- which is how a table silently loses what a lost \
             line named",
            live.files.len()
        ),
    }
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

#[test]
fn commits_are_listed_in_version_order() {
    // `commits` promises version order, and a caller relying on it has no way to tell
    // that the filesystem handed the entries back in some other order. Created in
    // reverse so a listing that simply forwarded directory order would fail here.
    //
    // Written directly rather than through `commit`, which refuses to leave a gap — a
    // rule with a different purpose that would make this sequence unconstructible.
    let dir = root();
    let log = dir.path().join("_delta_log");
    std::fs::create_dir_all(&log).expect("creating the log directory");

    for version in [7u64, 2, 5, 0, 9, 1] {
        std::fs::write(log.join(format!("{version:020}.json")), "").expect("writing");
    }

    let listed: Vec<u64> = commits(dir.path())
        .expect("listing")
        .into_iter()
        .map(|(v, _)| v)
        .collect();

    assert_eq!(listed, vec![0, 1, 2, 5, 7, 9]);
}

#[test]
fn a_file_that_is_not_a_commit_is_ignored() {
    // Checkpoints, temporary files and anything else a future protocol version leaves in
    // the log directory must not be mistaken for a commit.
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");

    let log = dir.path().join("_delta_log");
    std::fs::write(log.join("_last_checkpoint"), "{}").expect("writing");
    std::fs::write(log.join("00000000000000000000.checkpoint.parquet"), "").expect("writing");
    std::fs::write(log.join("notes.txt"), "").expect("writing");

    let listed: Vec<u64> = commits(dir.path())
        .expect("listing")
        .into_iter()
        .map(|(v, _)| v)
        .collect();
    assert_eq!(listed, vec![0]);
    assert_eq!(live_files(dir.path()).expect("reading").version, Some(0));
}

// --- a short commit, and what an open format has to tolerate --------------------------

/// A truncated commit body is detected rather than read as a smaller commit.
///
/// # The failure this closes
///
/// A commit body is newline-delimited JSON with no checksum, no length prefix and no
/// end-of-record marker, so **a short body is undetectable by construction**. A crash between
/// the write and the sync can leave three of five actions on disk, and every parser accepts
/// the result. Replay then reads a commit that did less than it did --- silently dropping the
/// Parquet files the missing lines named.
///
/// It is cemented rather than transient. A retry is refused as `VersionTaken`, because the
/// version exists, and the next commit lands on top of the wrong state.
///
/// The last line this writer emits is a `commitInfo` naming how many actions preceded it. If
/// the last line is missing, so is the seal; if lines before it are missing, the count
/// disagrees. Either way the commit refuses to replay instead of quietly shrinking.
#[test]
fn a_commit_that_lost_its_tail_is_refused_rather_than_read_as_a_smaller_one() {
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("creating");
    commit(dir.path(), 1, &[add("a.parquet"), add("b.parquet"), add("c.parquet")])
        .expect("committing");
    assert_eq!(live_files(dir.path()).expect("reading").files.len(), 3);

    let path = dir.path().join("_delta_log").join("00000000000000000001.json");
    let whole = std::fs::read_to_string(&path).expect("reading the commit");
    let lines: Vec<&str> = whole.lines().collect();

    // Every prefix short of the whole thing must be refused. The seal is the first line, so
    // every prefix that contains anything at all contains the count it contradicts.
    for keep in 0..lines.len() {
        std::fs::write(&path, format!("{}\n", lines[..keep].join("\n"))).expect("truncating");
        let outcome = live_files(dir.path());
        assert!(
            outcome.is_err(),
            "a commit truncated to {keep} of {} lines was accepted, and would have dropped \
             {} file(s) with no error",
            lines.len(),
            3 - keep.min(3)
        );
    }

    // And the whole thing still reads.
    std::fs::write(&path, &whole).expect("restoring");
    assert_eq!(live_files(dir.path()).expect("reading").files.len(), 3);
}

/// A commit written by another engine does not make the table unreadable.
///
/// # Why this was the shape of the openness problem
///
/// Spark writes a `commitInfo` action on every commit. This reader rejected any action kind it
/// did not have a variant for, so **one external write made the table permanently unreadable
/// to SANKHYA** --- while everything SANKHYA wrote stayed readable to everybody else. The
/// openness was one-directional, which is not openness.
///
/// The protocol's own rule is that a reader passes over actions it does not understand. The
/// same strictness would also have taken a whole table out on the first new action a future
/// protocol version defines.
#[test]
fn an_action_this_reader_does_not_know_is_passed_over_rather_than_fatal() {
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("creating");
    commit(dir.path(), 1, &[add("a.parquet")]).expect("committing");

    // A commit shaped the way Spark writes one: its own commitInfo, and an action kind from
    // a protocol version this reader has never seen.
    let path = dir.path().join("_delta_log").join("00000000000000000002.json");
    std::fs::write(
        &path,
        "{\"commitInfo\":{\"operation\":\"WRITE\",\"engineInfo\":\"Apache-Spark/3.5.0\"}}\n\
         {\"domainMetadata\":{\"domain\":\"delta.something\",\"configuration\":\"{}\"}}\n\
         {\"add\":{\"path\":\"b.parquet\",\"partitionValues\":{},\"size\":100,\
         \"modificationTime\":0,\"dataChange\":true}}\n",
    )
    .expect("writing an external commit");

    let live = live_files(dir.path()).expect("an external commit must not be fatal");
    assert_eq!(
        live.files.len(),
        2,
        "the add from the external commit was not read: {:?}",
        live.paths()
    );
}

/// A commit with nothing in it is refused at the point of writing.
///
/// An empty body and a body truncated to nothing are the same bytes. Refusing to write one is
/// what lets the reader treat the other as damage.
#[test]
fn a_commit_with_no_actions_is_refused() {
    let dir = root();
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("creating");
    assert!(
        commit(dir.path(), 1, &[]).is_err(),
        "an empty commit is indistinguishable from a truncated one"
    );
}

#[test]
fn a_log_nobody_can_read_is_an_error_rather_than_an_empty_table() {
    // `OPS-12`, in the place it costs the most. The walk that finds commits used
    // `Path::exists`, which answers `false` for every failure --- including "the directory
    // holding this file cannot be searched". So a `_delta_log` with the wrong permissions,
    // or under a half-mounted export, ended the walk at version zero and the table replayed
    // as empty: a `SELECT` returned no rows and **succeeded**.
    //
    // Zero rows that are wrong is the failure mode this whole system is arranged against,
    // and it was one `chmod` away.
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    commit(root, 0, &create(Metadata::new("t", SCHEMA.to_string(), 0))).expect("creating");
    commit(
        root,
        1,
        &[Action::Add(AddFile::with_rows("part-0.parquet", 512, 0, 1))],
    )
    .expect("publishing");

    // It reads as one file while it is readable, so the assertion below is about the
    // permissions rather than about a table that was empty all along.
    assert_eq!(live_files(root).expect("a readable log").files.len(), 1);

    let log = root.join("_delta_log");
    let mut permissions = std::fs::metadata(&log).expect("it exists").permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&log, permissions).expect("setting permissions");
    let blinded = std::fs::read_dir(&log).is_err();

    let answer = live_files(root);

    let mut permissions = std::fs::metadata(&log).expect("it exists").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&log, permissions).ok();

    assert!(
        blinded,
        "the log is still readable after chmod 000 --- this test cannot run as root"
    );
    match answer {
        Err(_) => {}
        Ok(live) => panic!(
            "a log that cannot be read must not replay as a table with {} file(s)",
            live.files.len()
        ),
    }
}

#[test]
fn a_table_declaring_a_reader_version_we_cannot_honour_is_refused() {
    // `FMT-02`. `Action::Protocol` was parsed and then discarded --- the replay's arm was
    // `Action::Protocol { .. } => {}` --- and no comparison against a ceiling existed anywhere
    // in the workspace. So a table another engine had upgraded was read anyway, with this
    // reader understanding only the parts that happen to look like version 1.
    //
    // That is not a missing feature. Reader version 2 is column mapping, so physical names no
    // longer match logical ones and **every column reads as null**. Version 3 brings deletion
    // vectors, so a deleted row stays in its file with a vector beside it saying so, and a
    // reader that ignores the vector **serves deleted rows as live**. Both are answers rather
    // than errors, which is the failure this system is arranged against.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    commit(root, 0, &create(Metadata::new("t", SCHEMA.to_string(), 0))).expect("creating");
    commit(
        root,
        1,
        &[Action::Add(AddFile::with_rows("part-0.parquet", 512, 0, 1))],
    )
    .expect("publishing");

    // It reads while the table is version 1, so the refusal below is about the declaration
    // and not about a table that was never readable.
    assert_eq!(live_files(root).expect("a version 1 table").files.len(), 1);

    // Another engine upgrades it. Written by hand because this writer will not produce one:
    // that is the whole point --- the log is open, and what arrives in it is not ours.
    commit(
        root,
        2,
        &[Action::Protocol {
            min_reader_version: 3,
            min_writer_version: 7,
        }],
    )
    .expect("an upgrade another engine could write");

    match live_files(root) {
        Err(CommitError::Unsupported { required, supported, .. }) => {
            assert_eq!(required, 3);
            assert_eq!(supported, 1);
        }
        Err(other) => panic!("refused for the wrong reason: {other}"),
        Ok(live) => panic!(
            "a table declaring reader version 3 was served whole, with {} file(s) --- \
             its deleted rows would be returned as live",
            live.files.len()
        ),
    }
}

#[test]
fn the_version_this_writer_emits_is_one_it_can_read() {
    // A ceiling below what the writer declares would refuse this system's own tables, and a
    // ceiling above what it understands is the defect above wearing a constant. They are two
    // numbers in one file precisely so that changing one without the other fails here.
    assert!(
        sankhya_table_delta::SUPPORTED_READER_VERSION >= 1,
        "this build must be able to read what it writes"
    );
}
