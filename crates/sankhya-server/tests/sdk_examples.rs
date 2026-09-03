//! Every shipped SDK example, run against a real server.
//!
//! # Why this is a test and not a README
//!
//! An example that does not run is documentation that lies, and it lies most convincingly
//! right after the code it describes has changed --- because nothing about a stale example
//! looks stale. The only way an example stays true is if something runs it.
//!
//! Writing these found four defects that unit tests, mutation tests and a green gate had all
//! missed, every one of them on the *front door*: a cube could not name a table in a schema at
//! all, the history column that says what is keeping a version alive named neither the
//! snapshot nor the clone, and the binding's cube navigation put the dimension where the
//! measure goes. Each was reachable only by somebody trying to use the product.
//!
//! # What this asserts, and what it deliberately does not
//!
//! That each example **exits zero** and writes nothing to stderr. Not what it prints: an
//! example is prose, and a test that pinned its output would be a second copy of the prose
//! that has to be edited whenever the first is. The failure this closes is *"it does not
//! run"*, which is the one that happens.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{start, write_warehouse};

/// The repository root, from this crate's manifest.
fn repository() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the workspace root is two levels up from this crate")
        .to_path_buf()
}

#[test]
fn every_python_example_runs_against_a_real_server() {
    let root = repository();
    let examples = root.join("sdk").join("python").join("examples");

    let mut scripts: Vec<std::path::PathBuf> = std::fs::read_dir(&examples)
        .expect("the examples directory is there")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "py")
                // `_common.py` is imported, not run. Named with a leading underscore for
                // exactly that reason, so this rule is the file's own convention rather
                // than a list here that somebody has to remember to update.
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with('_'))
        })
        .collect();
    scripts.sort();

    assert!(
        scripts.len() >= 8,
        "the SDK ships fewer examples than it did, which is a deletion rather than a pass: {scripts:?}"
    );

    // Python is what these are written in. Absent, this cannot report a pass --- an
    // environment that cannot run the examples has not checked them, and saying otherwise is
    // the failure this whole file exists to prevent.
    let interpreter = "python3";
    assert!(
        std::process::Command::new(interpreter)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success()),
        "`{interpreter}` is not available, so the shipped examples cannot be checked"
    );

    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let mut broken: Vec<String> = Vec::new();
    for script in &scripts {
        let outcome = std::process::Command::new(interpreter)
            .arg(script)
            .current_dir(&examples)
            .env("SANKHYA_HOST", "127.0.0.1")
            .env("SANKHYA_PORT", server.port.to_string())
            .env("SANKHYA_USER", "quickstart")
            .output()
            .expect("the interpreter runs");

        let name = script
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?")
            .to_owned();
        let complained = String::from_utf8_lossy(&outcome.stderr).trim().to_owned();
        if !outcome.status.success() {
            broken.push(format!(
                "{name} exited {:?}\n{}\n{complained}",
                outcome.status.code(),
                String::from_utf8_lossy(&outcome.stdout)
            ));
        } else if String::from_utf8_lossy(&outcome.stdout).trim().is_empty() {
            // An example that exits zero and prints nothing has demonstrated nothing. Every
            // one of these guards its fixture and returns early when it is missing, which is
            // right --- and which is also how an example stops running without anybody
            // noticing that it stopped.
            broken.push(format!("{name} exited zero and printed nothing, so it showed nothing"));
        } else if !complained.is_empty() {
            // A traceback the script caught and printed still means the example is teaching
            // somebody the wrong thing, so a clean exit with noise on stderr is a failure.
            broken.push(format!("{name} exited zero and complained:\n{complained}"));
        }
    }

    assert!(broken.is_empty(), "{}", broken.join("\n\n---\n\n"));
}

#[test]
fn the_bindings_own_unit_tests_pass() {
    // The one place this binding has logic: the two functions that turn a method call into a
    // statement. Everything else is a pass-through, and `ADR-0017` Decision 1 requires that it
    // stay so --- but a *statement builder* is not enforced anywhere downstream, and both of
    // this package's were wrong at once. A wrong statement is still a statement: it compiles,
    // it sends, and the refusal comes back looking like the user's mistake.
    //
    // Run from here rather than left to a Python test runner nobody invokes, because the rule
    // this repository keeps arriving at is that a check nothing runs is not a check.
    let root = repository();
    let outcome = std::process::Command::new("python3")
        .args(["-m", "unittest", "discover", "-s", "sdk/python/tests", "-v"])
        .current_dir(&root)
        .output()
        .expect("the interpreter runs");

    assert!(
        outcome.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );

    // `unittest` reports to stderr, and a run that discovered nothing also exits zero --- so
    // the count is checked rather than the status alone. A suite that silently found no tests
    // is the same false green as a test that cannot fail.
    let reported = String::from_utf8_lossy(&outcome.stderr);
    let ran: usize = reported
        .split("Ran ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    assert!(
        ran > 0,
        "the binding's unit tests discovered nothing, which also exits zero: {reported}"
    );
}
