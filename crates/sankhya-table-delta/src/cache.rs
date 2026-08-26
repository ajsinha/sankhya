//! Remembering a table's file set between query plans.
//!
//! # What this is for, and what it is not
//!
//! Every query plan needs the table's live file set, and computing it means replaying
//! the log. Replay is linear in the table's history, and a long-running process replans
//! the same table thousands of times — so a process that has already read a table's log
//! is paying for its whole history again on every query, to learn about the handful of
//! commits that have happened since.
//!
//! The cache pays for what has changed instead. It keeps the live set it last computed
//! and the version it was computed at, checks whether anything newer exists, and replays
//! only that.
//!
//! **It does nothing for a cold process**, and that is not a gap to be apologised for —
//! it is a different problem with a different answer. A process starting fresh has to
//! read something, and what it should read is a checkpoint rather than the whole log.
//! Checkpoints are not built.
//!
//! # Why staleness is impossible rather than unlikely
//!
//! A cache of a table's contents that can go stale is a cache that returns wrong answers.
//! This one cannot, because it never trusts its own version: every lookup asks the log
//! whether anything newer exists.
//!
//! Asking is one filesystem probe, not a directory listing. Versions are contiguous —
//! [`commit`](crate::commit) refuses anything that would leave a gap — so a reader that
//! knows the state at version *n* need only ask whether *n+1* exists. Listing instead
//! would make every lookup proportional to the table's whole history, which is the cost
//! the cache exists to remove.
//!
//! There is no invalidation, no expiry, and no notification to miss.

use crate::log::{newest_after, CommitError, LiveSet, Replay};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A cache of table file sets, safe to share across query plans.
#[derive(Debug, Default)]
pub struct LogCache {
    /// The replay is kept rather than the live set, so resuming does not rebuild the
    /// index over every live file — which would swap "linear in the history" for
    /// "linear in the table", better and still not right.
    entries: Mutex<HashMap<PathBuf, Replay>>,
}

/// What a lookup did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Nothing had changed; the cached set was returned unchanged.
    Current,
    /// Some commits were read and applied to the cached set.
    Advanced { commits_read: usize },
    /// Nothing was cached; the whole log was replayed.
    Cold,
}

impl LogCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The table's live file set, as of now.
    ///
    /// # Errors
    ///
    /// Returns an error if the log cannot be listed, read, or replayed. A lock poisoned
    /// by a panic in another thread is recovered from rather than propagated: the cached
    /// value is derived state and can be recomputed, so refusing every future query over
    /// it would turn one panic into a permanent outage.
    pub fn live_files(&self, table_root: &Path) -> Result<(LiveSet, Outcome), CommitError> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let cached = entries.get(table_root).map(Replay::version);
        let newest = newest_after(table_root, cached.flatten());

        // A cached version the log no longer has means the table was rebuilt underneath
        // us — dropped and recreated at the same path. Resuming from it would carry
        // files that no longer exist and every query would then read files that are not
        // there, so the entry is discarded and the log read from the beginning.
        let rebuilt = match (cached.flatten(), newest) {
            (Some(c), Some(n)) => c > n,
            (Some(_), None) => true,
            _ => false,
        };
        if rebuilt {
            entries.remove(table_root);
        }

        let outcome = match (cached, rebuilt) {
            (None, _) | (_, true) => Outcome::Cold,
            (Some(c), false) if c == newest => Outcome::Current,
            (Some(c), false) => Outcome::Advanced {
                commits_read: usize::try_from(newest.unwrap_or(0).saturating_sub(c.unwrap_or(0)))
                    .unwrap_or(0),
            },
        };

        // `Current` is only produced when the entry was found above, so this lookup
        // cannot miss. Falling through to the replay below if it ever did is both
        // correct and slower, which is the right way round for an impossible case.
        if outcome == Outcome::Current {
            if let Some(replay) = entries.get(table_root) {
                return Ok((replay.live_set(), outcome));
            }
        }

        let replay = entries.entry(table_root.to_path_buf()).or_default();
        replay.advance(table_root)?;
        Ok((replay.live_set(), outcome))
    }

    /// Forget everything.
    ///
    /// Correctness never requires this — the cache cannot go stale. It exists so a
    /// process can release memory for tables it will not query again.
    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// How many tables are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
