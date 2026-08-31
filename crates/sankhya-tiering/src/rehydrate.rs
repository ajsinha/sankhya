//! Reading archived data again without letting the copy become a second system of record.
//!
//! # The four properties, and why each is a type rather than a check
//!
//! `FR-TIER-20` names them together: a rehydration loads into a schema **excluded from every
//! publication**, is **never attached to the live parent**, is **read-only**, and carries a
//! **mandatory expiry** after which the copy is dropped automatically.
//!
//! Each one is the sort of property a review can confirm on the day and nothing enforces
//! afterwards, so each is expressed as something that cannot be built wrong:
//!
//! | Property | How |
//! |---|---|
//! | Excluded from every publication | [`Target::loading_into`] refuses a schema not asserted excluded |
//! | Never attached to the live parent | the same constructor refuses the parent's own schema, and nothing here takes a parent |
//! | Read-only | [`Rehydration`] has no method that writes and no mode that is not [`ReadOnly`] |
//! | Mandatory expiry | [`Expiry`] cannot be zero and [`Rehydration`] has no constructor without one |
//!
//! # Why the expiry is the load-bearing one
//!
//! `RSK-35`: *"rehydrated copies accumulate into a shadow system of record"*. That failure has
//! no moment --- nobody rehydrates a shadow system of record, they rehydrate one range for one
//! investigation, and then another, over a multi-year horizon, and each one is individually
//! reasonable. There is no day on which somebody could have decided differently, which is why
//! the expiry cannot be a decision made per rehydration.
//!
//! The other three are about what a copy *is*; this one is about how many there are.
//!
//! # Why a correction is a compensating entry by default
//!
//! `FR-TIER-19`. This is how record-keeping already works: a posted entry is reversed, not
//! erased. It preserves the audit trail completely, requires no rewrite, and is available
//! against archived data because it does not touch it --- the compensating row goes in the hot
//! tier and references the original.
//!
//! Controlled rewrite exists for the cases that genuinely need it, and it may not be
//! constructed without the two things that make it survivable: the prior version retained, and
//! an amendment link recorded. A rewrite that keeps neither is indistinguishable from the
//! archive having always said the new thing.

use crate::registry::Range;
use std::fmt;

/// Microseconds in a day.
const DAY: i64 = 86_400 * 1_000_000;

/// How long a rehydrated copy may exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Expiry {
    days: u32,
}

impl Expiry {
    /// The default horizon for an investigation.
    pub const DEFAULT_DAYS: u32 = 30;

    /// The longest a rehydration may be granted in one go.
    ///
    /// Not a safety property --- a person can rehydrate again --- but a limit on how far a
    /// single decision reaches. `RSK-35` is about accumulation over years, and a copy granted
    /// for years is that risk taken in one step.
    pub const MAXIMUM_DAYS: u32 = 90;

    /// An expiry of `days`.
    ///
    /// # Errors
    ///
    /// [`NotBounded`] when `days` is zero or beyond [`Self::MAXIMUM_DAYS`].
    pub const fn of(days: u32) -> Result<Self, NotBounded> {
        if days == 0 {
            return Err(NotBounded::Never);
        }
        if days > Self::MAXIMUM_DAYS {
            return Err(NotBounded::TooLong { days, maximum: Self::MAXIMUM_DAYS });
        }
        Ok(Self { days })
    }

    /// How many days.
    #[must_use]
    pub const fn days(&self) -> u32 {
        self.days
    }
}

impl Default for Expiry {
    fn default() -> Self {
        Self { days: Self::DEFAULT_DAYS }
    }
}

/// An expiry that would not bound the copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotBounded {
    /// Zero days, which is no expiry at all.
    Never,
    /// Longer than a single decision should reach.
    TooLong {
        /// What was asked for.
        days: u32,
        /// The most that is granted at once.
        maximum: u32,
    },
}

impl fmt::Display for NotBounded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Never => f.write_str(
                "an expiry of zero days is a copy that never expires, and rehydrated copies \
                 that never expire accumulate into a shadow system of record --- a failure with \
                 no moment at which anybody could have decided otherwise",
            ),
            Self::TooLong { days, maximum } => write!(
                f,
                "{days} days is longer than the {maximum} a single rehydration may be granted. \
                 A copy needed for longer is a copy somebody should have to ask for again"
            ),
        }
    }
}

/// The one access mode a rehydrated copy has.
///
/// A unit type rather than an enum with one variant used today: an enum invites a second
/// variant, and the second variant is the writable copy `FR-TIER-20` forbids.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReadOnly;

impl fmt::Display for ReadOnly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("read-only")
    }
}

/// Where a rehydration may load.
///
/// Constructed only by [`Target::loading_into`], which is where the two structural properties are
/// enforced. Holding one is evidence they were.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    schema: String,
}

impl Target {
    /// A schema that may receive a rehydrated copy.
    ///
    /// `excluded_from_publications` is asserted by whoever configured the schema, in the same
    /// spirit as the policy's other assertions: nothing here can look at a publication, and a
    /// classifier that guessed would be confidently wrong. Its absence is a refusal.
    ///
    /// # Errors
    ///
    /// [`Unsuitable`] when the schema is not asserted excluded, or is the live table's own.
    pub fn loading_into(
        schema: impl Into<String>,
        live_schema: &str,
        excluded_from_publications: bool,
    ) -> Result<Self, Unsuitable> {
        let schema = schema.into();
        if !excluded_from_publications {
            return Err(Unsuitable::MayBeCaptured { schema });
        }
        if schema == live_schema {
            return Err(Unsuitable::TheLiveSchema { schema });
        }
        Ok(Self { schema })
    }

    /// Which schema.
    #[must_use]
    pub fn schema(&self) -> &str {
        &self.schema
    }
}

/// Why a schema may not receive a rehydrated copy.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unsuitable {
    /// A publication could capture it, so the copy would return as duplicate rows.
    MayBeCaptured {
        /// The schema.
        schema: String,
    },
    /// It is the live table's own schema.
    TheLiveSchema {
        /// The schema.
        schema: String,
    },
}

impl fmt::Display for Unsuitable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MayBeCaptured { schema } => write!(
                f,
                "`{schema}` is not asserted excluded from every publication. A rehydrated copy \
                 in a captured schema is re-captured, and arrives in the published tier as \
                 duplicates of rows that are already there"
            ),
            Self::TheLiveSchema { schema } => write!(
                f,
                "`{schema}` is the live table's own schema. A rehydration is a copy to read, \
                 never an attachment to the parent, and loading into the parent's schema is how \
                 the two stop being distinguishable"
            ),
        }
    }
}

/// A copy of an archived range, loaded to be read and then dropped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rehydration {
    /// What it is called.
    pub name: String,
    /// The table the range came from.
    pub table: String,
    /// The range.
    pub range: Range,
    /// The archive it was read from.
    pub archive: String,
    /// Where it was loaded.
    pub target: Target,
    /// Who asked, so an accumulation has names against it.
    pub requested_by: String,
    /// When it was loaded, in microseconds from the epoch.
    pub at: i64,
    /// How long it may exist.
    pub expiry: Expiry,
    /// The only access it has.
    pub access: ReadOnly,
}

impl Rehydration {
    /// A rehydration.
    ///
    /// There is no constructor without an [`Expiry`], which is what makes the expiry mandatory
    /// rather than defaulted.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        table: impl Into<String>,
        range: Range,
        archive: impl Into<String>,
        target: Target,
        requested_by: impl Into<String>,
        at: i64,
        expiry: Expiry,
    ) -> Self {
        Self {
            name: name.into(),
            table: table.into(),
            range,
            archive: archive.into(),
            target,
            requested_by: requested_by.into(),
            at,
            expiry,
            access: ReadOnly,
        }
    }

    /// When it must be dropped.
    #[must_use]
    pub fn expires_at(&self) -> i64 {
        self.at.saturating_add(i64::from(self.expiry.days()).saturating_mul(DAY))
    }

    /// Whether it may still be read.
    #[must_use]
    pub fn live_at(&self, now: i64) -> bool {
        now < self.expires_at()
    }
}

/// Every rehydrated copy, and what is due to be dropped.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Register {
    copies: Vec<Rehydration>,
}

impl Register {
    /// Nothing rehydrated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a rehydration.
    pub fn record(&mut self, rehydration: Rehydration) {
        self.copies.push(rehydration);
    }

    /// Every copy, expired or not.
    #[must_use]
    pub fn copies(&self) -> &[Rehydration] {
        &self.copies
    }

    /// Drop everything past its expiry, returning what was dropped.
    ///
    /// **Automatic, and unconditional.** Unlike the quarantine reaper, this has nothing to
    /// weigh: a rehydrated copy is a copy of an archive that still exists, so dropping it loses
    /// nothing and keeping it is the risk. The two reapers look alike and are opposites.
    pub fn expire(&mut self, now: i64) -> Vec<Rehydration> {
        let (live, dropped): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.copies).into_iter().partition(|copy| copy.live_at(now));
        self.copies = live;
        dropped
    }

    /// How many copies exist, and the age of the oldest in days.
    ///
    /// `RSK-35`'s monitored signals, together, because either alone is uninformative: one copy
    /// held for a year and fifty held for a day are different problems and neither is visible
    /// in the other's number.
    #[must_use]
    pub fn accumulation(&self, now: i64) -> (usize, i64) {
        let oldest = self
            .copies
            .iter()
            .map(|copy| now.saturating_sub(copy.at) / DAY)
            .max()
            .unwrap_or(0);
        (self.copies.len(), oldest)
    }
}

/// How a correction to archived data is made.
///
/// `FR-TIER-19` makes the first the default, and the reason is that it is how record-keeping
/// already works: a posted entry is reversed, not erased.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Correction {
    /// A new row in the hot tier referencing the original.
    ///
    /// Touches nothing archived, so it is available whatever the archive's immutability
    /// controls say, and preserves the audit trail completely.
    Compensating {
        /// The row being corrected.
        original: String,
        /// The correcting row.
        entry: String,
    },
    /// The archived data itself rewritten.
    ///
    /// Constructible only through [`Correction::rewrite`], which requires both things that make
    /// it survivable.
    ControlledRewrite {
        /// The row being rewritten.
        original: String,
        /// Where the version before the rewrite is retained.
        prior_version: String,
        /// The link recording that an amendment happened, and to what.
        amendment: String,
    },
}

impl Correction {
    /// The default correction.
    pub fn compensating(original: impl Into<String>, entry: impl Into<String>) -> Self {
        Self::Compensating { original: original.into(), entry: entry.into() }
    }

    /// A controlled rewrite.
    ///
    /// # Errors
    ///
    /// [`NotSurvivable`] when the prior version or the amendment link is missing. A rewrite that
    /// keeps neither is indistinguishable from the archive having always said the new thing,
    /// which is the property archives exist to have.
    pub fn rewrite(
        original: impl Into<String>,
        prior_version: impl Into<String>,
        amendment: impl Into<String>,
    ) -> Result<Self, NotSurvivable> {
        let prior_version = prior_version.into();
        let amendment = amendment.into();
        if prior_version.trim().is_empty() {
            return Err(NotSurvivable::NoPriorVersion);
        }
        if amendment.trim().is_empty() {
            return Err(NotSurvivable::NoAmendmentLink);
        }
        Ok(Self::ControlledRewrite { original: original.into(), prior_version, amendment })
    }
}

/// Why a rewrite was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotSurvivable {
    /// Nothing says where the version before the rewrite went.
    NoPriorVersion,
    /// Nothing records that an amendment happened.
    NoAmendmentLink,
}

impl fmt::Display for NotSurvivable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPriorVersion => f.write_str(
                "a controlled rewrite must retain the version before it. Without that the \
                 rewrite is indistinguishable from the archive having always said the new thing",
            ),
            Self::NoAmendmentLink => f.write_str(
                "a controlled rewrite must record an amendment link. A corrected archive that \
                 does not say it was corrected is a corrected archive nobody can audit",
            ),
        }
    }
}
