//! Building an epoch from Arrow batches, under a memory budget.
//!
//! Hydration is one linear pass over the scan. Nothing is copied that can be borrowed and
//! nothing is materialised twice: keys are interned as they arrive, edges accumulate in a
//! flat vector, and the compressed structure is produced once at the end.
//!
//! # The budget is enforced while building, not after
//!
//! `FR-GRAPH-19` requires a hydration exceeding its declared budget to fail *at build time
//! with a clear error*, never at query time with an allocation failure. The difference is
//! not cosmetic. An allocation failure in managed mode takes the database down with it, so
//! a graph that turns out to be too large must be refused while it is still just a build
//! job --- and the refusal must say how large it was getting, or nobody can size the budget.
//!
//! The check therefore runs as edges accumulate rather than on the finished structure. By
//! the time the finished structure exists, the memory has already been taken.

use crate::epoch::{Epoch, EpochId};
use crate::spec::{EdgeSpec, GraphSpec};
use arrow_array::cast::AsArray;
use arrow_array::{Array, RecordBatch};
use sankhya_graph_algo::csr::{AdjacencyBuilder, Edge, Validity};
use sankhya_graph_algo::ids::{EdgeType, Interner, VertexType};
use std::collections::BTreeMap;

/// How much memory a hydration may use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryBudget {
    /// The ceiling, in bytes.
    pub max_bytes: usize,
}

impl MemoryBudget {
    /// A budget of this many bytes.
    #[must_use]
    pub const fn of(max_bytes: usize) -> Self {
        Self { max_bytes }
    }

    /// A budget large enough not to bind in a test.
    #[must_use]
    pub const fn generous() -> Self {
        Self {
            max_bytes: usize::MAX,
        }
    }
}

/// Accumulates rows into an epoch.
///
/// Held across many batches: a scan arrives in pieces and the interner has to persist
/// between them, or the same key in two batches becomes two vertices.
#[derive(Debug)]
pub struct Hydration {
    spec: GraphSpec,
    budget: MemoryBudget,
    interner: Interner,
    edges: Vec<Edge>,
    edge_type_ids: BTreeMap<String, u16>,
    vertex_type_ids: BTreeMap<String, u16>,
    rows_read: usize,
    rows_skipped: usize,
}

impl Hydration {
    /// Begin a hydration.
    #[must_use]
    pub fn new(spec: GraphSpec, budget: MemoryBudget) -> Self {
        let edge_type_ids = spec.edge_type_ids();
        let vertex_type_ids = spec.vertex_type_ids();
        Self {
            spec,
            budget,
            interner: Interner::new(),
            edges: Vec::new(),
            edge_type_ids,
            vertex_type_ids,
            rows_read: 0,
            rows_skipped: 0,
        }
    }

    /// Absorb one batch, applying every edge spec that its schema satisfies.
    ///
    /// A batch that satisfies no spec contributes nothing and is not an error: a scan may
    /// legitimately deliver several tables, and each spec reads the ones it recognises.
    pub fn absorb(&mut self, batch: &RecordBatch) -> Result<(), HydrationError> {
        for spec in self.spec.edges().to_vec() {
            if batch.column_by_name(&spec.source_column).is_none()
                || batch.column_by_name(&spec.target_column).is_none()
            {
                continue;
            }
            self.absorb_one(batch, &spec)?;
        }
        Ok(())
    }

    fn absorb_one(&mut self, batch: &RecordBatch, spec: &EdgeSpec) -> Result<(), HydrationError> {
        let Some(sources) = batch.column_by_name(&spec.source_column) else {
            return Ok(());
        };
        let Some(targets) = batch.column_by_name(&spec.target_column) else {
            return Ok(());
        };
        let valid_from = spec
            .valid_from_column
            .as_ref()
            .and_then(|c| batch.column_by_name(c));
        let valid_until = spec
            .valid_until_column
            .as_ref()
            .and_then(|c| batch.column_by_name(c));
        let weights = spec
            .weight_column
            .as_ref()
            .and_then(|c| batch.column_by_name(c));

        let source_type = VertexType(
            self.vertex_type_ids
                .get(&spec.source_type)
                .copied()
                .unwrap_or(0),
        );
        let target_type = VertexType(
            self.vertex_type_ids
                .get(&spec.target_type)
                .copied()
                .unwrap_or(0),
        );
        let edge_type = EdgeType(
            self.edge_type_ids
                .get(&spec.edge_type)
                .copied()
                .unwrap_or(0),
        );

        self.edges.reserve(batch.num_rows());
        for row in 0..batch.num_rows() {
            self.rows_read = self.rows_read.saturating_add(1);

            // A null endpoint is not an edge. Skipping is right and silence is not: a scan
            // whose join produced nulls would otherwise hydrate a graph quietly missing
            // most of its edges, and nothing would look wrong.
            let (Some(source_key), Some(target_key)) =
                (key_at(sources.as_ref(), row), key_at(targets.as_ref(), row))
            else {
                self.rows_skipped = self.rows_skipped.saturating_add(1);
                continue;
            };

            let source = self
                .interner
                .intern(&source_key, source_type)
                .map_err(HydrationError::TypeConflict)?;
            let target = self
                .interner
                .intern(&target_key, target_type)
                .map_err(HydrationError::TypeConflict)?;

            let from = valid_from
                .and_then(|a| instant_at(a.as_ref(), row))
                .unwrap_or(i64::MIN);
            let until = valid_until
                .and_then(|a| instant_at(a.as_ref(), row))
                .unwrap_or(i64::MAX);
            let weight = weights
                .and_then(|a| number_at(a.as_ref(), row))
                .unwrap_or(1.0);

            self.edges.push(Edge {
                source,
                target,
                edge_type,
                validity: Validity { from, until },
                weight,
            });

            // Checked as we go. By the time the finished structure exists the memory has
            // already been taken, and refusing then is refusing after the damage.
            if self.estimated_bytes() > self.budget.max_bytes {
                return Err(HydrationError::OverBudget {
                    budget_bytes: self.budget.max_bytes,
                    reached_bytes: self.estimated_bytes(),
                    edges_so_far: self.edges.len(),
                    vertices_so_far: self.interner.len(),
                });
            }
        }
        Ok(())
    }

    /// Roughly how much memory the finished epoch will need.
    ///
    /// The edge vector is counted twice over: the compressed structure holds each edge once
    /// forwards and once in the reverse index, and the flat vector is still alive while
    /// both are built.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        self.edges
            .len()
            .saturating_mul(std::mem::size_of::<Edge>().saturating_mul(2))
            .saturating_add(self.interner.heap_bytes())
    }

    /// How many rows were read.
    #[must_use]
    pub const fn rows_read(&self) -> usize {
        self.rows_read
    }

    /// How many rows contributed no edge because an endpoint was null.
    ///
    /// Reported rather than logged. A scan whose join produced nulls hydrates a graph
    /// quietly missing most of its edges, and a count is the only thing that reveals it.
    #[must_use]
    pub const fn rows_skipped(&self) -> usize {
        self.rows_skipped
    }

    /// Freeze into an epoch.
    pub fn finish(
        self,
        id: EpochId,
        snapshot: u64,
        built_at: i64,
    ) -> Result<Epoch, HydrationError> {
        let vertex_count = self.interner.len();
        let mut builder = AdjacencyBuilder::new(vertex_count);
        builder.reserve(self.edges.len());
        for edge in &self.edges {
            builder
                .push(*edge)
                .map_err(|e| HydrationError::OutOfRange(e.to_string()))?;
        }
        let adjacency = builder.build();

        let epoch = Epoch::new(id, snapshot, built_at, self.spec, adjacency, self.interner);
        if epoch.heap_bytes() > self.budget.max_bytes {
            return Err(HydrationError::OverBudget {
                budget_bytes: self.budget.max_bytes,
                reached_bytes: epoch.heap_bytes(),
                edges_so_far: epoch.edge_count(),
                vertices_so_far: epoch.vertex_count(),
            });
        }
        Ok(epoch)
    }
}

/// Read a key as bytes, whichever Arrow type carries it.
///
/// Integer keys are rendered big-endian so that the byte comparison an interner does agrees
/// with the numeric one. Little-endian would make key ordering depend on the machine.
fn key_at(array: &dyn Array, row: usize) -> Option<Vec<u8>> {
    if array.is_null(row) {
        return None;
    }
    use arrow_schema::DataType;
    match array.data_type() {
        DataType::Utf8 => Some(array.as_string::<i32>().value(row).as_bytes().to_vec()),
        DataType::LargeUtf8 => Some(array.as_string::<i64>().value(row).as_bytes().to_vec()),
        DataType::Binary => Some(array.as_binary::<i32>().value(row).to_vec()),
        DataType::Int32 => Some(
            array
                .as_primitive::<arrow_array::types::Int32Type>()
                .value(row)
                .to_be_bytes()
                .to_vec(),
        ),
        DataType::Int64 => Some(
            array
                .as_primitive::<arrow_array::types::Int64Type>()
                .value(row)
                .to_be_bytes()
                .to_vec(),
        ),
        DataType::UInt64 => Some(
            array
                .as_primitive::<arrow_array::types::UInt64Type>()
                .value(row)
                .to_be_bytes()
                .to_vec(),
        ),
        _ => None,
    }
}

/// Read an instant, whichever integral or timestamp type carries it.
fn instant_at(array: &dyn Array, row: usize) -> Option<i64> {
    if array.is_null(row) {
        return None;
    }
    use arrow_array::types;
    use arrow_schema::DataType;
    match array.data_type() {
        DataType::Int64 => Some(array.as_primitive::<types::Int64Type>().value(row)),
        DataType::Int32 => Some(i64::from(
            array.as_primitive::<types::Int32Type>().value(row),
        )),
        DataType::Timestamp(arrow_schema::TimeUnit::Microsecond, _) => Some(
            array
                .as_primitive::<types::TimestampMicrosecondType>()
                .value(row),
        ),
        DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, _) => Some(
            array
                .as_primitive::<types::TimestampMillisecondType>()
                .value(row),
        ),
        DataType::Timestamp(arrow_schema::TimeUnit::Nanosecond, _) => Some(
            array
                .as_primitive::<types::TimestampNanosecondType>()
                .value(row),
        ),
        DataType::Timestamp(arrow_schema::TimeUnit::Second, _) => Some(
            array
                .as_primitive::<types::TimestampSecondType>()
                .value(row),
        ),
        DataType::Date32 => Some(i64::from(
            array.as_primitive::<types::Date32Type>().value(row),
        )),
        _ => None,
    }
}

/// Read a weight, whichever numeric type carries it.
fn number_at(array: &dyn Array, row: usize) -> Option<f64> {
    if array.is_null(row) {
        return None;
    }
    use arrow_array::types;
    use arrow_schema::DataType;
    match array.data_type() {
        DataType::Float64 => Some(array.as_primitive::<types::Float64Type>().value(row)),
        #[allow(clippy::cast_lossless)]
        DataType::Float32 => Some(f64::from(
            array.as_primitive::<types::Float32Type>().value(row),
        )),
        #[allow(clippy::cast_precision_loss)]
        DataType::Int64 => Some(array.as_primitive::<types::Int64Type>().value(row) as f64),
        DataType::Int32 => Some(f64::from(
            array.as_primitive::<types::Int32Type>().value(row),
        )),
        _ => None,
    }
}

/// Why a hydration could not finish.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HydrationError {
    /// The graph would not fit in its declared budget.
    OverBudget {
        /// What it was allowed.
        budget_bytes: usize,
        /// What it had reached when the build stopped.
        reached_bytes: usize,
        /// How many edges had been absorbed.
        edges_so_far: usize,
        /// How many vertices had been interned.
        vertices_so_far: usize,
    },
    /// One key claimed two vertex types.
    TypeConflict(sankhya_graph_algo::ids::TypeConflict),
    /// An edge named a vertex outside the interned set. Should not happen.
    OutOfRange(String),
}

impl std::fmt::Display for HydrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OverBudget {
                budget_bytes,
                reached_bytes,
                edges_so_far,
                vertices_so_far,
            } => write!(
                f,
                "graph hydration exceeded its memory budget of {budget_bytes} bytes, \
                 reaching {reached_bytes} after {vertices_so_far} vertices and \
                 {edges_so_far} edges. Refused at build time deliberately: an allocation \
                 failure at query time takes the process down, and in managed mode the \
                 database with it"
            ),
            Self::TypeConflict(conflict) => write!(f, "{conflict}"),
            Self::OutOfRange(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for HydrationError {}
