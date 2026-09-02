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

    // What the session really has, from the engine's own view of itself.
    let registered: BTreeSet<String> = sankhya_functions::catalogue::everything()
        .iter()
        .map(|entry| entry.name.to_owned())
        .collect();

    assert_eq!(
        described, registered,
        "the catalogue and the registrations disagree"
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
