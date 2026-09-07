//! The gate inventory, generated.
//!
//! Lifted out of `main.rs` when that file passed the fifteen-hundred-line ceiling `check-loc`
//! enforces --- the same rule that has split `wiring.rs` three times, doing its job on the
//! file that implements it.

use std::collections::BTreeSet;
use std::path::Path;

/// The gate table a document embeds still matches the one this binary would emit.
///
/// Between the markers, byte for byte. A table that is *nearly* right is the failure being
/// prevented --- three hand-typed copies each omitted a different five checks and each read as
/// complete.
pub fn embedded_table_is_current(root: &Path) -> bool {
    const BEGIN: &str = "<!-- BEGIN GATE TABLE -->";
    const END: &str = "<!-- END GATE TABLE -->";
    let doc = root.join("docs").join("TESTING.md");
    let Ok(text) = std::fs::read_to_string(&doc) else {
        eprintln!("  COULD NOT READ  {}", doc.display());
        return false;
    };
    let embedded = text
        .split_once(BEGIN)
        .and_then(|(_, rest)| rest.split_once(END))
        .map(|(inner, _)| inner.trim().to_string());
    let Some(embedded) = embedded else {
        eprintln!(
            "  NO GATE TABLE   {} does not embed the generated gate table between its markers",
            doc.display()
        );
        return false;
    };
    if embedded == table(&crate::known_checks()).trim() {
        return true;
    }
    eprintln!(
        "  STALE GATE TABLE  {} disagrees with `cargo run -p xtask -- gate-table`. A check was added or renamed and the table did not follow --- which is how three documents came to say twenty",
        doc.display()
    );
    false
}

/// The gate inventory, as a markdown table, for a document to embed rather than retype.
///
/// # Why this is generated
///
/// Three documents carried a hand-typed table of the checks `check-all` runs. All three said
/// **twenty**, all three were written when that was true, and none of them moved: by the time
/// anybody counted there were twenty-six, and the five missing from the book's copy were
/// `check-durability`, `check-mutation-coverage`, `check-benchmarks`, `check-attribution` and
/// `check-unsafety` --- which are, between them, the checks that keep durability and every
/// published speed figure honest. One of the three also listed `sweep` as running *outside*
/// `check-all` while stating correctly twenty-eight lines earlier that it runs inside it.
///
/// A hand-maintained inventory of a moving set is a second source of truth that decays in
/// silence, and this repository has now met that failure in the metric catalogue, the error
/// catalogue, the objectives table and here. So the table is derived from the dispatch arms
/// themselves, the same way `known_checks` already is, and `check-docs` compares what a
/// document contains against what this returns.
pub fn table(known: &BTreeSet<String>) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("| Check | What it refuses |\n|---|---|\n");
    for name in known {
        let _ = writeln!(out, "| `{name}` | {} |", purpose_of(&name));
    }
    out
}

/// One line saying what a check refuses, keyed by its name.
///
/// Held here rather than in a document because a check without a stated purpose is a check
/// nobody can decide to keep, and a purpose that lives in prose drifts from the check.
fn purpose_of(name: &str) -> &'static str {
    match name {
        "check-layers" => "a dependency pointing the wrong way through the layer graph",
        "check-loc" => "a source file past the length a person reads in a sitting",
        "check-vocabulary" => "a core crate naming a domain concept it must not know about",
        "check-dupes" => "two versions of one heavy dependency in the graph",
        "check-docs" => "a broken link, a stale pin claim, a crate that does not exist, or a document that will not say what it claims is built",
        "check-features" => "a feature flag that changes behaviour nothing tests",
        "check-lints" => "the denied lint set across every target, and any warning at all from the shipping build",
        "check-unsafety" => "a third crate writing `unsafe`, or an opt-out that outlived its reason",
        "check-attribution" => "a dependency whose licence notice did not follow it",
        "check-mutation-coverage" => "a crate that decides something and has no mutation entry",
        "check-benchmarks" => "a published speed figure that names nothing which produced it, or names something that does not exist",
        "check-objectives" => "a service-level objective missing from the table that reports on it",
        "check-durability" => "a durable writer that syncs its bytes and not its directory entry",
        "check-mutations" => "a catalogue entry that no longer matches the source it names",
        "check-catalogues" => "a metric nothing emits, an error code nothing can raise, or a pageable thing with no runbook",
        "check-logging" => "a log statement recording something a caller supplied",
        "check-build-tree" => "a `target/` large enough to take the machine with it",
        "check-package" => "a manifest naming an image nothing builds, or a drain shorter than the grace period",
        "check-doc-numbers" => "a figure in prose that no longer matches what produces it",
        "check-surfaces" => "a SQL surface or a crate that nothing can reach",
        "check-atomic-writes" => "a publish path that writes in place rather than renaming",
        "check-lock-order" => "a nested lock acquisition that is not declared",
        "check-kernels" => "a numeric kernel with no oracle to check it against",
        "check-writers" => "a second writer to a warehouse",
        "check-performance" => "the `NFR-PERF` objectives --- **not** part of `check-all`: it needs a quiet machine and minutes, and CI does not opt in",
        "check-invariants" => "a rule naming a check that nobody runs",
        "check-tests" => "the test suite, and a test that passes by not running",
        "check-concurrency" => "a commit path that stops scaling with tables",
        _ => "see `xtask/src/main.rs`",
    }
}
