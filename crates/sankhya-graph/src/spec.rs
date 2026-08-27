//! How a table's columns become vertices and edges.
//!
//! Hydration reads Arrow batches produced by an ordinary published scan. Nothing about
//! those batches is graph-shaped: they are rows with columns, and the spec is what says
//! which column is a source, which a target, and which carries the instant the
//! relationship came into existence.
//!
//! Keeping this a value rather than a trait matters. A spec can be built from a query, sent
//! across a wire, compared for equality and recorded alongside an epoch --- so an epoch can
//! say not just what it holds but *how it was derived*, which is the difference between a
//! graph you can audit and one you have to take on faith.

use std::collections::BTreeMap;

/// Which column supplies which part of an edge.
///
/// Columns are named rather than positional. A positional spec silently binds to the wrong
/// column the day someone reorders a projection, and the resulting graph is wrong rather
/// than absent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EdgeSpec {
    /// The column holding the edge's origin key.
    pub source_column: String,
    /// The column holding its destination key.
    pub target_column: String,
    /// What kind of vertex the source is.
    pub source_type: String,
    /// What kind of vertex the target is.
    pub target_type: String,
    /// What kind of relationship this is.
    pub edge_type: String,
    /// The column holding the instant the edge becomes valid, if any.
    ///
    /// Absent means the edge is treated as always valid --- which disables every temporal
    /// guarantee for it. A graph mixing timed and untimed edges will answer a
    /// time-respecting query using the untimed ones freely, so this being optional is a
    /// deliberate risk rather than a convenience.
    pub valid_from_column: Option<String>,
    /// The column holding the instant it stops being valid, if any.
    pub valid_until_column: Option<String>,
    /// The column holding the edge's weight, if any. Absent means one.
    pub weight_column: Option<String>,
}

impl EdgeSpec {
    /// A spec for an untimed, unweighted edge between two columns.
    #[must_use]
    pub fn new(
        source_column: impl Into<String>,
        target_column: impl Into<String>,
        edge_type: impl Into<String>,
    ) -> Self {
        Self {
            source_column: source_column.into(),
            target_column: target_column.into(),
            source_type: "vertex".to_string(),
            target_type: "vertex".to_string(),
            edge_type: edge_type.into(),
            valid_from_column: None,
            valid_until_column: None,
            weight_column: None,
        }
    }

    /// The same spec, with the endpoints typed.
    #[must_use]
    pub fn between(
        mut self,
        source_type: impl Into<String>,
        target_type: impl Into<String>,
    ) -> Self {
        self.source_type = source_type.into();
        self.target_type = target_type.into();
        self
    }

    /// The same spec, reading validity from a column.
    #[must_use]
    pub fn valid_from(mut self, column: impl Into<String>) -> Self {
        self.valid_from_column = Some(column.into());
        self
    }

    /// The same spec, reading the end of validity from a column.
    #[must_use]
    pub fn valid_until(mut self, column: impl Into<String>) -> Self {
        self.valid_until_column = Some(column.into());
        self
    }

    /// The same spec, reading weight from a column.
    #[must_use]
    pub fn weighted_by(mut self, column: impl Into<String>) -> Self {
        self.weight_column = Some(column.into());
        self
    }

    /// Whether this edge carries time.
    ///
    /// Consulted by the epoch so it can report whether a time-respecting query over it
    /// means anything. An epoch built entirely from untimed edges will answer such a query
    /// with a static walk, and must say so.
    #[must_use]
    pub const fn is_temporal(&self) -> bool {
        self.valid_from_column.is_some()
    }
}

/// Every edge kind an epoch is built from.
///
/// Several specs may read from the same table --- a row frequently describes more than one
/// relationship --- and several may read from different tables entirely.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct GraphSpec {
    edges: Vec<EdgeSpec>,
}

impl GraphSpec {
    /// An empty spec.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an edge kind.
    #[must_use]
    pub fn with(mut self, edge: EdgeSpec) -> Self {
        self.edges.push(edge);
        self
    }

    /// Every edge kind, in declaration order.
    #[must_use]
    pub fn edges(&self) -> &[EdgeSpec] {
        &self.edges
    }

    /// Whether any declared edge kind is untimed.
    ///
    /// A single untimed edge kind is enough to make a time-respecting traversal misleading,
    /// because the traversal may route through it without constraint. The epoch reports
    /// this so a caller is not left to work it out from the spec.
    #[must_use]
    pub fn has_untimed_edges(&self) -> bool {
        self.edges.iter().any(|e| !e.is_temporal())
    }

    /// The distinct vertex type names, in a stable order.
    #[must_use]
    pub fn vertex_types(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .edges
            .iter()
            .flat_map(|e| [e.source_type.clone(), e.target_type.clone()])
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The distinct edge type names, in a stable order.
    #[must_use]
    pub fn edge_types(&self) -> Vec<String> {
        let mut names: Vec<String> = self.edges.iter().map(|e| e.edge_type.clone()).collect();
        names.sort();
        names.dedup();
        names
    }

    /// A map from each type name to the dense identifier hydration will assign it.
    ///
    /// Derived from the sorted names rather than from encounter order, so two hydrations of
    /// the same spec assign the same identifiers even if the data arrives differently. The
    /// incremental-equals-full property depends on it.
    #[must_use]
    pub fn edge_type_ids(&self) -> BTreeMap<String, u16> {
        self.edge_types()
            .into_iter()
            .enumerate()
            .map(|(index, name)| (name, u16::try_from(index).unwrap_or(u16::MAX)))
            .collect()
    }

    /// The same, for vertex types.
    #[must_use]
    pub fn vertex_type_ids(&self) -> BTreeMap<String, u16> {
        self.vertex_types()
            .into_iter()
            .enumerate()
            .map(|(index, name)| (name, u16::try_from(index).unwrap_or(u16::MAX)))
            .collect()
    }
}
