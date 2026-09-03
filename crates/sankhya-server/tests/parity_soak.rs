//! Every function, through every path, compared bit for bit.
//!
//! # The property, and why no unit test reaches it
//!
//! **A user does not care whether an answer came from OLTP, OLAP or the SDK, and the
//! experience must not depend on it.** A unit test asks whether *one* path is right, and three
//! paths can each be right about a different thing --- a float rendered to six places on one,
//! an array parsed with the wrong separator on another, a null that became a zero on the third.
//!
//! So this asks the only question that captures the requirement: **do the paths agree?** Of
//! every function in the server's own catalogue, with arguments generated from what the
//! catalogue says each one takes --- so a function added to the server is compared the next
//! time this runs, with no edit anywhere.
//!
//! # Why the driver is Python
//!
//! Because one of the paths is the Python binding, and a Rust harness cannot exercise it. The
//! test starts the shipping server and runs the soak against it, so what is compared is the
//! real binding against the real wire.
//!
//! # Running it longer
//!
//! ```text
//! SANKHYA_PARITY_MINUTES=30 \
//!   cargo test -p sankhya-server --test parity_soak -- --nocapture
//! ```

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    // `mod common` walks a wire buffer by index, and is compiled once per test file that
    // includes it --- so every one of them must allow this or the shared module fails under
    // whichever is strictest.
    clippy::indexing_slicing
)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod common;

use common::{start, write_warehouse};

#[test]
fn every_path_gives_the_same_answer() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the workspace root")
        .to_path_buf();

    // Python is one of the paths under test. Absent, this cannot report a pass: a run that
    // compared two paths and called it parity would be the claim this file exists to check.
    assert!(
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success()),
        "`python3` is not available, so the binding path cannot be compared"
    );

    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let minutes = std::env::var("SANKHYA_PARITY_MINUTES").unwrap_or_else(|_| "0.2".to_owned());
    let outcome = std::process::Command::new("python3")
        .arg(root.join("sdk").join("python").join("soak").join("parity.py"))
        .current_dir(&root)
        .env("SANKHYA_PORT", server.port.to_string())
        .env("SANKHYA_USER", "quickstart")
        .env("SANKHYA_PARITY_MINUTES", &minutes)
        .output()
        .expect("the soak runs");

    let said = String::from_utf8_lossy(&outcome.stdout);
    println!("{said}");
    if !outcome.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&outcome.stderr));
    }
    assert!(outcome.status.success(), "the paths disagreed");

    // A soak that ran nothing exits zero too, so what it *did* is checked rather than only
    // that it finished. This is the same false green a suite that discovered no tests gives.
    assert!(
        said.contains("comparison(s) between paths"),
        "the soak did not report what it compared"
    );
    let compared: usize = said
        .split("   ")
        .find_map(|line| {
            line.strip_suffix("comparison(s) between paths\n")
                .and_then(|count| count.trim().parse().ok())
        })
        .unwrap_or(0);
    assert!(
        compared > 100,
        "only {compared} comparison(s) were made, which is not a parity check"
    );

    // And the coverage is reported, because a soak that covers a hundred of a hundred and
    // twenty-eight and prints PASS has made a claim about twenty-eight it never ran.
    assert!(said.contains("callable by this generator"), "coverage was not reported");
}
