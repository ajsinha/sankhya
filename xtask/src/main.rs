//! Repository enforcement tooling.
//!
//! These checks are the mechanical half of the architecture: the layer DAG, the
//! file-length ceiling, the domain-vocabulary prohibition and the duplicate-version
//! gate. Each exists because the corresponding mistake is cheap to make, expensive
//! to unwind, and invisible in review.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
    if !run_all
        && !matches!(
            task.as_str(),
            "check-layers"
                | "check-loc"
                | "check-vocabulary"
                | "check-dupes"
                | "check-docs"
                | "check-features"
        )
    {
        eprintln!(
            "usage: cargo xtask \
             [check-all|check-layers|check-loc|check-vocabulary|check-dupes|check-docs\
             |check-features]"
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

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
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

/// The ceiling bounds cognitive load. It is not satisfied structurally: a split that
/// widens visibility or separates an invariant from its enforcement is a violation of
/// this rule, not compliance with it, and must be rejected in review.
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
