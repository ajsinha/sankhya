//! Arrow encoding and the Parquet write path.
//!
//! # Where this sits
//!
//! Decoded mutations arrive as text — that is how the source transmits them — and must
//! become typed columnar batches before anything can query them. This crate is that
//! boundary, and it is where the type mapping's promise is either kept or quietly
//! broken.
//!
//! # The rule
//!
//! A value that cannot be parsed into its declared type is a **hard error**, never a
//! null. Substituting a null would turn a parsing defect into missing data, which is
//! far harder to detect: the row is present, the query succeeds, and one column is
//! silently empty. Reconciliation would not necessarily catch it either, since row
//! counts would still agree.

#![doc(html_root_url = "https://docs.rs/sankhya-table")]

mod compact;
mod encode;
mod write;

pub use encode::{EncodeError, encode_batch};
pub use compact::{CompactionOutcome, compact_files, read_parquet_stats};
pub use write::{WriteReport, WriterConfig, write_parquet};
