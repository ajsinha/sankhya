//! Metrics that cannot be recorded unless they were declared first.
//!
//! Most metrics libraries take a string. This one takes a `&'static Metric` from
//! [`catalogue`], so an undeclared metric is not refused --- it cannot be written. Every
//! exported series therefore has a documented meaning, a unit, a group, and a bound on its
//! cardinality, because those are fields on the thing you had to pass to record it.
//!
//! Two properties follow, and both are checked rather than asserted:
//!
//! - **Nothing exports without documentation.** `docs/METRICS.md` is generated from
//!   [`catalogue::ALL`], and `cargo xtask check-catalogues` fails if the file on disk
//!   disagrees with what the catalogue would produce.
//! - **Nothing is documented without being exported.** The same check requires every
//!   declared metric to be recorded somewhere in the source, so the catalogue cannot drift
//!   into a wishlist.
//!
//! A metric that may page must name a runbook --- [`metric::Alert::runbook`] is not an
//! `Option` --- and the check requires that runbook to exist. `M6`'s fifth exit criterion
//! asks for a runbook for every alert that can page; this makes it hold rather than making
//! it auditable.

#![doc(html_root_url = "https://docs.rs/sankhya-metrics")]

pub mod catalogue;
pub mod metric;
pub mod registry;

pub use metric::{Alert, Group, Kind, Label, Metric, Unit, Values};
pub use registry::{Registry, Rejections};
