//! Judging a long run: does any bounded measure grow without bound?
//!
//! # What a soak is for, and what it is not
//!
//! Not "it did not crash" --- that is what it reports and it is the one thing nobody
//! doubted. A soak exists for the class of failure invisible in any single sample and
//! obvious across a week: memory that grows, descriptors that are not returned, a cache with
//! no eviction, compaction that never quite catches up.
//!
//! Every one of those is something this system has a **bound** for. A soak is where the
//! bound is found not to work.
//!
//! # The criterion, which "clean" was not
//!
//! > A soak passes when **no bounded measure has a projection that crosses its threshold
//! > within the observation horizon**.
//!
//! A measure trending upward with a crossing three weeks out is a failure, not a curiosity.
//! It is exactly the one that ships.
//!
//! # Three kinds of bounded
//!
//! The naive version watches every number and complains when one rises. Half of them are
//! supposed to. So [`measure::Bound`] distinguishes what must be flat, what must be flat
//! *per unit of work*, and what climbs and is reclaimed --- where the question is whether the
//! **peaks** climb, because a line through a sawtooth means nothing.
//!
//! # The harness is proven to notice
//!
//! A soak that would have been green anyway is an untested backup by another name. So a test
//! injects a leak and requires the harness to fail on it --- see `tests/leak.rs`.
//!
//! # Why this lives in the diagnostic rather than in a crate of its own
//!
//! It was a separate `sankhya-soak` crate, and `sankhya-server` carried a dependency on it
//! so that one test could ask whether a measure was growing. That is the wrong shape twice
//! over: a soak is a *test*, so it must not add infrastructure; and "does this measure grow
//! without bound" is not a test question --- it is what [`crate::check`] asks of a running
//! system, using the same projection machinery.
//!
//! The two differ on one point, deliberately. [`crate::projection`] gates on linearity,
//! because a diagnostic projecting a date from a wandering series would be inventing one.
//! A soak measure is noisy and flat when healthy, so that gate reported everything
//! unjudgeable --- so [`judge`] reads the slope directly. One module with two readings, not
//! two crates with one each.

pub mod judge;
pub mod measure;
pub mod report;
pub mod sample;

pub use judge::{judge, Verdict};
pub use measure::{Bound, Watched, WATCHED};
pub use report::Report;
pub use sample::Samples;
