//! Time.
//!
//! One representation, always UTC, always explicit. Three distinct *kinds* of time
//! exist in this system and must never be conflated: the time a business event
//! occurred, the time the transactional store committed it, and the time it became
//! visible analytically. Conflating them is a classic defect in this class of system,
//! so they are distinguished by field name at every use site rather than by a type
//! that would invite silent coercion.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A UTC instant with microsecond resolution.
///
/// Microseconds match the transactional store's own resolution, so a round trip
/// through SANKHYA neither gains false precision nor loses real precision.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Timestamp = Timestamp(0);

    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    #[must_use]
    pub const fn as_micros(self) -> i64 {
        self.0
    }

    /// Saturating rather than wrapping: a clock anomaly must not silently produce a
    /// timestamp on the far side of the epoch.
    #[must_use]
    pub const fn saturating_add_micros(self, delta: i64) -> Self {
        Self(self.0.saturating_add(delta))
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({}us)", self.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}us", self.0)
    }
}

/// A half-open validity interval `[from, to)`, open-ended when `to` is absent.
///
/// Edges carry these so traversal can be time-respecting. A path whose successive
/// edges are not non-decreasing in time is not a path through which anything can
/// actually have flowed, and returning such paths produces overwhelming false
/// positives in any flow analysis.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ValidityInterval {
    from: Timestamp,
    to: Option<Timestamp>,
}

impl ValidityInterval {
    /// Construct `[from, to)`. Returns `None` if the interval is inverted.
    #[must_use]
    pub fn new(from: Timestamp, to: Option<Timestamp>) -> Option<Self> {
        match to {
            Some(t) if t < from => None,
            _ => Some(Self { from, to }),
        }
    }

    #[must_use]
    pub const fn from_open(from: Timestamp) -> Self {
        Self { from, to: None }
    }

    #[must_use]
    pub const fn from(self) -> Timestamp {
        self.from
    }

    #[must_use]
    pub const fn to(self) -> Option<Timestamp> {
        self.to
    }

    #[must_use]
    pub fn contains(self, at: Timestamp) -> bool {
        at >= self.from && self.to.is_none_or(|t| at < t)
    }

    /// Whether this edge may be traversed at or after `at` — the predicate that makes
    /// a traversal time-respecting.
    #[must_use]
    pub fn live_at_or_after(self, at: Timestamp) -> bool {
        self.to.is_none_or(|t| t > at)
    }
}
