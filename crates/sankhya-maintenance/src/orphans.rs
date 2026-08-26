//! Reclaiming files nothing refers to.
//!
//! # Where orphans come from
//!
//! A compaction writes its output and then commits it. A process killed between the two
//! leaves a Parquet file that is complete, correct, and referenced by nothing. That is
//! the *designed* failure mode — the log lags the filesystem so that a crash leaves an
//! invisible file rather than a broken table — and the price of it is that something has
//! to sweep up.
//!
//! # The rule that makes sweeping safe
//!
//! **Age is the only defence against deleting a file that is about to be committed.**
//!
//! An uncommitted file and an orphaned one are indistinguishable: both are on disk and
//! in no log. The only thing separating them is time — one was written moments ago by a
//! process still running, the other hours ago by a process that died. So the threshold
//! must exceed the longest a write-then-commit can possibly take, including a stalled
//! one, and erring high costs disk while erring low costs data that was never recorded
//! as lost.
//!
//! # Why time travel constrains this too
//!
//! A file absent from the *current* live set may still be reachable from a retained
//! snapshot. Compaction's inputs are exactly that: superseded, absent from the live set,
//! and still the answer for a query pinned before the merge. Sweeping them on age alone
//! would break time travel silently, and only for queries that reach back far enough.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A file observed on disk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileOnDisk {
    pub name: String,
    pub bytes: u64,
    /// How long since it was written, in whatever tick the caller counts.
    pub age_ticks: u64,
}

/// When an unreferenced file may be removed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OrphanPolicy {
    /// How old an unreferenced file must be.
    ///
    /// Must exceed the longest possible interval between writing a file and committing
    /// it, because within that interval an uncommitted file and an orphan look identical.
    /// Erring high costs disk; erring low costs data.
    pub min_age_ticks: u64,
}

impl Default for OrphanPolicy {
    fn default() -> Self {
        Self {
            // Deliberately far beyond any plausible commit. The cost of this being too
            // large is storage; the cost of it being too small is a file deleted from
            // under the process that just wrote it.
            min_age_ticks: 7 * 24 * 60 * 60,
        }
    }
}

/// What a sweep decided.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct OrphanPlan {
    pub remove: Vec<String>,
    pub bytes_reclaimable: u64,
    /// Unreferenced files deliberately left, with the reason.
    ///
    /// Keeping a file is never an error. It costs storage; removing one too early costs
    /// data, or a query that fails on a file that is not there.
    pub retained: Vec<(String, String)>,
}

/// Decide which files may be reclaimed.
///
/// `live` is every path the table currently refers to; `reachable` is every path any
/// *retained* snapshot refers to, which for a table with time travel includes files the
/// live set no longer names.
///
/// Pure: it reads a listing and returns names, touching nothing.
#[must_use]
pub fn plan_orphan_cleanup(
    on_disk: &[FileOnDisk],
    live: &BTreeSet<String>,
    reachable: &BTreeSet<String>,
    policy: &OrphanPolicy,
) -> OrphanPlan {
    let mut plan = OrphanPlan::default();

    for file in on_disk {
        // The log is not data. Sweeping it would delete the table.
        if file.name.starts_with('_') || file.name.contains("_delta_log") {
            continue;
        }
        if live.contains(&file.name) {
            continue;
        }
        if reachable.contains(&file.name) {
            plan.retained.push((
                file.name.clone(),
                "a retained snapshot still reaches it; removing it would break time \
                 travel silently, and only for queries that reach back far enough"
                    .to_string(),
            ));
            continue;
        }
        if file.age_ticks < policy.min_age_ticks {
            plan.retained.push((
                file.name.clone(),
                format!(
                    "written {} ticks ago and the threshold is {}; a file this recent may \
                     be mid-commit rather than orphaned, and the two are indistinguishable",
                    file.age_ticks, policy.min_age_ticks
                ),
            ));
            continue;
        }

        plan.bytes_reclaimable = plan.bytes_reclaimable.saturating_add(file.bytes);
        plan.remove.push(file.name.clone());
    }

    plan
}

/// What a sweep actually did.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct OrphanReport {
    pub removed: Vec<PathBuf>,
    pub bytes_reclaimed: u64,
    /// Files the plan named that could not be removed, with the reason.
    ///
    /// A failure here is not a failure of the sweep. The file stays, and the next sweep
    /// tries again.
    pub failed: Vec<(String, String)>,
}

/// Remove what a plan named.
///
/// # Errors
///
/// Never fails as a whole. A file that cannot be removed is reported and the sweep
/// continues, because one undeletable file must not stop the rest from being reclaimed.
#[must_use]
pub fn sweep(plan: &OrphanPlan, directory: &Path) -> OrphanReport {
    let mut report = OrphanReport::default();

    for name in &plan.remove {
        let path = directory.join(name);
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(bytes);
                report.removed.push(path);
            }
            // Already gone is the desired state.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.removed.push(path),
            Err(e) => report.failed.push((name.clone(), e.to_string())),
        }
    }

    report
}
