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
    let reachable = every_crate_is_reachable_or_owned(root, &served);
    let named = every_kernel_has_a_name(root);
    ok && reachable && named
}

/// A computational kernel that no SQL name calls, with the reason it is allowed to exist.
///
/// # Why this list is short and hard to add to
///
/// Everything here is a kernel a **user cannot invoke**. That is the failure this whole module
/// exists for, applied one level down: `check` catches a crate that nothing reaches, and until
/// 2026-09-02 nothing caught a *function* that nothing reaches --- so thirteen tested kernels
/// sat in `sankhya-math` with no name on any surface, including linear regression and both
/// quantile kernels. Written, tested, mutation-tested, and unusable.
///
/// An entry here must say why the kernel is **not** something a user would call. "We will
/// expose it later" is not a reason; that is what the milestone in `IMPLEMENTATION_PLAN.md` is
/// for, and a kernel awaiting exposure should fail this check until it has a name.
const INTERNAL_KERNELS: &[(&str, &str)] = &[
    ("reduce::deterministic_sum", "the reduction every other kernel is built on. Not a function anybody calls on a column --- SQL's own `sum` is that --- and exposing it would offer two spellings of one operation"),
    ("reduce::exact_sum", "the fixed-point route inside `deterministic_sum`, and an implementation detail of it. A user choosing between them would be choosing an algorithm, not an answer: they return the same bits"),
    ("reduce::combine_partials", "combines partial sums from a parallel reduction. Reachable only from a planner that partitioned the work, which is not something a statement expresses"),
    ("vector::row_of", "reads one row out of a flat buffer. A layout helper, not an operation on data"),
    ("vector::matvec", "the kernel behind `mat_vec`, which is its name on the surface"),
    ("stats::mean", "the kernel behind `vec_mean`. `vector::mean` is the one the surface calls; this is the statistics module's own, and the two agree by construction"),
    ("quantile::quantile_of_sum", "quantiles of a *running total* rather than of values, used by the cube's consolidation. It answers a question a statement cannot currently pose, and inventing a spelling for it before anything asks would be guessing at the shape"),
];

/// Every public kernel is callable by name from SQL, or listed with the reason it is not.
///
/// # What this catches that nothing else did
///
/// A crate can be reachable, its functions covered by unit tests, its behaviour pinned by
/// mutation entries, its documentation accurate --- and the function can still be impossible
/// to call. Every existing check passes in that state, because every existing check looks at
/// the code rather than at the surface.
///
/// The reachability test is textual: a kernel is reached when some crate outside
/// `sankhya-math` names it as `module::function`. Textual rather than semantic because the
/// registration is textual --- a name string paired with a closure --- and a check that
/// resolved types would be a compiler for the sake of a list.
fn every_kernel_has_a_name(root: &Path) -> bool {
    println!("== check-kernels ==");
    let math = root.join("crates").join("sankhya-math").join("src");
    let Ok(modules) = std::fs::read_dir(&math) else {
        eprintln!("  UNREADABLE   {} could not be listed", math.display());
        return false;
    };

    // Everything any crate but `sankhya-math` says, in one string. A kernel is reached when
    // its qualified name appears in it.
    let mut callers = String::new();
    for entry in walk(&root.join("crates")) {
        if entry.starts_with(&math) {
            continue;
        }
        if entry.extension().is_some_and(|e| e == "rs") {
            if let Ok(text) = std::fs::read_to_string(&entry) {
                callers.push_str(&text);
            }
        }
    }

    let mut stranded: Vec<String> = Vec::new();
    let mut named = 0usize;
    let mut excused = 0usize;

    for module in modules.flatten() {
        let path = module.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if name == "lib" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("pub fn ") else {
                continue;
            };
            let kernel: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if kernel.is_empty() {
                continue;
            }
            let qualified = format!("{name}::{kernel}");
            // A **call**, however the module was spelled at the call site. Requiring the
            // module's own name reported thirty reachable kernels as stranded the first time
            // a caller wrote `use sankhya_math::distribution as d`, and a check that cries
            // wolf is one somebody silences.
            //
            // Two modules exporting one name share a match, which is why `stats::mean` carries
            // an entry below rather than relying on this.
            if INTERNAL_KERNELS.iter().any(|(listed, _)| *listed == qualified) {
                excused += 1;
            } else if is_named(&callers, &kernel) {
                named += 1;
            } else {
                stranded.push(qualified);
            }
        }
    }

    for kernel in &stranded {
        eprintln!(
            "  NO SQL NAME  `sankhya_math::{kernel}` is written and tested and nothing calls
             {:16}it. Give it a name on a surface, or list it in `INTERNAL_KERNELS` with the
             {:16}reason a user would never call it. A kernel nobody can invoke is work that
             {:16}was half done and looks finished.",
            "", "", ""
        );
    }

    if stranded.is_empty() {
        println!("   {named} kernel(s) reachable by name, {excused} listed as internal");
    }
    stranded.is_empty()
}

/// Whether any caller mentions `::<kernel>` as a path.
///
/// # Why this is not a substring search for the call
///
/// A kernel reaches the surface two ways, and both had to be learned the hard way. It can be
/// **called** --- `stats::variance(a, p)` --- or **passed** as a function reference, which is
/// how half of them are registered: `VectorFunction::unary("vec_median", stats::median)`. A
/// search for `::median(` finds the first and misses the second, and reported eleven reachable
/// kernels as stranded.
///
/// The boundary check is what keeps `mean` from matching `meanwhile`. Without it a kernel
/// could be reported as reachable because an unrelated identifier happened to start with its
/// name, which is the one failure mode worse than a false alarm here: a stranded kernel that
/// the check says is fine.
fn is_named(callers: &str, kernel: &str) -> bool {
    let needle = format!("::{kernel}");
    let mut from = 0usize;
    while let Some(at) = callers[from..].find(&needle) {
        let start = from + at;
        let after = start + needle.len();
        let boundary = callers[after..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric() && next != '_');
        if boundary {
            return true;
        }
        from = after;
    }
    false
}

/// Every `.rs` file under a directory.
fn walk(at: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![at.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// A crate that nothing reaches, with the reason it is allowed to exist anyway.
///
/// Every entry needs a **milestone**, not just a sentence. "We will get to it" is how ten
/// crates came to hold one line of source each while being named nowhere in the plan.
const UNREACHED: &[(&str, &str)] = &[
    ("sankhya-datagen", "generates the synthetic data the soak and the OLAP benchmarks load. Reached only from dev-dependencies, which this traversal deliberately ignores --- a *surface* reachable only from a test is the defect; a generator of test data is not one"),
    ("sankhya-objectstore", "M8 §12.1 --- where ADR-0013's version claim lands on an object store, as a conditional put"),
    ("sankhya-oltp-pg", "the PostgreSQL supervisor is built and tested against the vendored 17.11, and nothing wires it into the server yet: `Settings` has no OLTP configuration. Wiring it is M8 §12.2, beside leader election, which is what will need a running store"),
    ("sankhya-testkit", "the concurrency harness. Reached only from dev-dependencies, which this traversal deliberately ignores --- and rightly, since a testkit that a *product* crate depended on would be shipping test scaffolding to customers"),
    ("sankhya-tiering", "M9, and explicitly gated on the drills in IMPLEMENTATION_PLAN.md §13"),
    ("sankhya-mv", "undecided by ADR-0014, and listed rather than deleted because the design question is open"),
    ("sankhya-api-rest", "M8 §12.2, with soak criterion 7. The route table and the size decision are built and tested; serving them needs an HTTP listener, HTTP authentication and a *pre-materialisation* row estimate to decide inline-versus-ticket --- `deliver` refuses to be given a count taken after the rows exist, which is the whole point of it. That is a feature, not hygiene, and it is sized where the rest of criterion 7 lives"),
    ("sankhya-cdc-pg", "M2's carried remainder, not an M8 decision. The slot lifecycle, the lag thresholds and the source-safety ladder are built and tested; what is missing is the *driver* that runs them on a timer, which is exactly what STATUS records as outstanding for M2 --- `the slot lifecycle driver and the snapshot reader`"),
    ("sankhya-ports", "decided in M8 §12.1f: **delete**. Nothing implements a single trait in it, and its own header claims `Clock` and `IdGen` are injected everywhere and enforced by lint, neither of which is true --- a crate whose documentation asserts a property the workspace does not have is worse than an empty one. Listed rather than gone only because the deletion needs an owner's hand on it"),
    ("sankhya-pack", "M4 §8.6's carried remainder, not an M8 decision. The declarative tier is a *planned* tier --- ARCHITECTURE names it and expects it to express the substantial majority of a real pack --- so deleting it would discard a milestone's work, and the loader that reads a bundle directory into a running server is the piece that was never built"),
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
    /// A dev-dependency is not a way for a server to reach a SQL surface.
    ///
    /// This is the whole point of the check. `sankhya-cube-sql` was reachable from the
    /// server's *tests* long before it was reachable from the server, and that is precisely
    /// the state where a capability exists, is tested, and cannot be called.
    #[test]
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


    /// Reachability follows the graph, not just the first hop.
    #[test]
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


    /// A surface no server reaches fails the check.
    ///
    /// Against a synthetic tree, because the real one passes --- and a check that has only
    /// ever been run against a passing tree is a check nobody has seen work.
    #[test]
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


    /// A kernel with no name on any surface fails the build.
    ///
    /// The check that would have caught twelve of them, tested on a fixture where the answer
    /// is known --- because a check nobody has watched fail is a check nobody knows works,
    /// which is the same defect it exists to catch, one level up.
    #[test]
    fn a_kernel_nothing_can_call_fails_the_check() {
        let root = tempfile::tempdir().expect("a directory");
        let math = root.path().join("crates").join("sankhya-math").join("src");
        std::fs::create_dir_all(&math).expect("creating the math crate");
        std::fs::write(
            math.join("vector.rs"),
            "pub fn reachable(a: f64) -> f64 { a }\npub fn stranded(a: f64) -> f64 { a }\n",
        )
        .expect("writing kernels");

        // A crate that names one of them and not the other.
        let caller = root.path().join("crates").join("sankhya-olap").join("src");
        std::fs::create_dir_all(&caller).expect("creating the caller");
        std::fs::write(caller.join("lib.rs"), "fn x() { vector::reachable(1.0); }\n")
            .expect("writing the caller");

        assert!(
            !super::every_kernel_has_a_name(root.path()),
            "a kernel no surface names must fail the check"
        );

        // And naming it is what fixes it --- not deleting the check.
        std::fs::write(
            caller.join("lib.rs"),
            "fn x() { vector::reachable(1.0); vector::stranded(2.0); }\n",
        )
        .expect("writing the caller");
        assert!(super::every_kernel_has_a_name(root.path()));
    }

    /// The real repository can call every kernel it has written.
    ///
    /// Run against the actual tree, because a fixture is what passed on every one of the days
    /// twelve tested kernels had no name.
    #[test]
    fn every_real_kernel_has_a_name() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .expect("the workspace root");
        assert!(super::every_kernel_has_a_name(&root));
    }

    /// The real repository serves every SQL surface it builds.
    ///
    /// Run against the actual tree rather than a fixture, because the fixture is what would
    /// have passed on every one of the four days this was wrong.
    #[test]
    fn every_real_sql_surface_is_reachable() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .expect("the workspace root");
        assert!(super::check(&root));
    }

}
