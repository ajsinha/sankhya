//! Every SQL statement the documentation shows, executed against a real server.
//!
//! # The rot this exists to catch
//!
//! `check-doc-numbers` catches a stale figure and `check-docs` catches a stale status line.
//! Neither catches a **sentence that stopped being true**, and on 2026-09-03 three of those
//! surfaced in one morning --- each found by building something, none by a check:
//!
//! - `sankhya-alloc`'s manifest and module header both said it was *the only* crate permitted
//!   to write `unsafe`. A second was needed, and the sentence had quietly stopped being true.
//! - `ADR-0021` said a `FixedSizeList` read back from Parquet cannot carry tensor metadata.
//!   That was not a property of the format; the writer was dropping it.
//! - Chapter 19 documented a `MEAN`/`MAX` cube defect, with a transcript, as something a reader
//!   *must know about before declaring a non-`SUM` measure*. It had been fixed two days
//!   earlier. Wrong in the more damaging direction: it told people a working feature did not
//!   work, and its advice would have kept somebody from declaring the measure they needed.
//!
//! The last of those is the one a machine can catch, and this is how: the book's SQL is run,
//! statement by statement, and each statement's *outcome* is checked in both directions. A
//! statement shown as working must work; a statement shown as refused must be refused. A
//! demonstration of a refusal that quietly starts succeeding is a rule that has been removed
//! and a document that still claims it.
//!
//! # Why a block may be excused, and why the excuse must be written down
//!
//! Much of the book's SQL is illustrative --- it names a table from a transcript taken on
//! another machine, or a schema this fixture does not have. Those blocks are marked in the
//! document itself with an HTML comment, which is invisible when the book is rendered and
//! obvious to anybody editing it:
//!
//! ```text
//! <!-- sankhya:illustrative a transcript from the review server; `probe_e` is not a fixture -->
//! ```
//!
//! The reason is required and is printed in the run, so the excused set is visible rather than
//! implicit. A block with no marker must run.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::print_stdout)]

mod common;

use common::{start, write_warehouse, Session};
use std::path::{Path, PathBuf};

/// The marker that excuses a block, and the reason that must follow it.
const ILLUSTRATIVE: &str = "<!-- sankhya:illustrative";

/// One statement, where it came from, and whether it is shown as working.
#[derive(Debug)]
struct Shown {
    sql: String,
    refuses: bool,
    file: PathBuf,
    line: usize,
}

/// Every markdown file under `docs/`.
fn documents(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.join("docs")];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The SQL blocks of one document, as statements, plus the reasons any were excused.
fn shown_in(path: &Path) -> (Vec<Shown>, Vec<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (Vec::new(), Vec::new());
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut excused = Vec::new();

    let mut at = 0usize;
    while at < lines.len() {
        if lines[at].trim_start() != "```sql" {
            at += 1;
            continue;
        }
        // The marker, if there is one, is the nearest preceding non-blank line.
        let mut before = at;
        while before > 0 && lines[before - 1].trim().is_empty() {
            before -= 1;
        }
        let marker = before.checked_sub(1).and_then(|i| lines.get(i)).copied().unwrap_or("");
        let opened = at;
        at += 1;
        let mut body = String::new();
        while at < lines.len() && lines[at].trim_start() != "```" {
            body.push_str(lines[at]);
            body.push('\n');
            at += 1;
        }
        at += 1;

        if let Some(reason) = marker.trim().strip_prefix(ILLUSTRATIVE) {
            let reason = reason.trim_end_matches("-->").trim();
            assert!(
                reason.len() > 20,
                "{}:{} is excused without a usable reason",
                path.display(),
                opened + 1
            );
            excused.push(format!("{}:{}  {reason}", path.display(), opened + 1));
            continue;
        }
        out.extend(statements(&body, path, opened + 1));
    }
    (out, excused)
}

/// Whether the `--` at `at` is inside a quoted literal rather than starting a comment.
///
/// `WHERE note = 'a--b'` is one statement. Counting the quotes before the marker is enough for
/// what the book contains, and being wrong in the safe direction --- treating a real comment as
/// code --- produces a statement that fails loudly rather than one that silently runs half.
fn inside_a_literal(line: &str, at: usize) -> bool {
    line.get(..at).unwrap_or("").matches('\'').count() % 2 == 1
}

/// Whether a refusal is only *"this fixture does not have that object"*.
///
/// # Where the line is, and why it is here
///
/// Much of the book's SQL is a transcript taken against a warehouse this fixture is not: a
/// `payments` graph, a `documents` table, a cube called `sales` over another machine's data.
/// Those statements cannot run here and are not wrong.
///
/// So the check is narrower than *"every statement runs"* and sharper than nothing:
///
/// > **A statement shown as working must fail only because the object it names is absent.**
///
/// Everything else is caught --- a function renamed, a clause that no longer parses, a refusal
/// the system has stopped making, an example that names something it never created. That is the
/// class of rot this exists for, and it is the class no other check sees. `check-doc-numbers`
/// catches a stale figure; `check-docs` catches a stale status line; a sentence that stopped
/// being true had nothing looking at it until now.
fn only_absent(said: &str) -> bool {
    [
        "not found",
        "no cube named",
        "no graph named",
        "there is no table",
        "no such table",
        "there is no cube",
        "no aggregation",
        "is not a cube",
        "no feed called",
        "already exists",
    ]
    .iter()
    .any(|absent| said.contains(absent))
}

/// Whether a statement is a **grammar template** rather than something anybody could run.
///
/// `CREATE TABLE <name> CLONE <schema>.<table> [ AT VERSION <n> ]` is a syntax diagram. Running
/// it proves nothing, and requiring an editor to mark every one by hand would make the marker
/// so common that nobody would read it --- which is how a marker stops meaning anything.
///
/// Detected from the notation the book actually uses: a lowercase word in angle brackets, an
/// optional clause in square brackets, or an alternation in braces. A real comparison --- `a <
/// b` --- has spaces around the operator and does not match.
fn is_a_grammar(sql: &str) -> bool {
    let placeholder = sql.contains('<')
        && sql
            .split('<')
            .skip(1)
            .any(|rest| {
                rest.split_once('>').is_some_and(|(inner, _)| {
                    !inner.is_empty()
                        && inner.len() < 30
                        && inner
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '_' || c == ' ' || c == '.')
                })
            });
    // `…` and `...` are the book's elisions --- `SELECT ... FROM sales.orders` shows a shape,
    // not a query --- and a fragment that does not begin with a statement keyword is a clause
    // lifted out of one, like `WHERE sank_data_date = '2024-03-01'`.
    let elided = sql.contains('…') || sql.contains("...");
    let opens = sql.split_whitespace().next().unwrap_or("").to_uppercase();
    let a_statement = [
        "SELECT", "CREATE", "DROP", "SHOW", "SET", "INSERT", "UPDATE", "DELETE", "WITH",
        "EXPLAIN", "RESUME", "PAUSE", "ALTER", "GRANT", "REVOKE", "COPY", "VALUES", "TABLE",
    ]
    .contains(&opens.as_str());
    placeholder
        || elided
        || !a_statement
        || sql.contains("[ ")
        || sql.contains(" ]")
        || sql.contains("| ( ")
}

/// Split a block into statements, keeping the `-- REFUSES` marker with the one it precedes.
fn statements(block: &str, file: &Path, from: usize) -> Vec<Shown> {
    let mut out: Vec<Shown> = Vec::new();
    let mut current = String::new();
    let mut refuses = false;
    // Whether the previous non-comment line closed a statement. Without it, a reply written
    // *after* its statement --- which is what a transcript looks like --- was read as a heading
    // for the next one, so one statement was excused and another was held to a rule it never
    // claimed.
    let mut just_emitted = false;
    let mut line = from;
    let mut started = line;

    for raw in block.lines() {
        line += 1;
        let trimmed = raw.trim();
        if trimmed.starts_with("--") {
            // The book's own way of saying *this is refused* is a comment beginning `ERROR:`,
            // and it writes it on either side of the statement --- before, as a heading, and
            // after, as the transcript's reply. Both are honoured, because the alternative is
            // editing three documents to say the same thing in a second notation, and a second
            // notation for one idea is how the first one stops being obeyed.
            let says_refused = {
                let upper = trimmed.to_uppercase();
                upper.contains("REFUSES") || upper.contains("ERROR:")
            };
            if says_refused {
                if current.trim().is_empty() && !just_emitted {
                    // Before the statement it belongs to.
                    refuses = true;
                } else if let Some(last) = out.last_mut() {
                    // After it. `out.last` is the statement this reply is the reply to.
                    last.refuses = true;
                }
            }
            continue;
        }
        // A **trailing** comment, which the book uses constantly to annotate a line. Kept as
        // part of the statement, the `;` before it went unnoticed and two statements were run
        // as one --- which the server then refused as *"the context currently only supports a
        // single SQL statement"*, a failure of this test and not of the book.
        let in_body = current.matches("$$").count() % 2 == 1;
        let code = match trimmed.find("--") {
            Some(at) if !in_body && !inside_a_literal(trimmed, at) => {
                trimmed.get(..at).unwrap_or("").trim()
            }
            _ => trimmed,
        };
        if code.is_empty() && !in_body {
            continue;
        }
        just_emitted = false;
        if current.trim().is_empty() {
            started = line;
        }
        // Inside `$$ … $$` the text is a **body**, not SQL: it is Python, its indentation is
        // load-bearing, and a `--` in it is a comment in that language and not in this one. So
        // the line is kept exactly as written until the quoting closes.
        if in_body {
            current.push_str(raw);
            current.push('\n');
            continue;
        }
        current.push_str(code);
        current.push('\n');
        if code.ends_with(';') {
            out.push(Shown {
                sql: current.trim().trim_end_matches(';').to_owned(),
                refuses,
                file: file.to_path_buf(),
                line: started,
            });
            current.clear();
            refuses = false;
            just_emitted = true;
        }
    }
    // A block whose last statement has no semicolon is still a statement.
    if !current.trim().is_empty() {
        out.push(Shown {
            sql: current.trim().trim_end_matches(';').to_owned(),
            refuses,
            file: file.to_path_buf(),
            line: started,
        });
    }
    out
}

#[test]
fn every_statement_the_book_shows_behaves_as_it_says() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .to_path_buf();

    let mut shown = Vec::new();
    let mut excused = Vec::new();
    for document in documents(&root) {
        let (found, skipped) = shown_in(&document);
        shown.extend(found);
        excused.extend(skipped);
    }

    assert!(
        shown.len() + excused.len() > 40,
        "the book's SQL was not found; this test would pass by measuring nothing"
    );

    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let mut wrong = Vec::new();
    let mut ran = 0usize;
    let mut grammar = 0usize;
    let mut absent = 0usize;
    for statement in &shown {
        if is_a_grammar(&statement.sql) {
            grammar += 1;
            continue;
        }
        // A fresh session per statement: the book's blocks are independent of each other, and
        // one that sets a snapshot must not decide what the next chapter's example reads.
        let mut session = Session::open(server.port);
        let outcome = session.run(&statement.sql);
        ran += 1;
        let where_from = format!("{}:{}", statement.file.display(), statement.line);
        match (statement.refuses, outcome) {
            (false, Err(said)) if only_absent(&said) => absent += 1,
            (false, Err(said)) => wrong.push(format!(
                "{where_from} is shown as working and was refused:\n    {}\n    {said}",
                statement.sql.replace('\n', " ")
            )),
            (true, Ok(_)) => wrong.push(format!(
                "{where_from} is shown as refused and succeeded:\n    {}",
                statement.sql.replace('\n', " ")
            )),
            _ => {}
        }
    }

    println!(
        "   {ran} statement(s) run, {absent} naming an object this fixture does not have, \
         {grammar} syntax diagram(s) skipped, {} block(s) excused by name:",
        excused.len()
    );
    for reason in &excused {
        println!("     {reason}");
    }

    assert!(
        wrong.is_empty(),
        "{} statement(s) in the documentation do not behave as the documentation says:\n\n{}",
        wrong.len(),
        wrong.join("\n\n")
    );
}
