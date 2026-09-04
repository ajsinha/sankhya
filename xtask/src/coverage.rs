//! Which crates the mutation catalogue has never had an opinion about.
//!
//! # Why a count of mutations is not coverage
//!
//! `check-mutations` proves every catalogue entry still matches the source it names, and the
//! audit itself proves the suite fails on each. Neither asks the question that matters most:
//! **which crates does the catalogue never mention at all?**
//!
//! The answer was eighteen, about nineteen thousand lines --- including the whole of M4's
//! graph algorithms, its SQL surface and the published extension API, in a milestone marked
//! "Complete. Every exit criterion met.", and `sankhya-cdc-model`, the `pgoutput` decoder the
//! README singles out as validated against a real PostgreSQL stream.
//!
//! A headline figure of seven hundred mutations reads as thorough. It says nothing about
//! where they are, and they were nowhere near some of the most load-bearing code here.

use std::collections::BTreeSet;
use std::path::Path;

/// Crates that carry no logic worth mutating, with the reason.
///
/// Listed rather than inferred from size, because "small" is not the property --- a
/// twenty-line function that decides an authorization is worth ten mutations. What these
/// share is that they declare rather than decide: a binary that parses no arguments, a trait
/// with no implementation here, a re-export.
const DECLARATIVE: &[(&str, &str)] = &[
    ("sankhya-cli", "a binary that forwards to the server crate; it decides nothing"),
    ("sankhya-mv", "a placeholder for materialised views; the logic is in sankhya-cube"),
    ("sankhya-objectstore", "a re-export of the object-store types the workspace pins"),
    ("sankhya-ports", "trait definitions; every implementation lives elsewhere"),
];

/// Every crate the mutation catalogue names.
fn covered(root: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(root.join("tools/mutation-audit.py")) else {
        return BTreeSet::new();
    };
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        // The crate is the last field of each tuple: `"sankhya-foo"),`
        if let Some(rest) = trimmed.strip_prefix('"') {
            if let Some(name) = rest.strip_suffix("\"),") {
                if name.starts_with("sankhya-") && !name.contains(' ') {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

/// Every workspace crate that holds source.
fn crates(root: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().join("Cargo.toml").exists())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Every crate that decides something is represented in the mutation catalogue.
#[must_use]
pub fn check(root: &Path) -> bool {
    println!("== check-mutation-coverage ==");
    let covered = covered(root);
    let declarative: BTreeSet<&str> = DECLARATIVE.iter().map(|(name, _)| *name).collect();

    let mut ok = true;
    let mut uncovered = Vec::new();
    for name in crates(root) {
        if covered.contains(&name) || declarative.contains(name.as_str()) {
            continue;
        }
        uncovered.push(name);
    }
    uncovered.sort();

    for name in &uncovered {
        eprintln!(
            "  NO MUTATIONS   {name} has no entry in the mutation catalogue, so nothing has \
             ever asked whether its tests would notice a defect"
        );
        ok = false;
    }

    // A permission that outlives its reason is the failure `check-unsafety` already guards
    // against, and the same applies here: a crate excused as declarative that has since grown
    // logic is excused for a reason that stopped being true.
    for (name, why) in DECLARATIVE {
        if covered.contains(*name) {
            eprintln!(
                "  STALE EXCUSE   {name} is listed as declarative ({why}) and now has \
                 mutation entries. Remove it from the list"
            );
            ok = false;
        }
    }

    if ok {
        println!(
            "   {} crate(s) represented, {} declarative by exception",
            covered.len(),
            DECLARATIVE.len()
        );
    }
    ok
}
