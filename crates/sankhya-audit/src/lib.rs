//! Hash-chained audit of authorization decisions and the data versions they saw.

#![doc(html_root_url = "https://docs.rs/sankhya-audit")]

pub mod journal;
pub mod chain;
pub mod keys;

pub use chain::{Broken, Chain, DataVersion, Entry, Hash, Record, RecordedDecision};
pub use keys::{Envelope, KeyError, KeyId, KeyProvider, Rotation, WrappedKey};
