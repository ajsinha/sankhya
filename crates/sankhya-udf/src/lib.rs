//! A user's own aggregation: the four-method contract, run behind the sandbox.
//!
//! # What this is for
//!
//! A cube's measures compose along each dimension by a **declared rule** --- sum, last, max ---
//! and the reason that model exists is that the alternative produces plausible wrong figures: a
//! balance summed across time, a rate summed across anything. What it cannot express is the
//! rule that is *this* firm's: a weighted average with their weighting, an exposure netted
//! their way, a percentile with their interpolation.
//!
//! [ADR-0010](../../../docs/adr/0010-external-aggregations.md) settles the contract, and it is
//! the industry's because the industry converged on it for our reason:
//!
//! | Method | What it means here |
//! |---|---|
//! | `accumulate(state, values)` | Fold a **batch** into the state. Batch, not row |
//! | `merge(a, b)` | Combine two partial states. **Its presence is the composability declaration** |
//! | `finish(state)` | The state as a number |
//! | `initial()` | The empty state, optional |
//!
//! # The two things that make it safe to allow
//!
//! **It runs behind an operating-system boundary**, never in this process: no network, no
//! filesystem beyond the interpreter, no privilege, and bounded in time and memory. That is
//! [`sankhya_sandbox`], and [ADR-0023](../../../docs/adr/0023-the-sandbox-a-user-function-runs-in.md)
//! decides it. The function is handed rows a policy filtered *for a principal*, so a socket
//! would turn "may read" into "may publish".
//!
//! **Its determinism is exercised, not trusted.** A cuboid materialised from an aggregation
//! must give the same bits as the same query computed from base data, and Python can break that
//! without anybody lying --- dictionary ordering, a stray seed, floating point accumulated in a
//! different order. So [`Worker::declare`] accumulates the same values one way and then several
//! ways, merges the parts in two different groupings, and compares **by bits**. A function that
//! disagrees with itself is refused at declaration, with both answers, rather than found later
//! as two reports differing by a penny.

mod protocol;
mod worker;

pub use protocol::Refused;
pub use worker::{interpreter_needs, Aggregation, Worker};
