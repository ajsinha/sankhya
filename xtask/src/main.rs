//! Repository enforcement tooling.
//!
//! These checks are the mechanical half of the architecture: the layer DAG, the
//! file-length ceiling, the domain-vocabulary prohibition and the duplicate-version
//! gate. Each exists because the corresponding mistake is cheap to make, expensive
//! to unwind, and invisible in review.

mod catalogues;
mod logging;
mod atomicwrites;
mod buildtree;
mod docnumbers;
mod surfaces;
mod package;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const LOC_HARD: usize = 1500;
const LOC_WARN: usize = 800;

/// Layer 99 marks a domain pack, 100 tooling. The extension API sits at 15.
const LAYER_PACK: u32 = 99;
const LAYER_TOOLING: u32 = 100;

/// The critical dependency family. A duplicate version of any of these is a
/// correctness hazard, not an inefficiency: two Arrow majors make identically
/// named types incompatible. See docs/adr/0001-dependency-pin-set.md.
const CRITICAL_FAMILY: &[&str] = &[
    "arrow",
    "arrow-array",
    "arrow-schema",
    "arrow-buffer",
    "arrow-ipc",
    "parquet",
    "datafusion",
    "object_store",
    "delta_kernel",
];

/// Duplicates that are permitted because they never cross a SANKHYA API boundary.
/// Every entry is a deliberate decision, not an accumulation.
const DUP_ALLOWLIST: &[&str] = &[
    // Build-time CPU-feature detection for `sha2`, which the audit chain needs. No state,
    // no wire format, nothing that crosses an API boundary — two copies cost a few
    // kilobytes and can differ in no observable way. Chosen over `sha2 0.11`, which is a
    // generation ahead of the rest of the RustCrypto stack here and brings three
    // duplicates (`digest`, `crypto-common`, `block-buffer`) instead of this one.
    "cpufeatures",
    "base64",
    "foldhash",
    "getrandom",
    "hashbrown",
    "itertools",
    "rand",
    "rand_core",
    "syn",
    "windows-sys",
    "r-efi",
    "wasi",
    "windows-targets",
    "windows_x86_64_gnu",
    "windows-link",
    "generic-array",
    "bitflags",
    "heck",
    "regex-automata",
    "regex-syntax",
    "socket2",
    "winnow",
    // proptest pulls an older chacha; it is a dev dependency and never reaches a
    // SANKHYA API boundary.
    "rand_chacha",
    // Both reach the tree only through delta_kernel_default_engine, which is a
    // *dev*-dependency used as an oracle: it reads the Delta log SANKHYA writes and must
    // agree about the live set. Neither appears in any production dependency path, and
    // that is checked below rather than assumed.
    "reqwest",
    "core-foundation",
    // toml's own datetime type, internal to manifest parsing in tooling.
    "toml_datetime",
    "toml_parser",
    "toml_writer",
    "serde_spanned",
    // Pulled at two versions through the query engine's expression features. Both are
    // internal hashing and bignum utilities; neither appears in any SANKHYA signature,
    // so neither can cause the type incompatibility this gate exists to prevent.
    "ahash",
    "num-bigint",
];

/// Domain nouns that must not appear in core crates. The general-purpose claim is
/// that the core knows about tenants, tables, columns, edges and versions — and
/// nothing about any industry.
///
/// This lint catches *leakage*. It cannot catch *shape*: a core can be immaculately
/// neutral in its naming and still be bent toward one domain. Only the reference
/// packs catch that. Both mechanisms are needed.
const DOMAIN_WORDS: &[&str] = &[
    "trade",
    "counterparty",
    "notional",
    "portfolio",
    "basel",
    "isin",
    "cusip",
    "ledger",
    "aml",
    "kyc",
    "ubo",
    "laundering",
    "desk",
    "book_id",
    "shipment",
    "consignment",
    "patient",
    "icd10",
    "diagnosis_code",
    "sensor_reading",
    "invoice",
    "sku",
    // Entries must be DISTINCTIVELY domain-specific, never ordinary English that a
    // domain also happens to use. Two have been removed for exactly that reason:
    //
    //   "claim"     — the natural verb for "these two tiers claim the same positions"
    //   "diagnosis" — the natural noun for "that would delay the diagnosis"
    //
    // A lint that fires on ordinary prose gets worked around or switched off, which is
    // worse than a narrower lint that is always obeyed. Where a domain sense genuinely
    // needs catching, use a compound that cannot occur by accident: "claim_id",
    // "diagnosis_code".
];

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let root = repo_root();
    let mut failed = false;

    let run_all = task.is_empty() || task == "check-all";

    if run_all || task == "check-tests" {
        failed |= !check_tests(&root);
    }
    if run_all || task == "check-invariants" {
        failed |= !check_invariants(&root);
    }
    if run_all || task == "check-surfaces" {
        failed |= !surfaces::check(&root);
    }
    if run_all || task == "check-atomic-writes" {
        failed |= !atomicwrites::check(&root);
    }
    if run_all || task == "check-writers" {
        failed |= !check_writers(&root);
    }
    if run_all || task == "check-layers" {
        failed |= !check_layers(&root);
    }
    if run_all || task == "check-loc" {
        failed |= !check_loc(&root);
    }
    if run_all || task == "check-vocabulary" {
        failed |= !check_vocabulary(&root);
    }
    if run_all || task == "check-dupes" {
        failed |= !check_dupes(&root);
    }
    if run_all || task == "check-docs" {
        failed |= !check_docs(&root);
    }
    if run_all || task == "check-features" {
        failed |= !check_features(&root);
    }
    if run_all || task == "check-lints" {
        failed |= !check_lints(&root);
    }
    if run_all || task == "check-mutations" {
        failed |= !check_mutations(&root);
    }
    if run_all || task == "check-catalogues" {
        failed |= !catalogues::check(&root);
    }
    // Not a check: it writes. Kept out of `check-all` for that reason.
    if task == "write-catalogues" {
        match catalogues::write(&root) {
            Ok(()) => println!("wrote {} and {}", catalogues::METRICS_DOC, catalogues::ERRORS_DOC),
            Err(error) => {
                eprintln!("could not write the catalogues: {error}");
                failed = true;
            }
        }
    }
    if run_all || task == "check-logging" {
        failed |= !logging::check(&root);
    }
    if run_all || task == "check-build-tree" {
        failed |= !buildtree::check(&root);
    }
    if run_all || task == "check-package" {
        failed |= !package::check(&root);
    }
    if task == "sync-doc-numbers" {
        let mut docs = Vec::new();
        collect_markdown(&root, &mut docs);
        failed |= !docnumbers::sync(&root, &docs);
    }
    if run_all || task == "check-doc-numbers" {
        let mut docs = Vec::new();
        collect_markdown(&root, &mut docs);
        failed |= !docnumbers::check(&root, &docs);
    }
    // Last, and part of `check-all` on purpose: `check-tests` has just rebuilt the
    // workspace, so this is the moment the superseded generation exists and is identifiable.
    // A sweep that runs before the build sweeps the wrong thing.
    if run_all || task == "sweep" {
        failed |= !buildtree::sweep(&root, false);
    }
    if task == "sweep-dry-run" {
        failed |= !buildtree::sweep(&root, true);
    }
    // Deliberately not in `check-all`: it generates a scale-factor-1 dataset and runs
    // for minutes, and it needs a machine that is not otherwise busy. It belongs to the
    // performance pipeline, which runs it on its own.
    if task == "check-performance" {
        failed |= !check_performance(&root);
    }
    if !run_all
        && !matches!(
            task.as_str(),
            "check-layers"
                | "check-loc"
                | "check-vocabulary"
                | "check-dupes"
                | "check-docs"
                | "check-features"
                | "check-lints"
                | "check-mutations"
                | "check-doc-numbers"
                | "check-writers"
                | "check-invariants"
                | "check-tests"
                | "check-logging"
                | "check-package"
                | "check-build-tree"
                | "check-surfaces"
                | "check-atomic-writes"
                | "sweep"
                | "sync-doc-numbers"
                | "sweep-dry-run"
                | "check-catalogues"
                | "write-catalogues"
                | "check-performance"
        )
    {
        eprintln!(
            "usage: cargo xtask \
             [check-all|check-tests|check-invariants|check-writers|check-layers|check-loc|check-vocabulary|check-dupes|check-docs\
             |check-features|check-lints|check-mutations|check-doc-numbers\
             |check-catalogues|write-catalogues|check-logging|check-package|check-build-tree|check-surfaces|check-atomic-writes|sweep|sweep-dry-run|sync-doc-numbers|check-performance]"
        );
        return ExitCode::from(2);
    }

    if failed {
        eprintln!("\nxtask: FAILED");
        ExitCode::from(1)
    } else {
        println!("\nxtask: all checks passed");
        ExitCode::SUCCESS
    }
}

fn repo_root() -> PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if p.join("Cargo.toml").exists() && p.join("crates").exists() {
            return p;
        }
        match p.parent() {
            Some(parent) => p = parent.to_path_buf(),
            None => return std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

struct Crate {
    name: String,
    layer: u32,
    deps: Vec<String>,
}

fn load_crates(root: &Path) -> Vec<Crate> {
    let mut out = Vec::new();
    for group in ["crates", "packs"] {
        let dir = root.join(group);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let manifest = e.path().join("Cargo.toml");
            if !manifest.exists() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let v: toml::Table = match toml::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("  UNPARSEABLE  {}: {e}", manifest.display());
                    continue;
                }
            };

            let name = v
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string();

            let layer = v
                .get("package")
                .and_then(|p| p.get("metadata"))
                .and_then(|m| m.get("sankhya"))
                .and_then(|s| s.get("layer"))
                .and_then(|l| l.as_integer())
                .unwrap_or(-1);

            let deps = v
                .get("dependencies")
                .and_then(|d| d.as_table())
                .map(|t| t.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();

            if layer < 0 {
                eprintln!("  MISSING LAYER  {name} — add [package.metadata.sankhya] layer = N");
                continue;
            }
            out.push(Crate {
                name,
                layer: layer as u32,
                deps,
            });
        }
    }
    out
}

/// Dependencies point downward only. Packs are additionally restricted to a small
/// allowance of core crates — when a pack legitimately needs another, the build
/// fails, and that failure *is* the signal that the extension API has a gap.
fn check_layers(root: &Path) -> bool {
    println!("== check-layers ==");
    let crates = load_crates(root);
    let by_name: BTreeMap<&str, &Crate> = crates.iter().map(|c| (c.name.as_str(), c)).collect();
    let pack_allowance = ["sankhya-ext", "sankhya-types", "sankhya-error"];
    let mut ok = true;

    for c in &crates {
        for d in &c.deps {
            let Some(dep) = by_name.get(d.as_str()) else {
                continue;
            }; // external crate
            if c.layer == LAYER_PACK {
                if !pack_allowance.contains(&d.as_str()) {
                    eprintln!(
                        "  PACK DEP     {} -> {} : packs may depend only on {:?}.\n\
                         {:16}This failure means the extension API has a gap. Widen the API, not the allowance.",
                        c.name, d, pack_allowance, ""
                    );
                    ok = false;
                }
                continue;
            }
            if dep.layer == LAYER_PACK {
                eprintln!(
                    "  CORE->PACK   {} -> {} : no core crate may depend on a pack",
                    c.name, d
                );
                ok = false;
                continue;
            }
            if dep.layer == LAYER_TOOLING {
                eprintln!(
                    "  ->TOOLING    {} -> {} : tooling is not a dependency",
                    c.name, d
                );
                ok = false;
                continue;
            }
            // Strictly upward is always wrong. Same-layer is permitted — several
            // vocabulary crates legitimately build on one another — but only if the
            // graph stays acyclic, which is checked separately below.
            if dep.layer > c.layer && c.layer != LAYER_TOOLING {
                eprintln!(
                    "  UPWARD DEP   {} (L{}) -> {} (L{}) : dependencies point downward only",
                    c.name, c.layer, d, dep.layer
                );
                ok = false;
            }
        }
    }
    if let Some(cycle) = find_cycle(&crates, &by_name) {
        eprintln!("  CYCLE        {}", cycle.join(" -> "));
        ok = false;
    }

    println!(
        "   {} crates, dependency direction {}",
        crates.len(),
        if ok { "OK, acyclic" } else { "VIOLATED" }
    );
    ok
}

/// Depth-first cycle detection over the internal dependency graph.
///
/// Same-layer dependencies are allowed, so acyclicity is no longer implied by the
/// layer numbers alone.
///
/// In practice cargo rejects a cyclic *normal* dependency before this ever runs, so
/// this is defence in depth rather than the primary gate. It stays because it costs
/// nothing, it documents the invariant, and it still catches cycles that cargo
/// tolerates — notably through dev-dependencies, which this check deliberately
/// ignores for exactly that reason but which a future check may not.
fn find_cycle(crates: &[Crate], by_name: &BTreeMap<&str, &Crate>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    let mut marks: BTreeMap<&str, Mark> = BTreeMap::new();
    let mut stack: Vec<String> = Vec::new();

    fn visit<'a>(
        name: &'a str,
        by_name: &BTreeMap<&'a str, &'a Crate>,
        marks: &mut BTreeMap<&'a str, Mark>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        match marks.get(name) {
            Some(Mark::Done) => return None,
            Some(Mark::Open) => {
                let mut cycle = stack.clone();
                cycle.push(name.to_string());
                return Some(cycle);
            }
            None => {}
        }
        let Some(c) = by_name.get(name) else {
            return None;
        };
        marks.insert(c.name.as_str(), Mark::Open);
        stack.push(name.to_string());
        for d in &c.deps {
            if let Some(dep) = by_name.get(d.as_str()) {
                if let Some(cycle) = visit(dep.name.as_str(), by_name, marks, stack) {
                    return Some(cycle);
                }
            }
        }
        stack.pop();
        marks.insert(c.name.as_str(), Mark::Done);
        None
    }

    for c in crates {
        if let Some(cycle) = visit(c.name.as_str(), by_name, &mut marks, &mut stack) {
            return Some(cycle);
        }
    }
    None
}

fn code_lines(src: &str) -> usize {
    let mut n = 0usize;
    let mut in_block = false;
    for raw in src.lines() {
        let mut line = raw.trim();
        if in_block {
            match line.find("*/") {
                Some(i) => {
                    in_block = false;
                    line = line[i + 2..].trim();
                }
                None => continue,
            }
        }
        while let Some(i) = line.find("/*") {
            let head = line[..i].trim();
            match line[i..].find("*/") {
                Some(j) => line = &line[i + j + 2..],
                None => {
                    in_block = true;
                    line = "";
                }
            }
            if !head.is_empty() {
                n += 1;
                if in_block {
                    break;
                }
                continue;
            }
            if in_block {
                break;
            }
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        n += 1;
    }
    n
}

pub(crate) fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if p.is_dir() {
            if matches!(
                name,
                "target" | ".git" | "generated" | "snapshots" | "corpus"
            ) {
                continue;
            }
            rust_files(&p, out);
        } else if name.ends_with(".rs") {
            out.push(p);
        }
    }
}

fn check_loc(root: &Path) -> bool {
    println!("== check-loc ==");
    let mut files = Vec::new();
    for g in ["crates", "packs", "xtask"] {
        rust_files(&root.join(g), &mut files);
    }
    let mut ok = true;
    let mut warned = 0;
    let mut largest = (0usize, String::new());
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        let n = code_lines(&src);
        let rel = f.strip_prefix(root).unwrap_or(f).display().to_string();
        if n > largest.0 {
            largest = (n, rel.clone());
        }
        if n > LOC_HARD {
            eprintln!("  TOO LONG     {rel}: {n} code lines (hard limit {LOC_HARD})");
            ok = false;
        } else if n > LOC_WARN {
            println!("  approaching  {rel}: {n} code lines (warn at {LOC_WARN})");
            warned += 1;
        }
    }
    println!(
        "   {} files, largest {} at {} lines, {} approaching the limit",
        files.len(),
        largest.1,
        largest.0,
        warned
    );
    ok
}

/// Core crates may not name a domain concept.
fn check_vocabulary(root: &Path) -> bool {
    println!("== check-vocabulary ==");
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    let mut ok = true;
    let mut hits = 0;
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        let rel = f.strip_prefix(root).unwrap_or(f).display().to_string();
        // The generator and testkit legitimately construct example schemas.
        if rel.contains("sankhya-datagen") || rel.contains("sankhya-testkit") {
            continue;
        }
        let lower = src.to_lowercase();
        for w in DOMAIN_WORDS {
            let mut from = 0usize;
            while let Some(i) = lower[from..].find(w) {
                let at = from + i;
                let before = lower[..at].chars().next_back().unwrap_or(' ');
                let after = lower[at + w.len()..].chars().next().unwrap_or(' ');
                let boundary = |c: char| !(c.is_alphanumeric() || c == '_');
                if boundary(before) && boundary(after) {
                    let line = lower[..at].matches('\n').count() + 1;
                    eprintln!("  DOMAIN WORD  {rel}:{line}: '{w}' must not appear in a core crate");
                    hits += 1;
                    ok = false;
                }
                from = at + w.len();
            }
        }
    }
    println!("   {} core files scanned, {hits} violations", files.len());
    ok
}

/// A duplicate in the critical family is a correctness hazard. Everything else is
/// judged against a deliberate allowlist.
fn check_dupes(root: &Path) -> bool {
    println!("== check-dupes ==");
    let lock = root.join("Cargo.lock");
    if !lock.exists() {
        println!("   no Cargo.lock yet — skipped");
        return true;
    }
    let Ok(text) = std::fs::read_to_string(&lock) else {
        return true;
    };
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("name = \"") {
            name = v.trim_end_matches('"').to_string();
        } else if let Some(v) = line.strip_prefix("version = \"") {
            if !name.is_empty() {
                seen.entry(name.clone())
                    .or_default()
                    .push(v.trim_end_matches('"').to_string());
                name.clear();
            }
        }
    }
    let mut ok = true;
    let mut benign = 0;
    for (n, versions) in seen.iter().filter(|(_, v)| v.len() > 1) {
        if CRITICAL_FAMILY.contains(&n.as_str()) {
            eprintln!("  CRITICAL DUP {n}: {versions:?} — two majors of this family are type-incompatible");
            ok = false;
        } else if DUP_ALLOWLIST.contains(&n.as_str()) {
            benign += 1;
        } else {
            eprintln!(
                "  NEW DUP      {n}: {versions:?} — review, then allowlist deliberately or remove"
            );
            ok = false;
        }
    }
    println!(
        "   {} packages, {benign} allowlisted duplicates, critical family single-versioned: {}",
        seen.len(),
        !seen
            .iter()
            .any(|(n, v)| CRITICAL_FAMILY.contains(&n.as_str()) && v.len() > 1)
    );
    ok
}

/// Documentation rot, caught mechanically.
///
/// Prose drifts from code silently — nothing fails, nothing warns, and the gap is
/// discovered by a reader who then stops trusting the rest of the document. These
/// checks catch the mechanically checkable half: broken links, stale version claims,
/// and documents that reference crates or decision records that do not exist.
///
/// The other half — whether the prose still *describes* what the code does — is not
/// mechanically checkable and remains a review responsibility. Saying so here is
/// deliberate: a check that implied otherwise would be worse than no check.
fn check_docs(root: &Path) -> bool {
    println!("== check-docs ==");
    let mut ok = true;
    let mut docs = Vec::new();
    collect_markdown(root, &mut docs);

    let pins = workspace_pins(root);
    let crate_names: std::collections::BTreeSet<String> =
        load_crates(root).into_iter().map(|c| c.name).collect();

    let mut links = 0usize;
    let mut versions = 0usize;

    for doc in &docs {
        let Ok(text) = std::fs::read_to_string(doc) else {
            continue;
        };
        let rel = doc.strip_prefix(root).unwrap_or(doc).display().to_string();
        let dir = doc.parent().unwrap_or(root);

        // (a) Relative links must resolve.
        for target in markdown_link_targets(&text) {
            if target.starts_with("http")
                || target.starts_with('#')
                || target.starts_with("mailto:")
            {
                continue;
            }
            let path = target.split('#').next().unwrap_or(&target);
            if path.is_empty() {
                continue;
            }
            let candidate = dir.join(path);
            let alt = root.join(path);
            if !candidate.exists() && !alt.exists() {
                eprintln!("  BROKEN LINK  {rel}: {target}");
                ok = false;
            }
            links += 1;
        }

        // (b) A pinned version quoted in prose must match the workspace pin.
        //     The pin table is quoted in several documents; when it moves and the
        //     prose does not, every number a reader checks is wrong.
        for (name, pinned) in &pins {
            for quoted in quoted_versions(&text, name) {
                versions += 1;
                if &quoted != pinned {
                    eprintln!(
                        "  STALE VERSION {rel}: says {name} {quoted}, workspace pins {pinned}"
                    );
                    ok = false;
                }
            }
        }

        // (c) A crate named in backticks must exist.
        for referenced in backticked_crate_names(&text) {
            if referenced.starts_with("sankhya-") && !crate_names.contains(&referenced) {
                eprintln!("  MISSING CRATE {rel}: references `{referenced}`, which does not exist");
                ok = false;
            }
        }
    }

    println!(
        "   {} documents, {links} relative links, {versions} version claims checked",
        docs.len()
    );

    ok &= check_named_sources(root, &docs);
    ok &= check_status_agreement(root, &docs);
    ok &= check_the_motto(root, &docs);

    ok
}

/// The motto every document carries, and where.
///
/// *"To count is to make completely known."* --- the reading of **सम् + √ख्या** that the
/// README's opening paragraph draws out: to enumerate a thing, in Sanskrit, is to make it
/// completely known.
const MOTTO: &str = "To count is to make completely known.";

/// Every document carrying the wordmark carries the motto beneath it.
///
/// # Why this is checked rather than remembered
///
/// A convention applied to twenty-one documents by hand is a convention that holds until the
/// twenty-second is written, and the twenty-second is always written by somebody who has not
/// read the other twenty-one. Then the set is *mostly* consistent, which reads as carelessness
/// rather than as a rule.
///
/// Only documents that already carry the wordmark are held to it: the wordmark is what marks a
/// page as one of this project's own, and a README inside a fixture directory is not.
fn check_the_motto(root: &Path, docs: &[PathBuf]) -> bool {
    let mut ok = true;
    let mut carried = 0usize;
    for doc in docs {
        let Ok(text) = std::fs::read_to_string(doc) else {
            continue;
        };
        if !text.contains("wordmark-dice") {
            continue;
        }
        let rel = doc.strip_prefix(root).unwrap_or(doc).display().to_string();
        // Near the top, which is the whole point of a motto. Measured in lines rather than
        // bytes so a wide header does not push it out of range.
        let head: String = text.lines().take(20).collect::<Vec<&str>>().join("\n");
        if head.contains(MOTTO) {
            carried += 1;
        } else {
            eprintln!("  NO MOTTO     {rel}: the first 20 lines do not carry \"{MOTTO}\"");
            ok = false;
        }
    }
    println!("   {carried} document(s) carry the motto beneath the wordmark");
    ok
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if p.is_dir() {
            if matches!(name, "target" | ".git" | ".build" | "vendor" | "spikes") {
                continue;
            }
            collect_markdown(&p, out);
        } else if name.ends_with(".md") {
            out.push(p);
        }
    }
}

fn markdown_link_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == ']' && i + 1 < bytes.len() && bytes[i + 1] == '(' {
            let mut j = i + 2;
            let mut target = String::new();
            while j < bytes.len() && bytes[j] != ')' {
                target.push(bytes[j]);
                j += 1;
            }
            if !target.is_empty() && !target.contains(' ') {
                out.push(target);
            }
            i = j;
        }
        i += 1;
    }
    out
}

/// Versions quoted next to a crate name, in prose or in a table cell.
fn quoted_versions(text: &str, crate_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(i) = text[from..].find(crate_name) {
        let at = from + i;
        let before = text[..at].chars().next_back().unwrap_or(' ');
        let rest = &text[at + crate_name.len()..];
        let after = rest.chars().next().unwrap_or(' ');
        let boundary = |c: char| !(c.is_alphanumeric() || c == '_' || c == '-');
        if boundary(before) && boundary(after) {
            // Accept "name 1.2.3", "name` 1.2.3", "name | 1.2.3", "name = "1.2.3"".
            let window: String = rest.chars().take(24).collect();
            if let Some(v) = leading_version(&window) {
                out.push(v);
            }
        }
        from = at + crate_name.len();
    }
    out
}

fn leading_version(window: &str) -> Option<String> {
    let trimmed =
        window.trim_start_matches(|c: char| matches!(c, '`' | ' ' | '|' | '=' | '"' | '*' | ':'));
    let mut digits = String::new();
    for c in trimmed.chars() {
        if c.is_ascii_digit() || c == '.' {
            digits.push(c);
        } else {
            break;
        }
    }
    // Require at least major.minor.patch so "arrow 59" in prose is not treated as a
    // precise claim.
    (digits.matches('.').count() == 2 && digits.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .then_some(digits)
}

fn backticked_crate_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else { break };
        let inner = &after[..end];
        if inner.starts_with("sankhya-")
            && inner
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
        {
            out.push(inner.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

/// The exact-pinned versions from the workspace dependency table.
fn workspace_pins(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return out;
    };
    let Ok(v) = toml::from_str::<toml::Table>(&text) else {
        return out;
    };
    let Some(deps) = v
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table)
    else {
        return out;
    };
    for (name, spec) in deps {
        let raw = match spec {
            toml::Value::String(s) => Some(s.clone()),
            toml::Value::Table(t) => t
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            _ => None,
        };
        // Only exact pins are claims a document can be checked against.
        if let Some(pinned) = raw.and_then(|r| r.strip_prefix('=').map(str::to_string)) {
            out.insert(name.clone(), pinned);
        }
    }
    out
}

/// Features a dependency must declare because our own defaults require them.
///
/// # Why this check exists
///
/// Cargo unifies features across a crate's dependencies *and its dev-dependencies*.
/// A library crate can therefore pass its entire test suite while missing a feature its
/// public API needs, because a test-only dependency happened to enable it. The library
/// is broken for every real consumer and its own tests cannot tell.
///
/// That is not hypothetical: the Parquet writer's default compression is Zstandard, the
/// workspace pin did not enable `zstd`, and `sankhya-table`'s tests passed anyway
/// because DataFusion — a dev-dependency — turned it on. The defect surfaced only when
/// a second crate depended on the writer without also depending on DataFusion.
///
/// Checking the manifest rather than the resolved graph is deliberate: the resolved
/// graph is exactly the thing that hides the problem.
const REQUIRED_FEATURES: &[(&str, &[(&str, &str)])] = &[(
    "parquet",
    &[
        (
            "zstd",
            "WriterConfig::default() emits Zstandard; without this feature every write \
             panics inside the column writer",
        ),
        (
            "snap",
            "Snappy is the format's most widely written codec; we must be able to read \
             files other engines produced",
        ),
    ],
)];

fn check_features(root: &Path) -> bool {
    println!("== check-features ==");

    let manifest_path = root.join("Cargo.toml");
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            println!("  FAIL: reading {}: {e}", manifest_path.display());
            return false;
        }
    };
    let doc: toml::Table = match toml::from_str(&text) {
        Ok(d) => d,
        Err(e) => {
            println!("  FAIL: parsing {}: {e}", manifest_path.display());
            return false;
        }
    };

    let Some(deps) = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table)
    else {
        println!("  FAIL: [workspace.dependencies] is missing");
        return false;
    };

    let mut ok = true;
    for (crate_name, required) in REQUIRED_FEATURES {
        let Some(spec) = deps.get(*crate_name) else {
            println!("  FAIL: {crate_name} is not a workspace dependency");
            ok = false;
            continue;
        };
        let declared: Vec<&str> = spec
            .get("features")
            .and_then(toml::Value::as_array)
            .map(|a| a.iter().filter_map(toml::Value::as_str).collect())
            .unwrap_or_default();

        for (feature, because) in *required {
            if declared.contains(feature) {
                println!("  ok   {crate_name}/{feature}");
            } else {
                println!("  FAIL {crate_name}/{feature} is not declared — {because}");
                ok = false;
            }
        }
    }

    ok &= check_dev_only(root);

    if ok {
        println!("  all required features are declared on the workspace pin");
    }
    ok
}

/// Crates that must never appear in a production dependency path.
///
/// A test-only dependency is a claim about the shipped binary, and a claim in a comment
/// decays. `delta_kernel_default_engine` is used as an oracle — it reads the Delta log
/// SANKHYA writes and must agree about the live set — and moving it into
/// `[dependencies]` would pull eighty-four packages and a duplicated HTTP client into
/// the binary while looking like a one-line change.
const DEV_ONLY: &[(&str, &str)] = &[(
    "delta_kernel_default_engine",
    "it is an oracle for the log writer, not an I/O layer; DEC-06 couples to storage \
     metadata only",
)];

fn check_dev_only(root: &Path) -> bool {
    let mut ok = true;
    let crates_dir = root.join("crates");
    let Ok(entries) = std::fs::read_dir(&crates_dir) else {
        println!("  FAIL: {} is unreadable", crates_dir.display());
        return false;
    };

    for entry in entries.flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(doc) = toml::from_str::<toml::Table>(&text) else {
            continue;
        };
        let Some(deps) = doc.get("dependencies").and_then(toml::Value::as_table) else {
            continue;
        };

        for (name, because) in DEV_ONLY {
            if deps.contains_key(*name) {
                println!(
                    "  FAIL {} lists {name} as a production dependency — {because}",
                    entry.file_name().to_string_lossy()
                );
                ok = false;
            }
        }
    }

    if ok {
        for (name, _) in DEV_ONLY {
            println!("  ok   {name} is test-only");
        }
    }
    ok
}

/// The `NFR-PERF-*` objectives, run as a gate.
///
/// Separate from `check-all` because it costs minutes and needs a quiet machine, and a
/// check that people learn to skip is worse than one they have to invoke. This is the
/// command the performance pipeline runs; `docs/IMPLEMENTATION_PLAN.md` M3 exit
/// criterion 1 says "in the pipeline", and this is what makes that phrase mean
/// something a build can fail on.
fn check_performance(root: &Path) -> bool {
    println!("== check-performance");
    let status = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "test",
            "-p",
            "sankhya-olap",
            "--test",
            "tpch",
            "--release",
            "--",
            "--ignored",
            "--nocapture",
            "--exact",
            "the_performance_objectives_are_met",
        ])
        // The objectives are stated at scale factor 1. Running them at anything else
        // measures a different requirement.
        .env("SANKHYA_TPCH_SCALE", "1")
        .status();

    match status {
        Ok(status) if status.success() => {
            println!("   objectives met");
            true
        }
        Ok(_) => {
            eprintln!("   FAILED: at least one objective is not met");
            false
        }
        Err(error) => {
            eprintln!("   FAILED: could not run the gate: {error}");
            false
        }
    }
}

/// Clippy across every target, with the workspace's denied lints.
///
/// In `check-all` because the denied set is a safety policy, not a style preference:
/// `unwrap`, `expect`, `panic` and unchecked indexing are refused in library code
/// because a server must not abort on data it did not choose. A policy that does not
/// run is not a policy — this was declared in `Cargo.toml` from the start and had never
/// been enforced by anything, and the library code had accumulated violations in six
/// crates, including a wire decoder indexing attacker-supplied bytes.
///
/// Test targets allow the same lints, stated file by file rather than globally, because
/// a test panicking is how a test fails.
fn check_lints(root: &Path) -> bool {
    println!("== check-lints");
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["clippy", "--workspace", "--all-targets", "--keep-going"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            println!("   clean across every target");
            true
        }
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stderr);
            let count = text.lines().filter(|l| l.starts_with("error")).count();
            eprintln!("   FAILED: {count} clippy error(s)");
            for line in text.lines().filter(|l| l.starts_with("error")).take(10) {
                eprintln!("     {line}");
            }
            false
        }
        Err(error) => {
            eprintln!("   FAILED: could not run clippy: {error}");
            false
        }
    }
}

/// Enumerate tests without running them.
pub(crate) fn list_tests(root: &Path, ignored_only: bool) -> Option<usize> {
    let mut arguments = vec!["test", "--workspace", "--", "--list"];
    if ignored_only {
        arguments.push("--ignored");
    }
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(&arguments)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().filter(|line| line.ends_with(": test")).count())
}

/// Every mutation-catalogue entry still matches the source it names.
///
/// This is the catalogue's staleness check without the audit that follows it: no
/// compilation, no test run, milliseconds. It is separate from the audit precisely so it
/// can be cheap enough to gate every build.
///
/// It catches two failures that a diff review does not. A refactor moves the code an
/// entry names, and the entry goes on reporting a pass while proving nothing --- four
/// entries had drifted this way. And an audit run killed hard enough to defeat its
/// in-flight record leaves a deliberate defect applied in the tree; one such defect
/// reached a commit here, a comparison that stopped flipping `5 < x` into `x > 5`, which
/// makes the reader skip files that do hold matching rows. Both are invisible in the
/// working tree and neither announces itself.
fn check_mutations(root: &Path) -> bool {
    println!("== check-mutations");
    let output = Command::new("python3")
        .current_dir(root)
        .args(["tools/mutation-audit.py", "--check"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            println!("   every catalogue entry matches its source");
            true
        }
        Ok(output) => {
            eprintln!("   FAILED: the catalogue and the source disagree");
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines().take(20) {
                eprintln!("     {line}");
            }
            false
        }
        Err(error) => {
            eprintln!("   FAILED: could not run the mutation audit: {error}");
            false
        }
    }
}

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
fn check_status_agreement(root: &Path, docs: &[PathBuf]) -> bool {
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();

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
        for line in text.lines().take(20) {
            if let Some(rest) = line.strip_prefix("**Status:** ") {
                seen.entry(rest.trim().to_string()).or_default().push(name);
                break;
            }
        }
    }

    if seen.len() > 1 {
        eprintln!("  DISAGREEMENT: documents state different statuses");
        for (status, files) in &seen {
            eprintln!("    {:<50} {}", status, files.join(", "));
        }
        return false;
    }

    let Some((status, files)) = seen.iter().next() else {
        return true;
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
    let mut ok = true;
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
fn unfinished_milestones(root: &Path) -> Vec<String> {
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
        // In progress, specifically --- not merely unfinished. A status line naming every
        // milestone nobody has started yet is noise, and noise is what gets skipped when
        // the line does need changing. What must be named is what is in flight.
        if !trimmed.to_lowercase().contains("in progress") {
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

/// Every source file a document names by path must exist.
///
/// # Why this is not covered by the link check
///
/// The link check follows markdown links. This catches a path written in prose or in
/// backticks --- which is how documentation usually names a file, and which nothing verified.
///
/// It was added after `GUIDE.md` was found to say *"Every example here is executed by a test.
/// `crates/sankhya-server/tests/guide.rs` runs the SQL on this page and checks the answers,
/// so an example that stops working breaks the build rather than misleading a reader"* --- of
/// a file that did not exist. The promise was not merely stale: it asserted a verification
/// that was never happening, which is worse than saying nothing, because a reader who
/// believes it stops checking the examples themselves.
fn check_named_sources(root: &Path, docs: &[PathBuf]) -> bool {
    let mut ok = true;
    let mut checked = 0usize;
    for doc in docs {
        let Ok(text) = std::fs::read_to_string(doc) else {
            continue;
        };
        let rel = doc.strip_prefix(root).unwrap_or(doc).display().to_string();
        for named in named_source_paths(&text) {
            checked += 1;
            if !root.join(&named).exists() {
                eprintln!("  MISSING SOURCE  {rel}: names `{named}`, which does not exist");
                ok = false;
            }
        }
    }
    if ok {
        println!("   {checked} source path(s) named in prose all exist");
    }
    ok
}

/// Paths under `crates/` ending in `.rs` that a document mentions.
///
/// Deliberately narrow: a broad pattern over prose produces false positives, and a lint that
/// fires on ordinary writing gets switched off.
fn named_source_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || c == '`' || c == '(' || c == ')') {
        let token = token.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | '*' | '"'));
        if token.starts_with("crates/") && token.ends_with(".rs") && !token.contains("..") {
            out.push(token.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}


#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{check_named_sources, named_source_paths, unfinished_milestones};


    use std::path::Path;

    /// The hash is the generation; everything either side of it is the identity.
    #[test]
        /// A file whose name it cannot parse is a file it has no business deleting.
    #[test]
        /// The newest generations survive and the superseded ones go.
    ///
    /// Written because the sweep deletes files, and the only thing worse than a build tree
    /// that grows without bound is a cleanup that removes the build you are standing on.
    #[test]
        /// A dry run reports exactly what a real run would remove, and removes none of it.
    #[test]
        /// Set a file's modification time, so generation order is stated rather than raced for.
        /// A dev-dependency is not a way for a server to reach a SQL surface.
    ///
    /// This is the whole point of the check. `sankhya-cube-sql` was reachable from the
    /// server's *tests* long before it was reachable from the server, and that is precisely
    /// the state where a capability exists, is tested, and cannot be called.
    #[test]
        /// Reachability follows the graph, not just the first hop.
    #[test]
        /// A surface no server reaches fails the check.
    ///
    /// Against a synthetic tree, because the real one passes --- and a check that has only
    /// ever been run against a passing tree is a check nobody has seen work.
    #[test]
        /// The real repository serves every SQL surface it builds.
    ///
    /// Run against the actual tree rather than a fixture, because the fixture is what would
    /// have passed on every one of the four days this was wrong.
    #[test]
        /// The extractor finds paths written in prose and in backticks.
    ///
    /// Tested because the check that uses it had none, and a check nobody tests is a check
    /// that can be quietly disabled by a one-character edit --- which is exactly what a
    /// mutation of it demonstrated.
    #[test]
    fn a_source_path_is_found_however_it_is_written() {
        let text = "See `crates/sankhya-cube/src/cells.rs` and                     crates/sankhya-publish/src/publish.rs, plus (crates/x/tests/y.rs).";
        let found = named_source_paths(text);
        assert!(found.contains(&"crates/sankhya-cube/src/cells.rs".to_string()), "{found:?}");
        assert!(found.contains(&"crates/sankhya-publish/src/publish.rs".to_string()), "{found:?}");
        assert!(found.contains(&"crates/x/tests/y.rs".to_string()), "{found:?}");
    }

    #[test]
    fn ordinary_prose_is_not_mistaken_for_a_path() {
        // A lint that fires on ordinary writing gets switched off, which is worse than a
        // narrower one that is always obeyed.
        let text = "The crates are described below. See rust files and .rs extensions.";
        assert!(named_source_paths(text).is_empty(), "{:?}", named_source_paths(text));
    }

    #[test]
    fn a_path_is_reported_once_however_often_it_appears() {
        let text = "`crates/a/src/b.rs` and again crates/a/src/b.rs";
        assert_eq!(named_source_paths(text).len(), 1);
    }

    /// Every named path in this repository's own documentation exists.
    ///
    /// The check running against the real tree, so the test fails for the same reason the
    /// build does rather than for a reason invented here.
    #[test]
    fn the_documentation_names_only_files_that_exist() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace root is the xtask crate's parent")
            .to_path_buf();
        let mut docs = Vec::new();
        super::collect_markdown(&root, &mut docs);
        assert!(!docs.is_empty(), "no documents were found to check");
        assert!(
            super::check_named_sources(&root, &docs),
            "documentation names a source file that does not exist"
        );
    }

    /// A document naming a file that does not exist must **fail** the check.
    ///
    /// The positive test above --- "this repository's own docs are clean" --- passes just as
    /// happily when the check never reports anything, which a mutation demonstrated. A check
    /// is only tested by a case it has to reject.
    #[test]
    fn a_document_naming_a_missing_file_is_rejected() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let doc = dir.path().join("rotten.md");
        std::fs::write(&doc, "See `crates/nothing/src/absent.rs` for details.")
            .expect("writing the document");
        assert!(
            !check_named_sources(dir.path(), &[doc]),
            "a document naming a file that does not exist was accepted"
        );
    }

    #[test]
    fn a_document_naming_a_file_that_exists_is_accepted() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::create_dir_all(dir.path().join("crates/real/src")).expect("creating");
        std::fs::write(dir.path().join("crates/real/src/there.rs"), "// present")
            .expect("writing the source");
        let doc = dir.path().join("fine.md");
        std::fs::write(&doc, "See `crates/real/src/there.rs`.").expect("writing the document");
        assert!(check_named_sources(dir.path(), &[doc]));
    }

    #[test]
    /// A document naming a check that does not exist must be rejected.
    #[test]
    fn an_invariant_naming_a_check_that_does_not_run_is_rejected() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::create_dir_all(dir.path().join("docs")).expect("creating docs");
        // Every real check, plus one that does not exist.
        let mut text = String::from("| a rule | a reason | `check-imaginary` |\n");
        for check in super::KNOWN_CHECKS {
            text.push_str(&format!("| r | w | `{check}` |\n"));
        }
        std::fs::write(dir.path().join("docs/INVARIANTS.md"), text).expect("writing");
        assert!(
            !super::check_invariants(dir.path()),
            "a document naming a check that does not run was accepted"
        );
    }

    /// A check that runs and is documented nowhere must be rejected.
    ///
    /// The direction that found six of them on the day it was written.
    #[test]
    fn a_check_nobody_documented_is_rejected() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::create_dir_all(dir.path().join("docs")).expect("creating docs");
        std::fs::write(
            dir.path().join("docs/INVARIANTS.md"),
            "| a rule | a reason | `check-layers` |\n",
        )
        .expect("writing");
        assert!(!super::check_invariants(dir.path()));
    }

    #[test]
    fn the_real_invariants_document_names_every_check_and_no_others() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace root")
            .to_path_buf();
        assert!(super::check_invariants(&root));
    }

    fn milestones_in_progress_are_read_from_the_status_table() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace root")
            .to_path_buf();
        let found = unfinished_milestones(&root);
        // Read from STATUS.md rather than declared here, so this asserts the mechanism and
        // not a copy of the answer.
        assert!(!found.is_empty(), "no milestone is in progress, which cannot be right");
        assert!(found.iter().all(|m| m.starts_with('M')), "{found:?}");
    }
}

/// Who may write to a warehouse.
///
/// # The invariant
///
/// **There is one official writer to the warehouse, and it is `sankhya-publish`.** Every
/// other crate that writes data files or commits table-log actions is a second writer, and a
/// second writer is not a stylistic complaint --- it is a path that does not get the
/// guarantees the first one enforces.
///
/// That is not hypothetical here. `sankhya-publish` declares `partitionColumns` and lays
/// files out under `sank_data_date=…/`, as `FR-STORE-20` requires. The soak had its own
/// writer, so its warehouses were flat and non-conforming, and --- worse --- a soak that
/// bypasses the write path cannot find a defect in it. The publish path declared a partition
/// column it never wrote for months while a ten-gigabyte soak reported `PASS` beside it.
///
/// # Why an allowlist rather than a ban
///
/// One caller is genuinely not a second writer: `sankhya-maintenance` rewrites files that are
/// already published, which is a different operation from admitting new data. It is named
/// here with that reason rather than exempted silently.
///
/// `sankhya-ingest` was here, as a violation rather than an exemption: the CDC arrival path
/// wrote its own files and committed its own log, which is why its tables had no partition
/// columns and violated `FR-STORE-20`. It now publishes through `Publication`, so the entry
/// is gone --- and the check reported it as stale before anybody remembered to remove it,
/// which is the property that keeps a list like this honest.
///
/// The rest are violations that exist today. Listing them makes them visible and makes the
/// list shrink; the check's value is that **nothing new can be added without appearing
/// here**, which is the property a rule kept in somebody's head does not have.
const MAY_WRITE: &[(&str, &str)] = &[
    (
        "sankhya-publish",
        "the one official writer: it is what FR-STORE-20's partitioning and the date axis \
         are implemented in",
    ),
    (
        "sankhya-maintenance",
        "rewrites already-published files rather than admitting new data — compaction and \
         retention, not ingestion. It must preserve the layout publish established",
    ),
];

/// Calls that write to a warehouse.
/// Tests that write to a warehouse directly, and are waiting to be routed through the
/// product's own entry points.
///
/// # Why this list exists rather than a looser rule
///
/// `check-writers` used to skip every file under `tests/`, on the reasoning that tests drive
/// the writers rather than being writers. That stopped being true. The soak grouped files by
/// partition, merged them, committed the removals by hand --- and never retired the inputs,
/// because sequencing maintenance correctly is the product's job and the soak had quietly
/// taken it on. A run targeting ten gigabytes consumed sixty and died with a full disk.
///
/// The rule is now enforced for tests too. These files predate it. The list may **shrink and
/// never grow**: a file that stops writing is removed from it, and a file that starts writing
/// fails the check. That converts a backlog into something that gets paid down instead of
/// something that gets rediscovered.
const SECOND_WRITER_BACKLOG: &[&str] = &[
    // Empty, and it stays empty. Every entry was converted: fixtures now publish through
    // `Publication`, maintenance simulations call `Maintainer::tick`, and the states the
    // product refuses to write come from `sankhya_table_delta::malformed`.
];

const WRITES: &[&str] = &["write_parquet(", "compact_files(", "compact_files_sorted("];

/// Calls that commit to a table log.
const COMMITS: &[&str] = &["commit(&", "delta_commit(", "commit(root", "commit(table_root"];

fn check_writers(root: &Path) -> bool {
    println!("== check-writers ==");
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);

    let mut ok = true;
    let mut writers: BTreeMap<String, usize> = BTreeMap::new();
    // Which backlog entries still write. One that no longer does must leave the list, or the
    // ratchet only ever holds and never tightens.
    let mut backlog_seen: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        let rel = file.strip_prefix(root).unwrap_or(file).display().to_string();
        // The storage crates *are* the implementation being called, so they are not callers
        // of it --- and their own tests must call them, or the implementation is untested.
        if rel.contains("sankhya-table/") || rel.contains("sankhya-table-delta/") {
            continue;
        }
        // A test inside the crate that owns writing is testing it. A test anywhere else that
        // writes is *performing server work*, and that exemption used to be blanket.
        //
        // It is how the soak came to group files by partition, merge them, and commit the
        // removals by hand --- then forget to retire the inputs, and fill a disk. The rule
        // said only two crates may write to a warehouse; the check simply was not looking at
        // tests, so a test became the third writer and nothing said so.
        if rel.contains("/tests/")
            && MAY_WRITE.iter().any(|(name, _)| rel.contains(&format!("crates/{name}/")))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let Some(crate_name) = rel
            .strip_prefix("crates/")
            .and_then(|rest| rest.split('/').next())
        else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or(line);
            // For a test, the rule is about the *log*, not about bytes on disk.
            //
            // `write_parquet` writes a file. A file with no action referring to it is
            // invisible to every reader --- it is an orphan, and retention sweeps it. What
            // makes a second writer dangerous is mutating table state, and table state is
            // the log. So a test that writes a parquet and never commits is not a second
            // writer, and a test that commits is one however it produced the bytes.
            let calls: &[&str] = if rel.contains("/tests/") {
                COMMITS
            } else {
                &[]
            };
            let flagged = if rel.contains("/tests/") {
                calls.iter().any(|call| code.contains(call))
            } else {
                WRITES.iter().chain(COMMITS).any(|call| code.contains(call))
            };
            if flagged {
                *writers.entry(crate_name.to_string()).or_default() += 1;
                if SECOND_WRITER_BACKLOG.contains(&rel.as_str()) {
                    backlog_seen.insert(rel.clone());
                    continue;
                }
                if !MAY_WRITE.iter().any(|(name, _)| *name == crate_name) {
                    eprintln!(
                        "  SECOND WRITER  {rel}:{}: `{}` writes to a warehouse, and only \
                         sankhya-publish may. Route it through `Publication`, or add it to \
                         MAY_WRITE with a reason somebody can evaluate",
                        number + 1,
                        crate_name
                    );
                    ok = false;
                }
            }
        }
    }

    for (name, reason) in MAY_WRITE {
        if !writers.contains_key(*name) {
            eprintln!(
                "  STALE ENTRY    `{name}` is allowed to write and no longer does — delete \
                 the entry. An allowlist that only grows stops being read"
            );
            ok = false;
        }
        assert!(reason.len() > 40, "an allowlist entry needs a usable reason");
    }

    // The ratchet. A file that has been cleaned up must leave the list on the same commit,
    // or the backlog stops describing the work left and starts hiding it.
    for stale in SECOND_WRITER_BACKLOG {
        if !backlog_seen.contains(*stale) {
            eprintln!(
                "  CLEANED UP     {stale} no longer writes to a warehouse. Remove it from \
                 SECOND_WRITER_BACKLOG: a backlog that outlives the work it describes is a \
                 list nobody believes"
            );
            ok = false;
        }
    }

    if ok {
        let named: Vec<String> = writers
            .iter()
            .map(|(name, count)| format!("{name} ({count})"))
            .collect();
        println!(
            "   {} test file(s) still write directly, and the list may only shrink",
            SECOND_WRITER_BACKLOG.len()
        );
        println!("   only declared writers touch a warehouse: {}", named.join(", "));
    }
    ok
}

/// Every check `docs/INVARIANTS.md` names must exist.
///
/// # Why a document about enforcement needs enforcing
///
/// `INVARIANTS.md` lists the rules this system holds and, for each, where it is enforced.
/// That third column is the whole value of the document: a rule with a check behind it is a
/// guarantee, and a rule without one is a hope. If the column can name a check that has been
/// renamed or deleted, the document quietly turns every hope into an apparent guarantee ---
/// which is the precise failure the document was written about.
///
/// So the names are extracted and looked up. A rule honestly marked *nothing yet* is left
/// alone; it is already saying it is not enforced.
fn check_invariants(root: &Path) -> bool {
    println!("== check-invariants ==");
    let path = root.join("docs/INVARIANTS.md");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("  MISSING        docs/INVARIANTS.md does not exist");
        return false;
    };

    let mut named: BTreeSet<String> = BTreeSet::new();
    for token in text.split(|c: char| !(c.is_alphanumeric() || c == '-')) {
        if token.starts_with("check-") && token.len() > 6 {
            named.insert(token.to_string());
        }
    }

    let mut ok = true;
    for check in &named {
        if !KNOWN_CHECKS.contains(&check.as_str()) {
            eprintln!(
                "  UNKNOWN CHECK  docs/INVARIANTS.md names `{check}`, which xtask does not \
                 run. A document that can name a check nobody runs turns every rule in it \
                 into an apparent guarantee"
            );
            ok = false;
        }
    }

    // The reverse: a check that enforces something nobody wrote down.
    for check in KNOWN_CHECKS {
        if !named.contains(*check) {
            eprintln!(
                "  UNDOCUMENTED   `{check}` runs on every build and docs/INVARIANTS.md does \
                 not say what it protects. A rule nobody can find is a rule nobody keeps"
            );
            ok = false;
        }
    }

    if ok {
        println!("   {} named check(s), all of which run", named.len());
    }
    ok
}

/// Every check this tool runs.
///
/// Listed once, so the documentation check and the dispatch cannot disagree about what
/// exists.
const KNOWN_CHECKS: &[&str] = &[
    "check-invariants",
    "check-writers",
    "check-layers",
    "check-loc",
    "check-vocabulary",
    "check-dupes",
    "check-docs",
    "check-features",
    "check-lints",
    "check-mutations",
    "check-catalogues",
    "check-logging",
    "check-package",
    "check-doc-numbers",
    "check-surfaces",
    "check-atomic-writes",
    "check-build-tree",
    "check-tests",
];

/// Run the test suite.
///
/// # Why this was not here, and why that was the problem
///
/// `check-all` ran thirteen static checks --- layers, lints, documentation, mutations --- and
/// **not the tests**. `cargo test --workspace` appeared in this file exactly once, in a doc
/// comment describing what somebody else should run.
///
/// The consequence is the failure mode this project keeps finding in other people's work and
/// had in its own: a green report that was true about what it checked and silent about what
/// it did not. A maintenance test failed for some time while every commit said "all checks
/// passed", because the count of tests was obtained by *counting test functions* rather than
/// by running them --- a number that is equally correct whether they pass or not.
///
/// It is slow, and that is why it was left out. Slow is not a reason for a check to be
/// absent; it is a reason for it to be last.
fn check_tests(root: &Path) -> bool {
    println!("== check-tests ==");
    let started = std::time::Instant::now();
    let output = std::process::Command::new(env!("CARGO"))
        .arg("test")
        .arg("--workspace")
        .arg("--quiet")
        .current_dir(root)
        .output();

    let Ok(output) = output else {
        eprintln!("  COULD NOT RUN  cargo test could not be started");
        return false;
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    if !output.status.success() {
        // The failing lines, not the whole run. A wall of output is a wall nobody reads.
        for line in text.lines().filter(|line| {
            line.contains("panicked at")
                || line.starts_with("test result: FAILED")
                || line.starts_with("error")
        }) {
            eprintln!("  {line}");
        }
        eprintln!("  FAILED         the test suite does not pass");
        return false;
    }

    let passed: u64 = text
        .lines()
        .filter_map(|line| line.strip_prefix("test result: ok. "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter_map(|count| count.parse::<u64>().ok())
        .sum();
    println!(
        "   {passed} test(s) passed in {:.0}s",
        started.elapsed().as_secs_f64()
    );
    true
}
