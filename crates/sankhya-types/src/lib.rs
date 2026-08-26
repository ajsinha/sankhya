//! Core vocabulary for SANKHYA.
//!
//! This crate is the bottom of the dependency graph: no IO, no async runtime, no
//! filesystem, no network. Everything above it depends on these types, so they are
//! deliberately small, total, and free of domain meaning.
//!
//! # Why newtypes
//!
//! A bare `u64` can be a row count, a byte count, a tenant, or a log position, and
//! the compiler cannot tell you when you have confused two of them. In a system whose
//! value proposition is correctness, that is not an acceptable failure mode. No public
//! signature in this workspace takes a bare primitive where a domain-free newtype
//! exists.

#![doc(html_root_url = "https://docs.rs/sankhya-types")]

mod ids;
mod fixed;
mod position;
mod temporal;

pub use ids::{EdgeId, NodeId, QueryId, SchemaName, TableId, TableName, TenantId};
pub use fixed::{Fixed, FixedError, Scale};
pub use position::{Lsn, LsnRange, TableVersion};
pub use temporal::{Timestamp, ValidityInterval};
