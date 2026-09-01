//! Expiring a partition whose retention has run out.
//!
//! # Why this exists, and what it is not
//!
//! A quarantine that only grows is `RSK-35` in another costume: an accumulation nobody is
//! responsible for, each record individually reasonable, with no day on which anybody could
//! have decided otherwise. It is worse than accumulating backups, because it holds precisely
//! the records nobody looked at.
//!
//! So [`ADR-0018`](https://github.com/ajsinha/sankhya/blob/main/docs/adr/0018-a-record-that-does-not-fit.md)
//! makes a retention **mandatory** on every feed, and this is what enforces it.
//!
//! # Detached, never deleted row by row
//!
//! `DEC-23` says purge is by partition detach and never by row deletion, and nothing here is
//! an exception. Expiry commits `remove` actions for the files of a partition that has aged
//! out; the files stay on disk and are reclaimed by retirement after its grace period, which
//! is the protection a reader mid-query needs.
//!
//! It also means expiry is **reversible until retirement runs**: a partition detached by
//! mistake is re-attachable, which is exactly the property that makes running this
//! automatically defensible where running a delete would not be.
//!
//! # One quarantine, several feeds, one retention
//!
//! Every feed writes into the same quarantine table, and a partition holds whatever arrived
//! that day --- from feeds that may declare different retentions. A partition is therefore
//! kept until the **longest** retention any feed declares has passed.
//!
//! The alternative is one feed's short retention deleting another feed's records, which is a
//! feed being able to destroy data it did not produce by editing its own configuration.

use sankhya_table_delta::{commit, Action, CommitError, LiveSet, RemoveFile, Version};
use std::collections::BTreeSet;
use std::path::Path;

/// What expiry removed.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Expired {
    /// The partitions detached, as their directory names.
    pub partitions: BTreeSet<String>,
    /// How many files those partitions held.
    pub files: usize,
}

impl Expired {
    /// Whether anything was detached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.partitions.is_empty()
    }
}

/// The partition directory a file sits in, if it is in one.
///
/// Files at the table root belong to no partition and are never expired by date: a table
/// written before it was partitioned holds them, and detaching them on a guess about their
/// contents would be deleting data on no evidence.
fn partition_of(path: &str) -> Option<&str> {
    let (directory, _) = path.split_once('/')?;
    directory
        .starts_with(&format!("{}=", sankhya_schema::DATA_DATE_COLUMN))
        .then_some(directory)
}

/// The date a partition directory names, as days since the epoch.
///
/// `None` for a directory this does not understand, which is then never expired. Refusing to
/// guess is the whole point: a partition whose date cannot be read is one whose age is
/// unknown, and removing it would be acting on an assumption.
fn day_of(partition: &str) -> Option<i32> {
    let value = partition.split_once('=').map(|(_, value)| value)?;
    let mut parts = value.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let month = time::Month::try_from(month).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    // Julian day of 1970-01-01, so the result is days since the epoch.
    Some(date.to_julian_day() - 2_440_588)
}

/// Plan which partitions have aged out.
///
/// `today` and `retain_days` are given rather than read, so the decision is reproducible and
/// a test can state the day rather than wait for one.
#[must_use]
pub fn plan(live: &LiveSet, today: i32, retain_days: u32) -> Expired {
    let mut expired = Expired::default();
    // Saturating, so a retention longer than the epoch keeps everything rather than wrapping
    // into a cutoff in the future — which would expire the whole table.
    let cutoff = today.saturating_sub(i32::try_from(retain_days).unwrap_or(i32::MAX));
    for file in &live.files {
        let Some(partition) = partition_of(&file.path) else {
            continue;
        };
        let Some(day) = day_of(partition) else {
            continue;
        };
        if day < cutoff {
            expired.partitions.insert(partition.to_owned());
            expired.files = expired.files.saturating_add(1);
        }
    }
    expired
}

/// Detach the planned partitions, committing at `version`.
///
/// # Errors
///
/// [`CommitError`] when the commit cannot be made, including when another writer took the
/// version --- which the caller retries with a later one, exactly as compaction does.
pub fn detach(
    table_root: &Path,
    version: Version,
    live: &LiveSet,
    expired: &Expired,
    now: i64,
) -> std::result::Result<Version, CommitError> {
    let actions: Vec<Action> = live
        .files
        .iter()
        .filter(|file| {
            partition_of(&file.path).is_some_and(|partition| expired.partitions.contains(partition))
        })
        // `rewritten` rather than a deletion marker: the data is leaving the live set, and
        // the files themselves are reclaimed by retirement after its grace period.
        .map(|file| Action::Remove(RemoveFile::rewritten(file.path.clone(), now)))
        .collect();
    commit(table_root, version, &actions)
}
