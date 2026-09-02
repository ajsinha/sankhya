//! What happens when two writers reach for the same version at once.
//!
//! # Why this file exists
//!
//! `ARCHITECTURE.md` states the property outright: *"a writer picks the next version and
//! fails if someone took it, and the loser rebases."* That is the whole of the protocol's
//! concurrency control, and everything above it --- `Publication::append_rebasing`'s retry
//! loop, compaction rebasing onto the next free version, capture and maintenance committing
//! to one log without either stopping the other --- rests on the loser being *told*.
//!
//! It was not told. `commit` claimed a version by checking that the file was absent and then
//! renaming a staging file over it, and `rename(2)` replaces its destination silently. Two
//! committers arriving together both saw the version free, both renamed, and the second
//! overwrote the first: a commit vanished with no error to either party, and the rebase loop
//! never ran because the `VersionTaken` it waits for was never returned.
//!
//! Every test in this suite had exactly one committer per version, so none of them could see
//! it. That is the gap this file closes.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_table_delta::{commit, Action, AddFile, CommitError, RemoveFile};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier};

/// One writer's distinguishable action.
fn action(who: usize) -> Action {
    Action::Add(AddFile {
        path: format!("part-{who:05}.parquet"),
        partition_values: BTreeMap::new(),
        size: 1,
        modification_time: 0,
        data_change: true,
        stats: None,
    })
}

/// A table with version 0 already committed, so version 1 is the contested one.
fn table_at_zero() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    commit(dir.path(), 0, &[action(0)]).expect("the first commit");
    dir
}

const WRITERS: usize = 16;

#[test]
fn exactly_one_writer_wins_a_contested_version() {
    // The property the whole design rests on. Released together by a barrier, so they are
    // genuinely in the window rather than merely started at similar times.
    let dir = table_at_zero();
    let root = dir.path().to_path_buf();
    let gate = Arc::new(Barrier::new(WRITERS));

    let outcomes: Vec<Result<u64, CommitError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (1..=WRITERS)
            .map(|who| {
                let root = root.clone();
                let gate = Arc::clone(&gate);
                scope.spawn(move || {
                    gate.wait();
                    commit(&root, 1, &[action(who)])
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("no panic")).collect()
    });

    let winners = outcomes.iter().filter(|o| o.is_ok()).count();
    assert_eq!(winners, 1, "exactly one writer may claim a version: {outcomes:?}");

    for outcome in outcomes.iter().filter(|o| o.is_err()) {
        assert!(
            matches!(outcome, Err(CommitError::VersionTaken(1))),
            "a loser must be told it lost, and told why: {outcome:?}"
        );
    }
}

#[test]
fn the_winner_s_actions_are_the_ones_on_disk() {
    // A weaker version of this test would count winners and stop. It would pass while the
    // file held somebody else's body --- which is what a shared staging path produced: two
    // committers wrote the same temporary name, and the one that linked could publish the
    // other's actions.
    let dir = table_at_zero();
    let root = dir.path().to_path_buf();
    let gate = Arc::new(Barrier::new(WRITERS));

    let outcomes: Vec<(usize, Result<u64, CommitError>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (1..=WRITERS)
            .map(|who| {
                let root = root.clone();
                let gate = Arc::clone(&gate);
                scope.spawn(move || {
                    gate.wait();
                    (who, commit(&root, 1, &[action(who)]))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("no panic")).collect()
    });

    let winner = outcomes
        .iter()
        .find(|(_, outcome)| outcome.is_ok())
        .map(|(who, _)| *who)
        .expect("somebody won");

    let at_one: Vec<Action> = sankhya_table_delta::read_actions(&root)
        .expect("the log reads back")
        .into_iter()
        .filter(|(version, _)| *version == 1)
        .map(|(_, a)| a)
        .collect();
    assert_eq!(at_one, vec![action(winner)], "the file holds the winner's actions");
}

#[test]
fn a_contested_version_leaves_no_staging_files_behind() {
    // Fifteen losers each wrote a body. A claim that leaks its staging file turns every
    // contested commit into litter in the log directory --- the directory replay walks.
    let dir = table_at_zero();
    let root = dir.path().to_path_buf();
    let gate = Arc::new(Barrier::new(WRITERS));

    std::thread::scope(|scope| {
        for who in 1..=WRITERS {
            let root = root.clone();
            let gate = Arc::clone(&gate);
            scope.spawn(move || {
                gate.wait();
                let _ = commit(&root, 1, &[action(who)]);
            });
        }
    });

    let strays: Vec<String> = std::fs::read_dir(root.join("_delta_log"))
        .expect("the log directory")
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(ToString::to_string))
        .filter(|name| !name.ends_with(".json"))
        .collect();
    assert!(strays.is_empty(), "staging files left in the log: {strays:?}");
}

#[test]
fn writers_that_rebase_all_land_exactly_once() {
    // The behaviour the losers are supposed to have. Each writer retries onto the next free
    // version until it lands, which is what `Publication::append_rebasing` does one layer up
    // --- spelled out here because this crate is below it and cannot call it.
    //
    // Every writer must appear exactly once, and the versions must be contiguous: a lost
    // commit shows up as a missing writer, and a double-claim as a gap.
    let dir = table_at_zero();
    let root = dir.path().to_path_buf();
    let gate = Arc::new(Barrier::new(WRITERS));

    std::thread::scope(|scope| {
        for who in 1..=WRITERS {
            let root = root.clone();
            let gate = Arc::clone(&gate);
            scope.spawn(move || {
                gate.wait();
                let mut version = 1;
                for _ in 0..(WRITERS * 8) {
                    match commit(&root, version, &[action(who)]) {
                        Ok(_) => return,
                        Err(CommitError::VersionTaken(_) | CommitError::NonContiguous { .. }) => {
                            version = sankhya_table_delta::newest_after(&root, None)
                                .map_or(version + 1, |v| v + 1);
                        }
                        Err(other) => panic!("writer {who}: {other}"),
                    }
                }
                panic!("writer {who} never landed");
            });
        }
    });

    let replayed = sankhya_table_delta::read_actions(&root).expect("the log reads back");
    let mut landed: Vec<String> = replayed
        .iter()
        .filter(|(version, _)| *version >= 1)
        .filter_map(|(_, a)| match a {
            Action::Add(add) => Some(add.path.clone()),
            _ => None,
        })
        .collect();
    landed.sort();

    // Contiguous, with no gap: a lost commit shows up as a missing writer above, and a
    // double-claim as a hole here.
    let versions: BTreeSet<u64> = replayed.iter().map(|(v, _)| *v).collect();
    assert_eq!(
        versions,
        (0..=WRITERS as u64).collect::<BTreeSet<u64>>(),
        "versions are contiguous from the creating commit through every writer"
    );

    let mut expected: Vec<String> = (1..=WRITERS).map(|w| format!("part-{w:05}.parquet")).collect();
    expected.sort();
    assert_eq!(landed, expected, "every writer landed exactly once, and none was overwritten");
}

#[test]
fn both_halves_of_a_compaction_declare_that_no_rows_changed() {
    // `RemoveFile::rewritten` has said so since it was written, and its comment gives the
    // reason: a reader streaming changes would otherwise see every compacted row as a deletion
    // followed by a re-insertion --- a stream of spurious changes proportional to how well
    // maintenance is working, which is a perverse thing to punish.
    //
    // The **addition** declared `dataChange: true` anyway, so the stream was spurious in one
    // direction instead of two. Invisible until `SHOW HISTORY OF` printed a compaction as a
    // data change.
    let statistics = sankhya_table_delta::FileStatistics::default();
    let added = AddFile::rewritten("part-merged.parquet", 100, 0, &statistics);
    let removed = RemoveFile::rewritten("part-0000.parquet", 0);

    assert!(!added.data_change, "the merged file claimed to change rows");
    assert!(!removed.data_change);

    // And an ordinary append still declares that it does, or nothing downstream would ever
    // see a real change.
    let appended = AddFile::with_statistics("part-0001.parquet", 100, 0, &statistics);
    assert!(appended.data_change, "an append must declare a data change");
}
