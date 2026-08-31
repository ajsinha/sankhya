//! The week of disk that buys back a defect discovered late.
//!
//! # What the grace period is actually insuring against
//!
//! `FR-TIER-13`: a detached partition is retained in quarantine for a **mandatory** grace
//! period, during which re-attachment is a simple operation. `DEC-24` prices it: *"it costs a
//! week of disk and buys reversible recovery from a defect discovered late. Against permanent
//! loss of a retained record, this is the cheapest insurance in the system."*
//!
//! The failure it covers is specific and is not one verification can catch. Verification proves
//! the archive matches the source at the moment of the copy. It cannot prove the *policy* was
//! right --- that the range was the one somebody meant, that the tiering key meant what its
//! author thought, that a timezone did not shift a year's boundary. Those are discovered days
//! later by a person, and the only thing that helps then is the partition still being there.
//!
//! # Detach is undone by re-attaching; drop is undone by nothing
//!
//! This is why `FR-TIER-04` separates them and why `Phase::Detached` comes before
//! `Phase::Dropped` with quarantine in between. A detached partition is a catalog change: the
//! files are on disk, unreferenced by the live table, and re-attaching them is metadata. Once
//! dropped, the same recovery is a restore from backup at best and a rehydration from the
//! archive at worst --- both of which are operations somebody schedules rather than performs.
//!
//! # Two things the reaper must refuse
//!
//! **A partition still inside its grace period**, which is the whole mechanism.
//!
//! **A partition the registry no longer claims.** If the archival entry has gone --- a restore
//! that lost it, an entry withdrawn by hand --- then the quarantined copy is the *only* copy,
//! and reaping it on age is the permanent loss the grace period existed to prevent, performed
//! by the machinery meant to prevent it. Age alone is not a sufficient condition, and this is
//! the same shape as the orphan sweeper refusing to reclaim a file a retained snapshot reaches.

use crate::registry::{Range, Registry};
use std::fmt;

/// Microseconds in a day.
const DAY: i64 = 86_400 * 1_000_000;

/// How long a detached partition is kept.
///
/// # Why zero is not expressible
///
/// A grace period of nothing is a purge with no quarantine, which is `FR-TIER-13` not being
/// implemented rather than being configured. Making it unrepresentable costs one constructor
/// and removes the setting somebody reaches for when a disk is full at four in the morning.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grace {
    days: u32,
}

impl Grace {
    /// The default, from `DEC-24`.
    pub const DEFAULT_DAYS: u32 = 7;

    /// A grace period of `days`.
    ///
    /// # Errors
    ///
    /// [`NoGrace`] when `days` is zero.
    pub const fn of(days: u32) -> Result<Self, NoGrace> {
        if days == 0 {
            return Err(NoGrace);
        }
        Ok(Self { days })
    }

    /// How many days.
    #[must_use]
    pub const fn days(&self) -> u32 {
        self.days
    }
}

impl Default for Grace {
    fn default() -> Self {
        Self { days: Self::DEFAULT_DAYS }
    }
}

/// A grace period of zero was asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NoGrace;

impl fmt::Display for NoGrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "a grace period of zero days is a purge with no quarantine, which is the mandatory \
             retention of `FR-TIER-13` not being implemented rather than being configured",
        )
    }
}

/// One detached partition, still on disk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Held {
    /// The purge that detached it.
    pub purge: String,
    /// The table it came from.
    pub table: String,
    /// The range it covers.
    pub range: Range,
    /// Where the detached files are.
    pub storage: String,
    /// When it was detached, in microseconds from the epoch.
    pub detached_at: i64,
    /// How long it is kept.
    pub grace: Grace,
}

impl Held {
    /// When the grace period ends.
    #[must_use]
    pub fn released_at(&self) -> i64 {
        self.detached_at.saturating_add(i64::from(self.grace.days()).saturating_mul(DAY))
    }

    /// Whether re-attachment is still the simple operation `FR-TIER-13` promises.
    #[must_use]
    pub fn within_grace(&self, now: i64) -> bool {
        now < self.released_at()
    }
}

/// Why a partition could not be re-attached.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Nothing by that name is held.
    NotHeld {
        /// What was asked for.
        purge: String,
    },
    /// The grace period has ended and the partition has been reaped.
    GraceEnded {
        /// What was asked for.
        purge: String,
        /// When it ended.
        released_at: i64,
    },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHeld { purge } => write!(
                f,
                "no quarantined partition is held for the purge `{purge}`. If it was reaped, \
                 the archive is the copy, and the way back is a rehydration rather than a \
                 re-attachment"
            ),
            Self::GraceEnded { purge, released_at } => write!(
                f,
                "the grace period for `{purge}` ended at {released_at}. Re-attachment is a \
                 simple operation only while the files are still there; after that the archive \
                 is the copy and the way back is a rehydration"
            ),
        }
    }
}

/// A partition put back.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reattached {
    /// The purge that is being undone.
    pub purge: String,
    /// The table.
    pub table: String,
    /// The range now hot again.
    pub range: Range,
    /// Where the files were.
    pub storage: String,
    /// Whether an archival entry was withdrawn as part of it.
    ///
    /// `false` means the registry did not claim the range in the first place, which is worth
    /// reporting rather than treating as success: it means something already withdrew it, and
    /// the two facts should be reconciled by somebody.
    pub entry_withdrawn: bool,
}

/// Why a quarantined partition was kept rather than reaped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Kept {
    /// Still inside its grace period.
    WithinGrace {
        /// When it ends.
        released_at: i64,
    },
    /// The registry does not claim the range, so this copy is the only one.
    ///
    /// Age alone is not sufficient. Reaping here is the permanent loss the grace period exists
    /// to prevent, performed by the machinery meant to prevent it.
    Unarchived,
}

impl fmt::Display for Kept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WithinGrace { released_at } => {
                write!(f, "its grace period runs to {released_at}")
            }
            Self::Unarchived => f.write_str(
                "the archival registry does not claim this range, so the quarantined copy is \
                 the only one there is --- reaping it would be the loss quarantine exists to \
                 prevent",
            ),
        }
    }
}

/// What a sweep decided.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Reaping {
    /// Partitions whose storage may now be released.
    pub release: Vec<Held>,
    /// Partitions kept, with the reason.
    ///
    /// Keeping one is never an error. It costs disk; releasing one too early costs a retained
    /// record.
    pub kept: Vec<(Held, Kept)>,
}

/// Every detached partition still on disk.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Quarantine {
    held: Vec<Held>,
}

impl Quarantine {
    /// Nothing held.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a detached partition into quarantine.
    pub fn hold(&mut self, held: Held) {
        self.held.push(held);
    }

    /// What is held.
    #[must_use]
    pub fn held(&self) -> &[Held] {
        &self.held
    }

    /// Put a partition back, withdrawing its archival entry in the same call.
    ///
    /// # Why both halves are one call
    ///
    /// Re-attaching without withdrawing leaves a range the registry claims as archived and the
    /// catalog shows attached, which is the disagreement [`crate::unify`] has to serve hot and
    /// flag. Withdrawing without re-attaching leaves the range in neither tier, which is a
    /// coverage gap. Both are recoverable and neither should be reachable by forgetting a step,
    /// so there is no order to get wrong because there are not two calls.
    ///
    /// # Errors
    ///
    /// [`Refused`] when nothing is held under that name, or when the grace period has ended.
    pub fn reattach(
        &mut self,
        registry: &mut Registry,
        purge: &str,
        now: i64,
    ) -> Result<Reattached, Refused> {
        let at = self
            .held
            .iter()
            .position(|held| held.purge == purge)
            .ok_or_else(|| Refused::NotHeld { purge: purge.to_string() })?;

        let Some(held) = self.held.get(at) else {
            return Err(Refused::NotHeld { purge: purge.to_string() });
        };
        if !held.within_grace(now) {
            return Err(Refused::GraceEnded {
                purge: purge.to_string(),
                released_at: held.released_at(),
            });
        }

        let held = self.held.remove(at);
        let entry_withdrawn = registry.withdraw(&held.table, held.range).is_some();
        Ok(Reattached {
            purge: held.purge,
            table: held.table,
            range: held.range,
            storage: held.storage,
            entry_withdrawn,
        })
    }

    /// Release the storage of everything past its grace period and still archived.
    ///
    /// Mutates: what it releases, it stops holding.
    pub fn reap(&mut self, registry: &Registry, now: i64) -> Reaping {
        let mut reaping = Reaping::default();
        let archived = |held: &Held| {
            registry
                .entries()
                .iter()
                .any(|entry| entry.table == held.table && entry.range == held.range)
        };

        let mut still_held = Vec::new();
        for held in std::mem::take(&mut self.held) {
            if held.within_grace(now) {
                reaping
                    .kept
                    .push((held.clone(), Kept::WithinGrace { released_at: held.released_at() }));
                still_held.push(held);
            } else if archived(&held) {
                reaping.release.push(held);
            } else {
                reaping.kept.push((held.clone(), Kept::Unarchived));
                still_held.push(held);
            }
        }
        self.held = still_held;
        reaping
    }
}
