//! Session consistency: read modes, session tokens, read-your-own-writes.
//!
//! # Why this exists
//!
//! Without it, the very first thing anyone tries — write a row, then query it
//! analytically — shows the row missing. They will reasonably conclude the system is
//! broken, and they will be right to: a unified engine that cannot show you your own
//! write is two systems with a shared installer.
//!
//! # How it works
//!
//! A write returns a token carrying the position at which it committed. Passing that
//! token to a subsequent read sets the target position, and the read waits — bounded by
//! its deadline — until capture has reached it.
//!
//! The client therefore waits for **capture**, not for publication. That is the
//! difference between a wait measured in the batching interval and one measured in the
//! commit cadence, and it is why freshness can be a read-path property rather than a
//! write-path one.
//!
//! # Why waiting is better than either alternative
//!
//! Returning stale data silently is a wrong answer. Failing immediately would make the
//! feature useless, since capture is only ever milliseconds behind. Waiting, with an
//! explicit failure when the bound is exceeded, is the only option that is both correct
//! and usable.

use sankhya_types::Lsn;
use std::fmt;
use std::time::Duration;

/// A position a client has observed, returned by a write and presented to a read.
///
/// Opaque by intent. It carries a position today; making that visible in the API would
/// prevent ever carrying anything else without a breaking change.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SessionToken {
    position: Lsn,
}

impl SessionToken {
    #[must_use]
    pub const fn at(position: Lsn) -> Self {
        Self { position }
    }

    /// The position this token requires to be visible.
    ///
    /// Crate-facing rather than public: callers pass tokens around, they do not
    /// interpret them.
    #[must_use]
    pub(crate) const fn position(self) -> Lsn {
        self.position
    }

    /// The later of two tokens.
    ///
    /// A session accumulates the furthest position it has observed, so a read is never
    /// allowed to go backwards relative to something the client has already seen.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        if other.position > self.position { other } else { self }
    }
}

impl fmt::Display for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session@{}", self.position)
    }
}

/// What a reader is asking for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ReadMode {
    /// Whatever has been captured. No waiting, no guarantee.
    Eventual,
    /// At least everything the session has already observed.
    ///
    /// The mode that makes write-then-read work.
    #[default]
    ReadYourWrites,
    /// Within a stated staleness bound.
    Bounded { max_staleness: Duration },
    /// A pinned, immutable version.
    ///
    /// Deterministic and replayable precisely *because* it excludes anything still in
    /// flight. This is the mode reproducible outputs must use.
    Snapshot { at: Lsn },
}

/// What a reader should do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    /// Proceed, reading as of this position.
    Ready { target: Lsn },
    /// Wait for capture to reach this position, then proceed.
    ///
    /// The estimate is advisory — it says how far behind capture is, not how long it
    /// will take to catch up.
    Wait { until: Lsn, behind_by: u64 },
}

/// Why a read cannot be served.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FreshnessError {
    /// Capture did not reach the required position within the deadline.
    ///
    /// Failing is deliberate. Serving the older data would be a silently wrong answer,
    /// and nothing about it would look wrong.
    NotReached { required: Lsn, applied: Lsn },
    /// A pinned read asked for a version that has not been captured.
    SnapshotUnavailable { requested: Lsn, applied: Lsn },
}

impl fmt::Display for FreshnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReached { required, applied } => write!(
                f,
                "capture has reached {applied} but {required} was required; the read \
                 was refused rather than answered with older data"
            ),
            Self::SnapshotUnavailable { requested, applied } => write!(
                f,
                "version {requested} has not been captured; the furthest available is \
                 {applied}"
            ),
        }
    }
}

impl std::error::Error for FreshnessError {}

/// Decide whether a read may proceed, must wait, or must fail.
///
/// Pure: it takes the applied position rather than reading it, so every combination is
/// testable without a running pipeline.
///
/// # Errors
///
/// Returns [`FreshnessError`] when the request cannot be satisfied at all — a pinned
/// read of an uncaptured version. A read that merely needs to wait returns
/// [`Visibility::Wait`] rather than an error.
pub fn evaluate_visibility(
    mode: ReadMode,
    session: Option<SessionToken>,
    applied: Lsn,
) -> Result<Visibility, FreshnessError> {
    match mode {
        ReadMode::Eventual => Ok(Visibility::Ready { target: applied }),

        ReadMode::ReadYourWrites => {
            let Some(token) = session else {
                // No token means the client has observed nothing, so there is nothing
                // to be behind. This is the ordinary first request of a session.
                return Ok(Visibility::Ready { target: applied });
            };
            let required = token.position();
            if applied >= required {
                Ok(Visibility::Ready { target: applied })
            } else {
                Ok(Visibility::Wait {
                    until: required,
                    behind_by: required.get().saturating_sub(applied.get()),
                })
            }
        }

        ReadMode::Bounded { .. } => {
            // The staleness bound is enforced against measured lag by the caller, which
            // owns the clock. Here the request is simply served as of what exists.
            Ok(Visibility::Ready { target: applied })
        }

        ReadMode::Snapshot { at } => {
            if applied >= at {
                Ok(Visibility::Ready { target: at })
            } else {
                Err(FreshnessError::SnapshotUnavailable { requested: at, applied })
            }
        }
    }
}

/// Turn an exhausted wait into the error a client should see.
///
/// Separated from [`evaluate_visibility`] so the decision to give up is explicit at the
/// call site rather than buried in a policy.
#[must_use]
pub fn wait_exhausted(until: Lsn, applied: Lsn) -> FreshnessError {
    FreshnessError::NotReached { required: until, applied }
}
