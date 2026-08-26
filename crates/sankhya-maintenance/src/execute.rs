//! Running a compaction plan, and — separately — retiring what it replaced.
//!
//! These are two operations on purpose. See the module documentation for why the
//! separation is the thing that makes frequent compaction safe.

use sankhya_error::{Error, Result};
use sankhya_table::{compact_files_sorted, read_parquet_stats, CompactionOutcome, WriterConfig};
use sankhya_types::Lsn;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::compaction::CompactionPlan;

/// Execute a plan.
///
/// The output is written into `output_dir` alongside the inputs, which are left in
/// place. Nothing is removed here — see [`retire_inputs`].
///
/// `clustering` names the columns a settled partition is written in order of. Sorting is
/// what turns row-group bounds into a usable index: measured on TPC-H Q6, which selects
/// one year in seven, ordering the file by ship date took the query from 229 ms to
/// 55 ms. It is applied only when the plan says the partition has settled, because
/// sorting one that is still receiving writes means sorting it again tomorrow.
///
/// # Errors
///
/// Returns an error if any input is unreadable, if the merge fails, or if the output
/// does not hold exactly the rows the plan said its inputs held. The last case leaves
/// the output on disk for inspection and removes nothing.
pub fn run_compaction(
    plan: &CompactionPlan,
    directory: &Path,
    output_name: &str,
    config: WriterConfig,
    clustering: &[String],
) -> Result<CompactionOutcome> {
    let inputs: Vec<PathBuf> = plan
        .inputs
        .iter()
        .map(|f| directory.join(&f.name))
        .collect();

    // Sorted only where the partition has settled. Ordering one that is still receiving
    // writes produces a layout that was correct until the next append, for the cost of a
    // full sort on every pass.
    let clustering: &[String] = if plan.settled { clustering } else { &[] };

    let outcome = compact_files_sorted(
        &inputs,
        directory,
        output_name,
        plan.covers_through,
        config,
        clustering,
    )?;

    // The plan was computed from a listing that may be stale by the time it runs. If
    // the files on disk no longer hold what the plan believed, the discrepancy is
    // reported rather than absorbed: the merge itself is fine, but a scheduler acting
    // on stale statistics is a bug worth surfacing.
    if outcome.rows != plan.rows() {
        return Err(Error::InvariantViolated(format!(
            "the plan for {}/{} expected {} rows but its inputs now hold {}; the merged \
             output is on disk and no input was removed",
            plan.table,
            plan.partition,
            plan.rows(),
            outcome.rows
        )));
    }

    Ok(outcome)
}

/// When an input may be deleted.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RetentionPolicy {
    /// How long an input must survive past its replacement.
    ///
    /// A reader that listed files just before the merge is still entitled to open them.
    /// This must exceed the longest query the deployment permits, or a long-running
    /// scan can have a file vanish mid-read.
    pub grace_ticks: u64,
    /// Whether to require that the replacement re-verifies before anything is removed.
    ///
    /// Defaults on. Turning it off saves one footer read per retirement and is only
    /// sensible where the storage layer already guarantees durability of the write.
    pub verify_replacement: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            grace_ticks: 24,
            verify_replacement: true,
        }
    }
}

/// What a retirement did, or declined to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RetirementOutcome {
    pub removed: Vec<PathBuf>,
    /// Inputs deliberately kept, each with the reason.
    ///
    /// Keeping a file is never an error. It costs storage; removing one too early costs
    /// a query.
    pub retained: Vec<(PathBuf, String)>,
    pub bytes_reclaimed: u64,
}

/// Remove inputs a compaction replaced, if every precondition holds.
///
/// Preconditions, all of which must hold for a given input:
///
/// 1. The replacement exists and — unless the policy waives it — still reports the
///    expected row count. This is re-checked rather than trusted: the merge may have
///    succeeded hours ago.
/// 2. The input is not referenced by any retained snapshot.
/// 3. Enough time has passed that no reader could still be holding a listing that
///    predates the merge.
///
/// An input failing any of these is retained with a reason. That is the correct
/// outcome, not a failure — retirement is an optimisation, and declining it costs only
/// disk.
///
/// # Errors
///
/// Returns an error only if the replacement itself is missing or wrong, which means the
/// compaction did not actually happen and no input should be removed at all.
pub fn retire_inputs(
    outcome: &CompactionOutcome,
    referenced: &BTreeSet<Lsn>,
    ticks_since_merge: u64,
    policy: &RetentionPolicy,
) -> Result<RetirementOutcome> {
    if policy.verify_replacement {
        let (rows, _) = read_parquet_stats(&outcome.output).map_err(|e| {
            Error::InvariantViolated(format!(
                "the replacement {} could not be verified ({e}); nothing was removed",
                outcome.output.display()
            ))
        })?;
        if rows != outcome.rows {
            return Err(Error::InvariantViolated(format!(
                "the replacement {} holds {rows} rows but the merge reported {}; \
                 nothing was removed",
                outcome.output.display(),
                outcome.rows
            )));
        }
    }

    let mut removed = Vec::new();
    let mut retained = Vec::new();
    let mut bytes_reclaimed = 0u64;

    for input in &outcome.inputs_retained {
        if ticks_since_merge < policy.grace_ticks {
            retained.push((
                input.clone(),
                format!(
                    "within the grace period ({ticks_since_merge} of {} ticks); a reader \
                     that listed before the merge may still open it",
                    policy.grace_ticks
                ),
            ));
            continue;
        }

        // A snapshot pinned at or before this file's coverage may still resolve to it.
        if referenced.iter().any(|lsn| *lsn <= outcome.covers_through) {
            retained.push((
                input.clone(),
                "a retained snapshot may still reference it".to_string(),
            ));
            continue;
        }

        let bytes = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(input) {
            Ok(()) => {
                bytes_reclaimed = bytes_reclaimed.saturating_add(bytes);
                removed.push(input.clone());
            }
            // Already gone is the desired state, not a failure.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => removed.push(input.clone()),
            Err(e) => retained.push((input.clone(), format!("removal failed: {e}"))),
        }
    }

    Ok(RetirementOutcome {
        removed,
        retained,
        bytes_reclaimed,
    })
}
