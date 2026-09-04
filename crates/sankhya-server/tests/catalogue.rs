//! `functions()` against what a session actually registers.
//!
//! # Why this test lives in the server crate
//!
//! Because only here is the whole set registered. `sankhya-functions` holds the descriptions,
//! `sankhya-olap` the vector and matrix functions, `sankhya-cube-sql` the cubes and
//! `sankhya-graph-sql` the graph --- and no one of those crates can see the others, since they
//! sit at different layers.
//!
//! So the catalogue's completeness is a property of the **assembled server**, and this is the
//! only place it can be asserted. Without it, adding a function to any of those crates and
//! forgetting to describe it is a silent gap: every crate's own tests pass, and the binding
//! generated from the catalogue simply does not offer the new function.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{start, text_rows};
use std::collections::BTreeSet;

/// A server over an empty warehouse: the catalogue does not depend on any table.
fn running() -> (tempfile::TempDir, common::Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).expect("a warehouse");
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

#[test]
fn a_client_can_ask_what_this_server_can_compute() {
    // The whole reason `functions()` exists: a capability nobody can enumerate is a reference
    // manual nobody reads. Before it, the only way to learn what this server could compute was
    // to read Rust.
    let (_dir, server) = running();

    let rows = text_rows(server.port, "SELECT function, category, arity FROM functions()");
    assert!(rows.len() > 100, "the catalogue is far too short: {} rows", rows.len());

    let names: BTreeSet<&str> =
        rows.iter().filter_map(|row| row[0].as_deref()).collect();
    // One from each family, so a catalogue missing a whole crate's worth fails here.
    for expected in [
        "norm_inv",            // sankhya-functions
        "vec_cosine_similarity", // sankhya-olap, vectors
        "mat_cholesky",        // sankhya-functions, linear algebra
        "mat_determinant",     // sankhya-olap, matrices
        "cube_rollup",         // sankhya-cube-sql
        "graph_shortest_path", // sankhya-graph-sql
        "functions",           // itself, which a catalogue that omits is a catalogue that lies
    ] {
        assert!(names.contains(expected), "`{expected}` is not in the catalogue");
    }
}

#[test]
fn every_function_a_server_registers_is_described() {
    // The property that makes the catalogue trustworthy, and the one no single crate can
    // assert. A function registered and not described is one no binding will offer; an entry
    // for a function nobody registered becomes a method that does not plan.
    //
    // Asked of a **real session**, so describing a new function is required rather than
    // remembered.
    let (_dir, server) = running();

    let described: BTreeSet<String> = text_rows(server.port, "SELECT function FROM functions()")
        .iter()
        .filter_map(|row| row[0].clone())
        .collect();

    // The comparison this test used to make was against `catalogue::everything()` --- which
    // is the same list `functions()` is *served from*. Both sides read one source, so the
    // assertion held for any catalogue whatsoever, correct or not, and it had already missed
    // an addition. The comment above claimed a real session; only the left-hand side was one.
    //
    // The engine's own registry is the ground truth, and it is read in
    // `every_function_the_engine_registers_is_described` below.
    assert!(
        !described.is_empty(),
        "the server described no functions at all"
    );

    // And each described function actually plans, which is the claim the catalogue makes.
    // Checked on the nullary and unary numeric ones, where a call can be written without
    // knowing the shape --- the rest are covered by their own crates' tests.
    for name in ["norm_cdf", "erf", "gammaln"] {
        let rows = text_rows(server.port, &format!("SELECT {name}(1.0)"));
        assert_eq!(rows.len(), 1, "`{name}` is in the catalogue and did not answer");
    }
}

#[test]
fn the_catalogue_is_ordered_and_says_what_each_function_needs() {
    // Sorted, so two calls give the same order and a client diffing between versions sees only
    // what changed.
    let (_dir, server) = running();
    let rows = text_rows(
        server.port,
        "SELECT function, takes, gives, about FROM functions()",
    );

    let names: Vec<&str> = rows.iter().filter_map(|row| row[0].as_deref()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the catalogue is not in a stable order");

    for row in &rows {
        let name = row[0].as_deref().unwrap_or("?");
        assert!(row[1].as_deref().is_some_and(|t| !t.is_empty()), "{name} says nothing about what it takes");
        assert!(row[2].as_deref().is_some_and(|g| !g.is_empty()), "{name} says nothing about what it gives");
        assert!(
            row[3].as_deref().is_some_and(|a| a.len() > 20),
            "{name}'s description is too short to choose by"
        );
    }
}


/// What the **engine** holds, against what the catalogue says it holds.
///
/// # Why the other test could not do this
///
/// `every_function_a_server_registers_is_described` compared `functions()` --- served from
/// `catalogue::everything()` --- against `catalogue::everything()`. One source on both sides:
/// the assertion was true of any catalogue at all, including a wrong one, and it had already
/// let an addition through. A tautology is worse than no test, because the gate reports it as
/// coverage.
///
/// The registry a `SessionContext` actually carries is a different source, and it is the one
/// that decides whether a statement plans.
///
/// # Why the difference against a bare session
///
/// DataFusion registers about a hundred functions of its own --- `abs`, `coalesce`,
/// `date_trunc`. Those are not ours to describe. Subtracting a bare session's names from the
/// server's leaves exactly what SANKHYA added, which is exactly what the catalogue is a
/// description of.
#[test]
fn every_function_the_engine_registers_is_described() {
    use datafusion::prelude::SessionContext;

    let names = |context: &SessionContext| -> BTreeSet<String> {
        let state = context.state();
        let mut out: BTreeSet<String> = BTreeSet::new();
        out.extend(state.scalar_functions().keys().cloned());
        out.extend(state.aggregate_functions().keys().cloned());
        out.extend(state.window_functions().keys().cloned());
        // Table functions too. Leaving them out was not a small omission: `cube_rollup`,
        // `cube_slice`, `functions` and every `graph_*` entry are table functions, so a
        // check that read only the scalar kinds would report eleven of the catalogue's most
        // prominent entries as described-but-unregistered.
        out.extend(state.table_functions().keys().cloned());
        out
    };

    let bare = names(&SessionContext::new());

    // The same registrations `crates/sankhya-server/src/execute.rs` makes, in the same order.
    let context = SessionContext::new();
    sankhya_olap::register_constructors(&context);
    sankhya_olap::register_vector_functions(&context);
    sankhya_olap::register_matrix_functions(&context);
    sankhya_functions::register(&context);
    sankhya_functions::describe::register(&context, sankhya_functions::catalogue::everything());
    sankhya_graph_sql::functions::register(
        &context,
        std::sync::Arc::new(sankhya_graph_sql::catalog::GraphCatalog::new()),
    );
    // The cube surface, which `wiring.rs` registers rather than `execute.rs` --- against an
    // empty catalogue, because which cubes exist does not change which *functions* exist.
    //
    // Replicated here, and that duplication is the one weakness of this test. It is
    // self-policing in the direction that matters: a surface registered by the server and
    // missing from this list shows up as "described and not registered", which is precisely
    // how these five were found.
    let cubes = std::sync::Arc::new(sankhya_cube_sql::catalog::CubeCatalog::new());
    let log = std::sync::Arc::new(sankhya_cube::querylog::QueryLog::with_capacity(1));
    sankhya_cube_sql::functions::register(
        &context,
        std::sync::Arc::clone(&cubes),
        log,
        None,
    );
    sankhya_cube_sql::describe::register(
        &context,
        std::sync::Arc::new(Vec::new()),
        cubes,
    );

    let ours: BTreeSet<String> = names(&context).difference(&bare).cloned().collect();
    let described: BTreeSet<String> = sankhya_functions::catalogue::everything()
        .iter()
        .map(|entry| entry.name.to_owned())
        .collect();

    let undescribed: Vec<&String> = ours.difference(&described).collect();
    assert!(
        undescribed.is_empty(),
        "registered and not described --- no binding will offer these, and nothing else \
         would have noticed: {undescribed:?}"
    );

    // The other direction matters too, but only for the kinds the registry can see. A
    // catalogue entry for a function nobody registered becomes a binding method that does
    // not plan.
    let unregistered: Vec<&String> = described
        .difference(&ours)
        .filter(|name| !bare.contains(name.as_str()))
        .collect();
    assert!(
        unregistered.is_empty(),
        "described and not registered --- the binding will offer a method that cannot \
         plan: {unregistered:?}"
    );
}
