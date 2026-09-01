//! A declared feed: what arrives, how it is shaped, and where it lands.
//!
//! # What a feed is, and what it is not
//!
//! A **feed** is a declaration: a source of documents, a mapping from what those documents
//! contain to a table's columns, and the policies governing what happens when a document does
//! not fit. It is written down rather than compiled in, so adding a source is an act of
//! configuration.
//!
//! `sankhya-ingest` is the other kind of ingest --- change capture from a running database,
//! where the shape is discovered from the source's own catalogue rather than declared. The two
//! do not share a crate because they do not share a problem: capture cannot refuse a row that
//! its source considers valid, and a feed exists precisely to.
//!
//! **This crate is a producer for the write path that already exists.** Nothing here writes a
//! file. A feed that reached storage by another route would be the second writer
//! `check-writers` refuses.
//!
//! # The refusals, and why they are all at load
//!
//! [ADR-0018](https://github.com/ajsinha/sankhya/blob/main/docs/adr/0018-a-record-that-does-not-fit.md)
//! decides that a configuration may not silently widen a type, invent a value for a missing
//! key, or accept keys it has never seen. Each is a way to turn a defect at the source into
//! published data that looks fine, and looking fine is what makes it survive.
//!
//! So a [`Declaration`] is validated into a [`Feed`], **a `Feed` cannot be constructed any
//! other way**, and validation reports **every** failing rule rather than the first --- for
//! the reason the cube's measure validation gives: fixing them one at a time is how a person
//! gives up and writes the permissive thing everywhere.

#![doc(html_root_url = "https://docs.rs/sankhya-feed")]

pub mod bind;
pub mod command;
pub mod declare;
pub mod progress;
pub mod state;
pub mod quarantine;
pub mod run;
pub mod shape;
pub mod source;
pub mod stop;
pub mod validate;

pub use bind::{bind, Cell, Row, Unfit};
pub use command::{parse as parse_command, Command, CommandError};
pub use declare::{Column, DateFrom, Declaration, Microbatch, Missing, Quarantine, Unknown};
// `Standing` is not re-exported: `progress::Standing` says what a *source* is relative to the
// position, and `state::Standing` says what a *feed* is doing. Both are the right word in
// their own module, and flattening them here would force one of them to be renamed to
// something worse. Callers name the module.
pub use state::{Feeds, Health};
pub use progress::{Partial, Position};
pub use quarantine::{code, fingerprint, Refused};
pub use run::{run, Ran, RunError, Running};
pub use shape::{table_schema, Unassembled};
pub use source::{records, sources, Arrived};
pub use stop::{Outcomes, Reason, Span, Verdict};
pub use validate::{validate, Fault, Feed, Shaped};
