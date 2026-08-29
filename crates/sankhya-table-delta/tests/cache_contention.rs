//! One table's replay must not block another table's query.
//!
//! Neither of these is a safety test. The cache returned correct answers before and returns
//! correct answers now --- the question here is not "can this corrupt?" but "does this
//! serialize?", and that is a question no correctness test asks.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_table_delta::{commit, create, Action, AddFile, LogCache, Metadata};
use std::sync::{Arc, Barrier};
use std::time::Instant;

const SCHEMA: &str = r#"{"type":"struct","fields":[{"name":"id","type":"long","nullable":false,"metadata":{}}]}"#;

/// A table with `commits` versions, each adding one file.
fn a_table_with_history(root: &std::path::Path, commits: u64) {
    std::fs::create_dir_all(root).expect("the directory");
    commit(root, 0, &create(Metadata::new("t", SCHEMA.to_string(), 0))).expect("creating");
    for version in 1..=commits {
        let name = format!("part-{version:05}.parquet");
        commit(
            root,
            version,
            &[Action::Add(AddFile::with_rows(name, 128, 0, 10))],
        )
        .expect("publishing");
    }
}

#[test]
fn a_long_replay_of_one_table_does_not_stall_another() {
    // The defect this replaces: one mutex over every table's replay, held across the log
    // probe *and* the replay itself. A cold read of a large log blocked queries against
    // unrelated tables for its whole duration.
    //
    // Measured as a ratio rather than against a wall-clock budget. An absolute threshold on a
    // shared machine measures the machine; what matters is that the small table's lookups are
    // not paced by the large table's replay.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let big = dir.path().join("big");
    let small = dir.path().join("small");
    a_table_with_history(&big, 400);
    a_table_with_history(&small, 1);

    let cache = Arc::new(LogCache::new());
    let gate = Arc::new(Barrier::new(2));

    let (small_elapsed, small_lookups) = std::thread::scope(|scope| {
        let replaying = {
            let cache = Arc::clone(&cache);
            let gate = Arc::clone(&gate);
            let big = big.clone();
            scope.spawn(move || {
                gate.wait();
                // Cold every time, so this thread is genuinely replaying four hundred
                // commits rather than answering from what it just cached.
                for _ in 0..40 {
                    cache.clear();
                    cache.live_files(&big).expect("the big table replays");
                }
            })
        };

        gate.wait();
        let started = Instant::now();
        let mut lookups = 0usize;
        while !replaying.is_finished() {
            cache.live_files(&small).expect("the small table resolves");
            lookups += 1;
        }
        replaying.join().expect("no panic");
        (started.elapsed(), lookups)
    });

    // The threshold comes from measuring both states rather than from taste. On this machine
    // the small table manages about **46,500** lookups while the big one replays, and about
    // **618** when a global lock is put back --- a factor of seventy-five. Three thousand sits
    // five times above the blocked figure and fifteen times below the free one, which leaves
    // room for a loaded machine without leaving room for the defect.
    //
    // A first version asserted `> 100`. That is below the *blocked* number, so it passed with
    // the global lock restored: a test of contention that could not detect contention.
    assert!(
        small_lookups > 3_000,
        "the small table managed only {small_lookups} lookup(s) while the big one replayed, \
         in {small_elapsed:?} --- which is what being blocked behind somebody else's I/O \
         looks like"
    );
}

#[test]
fn two_tables_are_cached_independently() {
    // The striping, stated as behaviour rather than as an implementation detail: each table
    // keeps its own entry, and clearing or reading one does not disturb the other.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    a_table_with_history(&first, 3);
    a_table_with_history(&second, 5);

    let cache = LogCache::new();
    let (one, _) = cache.live_files(&first).expect("the first resolves");
    let (two, _) = cache.live_files(&second).expect("the second resolves");

    assert_eq!(one.files.len(), 3);
    assert_eq!(two.files.len(), 5);
    assert_eq!(cache.len(), 2, "two tables, two entries");
}

#[test]
fn concurrent_lookups_of_one_table_agree() {
    // Two queries on the same table serialize on that table's own lock, and must: they are
    // advancing the same replay. What they must never do is disagree about what they saw, or
    // apply the same commits twice.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("shared");
    a_table_with_history(&root, 50);

    let cache = Arc::new(LogCache::new());
    let gate = Arc::new(Barrier::new(8));
    let counts: Vec<usize> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let gate = Arc::clone(&gate);
                let root = root.clone();
                scope.spawn(move || {
                    gate.wait();
                    cache.live_files(&root).expect("resolves").0.files.len()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("no panic")).collect()
    });

    assert!(
        counts.iter().all(|seen| *seen == 50),
        "eight concurrent lookups of one table disagreed: {counts:?}"
    );
}
