//! Diagnostics that tell an operator *when*, not just *what*.
//!
//! `FR-OPS-17` states the whole design in one comparison: **"compaction debt is 400 GB" is
//! far less actionable than "at the current write rate, query latency on this table will
//! double in about nine days"**.
//!
//! The consequence it does not state is the one that shapes this crate. A time cannot be
//! computed from a single sample --- it needs a rate, and a rate needs observations over
//! time. So the first run of a diagnostic can report values and must not report dates, and
//! [`projection::Projection::Unknown`] is a first-class outcome that names what is missing.
//!
//! A projection invented from one sample is a number with a date attached, and a date is
//! exactly what gets believed and acted on.

#![doc(html_root_url = "https://docs.rs/sankhya-diagnostic")]

pub mod check;
pub mod collect;
pub mod history;
pub mod projection;
pub mod soak;

pub use check::{
    archive_attestation, compaction_debt, human_bytes, replication_lag, restore_drill,
    storage_headroom, Finding, Report, Severity,
};
pub use collect::{TableUnderReview, COMPACTION_DEBT};
pub use history::{History, HistoryError, Measure};
pub use projection::{Concern, Confidence, Observation, Projection, Trend, Unknown};
