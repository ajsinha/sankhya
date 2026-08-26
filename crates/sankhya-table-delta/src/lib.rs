//! The table log: which files are live, durably.
//!
//! # Why this exists at all
//!
//! Compaction only ever adds. Between a merge and the retirement of its inputs, a
//! table's directory holds both the file that was written and the files it replaced —
//! the same rows twice, by design, for at least a full grace period. So "which files
//! belong to this table" is **not answerable from the filesystem**, and every reader
//! and planner that tries gets a wrong answer for the duration of that window.
//!
//! A directory of Parquet files is a storage layout. It is not a table. The log is what
//! makes it one.
//!
//! # Why the Delta log specifically, and why it is written by hand
//!
//! The format is chosen so that other engines can read these tables with no SANKHYA
//! process in the path. That is the entire benefit, and it is only real if the log is
//! actually valid — which is a claim that has to be tested rather than asserted.
//!
//! So this crate writes the log itself, in about two hundred lines, and the kernel is
//! used **in tests only** as an independent oracle: it reads what this crate wrote and
//! must agree with this crate's own reader about the live set. That arrangement is what
//! DEC-06's metadata-only coupling actually asks for — the storage library supplies a
//! definition of correctness, not an I/O layer — and it keeps eighty-four packages and
//! a duplicated HTTP client out of the shipped binary.
//!
//! The log's write path is genuinely small. The read path replays actions in order,
//! which is the whole of the protocol that matters here.
//!
//! # What this deliberately does not implement
//!
//! Checkpoints, deletion vectors, column mapping, partition values, statistics in the
//! `add` action, and every optional protocol feature. A reader that requires any of
//! them will refuse these tables, which is the correct outcome: refusing is visible,
//! and a partially-implemented protocol feature is not.

#![doc(html_root_url = "https://docs.rs/sankhya-table-delta")]

mod log;
mod schema;

pub use log::{
    commit, create, live_files, read_actions, Action, AddFile, CommitError, Format, LiveSet,
    Metadata, RemoveFile, Version,
};
pub use schema::{schema_string, UnsupportedType};
