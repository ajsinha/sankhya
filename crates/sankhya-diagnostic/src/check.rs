//! What the diagnostic looks at, and how a finding is phrased.
//!
//! # Every finding carries a remediation
//!
//! `FR-OPS-16` requires it, and the reason is that a diagnostic without one converts an
//! operator's problem into a support ticket. "Compaction debt is high" is a fact; "run
//! `sankhya maintenance compact --table sales.orders`, or raise the duty cycle if this
//! recurs" is a thing to do.
//!
//! # Findings are ordered by *when*, not by *how bad*
//!
//! Severity orders a list by how loudly each item shouts. Time orders it by which one has to
//! be dealt with first, and those are different orders --- a warning that becomes an outage
//! tomorrow outranks an error that has been stable for a month.
//!
//! So the report sorts by projection, and a finding with no date sorts last however alarming
//! its value is. That is deliberate: an operator reading top-down should be reading a
//! schedule.

use crate::projection::{Concern, Confidence, Projection, Trend};
use std::fmt;

/// How much attention a finding deserves if nothing changes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    /// Worth knowing, not worth acting on.
    Note,
    /// Will become a problem.
    Warning,
    /// Is a problem now.
    Critical,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Note => "note",
            Self::Warning => "warning",
            Self::Critical => "critical",
        })
    }
}

/// One thing the diagnostic found.
#[derive(Clone, PartialEq, Debug)]
pub struct Finding {
    /// Which check produced it.
    pub check: &'static str,
    /// What it is about, when a check covers several things.
    pub subject: String,
    /// How much attention it deserves.
    pub severity: Severity,
    /// What was measured, in the units the operator thinks in.
    pub observed: String,
    /// When it becomes user-visible.
    pub projection: Projection,
    /// What to do about it.
    pub remediation: String,
}

impl Finding {
    /// The line an operator reads.
    #[must_use]
    pub fn describe(&self) -> String {
        // A semicolon rather than a full stop, because the projection continues the
        // sentence in lower case: "900 live files. at the current rate" reads as a typo.
        format!(
            "[{}] {} — {}; {}.\n         {}",
            self.severity,
            self.subject,
            self.observed,
            self.projection.describe(),
            self.remediation
        )
    }

    /// How urgent this is, for ordering. Smaller sorts first.
    ///
    /// Already-crossed first, then by how soon, then everything with no date. A finding
    /// with no projection sorts last however alarming its value, because an operator
    /// reading top-down should be reading a schedule rather than a list of adjectives.
    #[must_use]
    pub fn urgency(&self) -> (u8, i64) {
        match &self.projection {
            Projection::Already => (0, 0),
            Projection::Crossing { seconds, .. } => (1, *seconds),
            Projection::Receding => (2, 0),
            Projection::Beyond { .. } => (2, 1),
            Projection::Unknown { .. } => (3, 0),
        }
    }
}

/// Everything the diagnostic found, in the order to deal with it.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Report {
    findings: Vec<Finding>,
    /// Checks that ran and found nothing, so silence can be told from absence.
    clean: Vec<&'static str>,
    /// Checks that could not run, and why.
    ///
    /// Separate from a clean result, because "storage is reachable" and "I could not reach
    /// storage to find out" are opposite facts and both produce an empty finding list.
    skipped: Vec<(&'static str, String)>,
}

impl Report {
    /// An empty report.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a finding.
    pub fn found(&mut self, finding: Finding) {
        self.findings.push(finding);
        self.findings.sort_by_key(Finding::urgency);
    }

    /// Record that a check ran and found nothing.
    pub fn clean(&mut self, check: &'static str) {
        self.clean.push(check);
    }

    /// Record that a check could not run.
    pub fn skipped(&mut self, check: &'static str, why: impl Into<String>) {
        self.skipped.push((check, why.into()));
    }

    /// Everything found, most urgent first.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Checks that could not run.
    #[must_use]
    pub fn could_not_run(&self) -> &[(&'static str, String)] {
        &self.skipped
    }

    /// Whether anything needs attention now or soon.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Whether anything is a problem *now*.
    #[must_use]
    pub fn has_critical(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.severity == Severity::Critical)
    }

    /// A summary line.
    #[must_use]
    pub fn summary(&self) -> String {
        let dated = self
            .findings
            .iter()
            .filter(|f| f.projection.is_actionable())
            .count();
        format!(
            "{} check(s) clean, {} finding(s) of which {dated} have a date, {} check(s) \
             could not run",
            self.clean.len(),
            self.findings.len(),
            self.skipped.len()
        )
    }
}

/// The size at which a table's file count starts costing query latency.
///
/// Not a universal truth --- it depends on the query mix and the storage. It is here as a
/// named default rather than a literal buried in a comparison, so that an operator who
/// disagrees can find the number and change it.
pub const FILES_BEFORE_LATENCY_SUFFERS: f64 = 1_000.0;

/// Free space below which the diagnostic speaks up even with no rate to project from.
///
/// Absolute rather than a percentage, because the threshold it guards is zero and nothing is
/// a meaningful fraction of the way to zero.
pub const HEADROOM_WORTH_MENTIONING: f64 = 10.0 * 1024.0 * 1024.0 * 1024.0;

/// How much of the way to a threshold counts as near enough to mention undated.
const NEAR_ENOUGH_TO_MENTION: f64 = 0.80;

/// Whether an undated measure is close enough to the line to be worth saying so.
///
/// The first run of a diagnostic has one observation and therefore no rate, so every
/// projection is `Unknown` and every check would return nothing. A table sitting at 990 of
/// 1,000 files would produce silence, and silence from a diagnostic reads as health.
///
/// So nearness is reported without a date, at `Note` --- the measure may be perfectly
/// stable, and claiming otherwise from one sample is the invention this crate exists to
/// avoid. What it says is "you are near the line and I cannot yet tell you whether you are
/// moving", which is the whole truth available.
fn worth_mentioning_undated(
    latest: f64,
    watch_at: f64,
    concern: Concern,
    projection: &Projection,
) -> bool {
    if !matches!(projection, Projection::Unknown { .. }) {
        return false;
    }
    match concern {
        Concern::RisingTo => latest >= watch_at,
        Concern::FallingTo => latest <= watch_at,
    }
}

/// Compaction debt, expressed as when it starts costing query latency.
///
/// `FR-OPS-17`'s own example. The measure is the live file count, because that is what a
/// scan pays for: bytes are compacted away and file count is what pruning and opening cost.
#[must_use]
pub fn compaction_debt(table: &str, files: &Trend, now: i64) -> Option<Finding> {
    let latest = files.latest()?;
    let projection = files.time_until(FILES_BEFORE_LATENCY_SUFFERS, Concern::RisingTo, now);

    let near = worth_mentioning_undated(
        latest.value,
        FILES_BEFORE_LATENCY_SUFFERS * NEAR_ENOUGH_TO_MENTION,
        Concern::RisingTo,
        &projection,
    );
    // Receding, or too far off to matter and not yet measurable.
    if !projection.is_actionable() && !near {
        return None;
    }
    let severity = match projection {
        Projection::Already => Severity::Critical,
        Projection::Crossing { .. } => Severity::Warning,
        _ => Severity::Note,
    };

    Some(Finding {
        check: "compaction-debt",
        subject: format!("table {table}"),
        severity,
        observed: format!("{} live files", latest.value as i64),
        projection,
        remediation: format!(
            "Compact it: `sankhya maintenance compact --table {table}`. If this recurs, the \
             maintenance duty cycle is too low for this table's write rate — raising it is \
             the durable fix and compacting by hand is not."
        ),
    })
}

/// Storage headroom, expressed as when it runs out.
#[must_use]
pub fn storage_headroom(free_bytes: &Trend, now: i64) -> Option<Finding> {
    let latest = free_bytes.latest()?;
    // The threshold is zero: the question is when free space runs out, not when it passes an
    // arbitrary percentage. A percentage would need a total, which is a second measurement
    // that can be stale independently.
    let projection = free_bytes.time_until(0.0, Concern::FallingTo, now);
    let near = worth_mentioning_undated(
        latest.value,
        HEADROOM_WORTH_MENTIONING,
        Concern::FallingTo,
        &projection,
    );
    if !projection.is_actionable() && !near {
        return None;
    }

    Some(Finding {
        check: "storage-headroom",
        subject: "the data directory".to_string(),
        severity: match projection {
            Projection::Already => Severity::Critical,
            Projection::Crossing { .. } => Severity::Warning,
            _ => Severity::Note,
        },
        observed: format!("{} free", human_bytes(latest.value)),
        projection,
        remediation: "Add capacity, shorten retention, or let expiry reclaim superseded \
                      files. A full data directory stops the applier before it stops \
                      queries, so the first symptom is replication lag rather than a disk \
                      error."
            .to_string(),
    })
}

/// Replication lag, expressed as when it breaches the freshness objective.
#[must_use]
pub fn replication_lag(
    lag_seconds: &Trend,
    objective_seconds: f64,
    now: i64,
) -> Option<Finding> {
    let latest = lag_seconds.latest()?;
    let projection = lag_seconds.time_until(objective_seconds, Concern::RisingTo, now);
    let near = worth_mentioning_undated(
        latest.value,
        objective_seconds * NEAR_ENOUGH_TO_MENTION,
        Concern::RisingTo,
        &projection,
    );
    if !projection.is_actionable() && !near {
        return None;
    }

    Some(Finding {
        check: "replication-lag",
        subject: "change capture".to_string(),
        severity: match projection {
            Projection::Already => Severity::Critical,
            Projection::Crossing { .. } => Severity::Warning,
            _ => Severity::Note,
        },
        observed: format!("{:.0}s behind, objective {objective_seconds:.0}s", latest.value),
        projection,
        remediation: "Publication is falling behind capture. Check the maintenance duty \
                      cycle first — compaction competing with the applier is the usual \
                      cause, and the applier never yields. A growing arrival buffer is the \
                      same problem seen from the other side."
            .to_string(),
    })
}

/// How long a backup may go unproven before it is a finding.
///
/// Thirty days. Not derived from anything --- it is a judgement about how long an
/// organisation is willing to have been unable to restore without knowing it, and it is
/// named here so that somebody who disagrees can find the number.
pub const DRILL_OBJECTIVE_MICROS: i64 = 30 * 24 * 3_600 * 1_000_000;

/// Whether the backup has been proven restorable recently enough.
///
/// # The one measure whose rate is known before any observation
///
/// Everything else in this crate needs two samples before it can give a date, because a
/// value alone does not imply a rate. Staleness is different: it rises at exactly one second
/// per second, and always has. So this check gives a **firm** date on a first run, and it is
/// the only one that can.
///
/// That is worth stating rather than leaving as an accident of the arithmetic, because the
/// natural instinct is to feed this through the same [`Trend`] machinery as everything else
/// --- which would collect observations for a week to estimate a rate that is already known
/// exactly, and would report `TooFewObservations` in the meantime about the one thing that
/// needs no observing.
///
/// `FR-OPS-15`: *"an untested backup is a rumour"*. A backup that has never been proven is
/// therefore not a warning about the future --- it is already the thing the requirement
/// forbids.
#[must_use]
pub fn restore_drill(last_pass: Option<i64>, objective_micros: i64, now: i64) -> Option<Finding> {
    let remediation = "Run a restore drill: `sankhya-server drill`. If it fails, the backup is \
                       not a backup and this is an incident rather than a maintenance task. See \
                       docs/runbooks/restore-drill.md."
        .to_string();
    let Some(last) = last_pass else {
        return Some(Finding {
            check: "restore-drill",
            subject: "backup".to_string(),
            severity: Severity::Critical,
            // Deliberately not "0 days ago". Never having proven a backup and having proven
            // it a long time ago are different situations, and only one of them is evidence
            // that the drill works at all.
            observed: "no restore drill has ever passed".to_string(),
            projection: Projection::Already,
            remediation,
        });
    };

    let elapsed = now.saturating_sub(last);
    if elapsed >= objective_micros {
        return Some(Finding {
            check: "restore-drill",
            subject: "backup".to_string(),
            severity: Severity::Critical,
            observed: format!(
                "the last passing restore drill was {} ago",
                crate::projection::human_duration(elapsed / 1_000_000)
            ),
            projection: Projection::Already,
            remediation,
        });
    }

    // Reported before it is breached, because a drill takes time to schedule and a backup
    // that will be unproven next Tuesday is worth knowing about this week.
    let remaining = objective_micros.saturating_sub(elapsed);
    if remaining > objective_micros / 4 {
        return None;
    }
    Some(Finding {
        check: "restore-drill",
        subject: "backup".to_string(),
        severity: Severity::Warning,
        observed: format!(
            "the last passing restore drill was {} ago",
            crate::projection::human_duration(elapsed / 1_000_000)
        ),
        projection: Projection::Crossing {
            seconds: remaining / 1_000_000,
            // Firm on a first run, which no other check in this crate can be. Staleness
            // rises at one second per second and needs no observations to establish that.
            confidence: Confidence::Firm,
            fit: 1.0,
        },
        remediation,
    })
}

/// How long an immutability control may go unverified.
///
/// Ninety days. Longer than the restore objective deliberately: an attestation is a
/// *deliberate attempt at corruption* against a non-production copy, so running one is a
/// scheduled exercise rather than something to do weekly. The risk it covers --- `RSK-28`,
/// a storage policy replaced underneath a control somebody is relying on --- moves at the
/// speed of infrastructure change, not at the speed of data.
pub const ATTESTATION_OBJECTIVE_MICROS: i64 = 90 * 24 * 3_600 * 1_000_000;

/// Whether the write-once controls have been proven to still refuse writes.
///
/// # Why this is silent when nothing is archived
///
/// `RSK-28` is about *"immutability controls silently removed by a later storage policy
/// change"*, and a deployment that archives nothing has no such control to lose. A check that
/// fired anyway would be `Critical` on every install from the day it shipped, which is how a
/// check stops being read --- and it would be loudest on exactly the deployments where it
/// means least.
///
/// So `archived` gates it. The cost of that choice is that a deployment which *starts*
/// archiving inherits a check that has never passed, which is the correct state to be in and
/// is reported as such.
#[must_use]
pub fn archive_attestation(
    last_pass: Option<i64>,
    archived: bool,
    objective_micros: i64,
    now: i64,
) -> Option<Finding> {
    if !archived {
        return None;
    }
    let remediation = "Run an attestation against a non-production copy of the archive: \
                       `sankhya-server attest <archive>`. If it reports ALLOWED, the \
                       immutability control is not in force and everything relying on it is \
                       unprotected now — not at some future point."
        .to_string();
    let Some(last) = last_pass else {
        return Some(Finding {
            check: "archive-attestation",
            subject: "archive".to_string(),
            severity: Severity::Critical,
            // The same distinction the restore drill draws, for the same reason: never having
            // proven a control and having proven it long ago are different situations, and
            // only one of them is evidence the drill works at all.
            observed: "data is archived and no attestation has ever passed".to_string(),
            projection: Projection::Already,
            remediation,
        });
    };

    let elapsed = now.saturating_sub(last);
    if elapsed >= objective_micros {
        return Some(Finding {
            check: "archive-attestation",
            subject: "archive".to_string(),
            severity: Severity::Critical,
            observed: format!(
                "the last passing attestation was {} ago",
                crate::projection::human_duration(elapsed / 1_000_000)
            ),
            projection: Projection::Already,
            remediation,
        });
    }

    let remaining = objective_micros.saturating_sub(elapsed);
    if remaining > objective_micros / 4 {
        return None;
    }
    Some(Finding {
        check: "archive-attestation",
        subject: "archive".to_string(),
        severity: Severity::Warning,
        observed: format!(
            "the last passing attestation was {} ago",
            crate::projection::human_duration(elapsed / 1_000_000)
        ),
        projection: Projection::Crossing {
            seconds: remaining / 1_000_000,
            confidence: Confidence::Firm,
            fit: 1.0,
        },
        remediation,
    })
}

/// A byte count in the units an operator reads.
#[must_use]
pub fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes.abs();
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS.get(unit).copied().unwrap_or("B"))
}
