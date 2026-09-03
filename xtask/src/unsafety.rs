//! One crate may write `unsafe`. This says which, and fails if a second appears.
//!
//! # Why this is a check and not a convention
//!
//! The workspace sets `unsafe_code = "forbid"` for every crate that inherits its lints, and
//! that is the reason a warehouse handling other people's numbers has no memory-safety surface
//! to review. [ADR-0023](../../docs/adr/0023-the-sandbox-a-user-function-runs-in.md) Decision 8
//! makes exactly one exception, because every mechanism that isolates a user-supplied function
//! is a syscall made between `fork` and `exec`.
//!
//! An exception that is only written down becomes two exceptions and then a policy. The
//! difference between a rule and a habit is whether the build enforces it, so the permitted
//! crate is **named here**, and a manifest that stops inheriting the workspace lints without
//! being that crate fails the build.

use std::path::Path;

/// The crates permitted to opt out, and why each.
///
/// **Two, and the list is closed.** Adding a third is a decision that belongs in an ADR: each
/// entry is a place where a reader must check memory safety by reading rather than by trusting
/// the compiler, and the cost of that is paid per entry, forever.
const PERMITTED: &[(&str, &str)] = &[
    (
        "sankhya-alloc",
        "the counting allocator — a `GlobalAlloc` implementation cannot be written in safe \
         Rust, and the whole crate is short enough to read in a sitting",
    ),
    (
        "sankhya-sandbox",
        "ADR-0023 Decision 8 — the namespace, mount and rlimit syscalls that isolate a \
         user-supplied function are made between `fork` and `exec`",
    ),
];

/// Every crate inherits the workspace lints, except the one named above.
pub fn check(root: &Path) -> bool {
    println!("== check-unsafety ==");
    let mut ok = true;
    let mut opted_out = Vec::new();

    for group in ["crates", "packs"] {
        let Ok(entries) = std::fs::read_dir(root.join(group)) else {
            continue;
        };
        for entry in entries.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            // A crate that inherits is one whose `[lints]` section says so. Anything else ---
            // its own table, or no table at all --- is a crate the workspace's `forbid` does
            // not reach.
            let inherits = text.contains("[lints]") && text.contains("workspace = true");
            if !inherits {
                opted_out.push(name);
            }
        }
    }
    opted_out.sort();

    for name in &opted_out {
        match PERMITTED.iter().find(|(permitted, _)| permitted == name) {
            Some((_, why)) => println!("   {name} writes `unsafe` by exception: {why}"),
            None => {
                eprintln!(
                    "  UNSAFE       {name} does not inherit the workspace lints, so \
                     `forbid(unsafe_code)` does not reach it. One crate is permitted this and \
                     it is named in xtask/src/unsafety.rs — adding a second is a decision that \
                     belongs in an ADR, not in a manifest"
                );
                ok = false;
            }
        }
    }

    // And the exception must still be *used*. A permitted crate that no longer writes `unsafe`
    // should go back to inheriting, or the permission outlives the reason for it — which is how
    // an exception becomes a habit without anybody deciding anything.
    for (name, _) in PERMITTED {
        if !opted_out.iter().any(|found| found == name) {
            continue;
        }
        let sources = root.join("crates").join(name).join("src");
        let writes_unsafe = walk(&sources)
            .iter()
            .any(|path| {
                std::fs::read_to_string(path).is_ok_and(|text| text.contains("unsafe "))
            });
        if !writes_unsafe {
            eprintln!(
                "  UNUSED       {name} is permitted to write `unsafe` and does not. The \
                 exception should be withdrawn and the crate should inherit the workspace \
                 lints again"
            );
            ok = false;
        }
    }

    println!(
        "   {} crate(s) opt out of the workspace lints, {} permitted",
        opted_out.len(),
        PERMITTED.len()
    );
    ok
}

/// Every `.rs` file under a directory.
fn walk(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
    out
}
