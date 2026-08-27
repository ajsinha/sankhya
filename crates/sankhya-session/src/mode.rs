//! How current an answer has to be.
//!
//! `FR-API-12` requires three modes, and the reason there are three rather than one is that
//! the honest answer to "how fresh is this?" differs by what the answer is *for*.
//!
//! - A dashboard wants speed and tolerates lag, as long as the lag is bounded and reported.
//! - A decision made at a keystroke needs everything committed before it.
//! - A figure that will be compared against itself next week needs the *same* answer next
//!   week, which means naming the version rather than asking for the current one.
//!
//! The third is the one people forget, and it is the only one that makes a result
//! reproducible. A report rerun a month later against "now" is a different report, and two
//! people comparing their copies will find differences nobody can account for.

use std::fmt;

/// How current an answer must be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadMode {
    /// Everything committed before the request started must be visible.
    ///
    /// The slowest, because it may have to wait for the applier to catch up. Correct for a
    /// decision taken at the moment of asking.
    Strong,
    /// Any answer no more than `max_lag_micros` behind.
    ///
    /// The lag is a bound, not a hope: a tier further behind than this is refused rather
    /// than used. An unbounded "eventually consistent" mode would let a stalled applier
    /// serve week-old data indefinitely, and nothing about the result would say so.
    BoundedFreshness {
        /// The most staleness this request will accept, in microseconds.
        max_lag_micros: i64,
    },
    /// Exactly this version, whatever has happened since.
    ///
    /// The only mode that makes a result reproducible, and therefore the one required for
    /// anything used as evidence.
    Pinned {
        /// The table snapshot to read.
        snapshot: u64,
    },
}

impl ReadMode {
    /// Whether re-running the same query in this mode gives the same answer.
    ///
    /// Only pinning does. Strong and bounded modes both mean "current", and current changes.
    #[must_use]
    pub const fn is_reproducible(self) -> bool {
        matches!(self, Self::Pinned { .. })
    }

    /// Whether an answer this stale is acceptable.
    #[must_use]
    pub const fn accepts_lag(self, lag_micros: i64) -> bool {
        match self {
            Self::Strong => lag_micros <= 0,
            Self::BoundedFreshness { max_lag_micros } => lag_micros <= max_lag_micros,
            // A pinned read is not stale by any amount: it asked for a specific version and
            // got it. Measuring its lag against "now" would be measuring the wrong thing.
            Self::Pinned { .. } => true,
        }
    }
}

impl fmt::Display for ReadMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Strong => f.write_str("strong"),
            Self::BoundedFreshness { max_lag_micros } => {
                write!(f, "bounded-freshness({max_lag_micros}us)")
            }
            Self::Pinned { snapshot } => write!(f, "pinned(snapshot {snapshot})"),
        }
    }
}

/// Why a read mode could not be satisfied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TooStale {
    /// The data is further behind than the request allows.
    Behind {
        /// How far behind it is.
        lag_micros: i64,
        /// How far behind the request tolerates.
        allowed_micros: i64,
    },
}

impl fmt::Display for TooStale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Behind {
                lag_micros,
                allowed_micros,
            } => write!(
                f,
                "the data is {lag_micros}us behind and this request allows {allowed_micros}us. \
                 Refusing rather than answering: an unbounded tolerance lets a stalled \
                 applier serve week-old data with nothing in the result to say so"
            ),
        }
    }
}

impl std::error::Error for TooStale {}
