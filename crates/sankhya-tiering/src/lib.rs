//! Lifecycle tiering: policy, the purge state machine, the archival registry.
//!
//! **`M9`, in progress since 2026-08-30, and explicitly gated** --- see
//! `IMPLEMENTATION_PLAN.md` §13. Tiering may not ship until continuous reconciliation has run
//! clean across every table class, the restore drill has passed repeatedly, and an archive
//! attestation drill has passed on a non-production archive.
//!
//! This crate was empty until the gate's third criterion had something behind it. The
//! attestation drill is now built --- `sankhya-backup`'s `attest` --- so the gate is a thing
//! that can be *run and fail*, rather than a sentence nothing enforces. That was the condition
//! for starting, and it is recorded here because the ordering was the decision.
//!
//! # What is still true, and does not change by anything being built
//!
//! **Destructive purge against a system of record stays disabled until `M11`.** This crate will
//! hold the whole purge path --- the state machine, the verification, the quarantine, the
//! anomaly guard, the kill switches --- and building it is not arming it. Those are two
//! decisions and only the first belongs to development.
//!
//! `DEC-15` is why that is not caution for its own sake. The capture path replicates deletes,
//! so an ordinary `DELETE` used to purge tiered data would faithfully propagate and **erase
//! from the published tier exactly the data the purge was meant to preserve**. An archival
//! purge has to be distinguishable from a business delete *structurally* rather than by
//! convention, which is what `FR-TIER-04`'s detach-then-drop is for.
//!
//! # What is here now
//!
//! [`policy`] --- what a tiering policy declares, and the eligibility rules a table must
//! satisfy before one can exist. Pure functions of a declaration, so they can be tested
//! exhaustively rather than sampled, and evaluated at policy creation rather than at purge
//! time. `FR-TIER-10` is explicit that a type which cannot round-trip must make a table
//! ineligible **at policy creation, not at purge time** --- discovering it mid-purge means
//! discovering it with a partition already detached.
//!
//! [`authorize`] and [`machine`] --- who may start a purge, and the durable phase order it
//! moves through. The journal is written before the action, because the other order leaves a
//! partition detached with nothing recording it.
//!
//! [`encode`] and [`verify`] --- the canonical byte encoding [`policy`] promised exists, and
//! the exhaustive comparison `FR-TIER-09` requires: row count, primary-key set equality via a
//! Merkle digest over sorted blocks, and per-column checksums **taken in key order**, because
//! two rows with their values exchanged pass every check that is not. `FR-TIER-15` is enforced
//! by the type system rather than by review: [`verify::Proof`] cannot be constructed except
//! from a comparison that found nothing, and it is the only way into
//! [`machine::Phase::Verified`].

#![doc(html_root_url = "https://docs.rs/sankhya-tiering")]

pub mod authorize;
pub mod command;
pub mod defence;
pub mod encode;
pub mod evidence;
pub mod machine;
pub mod migrate;
pub mod permission;
pub mod policy;
pub mod quarantine;
pub mod registry;
pub mod rehydrate;
pub mod schedule;
pub mod unify;
pub mod verify;

pub use authorize::Authorization;
pub use machine::{Entry, Halt, Phase, Purge};
pub use policy::{Eligibility, Ineligible, Policy, Retention};
pub use defence::{Extents, Verdict};
pub use registry::{Entry as ArchiveEntry, Range, Registry};
pub use quarantine::{Grace, Quarantine};
pub use migrate::Cold;
pub use command::{Cleared, Proposal};
pub use permission::Permission;
pub use rehydrate::{Correction, Rehydration};
pub use evidence::{Pack, Seal};
pub use schedule::{BlastRadius, Schedule, Stage};
pub use unify::{Plan, Unservable};
pub use verify::{Fingerprint, Proof, Scan, Verification};
