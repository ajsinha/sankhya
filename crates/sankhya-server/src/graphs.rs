//! Building the graphs a warehouse declares, and deciding who may traverse them.
//!
//! # What was missing, and it was not an algorithm
//!
//! The traversal engine is complete: reachability, weighted and *k*-shortest loopless paths,
//! simple cycles, components, centrality, communities and multiplicative influence, each
//! bounded and each reporting its own truncation. Five table functions expose it, `GUIDE.md`
//! documents them, and every one of them answered *"no graph named '…'; known graphs are []"*
//! on every startable server under every configuration.
//!
//! `GraphCatalog` was constructed as a **temporary** inside the session builder --- the `Arc`
//! bound to nothing, the function running once per session --- so each session got a fresh
//! empty map with no handle by which anything could ever populate it. `register` and `publish`
//! had zero call sites anywhere, tests included. One of the three engines in the product's
//! name did not run.
//!
//! This is the population path. A declaration says which table's rows are edges; hydration
//! scans those tables and freezes an epoch; the epoch goes into a catalogue bound to the
//! server, where a session can find it.
//!
//! # Hydrated once, over every row, and then not shown to everybody
//!
//! An epoch is immutable and expensive, so one per server is the right shape. The consequence
//! is that it is built **unrestricted** --- the same providers `hydrate_unrestricted` uses for
//! an unrestricted cuboid --- and a caller a policy filters must therefore not be given it.
//!
//! So a graph is offered to a principal only where they may read **every** table it is built
//! from *and* their guard withholds nothing on those tables. A traversal over edges somebody
//! may not see is a disclosure through reachability, and an invisible one: every vertex it
//! returns is real, and nothing in the answer says it came from rows the caller is filtered
//! out of. Refusing is the same rule a cuboid built at the unrestricted scope already follows.
//!
//! **A row-filtered principal is told the graph does not exist**, not that they may not see
//! it, for the reason every listing here follows: saying so confirms it exists. Per-principal
//! epochs would let them traverse their own subgraph and are a different milestone --- an
//! epoch per scope is an epoch per policy shape, and the cost of that has to be measured
//! before it is chosen.

use crate::wiring::Server;
use sankhya_authz::principal::Principal;
use sankhya_graph::catalogue::Graph;
use sankhya_graph::epoch::EpochId;
use sankhya_graph::hydrate::{Hydration, MemoryBudget};
use std::sync::Arc;

impl Server {
    /// The graphs this warehouse declares, as a snapshot.
    ///
    /// A snapshot rather than a borrow, for the reason `cubes()` gives: a statement is planned
    /// against the graphs that existed when it started.
    #[must_use]
    pub fn declared_graphs(&self) -> Arc<Vec<Graph>> {
        self.declared_graphs.read().map_or_else(
            |poisoned| Arc::clone(&poisoned.into_inner()),
            |declared| Arc::clone(&declared),
        )
    }

    /// Load, validate and adopt the graphs a warehouse declares.
    ///
    /// Separate from `adopting_cubes` because the two fail differently and a caller wants to
    /// know which: a cube that will not validate leaves its tables readable, and a graph that
    /// will not hydrate leaves the table it reads perfectly queryable.
    ///
    /// These two live here rather than in `wiring.rs` because that file reaches its hard
    /// length limit every time a capability is added, and the right answer to that is not a
    /// larger limit --- a file nobody can hold in their head is where a statement comes to be
    /// intercepted twice, or not at all.
    #[must_use]
    pub fn adopting_graphs(self, warehouse: &std::path::Path) -> (Self, Vec<String>) {
        let complaints = adopt(&self, warehouse);
        (self, complaints)
    }
}

/// Load, validate and adopt the graphs a warehouse declares.
///
/// Returns what could not be adopted. A broken declaration does **not** stop the server: the
/// other graphs, every cube and every table are still served, and the complaint is printed
/// where somebody starting the server will read it. A warehouse that will not start because
/// one graph is misdeclared is a warehouse nobody can fix.
pub(crate) fn adopt(server: &Server, warehouse: &std::path::Path) -> Vec<String> {
    let mut complaints = Vec::new();
    let declarations = match sankhya_graph::catalogue::load_all(warehouse) {
        Ok(declarations) => declarations,
        Err(error) => {
            complaints.push(error.to_string());
            Vec::new()
        }
    };

    let mut adopted = Vec::new();
    for declaration in declarations {
        let name = declaration.name.clone();
        match declaration.validate() {
            Ok(graph) => adopted.push(graph),
            Err(rejections) => {
                let why: Vec<String> = rejections.iter().map(ToString::to_string).collect();
                complaints.push(format!("the graph `{name}`: {}", why.join("; ")));
            }
        }
    }

    for graph in &adopted {
        if let Err(why) = hydrate(server, graph) {
            // Declared and unhydrated, which the catalogue reports as a **distinct state** from
            // "no such graph" --- one is a name to wait on and the other is a typo. Registering
            // the empty slot is what makes that distinction reachable, so it happens even when
            // the build fails.
            server.graphs.register(graph.name(), Arc::new(sankhya_graph::EpochSlot::empty()));
            complaints.push(format!("the graph `{}`: {why}", graph.name()));
        }
    }

    if let Ok(mut declared) = server.declared_graphs.write() {
        *declared = Arc::new(adopted);
    }
    complaints
}

/// Scan a graph's tables and publish an epoch for it.
///
/// # Errors
/// A table it names that this server cannot serve, a scan that fails, or a hydration that will
/// not fit its declared budget. All three are reported rather than swallowed: a graph that is
/// silently absent answers every traversal with "no such graph", which sends somebody to fix a
/// name that is not wrong.
pub(crate) fn hydrate(server: &Server, graph: &Graph) -> Result<(), String> {
    use datafusion::prelude::SessionContext;
    use futures::StreamExt;

    let context = SessionContext::new();
    let reads = graph.reads();
    {
        // Registered **unsecured**, which is what makes this the unrestricted epoch. See the
        // module header for who is then allowed to traverse it.
        let servable = Arc::clone(&server.servable.read());
        for table in reads.iter() {
            let bare = table.rsplit('.').next().unwrap_or(table);
            let found = servable
                .iter()
                .find(|open| open.reference.table == bare)
                .ok_or_else(|| {
                    format!("it reads `{table}`, which this server does not serve")
                })?;
            context
                .register_table(bare, Arc::clone(&found.provider))
                .map_err(|why| format!("`{table}` could not be registered: {why}"))?;
        }
    }

    // **Every declared column must be in the table it names.**
    //
    // `Hydration::absorb` applies each spec only to batches whose schema satisfies it and
    // treats the rest as contributing nothing --- which is right, because a scan may deliver
    // several tables and each spec reads the ones it recognises. It also means a declaration
    // naming a column that exists nowhere hydrates **empty and succeeds**: the graph
    // registers, resolves, and answers every traversal with no rows, which reads exactly like
    // a traversal that found nothing. That is the failure this whole tier is arranged
    // against, arriving through the one place the reading loop cannot see it.
    //
    // So it is checked here, against the schema, before a row is read. Named in full, and
    // with what the table does have, because the fix is a one-word edit somebody has to find.
    for edge in &graph.declaration().edges {
        let bare = edge.table.rsplit('.').next().unwrap_or(&edge.table);
        let schema = tokio::task::block_in_place(|| {
            server.runtime.block_on(context.table(bare))
        })
        .map(|frame| frame.schema().clone())
        .map_err(|why| format!("`{}` could not be read: {why}", edge.table))?;
        let named = [
            Some(edge.spec.source_column.as_str()),
            Some(edge.spec.target_column.as_str()),
            edge.spec.valid_from_column.as_deref(),
            edge.spec.valid_until_column.as_deref(),
            edge.spec.weight_column.as_deref(),
        ];
        for column in named.into_iter().flatten() {
            if schema.field_with_unqualified_name(column).is_err() {
                let held: Vec<&str> =
                    schema.fields().iter().map(|field| field.name().as_str()).collect();
                return Err(format!(
                    "its `{}` edge names column `{column}`, which `{}` does not have --- it \
                     has {held:?}. Refused rather than hydrated without it: a graph built \
                     from a column that is not there has no edges, and answers every \
                     traversal with no rows",
                    edge.spec.edge_type, edge.table
                ));
            }
        }
    }

    let mut hydration = Hydration::new(
        graph.spec().clone(),
        MemoryBudget::of(graph.budget_bytes()),
    );
    for table in &reads {
        let bare = table.rsplit('.').next().unwrap_or(table);
        // Streamed, not collected, for the reason cube hydration streams: a table read whole
        // into `RecordBatch`es is two to four times its Parquet resident before a single edge
        // is absorbed, and the peak should be a batch.
        let absorbed = tokio::task::block_in_place(|| {
            server.runtime.block_on(async {
                let frame = context.table(bare).await.map_err(|why| why.to_string())?;
                let mut stream =
                    frame.execute_stream().await.map_err(|why| why.to_string())?;
                while let Some(batch) = stream.next().await {
                    let batch = batch.map_err(|why| why.to_string())?;
                    hydration.absorb(&batch).map_err(|why| why.to_string())?;
                }
                Ok::<(), String>(())
            })
        });
        absorbed.map_err(|why| format!("reading `{table}`: {why}"))?;
    }

    // The snapshot the epoch was built at, so a graph result can be reconciled with a
    // relational one taken at a different moment. Across every table it reads, for the same
    // reason a cube's is: an epoch spanning two tables read at two positions is an epoch that
    // matches neither.
    let snapshot = server.snapshot_across(&reads);
    let epoch = hydration
        .finish(EpochId(snapshot), snapshot, now_micros())
        .map_err(|why| why.to_string())?;
    server.graphs.publish(graph.name(), Arc::new(epoch));
    Ok(())
}

/// The graphs this caller may be told exist.
///
/// Every table the graph reads must be readable by them **and** unfiltered, because the epoch
/// holds every row. See the module header for why that is a refusal rather than a narrowed
/// answer.
pub(crate) fn visible_to(
    server: &Server,
    principal: &Principal,
) -> Arc<sankhya_graph_sql::catalog::GraphCatalog> {
    let declared = server.declared_graphs();
    let theirs = Arc::new(sankhya_graph_sql::catalog::GraphCatalog::new());
    for graph in declared.iter() {
        let reads = graph.reads();
        let permitted = server.scope_across(principal, &reads).is_some()
            && reads.iter().all(|table| server.withholds_nothing(principal, table));
        if !permitted {
            continue;
        }
        match server.graphs.resolve(graph.name()) {
            Ok(epoch) => theirs.publish(graph.name(), epoch),
            // Declared and not built. Registered empty so the caller is told to wait rather
            // than told the name is wrong.
            Err(_) => theirs
                .register(graph.name(), Arc::new(sankhya_graph::EpochSlot::empty())),
        }
    }
    theirs
}

/// Microseconds since the Unix epoch, or zero if the clock is before it.
fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_micros()).ok())
        .unwrap_or(0)
}
