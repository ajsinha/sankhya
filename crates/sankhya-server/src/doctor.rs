//! `sankhya doctor` --- the thing an operator runs when something feels wrong.
//!
//! # It runs without the server
//!
//! Deliberately a separate path from [`crate::wiring::start`], reading the warehouse
//! directly. The moment somebody reaches for a diagnostic is frequently the moment the
//! server will not start, and a diagnostic that needs a healthy server to tell you the
//! server is unhealthy is decoration.
//!
//! # It is meant to be run on a schedule
//!
//! Every projection comes from the difference between runs, so a single run on the day of an
//! incident can report values and no dates. Hourly from cron is what makes `FR-OPS-17`
//! actually true rather than technically implemented --- and the run itself says so when it
//! has too little history, rather than leaving the operator to wonder.
//!
//! # Exit status
//!
//! `0` clean, `1` findings, `2` the diagnostic could not do its job. The third is separate
//! because a monitoring system treating "I could not look" as "nothing found" is the failure
//! this whole crate is arranged against, and an exit status is where that gets decided.

use sankhya_diagnostic::collect::{run, TableUnderReview};
use std::path::Path;

/// Exit status for a clean run.
pub(crate) const CLEAN: i32 = 0;
/// Exit status when something was found.
pub(crate) const FINDINGS: i32 = 1;
/// Exit status when a check could not run.
pub(crate) const COULD_NOT_RUN: i32 = 2;

/// Look at a warehouse and print what is worth knowing.
///
/// `now` is passed in rather than read from a clock inside, so the whole run is replayable.
#[must_use]
pub(crate) fn doctor(warehouse: &Path, data_dir: &Path, now: i64) -> i32 {
    println!("SANKHYA doctor {}", env!("CARGO_PKG_VERSION"));
    println!("  warehouse {}", warehouse.display());

    if let Err(error) = std::fs::create_dir_all(data_dir) {
        // Without somewhere to record observations there will never be a second sample, so
        // this is worth saying loudly even though the run continues.
        eprintln!(
            "  WARNING: {} could not be created ({error}); this run cannot record what it \
             sees, so the next run will still have no rate to project from",
            data_dir.display()
        );
    }

    let (found, refused) = crate::warehouse::discover(warehouse);
    let tables: Vec<TableUnderReview> = found
        .iter()
        .map(|table| TableUnderReview::new(table.reference.to_string(), &table.root))
        .collect();
    println!("  {} table(s)", tables.len());

    let mut report = run(data_dir, &tables, now);

    // The backup's own health. Folded into the same report because an operator asking "is
    // this system all right" is asking one question, and a backup that has never been proven
    // is the most consequential answer in it.
    if let Some(finding) = sankhya_diagnostic::check::restore_drill(
        crate::backup::last_proven(data_dir),
        sankhya_diagnostic::check::DRILL_OBJECTIVE_MICROS,
        now,
    ) {
        report.found(finding);
    } else {
        report.clean("restore-drill");
    }

    // And whether the write-once controls anything archived depends on are still in force.
    // Silent when nothing is archived: a deployment with no archive has no such control to
    // lose, and a check that fired anyway would be Critical on every install from the day it
    // shipped, which is how a check stops being read.
    let archived = crate::backup::has_archive(&sankhya_backup::attest::Directory::at(data_dir));
    if let Some(finding) = sankhya_diagnostic::check::archive_attestation(
        sankhya_backup::attest::last_pass(data_dir),
        archived,
        sankhya_diagnostic::check::ATTESTATION_OBJECTIVE_MICROS,
        now,
    ) {
        report.found(finding);
    } else if archived {
        report.clean("archive-attestation");
    }
    for (path, why) in &refused {
        report.skipped("table-discovery", format!("{}: {why}", path.display()));
    }

    println!();
    if report.findings().is_empty() {
        println!("Nothing to report.");
    } else {
        // Most urgent first: already over the line, then soonest, then the undated. An
        // operator reading top-down is reading a schedule.
        for finding in report.findings() {
            println!("  {}", finding.describe());
        }
    }

    if !report.could_not_run().is_empty() {
        println!();
        println!("Could not run:");
        for (check, why) in report.could_not_run() {
            // Not folded in with the findings. "I did not look" and "I looked and it was
            // fine" produce the same silence, and only one of them is good news.
            println!("  [{check}] {why}");
        }
    }

    println!();
    println!("{}", report.summary());

    if !report.could_not_run().is_empty() {
        COULD_NOT_RUN
    } else if report.findings().is_empty() {
        CLEAN
    } else {
        FINDINGS
    }
}
