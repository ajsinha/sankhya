//! Domain-agnostic graph primitives over a borrowed adjacency snapshot.
//!
//! Nothing here allocates a graph. Every algorithm takes an [`Adjacency`] by reference and
//! returns a result sized by what it found, so one immutable structure serves every
//! concurrent reader without a lock and without a copy.
//!
//! # What is deliberately not here
//!
//! No domain vocabulary. The questions this library is built to answer are, in their
//! natural phrasing, questions about money, ownership and control --- but a primitive named
//! for one industry's version of a question cannot be reused by another asking the
//! structurally identical one. So the primitives are named for their *shape*:
//! [`paths::cycles`] rather than round-tripping, [`product::influence`] rather than
//! beneficial ownership, [`flow::max_flow`] rather than value transferred.
//!
//! The mapping from a domain's question to a primitive's shape lives in a pack, which is
//! where the domain's vocabulary belongs. `check-vocabulary` enforces this mechanically.
//!
//! # The constraints that matter
//!
//! Three properties separate a primitive that can support an investigation from one that
//! merely looks like it does:
//!
//! - **Time-respecting.** A path through a graph is not a path through *events* unless the
//!   edges are non-decreasing in time. Static reachability over a temporal graph answers a
//!   question nobody asked, and answers it optimistically. See [`traverse::time_respecting`].
//! - **Bounded.** Every primitive takes a [`budget::Budget`] and reports truncation
//!   explicitly. A truncated result must never be mistakable for an absence of results.
//! - **Total.** Results are ordered deterministically, so that the same graph and the same
//!   question produce byte-identical output --- the property the incremental-hydration
//!   test depends on.

#![doc(html_root_url = "https://docs.rs/sankhya-graph-algo")]

pub mod budget;
pub mod csr;
pub mod ids;
pub mod traverse;

pub use budget::{Bounded, Budget, Truncation};
pub use csr::{Adjacency, AdjacencyBuilder, Arc, Edge, Neighbours, Validity};
pub use ids::{EdgeMask, EdgeType, Interner, VertexId, VertexType};
pub use traverse::{reachable, time_respecting, Reached, TimeConstraints};
