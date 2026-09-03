//! What changed between two versions, over the wire.
//!
//! # Why this was deferred, and what unblocked it
//!
//! `M20` was deferred on 2026-09-02 with a specific objection: the log records file-level adds
//! and removes, so a **compaction** --- which rewrites files and changes no row --- would show as
//! a total replacement. *A diff that reports a compaction as a change is worse than no diff,
//! because it looks like an answer.*
//!
//! [ADR-0024](../../../docs/adr/0024-what-a-difference-between-two-versions-is.md) settles it:
//! a difference is a change to **rows**, and the log already knows which commits changed rows,
//! because every add and remove carries `dataChange` and a compaction writes `false` on both
//! sides. That is the writer's own statement about what it did, and every writer here is ours.
//!
//! # The test that matters
//!
//! Not that the numbers add up --- that a **compaction between the two versions contributes
//! nothing to them, and is still reported**. Both halves: silence about a compaction would be a
//! second misleading answer, leaving a reader wondering why the storage looks nothing like it
//! did.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{start, text_rows, write_warehouse, Running};

fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

fn ask(port: u16, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    std::panic::catch_unwind(|| text_rows(port, sql)).map_err(|panicked| {
        panicked
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panicked.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "the statement failed".to_owned())
    })
}

/// The one row `SHOW CHANGES` answers with, as numbers.
fn changes(port: u16, from: u64, to: u64) -> Vec<u64> {
    let rows = text_rows(
        port,
        &format!("SHOW CHANGES BETWEEN {from} AND {to} FOR sales.orders"),
    );
    assert_eq!(rows.len(), 1, "one row of answer: {rows:?}");
    rows[0]
        .iter()
        .map(|cell| cell.as_deref().unwrap_or("0").parse().unwrap_or(0))
        .collect()
}

#[test]
fn the_difference_counts_the_rows_the_commits_added() {
    // The fixture publishes four files of 250 rows across versions 1..4, so the range is a
    // thousand rows in four commits. Asserted as numbers rather than as "something changed",
    // because a diff that only says *yes* is a diff nobody can reconcile against.
    let (_dir, server) = running();
    let history = text_rows(server.port, "SHOW HISTORY OF sales.orders");
    assert!(history.len() >= 4, "the fixture has commits: {}", history.len());

    let whole = changes(server.port, 0, 4);
    assert_eq!(whole[0], 4, "four commits declared a data change");
    assert_eq!(whole[1], 1000, "a thousand rows added");
    assert_eq!(whole[2], 0, "and none removed");
    assert_eq!(whole[3], 4, "four files");
    assert_eq!(whole[5], 0, "no compaction in this range");
}

#[test]
fn the_range_is_exclusive_of_the_earlier_version_and_inclusive_of_the_later() {
    // `BETWEEN 4 AND 7` means what happened *after* 4, up to and including 7 --- what would be
    // new to a reader who last read version 4. Stated in the ADR because the other reading is
    // defensible, and the two differ by exactly one commit: the difference between a
    // reconciliation that ties out and one that is off by whatever that commit carried.
    let (_dir, server) = running();

    let all_four = changes(server.port, 0, 4);
    let last_three = changes(server.port, 1, 4);
    assert_eq!(all_four[0], 4);
    assert_eq!(last_three[0], 3, "version 1 is excluded, 4 is included");
    assert_eq!(
        all_four[1] - last_three[1],
        250,
        "and the rows differ by exactly the one commit that was dropped from the range"
    );

    // A range of nothing is nothing, rather than an error or a whole-table count.
    let none = changes(server.port, 4, 4);
    assert_eq!(none[0], 0, "no commits after 4 up to 4");
    assert_eq!(none[1], 0);
}

#[test]
fn a_version_nobody_has_is_refused_rather_than_answered_about_the_newest() {
    // `SET VERSION OF`'s rule, and here for the same reason: a caller who asked about a version
    // that does not exist must not be handed a difference that is real and is not theirs.
    let (_dir, server) = running();
    let Err(said) = ask(server.port, "SHOW CHANGES BETWEEN 0 AND 900 FOR sales.orders") else {
        panic!("a version nobody has must be refused");
    };
    assert!(said.contains("no version 900"), "and names it: {said}");
    assert!(said.contains("newest"), "and says what there is instead: {said}");
}

#[test]
fn a_range_that_runs_backwards_is_refused() {
    // Reversing it silently would report additions as removals, which is a wrong answer of
    // exactly the shape this whole feature exists to avoid producing.
    let (_dir, server) = running();
    let Err(said) = ask(server.port, "SHOW CHANGES BETWEEN 4 AND 1 FOR sales.orders") else {
        panic!("a backwards range must be refused");
    };
    assert!(
        said.contains("not earlier") && said.contains("additions as removals"),
        "and says what it would have produced: {said}"
    );
}

#[test]
fn a_table_nobody_can_read_is_absent_rather_than_forbidden() {
    // Saying *you may not ask about that* confirms it is there, which is the disclosure the
    // whole security chapter is careful about.
    let (_dir, server) = running();
    let Err(said) = ask(server.port, "SHOW CHANGES BETWEEN 0 AND 1 FOR nowhere.at_all") else {
        panic!("a table that does not exist must be refused");
    };
    assert!(said.contains("no table called"), "{said}");
}

#[test]
fn a_malformed_range_says_what_the_statement_looks_like() {
    let (_dir, server) = running();
    let Err(said) = ask(server.port, "SHOW CHANGES BETWEEN 0 FOR sales.orders") else {
        panic!("a statement missing its second version must be refused");
    };
    assert!(
        said.contains("AND") || said.contains("version number"),
        "and names the word it wanted: {said}"
    );
}
