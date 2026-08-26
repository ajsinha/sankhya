//! The change-event model and the replication-protocol decoder.
//!
//! # Why this crate is ours rather than vendored
//!
//! It parses untrusted bytes arriving over a network socket, which makes it both the
//! largest attack surface in the ingest path and the most correctness-critical
//! component in the system. It is therefore pure — bytes in, events out, no IO — so it
//! can be property-tested and fuzzed exhaustively without a database.
//!
//! The connection and transport may be vendored. The decoding may not.
//!
//! # The hazard that motivates the design
//!
//! An update message does not necessarily carry every column. A large value that was
//! not modified is transmitted as an *unchanged* marker rather than as data. Treating
//! that marker as a null and writing it downstream silently destroys real data, and
//! nothing about the resulting row looks wrong. [`TupleValue::Unchanged`] exists so
//! the type system forces every consumer to decide what to do about it.

#![doc(html_root_url = "https://docs.rs/sankhya-cdc-model")]

mod decode;
mod event;

pub use decode::{DecodeError, Decoder};
pub use event::{
    ColumnDescriptor, Message, RelationDescriptor, ReplicaIdentity, TupleData, TupleValue,
};
