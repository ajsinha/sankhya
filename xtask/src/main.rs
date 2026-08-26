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
    "arrow", "arrow-array", "arrow-schema", "arrow-buffer", "arrow-ipc",
    "parquet", "datafusion", "object_store", "delta_kernel",
];

/// Duplicates that are permitted because they never cross a SANKHYA API boundary.
/// Every entry is a deliberate decision, not an accumulation.
const DUP_ALLOWLIST: &[&str] = &[
    "base64", "foldhash", "getrandom", "hashbrown", "itertools",
    "rand", "rand_core", "syn", "windows-sys", "r-efi", "wasi",
    "windows-targets", "windows_x86_64_gnu", "windows-link", "generic-array",
    "bitflags", "heck", "regex-automata", "regex-syntax", "socket2", "winnow",
];

/// Domain nouns that must not appear in core crates. The general-purpose claim is
/// that the core knows about tenants, tables, columns, edges and versions — and
/// nothing about any industry.
///
/// This lint catches *leakage*. It cannot catch *shape*: a core can be immaculately
/// neutral in its naming and still be bent toward one domain. Only the reference
/// packs catch that. Both mechanisms are needed.
const DOMAIN_WORDS: &[&str] = &[
    "trade", "counterparty", "notional", "portfolio", "basel", "isin", "cusip",
    "ledger", "aml", "kyc", "ubo", "laundering", "desk", "book_id",
    "shipment", "consignment", "patient", "claim", "diagnosis", "icd10",
    "sensor_reading", "invoice", "sku",
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
    if !run_all
        && !matches!(
            task.as_str(),
            "check-layers" | "check-loc" | "check-vocabulary" | "check-dupes"
        )
    {
        eprintln!("usage: cargo xtask [check-all|check-layers|check-loc|check-vocabulary|check-dupes]");
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
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let manifest = e.path().join("Cargo.toml");
            if !manifest.exists() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
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
            out.push(Crate { name, layer: layer as u32, deps });
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
            let Some(dep) = by_name.get(d.as_str()) else { continue }; // external crate
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
                eprintln!("  CORE->PACK   {} -> {} : no core crate may depend on a pack", c.name, d);
                ok = false;
                continue;
            }
            if dep.layer == LAYER_TOOLING {
                eprintln!("  ->TOOLING    {} -> {} : tooling is not a dependency", c.name, d);
                ok = false;
                continue;
            }
            // Extension API sits at 1.5: usable from L2 upward, and depends only on L0/L1.
            if dep.layer >= c.layer && c.layer != LAYER_TOOLING {
                eprintln!(
                    "  UPWARD DEP   {} (L{}) -> {} (L{}) : dependencies point downward only",
                    c.name, c.layer, d, dep.layer
                );
                ok = false;
            }
        }
    }
    println!("   {} crates, dependency direction {}", crates.len(), if ok { "OK" } else { "VIOLATED" });
    ok
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
                if in_block { break; }
                continue;
            }
            if in_block { break; }
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
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if p.is_dir() {
            if matches!(name, "target" | ".git" | "generated" | "snapshots" | "corpus") {
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
        let Ok(src) = std::fs::read_to_string(f) else { continue };
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
        files.len(), largest.1, largest.0, warned
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
        let Ok(src) = std::fs::read_to_string(f) else { continue };
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
    let Ok(text) = std::fs::read_to_string(&lock) else { return true };
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("name = \"") {
            name = v.trim_end_matches('"').to_string();
        } else if let Some(v) = line.strip_prefix("version = \"") {
            if !name.is_empty() {
                seen.entry(name.clone()).or_default().push(v.trim_end_matches('"').to_string());
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
            eprintln!("  NEW DUP      {n}: {versions:?} — review, then allowlist deliberately or remove");
            ok = false;
        }
    }
    println!(
        "   {} packages, {benign} allowlisted duplicates, critical family single-versioned: {}",
        seen.len(),
        !seen.iter().any(|(n, v)| CRITICAL_FAMILY.contains(&n.as_str()) && v.len() > 1)
    );
    ok
}
