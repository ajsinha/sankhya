//! Scheduled tiering: off by default, stopped before the irreversible half, and watched by a
//! guard that compares a run against the runs before it.
//!
//! # Three gates, and only the third cannot be undone
//!
//! | Gate | Effect | Reversible |
//! |---|---|---|
//! | **Archive** | copy, verify, tag; nothing is removed | fully --- a no-op on the source |
//! | **Purge** | detach; data leaves the live table and stays on disk | trivially --- re-attach |
//! | **Drop** | remove from quarantine | **never** |
//!
//! `FR-TIER-31` states the recommended production configuration and it is worth quoting because
//! it sounds like a compromise and is not: **continuous automatic archive and verification, with
//! purge performed deliberately by a human.** That is the valuable half --- continuous machine
//! proof that the published copy is complete and correct --- at none of the risk. A deployment
//! that never advances past `Archive` still gets most of the benefit.
//!
//! So [`Stage::Archive`] is the default a schedule stops at, and going further is something
//! somebody writes down rather than something that happens by not being configured.
//!
//! # Why a new schedule cannot run live
//!
//! `FR-TIER-28`: scheduled tiering is disabled by default, a new schedule executes in **plan
//! mode first**, and the resulting digest requires human approval before it may run live. The
//! approval is of a digest rather than of a schedule, so what was approved is the plan somebody
//! read --- see [`crate::command`].
//!
//! # What the anomaly guard is really for
//!
//! `FR-TIER-29` says it plainly: comparing a candidate set against the schedule's history is
//! what catches *"a clock error, a timezone defect or a mis-edited policy **before** it archives
//! years of data in one pass"*.
//!
//! Those three failures share a shape. Nothing is broken --- the code is correct, the policy is
//! valid, the schedule fires on time --- and the *number of rows in scope* is wrong by orders of
//! magnitude, because a boundary moved. A correctness check cannot see it; only the size can.
//!
//! **The comparison is against the trailing median rather than the mean**, because the mean is
//! moved by the very outlier being looked for: one enormous run drags the average up and makes
//! the next enormous run look ordinary. A median of the last several runs does not care.

use std::fmt;

/// How far a scheduled run may go.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Stage {
    /// Copy, verify and tag. Nothing leaves the system of record.
    #[default]
    Archive,
    /// Detach. The partition leaves the live table and stays on disk.
    Purge,
    /// Release the quarantined partition. Not undoable.
    Drop,
}

impl Stage {
    /// Every stage, in order of what they cost to be wrong about.
    pub const ALL: [Self; 3] = [Self::Archive, Self::Purge, Self::Drop];

    /// Its name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Purge => "purge",
            Self::Drop => "drop",
        }
    }

    /// Whether reaching this stage removes anything from the system of record.
    #[must_use]
    pub const fn removes_from_source(&self) -> bool {
        matches!(self, Self::Purge | Self::Drop)
    }

    /// Whether reaching this stage can be undone.
    #[must_use]
    pub const fn reversible(&self) -> bool {
        !matches!(self, Self::Drop)
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How much one run, and one day, may move.
///
/// `FR-TIER-30`: limits apply **per run and cumulatively per day**, and on reaching one the job
/// stops cleanly at the limit and reports. Per-run alone is defeated by a schedule that fires
/// hourly; per-day alone lets one run take the whole day's allowance in a single mistake.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlastRadius {
    /// Ranges one run may move.
    pub ranges_per_run: usize,
    /// Ranges all runs together may move in a day.
    pub ranges_per_day: usize,
}

impl Default for BlastRadius {
    fn default() -> Self {
        Self { ranges_per_run: 4, ranges_per_day: 8 }
    }
}

/// What a run is permitted to do after the limits are applied.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Allowed {
    /// How many ranges may move.
    pub ranges: usize,
    /// Which limit stopped it short, if either did.
    pub stopped_by: Option<Limit>,
}

/// Which limit a run reached.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Limit {
    /// The per-run limit.
    Run {
        /// The limit.
        limit: usize,
    },
    /// The cumulative daily limit.
    Day {
        /// The limit.
        limit: usize,
        /// What today's earlier runs already moved.
        already: usize,
    },
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run { limit } => {
                write!(f, "the per-run limit of {limit} range(s); the run stopped cleanly at it")
            }
            Self::Day { limit, already } => write!(
                f,
                "the daily limit of {limit} range(s), of which {already} had already moved \
                 today; the run stopped cleanly at it"
            ),
        }
    }
}

impl BlastRadius {
    /// How many of `wanted` ranges this run may move.
    ///
    /// Stops **cleanly at the limit** rather than refusing the run: a schedule that refuses
    /// outright when it is one range over makes no progress at all, and an operator who has to
    /// raise a limit to get any work done raises it too far.
    #[must_use]
    pub fn allow(&self, wanted: usize, already_today: usize) -> Allowed {
        let by_run = wanted.min(self.ranges_per_run);
        let left_today = self.ranges_per_day.saturating_sub(already_today);
        let ranges = by_run.min(left_today);

        let stopped_by = if ranges == left_today && left_today < by_run {
            Some(Limit::Day { limit: self.ranges_per_day, already: already_today })
        } else if ranges < wanted {
            Some(Limit::Run { limit: self.ranges_per_run })
        } else {
            None
        };

        Allowed { ranges, stopped_by }
    }
}

/// Why a scheduled run halted for a person.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Halted {
    /// The schedule has never been approved in plan mode.
    NotApproved {
        /// Which schedule.
        schedule: String,
    },
    /// A kill switch is set.
    ///
    /// `FR-TIER-32`: a kill switch stops **new** phases and never aborts a job mid-detach, which
    /// is why this is a reason a run did not start rather than a way to interrupt one.
    Killed {
        /// Which switch, so an operator knows what to clear.
        switch: String,
    },
    /// The candidate set is far larger than this schedule's history.
    Anomalous {
        /// Which schedule.
        schedule: String,
        /// How many ranges this run would move.
        candidates: usize,
        /// The median of the trailing history.
        median: usize,
        /// The factor at which re-approval is required.
        factor: usize,
    },
}

impl fmt::Display for Halted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotApproved { schedule } => write!(
                f,
                "the schedule `{schedule}` has not been approved. A new schedule runs in plan \
                 mode first, and what is approved is the digest of a plan somebody read"
            ),
            Self::Killed { switch } => write!(
                f,
                "the kill switch `{switch}` is set, so no new phase was started. A running \
                 detach is never interrupted"
            ),
            Self::Anomalous { schedule, candidates, median, factor } => write!(
                f,
                "`{schedule}` would move {candidates} range(s) against a trailing median of \
                 {median}, past the factor of {factor} at which a person looks again. A clock \
                 error, a timezone defect and a mis-edited policy all look exactly like this: \
                 nothing is broken and the amount in scope is wrong by orders of magnitude"
            ),
        }
    }
}

/// A tiering schedule.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Schedule {
    /// Its name.
    pub name: String,
    /// Whether it is enabled.
    ///
    /// `FR-TIER-28` makes disabled the default, which is why [`Schedule::new`] produces one.
    pub enabled: bool,
    /// The digest a person approved, if one has been.
    pub approved: Option<crate::verify::Hash>,
    /// How far it may go.
    pub stage: Stage,
    /// How much it may move.
    pub blast_radius: BlastRadius,
    /// How many ranges each previous run moved, oldest first.
    pub history: Vec<usize>,
    /// The factor above the trailing median at which a person is asked again.
    pub anomaly_factor: usize,
}

impl Schedule {
    /// The default factor above the trailing median.
    pub const DEFAULT_ANOMALY_FACTOR: usize = 3;

    /// A new schedule: disabled, unapproved, and stopping at [`Stage::Archive`].
    ///
    /// Every one of those is `FR-TIER-28` or `FR-TIER-31`, and each is the default because the
    /// safe configuration should be what somebody gets by not deciding.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: false,
            approved: None,
            stage: Stage::Archive,
            blast_radius: BlastRadius::default(),
            history: Vec::new(),
            anomaly_factor: Self::DEFAULT_ANOMALY_FACTOR,
        }
    }

    /// The median of the trailing history, or `None` when there is not one yet.
    ///
    /// The median rather than the mean, because the mean is moved by the outlier being looked
    /// for: one enormous run drags the average up and makes the next enormous run ordinary.
    #[must_use]
    pub fn trailing_median(&self) -> Option<usize> {
        if self.history.is_empty() {
            return None;
        }
        let mut sorted = self.history.clone();
        sorted.sort_unstable();
        let middle = sorted.len() / 2;
        if sorted.len() % 2 == 1 {
            sorted.get(middle).copied()
        } else {
            let (low, high) = (sorted.get(middle - 1)?, sorted.get(middle)?);
            Some((low + high) / 2)
        }
    }

    /// Whether a run of `candidates` ranges may proceed, and how much of it.
    ///
    /// # Errors
    ///
    /// [`Halted`] when the schedule is unapproved, a kill switch is set, or the candidate set is
    /// anomalous against the trailing history.
    pub fn admit(
        &self,
        candidates: usize,
        already_today: usize,
        kill_switch: Option<&str>,
    ) -> Result<Allowed, Halted> {
        if let Some(switch) = kill_switch {
            return Err(Halted::Killed { switch: switch.to_string() });
        }
        if self.approved.is_none() || !self.enabled {
            return Err(Halted::NotApproved { schedule: self.name.clone() });
        }
        // No history is not a reason to halt. A schedule's first live run has nothing to be
        // compared against, and refusing it would mean no schedule could ever have a second run
        // --- the approval of the plan digest is what stands in for the comparison there.
        if let Some(median) = self.trailing_median() {
            if median > 0 && candidates > median.saturating_mul(self.anomaly_factor) {
                return Err(Halted::Anomalous {
                    schedule: self.name.clone(),
                    candidates,
                    median,
                    factor: self.anomaly_factor,
                });
            }
        }
        Ok(self.blast_radius.allow(candidates, already_today))
    }
}
