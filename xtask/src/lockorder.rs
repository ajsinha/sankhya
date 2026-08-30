//! No lock is taken while another is held, unless somebody said so.
//!
//! # Why a deadlock is not found by testing
//!
//! A race shows up under load: run it enough times and the bad interleaving happens. A deadlock
//! needs two threads to take two locks in opposite orders *at the same moment*, and if only one
//! call path holds two locks there is no order to reverse and no deadlock however hard the
//! system is hammered.
//!
//! That is what makes it dangerous. **The code that holds two locks is not the bug; the code
//! written six months later that holds them the other way round is.** By then the first
//! ordering is invisible --- it is one function among thousands, and nothing announces it.
//!
//! So this does not look for deadlocks. It looks for the *precondition* of one: a place where
//! two locks are held at once. Every such place must be declared, and declaring it names the
//! order, so the next person to hold both has something to be consistent with.
//!
//! # What this finds and what it cannot
//!
//! It finds a lock acquired while a guard binding is still in scope, syntactically, within one
//! function. It cannot see a lock taken inside a function called while a guard is held --- that
//! needs a call graph, and a lint that is half a call graph is one that reports confidently
//! about the half it has. What it does catch is the shape that has actually appeared here
//! twice in one day.

use crate::rust_files;
use std::path::Path;

/// Every place two locks are deliberately held at once, and the order they are taken in.
///
/// Empty, and it should stay that way. An entry is not a failure --- sometimes two locks
/// genuinely must be held --- but it is a commitment: every other path holding both must take
/// them in the same order, and adding the second entry is when this list starts earning itself.
const NESTED: &[(&str, &str)] = &[];

/// A guard binding: a `let` whose statement *ends* at the lock call.
///
/// The ending matters. `let cells = self.map.read().get(k).cloned();` acquires a lock and
/// releases it at the semicolon --- the binding is the value, not the guard. Treating that as a
/// held guard is how a checker like this produces noise nobody reads, and the first draft of
/// this one did exactly that on five sites out of seven.
fn guard_binding(code: &str) -> Option<String> {
    let trimmed = code.trim_end();
    let stripped = trimmed.strip_suffix(';')?;
    for ending in [".lock()", ".read()", ".write()"] {
        if let Some(head) = stripped.strip_suffix(ending) {
            let name = head
                .split('=')
                .next()?
                .trim()
                .strip_prefix("let ")?
                .trim_start_matches("mut ")
                .trim();
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Whether a line acquires a lock at all.
fn acquires(code: &str) -> bool {
    code.contains(".lock()") || code.contains(".read()") || code.contains(".write()")
}

/// No second lock is taken while a guard is live, unless the pair is declared.
pub(crate) fn check(root: &Path) -> bool {
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);

    let mut ok = true;
    let mut nested = 0usize;
    let mut scanned = 0usize;

    for file in &files {
        let rel = file.strip_prefix(root).unwrap_or(file).display().to_string();
        if rel.contains("/tests/") || rel.contains("/benches/") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        scanned += 1;

        let mut depth: i32 = 0;
        // Guards in scope, with the brace depth they were bound at.
        let mut held: Vec<(String, i32)> = Vec::new();

        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or(line);

            if acquires(code) && !held.is_empty() {
                let holding: Vec<&str> = held.iter().map(|(name, _)| name.as_str()).collect();
                let site = format!("{rel}:{}", number + 1);
                if NESTED.iter().any(|(where_, _)| where_ == &site) {
                    nested += 1;
                } else {
                    eprintln!(
                        "  NESTED LOCK    {site}: a lock is taken while {holding:?} is still \
                         held. Release the first, or declare the pair in `NESTED` with the \
                         order --- an ordering that exists only by accident is one the next \
                         change reverses"
                    );
                    ok = false;
                }
            }

            if let Some(name) = guard_binding(code) {
                held.push((name, depth));
            }

            depth += i32::try_from(code.matches('{').count()).unwrap_or(0);
            depth -= i32::try_from(code.matches('}').count()).unwrap_or(0);
            held.retain(|(_, at)| *at <= depth);
        }
    }

    if ok {
        println!("   {scanned} file(s) scanned, {nested} declared nested acquisition(s)");
    }
    ok
}
