//! The cache pays for what changed, and cannot go stale.
//!
//! Staleness is the only way a cache of a table's contents can be worse than no cache,
//! so most of what follows is about the situations where a naive one would return the
//! wrong file set.

use sankhya_table_delta::{
    commit, create, live_files, Action, AddFile, LogCache, Metadata, Outcome, RemoveFile,
};

const SCHEMA: &str = r#"{"type":"struct","fields":[]}"#;

fn add(name: &str) -> Action {
    Action::Add(AddFile::new(name, 100, 0))
}

fn root() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn build(dir: &std::path::Path, commits: u64) {
    commit(dir, 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");
    for v in 1..=commits {
        commit(dir, v, &[add(&format!("part-{v:08}.parquet"))]).expect("committing");
    }
}

#[test]
fn the_first_lookup_is_cold_and_the_second_is_not() {
    let dir = root();
    build(dir.path(), 20);
    let cache = LogCache::new();

    let (first, outcome) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(outcome, Outcome::Cold);
    assert_eq!(first.files.len(), 20);

    let (second, outcome) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(outcome, Outcome::Current);
    assert_eq!(second, first);
}

#[test]
fn a_new_commit_is_picked_up_without_being_told() {
    // No invalidation call, no expiry, no notification. The cache checks the log every
    // time, which is what makes it impossible to serve a stale file set.
    let dir = root();
    build(dir.path(), 5);
    let cache = LogCache::new();

    let (before, _) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(before.files.len(), 5);

    commit(dir.path(), 6, &[add("part-00000006.parquet")]).expect("committing");

    let (after, outcome) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(outcome, Outcome::Advanced { commits_read: 1 });
    assert_eq!(after.files.len(), 6);
    assert_eq!(after.version, Some(6));
}

#[test]
fn a_removal_reaches_the_cache_too() {
    // An add that goes missing is a query returning too few rows. A removal that does
    // not is a query reading a file that has been deleted, which fails loudly -- so this
    // direction is the less dangerous one, and is still wrong.
    let dir = root();
    build(dir.path(), 4);
    let cache = LogCache::new();

    let (before, _) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(before.files.len(), 4);

    commit(
        dir.path(),
        5,
        &[Action::Remove(RemoveFile::rewritten(
            "part-00000002.parquet",
            1,
        ))],
    )
    .expect("committing");

    let (after, _) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(after.files.len(), 3);
    assert!(!after.paths().contains(&"part-00000002.parquet"));
}

#[test]
fn the_cached_answer_matches_a_full_replay_exactly() {
    // The incremental path and the from-scratch path must agree, including on order --
    // a reader consumes files in the order the table declared them.
    let dir = root();
    build(dir.path(), 10);
    let cache = LogCache::new();
    cache.live_files(dir.path()).expect("warming");

    for v in 11..=30u64 {
        commit(dir.path(), v, &[add(&format!("part-{v:08}.parquet"))]).expect("committing");
        let (cached, _) = cache.live_files(dir.path()).expect("reading");
        let fresh = live_files(dir.path()).expect("replaying");
        assert_eq!(cached, fresh, "the caches diverged at version {v}");
    }
}

#[test]
fn a_rebuilt_table_is_not_replayed_forward_from_a_stale_base() {
    // The dangerous case. If the log is rebuilt underneath the cache -- a table dropped
    // and recreated at the same path -- replaying forward from the old base would carry
    // files that no longer exist, and every query would then read files that are not
    // there.
    let dir = root();
    build(dir.path(), 10);
    let cache = LogCache::new();

    let (before, _) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(before.files.len(), 10);

    // Rebuild: same path, shorter history, different contents.
    std::fs::remove_dir_all(dir.path().join("_delta_log")).expect("removing the log");
    commit(dir.path(), 0, &create(Metadata::new("t", SCHEMA, 0))).expect("commit 0");
    commit(dir.path(), 1, &[add("fresh.parquet")]).expect("committing");

    let (after, _) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(after.paths(), vec!["fresh.parquet"]);
    assert_eq!(after.version, Some(1));
    assert_eq!(after, live_files(dir.path()).expect("replaying"));
}

#[test]
fn separate_tables_do_not_share_an_entry() {
    let a = root();
    let b = root();
    build(a.path(), 3);
    build(b.path(), 7);

    let cache = LogCache::new();
    assert_eq!(
        cache.live_files(a.path()).expect("reading").0.files.len(),
        3
    );
    assert_eq!(
        cache.live_files(b.path()).expect("reading").0.files.len(),
        7
    );
    assert_eq!(cache.len(), 2);

    // And re-reading each still gives its own answer.
    assert_eq!(
        cache.live_files(a.path()).expect("reading").0.files.len(),
        3
    );
    assert_eq!(
        cache.live_files(b.path()).expect("reading").0.files.len(),
        7
    );
}

#[test]
fn a_table_with_no_log_is_empty_rather_than_an_error() {
    let dir = root();
    let cache = LogCache::new();
    let (live, _) = cache.live_files(dir.path()).expect("reading");
    assert!(live.files.is_empty());
    assert_eq!(live.version, None);
}

#[test]
fn clearing_loses_nothing_but_the_memory() {
    let dir = root();
    build(dir.path(), 6);
    let cache = LogCache::new();

    let (before, _) = cache.live_files(dir.path()).expect("reading");
    cache.clear();
    assert!(cache.is_empty());

    let (after, outcome) = cache.live_files(dir.path()).expect("reading");
    assert_eq!(outcome, Outcome::Cold);
    assert_eq!(after, before);
}

#[test]
fn concurrent_readers_agree() {
    // The cache is shared across query plans, which run concurrently. Every thread must
    // see a file set that is correct, not merely one that does not crash.
    use std::sync::Arc;

    let dir = root();
    build(dir.path(), 40);
    let cache = Arc::new(LogCache::new());
    let path = dir.path().to_path_buf();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let cache = Arc::clone(&cache);
            let path = path.clone();
            std::thread::spawn(move || {
                let mut counts = Vec::new();
                for _ in 0..25 {
                    counts.push(cache.live_files(&path).expect("reading").0.files.len());
                }
                counts
            })
        })
        .collect();

    for handle in handles {
        for count in handle.join().expect("a thread panicked") {
            assert_eq!(count, 40);
        }
    }
}

/// What the cache saves, measured.
///
/// Run with `cargo test -p sankhya-table-delta --test cache --release -- --ignored
/// --nocapture measure`.
///
/// The realistic shape is a long-running process replanning a table that is still being
/// written to: most lookups find nothing new, and the rest find a handful of commits.
#[test]
#[ignore = "a measurement, not an assertion"]
fn measure_what_the_cache_saves() {
    for history in [1_000u64, 10_000, 50_000] {
        let dir = root();
        build(dir.path(), history);
        let cache = LogCache::new();
        cache.live_files(dir.path()).expect("warming");

        let mut cold = std::time::Duration::MAX;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let live = live_files(dir.path()).expect("replaying");
            std::hint::black_box(&live);
            cold = cold.min(t.elapsed());
        }

        let mut unchanged = std::time::Duration::MAX;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let live = cache.live_files(dir.path()).expect("reading");
            std::hint::black_box(&live);
            unchanged = unchanged.min(t.elapsed());
        }

        // One new commit, as a table under continuous capture produces.
        commit(
            dir.path(),
            history + 1,
            &[add(&format!("part-{:08}.parquet", history + 1))],
        )
        .expect("committing");
        let t = std::time::Instant::now();
        let (_, outcome) = cache.live_files(dir.path()).expect("reading");
        let advanced = t.elapsed();
        assert_eq!(outcome, Outcome::Advanced { commits_read: 1 });

        println!(
            "{history:>6} commits: full replay {cold:>9.2?}   unchanged {unchanged:>9.2?}   \
             one new commit {advanced:>9.2?}"
        );
    }
}
