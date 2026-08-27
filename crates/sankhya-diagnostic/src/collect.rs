//! Taking the observations, and turning them into a report.
//!
//! # A run does two things, in this order
//!
//! It **records** what it saw, and then it **reports** on everything recorded. The order
//! matters: a run that reports first and records afterwards throws away its own newest
//! sample, and the projection is then always one run behind the truth.
//!
//! # A check that could not run is not a check that found nothing
//!
//! A table whose log will not replay produces no finding, and so does a healthy one. If
//! those two land in the same empty list, the report says "all clear" about a table it never
//! managed to look at --- which is the exact failure an operator is running this to avoid.
//! So every failure becomes a [`Report::skipped`] entry naming the table and the reason.

use crate::check::{compaction_debt, Report};
use crate::history::{History, HistoryError, Measure};
use crate::projection::Observation;
use std::path::{Path, PathBuf};

/// The check name under which a table's live file count is recorded.
pub const COMPACTION_DEBT: &str = "compaction-debt";

/// A table to look at: what it is called, and where it lives.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TableUnderReview {
    /// The qualified name, as an operator would type it.
    pub name: String,
    /// The directory holding its `_delta_log`.
    pub root: PathBuf,
}

impl TableUnderReview {
    /// Name a table.
    pub fn new(name: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            root: root.into(),
        }
    }
}

/// Observe every table, record what was seen, and report on it.
///
/// `now` is supplied rather than read from a clock, so a run can be replayed exactly.
///
/// The history is written to `data_dir` as each table is observed rather than in one batch
/// at the end, so a run interrupted half way still leaves the samples it managed to take.
/// The alternative loses a whole run's observations to a failure in the last table.
///
/// # Errors
///
/// Never. A failure to observe one table is recorded in the report and the rest are still
/// observed --- a diagnostic that stops at the first problem is one that reports the first
/// problem and hides the others, and the others are frequently the interesting ones.
pub fn run(
    data_dir: &Path,
    tables: &[TableUnderReview],
    now: i64,
) -> Report {
    let mut report = Report::new();
    let mut history = match History::read(data_dir) {
        Ok(history) => history,
        Err(error) => {
            // Not fatal. Values can still be reported; only the dates are lost, and saying
            // so is more useful than refusing to run.
            report.skipped("diagnostic-history", error.to_string());
            History::new()
        }
    };

    if history.damaged_lines() > 0 {
        report.skipped(
            "diagnostic-history",
            format!(
                "{} line(s) of history could not be read and were skipped; projections are \
                 drawn from fewer samples than the file appears to hold",
                history.damaged_lines()
            ),
        );
    }

    for table in tables {
        let measure = Measure::new(COMPACTION_DEBT, format!("table {}", table.name));
        match observe_live_files(&table.root) {
            Ok(files) => {
                #[allow(clippy::cast_precision_loss)]
                let observation = Observation::new(now, files as f64);
                if let Err(error) = history.append(data_dir, measure.clone(), observation) {
                    // Recorded once, not once per table: the reason is the same for all of
                    // them and repeating it buries the findings.
                    if report
                        .could_not_run()
                        .iter()
                        .all(|(check, _)| *check != "diagnostic-history")
                    {
                        report.skipped("diagnostic-history", error.to_string());
                    }
                    // The observation is still usable *this* run even if it did not reach
                    // the file, so it goes into the in-memory history regardless.
                    history.record(measure.clone(), observation);
                }
                match compaction_debt(&table.name, &history.trend(&measure), now) {
                    Some(finding) => report.found(finding),
                    None => report.clean(COMPACTION_DEBT),
                }
            }
            Err(why) => report.skipped(COMPACTION_DEBT, format!("table {}: {why}", table.name)),
        }
    }

    if history.should_compact() {
        if let Err(error) = history.compact(data_dir) {
            report.skipped("diagnostic-history-compaction", error.to_string());
        }
    }

    report
}

/// How many files a table currently consists of.
///
/// The file *count*, not the byte total, because a scan pays per file: opening it, reading
/// its footer, and deciding whether to prune it. A table of a thousand small files and one
/// of a thousand large files cost about the same to plan and the small one is the problem.
fn observe_live_files(table_root: &Path) -> Result<usize, String> {
    if !table_root.join("_delta_log").is_dir() {
        return Err("no _delta_log, so this is not a table".to_string());
    }
    sankhya_table_delta::live_files(table_root)
        .map(|live| live.files.len())
        .map_err(|error| error.to_string())
}

/// Record one observation of something the caller measured itself.
///
/// For measures this crate cannot take on its own --- free disk space needs a platform call
/// this workspace's `forbid(unsafe_code)` will not permit, so the caller that has one passes
/// the number in rather than this crate pretending it can find out.
///
/// # Errors
///
/// When the history cannot be written.
pub fn record(
    data_dir: &Path,
    history: &mut History,
    measure: Measure,
    observation: Observation,
) -> Result<(), HistoryError> {
    history.append(data_dir, measure, observation)
}
