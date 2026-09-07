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
    let examples = example_scripts(root);
    let checks = check_tasks(root);

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
            for (claimed, unit, spelled) in claimed_numbers(&line) {
                let actual = match unit {
                    "tests" => tests,
                    "examples" => examples,
                    "checks" => checks,
                    _ => mutations,
                };
                if claimed == actual {
                    continue;
                }
                // A figure spelled as a word is reported and not rewritten. "twenty-two
                // invariants" cannot be corrected to "24" without making the sentence wrong,
                // and correcting it by digits would edit an unrelated number on the line ---
                // which is how six documents came to claim `2,626099` tests.
                if !spelled.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    continue;
                }
                // The exact text that was matched, replaced once, keeping its own comma
                // style: prose writes 1,659 and a command line writes 1659, and each stays as
                // it was written.
                let replacement = if spelled.contains(',') {
                    with_thousands(actual)
                } else {
                    actual.to_string()
                };
                line = line.replacen(&spelled, &replacement, 1);
                touched += 1;
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


/// The runnable examples the Python SDK ships.
///
/// `_common.py` is a helper the others import, not an example. Counted rather than declared,
/// because the three documents quoting this number quoted three different values --- "eight",
/// "ten" and "twelve" --- and one of them sat three lines above its own table of twelve.
fn example_scripts(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root.join("sdk/python/examples")) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".py") && !name.starts_with('_')
        })
        .count()
}

/// The invariants `check-all` runs, counted from the source that runs them.
fn check_tasks(root: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(root.join("xtask/src/main.rs")) else {
        return 0;
    };
    text.lines()
        .filter(|line| line.contains("run_all || task == \"check-"))
        .count()
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
    let examples = example_scripts(root);
    let checks = check_tasks(root);

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
            for (claimed, unit, _spelled) in claimed_numbers(line) {
                checked += 1;
                let actual = match unit {
                    "tests" => tests,
                    "examples" => examples,
                    "checks" => checks,
                    _ => mutations,
                };
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


/// A number written as an English word, if that is what this is.
///
/// # Why a gate that reads digits is half a gate
///
/// Every count that had drifted in this repository was spelled as a **word**. One document
/// said "Eight runnable examples", another "Ten scripts" three lines above its own table of
/// twelve, and `docs/QUICKSTART.md` said `check-all` runs "eleven invariants" when it runs
/// twenty-two. This function walked back over *digits* to find the figure, so it could not
/// see any of them --- the one gate built to catch a stale number was structurally blind to
/// the form every stale number here took.
fn word_number(word: &str) -> Option<usize> {
    const WORDS: &[(&str, usize)] = &[
        ("one", 1), ("two", 2), ("three", 3), ("four", 4), ("five", 5), ("six", 6),
        ("seven", 7), ("eight", 8), ("nine", 9), ("ten", 10), ("eleven", 11),
        ("twelve", 12), ("thirteen", 13), ("fourteen", 14), ("fifteen", 15),
        ("sixteen", 16), ("seventeen", 17), ("eighteen", 18), ("nineteen", 19),
        ("twenty", 20), ("thirty", 30), ("forty", 40), ("fifty", 50),
    ];
    let lowered = word.to_ascii_lowercase();
    // "twenty-two" and the like, which is how prose writes them.
    if let Some((tens, units)) = lowered.split_once('-') {
        let tens = WORDS.iter().find(|(w, _)| *w == tens)?.1;
        let units = WORDS.iter().find(|(w, _)| *w == units)?.1;
        return (tens >= 20 && units < 10).then_some(tens + units);
    }
    WORDS.iter().find(|(w, _)| *w == lowered).map(|(_, n)| *n)
}

/// What an author writes on a line whose figures record a moment rather than describe now.
///
/// An HTML comment, so it does not render, and spelled out rather than terse so that somebody
/// meeting it in a diff can tell what it is for without looking it up.
pub const AS_MEASURED_THEN: &str = "<!-- figures-as-measured-then -->";

/// Every figure a line claims, as `(number, unit, the exact text that spelled it)`.
///
/// The third element exists because the rewriter used to do two global `replace` calls per
/// figure --- one for `2,610` and one for `2610` --- and a line holding more than one number
/// could have the second call edit text the first had just written. It produced `2,626099`
/// across six documents. Replacing the exact substring that was matched, once, cannot.
fn claimed_numbers(line: &str) -> Vec<(usize, &'static str, String)> {
    let mut found = Vec::new();
    // A line that records what was true at a moment is not a claim about now, and rewriting
    // it is not a correction --- it is falsifying a record.
    //
    // This tool was doing exactly that. `AUDIT_REPORT.md`, `REMEDIATION.md` and the README
    // all carry one sentence describing the state twelve reviewers observed: *"2,807 tests,
    // 741 mutations and twenty checks, against silent data loss on three production paths"*.
    // The test count matched ` tests` and was rewritten on **every commit**; the mutation
    // count did not match any marker and stayed frozen. So half the sentence tracked the
    // present, half described 2026-09-03, and nothing said which.
    //
    // At one commit the tool wrote **"0 tests"** into the audit's own verdict, and it shipped.
    //
    // This module's own documentation already asserted the right rule --- *"a historical
    // statement is about a moment and cannot rot, so it is left alone"* --- and had no way to
    // tell. Now it has one, written by the author of the sentence rather than inferred: the
    // marker below says *this figure is a record*, and the rewriter passes over the line.
    if line.contains(AS_MEASURED_THEN) {
        return found;
    }
    // The third column is whether a **word** may spell this figure.
    //
    // Not everywhere. Prose says "three tests hold it" about a local fact all the time, and
    // reading that as a claim about the whole suite would fill the gate with false alarms ---
    // which is how a check stops being read. Digits are safe there because nobody writes
    // "two thousand five hundred and ninety-six tests" except as the global claim.
    //
    // Words are allowed exactly where the figure is *only* ever a global count: how many
    // examples the SDK ships, how many invariants the gate runs. Those are the ones that
    // drifted, and they drifted in words.
    // The fourth column is a phrase the line must also contain.
    //
    // Without it, `" invariants"` matched "these two invariants" in prose about a single
    // component and reported it as a stale claim about the whole gate. A marker is only safe
    // when it can only ever mean the global figure; where it cannot, the context makes it so.
    for (marker, unit, words, context) in [
        (" tests", "tests", false, ""),
        (" specific defects", "mutations", false, ""),
        (" deliberate defects", "mutations", false, ""),
        // A third spelling, found because `REMEDIATION.md` used it to correct a stale figure
        // --- "the catalogue is **907 distinct defects, not 909**" --- and then went stale
        // itself. Two wrong numbers in one sentence about how many there are.
        (" distinct defects", "mutations", false, ""),
        (" sequential", "mutations", false, ""),
        (" runnable examples", "examples", true, ""),
        (" example scripts", "examples", true, ""),
        (" invariants", "checks", true, "check-all"),
    ] {
        if !context.is_empty() && !line.contains(context) {
            continue;
        }
        let mut from = 0usize;
        while let Some(at) = line.get(from..).and_then(|rest| rest.find(marker)) {
            let end = from + at;
            // Walk back over the digits and separators immediately before the marker ---
            // and over the emphasis around them.
            //
            // `**909** specific defects` stopped this walk dead on the first `*`, so it
            // collected nothing, the parse failed, and the figure was invisible. Two stale
            // counts were living behind exactly that: `docs/TESTING.md` claimed **909**
            // mutations and `docs/REMEDIATION.md` **907**, against a catalogue of 921, in the
            // two documents that describe this check. `TESTING.md` contradicted itself
            // eighty-eight lines apart and the tool built to catch that could not see either
            // number, because the author had made them bold.
            //
            // The emphasis is dropped from what is collected, so the rewriter still replaces
            // the digits and leaves the `**` where it was.
            let prefix = line.get(..end).unwrap_or("");
            let digits: String = prefix
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '*' || *c == '_')
                .collect::<Vec<char>>()
                .into_iter()
                .rev()
                .filter(|c| *c != '*' && *c != '_')
                .collect();
            if let Ok(value) = digits.replace(',', "").parse::<usize>() {
                found.push((value, unit, digits.clone()));
            } else if words {
                // Not digits. Take the word immediately before the marker, because prose
                // writes small counts out --- and every count that drifted here was small.
                let word = prefix.rsplit(|c: char| c.is_whitespace()).next().unwrap_or("");
                let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
                if let Some(value) = word_number(word) {
                    // The word itself, so the rewriter can tell it apart from a digit
                    // spelling and leave it alone. Rewriting "twenty-two" into "24" would
                    // make a sentence ungrammatical, and rewriting it by *digits* would edit
                    // some unrelated number on the same line.
                    found.push((value, unit, word.to_string()));
                }
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

#[cfg(test)]
mod rewriting {
    use super::{claimed_numbers, with_thousands};

    /// The exact text a figure was spelled with is what gets replaced.
    ///
    /// # The defect this pins
    ///
    /// The rewriter did two global `replace` calls per figure --- one for `2,610` and one for
    /// `2610` --- so a line holding more than one number could have the second call edit text
    /// the first had just written. It wrote `2,626099` into six documents at once, and the
    /// check then reported all six as stale, which is how it was noticed at all.
    #[test]
    fn a_figure_is_matched_with_the_text_that_spelled_it() {
        let found = claimed_numbers("2,610 tests and 773 deliberate defects");
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], (2610, "tests", "2,610".to_string()));
        assert_eq!(found[1], (773, "mutations", "773".to_string()));
    }

    /// A word-spelled figure is reported, and carries a word so the rewriter leaves it alone.
    ///
    /// Rewriting "twenty-two" to "24" makes the sentence wrong; rewriting it *by digits*
    /// edits whatever unrelated number shares the line. Reported and not rewritten is the
    /// only correct third option.
    #[test]
    fn a_word_spelled_figure_is_reported_but_not_rewritable() {
        let found = claimed_numbers("`check-all` runs twenty-two invariants");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0], (22, "checks", "twenty-two".to_string()));
        assert!(
            !found[0].2.starts_with(|c: char| c.is_ascii_digit()),
            "the rewriter decides by this, so it must not look like digits"
        );
    }

    /// Prose grouping and command-line grouping each stay as they were written.
    #[test]
    fn each_spelling_keeps_its_own_commas() {
        assert_eq!(with_thousands(2610), "2,610");
        assert_eq!(with_thousands(773), "773");
    }
}
