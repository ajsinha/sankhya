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
use sankhya_diagnostic::{History, Measure, Observation};
use std::path::Path;

/// The check name under which free space is recorded.
const STORAGE_HEADROOM: &str = "storage-headroom";

/// Free bytes on the filesystem holding `path`, or `None` if it could not be established.
///
/// # Why `df` and not a syscall
///
/// `statvfs` is the direct answer and this workspace forbids `unsafe`, which rules it out;
/// `sankhya-alloc` and `sankhya-sandbox` are the two crates excused and neither excuse
/// covers this. A dependency for one reading is a dependency to keep pinned for ever ---
/// the same judgement `soak/sample.rs` made when it read `/proc/self/status` rather than
/// wrapping a crate around it. `df -P` is POSIX, its output columns are specified, and this
/// runs once an hour from cron rather than in any hot path.
///
/// # Why `Option` and never zero
///
/// Zero free bytes is a perfectly plausible reading and a catastrophic one. A failure that
/// returned it would page somebody about a disk that is fine; a failure that returned the
/// filesystem's size would hide one that is not. Neither is available to be confused with
/// "the measurement did not happen".
fn free_bytes(path: &Path) -> Option<f64> {
    let output = std::process::Command::new("df")
        .arg("-P")
        .arg("-k")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Line 0 is the header. `-P` guarantees one line per filesystem after it, never wrapped,
    // which is the whole reason for the flag: without it a long device name wraps and the
    // columns move.
    let line = text.lines().nth(1)?;
    // Columns: filesystem, 1024-blocks, used, available, capacity, mounted-on.
    let blocks: f64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(blocks * 1024.0)
}

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

    // Free space, recorded before anything is reported, so this run's own sample is in the
    // history the projection is drawn from.
    //
    // `OPS-26`. `check::storage_headroom` was written, tested, exported --- and called by
    // nothing, so `doctor` could never warn about a filling disk. The hook for it was here
    // all along: `collect::record` exists precisely because free space needs a reading this
    // crate cannot take, and the caller that can was supposed to pass it in.
    let mut history = match History::read(data_dir) {
        Ok(history) => history,
        Err(_) => History::new(),
    };
    let headroom = Measure::new(STORAGE_HEADROOM, "the data directory");
    match free_bytes(data_dir) {
        Some(free) => {
            let observation = Observation::new(now, free);
            if sankhya_diagnostic::collect::record(
                data_dir,
                &mut history,
                headroom.clone(),
                observation,
            )
            .is_err()
            {
                // Usable this run even if it did not reach the file. The projection needs
                // more than one sample, but the *value* is reportable now.
                history.record(headroom.clone(), observation);
            }
        }
        None => {
            // Said, not assumed. A disk check that silently does not run is the failure this
            // whole crate is arranged against, and it is why there is a third exit status.
            eprintln!(
                "  WARNING: free space on {} could not be measured, so nothing will warn \
                 about it filling",
                data_dir.display()
            );
        }
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

    // Whether the disk is filling, from the sample taken above and every one before it.
    if free_bytes(data_dir).is_none() {
        report.skipped(
            STORAGE_HEADROOM,
            format!("free space on {} could not be measured", data_dir.display()),
        );
    } else if let Some(finding) =
        sankhya_diagnostic::check::storage_headroom(&history.trend(&headroom), now)
    {
        report.found(finding);
    } else {
        report.clean(STORAGE_HEADROOM);
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
