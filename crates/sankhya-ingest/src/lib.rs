//! The capture pipeline runtime.
//!
//! Joins the pieces that until now existed separately: decode a message, onboard the
//! relation it describes if this is the first sight of it, accumulate mutations into
//! transaction-aligned batches per table, and publish each batch as Parquet under the
//! mirrored path.
//!
//! # What this crate owns, and what it deliberately does not
//!
//! It owns the *sequencing*: which relation a message belongs to, when a table's batch
//! is ready, what the resulting file covers. It does **not** own the transport. Reading
//! bytes from a replication connection is a separate concern behind its own trait,
//! because the transport is the part most likely to be replaced and the least
//! interesting to test.
//!
//! # Per-table batching
//!
//! Each table batches independently. A single global batch would tie a busy table's
//! commit cadence to a quiet one's, so a trickle table would either force wasteful
//! commits on everything or be held hostage to a torrent.

#![doc(html_root_url = "https://docs.rs/sankhya-ingest")]

mod pipeline;

pub use pipeline::{Pipeline, PipelineStats, PublishedFile, TableState};
