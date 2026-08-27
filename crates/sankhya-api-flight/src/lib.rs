//! Arrow Flight SQL: the bulk data plane.
//!
//! The wire-protocol front door is a row protocol, so every query ends by taking columnar
//! batches apart one value at a time. This surface does not: a batch is encoded in Arrow's
//! IPC format and the client's buffers are the same shape as the server's.
//!
//! See [ADR-0006](../../../docs/adr/0006-flight-sql.md) for why the dependency is cheap
//! here, what is deliberately absent, and the one client-visible behaviour change --- an
//! error can arrive mid-stream, because nothing is materialised before sending.

#![doc(html_root_url = "https://docs.rs/sankhya-api-flight")]

pub mod service;
pub mod ticket;

pub use service::{Queries, SankhyaFlight, TICKET_LIFETIME_MICROS};
pub use ticket::{Refused, Ticket};
