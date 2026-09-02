//! Who still reads a file, once a file can belong to more than one table.
//!
//! **`M10`, started 2026-08-31, and design-gated** --- see
//! [ADR-0016](../../../docs/adr/0016-zero-copy-cloning.md), which had to be accepted before any
//! of this could be written.
//!
//! # The premise this crate exists to replace
//!
//! Three mechanisms decide that a file may be removed --- retirement, orphan collection and
//! `M9`'s purge --- and each consults **one table's log**. Every one of them is correct today
//! for the same reason: *a file belongs to exactly one table*. A clone makes that false, and
//! each becomes a way to delete data a clone is the only remaining reader of.
//!
//! The failure has no symptom at the time. Nothing errors, no query fails, and the evidence
//! arrives whenever somebody next reads that range of the clone --- possibly months later. That
//! is why `M10` is gated on a design rather than started from one.
//!
//! # What is here
//!
//! [`lineage`] --- what a clone records about where it came from, and how it survives being read
//! by something that has never heard of this system.
//!
//! [`family`] --- the transitive closure a reclamation decision has to consult instead of one
//! log. The answer `ADR-0016` chose: **reachability over the clone family**, not reference
//! counting, because a count that drifts low deletes data a clone is the only reader of, and
//! that is the exact failure the gate exists to prevent.
//!
//! # What is not here yet
//!
//! The clone action itself, the maintenance wiring, the refusals, and clone-aware backup. This
//! crate is the vocabulary those need, built first because all four consume it.

#![doc(html_root_url = "https://docs.rs/sankhya-clone")]

pub mod action;
pub mod ask;
pub mod ddl;
pub mod family;
pub mod lineage;
pub mod refuse;

pub use action::{clone_table, lineage_of};
pub use ask::{parse as parse_question, NotAQuestion, Question};
pub use ddl::{parse as parse_ddl, DdlError, Statement};
pub use family::{Cycle, Lineages};
pub use refuse::Refused;
pub use lineage::{Lineage, Malformed};
