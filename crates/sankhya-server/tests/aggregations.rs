//! A user's own aggregation, declared and used over the wire.
//!
//! # What this proves that the crate tests do not
//!
//! `sankhya-udf` proves the contract and `sankhya-olap` proves the aggregate. Neither proves
//! that a person sitting at `psql` can declare one and then use it --- which is the whole
//! capability, and the part that quietly does not exist when a kernel is built and never wired.
//!
//! So every statement below crosses the PostgreSQL wire protocol, against the shipping server.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![allow(clippy::print_stdout)]

mod common;

use common::{start, text_rows, write_warehouse, Running};

fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

/// One statement's rows, or the refusal it produced.
fn ask(port: u16, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    std::panic::catch_unwind(|| text_rows(port, sql)).map_err(|panicked| {
        panicked
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panicked.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "the statement failed".to_owned())
    })
}

/// A weighted mean: the rule a cube cannot express and a firm always has. `MEAN` is refused
/// along any dimension because an average of averages is not an average; this carries its
/// weight in its state, so it composes.
const WEIGHTED: &str = "CREATE AGGREGATION weighted_mean LANGUAGE PYTHON AS $$
def initial():
    return {'total': 0.0, 'weight': 0.0}

def accumulate(state, values):
    for i in range(0, len(values) - 1, 2):
        state['total'] += values[i] * values[i + 1]
        state['weight'] += values[i + 1]
    return state

def merge(a, b):
    return {'total': a['total'] + b['total'], 'weight': a['weight'] + b['weight']}

def finish(state):
    return state['total'] / state['weight'] if state['weight'] else 0.0
$$";

/// Whether this machine can host the boundary at all. Reported, never faked.
fn boundary_or_skip(port: u16) -> bool {
    match ask(port, WEIGHTED) {
        Ok(_) => true,
        Err(said) if said.contains("namespace") || said.contains("Linux") => {
            println!("SKIPPED: this machine cannot host the sandbox --- {said}");
            false
        }
        Err(said) => panic!("declaring a weighted mean failed: {said}"),
    }
}

#[test]
fn an_aggregation_is_declared_and_then_answers_a_group_by() {
    let (_dir, server) = running();
    if !boundary_or_skip(server.port) {
        return;
    }

    let shown = text_rows(server.port, "SHOW AGGREGATIONS");
    let names: Vec<String> = shown
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert!(names.iter().any(|n| n == "weighted_mean"), "it is served: {names:?}");

    // The source is shown, because ADR-0023 makes creating one a grant and a grant nobody can
    // review is a grant nobody should give.
    let source: String = shown
        .iter()
        .filter(|row| row.first().cloned().flatten().as_deref() == Some("weighted_mean"))
        .filter_map(|row| row.get(2).cloned().flatten())
        .next()
        .unwrap_or_default();
    assert!(source.contains("def merge"), "the author's code is stored as written: {source}");

    // And used. `sales.orders` carries `amount` and `margin_pct`; weighting the margin by the
    // amount is a number no built-in rule produces.
    let answers = text_rows(
        server.port,
        "SELECT region, weighted_mean(margin_pct, amount) AS w \
         FROM sales.orders WHERE region IS NOT NULL GROUP BY region ORDER BY region",
    );
    assert_eq!(answers.len(), 2, "two regions: {answers:?}");
    for row in &answers {
        let value: f64 = row[1].as_deref().unwrap_or("nan").parse().unwrap_or(f64::NAN);
        assert!(
            value.is_finite() && (0.0..1.0).contains(&value),
            "a weighted margin is a proportion: {row:?}"
        );
    }
}

#[test]
fn it_survives_a_restart() {
    // A declared aggregation held only in memory is one a restart drops, and the symptom is a
    // query that worked yesterday failing to plan.
    let (dir, server) = running();
    if !boundary_or_skip(server.port) {
        return;
    }
    let again = start(&dir.path().join("warehouse"), &dir.path().join("data-again"));
    let names: Vec<String> = text_rows(again.port, "SHOW AGGREGATIONS")
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert!(
        names.iter().any(|n| n == "weighted_mean"),
        "it must be there after a restart: {names:?}"
    );
    // And be callable, which is the half a listing does not prove.
    let answers = text_rows(
        again.port,
        "SELECT weighted_mean(margin_pct, amount) FROM sales.orders",
    );
    assert_eq!(answers.len(), 1, "and it answers: {answers:?}");
}

#[test]
fn one_that_disagrees_with_itself_is_refused_at_declaration() {
    // ADR-0010's check, reaching a person at a terminal. A cuboid built from this would differ
    // from base data by however much the batching happened to differ, and nothing about either
    // number would look wrong.
    let (_dir, server) = running();
    let batch_dependent = "CREATE AGGREGATION mean_of_batches LANGUAGE PYTHON AS $$
def initial():
    return {'total': 0.0, 'batches': 0}

def accumulate(state, values):
    for v in values:
        state['total'] += v
    state['batches'] += 1
    return state

def finish(state):
    return state['total'] / state['batches']
$$";
    let Err(said) = ask(server.port, batch_dependent) else {
        panic!("an aggregation whose answer depends on batching must not be declared");
    };
    if said.contains("namespace") || said.contains("Linux") {
        println!("SKIPPED: this machine cannot host the sandbox");
        return;
    }
    assert!(
        said.contains("how the rows were batched"),
        "the refusal names the property that failed: {said}"
    );

    let names: Vec<String> = text_rows(server.port, "SHOW AGGREGATIONS")
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert!(names.is_empty(), "and nothing was stored or served: {names:?}");
}

#[test]
fn dropping_one_stops_it_answering() {
    let (_dir, server) = running();
    if !boundary_or_skip(server.port) {
        return;
    }
    let _ = text_rows(server.port, "DROP AGGREGATION weighted_mean");

    let names: Vec<String> = text_rows(server.port, "SHOW AGGREGATIONS")
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert!(names.is_empty(), "it stops being served at once: {names:?}");

    let called = ask(
        server.port,
        "SELECT weighted_mean(margin_pct, amount) FROM sales.orders",
    );
    assert!(
        called.is_err(),
        "and a statement calling it is refused rather than answered by something else"
    );
}
