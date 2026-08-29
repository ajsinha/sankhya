//! Whether a SQL surface this workspace builds can actually be called.
//!
//! Four times in one day a crate exposing SQL functions turned out to be unreachable from the
//! thing that serves SQL: `sankhya-maintenance`, which nothing in production called;
//! `sankhya-cube-sql`, which no crate depended on; `sankhya-olap`, so every vector and matrix
//! function the guide documents answered `Invalid function`; and `sankhya-graph-sql`, the same
//! for five graph functions.
//!
//! Every one was found by accident --- a soak dying, a Cargo.toml read for another reason, a
//! guide example failing. A capability nothing reaches is indistinguishable, from outside,
//! from one that was never built, and nothing was looking.

use crate::rust_files;
use std::collections::BTreeSet;
use std::path::Path;

/// A SQL surface the server cannot reach, with the reason it is allowed to be.
///
/// Empty, and it should stay that way. An entry here says "this crate registers SQL functions
/// that no server serves", which is a decision somebody has to defend rather than a state a
/// dependency graph can drift into.
const UNSERVED_SURFACES: &[(&str, &str)] = &[];


/// Every crate registering SQL functions must be reachable from the server.
///
/// # Why this exists
///
/// Four times in one day a crate exposing a whole SQL surface turned out to be unreachable
/// from the thing that serves SQL:
///
/// - `sankhya-maintenance` --- a working, tested library that nothing in production called,
///   so a running server compacted nothing and retired nothing.
/// - `sankhya-cube-sql` --- depended on by no crate at all, so the cube surface existed and
///   could not be reached.
/// - `sankhya-olap` --- not a server dependency, so every vector, matrix and statistics
///   function the guide documents answered `Invalid function`.
/// - `sankhya-graph-sql` --- the same, for five graph functions the guide names.
///
/// Every one was found by accident: by a soak dying, by reading a Cargo.toml for another
/// reason, by a guide example failing. A capability nothing reaches is indistinguishable,
/// from outside, from one that was never built --- and nothing was looking.
///
/// The guide test catches this only for functions the guide happens to document. This catches
/// it for all of them.
pub fn check(root: &Path) -> bool {
    println!("== check-surfaces ==");

    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);

    // Crates that register SQL functions, found from the calls that do it.
    let mut registering: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        let rel = file.strip_prefix(root).unwrap_or(file).display().to_string();
        if rel.contains("/tests/") {
            continue;
        }
        let Some(crate_name) = rel
            .strip_prefix("crates/")
            .and_then(|rest| rest.split('/').next())
        else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines() {
            let code = line.split("//").next().unwrap_or(line);
            if code.contains("register_udf(")
                || code.contains("register_udtf(")
                || code.contains("impl TableFunctionImpl")
            {
                registering.insert(crate_name.to_string());
                break;
            }
        }
    }

    let served = reachable_from_server(root);
    let mut ok = true;
    let mut checked = 0usize;
    for crate_name in &registering {
        if crate_name == "sankhya-server" {
            continue;
        }
        checked += 1;
        if served.contains(crate_name) {
            continue;
        }
        if let Some((_, why)) = UNSERVED_SURFACES.iter().find(|(name, _)| name == crate_name) {
            assert!(why.len() > 30, "an unserved surface needs a usable reason");
            continue;
        }
        eprintln!(
            "  UNREACHABLE    `{crate_name}` registers SQL functions and no server depends \
             on it, so nothing it exposes can be called. A capability nothing reaches is \
             indistinguishable from one that was never built"
        );
        ok = false;
    }
    if ok {
        println!("   {checked} SQL surface(s) registered, and every one is reachable");
    }
    ok && every_crate_is_reachable_or_owned(root, &served)
}

/// A crate that nothing reaches, with the reason it is allowed to exist anyway.
///
/// Every entry needs a **milestone**, not just a sentence. "We will get to it" is how ten
/// crates came to hold one line of source each while being named nowhere in the plan.
const UNREACHED: &[(&str, &str)] = &[
    ("sankhya-datagen", "generates the synthetic data the soak and the OLAP benchmarks load. Reached only from dev-dependencies, which this traversal deliberately ignores --- a *surface* reachable only from a test is the defect; a generator of test data is not one"),
    ("sankhya-api-flight", "Arrow Flight SQL: reached only from `sankhya-api-rest`, which is itself unreached, so the bulk plane `GUIDE.md` §7a documents cannot be used. Found by this check on 2026-08-29 and the guide now says so. Wiring it is M8 §12.2, beside the gRPC transport it shares a transport story with"),
    ("sankhya-api-grpc", "M6's carried exit criterion 7, scheduled for M8 §12.2"),
    ("sankhya-objectstore", "M8 §12.1 --- where ADR-0013's version claim lands on an object store, as a conditional put"),
    ("sankhya-oltp-pg", "the PostgreSQL supervisor is built and tested against the vendored 17.11, and nothing wires it into the server yet: `Settings` has no OLTP configuration. Wiring it is M8 §12.2, beside leader election, which is what will need a running store"),
    ("sankhya-testkit", "M8 §12.1e --- deterministic fault injection, which is why the concurrency defects went unseen"),
    ("sankhya-tiering", "M9, and explicitly gated on the drills in IMPLEMENTATION_PLAN.md §13"),
    ("sankhya-mv", "undecided by ADR-0014, and listed rather than deleted because the design question is open"),
    ("sankhya-alloc", "a counting global allocator that nothing installs. Wiring it is M8 §12.1f and returns allocation figures the soak currently cannot see"),
    ("sankhya-api-rest", "a REST surface no server depends on. Wire or delete is an M8 §12.1f decision"),
    ("sankhya-cdc-pg", "a capture source no server depends on. Wire or delete is an M8 §12.1f decision"),
    ("sankhya-ports", "trait definitions nothing implements, including a `Clock` the crate claims is injected everywhere and enforced by lint --- neither is true. M8 §12.1f"),
    ("sankhya-pack", "the declarative pack tier: a parser, bundles, validation and hot reload that no server loads, so a bundle cannot be used. M8 §12.1f"),
];

/// Every crate is reachable from a binary, or listed with a reason and a milestone.
///
/// # Why the SQL check above was not enough
///
/// It looks for crates that register SQL functions, which is narrower than its purpose. A REST
/// surface, a capture source, a global allocator, a set of port traits and an entire declarative
/// pack tier --- about 2,600 lines --- all fell straight through it and were found by reading
/// manifests by hand.
///
/// A crate is a claim the repository makes about itself. This is what makes the claim checkable.
fn every_crate_is_reachable_or_owned(root: &Path, served: &BTreeSet<String>) -> bool {
    // Reachability from **every** root that ships, not only from the server. A pack is a
    // deliverable and `sankhya-ext` is the API it is written against; counting only the server
    // would report the published extension API as dead code.
    let mut reached = served.clone();
    let mut roots = vec!["sankhya-cli".to_string()];
    if let Ok(packs) = std::fs::read_dir(root.join("packs")) {
        for pack in packs.flatten() {
            if let Some(name) = pack.file_name().to_str() {
                roots.push(name.to_string());
            }
        }
    }
    for start in roots {
        reached.extend(reachable_from(root, &start));
    }
    let served = &reached;
    let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
        return true;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().join("Cargo.toml").is_file())
        .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
        .collect();
    names.sort();

    let mut ok = true;
    let mut listed = BTreeSet::new();
    for name in &names {
        if served.contains(name) {
            continue;
        }
        match UNREACHED.iter().find(|(crate_name, _)| crate_name == name) {
            Some((_, why)) => {
                assert!(why.len() > 30, "`{name}` is excused without a usable reason");
                listed.insert(name.clone());
            }
            None => {
                eprintln!(
                    "  UNREACHED      `{name}` is in the workspace and no binary depends on                      it. Wire it, delete it, or list it in `UNREACHED` with the milestone that                      will. A crate nothing reaches is a claim the repository does not keep"
                );
                ok = false;
            }
        }
    }
    // An excuse for a crate that is now reachable, or gone, must go too --- or the list only
    // grows and stops describing anything.
    for (name, _) in UNREACHED {
        if !listed.contains(*name) {
            eprintln!(
                "  STALE EXCUSE   `{name}` is listed as unreached and is either reachable now                  or no longer exists"
            );
            ok = false;
        }
    }
    if ok {
        println!("   {} crate(s) reachable, {} listed with a milestone", served.len(), listed.len());
    }
    ok
}


/// Every crate the server depends on, transitively, within this workspace.
pub(crate) fn reachable_from_server(root: &Path) -> BTreeSet<String> {
    reachable_from(root, "sankhya-server")
}

/// Every crate `start` depends on, transitively, within this workspace.
pub(crate) fn reachable_from(root: &Path, start: &str) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    let mut queue = vec![start.to_string()];
    while let Some(name) = queue.pop() {
        if !reached.insert(name.clone()) {
            continue;
        }
        let in_crates = root.join("crates").join(&name).join("Cargo.toml");
        let manifest_path = if in_crates.is_file() {
            in_crates
        } else {
            root.join("packs").join(&name).join("Cargo.toml")
        };
        let Ok(manifest) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        // Only the real dependencies. A dev-dependency is what a test reaches for, and a
        // surface reachable only from a test is exactly the state this check exists to find.
        for line in manifest.lines() {
            if line.trim_start().starts_with("[dev-dependencies]") {
                break;
            }
            if let Some(dependency) = line.split('=').next().map(str::trim) {
                if dependency.starts_with("sankhya-") {
                    queue.push(dependency.to_string());
                }
            }
        }
    }
    reached
}


#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
fn reaching_a_crate_only_from_tests_is_not_reaching_it() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let crates = root.path().join("crates");
        for (name, manifest) in [
            (
                "sankhya-server",
                "[dependencies]\nsankhya-real = { path = \"../sankhya-real\" }\n\n                 [dev-dependencies]\nsankhya-only-in-tests = { path = \"../x\" }\n",
            ),
            ("sankhya-real", "[dependencies]\n"),
            ("sankhya-only-in-tests", "[dependencies]\n"),
        ] {
            let at = crates.join(name);
            std::fs::create_dir_all(&at).expect("creating");
            std::fs::write(at.join("Cargo.toml"), manifest).expect("writing");
        }

        let reached = super::reachable_from_server(root.path());
        assert!(reached.contains("sankhya-real"), "a real dependency is reached");
        assert!(
            !reached.contains("sankhya-only-in-tests"),
            "a dev-dependency is what a test reaches for, and a surface reachable only from \
             a test is exactly the state this check exists to find"
        );
    }


fn reaching_is_transitive() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let crates = root.path().join("crates");
        for (name, manifest) in [
            ("sankhya-server", "[dependencies]\nsankhya-middle = { path = \"../m\" }\n"),
            ("sankhya-middle", "[dependencies]\nsankhya-deep = { path = \"../d\" }\n"),
            ("sankhya-deep", "[dependencies]\n"),
        ] {
            let at = crates.join(name);
            std::fs::create_dir_all(&at).expect("creating");
            std::fs::write(at.join("Cargo.toml"), manifest).expect("writing");
        }

        let reached = super::reachable_from_server(root.path());
        assert!(reached.contains("sankhya-deep"), "two hops away is still reachable");
    }


fn a_surface_no_server_reaches_is_refused() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let crates = root.path().join("crates");
        for (name, manifest) in [
            ("sankhya-server", "[dependencies]\n"),
            ("sankhya-stranded-sql", "[dependencies]\n"),
        ] {
            let at = crates.join(name).join("src");
            std::fs::create_dir_all(&at).expect("creating");
            std::fs::write(
                crates.join(name).join("Cargo.toml"),
                manifest,
            )
            .expect("writing the manifest");
            std::fs::write(at.join("lib.rs"), "// nothing\n").expect("writing a source file");
        }
        // A crate that registers a table function and that no server depends on.
        std::fs::write(
            crates.join("sankhya-stranded-sql").join("src").join("lib.rs"),
            "pub fn wire(c: &X) { c.register_udtf(\"stranded\", y); }\n",
        )
        .expect("writing");

        assert!(
            !super::check(root.path()),
            "a crate registering SQL functions that no server depends on must fail the check"
        );
    }


fn every_real_sql_surface_is_reachable() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .expect("the workspace root");
        assert!(super::check(&root));
    }

}
