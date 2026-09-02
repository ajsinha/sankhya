//! A name for one instant across many tables.
//!
//! # What a snapshot is, and what it is not
//!
//! A **position**, not a table. A clone pins one table at one version and appears in the
//! catalogue under its own name; a snapshot pins *many* tables at one consistent position and
//! is quoted by a query rather than queried. A clone freezes a thing; a snapshot freezes a
//! moment.
//!
//! It holds no rows, no schema and no files of its own --- only a version per table and the day
//! it stops being honoured.
//!
//! # Why it exists
//!
//! A calculation reads four tables: a population of records, a set of rates, a set of curves,
//! and the hierarchy they roll up through. It must read **all of them as of one instant**, or
//! the reconciliation problem this system exists to remove reappears *inside a single query*:
//! four tables, four moments, one number that reconciles to nothing.
//!
//! [ADR-0019](https://github.com/ajsinha/sankhya/blob/main/docs/adr/0019-named-snapshots.md)
//! decides the three questions that had no obvious answer, and this crate is the model those
//! decisions describe. It reads and writes documents; it does not read a warehouse, resolve a
//! position, or authorize anybody --- each of those belongs to something that already does it.

#![forbid(unsafe_code)]

pub mod expire;
pub mod model;
pub mod statement;

pub use expire::{Expiry, Standing};
pub use model::{Malformed, Pinned, Snapshot};
pub use statement::{parse, NotAStatement, Statement};
