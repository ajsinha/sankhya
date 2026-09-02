//! Every shipped `psql` example, executed statement by statement against a real server.
//!
//! # Why this exists alongside the Python one
//!
//! `sdk/sql/examples/` is the surface most users meet first, and it was the least checked
//! thing in the repository. Reading it found two statements that could never have run --- a
//! cube navigation with the dimension in the measure's place, and a set of vector functions
//! under names that do not exist --- both of which had been *reviewed*, and one of which a
//! written note claimed had been verified against a live server.
//!
//! The reason a review misses these is that a `psql` script with `ON_ERROR_STOP off` prints
//! its errors and keeps going, and a person scrolling the output sees a wall of results. It
//! looks like it ran. Only something that reads the exit of each statement can tell.
//!
//! # How a statement that is *meant* to fail is handled
//!
//! Several of these files exist to demonstrate refusals, so "no statement failed" is the wrong
//! assertion. A statement preceded by a `-- REFUSES` line is required to fail, and a statement
//! without one is required to succeed. Both directions are checked, because a demonstration of
//! a refusal that quietly starts succeeding is a rule that has been removed and a document that
//! still claims it.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{start, write_warehouse, Session};

/// One statement from an example file, and whether it is meant to work.
struct Statement {
    sql: String,
    refuses: bool,
    line: usize,
}

/// Split a `psql` script into statements.
///
/// Handles what these files actually contain: `psql` backslash commands, line comments, nested
/// block comments, and single-, double- and backtick-quoted strings. A splitter that only
/// looked for `;` would cut a statement in half at a semicolon inside a comment --- and the
/// half it kept would fail, which reads as a broken example rather than a broken splitter.
fn statements(script: &str) -> Vec<Statement> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut refuses = false;
    let mut at_line = 1usize;
    let mut line = 1usize;

    let mut chars = script.chars().peekable();
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    let mut in_line_comment = false;
    let mut at_line_start = true;

    while let Some(c) = chars.next() {
        if c == '\n' {
            line += 1;
        }

        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
                at_line_start = true;
                current.push(c);
            }
            continue;
        }
        if depth > 0 {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                depth -= 1;
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                depth += 1;
            }
            continue;
        }
        if let Some(open) = quote {
            current.push(c);
            if c == open {
                quote = None;
            }
            continue;
        }

        // A `psql` backslash command occupies a whole line and is not SQL.
        if at_line_start && c == '\\' {
            in_line_comment = true;
            continue;
        }
        if c == '-' && chars.peek() == Some(&'-') {
            chars.next();
            // The marker is read here, from the comment itself, so it travels with the
            // statement it describes rather than living in a list somewhere else.
            let body: String = chars.clone().take_while(|&c| c != '\n').collect();
            if body.trim().eq_ignore_ascii_case("REFUSES") {
                refuses = true;
            }
            in_line_comment = true;
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            depth += 1;
            continue;
        }
        if c == '\'' || c == '"' || c == '`' {
            quote = Some(c);
            current.push(c);
            at_line_start = false;
            continue;
        }
        if c == ';' {
            if !current.trim().is_empty() {
                out.push(Statement { sql: current.trim().to_owned(), refuses, line: at_line });
            }
            current.clear();
            refuses = false;
            at_line = line;
            at_line_start = false;
            continue;
        }

        at_line_start = c == '\n';
        if at_line_start && current.trim().is_empty() {
            at_line = line;
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        out.push(Statement { sql: current.trim().to_owned(), refuses, line: at_line });
    }
    out
}

#[test]
fn every_sql_example_runs_against_a_real_server() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the workspace root")
        .to_path_buf();
    let examples = root.join("sdk").join("sql").join("examples");

    let mut scripts: Vec<std::path::PathBuf> = std::fs::read_dir(&examples)
        .expect("the examples directory is there")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .collect();
    scripts.sort();
    assert!(
        scripts.len() >= 8,
        "the SQL examples have shrunk, which is a deletion rather than a pass: {scripts:?}"
    );

    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let mut wrong: Vec<String> = Vec::new();
    for script in &scripts {
        let name = script.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let text = std::fs::read_to_string(script).expect("the script reads");
        let parsed = statements(&text);
        assert!(!parsed.is_empty(), "{name} has no statements in it");

        // One session per file, because these files use session settings --- `SET SNAPSHOT`,
        // `SET VERSION OF` --- and a connection per statement would discard them and answer
        // from the present while the file believes it is reading the past.
        let mut session = Session::open(server.port);
        for statement in parsed {
            let outcome = session.run(&statement.sql);
            let first = statement.sql.lines().next().unwrap_or("").trim();
            match (statement.refuses, outcome) {
                (false, Err(refused)) => wrong.push(format!(
                    "{name}:{} was refused and is not marked `-- REFUSES`\n  {first}\n  {refused}",
                    statement.line
                )),
                (true, Ok(_)) => wrong.push(format!(
                    "{name}:{} is marked `-- REFUSES` and was accepted\n  {first}",
                    statement.line
                )),
                _ => {}
            }
        }
    }

    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
}
