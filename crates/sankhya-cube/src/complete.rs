//! How much of an aggregate's input the aggregate actually saw.
//!
//! # A filtered total is a different number wearing the same clothes
//!
//! Row-level policy means two principals may run the same query and legitimately get
//! different totals. That is the security model working, and it is not the problem.
//!
//! The problem is that `4,182,900` computed over every row and `4,182,900` computed over the
//! sixty percent of rows a principal may read **render identically**. Nothing about the
//! second says it is a partial figure, so it is reconciled against a complete one, put in a
//! report, and acted on. `FR-QUERY-13` and `FR-CUBE-13` ask for a completeness measure for
//! exactly this reason, and [`Assessed`] makes it inseparable from the value --- the same
//! shape as [`Bounded`](sankhya_graph_algo::budget::Bounded) and
//! [`Consolidation`](crate::consolidate::Consolidation), because it is the same mistake.
//!
//! # Completeness cannot be computed from what survived
//!
//! This is the trap, and it is easy to build the whole feature and still have it read a
//! hundred percent forever.
//!
//! Policy removes rows. Removed rows leave no trace: a cell whose every row was withheld is
//! simply **absent**, indistinguishable from a cell that never had data. Count what arrived
//! and divide by what arrived, and the answer is always one. Count populated cells and the
//! answer is still one, because the empty ones are not there to count.
//!
//! So the withheld count must come **from the filter**, at the point rows are dropped, and
//! it is a parameter here rather than something this module derives. [`Completeness::of`]
//! takes both numbers because only the caller that applied the policy has the second one.
//!
//! # What this does not do
//!
//! It does not make the aggregate private. Repeated aggregates over overlapping filtered
//! sets can reveal individual rows by differencing --- ask for a total, ask again with one
//! more member included, subtract --- and reporting completeness does nothing about that.
//! Defending against it needs query auditing or noise, neither of which is here.
//!
//! Reporting completeness is itself a small disclosure: it tells a principal how many rows
//! they were not allowed to see. Refusing below a threshold discloses less but still
//! discloses, since a failure means completeness fell short. Both are accepted, because the
//! alternative is handing somebody a partial total labelled as a total, and a wrong number
//! acted on is worse than a bounded leak about cardinality. This is written down rather than
//! left implied so that nobody reads a completeness measure as a privacy guarantee.

use std::fmt;

/// How much of an aggregate's intended input reached it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completeness {
    contributed: u64,
    withheld: u64,
}

impl Completeness {
    /// Rows that contributed, and rows that did not reach the aggregate.
    ///
    /// The second number has to come from whatever dropped them. Nothing downstream can
    /// recover it: a dropped row leaves no trace, so an aggregate that counts only what
    /// arrived reports itself complete however much was removed.
    ///
    /// # What the second number is, and is not
    ///
    /// It is **every** row the aggregate did not see, whatever removed it. On the served path
    /// today the only supplier is hydration, which counts a null dimension key or a null
    /// measure --- so in practice the figure a caller sees is a data-quality one.
    ///
    /// It used to be documented, and reported to callers, as *rows policy withheld*. Nothing
    /// anywhere supplies a policy count: cube authorization is table-level and
    /// all-or-nothing, so a principal either hydrates the cube or does not. An operator shown
    /// `withheld: 333` was being told a policy had hidden 333 rows whose region was null.
    ///
    /// Naming it for the mechanism rather than for one possible cause is also what lets a row
    /// filter start supplying it later without the meaning shifting underneath a reader.
    #[must_use]
    pub const fn of(contributed: u64, withheld: u64) -> Self {
        Self { contributed, withheld }
    }

    /// Nothing was dropped.
    ///
    /// Named, so that claiming completeness is a positive act. A `Default` here would let
    /// every value that nobody thought about report itself complete.
    #[must_use]
    pub const fn complete(contributed: u64) -> Self {
        Self { contributed, withheld: 0 }
    }

    /// How many rows contributed.
    #[must_use]
    pub const fn contributed(&self) -> u64 {
        self.contributed
    }

    /// How many rows did not reach the aggregate, for any reason. See [`Completeness::of`].
    #[must_use]
    pub const fn withheld(&self) -> u64 {
        self.withheld
    }

    /// How many rows the aggregate would have seen had none been dropped.
    #[must_use]
    pub const fn considered(&self) -> u64 {
        self.contributed.saturating_add(self.withheld)
    }

    /// The fraction that contributed, or `None` when there was nothing to see.
    ///
    /// `None` rather than `1.0`, because an aggregate over no rows at all is not a complete
    /// aggregate --- it is the absent-versus-zero distinction from [`crate::cells`] again,
    /// and rounding it up to "complete" is how an empty result passes a threshold.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        let considered = self.considered();
        if considered == 0 {
            return None;
        }
        Some(self.contributed as f64 / considered as f64)
    }

    /// Whether every intended row contributed.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.withheld == 0 && self.contributed > 0
    }

    /// Completeness of two inputs combined.
    ///
    /// Counts are added rather than fractions averaged. A mean of fractions weights a cell
    /// of three rows the same as one of three million, so a roll-up over one heavily
    /// filtered small cell and one complete large one reports about half.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        Self {
            contributed: self.contributed.saturating_add(other.contributed),
            withheld: self.withheld.saturating_add(other.withheld),
        }
    }
}

/// The least completeness a caller will accept.
///
/// `FR-QUERY-13`: below the threshold the query **fails rather than returning a flattering
/// result**. The refusal is the feature --- a caller who wanted the partial figure can ask
/// for it by name, and one who did not is not handed it by default.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Threshold {
    at_least: f64,
}

impl Threshold {
    /// Nothing may be withheld.
    pub const COMPLETE: Self = Self { at_least: 1.0 };

    /// A threshold, as a fraction between zero and one.
    ///
    /// # Errors
    /// [`NotAFraction`] for anything outside that range or not a number. A threshold of 1.5
    /// refuses everything and a threshold of `NaN` compares false against every value, so
    /// both would silently reject every aggregate --- and "the query failed" is exactly what
    /// a genuine policy breach looks like, so nobody would question it.
    pub fn at_least(fraction: f64) -> Result<Self, NotAFraction> {
        if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
            return Err(NotAFraction { given: fraction });
        }
        Ok(Self { at_least: fraction })
    }

    /// Whether this completeness meets it.
    #[must_use]
    pub fn met_by(&self, completeness: &Completeness) -> bool {
        completeness
            .fraction()
            .is_some_and(|seen| seen >= self.at_least)
    }

    /// The fraction required.
    #[must_use]
    pub const fn fraction(&self) -> f64 {
        self.at_least
    }
}

/// A threshold outside zero to one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct NotAFraction {
    /// What was given.
    pub given: f64,
}

impl fmt::Display for NotAFraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a completeness threshold of {} is not a fraction between 0 and 1; it would \
             reject every aggregate, and a rejection is indistinguishable from a genuine \
             policy shortfall",
            self.given
        )
    }
}

impl std::error::Error for NotAFraction {}

/// A value together with how much of its input it saw.
///
/// The two are inseparable by construction, because a partial total that has been separated
/// from its completeness is just a total.
#[derive(Clone, PartialEq, Debug)]
pub struct Assessed<T> {
    value: T,
    completeness: Completeness,
}

impl<T> Assessed<T> {
    /// A value and its completeness.
    #[must_use]
    pub const fn new(value: T, completeness: Completeness) -> Self {
        Self { value, completeness }
    }

    /// How much of the input it saw.
    #[must_use]
    pub const fn completeness(&self) -> &Completeness {
        &self.completeness
    }

    /// The value, only if it meets the threshold.
    ///
    /// # Errors
    /// [`Insufficient`] naming what was required and what was seen. Anything presented as a
    /// total should come through here.
    pub fn meeting(&self, threshold: &Threshold) -> Result<&T, Insufficient> {
        if threshold.met_by(&self.completeness) {
            return Ok(&self.value);
        }
        Err(Insufficient {
            required: threshold.fraction(),
            seen: self.completeness.fraction(),
            withheld: self.completeness.withheld(),
        })
    }

    /// The value whatever its completeness.
    ///
    /// Named so that reading it is a decision, and so that a reviewer can find every place
    /// that made it.
    #[must_use]
    pub const fn regardless(&self) -> &T {
        &self.value
    }

    /// Apply a function, keeping the completeness.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Assessed<U> {
        Assessed {
            value: f(self.value),
            completeness: self.completeness,
        }
    }
}

/// An aggregate that saw too little of its input to be reported.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Insufficient {
    /// The fraction required.
    pub required: f64,
    /// The fraction seen, or `None` when there was nothing to see.
    pub seen: Option<f64>,
    /// How many rows did not reach the aggregate, for any reason. See [`Completeness::of`].
    pub withheld: u64,
}

impl fmt::Display for Insufficient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.seen {
            Some(seen) => write!(
                f,
                "this aggregate saw {:.1}% of its input where {:.1}% was required, {} row(s) \
                 not having reached it — it is refused rather than returned, because a \
                 partial total is indistinguishable from a complete one once it is a number \
                 on a page",
                seen * 100.0,
                self.required * 100.0,
                self.withheld
            ),
            None => write!(
                f,
                "this aggregate had no input at all, which is not the same as having seen \
                 all of it; no completeness threshold can be met by nothing"
            ),
        }
    }
}

impl std::error::Error for Insufficient {}
