//! Turning a change-event stream into table mutations.
//!
//! # Why this is a separate, pure crate
//!
//! This is the highest-leverage testability decision in the project. Placing the seam
//! at *decoded events in, mutation plan out* means the apply logic — batching,
//! transaction boundaries, idempotency, the withheld-value rule — is exercised by
//! thousands of randomised crash and interleaving scenarios in milliseconds, against
//! an in-memory table, with no database anywhere.
//!
//! Testing the same logic through a live database would be a thousand times slower and
//! non-deterministic, which in practice means it would be tested far less.
//!
//! # The rule that governs batching
//!
//! A batch may span several source transactions but must never split one. Only a
//! sealed transaction is eligible to be flushed, so a partially received transaction
//! is carried forward across batch boundaries rather than written.

#![doc(html_root_url = "https://docs.rs/sankhya-cdc-apply")]

mod batch;
mod mutation;

pub use batch::{Batcher, BatchPolicy, FlushReason};
pub use mutation::{Mutation, MutationPlan, Op, Row, apply_unchanged, op_of};
