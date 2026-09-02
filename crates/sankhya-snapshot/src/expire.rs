//! When a snapshot stops being honoured, and what a read of an expired one is told.

use std::fmt;

use crate::model::Snapshot;

/// How long a snapshot lives, as asked for when it was taken.
///
/// # Why there is no unbounded form
///
/// `ADR-0019` Decision 3. A snapshot pins files, so one that never expired would hold a whole
/// warehouse's versions alive forever --- `RSK-35`, the accumulation nobody is responsible for,
/// at warehouse scale rather than table scale.
///
/// `EXPIRE NEVER` is not spelled, and this type cannot express it. A decision nobody will
/// revisit, whose storage cost falls on somebody who did not make it, should not be one word
/// away.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Expiry {
    /// How many days from the day it was taken.
    days: u32,
}

/// The longest a snapshot may be asked to live.
///
/// Not a technical limit: a bound on how far ahead one person may commit storage that somebody
/// else will pay for. Two years is longer than any reproducibility requirement met so far and
/// far short of *forever*, which is the value this exists to make unavailable.
pub const LONGEST_DAYS: u32 = 730;

impl Expiry {
    /// A lifetime in days.
    ///
    /// # Errors
    ///
    /// [`Unaskable`] for zero days --- which is a snapshot that expires before anything can
    /// read it, and is a typo rather than a request --- or for longer than [`LONGEST_DAYS`].
    pub fn days(days: u32) -> Result<Self, Unaskable> {
        if days == 0 {
            return Err(Unaskable::Immediate);
        }
        if days > LONGEST_DAYS {
            return Err(Unaskable::TooLong { asked: days });
        }
        Ok(Self { days })
    }

    /// How many days it lasts.
    #[must_use]
    pub const fn count(self) -> u32 {
        self.days
    }

    /// The day this expires, given the day it was taken.
    ///
    /// Saturating, because a `taken_on` near the end of the representable range must not wrap
    /// into the past and make a fresh snapshot expired on arrival.
    #[must_use]
    pub fn falls_on(self, taken_on: i32) -> i32 {
        taken_on.saturating_add(i32::try_from(self.days).unwrap_or(i32::MAX))
    }
}

/// Why a lifetime cannot be asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unaskable {
    /// Zero days: expired before anything could read it.
    Immediate,
    /// Longer than this system will hold storage on one person's word.
    TooLong {
        /// What was asked for.
        asked: u32,
    },
}

impl fmt::Display for Unaskable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Immediate => write!(
                f,
                "a snapshot must last at least one day. Zero is a snapshot that expires \
                 before anything can read it, which is a typo rather than a request"
            ),
            Self::TooLong { asked } => write!(
                f,
                "{asked} days is longer than the {LONGEST_DAYS} a snapshot may be asked to \
                 live. The limit is not technical: a snapshot pins files, and this bounds how \
                 far ahead one person may commit storage somebody else will pay for"
            ),
        }
    }
}

impl std::error::Error for Unaskable {}

/// Where a snapshot stands on a given day.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    /// Honoured: reads as of it are served, and its files are pinned.
    Live,
    /// Past its day: reads are refused, and its files stop being pinned.
    ///
    /// Expiry **detaches** the pins; it deletes nothing. Retirement reclaims the files
    /// afterwards on its own grace period --- exactly as quarantine expiry works --- so an
    /// expiry that should not have happened is reversible for as long as that period lasts.
    Expired,
}

/// Where `snapshot` stands on `today`.
///
/// `today` is given rather than read, so the decision is reproducible and testable. A component
/// that reads a clock cannot be replayed, and this one decides whether files may be reclaimed.
#[must_use]
pub fn standing(snapshot: &Snapshot, today: i32) -> Standing {
    if today > snapshot.expires_on {
        Standing::Expired
    } else {
        Standing::Live
    }
}

/// What a reader is told when the snapshot they quoted has expired.
///
/// # Why this is a sentence and not an empty answer
///
/// `ADR-0019` Decision 3. A read after expiry must never be answered short and must never fall
/// back to *now*: both look like success, and the second silently substitutes the present for
/// the instant somebody asked for. The refusal names the day, which is what turns *"my report
/// changed"* into *"my snapshot expired on Tuesday"*.
#[must_use]
pub fn expired_message(snapshot: &Snapshot) -> String {
    format!(
        "the snapshot `{}` expired, and a read as of it is refused rather than answered from \
         the present. It was taken by `{}` and pinned {} table(s). Take a new snapshot, or \
         query without one",
        snapshot.name,
        snapshot.taken_by,
        snapshot.len()
    )
}

/// What a reader is told when the snapshot they quoted does not name a table they asked for.
///
/// # Why this is refused rather than answered with nothing
///
/// `ADR-0019` Decision 2, and the decision this whole feature turns on. A table the snapshot
/// does not name **did not exist** when it was taken. Answering with no rows treats "did not
/// exist" as "was empty", and a join against it returns the rows that survive an inner join
/// with nothing --- which is none, reported as success. The answer is not incomplete; it is
/// confidently zero.
#[must_use]
pub fn unnamed_message(snapshot: &Snapshot, table: &str) -> String {
    format!(
        "the snapshot `{}` does not name `{table}`, so that table did not exist when it was \
         taken. Refused rather than answered as empty: a table that did not exist is not a \
         table that was empty, and a join against one would return a confident zero. Take a \
         newer snapshot, or leave `{table}` out of this query",
        snapshot.name
    )
}
