//! Dense internal identifiers, and the mapping from external keys onto them.
//!
//! Traversal is an inner loop over adjacency slices, and the only representation that
//! makes that loop cheap is a dense integer that indexes directly into an array. External
//! keys are whatever the source table happens to carry --- a string, a UUID, a bigint ---
//! and none of those index anything.
//!
//! So hydration assigns each vertex a `VertexId` in `0..n`, and every structure downstream
//! is an array of length `n`. The mapping back to the external key is held once, and
//! consulted only at the edges of a query: when seeds come in, and when results go out.

use std::collections::HashMap;

/// A vertex's position in the adjacency arrays.
///
/// Dense by construction: hydration hands these out sequentially, so `VertexId(k)` is a
/// valid index into every per-vertex array in the same epoch, and into no other epoch's.
/// Mixing ids across epochs is meaningless, which is why an epoch owns its mapping rather
/// than sharing a global one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct VertexId(pub u32);

impl VertexId {
    /// As an array index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A vertex type --- what kind of thing this vertex is.
///
/// Types are per-epoch and dense, like vertices. A heterogeneous network is the normal
/// case rather than the exception, and a single untyped graph cannot represent one without
/// encoding the type into a weight, which then silently participates in arithmetic.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct VertexType(pub u16);

/// An edge type --- what kind of relationship this edge is.
///
/// Each edge type gets its own adjacency segment, so a traversal restricted to one type
/// reads only that type's edges rather than reading everything and filtering. The
/// difference is not a constant factor when one type dominates the edge count.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct EdgeType(pub u16);

/// Which edge types a traversal may follow.
///
/// A mask rather than a single type, because the useful questions are almost always
/// "follow these three kinds of edge and not those two". Empty means *no* edge type, not
/// all of them: a caller that forgot to populate the mask gets an empty result, which is
/// visibly wrong, rather than a full traversal, which looks like an answer.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EdgeMask {
    allowed: Vec<EdgeType>,
}

impl EdgeMask {
    /// A mask permitting exactly the listed types.
    #[must_use]
    pub fn of(types: impl IntoIterator<Item = EdgeType>) -> Self {
        let mut allowed: Vec<EdgeType> = types.into_iter().collect();
        allowed.sort_unstable();
        allowed.dedup();
        Self { allowed }
    }

    /// A mask permitting every type an epoch holds.
    ///
    /// Takes the count rather than being a bare `All` variant, so that the mask is always
    /// an explicit list. `All` would have to mean "every type in whichever epoch this is
    /// applied to", and a mask that changes meaning depending on where it is used is the
    /// kind of thing that silently starts including a new edge type the day one is added.
    #[must_use]
    pub fn all(edge_type_count: u16) -> Self {
        Self {
            allowed: (0..edge_type_count).map(EdgeType).collect(),
        }
    }

    /// Whether this type may be followed.
    #[must_use]
    pub fn permits(&self, edge_type: EdgeType) -> bool {
        self.allowed.binary_search(&edge_type).is_ok()
    }

    /// The permitted types, ascending.
    #[must_use]
    pub fn types(&self) -> &[EdgeType] {
        &self.allowed
    }

    /// Whether the mask permits nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

/// The external key of a vertex, as the source table carries it.
///
/// Held as bytes rather than a string because the source may key on a UUID or a
/// fixed-width integer, and forcing those through a string costs an allocation per vertex
/// on a structure with millions of them.
pub type ExternalKey = Vec<u8>;

/// The two-way mapping between external keys and dense ids.
///
/// Owned by an epoch. Interning is append-only within a build: an id, once handed out,
/// means the same vertex for the lifetime of the epoch.
#[derive(Debug, Default)]
pub struct Interner {
    forward: HashMap<ExternalKey, VertexId>,
    reverse: Vec<ExternalKey>,
    vertex_types: Vec<VertexType>,
}

impl Interner {
    /// An empty mapping.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The id for this key, assigning one if the key is new.
    ///
    /// The type is recorded on first sight only. A key arriving twice with two different
    /// types is a source-data conflict, and this reports it rather than letting the last
    /// writer win --- a vertex whose type depends on scan order would make every
    /// type-filtered traversal non-deterministic.
    pub fn intern(
        &mut self,
        key: &[u8],
        vertex_type: VertexType,
    ) -> Result<VertexId, TypeConflict> {
        if let Some(existing) = self.forward.get(key) {
            let recorded = self.vertex_types.get(existing.index()).copied();
            if recorded != Some(vertex_type) {
                return Err(TypeConflict {
                    key: key.to_vec(),
                    recorded: recorded.unwrap_or(VertexType(u16::MAX)),
                    offered: vertex_type,
                });
            }
            return Ok(*existing);
        }
        let id = VertexId(u32::try_from(self.reverse.len()).unwrap_or(u32::MAX));
        self.forward.insert(key.to_vec(), id);
        self.reverse.push(key.to_vec());
        self.vertex_types.push(vertex_type);
        Ok(id)
    }

    /// The id for this key, if it has one. Does not assign.
    #[must_use]
    pub fn lookup(&self, key: &[u8]) -> Option<VertexId> {
        self.forward.get(key).copied()
    }

    /// The external key behind an id.
    #[must_use]
    pub fn key(&self, id: VertexId) -> Option<&[u8]> {
        self.reverse.get(id.index()).map(Vec::as_slice)
    }

    /// The type of a vertex.
    #[must_use]
    pub fn vertex_type(&self, id: VertexId) -> Option<VertexType> {
        self.vertex_types.get(id.index()).copied()
    }

    /// How many vertices have been interned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    /// Whether nothing has been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty()
    }

    /// Roughly how many bytes this mapping occupies.
    ///
    /// Approximate on purpose: it counts the key bytes and the per-vertex overhead, and
    /// does not attempt to model the hash table's load factor. The budget it feeds is a
    /// guard against building something that will not fit, not an accounting record.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let keys: usize = self.reverse.iter().map(Vec::len).sum();
        // Each key is stored twice --- once in each direction --- plus a `VertexId` and a
        // `VertexType` per vertex, plus the `Vec` headers on both copies.
        keys.saturating_mul(2)
            + self
                .reverse
                .len()
                .saturating_mul(2 * std::mem::size_of::<ExternalKey>() + 6)
    }
}

/// One external key claimed two different vertex types.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeConflict {
    /// The key that arrived twice.
    pub key: ExternalKey,
    /// The type recorded when it was first seen.
    pub recorded: VertexType,
    /// The type offered the second time.
    pub offered: VertexType,
}

impl std::fmt::Display for TypeConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "vertex key {:?} was first seen as type {} and is now offered as type {}; a \
             vertex whose type depends on scan order makes every type-filtered traversal \
             non-deterministic",
            String::from_utf8_lossy(&self.key),
            self.recorded.0,
            self.offered.0
        )
    }
}

impl std::error::Error for TypeConflict {}
