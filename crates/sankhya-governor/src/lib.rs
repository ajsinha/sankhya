//! Deciding what the system will refuse to do, and why.
//!
//! Two mechanisms with one purpose: keeping the machine inside limits it cannot recover
//! from crossing.
//!
//! [`admission`] decides whether a query may start, because the query engine's hash
//! joins do not spill and an unaffordable query does not degrade — it terminates the
//! process.
//!
//! [`pressure`] decides how much the system will give up to protect the source, because
//! the source's limits are the ones that cannot be recovered from at all.
//!
//! Both are pure functions over declared state. That is deliberate: a system's behaviour
//! under load is exactly the behaviour nobody can reproduce on demand, so the part that
//! decides it should be the part that needs no machine to test.

#![doc(html_root_url = "https://docs.rs/sankhya-governor")]

pub mod admission;
pub mod pressure;

pub use admission::{admit, Decision, Demand, PoolState, Posture, Rejection, TenantLimits};
pub use pressure::{assess, Assessment, Level, Signals, Thresholds};
