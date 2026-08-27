//! Compressed sparse row adjacency, one segment per edge type.
//!
//! The layout answers one question cheaply, because every traversal asks it millions of
//! times: *given a vertex, an edge type and a time, which edges leave it after that time?*
//!
//! Within a segment, edges are sorted by source and then by the instant the edge becomes
//! valid. `offsets[v]..offsets[v + 1]` is the contiguous run belonging to `v`, and because
//! that run is itself time-ordered, "after `t`" is a binary search within it followed by a
//! slice to the end. No filtering, no allocation, no per-edge branch.
//!
//! The reverse index is a second set of segments built on the target, so that asking which
//! edges *arrive* at a vertex is the same operation rather than a scan of everything.

use crate::ids::{EdgeMask, EdgeType, VertexId};

/// A half-open interval during which an edge exists.
///
/// Half-open --- `[from, until)` --- so that an edge ending exactly when another begins
/// does not produce a moment where both are live. Closed intervals make that a
/// one-instant overlap, and a time-respecting traversal will happily hop through it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Validity {
    /// When the edge starts existing.
    pub from: i64,
    /// When it stops, or `i64::MAX` for an edge that has not ended.
    pub until: i64,
}

impl Validity {
    /// An edge that exists from `from` onwards and has not ended.
    #[must_use]
    pub const fn from(from: i64) -> Self {
        Self {
            from,
            until: i64::MAX,
        }
    }

    /// An edge live for the whole of representable time.
    #[must_use]
    pub const fn always() -> Self {
        Self {
            from: i64::MIN,
            until: i64::MAX,
        }
    }

    /// Whether the edge is live at this instant.
    #[must_use]
    pub const fn contains(self, at: i64) -> bool {
        self.from <= at && at < self.until
    }

    /// Whether the edge is live at any point within `[from, until)`.
    #[must_use]
    pub const fn overlaps(self, from: i64, until: i64) -> bool {
        self.from < until && from < self.until
    }
}

/// One edge, as the builder receives it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Edge {
    /// Where it leaves from.
    pub source: VertexId,
    /// Where it arrives.
    pub target: VertexId,
    /// What kind of relationship it is.
    pub edge_type: EdgeType,
    /// When it exists.
    pub validity: Validity,
    /// The edge's cost or capacity, as the caller defines it.
    pub weight: f64,
}

/// One edge as traversal sees it: everything except the source, which is implied by the
/// slice it came from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Arc {
    /// The vertex at the other end.
    pub target: VertexId,
    /// When this edge exists.
    pub validity: Validity,
    /// The edge's cost or capacity.
    pub weight: f64,
}

/// The adjacency for a single edge type.
///
/// Held as parallel arrays rather than an array of structs. Traversal that ignores time
/// touches only `targets`, and keeping it contiguous means a cache line carries sixteen
/// candidate vertices instead of four.
#[derive(Clone, Debug)]
struct Segment {
    /// `vertex_count + 1` entries; `offsets[v]..offsets[v + 1]` is `v`'s run.
    offsets: Vec<u32>,
    targets: Vec<VertexId>,
    validity: Vec<Validity>,
    weights: Vec<f64>,
}

impl Segment {
    /// The half-open range of `vertex`'s edges within the parallel arrays.
    ///
    /// Returns an empty range for a vertex out of bounds rather than refusing: callers are
    /// traversal inner loops, and a vertex with no edges and a vertex that does not exist
    /// are the same thing to them. A malformed `offsets` --- end before start --- also
    /// yields empty, because a traversal that silently reads a reversed slice is worse
    /// than one that finds nothing.
    fn run(&self, vertex: VertexId) -> (usize, usize) {
        let start = self.offsets.get(vertex.index()).copied().unwrap_or(0) as usize;
        let end = self
            .offsets
            .get(vertex.index().saturating_add(1))
            .copied()
            .unwrap_or(0) as usize;
        if end < start || end > self.targets.len() {
            return (0, 0);
        }
        (start, end)
    }

    fn heap_bytes(&self) -> usize {
        self.offsets.len() * std::mem::size_of::<u32>()
            + self.targets.len() * std::mem::size_of::<VertexId>()
            + self.validity.len() * std::mem::size_of::<Validity>()
            + self.weights.len() * std::mem::size_of::<f64>()
    }
}

/// The adjacency structure a traversal reads.
///
/// Immutable once built. Everything a query does is a borrow of these arrays --- there is
/// no copying of adjacency into a per-query structure, which is what makes an epoch
/// shareable by every concurrent reader without a lock.
#[derive(Clone, Debug)]
pub struct Adjacency {
    vertex_count: usize,
    /// The smallest edge weight anywhere in the structure.
    ///
    /// Recorded at build time because the routines that care cannot detect it reliably at
    /// query time. Dijkstra settles a vertex and moves on; whether it ever *relaxes* the
    /// negative edge depends on the costs it happens to meet first, so a check during
    /// relaxation fires or does not fire depending on the query. A graph either has a
    /// negative edge or it does not, and that is knowable once.
    min_weight: f64,
    /// Indexed by `EdgeType.0`. A type with no edges still gets a segment, so that
    /// indexing by type never has to distinguish absent from empty.
    outgoing: Vec<Segment>,
    incoming: Vec<Segment>,
}

impl Adjacency {
    /// How many vertices the structure covers.
    #[must_use]
    pub const fn vertex_count(&self) -> usize {
        self.vertex_count
    }

    /// Whether any edge carries a negative weight.
    ///
    /// Routines assuming non-negative weights consult this instead of watching for one as
    /// they go, because watching for one is not reliable: whether the offending edge is
    /// ever examined depends on the query.
    #[must_use]
    pub fn has_negative_weight(&self) -> bool {
        self.min_weight < 0.0
    }

    /// How many edge types it distinguishes.
    #[must_use]
    pub fn edge_type_count(&self) -> u16 {
        u16::try_from(self.outgoing.len()).unwrap_or(u16::MAX)
    }

    /// How many edges it holds, across every type.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.outgoing.iter().map(|s| s.targets.len()).sum()
    }

    /// A mask permitting every type this structure holds.
    #[must_use]
    pub fn all_edge_types(&self) -> EdgeMask {
        EdgeMask::all(self.edge_type_count())
    }

    /// Edges leaving `vertex` by `edge_type`, in ascending order of validity start.
    ///
    /// The returned slices are parallel and always the same length.
    #[must_use]
    pub fn out_edges(&self, vertex: VertexId, edge_type: EdgeType) -> Neighbours<'_> {
        Self::slice(&self.outgoing, vertex, edge_type)
    }

    /// Edges arriving at `vertex` by `edge_type`.
    #[must_use]
    pub fn in_edges(&self, vertex: VertexId, edge_type: EdgeType) -> Neighbours<'_> {
        Self::slice(&self.incoming, vertex, edge_type)
    }

    fn slice(segments: &[Segment], vertex: VertexId, edge_type: EdgeType) -> Neighbours<'_> {
        let Some(segment) = segments.get(edge_type.0 as usize) else {
            return Neighbours::EMPTY;
        };
        let (start, end) = segment.run(vertex);
        Neighbours {
            targets: segment.targets.get(start..end).unwrap_or(&[]),
            validity: segment.validity.get(start..end).unwrap_or(&[]),
            weights: segment.weights.get(start..end).unwrap_or(&[]),
        }
    }

    /// How many edges leave `vertex`, across every type the mask permits.
    ///
    /// Traversal uses this to apply a degree cap before expanding rather than after. In a
    /// power-law network a handful of vertices have enormous degree, and expanding one
    /// touches most of the graph to return paths that mean nothing.
    #[must_use]
    pub fn out_degree(&self, vertex: VertexId, mask: &EdgeMask) -> usize {
        mask.types()
            .iter()
            .map(|t| self.out_edges(vertex, *t).len())
            .sum()
    }

    /// How many edges arrive at `vertex`, across every type the mask permits.
    #[must_use]
    pub fn in_degree(&self, vertex: VertexId, mask: &EdgeMask) -> usize {
        mask.types()
            .iter()
            .map(|t| self.in_edges(vertex, *t).len())
            .sum()
    }

    /// Roughly how many bytes the adjacency occupies.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.outgoing
            .iter()
            .chain(&self.incoming)
            .map(Segment::heap_bytes)
            .sum()
    }
}

/// A borrowed view of one vertex's edges of one type.
///
/// The three slices are parallel: index `i` in each describes the same edge.
#[derive(Clone, Copy, Debug)]
pub struct Neighbours<'a> {
    targets: &'a [VertexId],
    validity: &'a [Validity],
    weights: &'a [f64],
}

impl<'a> Neighbours<'a> {
    const EMPTY: Self = Self {
        targets: &[],
        validity: &[],
        weights: &[],
    };

    /// How many edges.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.targets.len()
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// The vertices at the far end, without validity or weight.
    ///
    /// The cheapest thing traversal can ask for, and what an untimed reachability query
    /// wants: a contiguous run of `u32`, nothing else touched.
    #[must_use]
    pub const fn targets(&self) -> &'a [VertexId] {
        self.targets
    }

    /// Every edge, as a struct.
    pub fn iter(&self) -> impl Iterator<Item = Arc> + 'a {
        let validity = self.validity;
        let weights = self.weights;
        self.targets.iter().enumerate().map(move |(i, target)| Arc {
            target: *target,
            validity: validity.get(i).copied().unwrap_or(Validity::always()),
            weight: weights.get(i).copied().unwrap_or(1.0),
        })
    }

    /// Only the edges live at `at`.
    ///
    /// Filters rather than slices: an edge's *end* is not ordered, so the live set at an
    /// instant is not contiguous even though the starts are sorted. The `from` bound is
    /// still used to stop early.
    pub fn live_at(&self, at: i64) -> impl Iterator<Item = Arc> + 'a {
        self.starting_by(at)
            .filter(move |arc| at < arc.validity.until)
    }

    /// Edges whose validity begins at or before `at`, in order.
    ///
    /// This *is* a slice, because starts are sorted: a binary search for the first start
    /// after `at` bounds the run. This is the operation `FR-GRAPH-04` exists for.
    pub fn starting_by(&self, at: i64) -> impl Iterator<Item = Arc> + 'a {
        let end = self.validity.partition_point(|v| v.from <= at);
        self.take(end)
    }

    /// Edges whose validity begins at or after `at`, in order.
    ///
    /// The forward half of the same binary search, and the one a time-respecting traversal
    /// wants: having arrived at a vertex at time `t`, the edges it may take next are
    /// exactly those starting at `t` or later.
    pub fn starting_from(&self, at: i64) -> impl Iterator<Item = Arc> + 'a {
        let start = self.validity.partition_point(|v| v.from < at);
        let (targets, validity, weights) = (self.targets, self.validity, self.weights);
        (start..self.len()).map(move |i| Arc {
            target: targets.get(i).copied().unwrap_or(VertexId(0)),
            validity: validity.get(i).copied().unwrap_or(Validity::always()),
            weight: weights.get(i).copied().unwrap_or(1.0),
        })
    }

    fn take(&self, end: usize) -> impl Iterator<Item = Arc> + 'a {
        let (targets, validity, weights) = (self.targets, self.validity, self.weights);
        (0..end.min(targets.len())).map(move |i| Arc {
            target: targets.get(i).copied().unwrap_or(VertexId(0)),
            validity: validity.get(i).copied().unwrap_or(Validity::always()),
            weight: weights.get(i).copied().unwrap_or(1.0),
        })
    }
}

/// Accumulates edges and produces an [`Adjacency`].
///
/// Deliberately a separate type. The built structure has no mutating operation at all, so
/// there is no way for a query to modify an epoch it is reading --- the property that lets
/// every reader share one without a lock.
#[derive(Debug, Default)]
pub struct AdjacencyBuilder {
    edges: Vec<Edge>,
    vertex_count: usize,
    edge_type_count: u16,
}

impl AdjacencyBuilder {
    /// A builder for a graph of `vertex_count` vertices.
    #[must_use]
    pub fn new(vertex_count: usize) -> Self {
        Self {
            edges: Vec::new(),
            vertex_count,
            edge_type_count: 0,
        }
    }

    /// Reserve space for `n` further edges.
    ///
    /// Hydration knows the row count before it starts, and a single reservation is the
    /// difference between one allocation and twenty on a large graph.
    pub fn reserve(&mut self, n: usize) {
        self.edges.reserve(n);
    }

    /// Add one edge.
    ///
    /// Out-of-range endpoints are refused here rather than at traversal time. An edge to a
    /// vertex that does not exist would otherwise be silently unreachable, and the graph
    /// would answer "not connected" for a pair the source data says is connected.
    pub fn push(&mut self, edge: Edge) -> Result<(), OutOfRange> {
        if edge.source.index() >= self.vertex_count {
            return Err(OutOfRange {
                vertex: edge.source,
                vertex_count: self.vertex_count,
            });
        }
        if edge.target.index() >= self.vertex_count {
            return Err(OutOfRange {
                vertex: edge.target,
                vertex_count: self.vertex_count,
            });
        }
        self.edge_type_count = self.edge_type_count.max(edge.edge_type.0.saturating_add(1));
        self.edges.push(edge);
        Ok(())
    }

    /// How many edges have been offered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Whether none have.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Sort into segments and freeze.
    ///
    /// One pass to count, one to place. Sorting is by `(type, endpoint, validity.from)`,
    /// which is what makes both the per-vertex run contiguous and the run itself
    /// time-ordered.
    #[must_use]
    pub fn build(mut self) -> Adjacency {
        let types = self.edge_type_count as usize;
        let mut outgoing = Vec::with_capacity(types);
        let mut incoming = Vec::with_capacity(types);

        for t in 0..self.edge_type_count {
            outgoing.push(Self::segment(
                &mut self.edges,
                self.vertex_count,
                EdgeType(t),
                true,
            ));
            incoming.push(Self::segment(
                &mut self.edges,
                self.vertex_count,
                EdgeType(t),
                false,
            ));
        }

        let min_weight = self
            .edges
            .iter()
            .map(|e| e.weight)
            .fold(f64::INFINITY, f64::min);

        Adjacency {
            vertex_count: self.vertex_count,
            min_weight,
            outgoing,
            incoming,
        }
    }

    /// Build one direction of one type's segment.
    ///
    /// `forward` selects which endpoint keys the segment: the source for out-edges, the
    /// target for the reverse index.
    fn segment(
        edges: &mut [Edge],
        vertex_count: usize,
        edge_type: EdgeType,
        forward: bool,
    ) -> Segment {
        let key = |e: &Edge| if forward { e.source } else { e.target };
        let far = |e: &Edge| if forward { e.target } else { e.source };

        let mut counts = vec![0u32; vertex_count.saturating_add(1)];
        let mut total = 0usize;
        for edge in edges.iter().filter(|e| e.edge_type == edge_type) {
            if let Some(slot) = counts.get_mut(key(edge).index()) {
                *slot = slot.saturating_add(1);
                total += 1;
            }
        }

        // Prefix sum in place: `counts[v]` becomes the start of `v`'s run.
        let mut offsets = vec![0u32; vertex_count.saturating_add(1)];
        let mut running = 0u32;
        for v in 0..vertex_count {
            if let Some(slot) = offsets.get_mut(v) {
                *slot = running;
            }
            running = running.saturating_add(counts.get(v).copied().unwrap_or(0));
        }
        if let Some(last) = offsets.get_mut(vertex_count) {
            *last = running;
        }

        let mut targets = vec![VertexId(0); total];
        let mut validity = vec![Validity::always(); total];
        let mut weights = vec![0.0f64; total];

        // A cursor per vertex, walked as edges are placed.
        let mut cursor = offsets.clone();
        for edge in edges.iter().filter(|e| e.edge_type == edge_type) {
            let v = key(edge).index();
            let Some(at) = cursor.get_mut(v) else {
                continue;
            };
            let slot = *at as usize;
            *at = at.saturating_add(1);
            if let Some(t) = targets.get_mut(slot) {
                *t = far(edge);
            }
            if let Some(w) = validity.get_mut(slot) {
                *w = edge.validity;
            }
            if let Some(w) = weights.get_mut(slot) {
                *w = edge.weight;
            }
        }

        // Each run is now grouped but arbitrarily ordered within. Sorting each run
        // independently is what `FR-GRAPH-04` requires, and doing it per run rather than
        // globally keeps it O(sum of d log d) instead of O(E log E).
        for v in 0..vertex_count {
            let start = offsets.get(v).copied().unwrap_or(0) as usize;
            let end = offsets.get(v.saturating_add(1)).copied().unwrap_or(0) as usize;
            if end <= start.saturating_add(1) || end > total {
                continue;
            }
            let mut run: Vec<(VertexId, Validity, f64)> = (start..end)
                .map(|i| {
                    (
                        targets.get(i).copied().unwrap_or(VertexId(0)),
                        validity.get(i).copied().unwrap_or(Validity::always()),
                        weights.get(i).copied().unwrap_or(0.0),
                    )
                })
                .collect();
            // By start instant, then by target, so the order is total and a rebuild of the
            // same edges produces byte-identical arrays. The incremental-equals-full
            // property test depends on that determinism.
            run.sort_by(|a, b| a.1.from.cmp(&b.1.from).then(a.0.cmp(&b.0)));
            for (offset, (t, val, w)) in run.into_iter().enumerate() {
                let i = start.saturating_add(offset);
                if let Some(slot) = targets.get_mut(i) {
                    *slot = t;
                }
                if let Some(slot) = validity.get_mut(i) {
                    *slot = val;
                }
                if let Some(slot) = weights.get_mut(i) {
                    *slot = w;
                }
            }
        }

        Segment {
            offsets,
            targets,
            validity,
            weights,
        }
    }
}

/// An edge named a vertex the graph does not have.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutOfRange {
    /// The offending endpoint.
    pub vertex: VertexId,
    /// How many vertices the graph holds.
    pub vertex_count: usize,
}

impl std::fmt::Display for OutOfRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "edge names vertex {} in a graph of {} vertices; an edge to a vertex that does \
             not exist would be silently unreachable, and the graph would report \
             'not connected' for a pair the source data connects",
            self.vertex.0, self.vertex_count
        )
    }
}

impl std::error::Error for OutOfRange {}
