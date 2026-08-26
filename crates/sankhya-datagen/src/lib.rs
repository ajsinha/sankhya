//! Deterministic synthetic data with known ground truth.
//!
//! # Why the schemas here are deliberately not financial
//!
//! SANKHYA's core must be domain-agnostic, and that claim is tested rather than
//! asserted. If the only fixtures were trading data, a core quietly shaped around one
//! industry would still pass every test. These ten schemas span logistics, telemetry,
//! retail, media, healthcare-adjacent and civic domains precisely so that a
//! finance-shaped assumption shows up as an awkward fit.
//!
//! # Why deterministic
//!
//! Reconciliation asserts that what arrives analytically matches what was written
//! transactionally. That comparison needs an *independent* model of the truth — not a
//! second query against the source, which would let a defect in the reader conceal a
//! defect in the pipeline. A seeded generator is that independent model: the same seed
//! reproduces the same rows on any machine, so expected state can be recomputed rather
//! than stored.

#![doc(html_root_url = "https://docs.rs/sankhya-datagen")]

mod schema;
mod generate;

pub use generate::{Generator, RowBatch, Scale};
pub use schema::{Column, ColumnKind, Schema, WriteProfile, all_schemas, schema_by_name};
