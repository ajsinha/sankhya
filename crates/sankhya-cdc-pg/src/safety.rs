//! The escalation ladder.

use crate::slot::{SlotState, WalStatus};
use std::fmt;
use std::time::Duration;

/// How serious a situation is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    /// Normal.
    Normal,
    /// Worth noticing. Optional work is deferred.
    Watch,
    /// Analytical throughput is reduced to let capture catch up.
    Constrain,
    /// New analytical work is refused entirely. Someone is paged.
    Protect,
    /// The analytical tier is sacrificed to save the source.
    Sacrifice,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Normal => "normal",
            Self::Watch => "watch",
            Self::Constrain => "constrain",
            Self::Protect => "protect",
            Self::Sacrifice => "sacrifice",
        })
    }
}

impl Severity {
    /// Whether an operator should be woken.
    #[must_use]
    pub const fn pages(self) -> bool {
        matches!(self, Self::Protect | Self::Sacrifice)
    }

    /// Whether new analytical work may still be admitted.
    #[must_use]
    pub const fn admits_queries(self) -> bool {
        matches!(self, Self::Normal | Self::Watch | Self::Constrain)
    }
}

/// What to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Escalation {
    pub severity: Severity,
    /// What is happening, in an operator's terms.
    pub reason: String,
    /// Defer optional maintenance to free resources for capture.
    pub defer_maintenance: bool,
    /// Reduce analytical concurrency.
    pub shed_queries: bool,
    /// Refuse new analytical work entirely.
    pub refuse_queries: bool,
    /// Abandon the unread log, record a gap, and begin re-snapshotting.
    ///
    /// Deliberate and irreversible. It is chosen only because the alternative — letting
    /// the source run out of storage — is worse.
    pub sacrifice_analytics: bool,
}

impl Escalation {
    fn normal() -> Self {
        Self {
            severity: Severity::Normal,
            reason: "capture is keeping up".into(),
            defer_maintenance: false,
            shed_queries: false,
            refuse_queries: false,
            sacrifice_analytics: false,
        }
    }
}

/// Thresholds, expressed as fractions of the source's own retention limit.
///
/// Fractions rather than absolutes, because the point is to act *before* the source
/// does, and the source's limit is the only reference that matters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SafetyPolicy {
    /// The source's retention limit, in bytes.
    pub retention_limit_bytes: u64,
    /// Fraction at which to defer optional work.
    pub watch_fraction_percent: u32,
    /// Fraction at which to reduce analytical concurrency.
    pub constrain_fraction_percent: u32,
    /// Fraction at which to refuse analytical work.
    pub protect_fraction_percent: u32,
    /// Fraction at which to sacrifice the analytical tier.
    ///
    /// **Must be below 100.** At 100 the source acts first, and the slot is destroyed
    /// rather than deliberately abandoned.
    pub sacrifice_fraction_percent: u32,
    /// Lag at which to begin deferring work, independent of byte volume.
    pub watch_lag: Duration,
    /// Lag at which to reduce analytical concurrency.
    pub constrain_lag: Duration,
}

impl Default for SafetyPolicy {
    fn default() -> Self {
        Self {
            retention_limit_bytes: 8 * 1024 * 1024 * 1024,
            watch_fraction_percent: 25,
            constrain_fraction_percent: 40,
            protect_fraction_percent: 60,
            // Deliberately well below the source's own limit. The margin is the time
            // available to abandon the log on our terms rather than have it removed.
            sacrifice_fraction_percent: 85,
            watch_lag: Duration::from_secs(60),
            constrain_lag: Duration::from_secs(300),
        }
    }
}

impl SafetyPolicy {
    /// Whether the thresholds are ordered such that SANKHYA always acts first.
    ///
    /// A policy that fails this cannot protect the source: the database would remove
    /// the log before SANKHYA ever escalated, and recovery would be a full re-snapshot
    /// rather than a deliberate, recorded gap.
    #[must_use]
    pub const fn is_well_ordered(&self) -> bool {
        self.watch_fraction_percent < self.constrain_fraction_percent
            && self.constrain_fraction_percent < self.protect_fraction_percent
            && self.protect_fraction_percent < self.sacrifice_fraction_percent
            && self.sacrifice_fraction_percent < 100
    }

    fn threshold(&self, percent: u32) -> u64 {
        self.retention_limit_bytes
            .saturating_mul(u64::from(percent))
            .saturating_div(100)
    }
}

/// Decide what to do about a slot.
///
/// Pure: it takes the observed state rather than querying for it, so every rung of the
/// ladder is testable without a database and without waiting for a real stall.
#[must_use]
pub fn assess(policy: &SafetyPolicy, state: &SlotState, lag: Duration) -> Escalation {
    // An already-lost slot is past every threshold. Nothing can be salvaged from it,
    // and the only correct action is to record the gap and rebuild.
    if !state.status.is_usable() {
        return Escalation {
            severity: Severity::Sacrifice,
            reason: format!(
                "slot {} is lost: the source removed the log it was holding. \
                 It cannot be resumed; affected tables must be re-snapshotted",
                state.name
            ),
            defer_maintenance: true,
            shed_queries: true,
            refuse_queries: true,
            sacrifice_analytics: true,
        };
    }

    let retained = state.retained_bytes;

    if retained >= policy.threshold(policy.sacrifice_fraction_percent)
        || state.status == WalStatus::Unreserved
    {
        return Escalation {
            severity: Severity::Sacrifice,
            reason: format!(
                "retained log has reached {retained} bytes ({}), which is close enough \
                 to the source's limit that waiting risks the source removing it first. \
                 Abandoning the unread log deliberately, recording a gap, and \
                 re-snapshotting is better than losing the slot",
                state.status
            ),
            defer_maintenance: true,
            shed_queries: true,
            refuse_queries: true,
            sacrifice_analytics: true,
        };
    }

    if retained >= policy.threshold(policy.protect_fraction_percent)
        || state.status == WalStatus::Extended
    {
        return Escalation {
            severity: Severity::Protect,
            reason: format!(
                "retained log has reached {retained} bytes ({}); refusing new \
                 analytical work so capture can drain",
                state.status
            ),
            defer_maintenance: true,
            shed_queries: true,
            refuse_queries: true,
            sacrifice_analytics: false,
        };
    }

    if retained >= policy.threshold(policy.constrain_fraction_percent) || lag >= policy.constrain_lag
    {
        return Escalation {
            severity: Severity::Constrain,
            reason: format!(
                "capture is {lag:?} behind and holding {retained} bytes; reducing \
                 analytical concurrency"
            ),
            defer_maintenance: true,
            shed_queries: true,
            refuse_queries: false,
            sacrifice_analytics: false,
        };
    }

    if retained >= policy.threshold(policy.watch_fraction_percent) || lag >= policy.watch_lag {
        return Escalation {
            severity: Severity::Watch,
            reason: format!(
                "capture is {lag:?} behind and holding {retained} bytes; deferring \
                 optional maintenance"
            ),
            defer_maintenance: true,
            shed_queries: false,
            refuse_queries: false,
            sacrifice_analytics: false,
        };
    }

    Escalation::normal()
}
