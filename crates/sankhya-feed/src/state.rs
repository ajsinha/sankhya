//! What a feed is doing, where something other than the feed can see it.
//!
//! # Why a halted feed needs somewhere to live
//!
//! [ADR-0018](https://github.com/ajsinha/sankhya/blob/main/docs/adr/0018-a-record-that-does-not-fit.md)
//! decides that a run of records which do not fit stops the feed and that it **waits for a
//! person**. That makes "halted" a *state*, and a state nobody can query is a state that exists
//! only in whichever log line happened to be printed at the moment it began.
//!
//! An operator arriving an hour later has the same question as one arriving immediately ---
//! *what is running, and what is not* --- and should not have to answer it by grepping.
//!
//! # Why the halt count is kept
//!
//! A feed that halted, was resumed, and halted again for the same reason is a different
//! situation from one that halted once: the first says the source is still wrong, and somebody
//! is resuming a feed rather than fixing it. Keeping the count is what lets that be seen
//! without reading back through a log.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

/// Whether a feed is running.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Health {
    /// Running: it will look at its directory again on the next tick.
    Running,
    /// Stopped, and waiting for somebody.
    Halted {
        /// When, in microseconds since the epoch.
        since: i64,
        /// Why, in the words the stop control used.
        reason: String,
    },
}

impl Health {
    /// The word an operator reads.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Halted { .. } => "halted",
        }
    }
}

/// What one feed has done.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Standing {
    /// The feed's name.
    pub name: String,
    /// Whether it is running.
    pub health: Health,
    /// Runs completed, halted or not.
    pub runs: u64,
    /// Rows published, over the life of this process.
    pub published: u64,
    /// Records quarantined, over the life of this process.
    pub quarantined: u64,
    /// Sources skipped as already read.
    ///
    /// Steady for a spool that keeps its files, and growing when sources are arriving behind
    /// the mark --- which is the only way that event can be noticed at all.
    pub skipped: u64,
    /// How many times this feed has halted since the process started.
    pub halts: u64,
    /// When it last ran, in microseconds since the epoch. Zero before its first run.
    pub last_run: i64,
}

impl Standing {
    /// A feed that has not run yet.
    #[must_use]
    pub fn fresh(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            health: Health::Running,
            runs: 0,
            published: 0,
            quarantined: 0,
            skipped: 0,
            halts: 0,
            last_run: 0,
        }
    }

    /// Whether this feed will be run again without somebody asking.
    #[must_use]
    pub const fn is_halted(&self) -> bool {
        matches!(self.health, Health::Halted { .. })
    }
}

/// Every feed this process runs, and what each is doing.
///
/// Shared between the task that runs feeds and whatever answers a query about them. A plain
/// `Mutex` rather than anything cleverer: it is taken for the length of a map update, once per
/// feed per tick, and the contended case does not exist.
#[derive(Debug, Default)]
pub struct Feeds {
    standing: Mutex<BTreeMap<String, Standing>>,
}

impl Feeds {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a feed, before it has run.
    ///
    /// Declared rather than created on first run, so a feed that has never managed to run is
    /// still visible --- which is the case an operator most needs to see.
    pub fn declare(&self, name: &str) {
        let mut standing = self.standing.lock().unwrap_or_else(PoisonError::into_inner);
        standing
            .entry(name.to_owned())
            .or_insert_with(|| Standing::fresh(name));
    }

    /// Record what a run did.
    pub fn ran(&self, name: &str, published: u64, quarantined: u64, skipped: u64, at: i64) {
        let mut standing = self.standing.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = standing
            .entry(name.to_owned())
            .or_insert_with(|| Standing::fresh(name));
        entry.runs = entry.runs.saturating_add(1);
        entry.published = entry.published.saturating_add(published);
        entry.quarantined = entry.quarantined.saturating_add(quarantined);
        entry.skipped = entry.skipped.saturating_add(skipped);
        entry.last_run = at;
    }

    /// Record that a feed has stopped.
    ///
    /// Idempotent in the state and **not** in the count: a feed already halted for the same
    /// reason is not halting twice, and only a resume-then-halt is a second halt.
    pub fn halted(&self, name: &str, reason: &str, at: i64) {
        let mut standing = self.standing.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = standing
            .entry(name.to_owned())
            .or_insert_with(|| Standing::fresh(name));
        if entry.is_halted() {
            return;
        }
        entry.halts = entry.halts.saturating_add(1);
        entry.health = Health::Halted { since: at, reason: reason.to_owned() };
    }

    /// Set a feed running again.
    ///
    /// `false` when there is no such feed, so a caller can say *"no feed called that"* rather
    /// than reporting success for a name nobody has. Resuming a feed that is already running
    /// is `true` and does nothing: the caller asked for a state and got it.
    pub fn resume(&self, name: &str) -> bool {
        let mut standing = self.standing.lock().unwrap_or_else(PoisonError::into_inner);
        match standing.get_mut(name) {
            None => false,
            Some(entry) => {
                entry.health = Health::Running;
                true
            }
        }
    }

    /// Whether this feed should be run on the next tick.
    #[must_use]
    pub fn should_run(&self, name: &str) -> bool {
        let standing = self.standing.lock().unwrap_or_else(PoisonError::into_inner);
        standing.get(name).is_none_or(|entry| !entry.is_halted())
    }

    /// Every feed, by name.
    #[must_use]
    pub fn all(&self) -> Vec<Standing> {
        let standing = self.standing.lock().unwrap_or_else(PoisonError::into_inner);
        standing.values().cloned().collect()
    }
}
