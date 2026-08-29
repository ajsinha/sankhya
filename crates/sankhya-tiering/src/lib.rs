//! Lifecycle tiering: policy, the purge state machine, the archival registry.
//!
//! **Empty by intent, and dated. Scheduled: M9, and explicitly gated** --- see
//! `IMPLEMENTATION_PLAN.md` §13. Tiering may not ship until continuous reconciliation has run
//! clean across every table class, the restore drill has passed repeatedly, and an archive
//! attestation drill has passed on a non-production archive.
//!
//! The gate is written down so that schedule pressure cannot quietly make this decision.
//! Purging the system of record before the copy is provably correct is indefensible, and no
//! amount of care in the tiering code substitutes for demonstrated reconciliation. This crate
//! staying empty until then is the gate working, not the gate being ignored.
