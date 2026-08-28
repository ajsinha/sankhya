//! The figures documents quote, checked and kept current.
//!
//! A test count in prose is derived data. Deriving it by hand is how it goes stale: every
//! commit that adds a test turns a green build red in seven documents, and the fix is a
//! careful edit across two files and both spellings of the same number. Done often enough,
//! the tempting move becomes not adding the test.
//!
//! So there are two halves. [`check`] fails a build whose prose has drifted, and [`sync`]
//! rewrites the drift away. The check stays because running the fixer is a deliberate act,
//! and a build must still fail if somebody skipped it.

use crate::list_tests;
use std::path::{Path, PathBuf};

/// Numbers a document claims about this repository are the numbers this repository has.
///
/// Test counts and mutation counts rot on almost every commit, silently, and a reader has
/// no way to tell a stale figure from a current one --- both are just a number. A document
/// asserting "630 tests" when there are 1,098 is not merely out of date: it is evidence
/// that nobody has checked, which devalues every other figure in the same document.
///
/// Only figures that are mechanically knowable are checked. A historical statement --- "one
/// entry was inert until corrected" --- is about a moment and cannot rot, so it is left
/// alone. A check that fired on prose would be switched off, and then it would catch
/// nothing at all.
/// Rewrite every stale figure a document quotes, and report what moved.
///
/// # Why a fixer and not only a check
///
/// The check has always been right and it made the work manual: every commit that adds a
/// test turns a green build red in seven documents, and the fix is a `sed` somebody has to
/// get right across two files and five spellings of the same number. Done by hand often
/// enough, the tempting move becomes not adding the test.
///
/// A figure quoted in prose is derived data. Deriving it by hand is the rot; deriving it
/// mechanically is the cure --- and the check stays, because the *fixer* running is a
/// deliberate act and a build must still fail if somebody skipped it.
pub fn sync(root: &Path, docs: &[PathBuf]) -> bool {
    println!("== sync-doc-numbers ==");
    let (Some(mutations), Some(tests)) = (catalogue_size(root), test_count(root)) else {
        eprintln!("   FAILED: could not count the tests or the mutation catalogue");
        return false;
    };

    let mut rewritten = 0usize;
    let mut files = 0usize;
    for path in docs {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut out = String::with_capacity(text.len());
        let mut touched = 0usize;
        for (index, line) in text.lines().enumerate() {
            let mut line = line.to_string();
            for (claimed, unit) in claimed_numbers(&line) {
                let actual = if unit == "tests" { tests } else { mutations };
                if claimed != actual {
                    // Both spellings, because prose writes 1,659 and a command line writes
                    // 1659, and a fixer that knows only one leaves the other stale --- which
                    // is worse than not running, since the check then passes on half a file.
                    line = line
                        .replace(&with_thousands(claimed), &with_thousands(actual))
                        .replace(&claimed.to_string(), &actual.to_string());
                    touched += 1;
                }
            }
            if index > 0 || !text.is_empty() {
                out.push_str(&line);
                out.push('\n');
            }
        }
        if touched > 0 {
            if std::fs::write(path, out).is_err() {
                eprintln!("  COULD NOT WRITE {}", path.display());
                return false;
            }
            let rel = path.strip_prefix(root).unwrap_or(path).display();
            println!("   {rel}: {touched} figure(s) brought up to date");
            rewritten += touched;
            files += 1;
        }
    }
    if rewritten == 0 {
        println!("   every claimed figure already matches {tests} tests and {mutations} mutations");
    } else {
        println!("   {rewritten} figure(s) across {files} document(s)");
    }
    true
}


/// A number as prose writes it: grouped in threes.
fn with_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}


pub fn check(root: &Path, docs: &[PathBuf]) -> bool {
    println!("== check-doc-numbers ==");

    let Some(mutations) = catalogue_size(root) else {
        eprintln!("   FAILED: could not count the mutation catalogue");
        return false;
    };
    let Some(tests) = test_count(root) else {
        eprintln!("   FAILED: could not count the tests");
        return false;
    };

    // `(number) tests` and `(number) specific|deliberate defects`, which are the two figures
    // documents actually quote.
    let mut ok = true;
    let mut checked = 0usize;
    for path in docs {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(path).display();
        for (line_number, line) in text.lines().enumerate() {
            for (claimed, unit) in claimed_numbers(line) {
                checked += 1;
                let actual = if unit == "tests" { tests } else { mutations };
                if claimed != actual {
                    eprintln!(
                        "  STALE NUMBER {rel}:{}: claims {claimed} {unit}, and there are \
                         {actual}",
                        line_number + 1
                    );
                    ok = false;
                }
            }
        }
    }
    println!(
        "   {checked} claimed figure(s) checked against {tests} tests and {mutations} mutations"
    );
    ok
}


/// Every figure a line claims, as `(number, unit)`.
fn claimed_numbers(line: &str) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for (marker, unit) in [
        (" tests", "tests"),
        (" specific defects", "mutations"),
        (" deliberate defects", "mutations"),
        (" sequential", "mutations"),
    ] {
        let mut from = 0usize;
        while let Some(at) = line.get(from..).and_then(|rest| rest.find(marker)) {
            let end = from + at;
            // Walk back over the digits and separators immediately before the marker.
            let prefix = line.get(..end).unwrap_or("");
            let digits: String = prefix
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .collect::<Vec<char>>()
                .into_iter()
                .rev()
                .collect();
            if let Ok(value) = digits.replace(',', "").parse::<usize>() {
                found.push((value, unit));
            }
            from = end + marker.len();
        }
    }
    found
}


/// How many entries the mutation catalogue holds.
fn catalogue_size(root: &Path) -> Option<usize> {
    let text = std::fs::read_to_string(root.join("tools/mutation-audit.py")).ok()?;
    // Each entry opens with a parenthesised tuple whose first element is a quoted label
    // containing a colon. Counting those is cheap and does not need Python.
    Some(
        text.lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("(\"") && trimmed.contains(": ")
            })
            .count(),
    )
}


/// How many tests the workspace runs.
///
/// Listed rather than executed: `--list` compiles the test binaries and enumerates them
/// without running anything, so this costs a build that `check-lints` has already paid for.
/// Ignored tests are excluded, because the figure documents quote is what a plain
/// `cargo test --workspace` reports.
fn test_count(root: &Path) -> Option<usize> {
    let listed = list_tests(root, false)?;
    let ignored = list_tests(root, true)?;
    // `--list` enumerates ignored tests alongside the rest and does not mark them, so the
    // ignored ones are counted separately and subtracted. Quoting the listed total instead
    // would overstate by however many tests need a database or a built server.
    Some(listed.saturating_sub(ignored))
}


#[cfg(test)]
mod tests {
    use super::with_thousands;

    /// Prose writes 1,660 and a command line writes 1660.
    ///
    /// A fixer that knows only one spelling leaves the other stale, which is worse than not
    /// running: the check then passes on half a file, and the wrong number is the one nobody
    /// looked at.
    #[test]
    fn a_figure_is_written_the_way_prose_writes_it() {
        assert_eq!(with_thousands(7), "7");
        assert_eq!(with_thousands(999), "999");
        assert_eq!(with_thousands(1_000), "1,000");
        assert_eq!(with_thousands(1_660), "1,660");
        assert_eq!(with_thousands(1_234_567), "1,234,567");
    }
}
