//! Arbitrating maintenance work against the machine budget.
//!
//! # One scheduler, both sides
//!
//! The transactional and analytical sides share a machine, so they must share a
//! scheduler. A freeze emergency on the source and a compaction backlog in the
//! warehouse draw from the same budget, and two independent schedulers cannot arbitrate
//! between them — each would see only its own queue and conclude, correctly and
//! uselessly, that its work is the most important thing running.
//!
//! # Why the classes are strict rather than weighted
//!
//! A weighted scheduler expresses "compaction matters somewhat less than freeze
//! remediation". That is the wrong statement. A transaction-identifier wraparound stops
//! the database; a compaction backlog makes queries slower. There is no weighting under
//! which the second should ever go first, and a scheduler able to express one will
//! eventually pick it.
//!
//! So the classes are strictly ordered and the top two may preempt queries outright. A
//! slow query is a worse outcome than a fast one and a better outcome than a stopped
//! database.
//!
//! # Starvation is reported, not fixed
//!
//! Strict priority starves the bottom of the queue, and the usual remedy is to age
//! deferred work upward. This scheduler deliberately does not: ageing a compaction above
//! a freeze emergency is precisely the decision the class ordering exists to prevent.
//!
//! Instead, deferral is counted and reported. A system where compaction has been
//! deferred for a week has a capacity problem that promoting one job would hide rather
//! than solve.
//!
//! # What this scheduler cannot do
//!
//! **It cannot destroy retained history.** Erasure is not the bottom of this ladder; it
//! is a different job class with a different authorization path, and it is not
//! representable in [`Class`] at all. That is a structural guarantee rather than a
//! configuration choice, because the failure it prevents — a misconfigured retention
//! default quietly deleting records that were legally required to persist — is
//! unrecoverable and silent.

use std::fmt;

/// What kind of work this is, and therefore what it may cost.
///
/// # Ordering
///
/// Declaration order is the scheduling order, and [`Ord`] is derived from it. Adding a
/// variant changes the priority ladder, which is the intended way to change it and
/// should be a deliberate act.
///
/// # What is deliberately absent
///
/// There is no erasure class, and there must never be one. See the module
/// documentation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Class {
    /// Transaction-identifier freeze, slot-lag remediation. May preempt queries.
    Safety,
    /// Log and disk reclamation, emergency compaction. May preempt queries, audited.
    Availability,
    /// Compaction, delete merging, statistics. Within the duty cycle.
    Performance,
    /// Expiry, orphan cleanup, metadata maintenance, tiering. Within the duty cycle.
    Housekeeping,
    /// Re-clustering, cold view refresh. Windows only, and first to be deferred.
    Optional,
}

impl Class {
    /// Whether work of this class may take capacity from running queries.
    ///
    /// True for exactly the two classes whose failure is worse than a slow query. This
    /// is the one place the distinction is made, so it can be read in one line.
    #[must_use]
    pub const fn may_preempt_queries(self) -> bool {
        matches!(self, Self::Safety | Self::Availability)
    }

    /// Whether the duty cycle bounds this class.
    ///
    /// The top two classes are unbounded, because a budget that can stop freeze
    /// remediation is a budget that can stop the database.
    #[must_use]
    pub const fn bounded_by_duty_cycle(self) -> bool {
        !self.may_preempt_queries()
    }

    /// Whether this class runs only inside a maintenance window.
    #[must_use]
    pub const fn windows_only(self) -> bool {
        matches!(self, Self::Optional)
    }

    /// Whether an audit record is required when this runs.
    #[must_use]
    pub const fn audited(self) -> bool {
        matches!(self, Self::Availability)
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Safety => "safety",
            Self::Availability => "availability",
            Self::Performance => "performance",
            Self::Housekeeping => "housekeeping",
            Self::Optional => "optional",
        };
        f.write_str(s)
    }
}

/// A unit of maintenance work awaiting a decision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Job {
    pub name: String,
    pub class: Class,
    /// Expected cost, in whatever tick the caller counts budget in.
    pub estimated_ticks: u64,
    /// Whether the job checkpoints and resumes.
    ///
    /// A job that can only run to completion will never complete on a busy system, so
    /// one is refused rather than started when it cannot fit in the remaining budget.
    /// Starting it would burn the whole duty cycle and finish nothing.
    pub resumable: bool,
    /// How long this has already waited.
    ///
    /// Carried so deferral is visible. It does **not** promote the job — see the module
    /// documentation on why ageing is the wrong remedy here.
    pub ticks_deferred: u64,
    /// Ticks until this becomes user-visible, if it ever does.
    ///
    /// This is what orders jobs within a class. "Compaction debt is large" is far less
    /// actionable than "at the current write rate, latency on this table doubles in
    /// about nine days", and it is also the better scheduling signal: the job about to
    /// hurt someone should go first regardless of how large its backlog is.
    pub ticks_to_visible: Option<u64>,
}

/// What the machine looks like right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SystemState {
    pub in_maintenance_window: bool,
    /// Queries currently executing. Only relevant to whether preemption is *visible*;
    /// it never changes whether a class is permitted to preempt.
    pub queries_running: usize,
    /// Budget left in this duty cycle, for classes the duty cycle bounds.
    pub duty_cycle_ticks_remaining: u64,
}

/// Why a job was not run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Deferral {
    /// The duty cycle is exhausted.
    BudgetExhausted { remaining: u64, needed: u64 },
    /// The job cannot checkpoint and does not fit in what is left.
    ///
    /// Distinct from budget exhaustion because the remedy differs: this job will never
    /// run on a system this busy until it is made resumable.
    WouldNotFinish { remaining: u64, needed: u64 },
    /// Optional work outside a maintenance window.
    OutsideWindow,
}

impl fmt::Display for Deferral {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BudgetExhausted { remaining, needed } => write!(
                f,
                "the duty cycle has {remaining} ticks left and this needs {needed}"
            ),
            Self::WouldNotFinish { remaining, needed } => write!(
                f,
                "this job cannot checkpoint, needs {needed} ticks and has {remaining}; \
                 starting it would spend the remaining budget and finish nothing"
            ),
            Self::OutsideWindow => {
                f.write_str("optional work runs only inside a maintenance window")
            }
        }
    }
}

/// A job to run, and what running it costs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Scheduled {
    pub job: Job,
    /// Whether running this will take capacity from queries in flight.
    pub preempts_queries: bool,
    /// Whether an audit record must be written.
    pub audited: bool,
}

/// The decision for one cycle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Schedule {
    /// In the order they should run.
    pub run: Vec<Scheduled>,
    /// Everything not run, with the reason.
    pub deferred: Vec<(Job, Deferral)>,
}

impl Schedule {
    #[must_use]
    pub fn preempts_queries(&self) -> bool {
        self.run.iter().any(|s| s.preempts_queries)
    }

    /// The longest a deferred job has been waiting.
    ///
    /// The signal that a system is short of capacity rather than merely busy.
    #[must_use]
    pub fn longest_deferral(&self) -> Option<(&Job, u64)> {
        self.deferred
            .iter()
            .max_by_key(|(job, _)| job.ticks_deferred)
            .map(|(job, _)| (job, job.ticks_deferred))
    }

    /// The soonest any deferred job becomes user-visible.
    ///
    /// Reported so an operator learns that something will hurt before it does, rather
    /// than afterwards.
    #[must_use]
    pub fn soonest_visible_deferral(&self) -> Option<(&Job, u64)> {
        self.deferred
            .iter()
            .filter_map(|(job, _)| job.ticks_to_visible.map(|t| (job, t)))
            .min_by_key(|(_, t)| *t)
    }
}

/// Decide what runs this cycle.
///
/// Jobs are considered in class order, and within a class by how soon they become
/// user-visible, then by how long they have waited. A job never changes class because
/// it has waited — see the module documentation.
#[must_use]
pub fn schedule(jobs: &[Job], state: &SystemState) -> Schedule {
    let mut ordered: Vec<&Job> = jobs.iter().collect();
    ordered.sort_by(|a, b| {
        a.class
            .cmp(&b.class)
            // A job with no visible consequence sorts after every job that has one.
            .then_with(|| {
                a.ticks_to_visible
                    .unwrap_or(u64::MAX)
                    .cmp(&b.ticks_to_visible.unwrap_or(u64::MAX))
            })
            .then_with(|| b.ticks_deferred.cmp(&a.ticks_deferred))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut run = Vec::new();
    let mut deferred = Vec::new();
    let mut budget = state.duty_cycle_ticks_remaining;

    for job in ordered {
        if job.class.windows_only() && !state.in_maintenance_window {
            deferred.push((job.clone(), Deferral::OutsideWindow));
            continue;
        }

        if job.class.bounded_by_duty_cycle() {
            if budget == 0 || job.estimated_ticks > budget {
                let reason = if job.resumable {
                    Deferral::BudgetExhausted {
                        remaining: budget,
                        needed: job.estimated_ticks,
                    }
                } else {
                    Deferral::WouldNotFinish {
                        remaining: budget,
                        needed: job.estimated_ticks,
                    }
                };
                deferred.push((job.clone(), reason));
                continue;
            }
            budget = budget.saturating_sub(job.estimated_ticks);
        }

        run.push(Scheduled {
            job: job.clone(),
            preempts_queries: job.class.may_preempt_queries() && state.queries_running > 0,
            audited: job.class.audited(),
        });
    }

    Schedule { run, deferred }
}
