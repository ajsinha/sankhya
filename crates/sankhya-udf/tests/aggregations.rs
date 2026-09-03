//! A user's own aggregation, declared, exercised and computed.
//!
//! # What these prove, and what a weaker test would have proved
//!
//! Every test here runs the author's Python behind the real boundary and reads the number that
//! comes back. A test that asserted the *request bytes* would pass for a worker that never ran,
//! and a test that stubbed the sandbox would pass for a machine where the boundary does not
//! exist --- which `ADR-0023` Decision 3 says is a case that must be detected, not papered over.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
#![allow(clippy::print_stdout)]

use sankhya_udf::{Refused, Worker};
use std::path::Path;

/// A weighted mean: the aggregation nobody can express as a cube rule, and the reason this
/// exists. `Mean` is refused along any dimension because an average of averages is not an
/// average; a weighted mean carries the weight in its state, and therefore composes.
const WEIGHTED: &str = r#"
def initial():
    return {"total": 0.0, "weight": 0.0}

def accumulate(state, values):
    # `values` alternates value, weight. A memoryview of doubles, so this is two reads per
    # pair and no list is built.
    for i in range(0, len(values) - 1, 2):
        state["total"] += values[i] * values[i + 1]
        state["weight"] += values[i + 1]
    return state

def merge(a, b):
    return {"total": a["total"] + b["total"], "weight": a["weight"] + b["weight"]}

def finish(state):
    return state["total"] / state["weight"] if state["weight"] else 0.0
"#;

/// The same, without a merge. Perfectly usable; it simply does not compose.
const NO_MERGE: &str = r#"
def initial():
    return {"total": 0.0, "n": 0}

def accumulate(state, values):
    for v in values:
        state["total"] += v
        state["n"] += 1
    return state

def finish(state):
    return state["total"]
"#;

fn worker() -> Option<Worker> {
    let python = Path::new("/usr/bin/python3");
    if !python.exists() {
        println!("SKIPPED: no /usr/bin/python3");
        return None;
    }
    match Worker::start(python) {
        Ok(worker) => Some(worker),
        Err(Refused::NoBoundary(said)) => {
            // A machine that cannot host the boundary is a legitimate outcome and is reported
            // rather than passed over. What would be wrong is running the function anyway.
            println!("SKIPPED: {said}");
            None
        }
        Err(other) => panic!("{other}"),
    }
}

#[test]
fn a_weighted_mean_is_declared_and_computes_what_it_should() {
    let Some(worker) = worker() else { return };
    let aggregation = worker
        .declare("weighted_mean", WEIGHTED)
        .expect("a weighted mean is deterministic and associative");
    assert!(aggregation.composes, "it declares a merge, so it composes");

    // (10*1 + 20*3) / (1 + 3) = 70/4 = 17.5
    let state = worker
        .accumulate(&aggregation, &[], &[10.0, 1.0, 20.0, 3.0])
        .expect("it accumulates");
    let answer = worker.finish(&aggregation, &state).expect("it finishes");
    assert_eq!(answer, 17.5, "the weighted mean of 10 at weight 1 and 20 at weight 3");
}

#[test]
fn partial_results_combine_to_the_same_number_as_one_pass() {
    // The property a `merge` claims, checked over a real roll-up shape: two groups computed
    // apart and combined must equal the whole computed together. This is what lets a cuboid be
    // answered from a coarser one, and it is the claim that would otherwise be believed.
    let Some(worker) = worker() else { return };
    let aggregation = worker.declare("weighted_mean", WEIGHTED).expect("declared");

    let left = worker.accumulate(&aggregation, &[], &[10.0, 1.0, 20.0, 3.0]).expect("left");
    let right = worker.accumulate(&aggregation, &[], &[30.0, 2.0, 5.0, 4.0]).expect("right");
    let combined = worker.merge(&aggregation, &left, &right).expect("merged");
    let from_parts = worker.finish(&aggregation, &combined).expect("finished");

    let whole = worker
        .accumulate(&aggregation, &[], &[10.0, 1.0, 20.0, 3.0, 30.0, 2.0, 5.0, 4.0])
        .expect("whole");
    let in_one = worker.finish(&aggregation, &whole).expect("finished");

    assert_eq!(
        from_parts.to_bits(),
        in_one.to_bits(),
        "bit for bit. A difference of 1e-16 between a cuboid and its base data is exactly what \
         a figure failing to tie out looks like"
    );
}

#[test]
fn an_aggregation_without_a_merge_is_usable_and_does_not_compose() {
    // Not a failure. `ADR-0010`: a declared `merge` means the measure composes; no merge means
    // it is computed from base data every time. Both are legitimate and the difference has to
    // be visible, because one of them may be answered from an ancestor and the other may not.
    let Some(worker) = worker() else { return };
    let aggregation = worker.declare("plain_total", NO_MERGE).expect("declared");
    assert!(!aggregation.composes, "no merge, so it does not compose");

    let state = worker.accumulate(&aggregation, &[], &[1.5, 2.5, 3.0]).expect("accumulates");
    assert_eq!(worker.finish(&aggregation, &state).expect("finishes"), 7.0);

    let refused = worker
        .merge(&aggregation, &state, &state)
        .expect_err("merging one that declares no merge is refused");
    assert!(
        refused.to_string().contains("does not compose"),
        "and the refusal says why rather than reporting a missing attribute: {refused}"
    );
}

#[test]
fn an_aggregation_that_disagrees_with_itself_is_refused_at_declaration() {
    // The check `ADR-0010` requires, and the reason it exists: this function is a perfectly
    // ordinary-looking mean whose answer depends on how the rows happened to be batched. It
    // would produce a cuboid that differs from base data by however much the batching differed,
    // and nothing about either number would look wrong.
    let Some(worker) = worker() else { return };
    let batch_dependent = r#"
def initial():
    return {"total": 0.0, "batches": 0}

def accumulate(state, values):
    for v in values:
        state["total"] += v
    state["batches"] += 1
    return state

def finish(state):
    return state["total"] / state["batches"]
"#;
    let refused = worker
        .declare("mean_of_batches", batch_dependent)
        .expect_err("it must not be trusted");
    let said = refused.to_string();
    assert!(
        said.contains("how the rows were batched"),
        "the refusal names the property that failed: {said}"
    );
    assert!(
        said.contains(" in one batch") && said.contains(" in three"),
        "and shows both answers, because the difference is the evidence: {said}"
    );
}

#[test]
fn a_merge_that_is_not_associative_is_refused_at_declaration() {
    // Subtler than the one above and worse: it agrees with itself on a single pass and differs
    // only when a roll-up combines the parts in a different order. Found months later, as a
    // total that does not match its own subtotals.
    let Some(worker) = worker() else { return };
    let lopsided = r#"
def initial():
    return {"v": 0.0}

def accumulate(state, values):
    for v in values:
        state["v"] += v
    return state

def merge(a, b):
    # Not associative: the left operand is weighted more heavily.
    return {"v": a["v"] * 1.0 + b["v"] * 0.5}

def finish(state):
    return state["v"]
"#;
    let refused = worker.declare("lopsided", lopsided).expect_err("it must not be trusted");
    let said = refused.to_string();
    assert!(
        said.contains("not associative"),
        "the refusal names the property: {said}"
    );
    assert!(
        said.contains("merged left to right") && said.contains("merged right to left"),
        "and shows the two groupings that disagreed: {said}"
    );
}

#[test]
fn a_merge_that_is_barely_wrong_is_refused_too() {
    // The check that must be **bit for bit** rather than close enough, and the case that shows
    // why. This merge is associative to twelve decimal places and not to sixteen --- a
    // comparison with any tolerance at all accepts it.
    //
    // A tolerance would be the accommodating choice and it is exactly wrong: a cuboid that
    // agrees with base data to twelve places is a cuboid that disagrees, and the difference
    // surfaces as a total that does not match its own subtotals in the last penny. That is the
    // hardest kind of defect to find and the easiest to have prevented here.
    let Some(worker) = worker() else { return };
    let barely = r#"
def initial():
    return {"v": 0.0}

def accumulate(state, values):
    for v in values:
        state["v"] += v
    return state

def merge(a, b):
    # Associative to about twelve places. Not to sixteen.
    return {"v": a["v"] + b["v"] + a["v"] * 1e-13}

def finish(state):
    return state["v"]
"#;
    let refused = worker.declare("barely", barely).expect_err("close enough is not the same");
    assert!(
        refused.to_string().contains("not associative"),
        "and it is refused for the reason it is wrong: {refused}"
    );
}

#[test]
fn a_function_that_never_returns_is_stopped() {
    // The bound `ADR-0010` asks for, reaching the caller as a sentence about their function.
    let Some(worker) = worker() else { return };
    let bounds = sankhya_sandbox::Bounds {
        wall: std::time::Duration::from_millis(600),
        ..sankhya_sandbox::Bounds::modest()
    };
    let worker = worker.within(bounds);
    let forever = r#"
def accumulate(state, values):
    while True:
        pass

def finish(state):
    return 0.0
"#;
    let refused = worker.declare("forever", forever).expect_err("it is stopped");
    assert!(
        refused.to_string().contains("did not finish"),
        "and told as something about their function: {refused}"
    );
}

#[test]
fn a_state_that_cannot_be_stored_is_refused_rather_than_stored() {
    // `ADR-0010`: a measure whose state is not serialisable is usable and not materialisable,
    // *said at declaration time* rather than discovered when a cuboid fails to write.
    let Some(worker) = worker() else { return };
    let unstorable = r#"
def initial():
    return {"seen": set()}

def accumulate(state, values):
    for v in values:
        state["seen"].add(v)
    return state

def finish(state):
    return float(len(state["seen"]))
"#;
    let refused = worker.declare("distinct", unstorable).expect_err("a set is not JSON");
    assert!(
        refused.to_string().contains("not JSON"),
        "and says what about it cannot be stored: {refused}"
    );
}

#[test]
fn the_authors_code_cannot_reach_the_network() {
    // The boundary, reached through the layer that will actually carry a user's function. The
    // sandbox crate proves the mechanism; this proves the mechanism is *applied here*, which is
    // a different claim and the one that would silently stop being true.
    let Some(worker) = worker() else { return };
    let exfiltrating = r#"
import socket

def accumulate(state, values):
    s = socket.socket()
    s.settimeout(2)
    s.connect(("1.1.1.1", 80))
    return {"sent": True}

def finish(state):
    return 1.0
"#;
    let refused = worker
        .declare("exfiltrate", exfiltrating)
        .expect_err("a function that opens a socket must not run");
    let said = refused.to_string();
    // `ENETUNREACH`, by number. "It failed" would pass on a build machine with no internet and
    // would go on passing after somebody removed the network namespace --- which is the whole
    // failure mode this test exists to catch.
    assert!(
        said.contains("Network is unreachable") || said.contains("errno 101"),
        "it must fail because this namespace has no route, not merely fail: {said}"
    );
}
