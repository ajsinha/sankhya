//! The escalation ladder, and the one ordering rule behind it.
//!
//! # Why the signals go through a bus
//!
//! Each subsystem knows something about how stressed it is, and the tempting design is
//! to let each one react to what it knows. That produces a system whose behaviour under
//! load is the sum of several independent local decisions, which is untestable and
//! usually contradictory — the applier lengthening its commit interval to reduce load
//! while the compactor shortens its duty cycle to keep up with the files that used to
//! arrive.
//!
//! So the signals are typed values on a bus, and the escalation is evaluated in **one
//! place** from all of them together. That makes the behaviour under any combination of
//! pressures a pure function, which means it can be tested rather than observed in
//! production.
//!
//! # The ordering rule
//!
//! **The source outranks the analytical tier, the analytical tier outranks maintenance,
//! and maintenance outranks nothing — except when it is defending the source.**
//!
//! Everything below follows from that. Queries are sacrificed to protect the source, but
//! not to protect maintenance; maintenance is deferred to protect queries, but preempts
//! them when it is reclaiming log space the source needs.
//!
//! # Why the thresholds must fire before the database's own
//!
//! The last rung sacrifices analytical freshness to save the source, and it exists
//! because the alternative is worse. If the retained log grows until the database
//! invalidates the replication slot, the slot cannot be resumed: every replicated table
//! needs a complete re-snapshot, which on a large warehouse is hours to days of
//! unavailability.
//!
//! Advancing the slot deliberately, recording a gap, and re-snapshotting only the
//! affected tables is a bad outcome chosen on purpose. Having the database choose it for
//! us is the same bad outcome, unbounded and unannounced. **The whole ladder is arranged
//! so that SANKHYA degrades on its own terms first.**

use std::fmt;

/// What some part of the system is reporting.
///
/// Fractions of the limit rather than absolute values, so the ladder does not need to
/// know each subsystem's configuration to compare them.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Signals {
    /// Retained write-ahead log, as a fraction of what the source will tolerate.
    ///
    /// The most dangerous signal, because passing 1.0 is not a degradation but an
    /// unrecoverable state: the slot is invalidated and cannot be resumed.
    pub retained_log: f64,
    /// Transaction-identifier freeze age, as a fraction of the wraparound limit.
    ///
    /// The other unrecoverable one. Passing it stops the database.
    pub freeze_age: f64,
    /// Arrival tier occupancy, as a fraction of its hard limit.
    pub arrival_buffer: f64,
    /// Capture lag, as a fraction of the tolerated bound.
    pub lag: f64,
    /// Compaction backlog, as a fraction of the point at which queries suffer.
    pub compaction_debt: f64,
}

impl Signals {
    /// The highest of the two signals that are unrecoverable when exceeded.
    #[must_use]
    pub fn source_risk(&self) -> f64 {
        self.retained_log.max(self.freeze_age)
    }
}

/// How stressed the system is, and therefore what it will refuse to do.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Level {
    /// Full admission; maintenance at normal duty.
    #[default]
    Normal,
    /// Defer optional maintenance; increase batch size.
    Watch,
    /// Reduce admission; suspend re-clustering; lengthen the commit interval.
    ///
    /// Freshness is still served, because the arrival tier holds what has not yet been
    /// published — which is what makes lengthening the commit interval a safe first
    /// lever rather than a visible one.
    Constrain,
    /// Stop admitting; existing queries run to their deadline; everything to the applier.
    Protect,
    /// Sacrifice analytical freshness to save the source.
    ///
    /// The slot is advanced with a recorded gap, affected tables are marked for
    /// re-snapshot, and the gap is reported in provenance until it is closed.
    Sacrifice,
}

impl Level {
    /// Whether new queries are admitted at this level.
    #[must_use]
    pub const fn admits_queries(self) -> bool {
        matches!(self, Self::Normal | Self::Watch | Self::Constrain)
    }

    /// Whether maintenance beyond what defends the source may run.
    #[must_use]
    pub const fn runs_optional_maintenance(self) -> bool {
        matches!(self, Self::Normal)
    }

    /// Whether reaching this level loses data that must be re-read from the source.
    ///
    /// True only at the last rung. Naming it here rather than leaving it implicit,
    /// because it is the difference between a degradation and an incident.
    #[must_use]
    pub const fn loses_continuity(self) -> bool {
        matches!(self, Self::Sacrifice)
    }

    /// What this level means for admission.
    ///
    /// The link between the two halves of this crate, and the reason they live together.
    /// Keeping them apart would leave the ladder's most consequential effect — that the
    /// system stops serving — as a convention each caller had to remember rather than a
    /// value it is handed.
    #[must_use]
    pub const fn posture(self) -> crate::admission::Posture {
        if self.admits_queries() {
            crate::admission::Posture::Admitting
        } else {
            crate::admission::Posture::Shedding
        }
    }

    /// Whether an operator should be paged.
    #[must_use]
    pub const fn pages(self) -> bool {
        matches!(self, Self::Protect | Self::Sacrifice)
    }
}

impl fmt::Display for Level {
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

/// Where each rung sits.
///
/// The source thresholds are deliberately below 1.0: the ladder must act while there is
/// still room, because at 1.0 the database has already acted and its action cannot be
/// undone.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Thresholds {
    pub watch: f64,
    pub constrain: f64,
    pub protect: f64,
    pub sacrifice: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            watch: 0.5,
            constrain: 0.7,
            protect: 0.85,
            // Not 1.0. At 1.0 the source has already invalidated the slot, and the
            // choice this rung exists to make deliberately has been made for us,
            // unbounded and unannounced.
            sacrifice: 0.95,
        }
    }
}

/// The evaluated level, with what drove it.
#[derive(Clone, PartialEq, Debug)]
pub struct Assessment {
    pub level: Level,
    /// The signal that put the system here, named so an operator does not have to guess.
    pub driver: &'static str,
    pub value: f64,
}

/// Evaluate the ladder.
///
/// Every signal is considered and the **highest** resulting level wins, because the
/// levels describe what the system will refuse to do and a refusal justified by any one
/// pressure is justified.
#[must_use]
pub fn assess(signals: &Signals, thresholds: &Thresholds) -> Assessment {
    // Source risk uses the full ladder, including the last rung. Nothing else does:
    // sacrificing continuity is only ever worth it to save the source, so no amount of
    // compaction debt or query lag can reach that rung.
    let mut worst = Assessment {
        level: Level::Normal,
        driver: "none",
        value: 0.0,
    };

    let mut consider = |level: Level, driver: &'static str, value: f64| {
        if level > worst.level {
            worst = Assessment {
                level,
                driver,
                value,
            };
        }
    };

    let retained = rung(signals.retained_log, thresholds, true);
    consider(retained, "retained log", signals.retained_log);

    let freeze = rung(signals.freeze_age, thresholds, true);
    consider(freeze, "freeze age", signals.freeze_age);

    // The arrival tier filling is serious — publication has stalled — but it is a
    // memory problem, not a continuity one, and it is capped below the last rung.
    let buffer = rung(signals.arrival_buffer, thresholds, false);
    consider(buffer, "arrival buffer", signals.arrival_buffer);

    let lag = rung(signals.lag, thresholds, false).min(Level::Constrain);
    consider(lag, "capture lag", signals.lag);

    // Compaction debt makes queries slower. It never justifies refusing them.
    let debt = rung(signals.compaction_debt, thresholds, false).min(Level::Watch);
    consider(debt, "compaction debt", signals.compaction_debt);

    worst
}

/// Which rung a single fraction reaches.
///
/// `may_sacrifice` gates the last rung, which is reserved for the two signals whose
/// limits are unrecoverable.
fn rung(value: f64, thresholds: &Thresholds, may_sacrifice: bool) -> Level {
    if may_sacrifice && value >= thresholds.sacrifice {
        return Level::Sacrifice;
    }
    if value >= thresholds.protect {
        return Level::Protect;
    }
    if value >= thresholds.constrain {
        return Level::Constrain;
    }
    if value >= thresholds.watch {
        return Level::Watch;
    }
    Level::Normal
}
