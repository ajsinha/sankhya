//! Read-path planning: deciding which tiers answer a query, and proving it is safe.
//!
//! # The problem
//!
//! A query may be answered from several tiers at once — the transactional store, an
//! in-memory arrival buffer, published tables, derived aggregates. Each holds a
//! different span of history. Get the seam wrong in one direction and rows are counted
//! twice; get it wrong in the other and rows vanish. Neither failure announces itself:
//! the query returns a plausible number.
//!
//! # The resolution
//!
//! Every tier declares the log-position interval it covers. The planner selects a set
//! whose intervals are **contiguous, non-overlapping, and collectively cover
//! `[0, target]`**. Because the intervals are half-open at the start, adjacent tiers
//! abut exactly — there is no window in which both contain a position, and none in
//! which neither does.
//!
//! When no such set exists the planner **fails the query**. A coverage gap is a
//! correctness event, and answering partially would hide it.
//!
//! # Why the log position and not the clock
//!
//! The position is a global, transaction-consistent ordering. A transaction touching
//! several tables carries one position, so a single target either includes all of it or
//! none of it. Splicing on wall-clock time would lose that, and a query could see one
//! half of a transaction without the other.

#![doc(html_root_url = "https://docs.rs/sankhya-plan")]

mod session;
mod splice;

pub use session::{
    evaluate_visibility, wait_exhausted, FreshnessError, ReadMode, SessionToken, Visibility,
};
pub use splice::{is_exact_cover, plan_splice, Splice, SpliceError, TierRef};
