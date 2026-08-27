//! The cube lattice and the additivity algebra.
//!
//! Layer 1, and **no dependencies at all** — the same shape as `sankhya-graph-algo`, for the
//! same reason. These are pure functions of a declaration, so their property tests are fast
//! enough to exhaust a space rather than sample it, and that matters here more than
//! anywhere: [`ancestor`] is where a cube engine produces wrong answers.
//!
//! # The two decisions this crate exists to make
//!
//! **May this measure be summed along this axis?** ([`measure`]) A measure with no declared
//! rule is refused rather than defaulted to summation, because the default is wrong for an
//! entire class of measures and wrong invisibly.
//!
//! **May this query be answered from that materialised cuboid?** ([`ancestor`]) Refusing a
//! valid roll-up costs a slow query. Permitting an invalid one produces a number that is
//! wrong, plausible, and derived from real data.
//!
//! Everything else about cubing — resolving a definition against tables, executing over
//! Arrow, choosing what to materialise — sits above this and depends on it being right.

#![doc(html_root_url = "https://docs.rs/sankhya-cube-algo")]

pub mod ancestor;
pub mod hierarchy;
pub mod measure;

pub use ancestor::{answerable_from, rolled_away, Answerable};
pub use hierarchy::{Cyclic, Hierarchy};
pub use measure::{Along, Measure, Rule, Undeclared};
