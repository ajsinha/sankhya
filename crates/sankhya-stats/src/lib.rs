//! Statistics the optimizer can use, and the pruning they may be trusted for.
//!
//! # Why these live outside the table log
//!
//! The log says which files a table consists of. Getting that wrong makes queries fail
//! or double-count, so it is written conservatively and never guessed at.
//!
//! Statistics are different in exactly one respect that changes everything: **they are
//! rebuildable**. A wrong statistic can be recomputed from the data, and until it is,
//! the worst outcome should be a slow plan. That is only true if statistics are never
//! trusted for anything that changes an *answer* — which is the rule this crate is built
//! around.
//!
//! # The rule
//!
//! **A statistic may make a query slower. It may never make a query wrong.**
//!
//! Concretely: bounds may be used to skip a file only when they *prove* nothing in it
//! can match. Anything uncertain — an absent bound, a type that does not line up, a
//! comparison that cannot be made — means the file is read. Reading a file that turns
//! out to hold nothing costs time. Skipping a file that holds something costs an answer,
//! silently, and nothing downstream can detect it.
//!
//! Distinct-value estimates are deliberately kept away from pruning altogether. They are
//! approximate by construction, so no pruning decision may depend on them however
//! convenient it looks.

#![doc(html_root_url = "https://docs.rs/sankhya-stats")]

mod column;
mod overflow;
mod prune;
mod sketch;

pub use column::{Bound, ColumnStats, MergeError};
pub use overflow::{decimal_sum_risk, integer_sum_risk, SumRisk};
pub use prune::{can_skip, Predicate};
pub use sketch::DistinctSketch;
