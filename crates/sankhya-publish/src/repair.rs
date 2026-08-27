//! Repairing a table, only where the repair can be **derived** rather than guessed.
//!
//! # The rule this whole module is built around
//!
//! A repair tool that guesses is worse than no repair tool. The failure mode of this system
//! it most needs to avoid is an answer that is wrong and looks right, and a tool that
//! invents a plausible value writes exactly that into the table permanently --- with an
//! operator's confidence attached to it, because a tool said it was fixed.
//!
//! So every action here derives its value from evidence that already exists:
//!
//! - **Missing statistics** are recomputed by reading the file. The file *is* the truth;
//!   nothing is invented.
//! - **A missing row count** comes from the Parquet footer, which records it.
//! - **A missing `partitionValues`** on an unpartitioned table is the empty map, which is
//!   what the format requires and is not a choice.
//!
//! Everything else is **refused and explained**. A missing schema cannot be inferred from a
//! file: a table with a column added after those files were written would infer a schema
//! missing it, and an empty table has nothing to infer from at all. A key column nobody
//! declared cannot be recovered, because only a person knows what identified a row.
//!
//! # Three properties that make this safe to run
//!
//! **It never deletes.** A repair that removes data is not a repair. Nothing here removes a
//! file, an action, or a version.
//!
//! **It repairs by appending.** The log is append-only, so a repair writes a *new version*
//! superseding the broken one. The broken state stays readable for forensics, the repair is
//! itself revertible, and time travel to before the repair still works.
//!
//! **It plans before it acts.** [`plan`] reads and decides; [`apply`] writes. The plan is
//! printable, reviewable, and doing nothing with it is always an option.

use crate::verify::{verify, Finding, Report};
use sankhya_table_delta::{commit, live_files, Action, AddFile};
use std::fmt;
use std::path::{Path, PathBuf};

/// Something a repair would do, and what it derives it from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Repair {
    /// Recompute a file's column statistics by reading it.
    RecomputeStatistics {
        /// Which file.
        file: String,
    },
}

/// Something a repair cannot do, and what a person would have to decide.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NeedsAPerson {
    /// What is wrong.
    pub finding: Finding,
    /// Why it cannot be derived.
    pub why: String,
    /// What the person has to decide.
    pub decision: String,
}

/// What a repair would do, before it does anything.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    /// Where the table is.
    pub root: PathBuf,
    /// What can be derived and fixed.
    pub actions: Vec<Repair>,
    /// What cannot, and why.
    pub refused: Vec<NeedsAPerson>,
}

impl Plan {
    /// Whether there is anything to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.refused.is_empty()
    }

    /// Whether applying this plan would leave the table clean.
    ///
    /// False when something needs a person, so an operator knows before they start whether
    /// this is the whole fix or only part of it.
    #[must_use]
    pub fn would_fully_repair(&self) -> bool {
        self.refused.is_empty() && !self.actions.is_empty()
    }

    /// A summary line.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "nothing to repair".to_string();
        }
        format!(
            "{} action(s) can be derived, {} finding(s) need a person",
            self.actions.len(),
            self.refused.len()
        )
    }
}

/// Decide what could be repaired, without touching anything.
///
/// Reads the log and, for findings that need it, the data files. Writes nothing.
#[must_use]
pub fn plan(root: &Path) -> Plan {
    let report: Report = verify(root);
    let mut actions = Vec::new();
    let mut refused = Vec::new();

    for finding in &report.findings {
        match finding {
            // Derivable: the file holds the truth, and reading it invents nothing.
            Finding::NoStatistics { file } | Finding::NoRowCount { file } => {
                let action = Repair::RecomputeStatistics { file: file.clone() };
                if !actions.contains(&action) {
                    actions.push(action);
                }
            }

            Finding::NoMetadata => refused.push(NeedsAPerson {
                finding: finding.clone(),
                why: "a schema cannot be inferred from the files. A table with a column \
                      added after those files were written would infer a schema missing \
                      it, and an empty table has nothing to infer from at all"
                    .to_string(),
                decision: "Supply the schema the table is supposed to have, and republish \
                           its creation commit."
                    .to_string(),
            }),

            Finding::SchemaNotRepresentable { detail } => refused.push(NeedsAPerson {
                finding: finding.clone(),
                why: format!(
                    "{detail}. Changing a column's type means rewriting every file, which \
                     is a migration rather than a repair — and choosing the replacement \
                     type is a decision about what the data means"
                ),
                decision: "Choose the type the column should have, and republish the table \
                           through the publishing library."
                    .to_string(),
            }),

            Finding::KeyColumnMissing { column } => refused.push(NeedsAPerson {
                finding: finding.clone(),
                why: format!(
                    "'{column}' is declared as identifying a row and is not in the schema. \
                     Only a person knows whether the column was renamed, the declaration \
                     was a typo, or the table should not be keyed at all — and guessing \
                     wrong silently resolves distinct rows into one"
                ),
                decision: "Decide whether to correct the declaration, rename the column, or \
                           make the table append-only."
                    .to_string(),
            }),

            Finding::MissingRequiredField { file, field } => refused.push(NeedsAPerson {
                finding: finding.clone(),
                why: format!(
                    "the action for {file} omits '{field}'. Rewriting an action means \
                     rewriting a committed version, and this tool only ever appends"
                ),
                decision: "Republish the affected files through the publishing library, \
                           which writes every required field."
                    .to_string(),
            }),

            Finding::Unreadable { detail } => refused.push(NeedsAPerson {
                finding: finding.clone(),
                why: format!("{detail}. There is nothing here to repair"),
                decision: "Check that this is a table directory and that it is readable."
                    .to_string(),
            }),
        }
    }

    Plan {
        root: root.to_path_buf(),
        actions,
        refused,
    }
}

/// What a repair actually did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Outcome {
    /// The log version the repair wrote, if it wrote one.
    pub version: Option<u64>,
    /// Files whose statistics were recomputed.
    pub repaired: Vec<String>,
    /// Actions that could not be completed, and why.
    pub failed: Vec<(String, String)>,
    /// What verification says now.
    pub after: Report,
}

impl Outcome {
    /// Whether the table verifies clean afterwards.
    ///
    /// Checked by re-running verification rather than by assuming the actions worked. A
    /// repair tool that reports success without looking is one nobody should trust.
    #[must_use]
    pub fn is_clean_now(&self) -> bool {
        self.after.is_clean()
    }

    /// A summary line.
    #[must_use]
    pub fn summary(&self) -> String {
        let wrote = self.version.map_or_else(
            || "wrote nothing".to_string(),
            |v| format!("wrote version {v}"),
        );
        format!(
            "{wrote}, repaired {} file(s), {} failed — afterwards: {}",
            self.repaired.len(),
            self.failed.len(),
            self.after.summary()
        )
    }
}

/// Carry out a plan.
///
/// Every repair is appended as a **new log version**. Nothing is deleted, no committed
/// version is rewritten, and the broken state stays readable — so the repair is auditable,
/// revertible, and time travel to before it still works.
///
/// # Errors
///
/// Returns the outcome even when individual actions fail, because a partial repair is
/// worth reporting: three files fixed and one unreadable is more useful than a single
/// failure that hides the three.
pub fn apply(plan: &Plan) -> Result<Outcome, RepairError> {
    if plan.actions.is_empty() {
        return Ok(Outcome {
            version: None,
            repaired: Vec::new(),
            failed: Vec::new(),
            after: verify(&plan.root),
        });
    }

    let live = live_files(&plan.root).map_err(|error| RepairError::Unreadable {
        detail: error.to_string(),
    })?;
    let next_version = live.version.map_or(0, |version| version.saturating_add(1));

    let mut repaired = Vec::new();
    let mut failed = Vec::new();
    let mut replacements: Vec<Action> = Vec::new();

    for action in &plan.actions {
        let Repair::RecomputeStatistics { file } = action;
        let Some(existing) = live.files.iter().find(|f| &f.path == file) else {
            failed.push((
                file.clone(),
                "the file is no longer in the live set; nothing to repair".to_string(),
            ));
            continue;
        };
        match recompute(&plan.root, existing) {
            Ok(replacement) => {
                replacements.push(Action::Add(replacement));
                repaired.push(file.clone());
            }
            Err(reason) => failed.push((file.clone(), reason)),
        }
    }

    if replacements.is_empty() {
        return Ok(Outcome {
            version: None,
            repaired,
            failed,
            after: verify(&plan.root),
        });
    }

    // An `add` for a path already present replaces that path's entry rather than
    // duplicating it. That is what makes an append able to repair rather than only extend.
    commit(&plan.root, next_version, &replacements).map_err(|error| RepairError::Commit {
        version: next_version,
        detail: error.to_string(),
    })?;

    Ok(Outcome {
        version: Some(next_version),
        repaired,
        failed,
        // Re-verified rather than assumed. A repair tool that reports success without
        // looking is one nobody should trust.
        after: verify(&plan.root),
    })
}

/// Read a file and produce an `add` action carrying its true statistics.
fn recompute(root: &Path, existing: &AddFile) -> Result<AddFile, String> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let path = root.join(&existing.path);
    let file = std::fs::File::open(&path).map_err(|error| format!("opening: {error}"))?;
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(existing.size);

    let builder = ParquetRecordBatchReaderBuilder::try_new(
        file.try_clone()
            .map_err(|error| format!("reopening: {error}"))?,
    )
    .map_err(|error| format!("reading: {error}"))?;
    let rows = u64::try_from(builder.metadata().file_metadata().num_rows()).unwrap_or(0);

    // The whole file, because column bounds cannot be derived from a footer alone in a way
    // this system trusts — Parquet's own page statistics are optional and may be absent for
    // exactly the file that is missing them here.
    let reader = builder
        .build()
        .map_err(|error| format!("opening the reader: {error}"))?;
    let mut merged: std::collections::BTreeMap<String, sankhya_stats::ColumnStats> =
        std::collections::BTreeMap::new();
    for batch in reader {
        let batch = batch.map_err(|error| format!("decoding: {error}"))?;
        for (name, stats) in sankhya_table::column_stats(&batch) {
            match merged.entry(name.clone()) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(stats);
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    // A merge that cannot compare two bounds means the column holds values
                    // of incompatible shape. Widening to "unknown" is the only safe answer:
                    // a bound narrower than the truth causes a file to be skipped that
                    // holds matching rows, which is a wrong answer rather than a slow one.
                    if slot.get_mut().merge(&stats).is_err() {
                        return Err(format!(
                            "column '{name}' has bounds that cannot be compared across \
                             batches, so no bound can be recorded for it without risking \
                             one narrower than the truth"
                        ));
                    }
                }
            }
        }
    }

    Ok(AddFile::with_statistics(
        existing.path.clone(),
        bytes,
        existing.modification_time,
        &sankhya_table_delta::from_column_stats(rows, &merged),
    ))
}

/// Why a repair could not proceed at all.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RepairError {
    /// The table's log could not be read.
    Unreadable {
        /// What went wrong.
        detail: String,
    },
    /// The repair commit failed.
    ///
    /// The table is unchanged: a commit either lands or does not, so a failure here leaves
    /// exactly what was there before.
    Commit {
        /// Which version was attempted.
        version: u64,
        /// What went wrong.
        detail: String,
    },
}

impl fmt::Display for RepairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { detail } => write!(f, "the table could not be read: {detail}"),
            Self::Commit { version, detail } => write!(
                f,
                "the repair commit for version {version} failed: {detail}. The table is \
                 unchanged — a commit either lands or does not"
            ),
        }
    }
}

impl std::error::Error for RepairError {}
