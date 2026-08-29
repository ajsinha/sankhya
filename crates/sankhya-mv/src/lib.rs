//! Materialized view definition and incremental refresh.
//!
//! **Empty, and deliberately undecided** --- see
//! [ADR-0014](../../docs/adr/0014-materialized-views-and-the-cube-lifetime.md).
//!
//! Every mechanism a materialized view needs already exists in `sankhya-cube`: a persisted
//! definition, materialisation keyed by *(definition, snapshot, scope, shape)*, a staleness
//! target that is checked rather than estimated, refresh on the maintenance tick, reclamation
//! of superseded results, and completeness carried from the filter to the presentation. None
//! of it is cube-specific.
//!
//! But a materialized view is **not** a cube with a query for a fact table, and the direction
//! matters: a maintained cube is one *shape* of maintained query, and a general view has no
//! grain, no measures and no additivity --- so the lattice, roll-up and ancestor answering
//! have no analogue. What is genuinely separate is **incremental** refresh, which is the part
//! this crate's own title names and the part nothing has built.
//!
//! So this is neither adopted with a date nor deleted. It is listed with a reason, which is
//! the honest state for a crate whose design question is still open, and the deliberate
//! difference from the nine empty crates resolved alongside it on 2026-08-28.
//!
//! It cannot be resolved until ADR-0012's declared-query cube exists: until a cube can be
//! defined over a query rather than a table name, folding views into that lifetime cannot be
//! built and a separate crate would have nothing to depend on.
