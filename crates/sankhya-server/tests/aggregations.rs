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

use common::{start, start_with_user_functions, text_rows, write_warehouse, Running};

fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start_with_user_functions(&warehouse, &dir.path().join("data"));
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
    let again =
        start_with_user_functions(&dir.path().join("warehouse"), &dir.path().join("data-again"));
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

/// A root-mean-square: a rule no built-in expresses, and one a cube can use.
///
/// Single-argument on purpose. A cube measure **is one column**, so an aggregation used as a
/// cube rule sees one value per fact. A weighted mean needs two columns and is therefore a
/// query-level aggregate rather than a cube rule until a measure can name more than one column
/// --- which is a different feature and is not this one pretending to be it.
const RMS: &str = "CREATE AGGREGATION rms LANGUAGE PYTHON AS $$
def initial():
    return {'sq': 0.0, 'n': 0}

def accumulate(state, values):
    for v in values:
        state['sq'] += v * v
        state['n'] += 1
    return state

def merge(a, b):
    return {'sq': a['sq'] + b['sq'], 'n': a['n'] + b['n']}

def finish(state):
    return (state['sq'] / state['n']) ** 0.5 if state['n'] else 0.0
$$";

/// Two dimensions on purpose. One cannot demonstrate a roll-up --- rolling *up* means rolling a
/// dimension **away**, and with a single dimension every query is already the base grain, so the
/// code that combines a user's aggregation across cells would never run.
const CUBE: &str = "CREATE CUBE spread FROM sales.orders \
     DIMENSION region FROM sales.regions ON region (LEVEL area = region) \
     DIMENSION period FROM sales.orders ON period (LEVEL quarter = period) \
     MEASURE margin_pct (AGGREGATION rms ALONG region, AGGREGATION rms ALONG period)";

#[test]
fn a_cube_measure_may_be_an_aggregation_of_your_own() {
    // The owner's ask, end to end: *user supplied merge functions can be very useful for custom
    // cubing.* A root mean square is not a sum, a last, a max, a min or a mean, so before this
    // there was no way to declare it as a measure at all --- and `MEAN` is refused along any
    // dimension for the reason that makes this worth having: an average of averages is not an
    // average.
    let (_dir, server) = running();
    match ask(server.port, RMS) {
        Ok(_) => {}
        Err(said) if said.contains("namespace") || said.contains("Linux") => {
            println!("SKIPPED: this machine cannot host the sandbox");
            return;
        }
        Err(said) => panic!("declaring it failed: {said}"),
    }

    let _ = ask(server.port, CUBE).expect("a cube may name a declared aggregation");

    // `by=region` rolls **period away**, so each answer is computed over every fact in that
    // region across both quarters. That is the path a single-dimension cube could never reach,
    // and it is where a user's aggregation has to be recomputed over the union of its children's
    // facts rather than over numbers made from them.
    let answers = text_rows(
        server.port,
        "SELECT region, margin_pct FROM cube_rollup('spread', 'margin_pct', 'by=region') \
         ORDER BY region",
    );
    assert_eq!(answers.len(), 2, "one row per region, with the quarters rolled away: {answers:?}");

    // Against the same arithmetic written out in SQL, which is the only assertion that can fail
    // for the right reason. A row count proves the roll-up ran; only the number proves the
    // author's function is what computed it.
    let expected = text_rows(
        server.port,
        "SELECT o.region, sqrt(avg(o.margin_pct * o.margin_pct)) AS rms \
           FROM sales.orders o WHERE o.region IS NOT NULL \
          GROUP BY o.region ORDER BY o.region",
    );
    assert_eq!(expected.len(), 2, "the control: {expected:?}");
    for (mine, theirs) in answers.iter().zip(expected.iter()) {
        let a: f64 = mine[1].as_deref().unwrap_or("nan").parse().unwrap_or(f64::NAN);
        let b: f64 = theirs[1].as_deref().unwrap_or("nan").parse().unwrap_or(f64::NAN);
        assert!(
            (a - b).abs() < 1e-9,
            "the cube's measure must be what the author's function computes: {a} against {b}"
        );
    }
}

#[test]
fn a_cube_naming_an_aggregation_that_does_not_exist_is_refused() {
    // At declaration, where the person who typed the name is still here --- rather than at the
    // first query, three weeks later, run by somebody else.
    let (_dir, server) = running();
    let Err(said) = ask(server.port, CUBE) else {
        panic!("a cube must not name an aggregation this server has never heard of");
    };
    assert!(
        said.contains("SHOW AGGREGATIONS"),
        "and the refusal says how to find out what it does have: {said}"
    );
}


/// The door is shut on a server nobody opened.
///
/// # Why this test is the point of the switch
///
/// Every other test in this file calls `start_with_user_functions`, so all of them exercise a
/// server with the capability granted --- which is exactly how the defect survived. The
/// statement ran arbitrary Python for any caller, including one holding no roles, and then
/// recorded an audit entry saying the decision had been allowed. Nothing here asserted the
/// closed case because nothing ever ran the closed case.
///
/// This uses `start`, which is what an operator who has decided nothing gets.
#[test]
fn a_server_nobody_opened_refuses_to_run_code_it_was_handed() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let refused = ask(server.port, WEIGHTED).expect_err("a closed server ran supplied code");
    assert!(
        refused.contains("does not accept user-supplied aggregations"),
        "refused for the wrong reason: {refused}"
    );
    assert!(
        refused.contains("42501"),
        "a capability refusal must carry insufficient privilege: {refused}"
    );

    // Dropping is state too, and was equally ungated.
    let dropped = ask(server.port, "DROP AGGREGATION weighted_mean")
        .expect_err("a closed server accepted a DROP");
    assert!(dropped.contains("does not accept user-supplied aggregations"), "{dropped}");

    // Reading the list is not running one, so it still answers --- an operator who has just
    // shut the door needs to see what came in while it was open.
    let listed = ask(server.port, "SHOW AGGREGATIONS").expect("SHOW is not gated");
    assert!(listed.is_empty(), "nothing was declared: {listed:?}");
}
