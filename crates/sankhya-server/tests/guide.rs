//! The guide's SQL, executed.
//!
//! # What this makes true
//!
//! `GUIDE.md` opens by saying its examples are executed by a test, so an example that stops
//! working breaks the build rather than misleading a reader. **That sentence named this file
//! and this file did not exist.** Nothing ran the guide's SQL; the promise asserted a
//! verification that was never happening, which is worse than promising nothing — a reader
//! who believes it stops checking the examples themselves.
//!
//! # Reading the document rather than restating it
//!
//! The statements are extracted from `docs/GUIDE.md` at test time. A test holding its own
//! copy of the examples passes while the document says something else, which is the same
//! failure one level up.
//!
//! # Every block is accounted for
//!
//! Not every example can run here: some query tables that belong to the reader, and some are
//! deliberate errors shown to explain a refusal. Those are listed in [`NOT_RUN`] with a
//! reason each. The test asserts the accounting is **complete** — a block that is neither
//! executed nor listed fails, so an example cannot be quietly neither.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{query_outcome, start, write_warehouse};

/// Blocks that cannot run here, and why.
///
/// Matched on a distinctive fragment of the block. A reason is required: "it does not run" is
/// not one, and an entry nobody can evaluate is how a list like this becomes a way of
/// excusing whatever fails.
const NOT_RUN: &[(&str, &str)] = &[
    (
        "vec_dot(a, b)",
        "queries a table of the reader's own embeddings; the guide is showing the shape of \
         the call, not a warehouse this test could supply",
    ),
    (
        "FROM documents",
        "queries a documents table with an embedding column that the reader supplies",
    ),
    (
        "mat_determinant(covariance)",
        "a table of covariance matrices the reader brings",
    ),
    (
        "FROM pairs",
        "a table of matrix pairs the reader brings",
    ),
    (
        "vec_mean(readings)",
        "a table of reading vectors the reader brings",
    ),
    (
        "graph_reachable",
        "needs a hydrated graph; covered by sankhya-graph-sql's own tests",
    ),
    (
        "graph_time_respecting",
        "needs a hydrated graph with timed edges; covered by sankhya-graph-sql's own tests",
    ),
    (
        "FROM salaries",
        "illustrates a row policy over a table the reader defines",
    ),
    (
        "\\dt",
        "psql's own meta-commands, which the client expands before anything reaches the \
         server. They belong in the guide because a reader will type them, and they are not \
         SQL for this test to run",
    ),
];

/// Every document whose SQL this test executes.
///
/// The tutorials are here for the same reason the guide is. A tutorial is the document a
/// reader trusts most --- they are following it step by step, with no independent way to tell
/// a stale instruction from a current one --- so an untested one rots in the worst possible
/// place. Adding a tutorial to `docs/tutorials/` and not to this list is caught by
/// [`every_tutorial_is_executed`], which fails on a file nothing runs.
const DOCUMENTS: &[&str] = &["../../docs/GUIDE.md", "../../docs/TUTORIALS.md"];

/// The fenced `sql` blocks of every document in [`DOCUMENTS`], in order.
fn sql_blocks() -> Vec<String> {
    DOCUMENTS.iter().flat_map(|relative| blocks_of(relative)).collect()
}

/// The fenced `sql` blocks of one document.
fn blocks_of(relative: &str) -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(relative)
        .canonicalize()
        .unwrap_or_else(|_| panic!("{relative} is beside the crates it documents"));
    let text = std::fs::read_to_string(path).expect("reading the document");

    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        match (&mut current, line.trim_start()) {
            (None, "```sql") => current = Some(String::new()),
            (Some(_), "```") => {
                if let Some(block) = current.take() {
                    blocks.push(block);
                }
            }
            (Some(block), _) => {
                block.push_str(line);
                block.push('\n');
            }
            (None, _) => {}
        }
    }
    blocks
}

/// The statements of a block, with comment-only lines and trailing comments removed.
///
/// A `-- ERROR:` line marks the block as showing a refusal rather than a result, and the
/// whole block is then expected to fail.
fn statements(block: &str) -> (Vec<String>, bool) {
    let expects_error = block.contains("-- ERROR");
    let mut out = Vec::new();
    let mut current = String::new();
    for line in block.lines() {
        let code = line.split_once("--").map_or(line, |(before, _)| before);
        if code.trim().is_empty() {
            continue;
        }
        current.push_str(code);
        current.push(' ');
        if code.contains(';') {
            out.push(current.trim().trim_end_matches(';').to_string());
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().trim_end_matches(';').to_string());
    }
    (out, expects_error)
}

#[test]
fn every_document_this_test_runs_actually_exists() {
    // The accounting, the other direction. It used to read `docs/tutorials/` and require every
    // file in it to be named in `DOCUMENTS`, so a tutorial could not be added and quietly left
    // unverified. The four tutorials are now one document --- `docs/TUTORIALS.md` --- and a
    // directory-listing assertion has nothing left to list.
    //
    // What it was protecting is still worth protecting: a document named here that is not on
    // disk makes `blocks_of` yield nothing, and a test that runs zero statements passes. That
    // is the failure `CLM-16` is: a check that reports green by measuring nothing.
    for relative in DOCUMENTS {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        assert!(
            path.is_file(),
            "{relative} is named here and is not on disk, so its statements are silently not run"
        );
    }
    assert!(
        !sql_blocks().is_empty(),
        "the documents named here contain no SQL, so this test would pass by running nothing"
    );
}

#[test]
fn every_guide_example_is_executed_or_accounted_for() {
    let blocks = sql_blocks();
    assert!(
        blocks.len() >= 10,
        "only {} sql blocks found; the extractor and the guide disagree",
        blocks.len()
    );

    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let data = tempfile::tempdir().expect("a temporary directory");
    write_warehouse(warehouse.path());
    let server = start(warehouse.path(), data.path());

    let mut ran = 0usize;
    let mut excused = 0usize;
    for (index, block) in blocks.iter().enumerate() {
        if let Some((_, why)) = NOT_RUN.iter().find(|(fragment, _)| block.contains(fragment)) {
            assert!(
                why.len() > 20,
                "block {index} is excused without a usable reason"
            );
            excused += 1;
            continue;
        }

        let (statements, expects_error) = statements(block);
        assert!(
            !statements.is_empty(),
            "block {index} has no statements and is not excused: {block}"
        );
        for statement in statements {
            // A refusal example must refuse. The wire client reports rows, so an error is a
            // query that returns none *and* is documented as an error --- checked together so
            // an empty result cannot pass as a refusal.
            let rows = query_outcome(server.port, &statement);
            match (expects_error, rows) {
                // A documented refusal must actually be refused. Accepting "returned no
                // rows" was the same flaw one level up: a statement that succeeds and matches
                // nothing is indistinguishable from one the server rejected, so an example
                // documented as an error could quietly have stopped being one.
                (true, Ok(rows)) => panic!(
                    "block {index} is documented as an error and succeeded with {rows} \
                     row(s): {statement}"
                ),
                (true, Err(_)) => {}
                (false, Ok(_)) => {}
                (false, Err(why)) => {
                    panic!(
                        "block {index} failed and is not documented as an error: {statement}\n\
                         {why}"
                    )
                }
            }
            ran += 1;
        }
    }

    assert_eq!(
        excused + blocks.iter().filter(|b| !NOT_RUN.iter().any(|(f, _)| b.contains(f))).count(),
        blocks.len(),
        "a block was neither executed nor accounted for"
    );
    assert!(ran > 0, "no statement ran, so this test proves nothing");
    println!("guide: {ran} statement(s) executed, {excused} block(s) accounted for");
}
