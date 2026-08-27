//! `sankhya-publish` — publish a table, or check one.
//!
//! A command-line front end to the library, so a publisher that is not written in Rust can
//! still take the supported path. Air-gapped by construction: one static binary, no runtime
//! download, no network.

// A command-line tool that cannot print to its own console is not much of a tool.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use sankhya_publish::verify::verify;

fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let command = arguments.first().map(String::as_str);

    match command {
        Some("verify") => match arguments.get(1) {
            Some(path) => run_verify(std::path::Path::new(path)),
            None => usage(),
        },
        _ => usage(),
    }
}

/// Check a table and report what is wrong with it.
fn run_verify(root: &std::path::Path) -> std::process::ExitCode {
    let report = verify(root);
    println!("{}", report.summary());

    for finding in &report.findings {
        // Correctness findings to stderr so a pipeline that only watches stderr still sees
        // them, and slowness findings to stdout so they do not look like failures.
        if finding.affects_correctness() {
            eprintln!("  {finding}");
        } else {
            println!("  {finding}");
        }
    }

    if report.has_correctness_findings() {
        // Distinct from the merely-slow case, so a build gate can fail on one and not the
        // other. A table that is slow can wait; one that returns wrong answers cannot.
        return std::process::ExitCode::from(2);
    }
    if report.is_clean() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(1)
    }
}

fn usage() -> std::process::ExitCode {
    eprintln!(
        "usage: sankhya-publish verify <table-directory>\n\
         \n\
         Checks a table's log against the invariants the publishing library maintains, and\n\
         reports what is wrong rather than only whether. Reads only the log; no data file is\n\
         opened.\n\
         \n\
         Exit status: 0 clean, 1 findings that make queries slow, 2 findings that make them\n\
         wrong."
    );
    std::process::ExitCode::from(64)
}
