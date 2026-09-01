//! Compaction policy and the maintenance scheduler.
//!
//! # Why this is a first-class subsystem rather than a background chore
//!
//! Continuous capture writes small files continuously — that is inherent to committing
//! on a cadence rather than in one batch. Small files are the leading cause of
//! analytical slowness in this design, and the cost falls on **query planning** rather
//! than on scanning: listing files, reading footers, and resolving metadata are all
//! paid before a single row is read.
//!
//! That makes it a fixed cost per query, so its impact is inversely proportional to
//! query size. It is invisible on a long aggregation and dominant on a short
//! interactive one — which is exactly the class of query anyone notices.
//!
//! So the system contains a feedback loop: capture creates the mess, maintenance
//! clears it, and queries pay if maintenance falls behind. Compaction cadence is
//! therefore an input to the read-path latency budget, not a background nicety.
//!
//! # The rule that makes aggressive compaction safe
//!
//! **Compaction only ever adds. A separate operation ever removes.**
//!
//! Rewriting is safe for readers in flight because their snapshot still references the
//! files they resolved. Only *deletion* is hazardous, and deletion is a distinct
//! operation with its own preconditions. Separating them means the frequent, cheap
//! operation carries essentially no risk, and the dangerous one runs rarely and under
//! stricter conditions.

#![doc(html_root_url = "https://docs.rs/sankhya-maintenance")]

mod compaction;
pub mod expire;
pub mod cuboid;
mod driver;
mod execute;
pub mod layout;
mod orphans;
mod schedule;
mod service;

pub use compaction::{
    plan_compaction, CompactionPlan, CompactionPolicy, CompactionUrgency, FileStat, PartitionState,
};
pub use driver::{
    apply, checkpoint_if_due, commit_tick, execute_tick, plan_tick, retire_completed, DriverPolicy,
    PendingCompaction, TickPlan, TickReport, CHECKPOINT_INTERVAL,
};
pub use layout::{check as check_clustering, clustering, clustering_key, declared};
pub use execute::{
    retire_inputs, run_compaction, RetentionPolicy, RetirementOutcome, StillReferenced,
};
pub use orphans::{plan_orphan_cleanup, sweep, FileOnDisk, OrphanPlan, OrphanPolicy, OrphanReport};
pub use schedule::{schedule, Class, Deferral, Job, Schedule, Scheduled, SystemState};
pub use service::{
    partitions_of, spawn as spawn_maintenance, spawn_watching as spawn_maintenance_watching, tables_under, MaintenanceHandle,
    MaintenancePolicy, Maintainer,
};
