//! `sankhya-publish` — publish a table, or check one.
//!
//! A command-line front end to the library, so a publisher that is not written in Rust can
//! still take the supported path. Air-gapped by construction: one static binary, no runtime
//! download, no network.

// A command-line tool that cannot print to its own console is not much of a tool.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use sankhya_publish::repair::{apply, plan};
use sankhya_publish::verify::verify;

fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let command = arguments.first().map(String::as_str);

    match command {
        Some("verify") => match arguments.get(1) {
            Some(path) => run_verify(std::path::Path::new(path)),
            None => usage(),
        },
        Some("repair") => match arguments.get(1) {
            Some(path) => run_repair(
                std::path::Path::new(path),
                // Dry run unless asked. A repair tool that acts because it was invoked is a
                // liability: the commonest way to run one is by accident, on the wrong
                // directory, at three in the morning.
                arguments.iter().any(|a| a == "--apply"),
            ),
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

/// Show what could be repaired, and optionally do it.
fn run_repair(root: &std::path::Path, apply_it: bool) -> std::process::ExitCode {
    let plan = plan(root);
    println!("{}", plan.summary());

    for action in &plan.actions {
        let sankhya_publish::repair::Repair::RecomputeStatistics { file } = action;
        println!("  would recompute statistics for {file} by reading it");
    }
    for refusal in &plan.refused {
        eprintln!("  NEEDS A PERSON: {}", refusal.why);
        eprintln!("                  {}", refusal.decision);
    }

    if plan.is_empty() {
        return std::process::ExitCode::SUCCESS;
    }

    if !apply_it {
        println!("\nnothing was changed. Re-run with --apply to carry this out.");
        // Distinct from both clean and failed: there is work to do and none was done.
        return std::process::ExitCode::from(1);
    }

    match apply(&plan) {
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(2)
        }
        Ok(outcome) => {
            println!("{}", outcome.summary());
            for (file, reason) in &outcome.failed {
                eprintln!("  could not repair {file}: {reason}");
            }
            if outcome.is_clean_now() {
                std::process::ExitCode::SUCCESS
            } else {
                // Repaired what it could, and something remains. Not a failure of the
                // repair, and not success either.
                std::process::ExitCode::from(1)
            }
        }
    }
}

fn usage() -> std::process::ExitCode {
    eprintln!(
        "usage: sankhya-publish verify <table-directory>\n\
                sankhya-publish repair <table-directory> [--apply]\n\
         \n\
         Checks a table's log against the invariants the publishing library maintains, and\n\
         reports what is wrong rather than only whether. Reads only the log; no data file is\n\
         opened.\n\
         \n\
         verify reports what is wrong rather than only whether, and does not open a data\n\
         file. Exit status: 0 clean, 1 findings that make queries slow, 2 findings that\n\
         make them wrong.\n\
         \n\
         repair fixes only what can be DERIVED from evidence that already exists, and\n\
         refuses anything needing a guess, saying what a person has to decide. It never\n\
         deletes and never rewrites a committed version: a repair is appended as a new\n\
         version, so the broken state stays readable and the repair is revertible.\n\
         Without --apply it changes nothing. Exit status: 0 clean, 1 work remains, 2\n\
         failed."
    );
    std::process::ExitCode::from(64)
}
