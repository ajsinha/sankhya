//! An immutable graph bound to a snapshot, and the slot that publishes it.
//!
//! # Why an epoch is immutable
//!
//! Every query holds a reference to the adjacency arrays and reads them without a lock.
//! That is only sound because nothing can modify them: a hydration builds a *new* epoch and
//! swaps the pointer, and readers on the old one keep reading it until they finish. The
//! reference count is what frees it, so a long query never has the ground moved under it
//! and a rebuild never waits for one to end.
//!
//! # Why every result carries the epoch's identity
//!
//! A graph derived from tables is always *as of* something. A result that does not say
//! which snapshot it came from cannot be reconciled with a relational result taken at a
//! different moment, and the two will disagree in ways nobody can account for. So an epoch
//! carries its source snapshot and the lag at build time, and `FR-GRAPH-10` requires every
//! result to report them.

use crate::spec::GraphSpec;
use sankhya_graph_algo::csr::Adjacency;
use sankhya_graph_algo::ids::{EdgeType, Interner, VertexId, VertexType};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Which epoch a result came from.
///
/// Monotonic within a process. Two epochs with the same identifier are the same epoch;
/// comparing identifiers across processes means nothing, which is why the source snapshot
/// is carried separately.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct EpochId(pub u64);

/// A hydrated graph, frozen.
#[derive(Debug)]
pub struct Epoch {
    id: EpochId,
    snapshot: u64,
    built_at: i64,
    spec: GraphSpec,
    adjacency: Adjacency,
    interner: Interner,
    edge_type_names: BTreeMap<u16, String>,
    vertex_type_names: BTreeMap<u16, String>,
    heap_bytes: usize,
}

impl Epoch {
    /// Assemble an epoch. Called by hydration, which is the only thing that should.
    pub(crate) fn new(
        id: EpochId,
        snapshot: u64,
        built_at: i64,
        spec: GraphSpec,
        adjacency: Adjacency,
        interner: Interner,
    ) -> Self {
        let edge_type_names = spec
            .edge_type_ids()
            .into_iter()
            .map(|(name, index)| (index, name))
            .collect();
        let vertex_type_names = spec
            .vertex_type_ids()
            .into_iter()
            .map(|(name, index)| (index, name))
            .collect();
        let heap_bytes = adjacency.heap_bytes().saturating_add(interner.heap_bytes());
        Self {
            id,
            snapshot,
            built_at,
            spec,
            adjacency,
            interner,
            edge_type_names,
            vertex_type_names,
            heap_bytes,
        }
    }

    /// Which epoch this is.
    #[must_use]
    pub const fn id(&self) -> EpochId {
        self.id
    }

    /// The table snapshot version it was built from.
    #[must_use]
    pub const fn snapshot(&self) -> u64 {
        self.snapshot
    }

    /// When it was built.
    #[must_use]
    pub const fn built_at(&self) -> i64 {
        self.built_at
    }

    /// How far behind `now` this epoch is.
    ///
    /// The number a caller needs to decide whether the answer is current enough to act on.
    /// Reported rather than judged: what counts as too stale is a property of the question,
    /// not of the graph.
    #[must_use]
    pub const fn lag(&self, now: i64) -> i64 {
        now.saturating_sub(self.built_at)
    }

    /// The adjacency, for the primitives to borrow.
    #[must_use]
    pub const fn adjacency(&self) -> &Adjacency {
        &self.adjacency
    }

    /// How the epoch was derived.
    #[must_use]
    pub const fn spec(&self) -> &GraphSpec {
        &self.spec
    }

    /// Whether a time-respecting query over this epoch means what it appears to.
    ///
    /// False when any edge kind was hydrated without a validity column: a traversal may
    /// route through such an edge at any moment, so the ordering guarantee does not hold
    /// across the whole graph. `FR-GRAPH-03` forbids offering static traversal for flow
    /// analysis, and this is how a caller finds out that is what they would be getting.
    #[must_use]
    pub fn is_fully_temporal(&self) -> bool {
        !self.spec.has_untimed_edges()
    }

    /// The dense id for an external key.
    #[must_use]
    pub fn vertex(&self, key: &[u8]) -> Option<VertexId> {
        self.interner.lookup(key)
    }

    /// The external key behind a dense id.
    #[must_use]
    pub fn key(&self, vertex: VertexId) -> Option<&[u8]> {
        self.interner.key(vertex)
    }

    /// What kind of thing a vertex is.
    #[must_use]
    pub fn vertex_type(&self, vertex: VertexId) -> Option<&str> {
        let VertexType(index) = self.interner.vertex_type(vertex)?;
        self.vertex_type_names.get(&index).map(String::as_str)
    }

    /// The dense identifier for a named edge type.
    #[must_use]
    pub fn edge_type(&self, name: &str) -> Option<EdgeType> {
        self.spec.edge_type_ids().get(name).copied().map(EdgeType)
    }

    /// The name of a dense edge type.
    #[must_use]
    pub fn edge_type_name(&self, edge_type: EdgeType) -> Option<&str> {
        self.edge_type_names.get(&edge_type.0).map(String::as_str)
    }

    /// How many vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.adjacency.vertex_count()
    }

    /// How many edges, across every type.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.adjacency.edge_count()
    }

    /// Roughly how much memory this epoch occupies.
    #[must_use]
    pub const fn heap_bytes(&self) -> usize {
        self.heap_bytes
    }

    /// Bytes per vertex and per edge, for sizing hardware before buying it.
    ///
    /// `FR-GRAPH-03`'s exit criterion asks for these to be published. They are computed
    /// from a real epoch rather than estimated from the type sizes, because the interner's
    /// key storage dominates and depends entirely on how long the source keys are.
    #[must_use]
    pub fn footprint(&self) -> Footprint {
        #[allow(clippy::cast_precision_loss)]
        Footprint {
            total_bytes: self.heap_bytes,
            bytes_per_vertex: if self.vertex_count() == 0 {
                0.0
            } else {
                self.heap_bytes as f64 / self.vertex_count() as f64
            },
            bytes_per_edge: if self.edge_count() == 0 {
                0.0
            } else {
                self.heap_bytes as f64 / self.edge_count() as f64
            },
        }
    }
}

/// What an epoch costs in memory.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Footprint {
    /// Total bytes on the heap.
    pub total_bytes: usize,
    /// Bytes divided by vertex count.
    pub bytes_per_vertex: f64,
    /// Bytes divided by edge count.
    pub bytes_per_edge: f64,
}

/// A reference-counted handle to a published epoch.
pub type EpochRef = Arc<Epoch>;

/// The published epoch for one graph, replaceable without blocking readers.
///
/// A read takes the lock only long enough to clone an `Arc`, and a swap only long enough to
/// replace one. Neither waits for a query: a reader that has cloned the handle owns its
/// epoch until it drops it, and a swap during that time leaves the old epoch alive.
#[derive(Debug, Default)]
pub struct EpochSlot {
    current: parking_lot::RwLock<Option<EpochRef>>,
}

impl EpochSlot {
    /// An empty slot --- nothing hydrated yet.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A slot already holding an epoch.
    #[must_use]
    pub fn holding(epoch: EpochRef) -> Self {
        Self {
            current: parking_lot::RwLock::new(Some(epoch)),
        }
    }

    /// The current epoch, or `None` if nothing is hydrated.
    ///
    /// Returns an owned handle rather than a borrow, deliberately. A borrow would hold the
    /// lock for the duration of the query and block every rebuild behind the slowest
    /// traversal.
    #[must_use]
    pub fn current(&self) -> Option<EpochRef> {
        self.current.read().clone()
    }

    /// Replace the epoch, returning the one displaced.
    ///
    /// The displaced epoch is freed when its last reader drops it, not here.
    pub fn publish(&self, epoch: EpochRef) -> Option<EpochRef> {
        let mut slot = self.current.write();
        slot.replace(epoch)
    }

    /// Whether anything is hydrated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.current.read().is_none()
    }

    /// The current epoch, or a typed refusal naming what is missing.
    ///
    /// `FR-GRAPH-20` requires a graph request during rehydration to fail with a named
    /// error rather than a generic one or a hang, because the caller's correct response ---
    /// retry shortly --- differs from their response to a real failure.
    pub fn require(&self) -> Result<EpochRef, NotHydrated> {
        self.current().ok_or(NotHydrated)
    }
}

/// No epoch is published for this graph yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NotHydrated;

impl std::fmt::Display for NotHydrated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "no graph epoch is published yet: the graph is building or has not been \
             hydrated. This is a wait-and-retry condition, not a failure of the query",
        )
    }
}

impl std::error::Error for NotHydrated {}
