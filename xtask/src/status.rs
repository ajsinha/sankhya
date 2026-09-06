//! Whether the documents agree about what is built, and whether that agreement is true.
//!
//! Extracted from `main.rs` when it passed the 1500-line ceiling `check-loc` enforces ---
//! the same rule that has already split `wiring.rs` twice. A file nobody can read in a
//! sitting is a file whose checks nobody reviews.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The directory whose documents must all declare a status.
const BOOK: &str = "book";

/// Every document's `**Status:**` line says the same thing.
///
/// Documentation rot is usually not a false statement; it is two true-at-different-times
/// statements sitting in different files. This catches the specific case that has
/// actually happened here: four documents carried "Design phase" long after
/// implementation started, and two of those had been half-updated into "Implementation —
/// M0–M3 complete — no implementation has begun", which is a sentence that contradicts
/// itself and which nobody reading one document in isolation would notice.
///
/// Only files declaring a `**Status:**` header line participate. Prose status paragraphs
/// are left alone: this checks the machine-readable claim, not the writing.
///
/// # Except that opting out was free, and the book had taken it
///
/// A document with no header simply did not participate, and a repository-wide search for
/// `**Status:**` across `docs/book/` returned **nothing** --- twenty-seven chapters, none of
/// them visible to this check. Which is how four of them came to carry milestone claims the
/// canonical line contradicts, a digest six architecture decisions behind, and a requirement
/// count that disagrees with its own table.
///
/// That is worse than disagreement. A document that disagrees is caught here; a document
/// that declines to say anything is not, and declining costs nothing. So a chapter of the
/// book is now **required** to carry the line, by [`BOOK`] below. The rest of the repository
/// keeps the opt-out, because a runbook or an ADR is not making a claim about what is built.
pub(crate) fn check_status_agreement(root: &Path, docs: &[PathBuf]) -> bool {
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut silent: Vec<String> = Vec::new();

    for doc in docs {
        // Architecture decision records carry their own status vocabulary — Accepted,
        // Superseded — which is about the decision, not about the project. They are a
        // different kind of claim and are excluded rather than forced to agree.
        if doc.components().any(|c| c.as_os_str() == "adr") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(doc) else {
            continue;
        };
        let name = doc
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut declared = false;
        for line in text.lines().take(20) {
            if let Some(rest) = line.strip_prefix("**Status:** ") {
                seen.entry(rest.trim().to_string()).or_default().push(name.clone());
                declared = true;
                break;
            }
        }
        // A chapter of the book states what this system does, at length, to somebody
        // deciding whether to use it. Saying nothing about which of it is built is the
        // omission this check exists to catch, arriving as silence rather than as a
        // contradiction.
        if !declared && doc.components().any(|c| c.as_os_str() == BOOK) {
            silent.push(
                doc.strip_prefix(root)
                    .unwrap_or(doc)
                    .display()
                    .to_string(),
            );
        }
    }

    let mut ok = true;
    if !silent.is_empty() {
        for path in &silent {
            eprintln!(
                "  NO STATUS       {path} declares no `**Status:**` line, so nothing checks what it claims is built"
            );
        }
        ok = false;
    }

    if seen.len() > 1 {
        eprintln!("  DISAGREEMENT: documents state different statuses");
        for (status, files) in &seen {
            eprintln!("    {:<50} {}", status, files.join(", "));
        }
        return false;
    }

    let Some((status, files)) = seen.iter().next() else {
        return ok;
    };

    // Agreement is not accuracy.
    //
    // This check passed for a week while every document said "M0–M5 complete, M6 in
    // progress" and M7 was half built. Seven documents agreeing is exactly what a stale
    // line looks like: nothing disagrees with it, because they were all written at the same
    // moment and none of them has moved since.
    //
    // So the agreed line is checked against something that *does* move --- STATUS.md's
    // milestone table, which is edited as work lands. Every milestone that table calls
    // unfinished must be named in the status line.
    let unfinished = unfinished_milestones(root);
    for milestone in &unfinished {
        if !status.contains(milestone.as_str()) {
            eprintln!(
                "  STALE STATUS  the status line does not mention {milestone}, which \
                 docs/STATUS.md lists as in progress: {status:?}"
            );
            ok = false;
        }
    }
    // The README carries the same claim as a badge and a paragraph rather than a
    // `**Status:**` line, so the agreement check above cannot see it --- which is why it was
    // the last document still saying "M5 complete" after every other had moved.
    ok &= readme_names(root, &unfinished);

    if ok {
        println!(
            "   {} documents agree on status, and it names every milestone in progress \
             ({}): {status}",
            files.len(),
            if unfinished.is_empty() {
                "none".to_string()
            } else {
                unfinished.join(", ")
            }
        );
    }
    ok
}

/// Whether the README's badge and status section name every milestone in progress.
///
/// A separate check because the README states its status in two places and in neither of the
/// forms the rest of the documentation uses. Both are checked: a badge that disagrees with
/// the prose beneath it is the version most people see.
fn readme_names(root: &Path, in_progress: &[String]) -> bool {
    let Ok(text) = std::fs::read_to_string(root.join("README.md")) else {
        return true;
    };
    let badge: String = text
        .lines()
        .filter(|line| line.contains("img.shields.io/badge/status"))
        .collect();
    let status: String = text
        .split("## Status")
        .nth(1)
        .map(|rest| rest.lines().take(6).collect())
        .unwrap_or_default();

    let mut ok = true;
    for milestone in in_progress {
        if !badge.contains(milestone.as_str()) {
            eprintln!("  STALE STATUS  README.md's status badge does not mention {milestone}");
            ok = false;
        }
        if !status.contains(milestone.as_str()) {
            eprintln!("  STALE STATUS  README.md's Status section does not mention {milestone}");
            ok = false;
        }
    }
    ok
}

/// Milestones `docs/STATUS.md` describes as in progress.
///
/// Read from the milestone table rather than declared here, so that recording a milestone as
/// finished in one place is what makes the status line allowed to stop mentioning it. The
/// table is edited as work lands; the status line is not, which is the whole problem.
/// Whether a milestone row describes something not yet settled.
///
/// Three shapes, and the third is the one that hid: an explicit "in progress"; the hedge
/// "substantially"; and a **count of criteria** --- "four of five", "six of eight" --- which
/// is how the table says *some, not all* without using either other phrase.
///
/// Deliberately not triggered by "all six exit criteria met" or "closed 2026-08-26": the
/// first is a total and the second is a date. Only `<number> of <number>` counts.
fn is_unsettled(row: &str) -> bool {
    const NUMBERS: &[&str] = &[
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    if row.contains("in progress") || row.contains("substantially") {
        return true;
    }
    for first in NUMBERS {
        for second in NUMBERS {
            if row.contains(&format!("{first} of {second}")) {
                return true;
            }
        }
    }
    false
}

pub(crate) fn unfinished_milestones(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join("docs/STATUS.md")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("| **M") {
            continue;
        }
        let Some(name) = trimmed
            .strip_prefix("| **")
            .and_then(|rest| rest.split("**").next())
        else {
            continue;
        };
        // In flight *or* partly done --- not merely unfinished. A status line naming every
        // milestone nobody has started yet is noise, and noise is what gets skipped when
        // the line does need changing.
        //
        // "In progress" alone was the whole test for a week, and it let four milestones
        // through: this table said "Substantially complete", "Closed. Four of five exit
        // criteria met" and "Complete on six of eight" while ten documents and the README
        // all agreed on "M0--M8 complete". Agreement is not accuracy, and neither is a
        // keyword. What marks a milestone as unsettled is a *count* --- "four of five",
        // "six of eight" --- because that is how this table says "some, not all".
        if !is_unsettled(&trimmed.to_lowercase()) {
            continue;
        }
        for part in name.split(['–', '-']) {
            let part = part.trim().trim_start_matches("**");
            if part.starts_with('M') && part.len() >= 2 {
                out.push(part.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
