//! Slot lifecycle, lag measurement, and the source-safety escalation ladder.
//!
//! # INV-2, and why it needs its own module
//!
//! > SANKHYA can never bloat, wedge or exhaust the storage of the database it
//! > replicates from.
//!
//! A logical replication slot retains write-ahead log from its restart position
//! forward. If the consumer stalls — starved of processor time by a runaway analytical
//! query, blocked on slow storage, or simply stopped — the source retains log
//! indefinitely until its volume fills, at which point it shuts down.
//!
//! **The failure mode is an analytical component taking down the transactional
//! system.** It is the worst outcome this architecture can produce, and it is entirely
//! self-inflicted.
//!
//! # Why the thresholds are ordered the way they are
//!
//! The database has its own safety valve: past a configured retention limit it
//! **invalidates the slot**. That protects the database, but an invalidated slot cannot
//! be resumed — recovery is a full re-snapshot of every replicated table.
//!
//! So SANKHYA's thresholds are deliberately set *below* the database's, in order that
//! **SANKHYA degrades on its own terms before the database degrades it**. Sacrificing
//! the analytical tier deliberately, with a recorded gap and an automatic re-snapshot,
//! is enormously better than discovering the slot was destroyed.
//!
//! This was not theoretical during development: a slot was invalidated by exactly this
//! mechanism during a bulk load, reporting `wal_status = 'lost'`.

#![doc(html_root_url = "https://docs.rs/sankhya-cdc-pg")]

mod safety;
mod slot;

pub use safety::{assess, Escalation, SafetyPolicy, Severity};
pub use slot::{SlotHealth, SlotState, WalStatus};
