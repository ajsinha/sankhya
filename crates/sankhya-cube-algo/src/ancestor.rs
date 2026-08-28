//! Whether one cuboid's answer can be built from another's.
//!
//! # This is where the wrong answers live
//!
//! A query for a cuboid nobody materialised can often be answered by aggregating a **finer**
//! one that somebody did. That is the entire economic argument for materialising anything:
//! one stored cuboid answers many queries.
//!
//! It is also the single most dangerous operation in a cube engine, and the danger is
//! asymmetric. Refusing a roll-up that would have been valid costs a slower query. Allowing
//! one that is not produces **a number that is wrong, plausible, and derived from real
//! data** — no null, no error, nothing missing. It reconciles against nothing because
//! nobody reconciles a subtotal.
//!
//! The rule is short:
//!
//! > A query for cuboid **C** may be answered from a materialised cuboid **D** only when
//! > every dimension in **D** and not in **C** — the dimensions being rolled *away* — has a
//! > rule that composes.
//!
//! # Semi-additive is worse than non-additive
//!
//! A non-additive measure fails obviously: no dimension composes, nothing can be rolled up,
//! and every query goes to base data. Slow, and correct.
//!
//! A semi-additive one is correct along most dimensions and wrong along exactly one. Roll a
//! balance up across accounts and it is right; roll the same balance up across time in the
//! same query and it is wrong — and the two look identical in a result set. It survives
//! casual checking precisely because most of it is right.
//!
//! That is why [`Measure::rule`] is per dimension and why this function takes the dimensions
//! being removed rather than a single verdict about the measure.
//!
//! [`Measure::rule`]: crate::measure::Measure::rule

use crate::measure::{Measure, Rule};
use std::fmt;

/// Whether a roll-up is permitted, and if not, why not.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Answerable {
    /// The finer cuboid may be aggregated to answer the coarser one.
    Yes,
    /// It may not. The query goes to base data.
    No {
        /// Which measure.
        measure: String,
        /// The dimension whose rule does not compose, and the rule.
        ///
        /// One dimension, not all of them: the first that forbids it is enough to refuse,
        /// and naming a single reason is more useful to somebody deciding what to
        /// materialise than a list.
        dimension: String,
        /// The rule that forbade it.
        rule: Rule,
    },
    /// A dimension being rolled away has no declared rule at all.
    ///
    /// Distinct from `No`, and it must be: `No` means *this is not valid*, which is a
    /// modelling answer. This means *nobody said*, which is a definition error and should
    /// have been refused when the cube loaded.
    Undeclared {
        /// Which measure.
        measure: String,
        /// Which dimension it says nothing about.
        dimension: String,
    },
}

impl Answerable {
    /// Whether the roll-up may proceed.
    #[must_use]
    pub const fn permitted(&self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// May `measure` be answered by rolling `rolling_away` out of a materialised cuboid?
///
/// `rolling_away` is the set of dimensions present in the materialised cuboid and absent
/// from the query — the axes being collapsed. Dimensions the query keeps are irrelevant: no
/// aggregation happens along them.
#[must_use]
pub fn answerable_from(measure: &Measure, rolling_away: &[&str]) -> Answerable {
    for dimension in rolling_away {
        match measure.rule(dimension) {
            None => {
                return Answerable::Undeclared {
                    measure: measure.name.to_string(),
                    dimension: (*dimension).to_string(),
                }
            }
            Some(rule) if !rule.composes() => {
                return Answerable::No {
                    measure: measure.name.to_string(),
                    dimension: (*dimension).to_string(),
                    rule,
                }
            }
            Some(_) => {}
        }
    }
    Answerable::Yes
}

/// The dimensions rolled away when answering `query` from `materialised`.
///
/// Everything the materialised cuboid groups by that the query does not. Returns `None` when
/// the materialised cuboid is **not finer** — it lacks a dimension the query needs, so it
/// cannot answer it at any price and this is not a roll-up question at all.
#[must_use]
pub fn rolled_away<'a>(query: &[&'a str], materialised: &[&'a str]) -> Option<Vec<&'a str>> {
    if query.iter().any(|wanted| !materialised.contains(wanted)) {
        return None;
    }
    Some(
        materialised
            .iter()
            .filter(|held| !query.contains(held))
            .copied()
            .collect(),
    )
}

impl fmt::Display for Answerable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yes => f.write_str("the roll-up is valid"),
            Self::No {
                measure,
                dimension,
                rule,
            } => write!(
                f,
                "`{measure}` combines by `{rule}` along `{dimension}`, which does not compose \
                 — an aggregate of aggregates along that axis is not the aggregate. This \
                 query goes to base data. Rolling it up would produce a number that is \
                 wrong, plausible, and derived from real data, with nothing missing and no \
                 error raised"
            ),
            Self::Undeclared { measure, dimension } => write!(
                f,
                "`{measure}` declares no rule along `{dimension}`, so whether it may be \
                 rolled up there is unknown. This is a definition error rather than a \
                 modelling answer, and the cube should have refused to load"
            ),
        }
    }
}
