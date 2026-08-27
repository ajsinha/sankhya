//! The published extension API --- the only crate here with a stability commitment.
//!
//! A pack may depend on this crate, `sankhya-types` and `sankhya-error`, and on nothing
//! else. That restriction is enforced by `cargo xtask check-layers`, and the enforcement is
//! the point: when a pack legitimately needs something this crate does not offer, the build
//! fails, and **that failure is the signal that the API has a gap**. The response is to
//! widen the API, never to widen the allowance.
//!
//! # What this crate is for
//!
//! To make "general-purpose" a mechanical claim rather than a marketing one. The test is
//! stated as an exit criterion: **each reference pack's change touches zero core files**.
//! Two packs from unrelated industries run on an unmodified engine, or the claim is false.
//!
//! # Why the types here are SANKHYA's own
//!
//! Nothing in this crate re-exports a type from the query engine. Doing so would hand this
//! crate's stability commitment to a project that has not made one, and a pack compiled
//! last year would stop building the day the engine's `DataType` changed shape. There is a
//! conversion cost at the boundary and it buys exactly that independence.
//!
//! # Cancellation
//!
//! Pack code runs inside a query, and a query has a deadline. [`Invocation::check`] is how
//! a well-behaved function cooperates --- a cheap atomic load, called between units of work.
//! A function that never calls it is stopped anyway, by the harness, and the query fails
//! with an error naming the pack rather than hanging. See [`sandbox`].

#![doc(html_root_url = "https://docs.rs/sankhya-ext")]

pub mod error;
pub mod function;
pub mod registry;
pub mod sandbox;
pub mod value;

pub use error::PackError;
pub use function::{Invocation, Pack, PackInfo, ScalarFunction, Signature, TableFunction};
pub use registry::{Registration, Registry};
pub use sandbox::{run_bounded, Sandbox, SandboxError};
pub use value::{LogicalType, Value};
