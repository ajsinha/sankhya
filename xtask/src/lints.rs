//! The lint gate: the denied set across every target, and silence from what ships.

use std::path::Path;
use std::process::Command;
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
pub fn check_lints(root: &Path) -> bool {
    println!("== check-lints");
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        // **A target directory of its own**, and it is not tidiness.
        //
        // `cargo clippy` substitutes its own driver for `rustc` and writes different
        // fingerprints for the same crate. Sharing one directory with `cargo test` therefore
        // means each invalidates everything the other built --- so a `check-all` compiled the
        // whole workspace once for the tests and again for the lints, and whichever ran last
        // left the tree poisoned for the next thing anybody ran. Three full builds where one
        // would do, on a workspace whose `deps` directory is ninety-six gigabytes.
        //
        // Measured on 2026-09-05: this is the single largest cost in the gate, and it is
        // entirely an artefact of the two tools sharing a directory.
        .env("CARGO_TARGET_DIR", root.join("target").join("lints"))
        .args(["clippy", "--workspace", "--all-targets", "--keep-going"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            // Success is not silence. The workspace denies its clippy lints, so a clippy
            // finding is an `error` and fails below --- and a **rustc** warning is not: an
            // unused import, a dead function, an `unreachable_pub` all compile clean and
            // exit zero. `cargo build --workspace` reached **77** of them against a
            // repository whose documentation emphasises lint cleanliness, and the eighteen
            // that named something real were invisible under fifty-nine that named one
            // arrangement. That is the failure mode: warnings are read until there are too
            // many, and then they are never read again. `RUN-15`.
            //
            // Scoped to what ships. `--all-targets` adds tests and benchmarks, where the
            // pedantic lints are warnings rather than denials by deliberate choice --- a
            // test indexes its own fixtures and says so --- and failing on those would be a
            // different decision from this one, taken by accident. The count is printed so
            // that it drifts visibly instead of silently.
            println!("   clean across every target");
            let noisy = String::from_utf8_lossy(&output.stderr)
                .lines()
                .filter(|line| line.starts_with("warning:") && !line.contains("generated"))
                .count();
            println!("   {noisy} warning(s) across tests and benchmarks, where the pedantic lints warn rather than deny");
            shipping_build_is_silent(root)
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

/// Whether the code that ships compiles with nothing to say.
///
/// `cargo build --workspace` rather than `--all-targets`: this is the binary and the
/// libraries, which is what a warning is about when it is about something. Zero is the only
/// passing number, because any other one is a threshold, and a threshold is what let seventy-
/// seven accumulate.
fn shipping_build_is_silent(root: &Path) -> bool {
    let Ok(output) = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["build", "--workspace"])
        .output()
    else {
        eprintln!("   FAILED: could not build the workspace to read its warnings");
        return false;
    };
    let text = String::from_utf8_lossy(&output.stderr);
    let warnings: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("warning:") && !line.contains("generated"))
        .collect();
    if warnings.is_empty() {
        println!("   and the shipping build says nothing at all");
        return true;
    }
    eprintln!("   FAILED: {} warning(s) from `cargo build --workspace`. A warning that is tolerated is a warning nobody reads, and the ones worth reading go under it:", warnings.len());
    for line in warnings.iter().take(10) {
        eprintln!("     {line}");
    }
    false
}

