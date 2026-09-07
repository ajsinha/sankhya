//! The table functions themselves.
//!
//! Each one resolves a named graph, reads its bounds from the call, runs a bounded
//! traversal, and returns the rows with the epoch's identity and the truncation attached to
//! every one of them.
//!
//! The columns common to every function are deliberate. `epoch` and `snapshot` let a graph
//! result be reconciled with a relational one; `truncated` and `truncation_reason` make it
//! impossible to project away the fact that the search stopped early. Both would be tidier
//! as query metadata, and both would then be lost by the first `SELECT` that did not
//! mention them.

use crate::args::Arguments;
use crate::catalog::GraphCatalog;
use crate::result::TraversalTable;
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, UInt32Array,
    UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use datafusion::catalog::TableProvider;
use datafusion::common::{plan_datafusion_err, plan_err, Result};
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::Expr;
use sankhya_graph::epoch::EpochRef;
use sankhya_graph_algo::budget::{Bounded, Truncation};
use sankhya_graph_algo::ids::{EdgeMask, VertexId};
use sankhya_graph_algo::{paths, product, traverse};
use std::sync::Arc;

/// Register every graph function against a session.
///
/// One call, so a session either has the whole surface or none of it. A partially
/// registered catalogue means a query works on one node and fails on another.
pub fn register(context: &SessionContext, catalog: Arc<GraphCatalog>) {
    context.register_udtf("graph_reachable", Arc::new(Reachable(Arc::clone(&catalog))));
    context.register_udtf(
        "graph_time_respecting",
        Arc::new(TimeRespecting(Arc::clone(&catalog))),
    );
    context.register_udtf(
        "graph_shortest_path",
        Arc::new(ShortestPath(Arc::clone(&catalog))),
    );
    context.register_udtf("graph_cycles", Arc::new(Cycles(Arc::clone(&catalog))));
    context.register_udtf("graph_influence", Arc::new(Influence(catalog)));
}

/// The columns every graph function carries, whatever else it returns.
fn provenance_fields() -> Vec<Field> {
    vec![
        Field::new("epoch", DataType::UInt64, false),
        Field::new("snapshot", DataType::UInt64, false),
        Field::new("truncated", DataType::Boolean, false),
        Field::new("truncation_reason", DataType::Utf8, true),
    ]
}

/// The provenance columns, filled for `rows` rows.
fn provenance_columns(epoch: &EpochRef, truncation: &Truncation, rows: usize) -> Vec<ArrayRef> {
    let reason = truncation.explain();
    vec![
        Arc::new(UInt64Array::from(vec![epoch.id().0; rows])),
        Arc::new(UInt64Array::from(vec![epoch.snapshot(); rows])),
        Arc::new(BooleanArray::from(vec![!truncation.is_complete(); rows])),
        Arc::new(StringArray::from(vec![reason; rows])),
    ]
}

/// Resolve the graph named in argument one.
fn graph_of(catalog: &GraphCatalog, args: &Arguments) -> Result<EpochRef> {
    let name = args.string_at(0, "graph name")?;
    catalog
        .resolve(&name)
        .map_err(|e| plan_datafusion_err!("{e}"))
}

/// Read the seed keys from argument two, resolving them against the epoch.
///
/// A seed key that is not in the graph is reported rather than skipped. Silently dropping
/// it turns "this entity is not in the graph" into "this entity is connected to nothing",
/// and those are answers a reader will act on differently.
fn seeds_of(epoch: &EpochRef, args: &Arguments) -> Result<Vec<VertexId>> {
    let raw = args.string_at(1, "seed key, or a comma-separated list of them")?;
    let mut seeds = Vec::new();
    let mut missing = Vec::new();
    for key in raw.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        match epoch.vertex(key.as_bytes()) {
            Some(vertex) => seeds.push(vertex),
            None => missing.push(key.to_string()),
        }
    }
    if !missing.is_empty() {
        return plan_err!(
            "these seed keys are not vertices in this graph: {missing:?}. Refusing rather \
             than skipping them: a missing seed would otherwise read as an entity that is \
             connected to nothing"
        );
    }
    if seeds.is_empty() {
        return plan_err!("at least one seed key is required");
    }
    Ok(seeds)
}

/// Build the edge mask from the optional `edge_types` argument.
fn mask_of(epoch: &EpochRef, args: &Arguments) -> Result<EdgeMask> {
    let named = args.list("edge_types");
    if named.is_empty() {
        return Ok(epoch.adjacency().all_edge_types());
    }
    let mut types = Vec::new();
    for name in &named {
        let Some(edge_type) = epoch.edge_type(name) else {
            return plan_err!(
                "'{name}' is not an edge type in this graph. An unrecognised type would \
                 otherwise silently narrow the traversal to nothing"
            );
        };
        types.push(edge_type);
    }
    Ok(EdgeMask::of(types))
}

/// The external key of a vertex, or its numeric id if the key is not text.
fn key_of(epoch: &EpochRef, vertex: VertexId) -> String {
    epoch.key(vertex).map_or_else(
        || vertex.0.to_string(),
        |k| String::from_utf8_lossy(k).into_owned(),
    )
}

// ---------------------------------------------------------------------------

/// `graph_reachable(graph, seeds, ...)` --- everything reachable, ignoring time.
#[derive(Debug)]
struct Reachable(Arc<GraphCatalog>);

impl datafusion::catalog::TableFunctionImpl for Reachable {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 2)?;
        let epoch = graph_of(&self.0, &args)?;
        let seeds = seeds_of(&epoch, &args)?;
        let mask = mask_of(&epoch, &args)?;
        let budget = args.budget()?;

        let found = traverse::reachable(epoch.adjacency(), &seeds, &mask, &budget);
        reached_batch(&epoch, &found, false)
    }
}

/// `graph_time_respecting(graph, seeds, ...)` --- only routes time permits.
#[derive(Debug)]
struct TimeRespecting(Arc<GraphCatalog>);

impl datafusion::catalog::TableFunctionImpl for TimeRespecting {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 2)?;
        let epoch = graph_of(&self.0, &args)?;
        let seeds = seeds_of(&epoch, &args)?;
        let mask = mask_of(&epoch, &args)?;
        let budget = args.budget()?;

        let mut constraints = traverse::TimeConstraints::none();
        if let Some(from) = args.integer("from")? {
            constraints.from = from;
        }
        if let Some(until) = args.integer("until")? {
            constraints.until = until;
        }
        if let Some(dwell) = args.integer("max_dwell")? {
            constraints.max_dwell = dwell;
        }
        if let Some(dwell) = args.integer("min_dwell")? {
            constraints.min_dwell = dwell;
        }
        if let Some(fraction) = args.number("min_conservation")? {
            if !(0.0..=1.0).contains(&fraction) {
                return plan_err!("'min_conservation' is a fraction and must be between 0 and 1");
            }
            constraints.min_conservation = fraction;
        }
        let start_at = args.integer("start_at")?.unwrap_or(i64::MIN);

        let found = traverse::time_respecting(
            epoch.adjacency(),
            &seeds,
            &mask,
            start_at,
            &constraints,
            &budget,
        );
        reached_batch(&epoch, &found, true)
    }
}

/// The schema and rows shared by the two expansion functions.
fn reached_batch(
    epoch: &EpochRef,
    found: &Bounded<Vec<traverse::Reached>>,
    temporal: bool,
) -> Result<Arc<dyn TableProvider>> {
    let mut fields = vec![
        Field::new("vertex", DataType::Utf8, false),
        Field::new("vertex_type", DataType::Utf8, true),
        Field::new("depth", DataType::UInt32, false),
        Field::new("via", DataType::Utf8, true),
    ];
    if temporal {
        fields.push(Field::new("reached_at", DataType::Int64, true));
    }
    fields.extend(provenance_fields());
    let schema: SchemaRef = Arc::new(Schema::new(fields));

    let rows = found.found.len();
    let vertices: StringArray = found
        .found
        .iter()
        .map(|r| Some(key_of(epoch, r.vertex)))
        .collect();
    let types: StringArray = found
        .found
        .iter()
        .map(|r| epoch.vertex_type(r.vertex))
        .collect();
    let depths = UInt32Array::from(found.found.iter().map(|r| r.depth).collect::<Vec<_>>());
    let via: StringArray = found
        .found
        .iter()
        .map(|r| r.via.map(|v| key_of(epoch, v)))
        .collect();

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(vertices),
        Arc::new(types),
        Arc::new(depths),
        Arc::new(via),
    ];
    if temporal {
        let at: Int64Array = found
            .found
            .iter()
            .map(|r| (r.at != i64::MIN).then_some(r.at))
            .collect();
        columns.push(Arc::new(at));
    }
    columns.extend(provenance_columns(epoch, &found.truncation, rows));

    let batch = RecordBatch::try_new(schema, columns)?;
    Ok(Arc::new(TraversalTable::new(batch)))
}

// ---------------------------------------------------------------------------

/// `graph_shortest_path(graph, from, to, ...)` --- one row per step of the cheapest route.
#[derive(Debug)]
struct ShortestPath(Arc<GraphCatalog>);

impl datafusion::catalog::TableFunctionImpl for ShortestPath {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 3)?;
        let epoch = graph_of(&self.0, &args)?;
        let mask = mask_of(&epoch, &args)?;
        let budget = args.budget()?;

        let from_key = args.string_at(1, "origin key")?;
        let to_key = args.string_at(2, "destination key")?;
        let (Some(from), Some(to)) = (
            epoch.vertex(from_key.as_bytes()),
            epoch.vertex(to_key.as_bytes()),
        ) else {
            return plan_err!(
                "'{from_key}' or '{to_key}' is not a vertex in this graph; refusing rather \
                 than reporting no route between things that are not there"
            );
        };

        let k = args.integer("k")?.unwrap_or(1).max(1);
        let found = paths::k_shortest_loopless(
            epoch.adjacency(),
            from,
            to,
            usize::try_from(k).unwrap_or(1),
            &mask,
            &budget,
        )
        .map_err(|e| plan_datafusion_err!("{e}"))?;

        path_batch(&epoch, &found)
    }
}

/// `graph_cycles(graph, seeds, ...)` --- every simple circuit, one row per step.
#[derive(Debug)]
struct Cycles(Arc<GraphCatalog>);

impl datafusion::catalog::TableFunctionImpl for Cycles {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 2)?;
        let epoch = graph_of(&self.0, &args)?;
        let seeds = seeds_of(&epoch, &args)?;
        let mask = mask_of(&epoch, &args)?;
        let budget = args.budget()?;

        let found = paths::cycles(epoch.adjacency(), &seeds, &mask, &budget);
        path_batch(&epoch, &found)
    }
}

/// One row per step of each path, so the result joins naturally against a vertex table.
fn path_batch(
    epoch: &EpochRef,
    found: &Bounded<Vec<paths::Path>>,
) -> Result<Arc<dyn TableProvider>> {
    let mut fields = vec![
        Field::new("path_id", DataType::UInt32, false),
        Field::new("position", DataType::UInt32, false),
        Field::new("vertex", DataType::Utf8, false),
        Field::new("path_cost", DataType::Float64, false),
        Field::new("path_hops", DataType::UInt32, false),
    ];
    fields.extend(provenance_fields());
    let schema: SchemaRef = Arc::new(Schema::new(fields));

    let mut path_ids = Vec::new();
    let mut positions = Vec::new();
    let mut vertices = Vec::new();
    let mut costs = Vec::new();
    let mut hops = Vec::new();
    for (index, path) in found.found.iter().enumerate() {
        for (position, vertex) in path.vertices.iter().enumerate() {
            path_ids.push(u32::try_from(index).unwrap_or(u32::MAX));
            positions.push(u32::try_from(position).unwrap_or(u32::MAX));
            vertices.push(Some(key_of(epoch, *vertex)));
            costs.push(path.cost);
            hops.push(u32::try_from(path.hops()).unwrap_or(u32::MAX));
        }
    }
    let rows = path_ids.len();

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(UInt32Array::from(path_ids)),
        Arc::new(UInt32Array::from(positions)),
        Arc::new(vertices.into_iter().collect::<StringArray>()),
        Arc::new(Float64Array::from(costs)),
        Arc::new(UInt32Array::from(hops)),
    ];
    columns.extend(provenance_columns(epoch, &found.truncation, rows));

    let batch = RecordBatch::try_new(schema, columns)?;
    Ok(Arc::new(TraversalTable::new(batch)))
}

// ---------------------------------------------------------------------------

/// `graph_influence(graph, seeds, ...)` --- damped multiplicative reach.
#[derive(Debug)]
struct Influence(Arc<GraphCatalog>);

impl datafusion::catalog::TableFunctionImpl for Influence {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 2)?;
        let epoch = graph_of(&self.0, &args)?;
        let seeds = seeds_of(&epoch, &args)?;
        let mask = mask_of(&epoch, &args)?;
        let budget = args.budget()?;

        let mut damping = product::Damping::none();
        if let Some(per_hop) = args.number("damping")? {
            if !(0.0..=1.0).contains(&per_hop) {
                return plan_err!("'damping' is a fraction and must be between 0 and 1");
            }
            damping.per_hop = per_hop;
        }
        if let Some(floor) = args.number("floor")? {
            if floor <= 0.0 {
                return plan_err!(
                    "'floor' must be above zero: it is what makes the search terminate on a \
                     cyclic graph, not an optimisation"
                );
            }
            damping.floor = floor;
        }

        let found = product::influence(epoch.adjacency(), &seeds, &mask, damping, &budget);

        let mut fields = vec![
            Field::new("vertex", DataType::Utf8, false),
            Field::new("vertex_type", DataType::Utf8, true),
            Field::new("score", DataType::Float64, false),
        ];
        fields.extend(provenance_fields());
        let schema: SchemaRef = Arc::new(Schema::new(fields));

        let rows = found.found.len();
        let vertices: StringArray = found
            .found
            .iter()
            .map(|s| Some(key_of(&epoch, s.vertex)))
            .collect();
        let types: StringArray = found
            .found
            .iter()
            .map(|s| epoch.vertex_type(s.vertex))
            .collect();
        let scores = Float64Array::from(found.found.iter().map(|s| s.value).collect::<Vec<_>>());

        let mut columns: Vec<ArrayRef> =
            vec![Arc::new(vertices), Arc::new(types), Arc::new(scores)];
        columns.extend(provenance_columns(&epoch, &found.truncation, rows));

        let batch = RecordBatch::try_new(schema, columns)?;
        Ok(Arc::new(TraversalTable::new(batch)))
    }
}
