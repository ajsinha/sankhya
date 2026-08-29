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
//!
//! # Why one lock over every table was the wrong shape
//!
//! Until 2026-08-29 this held a single `Mutex<HashMap<PathBuf, Replay>>` --- and held it
//! **across the filesystem work**: the probe for a newer version, and the replay of whatever
//! it found. Every query on every table took that lock, so a cold replay of a large log blocked
//! queries against unrelated tables for its whole duration.
//!
//! Nothing about it was unsafe, which is why it survived: it returned correct answers and no
//! test could tell. It was a throughput defect, and the question that finds those is not "can
//! this corrupt?" but "does this serialize?".
//!
//! Two changes, and the first matters more than the second. **The lock is no longer held across
//! I/O**: the map is locked only long enough to find a table's entry, and the reading happens
//! under that table's own lock. And the map itself is **striped**, so two tables rarely touch
//! the same one.
//!
//! Striping alone would have been the lesser fix. It reduces how many threads wait; taking the
//! I/O out of the critical section changes what they are waiting for. Two queries on the *same*
//! table still serialize, and must --- they are advancing the same replay.

use crate::log::{newest_after, CommitError, LiveSet, Replay};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How many independent maps the cache is split across.
///
/// A power of two, and well above any plausible core count: two tables sharing a shard wait
/// on each other for the length of a map lookup, which is cheap, and the cost of more shards
/// is one empty `HashMap` each.
const SHARDS: usize = 64;

/// A cache of table file sets, safe to share across query plans.
#[derive(Debug)]
pub struct LogCache {
    /// The replay is kept rather than the live set, so resuming does not rebuild the
    /// index over every live file — which would swap "linear in the history" for
    /// "linear in the table", better and still not right.
    ///
    /// Each table's replay is behind **its own** lock, and the shard lock is held only to find
    /// it. That is what keeps a long replay of one table off every other table's path.
    shards: Vec<Mutex<HashMap<PathBuf, Arc<Mutex<Replay>>>>>,
}

impl Default for LogCache {
    fn default() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
        }
    }
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
        // The shard is locked only to find this table's entry, and released before any
        // filesystem work happens. Holding it across the probe and the replay is what made a
        // cold read of one table block queries against every other.
        let Some(shard) = self.shard_for(table_root) else {
            // A cache with no shards holds nothing. Replaying directly is correct and slower,
            // which is the right way round for a case that cannot happen.
            let mut replay = Replay::default();
            replay.advance(table_root)?;
            return Ok((replay.live_set(), Outcome::Cold));
        };
        let entry = {
            let mut shard = shard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(
                shard
                    .entry(table_root.to_path_buf())
                    .or_insert_with(|| Arc::new(Mutex::new(Replay::default()))),
            )
        };

        // This table's own lock. Two queries on one table serialize here and must: they are
        // advancing the same replay, and letting both advance it would apply the same commits
        // twice.
        let mut replay = entry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let cached = replay.version();
        let newest = newest_after(table_root, cached);

        // A cached version the log no longer has means the table was rebuilt underneath
        // us — dropped and recreated at the same path. Resuming from it would carry
        // files that no longer exist and every query would then read files that are not
        // there, so the entry is discarded and the log read from the beginning.
        let rebuilt = match (cached, newest) {
            (Some(c), Some(n)) => c > n,
            (Some(_), None) => true,
            _ => false,
        };
        if rebuilt {
            *replay = Replay::default();
        }

        let outcome = match (cached, rebuilt) {
            (None, _) | (_, true) => Outcome::Cold,
            (Some(c), false) if Some(c) == newest => Outcome::Current,
            (Some(c), false) => Outcome::Advanced {
                commits_read: usize::try_from(newest.unwrap_or(0).saturating_sub(c))
                    .unwrap_or(0),
            },
        };

        if outcome == Outcome::Current {
            return Ok((replay.live_set(), outcome));
        }

        replay.advance(table_root)?;
        Ok((replay.live_set(), outcome))
    }

    /// Which shard a table's entry lives in, or `None` for a cache with no shards.
    ///
    /// `None` is structurally unreachable --- `default` always builds [`SHARDS`] of them ---
    /// and is returned rather than papered over because the alternatives are worse: indexing
    /// panics in a library, and a fallback shard would silently make every table share one.
    fn shard_for(&self, table_root: &Path) -> Option<&Mutex<HashMap<PathBuf, Arc<Mutex<Replay>>>>> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        table_root.hash(&mut hasher);
        let index = usize::try_from(hasher.finish()).unwrap_or(0) % self.shards.len().max(1);
        self.shards.get(index)
    }

    /// Forget everything.
    ///
    /// Correctness never requires this — the cache cannot go stale. It exists so a
    /// process can release memory for tables it will not query again.
    pub fn clear(&self) {
        for shard in &self.shards {
            shard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
    }

    /// How many tables are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| {
                shard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len()
            })
            .sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
