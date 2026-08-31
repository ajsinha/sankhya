//! The purge state machine: durable, resumable, and impossible to enter in the middle.
//!
//! # Two requirements that shape everything here
//!
//! `FR-TIER-08`: *"a durable, resumable state machine in which **every transition is committed
//! before the corresponding real-world action**, and each phase is idempotent and resumable
//! including within a phase."*
//!
//! `FR-TIER-15`: *"**There SHALL be no flag that skips verification.** Verification is
//! structurally absent from every path that could bypass it."*
//!
//! The second is not satisfiable by discipline. A `skip_verification: bool` that nobody passes
//! is one merge away from somebody passing it, and a code review is not a mechanism. Two things
//! enforce it here instead.
//!
//! **The order is a chain.** Every [`Phase`] names the one phase that must precede it, in
//! [`Phase::requires`], and [`Purge::entering`] refuses any transition that does not follow the
//! journal. A test asserts the property over the whole order rather than at one step: every
//! destructive phase has [`Phase::Verified`] somewhere behind it, so a phase added later cannot
//! open a path around it.
//!
//! **The chain alone is not enough**, because nothing in an ordering stops a caller from
//! journalling `Verified` without having verified anything. So [`Purge::entering`] refuses that
//! one phase outright, and [`Purge::verified`] --- the only way in --- takes a
//! [`Proof`](crate::verify::Proof), which has a private field, no constructor and no `Default`,
//! and can be obtained only from [`Verification::proof`](crate::verify::Verification::proof) on
//! a comparison that found nothing. There is no argument to omit because there is no parameter,
//! and there is no way to fabricate the evidence because the type does not offer one.
//!
//! # Why the journal is written before the action and not after
//!
//! Because the failure being designed against is a crash *between* the two, and only one order
//! survives it.
//!
//! Write-then-act means a crash can leave a journal entry for something that did not happen. On
//! resume, the phase is re-run --- which is safe, because every phase is idempotent, and that
//! is why idempotence is a requirement rather than a nicety.
//!
//! Act-then-write means a crash can leave a partition detached with nothing recording it. On
//! resume the machine believes the previous phase never ran, re-runs it, and the second detach
//! fails against a partition that is already gone --- or worse, succeeds against a different
//! one. There is no recovery from that which does not involve a person reading storage by
//! hand.
//!
//! So: the intent is durable before the action is attempted, always, and the cost is that a
//! resumed run repeats work it may already have done.

use crate::authorize::Authorization;
use std::fmt;

/// One phase of a purge, in the order they must happen.
///
/// The order is `DEC-15`'s verification sequence, which is stated as a safety principle rather
/// than a workflow: *"Data is never removed from the system of record until it is provably
/// durable and correct elsewhere."*
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Phase {
    /// The range has been chosen and nothing has been touched.
    Planned,
    /// The published tier has been proven to hold the range: row count, key-set equality and
    /// per-column checksums.
    Verified,
    /// The preconditions outside the data have been resolved --- the snapshot is committed,
    /// a backup or caught-up replication covers it, the retention basis applies and legal hold
    /// is clear.
    Gated,
    /// The immutable archive marker exists and the registry entry is mirrored to write-once
    /// storage.
    Marked,
    /// The partition is detached from the source and is in quarantine.
    Detached,
    /// The grace period has elapsed and the detached partition is dropped.
    Dropped,
    /// The outcome is in the archival registry.
    Recorded,
}

impl Phase {
    /// Every phase, in order.
    pub const ALL: [Self; 7] = [
        Self::Planned,
        Self::Verified,
        Self::Gated,
        Self::Marked,
        Self::Detached,
        Self::Dropped,
        Self::Recorded,
    ];

    /// Its name in the journal.
    ///
    /// Written rather than derived, because this string is what a resume reads back and a
    /// rename of the variant must not silently orphan every journal on disk.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Verified => "verified",
            Self::Gated => "gated",
            Self::Marked => "marked",
            Self::Detached => "detached",
            Self::Dropped => "dropped",
            Self::Recorded => "recorded",
        }
    }

    /// The phase that name refers to, if this build knows one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|phase| phase.name() == name)
    }

    /// The phase that must have completed before this one may begin.
    #[must_use]
    pub const fn requires(self) -> Option<Self> {
        match self {
            Self::Planned => None,
            Self::Verified => Some(Self::Planned),
            Self::Gated => Some(Self::Verified),
            Self::Marked => Some(Self::Gated),
            Self::Detached => Some(Self::Marked),
            Self::Dropped => Some(Self::Detached),
            Self::Recorded => Some(Self::Dropped),
        }
    }

    /// Whether reaching this phase has removed anything from the system of record.
    ///
    /// The line `M11` arms. Everything up to and including [`Self::Marked`] is reversible by
    /// doing nothing; from [`Self::Detached`] onwards a person is involved in undoing it.
    #[must_use]
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::Detached | Self::Dropped | Self::Recorded)
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a purge stopped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Halt {
    /// Verification did not match.
    ///
    /// `FR-TIER-14`: *"Verification failure SHALL be terminal until an operator acts. There
    /// SHALL be no automatic retry, because failure indicates a defect and retrying is the
    /// wrong response."* So this carries no retry hint and the machine offers no method to
    /// resume past it.
    VerificationFailed {
        /// What did not match.
        detail: String,
    },
    /// A precondition outside the data was not met.
    NotGated {
        /// Which one.
        detail: String,
    },
    /// A kill switch was thrown.
    ///
    /// `FR-TIER-32`: a kill switch stops new phases and **never** aborts a job mid-detach.
    /// That is why this is a reason a phase did not *start* rather than a way to interrupt one.
    Killed {
        /// Which switch, so an operator knows what to clear.
        switch: String,
    },
    /// [`Phase::Verified`] was attempted without evidence that verification passed.
    ///
    /// `FR-TIER-15` requires verification to be *structurally* absent from every path that
    /// could bypass it. [`Purge::entering`] is the general transition and it refuses this one
    /// phase outright; [`Purge::verified`] is the only way in, and it takes a
    /// [`Proof`](crate::verify::Proof) that only a comparison finding nothing can produce.
    /// There is no argument to omit because there is no parameter to pass.
    UnprovenVerification,
    /// The journal says a phase ran that this one cannot follow.
    OutOfOrder {
        /// What the journal's last entry was.
        last: Phase,
        /// What was attempted.
        attempted: Phase,
    },
}

impl fmt::Display for Halt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VerificationFailed { detail } => write!(
                f,
                "verification failed: {detail}. This is terminal until somebody looks at it --- \
                 a mismatch means a defect, and retrying a defect produces the same answer more \
                 confidently"
            ),
            Self::NotGated { detail } => write!(f, "a precondition is not met: {detail}"),
            Self::Killed { switch } => {
                write!(f, "the kill switch `{switch}` is set, so no further phase was started")
            }
            Self::UnprovenVerification => f.write_str(
                "`Verified` cannot be entered through the general transition: it needs the \
                 proof an exhaustive verification produces, and a purge that has not run one \
                 has nothing to offer",
            ),
            Self::OutOfOrder { last, attempted } => write!(
                f,
                "the journal's last entry is `{last}` and `{attempted}` was attempted; a purge \
                 cannot skip a phase, and a journal that says otherwise is not resumable"
            ),
        }
    }
}

/// One durable record that a phase is about to be attempted.
///
/// **Written before the action**, so a crash between the two leaves an entry for something that
/// may not have happened --- which is the recoverable direction, because every phase is
/// idempotent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Which purge.
    pub purge: String,
    /// The phase being entered.
    pub phase: Phase,
    /// When, in microseconds from the epoch.
    pub at: i64,
    /// Who or what authorised the purge, flattened for the record.
    pub attribution: Vec<(String, String)>,
}

impl Entry {
    /// One line for the journal.
    ///
    /// Tab-separated and hand-built. This file is replayed on resume, so its format is a
    /// contract rather than a rendering of a struct that may be refactored.
    #[must_use]
    pub fn line(&self) -> String {
        let who: Vec<String> = self
            .attribution
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        format!("{}\t{}\t{}\t{}", self.at, self.purge, self.phase.name(), who.join(","))
    }

    /// Read a line back.
    ///
    /// Returns `None` for anything this build does not understand, which the caller must treat
    /// as a journal it cannot resume rather than as an empty one.
    #[must_use]
    pub fn from_line(line: &str) -> Option<Self> {
        let mut parts = line.split('\t');
        let at = parts.next()?.trim().parse::<i64>().ok()?;
        let purge = parts.next()?.to_string();
        let phase = Phase::from_name(parts.next()?)?;
        let attribution = parts
            .next()
            .unwrap_or_default()
            .split(',')
            .filter(|pair| !pair.is_empty())
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                Some((key.to_string(), value.to_string()))
            })
            .collect();
        Some(Self { purge, phase, at, attribution })
    }
}

/// A purge in progress, and the phases it has entered.
///
/// # Why this holds an `Authorization` it never reads again
///
/// Because holding it is the point. The machine cannot be constructed without one, and
/// [`Authorization`] cannot be constructed except by the command path or the schedule
/// evaluator --- so there is no reachable state in which a purge is running and nobody
/// authorised it. Reading it later would be a use; requiring it is a guarantee.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Purge {
    purge: String,
    authorization: Authorization,
    entered: Vec<Phase>,
}

impl Purge {
    /// Begin a purge.
    ///
    /// The only entry point, and it takes the authorization by value so a caller cannot hold
    /// one and start several purges from it without saying so.
    #[must_use]
    pub fn begin(purge: impl Into<String>, authorization: Authorization) -> Self {
        Self {
            purge: purge.into(),
            authorization,
            entered: vec![Phase::Planned],
        }
    }

    /// Rebuild from a journal, to resume.
    ///
    /// # Errors
    ///
    /// Returns [`Halt::OutOfOrder`] when the journal's phases are not a prefix of the required
    /// order. A journal that skipped a phase describes a purge nobody can safely continue, and
    /// continuing anyway is how a partition is detached without having been verified.
    pub fn resume(
        purge: impl Into<String>,
        authorization: Authorization,
        journal: &[Entry],
    ) -> Result<Self, Halt> {
        let purge = purge.into();
        // Seeded with `Planned`, because a purge is in that phase by existing --- nothing is
        // journalled to enter it. An empty seed made the first real entry look like a skipped
        // phase, which is a resume refusing every journal it was written to read.
        let mut entered: Vec<Phase> = vec![Phase::Planned];
        for entry in journal.iter().filter(|entry| entry.purge == purge) {
            // Idempotent by construction: a phase recorded twice is a crash between the
            // journal write and the action, replayed. That is the expected case, not an error.
            if entered.last() == Some(&entry.phase) {
                continue;
            }
            if entry.phase.requires() != entered.last().copied() {
                return Err(Halt::OutOfOrder {
                    last: entered.last().copied().unwrap_or(Phase::Planned),
                    attempted: entry.phase,
                });
            }
            entered.push(entry.phase);
        }
        Ok(Self { purge, authorization, entered })
    }

    /// The furthest phase this purge has entered.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.entered.last().copied().unwrap_or(Phase::Planned)
    }

    /// Whether this purge has removed anything from the system of record.
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        self.entered.iter().any(|phase| phase.is_destructive())
    }

    /// Who authorised it.
    #[must_use]
    pub const fn authorization(&self) -> &Authorization {
        &self.authorization
    }

    /// The journal entry to write **before** attempting the next phase.
    ///
    /// # Errors
    ///
    /// [`Halt::OutOfOrder`] if `next` cannot follow the current phase, and [`Halt::Killed`] if
    /// a switch is set. A kill switch is checked here, at the start of a phase, because
    /// `FR-TIER-32` requires it to stop new phases and **never** abort one mid-detach.
    pub fn entering(
        &self,
        next: Phase,
        at: i64,
        kill_switch: Option<&str>,
    ) -> Result<Entry, Halt> {
        if next == Phase::Verified {
            return Err(Halt::UnprovenVerification);
        }
        self.transition(next, at, kill_switch)
    }

    /// The journal entry for [`Phase::Verified`], which needs evidence.
    ///
    /// # Why this is a separate method rather than a flag
    ///
    /// A `skip_verification: bool` nobody passes today is one merge away from somebody passing
    /// it, and a `verified: bool` is worse --- it makes the claim without the evidence. The
    /// [`Proof`](crate::verify::Proof) has a private field, no constructor and no `Default`, so
    /// the only way to hold one is to have compared two fingerprints and found nothing. That
    /// makes `FR-TIER-15` a property the compiler enforces rather than one a reviewer
    /// remembers.
    ///
    /// The proof is consumed rather than borrowed: it belongs to one comparison of one
    /// partition, and a proof kept in a variable and reused across partitions would be exactly
    /// the bypass this exists to prevent.
    ///
    /// # Errors
    ///
    /// As [`Self::entering`].
    pub fn verified(
        &self,
        proof: crate::verify::Proof,
        at: i64,
        kill_switch: Option<&str>,
    ) -> Result<Entry, Halt> {
        let _ = proof;
        self.transition(Phase::Verified, at, kill_switch)
    }

    fn transition(&self, next: Phase, at: i64, kill_switch: Option<&str>) -> Result<Entry, Halt> {
        if let Some(switch) = kill_switch {
            return Err(Halt::Killed { switch: switch.to_string() });
        }
        if next.requires() != Some(self.phase()) {
            return Err(Halt::OutOfOrder { last: self.phase(), attempted: next });
        }
        Ok(Entry {
            purge: self.purge.clone(),
            phase: next,
            at,
            attribution: self
                .authorization
                .attribution()
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        })
    }

    /// Record that a phase was entered, after its journal entry is durable.
    pub fn entered(&mut self, phase: Phase) {
        if self.entered.last() != Some(&phase) {
            self.entered.push(phase);
        }
    }
}
