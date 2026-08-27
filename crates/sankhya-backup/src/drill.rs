//! Proving the backup restores, and keeping the proof.
//!
//! # "An untested backup is a rumour"
//!
//! `FR-OPS-15`'s own words, and the whole reason this module exists rather than a
//! `verify_manifest()` function that checks the files are present.
//!
//! **A file-presence check passes on a truncated Parquet.** It passes on a file whose bytes
//! were replaced with a different table's. It passes on every failure mode that matters,
//! because the thing that goes wrong with a backup is almost never that a file is missing
//! --- a missing file is loud. What goes wrong is that a file is there and wrong.
//!
//! So a drill **reads the data back and recomputes its digest**. That is expensive, and it
//! is the only version of this that means anything.
//!
//! # The evidence records failures, or it is marketing
//!
//! A drill history with no failures in three years describes one of two situations, and
//! nothing in the history says which: a very good system, or a drill that does not really
//! run. So the record is append-only, a failure is written with the same ceremony as a
//! success, and a drill that could not start is recorded distinctly from one that ran and
//! passed --- the same distinction the diagnostic draws between a clean check and one that
//! could not run, and for the same reason.

use crate::manifest::Manifest;
use sankhya_ingest::TableDigest;
use sankhya_table_delta::Version;
use std::fmt;
use std::path::{Path, PathBuf};

/// The file a drill's evidence is appended to.
pub const EVIDENCE_FILE: &str = "restore-drills.jsonl";

/// What happened to one table during a drill.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TableOutcome {
    /// The data read back and digested to what the manifest recorded.
    Verified {
        /// How many rows were read.
        rows: u64,
    },
    /// The data read back and digested to something else.
    ///
    /// The dangerous outcome, and the one a presence check never reaches.
    DigestMismatch {
        /// What the manifest recorded.
        expected_rows: u64,
        /// What was actually there.
        found_rows: u64,
        /// Whether the checksums agreed, when the row counts did.
        checksum_agreed: bool,
    },
    /// The table could not be read at all.
    Unreadable {
        /// What went wrong.
        why: String,
    },
    /// The manifest's own record of this table is unusable.
    ///
    /// Distinct from a mismatch, because it indicts the *manifest* rather than the data, and
    /// sends an operator to a different artefact.
    ManifestUnreadable,
}

impl TableOutcome {
    /// Whether this table is proven restorable.
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

/// What a whole drill found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Evidence {
    /// Which backup was drilled.
    pub backup: String,
    /// When, in microseconds from the epoch.
    pub at: i64,
    /// Per table, in the manifest's order.
    pub tables: Vec<(String, TableOutcome)>,
    /// Why the drill could not start at all, if it could not.
    ///
    /// A drill that never ran and a drill that ran and passed both produce no failures. If
    /// they land in the same record, the history says a backup was proven when nothing
    /// looked at it.
    pub could_not_start: Option<String>,
}

impl Evidence {
    /// Whether every table verified and the drill actually ran.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.could_not_start.is_none()
            && !self.tables.is_empty()
            && self.tables.iter().all(|(_, outcome)| outcome.is_verified())
    }

    /// The tables that did not verify.
    #[must_use]
    pub fn failures(&self) -> Vec<&(String, TableOutcome)> {
        self.tables
            .iter()
            .filter(|(_, outcome)| !outcome.is_verified())
            .collect()
    }

    /// One line for the append-only record.
    ///
    /// Hand-built rather than derived, so the fields are chosen deliberately: this file is
    /// read by a person during an incident, and a serialiser's idea of a good shape is not
    /// the same as a legible one.
    #[must_use]
    pub fn to_line(&self) -> String {
        let verdict = if self.could_not_start.is_some() {
            "could-not-start"
        } else if self.passed() {
            "pass"
        } else {
            "FAIL"
        };
        let detail = match &self.could_not_start {
            Some(why) => format!(", \"why\": {}", quote(why)),
            None => {
                let failed: Vec<String> = self
                    .failures()
                    .iter()
                    .map(|(table, outcome)| format!("{table}: {outcome}"))
                    .collect();
                if failed.is_empty() {
                    String::new()
                } else {
                    format!(", \"failures\": {}", quote(&failed.join("; ")))
                }
            }
        };
        format!(
            "{{\"at\": {}, \"backup\": {}, \"verdict\": \"{verdict}\", \"tables\": {}{detail}}}",
            self.at,
            quote(&self.backup),
            self.tables.len()
        )
    }
}

/// A JSON string, with the three escapes that matter.
fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

/// How a drill reads a table's data back.
///
/// A trait so the drill can be exercised against a table on disk, against a restored copy in
/// a scratch location, or against a fixture --- and so the expensive real implementation is
/// not the only thing the drill's own logic can be tested through.
pub trait ReadsBack {
    /// Digest the data of `table` at `version`.
    ///
    /// # Errors
    ///
    /// Anything that stopped it, as text an operator can act on.
    fn digest_of(&self, table: &str, version: Version) -> Result<TableDigest, String>;
}

/// Run a drill against a manifest.
///
/// Never fails: every way it can go wrong is an outcome to be recorded rather than an error
/// to propagate. A drill that returns `Err` produces no evidence, and evidence is the
/// deliverable.
#[must_use]
pub fn drill(manifest: &Manifest, source: &dyn ReadsBack, now: i64) -> Evidence {
    let mut tables = Vec::with_capacity(manifest.tables.len());
    for snapshot in &manifest.tables {
        let outcome = match snapshot.digest() {
            None => TableOutcome::ManifestUnreadable,
            Some(expected) => match source.digest_of(&snapshot.table, snapshot.version) {
                Err(why) => TableOutcome::Unreadable { why },
                Ok(found) if found == expected => TableOutcome::Verified {
                    rows: found.rows(),
                },
                Ok(found) => TableOutcome::DigestMismatch {
                    expected_rows: expected.rows(),
                    found_rows: found.rows(),
                    // Reported separately, because the two failures mean different things:
                    // a row count that matches with a different checksum is altered data,
                    // and a different row count is lost or duplicated data.
                    checksum_agreed: found.checksum() == expected.checksum(),
                },
            },
        };
        tables.push((snapshot.table.clone(), outcome));
    }

    Evidence {
        backup: manifest.id.to_string(),
        at: now,
        tables,
        could_not_start: None,
    }
}

/// Record that a drill could not run.
#[must_use]
pub fn could_not_start(backup: &str, at: i64, why: impl Into<String>) -> Evidence {
    Evidence {
        backup: backup.to_string(),
        at,
        tables: Vec::new(),
        could_not_start: Some(why.into()),
    }
}

/// Append evidence to the record.
///
/// # Errors
///
/// When the file cannot be written. Surfaced rather than swallowed: evidence that was not
/// recorded is the same as a drill that did not happen, and the caller has to know which it
/// has.
pub fn record(directory: &Path, evidence: &Evidence) -> Result<(), EvidenceError> {
    use std::io::Write as _;
    std::fs::create_dir_all(directory).map_err(|error| EvidenceError {
        path: directory.to_path_buf(),
        why: error.to_string(),
    })?;
    let path = directory.join(EVIDENCE_FILE);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| EvidenceError {
            path: path.clone(),
            why: error.to_string(),
        })?;
    writeln!(file, "{}", evidence.to_line()).map_err(|error| EvidenceError {
        path,
        why: error.to_string(),
    })
}

/// When a drill last passed, from the record.
///
/// `None` means no drill has ever passed --- which includes the case where drills have run
/// and all of them failed. That is the answer an operator needs, and it is why this reads
/// the record rather than a stored "last drill" timestamp somebody might update on a
/// failure.
#[must_use]
pub fn last_pass(directory: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(directory.join(EVIDENCE_FILE)).ok()?;
    text.lines()
        .filter(|line| line.contains("\"verdict\": \"pass\""))
        .filter_map(|line| {
            let rest = line.strip_prefix("{\"at\": ")?;
            let (number, _) = rest.split_once(',')?;
            number.trim().parse::<i64>().ok()
        })
        .max()
}

/// The evidence could not be written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EvidenceError {
    /// Which path.
    pub path: PathBuf,
    /// What the filesystem said.
    pub why: String,
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the restore-drill evidence at {} could not be written ({}); a drill whose \
             result was not recorded is indistinguishable from one that never ran, and \
             `FR-OPS-15` asks for retained evidence rather than for a drill",
            self.path.display(),
            self.why
        )
    }
}

impl std::error::Error for EvidenceError {}

impl fmt::Display for TableOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Verified { rows } => write!(f, "verified, {rows} row(s)"),
            Self::DigestMismatch {
                expected_rows,
                found_rows,
                checksum_agreed,
            } => {
                if expected_rows == found_rows && !checksum_agreed {
                    write!(
                        f,
                        "the row count matches at {found_rows} and the data does not — rows \
                         were altered, not lost"
                    )
                } else {
                    write!(
                        f,
                        "expected {expected_rows} row(s) and found {found_rows} — rows were \
                         lost or duplicated"
                    )
                }
            }
            Self::Unreadable { why } => write!(f, "could not be read ({why})"),
            Self::ManifestUnreadable => {
                f.write_str("the manifest's own record of this table is unreadable")
            }
        }
    }
}
