//! What a table's log says happened, version by version.
//!
//! # Why this is a summary and not a diff
//!
//! The log records **file-level** adds and removes. It knows that a file arrived and another
//! left; it does not know which *rows* differ, and it cannot: a compaction rewrites files
//! without changing a single row, and would look like a total replacement to anything counting
//! files.
//!
//! So this reports what the log honestly knows --- versions, when, files added and removed, and
//! **whether the commit changed data at all** --- and does not pretend to a row-level
//! difference. That question is `M20`'s, and it needs a decision before it needs code: a diff
//! that reports a compaction as a change is worse than no diff, because it looks like an answer.
//!
//! # The rule a reader must understand
//!
//! **A commit remaining in the log is not the same as its data remaining on disk.** Retirement
//! deletes the files a merge replaced once nothing references them, so an old version listed
//! here may no longer be readable. Only a snapshot or a clone keeps one alive.

use std::path::Path;

use crate::log::{read_actions, Action, CommitError, Version};

/// One commit, as the log describes it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    /// The version this commit produced.
    pub version: Version,
    /// When, in milliseconds from the epoch, or `None` for a commit that recorded no time.
    ///
    /// Taken from the newest file the commit touched. A commit that only changed metadata has
    /// no file to take a time from, and reporting zero would be a date in 1970 presented as a
    /// fact.
    pub at: Option<i64>,
    /// How many files it added.
    pub added: usize,
    /// How many it removed.
    pub removed: usize,
    /// The bytes the added files hold.
    pub bytes_added: u64,
    /// Whether this commit changed **data**, as the writer declared.
    ///
    /// `false` for a compaction: it rewrites files and changes no rows. The distinction is the
    /// log's own --- `dataChange` on every add and remove --- and reporting it is what keeps
    /// this from telling somebody their table changed when it did not.
    pub changed_data: bool,
    /// Whether it declared the table's schema, which the first commit of a table does.
    pub declared_schema: bool,
}

/// Every commit a table's log holds, oldest first.
///
/// # Errors
///
/// [`CommitError`] when the log cannot be read. An unreadable log is reported rather than
/// summarised as empty: "this table has no history" and "I could not read its history" lead to
/// opposite actions.
pub fn history(table_root: &Path) -> Result<Vec<Change>, CommitError> {
    let mut changes: Vec<Change> = Vec::new();
    for (version, action) in read_actions(table_root)? {
        // One row per version, accumulated: a commit is several actions and a reader thinks of
        // it as one event.
        let at = changes.iter().position(|change| change.version == version);
        let index = match at {
            Some(index) => index,
            None => {
                changes.push(Change {
                    version,
                    at: None,
                    added: 0,
                    removed: 0,
                    bytes_added: 0,
                    changed_data: false,
                    declared_schema: false,
                });
                changes.len().saturating_sub(1)
            }
        };
        let Some(change) = changes.get_mut(index) else {
            continue;
        };
        match action {
            Action::Add(file) => {
                change.added = change.added.saturating_add(1);
                change.bytes_added = change.bytes_added.saturating_add(file.size);
                change.changed_data |= file.data_change;
                change.at = Some(
                    change
                        .at
                        .map_or(file.modification_time, |seen| seen.max(file.modification_time)),
                );
            }
            Action::Remove(file) => {
                change.removed = change.removed.saturating_add(1);
                change.changed_data |= file.data_change;
                change.at = Some(
                    change
                        .at
                        .map_or(file.deletion_timestamp, |seen| {
                            seen.max(file.deletion_timestamp)
                        }),
                );
            }
            Action::Metadata(_) => change.declared_schema = true,
            Action::Protocol { .. } => {}
        }
    }
    Ok(changes)
}

/// What a commit did, in one word, for a person reading a list of them.
///
/// Derived rather than stored, because the log records facts and this is a reading of them ---
/// and a reading that lived in the log would be one more thing a writer could get wrong.
#[must_use]
pub fn describe(change: &Change) -> &'static str {
    if change.declared_schema && change.added == 0 && change.removed == 0 {
        "created"
    } else if !change.changed_data && change.added > 0 && change.removed > 0 {
        // A compaction: files replaced, no rows changed. Named, because a reader who sees this
        // as "changed" will stop trusting the column.
        "compacted"
    } else if change.added > 0 && change.removed == 0 {
        "appended"
    } else if change.removed > 0 && change.added == 0 {
        "removed"
    } else if change.added > 0 {
        "rewritten"
    } else {
        "metadata"
    }
}

/// What changed between two versions of a table.
///
/// [ADR-0024](../../../docs/adr/0024-what-a-difference-between-two-versions-is.md): a difference
/// is a change to **rows**, and a compaction is not one. Every number here is read from the log
/// and none of them opens a Parquet file, so this is answerable on a table nobody would consider
/// scanning --- which is what makes it usable where it is wanted, between two reporting runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Difference {
    /// How many commits between the two versions declared a data change.
    pub commits: usize,
    /// Files those commits added, and the rows in them.
    pub files_added: usize,
    /// The rows those files hold, from each file's own `numRecords`.
    pub rows_added: u64,
    /// Files those commits removed, and the rows that were in them.
    pub files_removed: usize,
    /// The rows those files held, looked up from the commit that added each one.
    pub rows_removed: u64,
    /// Compactions in the range, counted and **not** folded into the numbers above.
    ///
    /// Named rather than omitted. A diff reporting *nothing changed* over a range in which
    /// every file was rewritten tells the truth about rows and leaves the reader wondering why
    /// the storage looks nothing like it did --- and a number withheld to avoid confusing
    /// somebody is a number they will need and will then get somewhere less careful.
    pub compactions: usize,
}

/// Why a difference could not be computed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NoDifference {
    /// The log could not be read.
    Unreadable(String),
    /// A version nobody has.
    ///
    /// Refused rather than clamped to the newest, which is `SET VERSION OF`'s rule and is here
    /// for the same reason: a caller who asked for a version that does not exist must not be
    /// handed a different one silently.
    NoSuchVersion {
        /// The version asked for.
        wanted: Version,
        /// The newest this table has.
        newest: Version,
    },
    /// The earlier version is not earlier.
    Backwards {
        /// The one given first.
        from: Version,
        /// The one given second.
        to: Version,
    },
}

impl std::fmt::Display for NoDifference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(said) => write!(f, "this table's log could not be read: {said}"),
            Self::NoSuchVersion { wanted, newest } => write!(
                f,
                "this table has no version {wanted}; its newest is {newest}. Refused rather \
                 than answered about the newest: a caller who asked about a version nobody has \
                 would be handed a difference that is real and is not the one they asked for"
            ),
            Self::Backwards { from, to } => write!(
                f,
                "version {from} is not earlier than {to}. A difference runs forwards, and \
                 reversing it silently would report additions as removals"
            ),
        }
    }
}

impl std::error::Error for NoDifference {}

/// What changed between two versions, exclusive of `from` and inclusive of `to`.
///
/// `BETWEEN 4 AND 7` means *what happened after 4, up to and including 7* --- the changes that
/// would be new to a reader who last read version 4. Stated because the other reading is
/// defensible and the two differ by exactly one commit, which is the difference between a
/// reconciliation that ties out and one that is off by whatever that commit carried.
///
/// # Errors
///
/// [`NoDifference`] for an unreadable log, a version nobody has, or a range that runs backwards.
pub fn difference(
    table_root: &Path,
    from: Version,
    to: Version,
) -> Result<Difference, NoDifference> {
    if from > to {
        return Err(NoDifference::Backwards { from, to });
    }
    let actions =
        read_actions(table_root).map_err(|error| NoDifference::Unreadable(error.to_string()))?;

    // Every file's row count, from the commit that added it --- because a removal names a path
    // and says nothing about what was in it. Built over the **whole** log rather than the range,
    // since a file removed in the range was added before it.
    let mut rows_in: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut newest: Version = 0;
    for (version, action) in &actions {
        if *version > newest {
            newest = *version;
        }
        if let Action::Add(add) = action {
            rows_in.insert(add.path.clone(), records_in(add));
        }
    }
    for wanted in [from, to] {
        if wanted > newest {
            return Err(NoDifference::NoSuchVersion { wanted, newest });
        }
    }

    // Which commits in the range declared a data change, and which were compactions. Decided
    // per commit rather than per action: a commit is one event to a reader, and one action of
    // it declaring a change makes the commit one.
    let mut changed: std::collections::BTreeSet<Version> = std::collections::BTreeSet::new();
    let mut touched: std::collections::BTreeSet<Version> = std::collections::BTreeSet::new();
    for (version, action) in &actions {
        if *version <= from || *version > to {
            continue;
        }
        let (declares, is_file) = match action {
            Action::Add(add) => (add.data_change, true),
            Action::Remove(remove) => (remove.data_change, true),
            _ => (false, false),
        };
        if is_file {
            touched.insert(*version);
        }
        if declares {
            changed.insert(*version);
        }
    }

    let mut difference = Difference {
        commits: changed.len(),
        compactions: touched.difference(&changed).count(),
        ..Difference::default()
    };
    for (version, action) in &actions {
        if !changed.contains(version) {
            continue;
        }
        match action {
            Action::Add(add) if add.data_change => {
                difference.files_added += 1;
                difference.rows_added += records_in(add);
            }
            Action::Remove(remove) if remove.data_change => {
                difference.files_removed += 1;
                // The rows that were in it, from the commit that added it. A file this log
                // never added --- which a hand-edited log could contain --- contributes no
                // rows rather than a guess.
                difference.rows_removed += rows_in.get(&remove.path).copied().unwrap_or(0);
            }
            _ => {}
        }
    }
    Ok(difference)
}

/// The rows one added file holds, from its own statistics.
fn records_in(add: &crate::log::AddFile) -> u64 {
    add.stats
        .as_deref()
        .and_then(|text| serde_json::from_str::<crate::stats::FileStatistics>(text).ok())
        .map_or(0, |statistics| statistics.num_records)
}
