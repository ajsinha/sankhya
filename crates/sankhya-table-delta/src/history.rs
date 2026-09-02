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
