//! Checking a table written by anything at all.
//!
//! Making the publishing library the supported path is a recommendation, and a
//! recommendation is not an invariant. A table can arrive written by an older version, by a
//! script somebody wrote before the library existed, or by a vendor who did not ask.
//!
//! So the reader does not assume the library was used.
//!
//! # Why this reports *what* is wrong rather than *whether*
//!
//! "This table is invalid" sends someone to open a support ticket. "Fourteen files have no
//! column statistics, so every query against this table reads all of them" sends them to
//! fix it. The second is the same information, expressed so that acting on it is possible.
//!
//! # Why it is separate from reading
//!
//! A table that fails verification may be perfectly readable --- missing statistics make it
//! slow, not wrong. Coupling verification to reading would mean a table that is largely
//! correct could not be read at all, which serves nobody. So reading never calls this, and
//! this never reads data.

use crate::class::{key_columns, TableClass};
use sankhya_table_delta::{read_actions, schema_from_string, Action, Metadata};
use std::fmt;
use std::path::Path;

/// What verification found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Report {
    /// The table's class, as its log declares it.
    pub class: TableClass,
    /// The identifying columns, if it is mutable.
    pub key_columns: Vec<String>,
    /// How many live files it has.
    pub files: usize,
    /// How many of those carry column statistics.
    pub files_with_statistics: usize,
    /// Everything wrong with it, in the order found.
    pub findings: Vec<Finding>,
}

impl Report {
    /// Whether nothing is wrong.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Whether anything found would produce a *wrong answer* rather than a slow one.
    ///
    /// The distinction operators need most. A table that is merely slow can wait until
    /// Monday; one that returns wrong answers cannot.
    #[must_use]
    pub fn has_correctness_findings(&self) -> bool {
        self.findings.iter().any(Finding::affects_correctness)
    }

    /// A summary line.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_clean() {
            return format!(
                "{} table, {} file(s), all with statistics — nothing to report",
                self.class, self.files
            );
        }
        format!(
            "{} table, {} file(s), {} with statistics — {} finding(s), {} affecting correctness",
            self.class,
            self.files,
            self.files_with_statistics,
            self.findings.len(),
            self.findings
                .iter()
                .filter(|f| f.affects_correctness())
                .count()
        )
    }
}

/// One thing wrong with a table.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Finding {
    /// The log has no metadata action, so the table has no declared schema.
    NoMetadata,
    /// The log could not be read at all.
    Unreadable {
        /// What went wrong.
        detail: String,
    },
    /// The declared schema does not round-trip through this system's type mapping.
    SchemaNotRepresentable {
        /// What the mapping said.
        detail: String,
    },
    /// A live file carries no column statistics.
    NoStatistics {
        /// Which file.
        file: String,
    },
    /// A live file declares no row count.
    NoRowCount {
        /// Which file.
        file: String,
    },
    /// A mutable table names a key column its schema does not have.
    KeyColumnMissing {
        /// Which column.
        column: String,
    },
    /// An `add` action omits a field the format requires.
    ///
    /// The finding this system made about itself, before an independent implementation
    /// found it: a reader ignores a field it never writes, so the writer's own reader is
    /// perfectly happy with a log nothing else will accept.
    MissingRequiredField {
        /// Which file's action.
        file: String,
        /// Which field.
        field: String,
    },
}

impl Finding {
    /// Whether this produces a wrong answer rather than a slow one.
    #[must_use]
    pub const fn affects_correctness(&self) -> bool {
        match self {
            // Slow, not wrong. Every query reads the file instead of pruning it.
            Self::NoStatistics { .. } => false,
            Self::NoMetadata
            | Self::Unreadable { .. }
            | Self::SchemaNotRepresentable { .. }
            | Self::NoRowCount { .. }
            | Self::KeyColumnMissing { .. }
            | Self::MissingRequiredField { .. } => true,
        }
    }

    /// What to do about it.
    #[must_use]
    pub fn remediation(&self) -> String {
        match self {
            Self::NoMetadata => {
                "The table has no schema. Republish it, or restore its log.".to_string()
            }
            Self::Unreadable { .. } => {
                "The log could not be read. Check permissions and that the directory is a \
                 table."
                    .to_string()
            }
            Self::SchemaNotRepresentable { .. } => {
                "A column's type is one this system cannot read exactly. Republish that \
                 column as a type it can, rather than accepting values that are merely \
                 similar."
                    .to_string()
            }
            Self::NoStatistics { file } => format!(
                "{file} cannot be pruned, so every query reads it. Rewrite it through the \
                 publishing library, which always records statistics. This is a \
                 performance problem, not a correctness one."
            ),
            Self::NoRowCount { file } => format!(
                "{file} declares no row count, so planning must open it to learn one — and \
                 a cardinality the planner does not have is a join it orders badly."
            ),
            Self::KeyColumnMissing { column } => format!(
                "The table is mutable and names '{column}' as identifying a row, but has no \
                 such column. Every merge returns nothing for that key."
            ),
            Self::MissingRequiredField { file, field } => format!(
                "The action for {file} omits '{field}', which the format requires. This \
                 system's reader will not notice, because a reader ignores a field it never \
                 writes — an independent implementation will refuse the table outright."
            ),
        }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = if self.affects_correctness() {
            "WRONG"
        } else {
            "slow"
        };
        write!(f, "[{severity}] {}", self.remediation())
    }
}

/// Verify a table's log against the invariants the publishing library maintains.
///
/// Reads only the log. No data file is opened, so this is cheap enough to run over a whole
/// warehouse at startup.
#[must_use]
pub fn verify(table_root: &Path) -> Report {
    let mut findings = Vec::new();

    // A directory with no log is not a table. Saying "this table has no metadata" would
    // send the reader looking for a corrupt log where there is no table at all.
    if !table_root.join("_delta_log").is_dir() {
        return Report {
            class: TableClass::External,
            key_columns: Vec::new(),
            files: 0,
            files_with_statistics: 0,
            findings: vec![Finding::Unreadable {
                detail: format!(
                    "{} has no _delta_log, so it is not a table",
                    table_root.display()
                ),
            }],
        };
    }

    let actions = match read_actions(table_root) {
        Ok(actions) => actions,
        Err(error) => {
            return Report {
                class: TableClass::External,
                key_columns: Vec::new(),
                files: 0,
                files_with_statistics: 0,
                findings: vec![Finding::Unreadable {
                    detail: error.to_string(),
                }],
            };
        }
    };

    // The *last* metadata action. A schema evolution writes a new one, and reading the
    // first would verify the table's original shape forever.
    let metadata: Option<&Metadata> = actions.iter().rev().find_map(|(_, action)| match action {
        Action::Metadata(metadata) => Some(metadata),
        _ => None,
    });

    let Some(metadata) = metadata else {
        findings.push(Finding::NoMetadata);
        return Report {
            class: TableClass::External,
            key_columns: Vec::new(),
            files: 0,
            files_with_statistics: 0,
            findings,
        };
    };

    let class = TableClass::from_configuration(&metadata.configuration);
    let keys = key_columns(&metadata.configuration);

    let schema = match schema_from_string(&metadata.schema_string) {
        Ok(schema) => Some(schema),
        Err(error) => {
            findings.push(Finding::SchemaNotRepresentable {
                detail: error.to_string(),
            });
            None
        }
    };

    if let Some(schema) = &schema {
        for column in &keys {
            if schema.field_with_name(column).is_err() {
                findings.push(Finding::KeyColumnMissing {
                    column: column.clone(),
                });
            }
        }
    }

    // Live files only: one that was added and later removed is not this table's problem.
    let live = sankhya_table_delta::live_files(table_root).ok();
    let files: &[sankhya_table_delta::AddFile] =
        live.as_ref().map_or(&[], |set| set.files.as_slice());

    let mut with_statistics = 0usize;
    for file in files {
        match &file.stats {
            None => findings.push(Finding::NoStatistics {
                file: file.path.clone(),
            }),
            Some(stats) => {
                with_statistics += 1;
                if !stats.contains("numRecords") {
                    findings.push(Finding::NoRowCount {
                        file: file.path.clone(),
                    });
                }
            }
        }
    }

    Report {
        class,
        key_columns: keys,
        files: files.len(),
        files_with_statistics: with_statistics,
        findings,
    }
}
