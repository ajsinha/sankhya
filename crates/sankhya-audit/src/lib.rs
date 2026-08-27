//! Hash-chained audit of authorization decisions and the data versions they saw.

#![doc(html_root_url = "https://docs.rs/sankhya-audit")]

pub mod chain;

pub use chain::{Broken, Chain, DataVersion, Entry, Hash, Record, RecordedDecision};
