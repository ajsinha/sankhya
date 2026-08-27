//! Paths and cycles, checked against brute force rather than against themselves.
//!
//! Every routine here is compared with an exhaustive enumeration written independently in
//! this file. That is the only check worth much: both the shortest-path and the k-shortest
//! routines return well-formed results when they are wrong, and a test that compares an
//! implementation to its own output confirms nothing.
//!
//! The two hazards the design notes record are tested directly:
//! `k_shortest_returns_paths_not_walks` and `cycle_enumeration_is_not_all_pairs_distance`.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_graph_algo::budget::Budget;
use sankhya_graph_algo::csr::{Adjacency, AdjacencyBuilder, Edge, Validity};
use sankhya_graph_algo::ids::{EdgeMask, EdgeType, VertexId};
use sankhya_graph_algo::paths::{cycles, k_shortest_loopless, shortest_path, Path};

fn v(n: u32) -> VertexId {
    VertexId(n)
}

fn edge(source: u32, target: u32, weight: f64) -> Edge {
    Edge {
        source: v(source),
        target: v(target),
        edge_type: EdgeType(0),
        validity: Validity::always(),
        weight,
    }
}

fn build(vertices: usize, edges: &[Edge]) -> Adjacency {
    let mut b = AdjacencyBuilder::new(vertices);
    for e in edges {
        b.push(*e)
            .expect("test fixture pushed an out-of-range edge");
    }
    b.build()
}

fn any() -> EdgeMask {
    EdgeMask::of([EdgeType(0)])
}

// ---------------------------------------------------------------------------
// The independent reference: exhaustive enumeration of every simple path.
//
// Correct by being obviously correct rather than by being clever. Exponential, so it is
// only ever run on graphs of a dozen vertices — which is the size at which a subtle
// off-by-one in Yen's algorithm still shows up.
// ---------------------------------------------------------------------------

fn all_simple_paths(graph: &Adjacency, from: VertexId, to: VertexId) -> Vec<Path> {
    let mut out = Vec::new();
    let mut stack = vec![from];
    walk(graph, from, to, &mut stack, 0.0, &mut out);
    out.sort_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.vertices.cmp(&b.vertices))
    });
    out
}

fn walk(
    graph: &Adjacency,
    at: VertexId,
    to: VertexId,
    stack: &mut Vec<VertexId>,
    cost: f64,
    out: &mut Vec<Path>,
) {
    if at == to && stack.len() > 1 {
        out.push(Path {
            vertices: stack.clone(),
            cost,
        });
        return;
    }
    for arc in graph.out_edges(at, EdgeType(0)).iter() {
        if stack.contains(&arc.target) {
            continue;
        }
        stack.push(arc.target);
        walk(graph, arc.target, to, stack, cost + arc.weight, out);
        stack.pop();
    }
}

/// A graph with several routes of differing cost, and a couple of ties.
fn diamond_with_detours() -> Adjacency {
    build(
        6,
        &[
            edge(0, 1, 1.0),
            edge(0, 2, 2.0),
            edge(1, 3, 4.0),
            edge(2, 3, 2.0),
            edge(1, 4, 1.0),
            edge(4, 3, 1.0),
            edge(3, 5, 1.0),
            edge(2, 5, 9.0),
        ],
    )
}

// ---------------------------------------------------------------------------

#[test]
fn the_shortest_path_matches_exhaustive_enumeration() {
    let graph = diamond_with_detours();
    let reference = all_simple_paths(&graph, v(0), v(5));
    let cheapest = reference.first().expect("the fixture connects 0 to 5");

    let found = shortest_path(&graph, v(0), v(5), &any(), &Budget::generous())
        .expect("the fixture has no negative edge");
    let path = found.found.expect("0 reaches 5");

    assert_eq!(path.cost, cheapest.cost, "cost must match brute force");
    assert_eq!(path.vertices, cheapest.vertices);
    assert_eq!(
        path.vertices,
        vec![v(0), v(1), v(4), v(3), v(5)],
        "the cheap detour through 4 beats both direct arms"
    );
}

#[test]
fn an_unreachable_destination_is_none_rather_than_an_empty_path() {
    // A zero-length path and no path at all are different answers. Returning an empty
    // vector for the second makes them the same.
    let graph = build(3, &[edge(0, 1, 1.0)]);
    let found = shortest_path(&graph, v(0), v(2), &any(), &Budget::generous())
        .expect("no negative edge here");
    assert_eq!(found.found, None);
}

#[test]
fn a_path_from_a_vertex_to_itself_is_the_trivial_one() {
    let graph = build(2, &[edge(0, 1, 1.0)]);
    let found = shortest_path(&graph, v(0), v(0), &any(), &Budget::generous())
        .expect("no negative edge here");
    let path = found.found.expect("a vertex reaches itself");
    assert_eq!(path.vertices, vec![v(0)]);
    assert_eq!(path.cost, 0.0);
    assert_eq!(path.hops(), 0);
}

#[test]
fn a_negative_weight_is_refused_rather_than_mishandled() {
    // Dijkstra's assumption is that settling a vertex fixes its distance; a negative edge
    // breaks it and the answer is wrong rather than slow.
    //
    // This fixture is the reason the check is a property of the graph rather than a test
    // applied to each edge as it is relaxed. Vertex 2 is settled at cost 1.0 by the direct
    // edge before the negative edge out of 1 is ever examined, so a check during
    // relaxation never fires — and returns 1.0 when the true shortest cost is -5.0.
    let graph = build(3, &[edge(0, 1, 5.0), edge(1, 2, -10.0), edge(0, 2, 1.0)]);

    let refused = shortest_path(&graph, v(0), v(2), &any(), &Budget::generous());
    assert_eq!(refused, Err(sankhya_graph_algo::paths::NegativeWeight));
    assert!(refused
        .unwrap_err()
        .to_string()
        .contains("wrong rather than slow"));

    // And the refusal is not confusable with 'no route exists', which is the whole reason
    // it is a separate type rather than a None.
    let unreachable = shortest_path(
        &build(3, &[edge(0, 1, 1.0)]),
        v(0),
        v(2),
        &any(),
        &Budget::generous(),
    );
    assert_eq!(unreachable.map(|b| b.found), Ok(None));
}

#[test]
fn k_shortest_matches_exhaustive_enumeration_in_order() {
    let graph = diamond_with_detours();
    let reference = all_simple_paths(&graph, v(0), v(5));

    let found = k_shortest_loopless(&graph, v(0), v(5), 4, &any(), &Budget::generous())
        .expect("the fixture has no negative edge");
    assert_eq!(found.found.len(), 4, "the fixture has at least four routes");

    for (rank, (got, want)) in found.found.iter().zip(reference.iter()).enumerate() {
        assert_eq!(
            got.cost, want.cost,
            "rank {rank}: cost disagrees with brute force"
        );
        assert_eq!(got.vertices, want.vertices, "rank {rank}: route disagrees");
    }
}

#[test]
fn k_shortest_returns_paths_not_walks() {
    // The hazard the design notes record. A common routine sold under this name returns the
    // k smallest *walk* lengths, and a walk may revisit a vertex freely. A route that
    // visits the same vertex three times is not a route, and nothing about the result's
    // shape says which kind you were given.
    let graph = build(
        5,
        &[
            edge(0, 1, 1.0),
            edge(1, 2, 1.0),
            edge(2, 1, 1.0), // a two-cycle a walk would happily go round
            edge(2, 3, 1.0),
            edge(1, 3, 8.0),
            edge(3, 4, 1.0),
        ],
    );

    let found = k_shortest_loopless(&graph, v(0), v(4), 8, &any(), &Budget::generous())
        .expect("the fixture has no negative edge");
    assert!(!found.found.is_empty(), "0 reaches 4");
    for path in &found.found {
        assert!(
            path.is_loopless(),
            "a returned route revisits a vertex: {:?} — this is a walk, not a path",
            path.vertices
        );
    }
}

#[test]
fn k_shortest_returns_distinct_routes_ascending_in_cost() {
    let graph = diamond_with_detours();
    let found = k_shortest_loopless(&graph, v(0), v(5), 5, &any(), &Budget::generous())
        .expect("the fixture has no negative edge");

    for pair in found.found.windows(2) {
        let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        assert!(a.cost <= b.cost, "results must ascend in cost");
        assert_ne!(a.vertices, b.vertices, "results must be distinct");
    }
}

#[test]
fn asking_for_more_routes_than_exist_returns_what_there_is() {
    let graph = build(3, &[edge(0, 1, 1.0), edge(1, 2, 1.0)]);
    let found = k_shortest_loopless(&graph, v(0), v(2), 10, &any(), &Budget::generous())
        .expect("the fixture has no negative edge");

    assert_eq!(found.found.len(), 1, "there is exactly one route");
    assert!(
        found.is_complete(),
        "exhausting the routes is not truncation; there was nothing left to find"
    );
}

#[test]
fn a_simple_cycle_is_reported_once_not_once_per_starting_point() {
    // Without rotation-normalising, the triangle below is found three times — once from
    // each member — and a count of cycles becomes a count of cycles times their length.
    let graph = build(3, &[edge(0, 1, 1.0), edge(1, 2, 1.0), edge(2, 0, 1.0)]);
    let found = cycles(&graph, &[v(0), v(1), v(2)], &any(), &Budget::generous());

    assert_eq!(found.found.len(), 1, "one triangle, reported once");
    let cycle = found.found.first().expect("one cycle");
    assert_eq!(
        cycle.vertices,
        vec![v(0), v(1), v(2), v(0)],
        "normalised to start at the smallest vertex and closed"
    );
}

#[test]
fn cycle_enumeration_is_not_all_pairs_distance() {
    // The other naming hazard: one well-known name covers both simple-cycle enumeration and
    // all-pairs shortest path. This graph has a route from 0 to 2 and no cycle at all, so a
    // routine answering the distance question returns results where the correct answer is
    // empty.
    let graph = build(3, &[edge(0, 1, 1.0), edge(1, 2, 1.0)]);
    let found = cycles(&graph, &[v(0), v(1), v(2)], &any(), &Budget::generous());

    assert!(
        found.found.is_empty(),
        "an acyclic graph has no cycles, however many paths it has: {:?}",
        found.found
    );
    assert!(
        found.is_complete(),
        "and that emptiness is the whole answer"
    );
}

#[test]
fn two_separate_cycles_are_both_found() {
    let graph = build(
        6,
        &[
            edge(0, 1, 1.0),
            edge(1, 0, 1.0),
            edge(3, 4, 1.0),
            edge(4, 5, 1.0),
            edge(5, 3, 1.0),
        ],
    );
    let seeds: Vec<VertexId> = (0..6).map(v).collect();
    let found = cycles(&graph, &seeds, &any(), &Budget::generous());

    assert_eq!(found.found.len(), 2);
    let lengths: Vec<usize> = found.found.iter().map(Path::hops).collect();
    assert_eq!(lengths, vec![2, 3], "a two-cycle and a three-cycle");
}

#[test]
fn a_self_loop_is_not_a_cycle_of_length_two() {
    let graph = build(2, &[edge(0, 0, 1.0), edge(0, 1, 1.0)]);
    let found = cycles(&graph, &[v(0), v(1)], &any(), &Budget::generous());
    assert!(
        found.found.is_empty(),
        "a self-loop has no interior and is not a circuit through other vertices"
    );
}

#[test]
fn stopping_early_is_distinguishable_from_finding_nothing() {
    // The distinction that matters most in this file. Cycle enumeration is exponential, so
    // a real query will hit its bound, and "no cycles" versus "stopped looking" are
    // conclusions with opposite meanings.
    let mut edges = Vec::new();
    for i in 0..8u32 {
        for j in 0..8u32 {
            if i != j {
                edges.push(edge(i, j, 1.0));
            }
        }
    }
    let graph = build(8, &edges);
    let seeds: Vec<VertexId> = (0..8).map(v).collect();

    let cut = cycles(
        &graph,
        &seeds,
        &any(),
        &Budget::generous().with_max_results(5),
    );
    assert_eq!(cut.found.len(), 5);
    assert!(
        !cut.is_complete(),
        "a complete graph has far more than five cycles"
    );
    assert_eq!(cut.complete_only(), None);

    let empty = cycles(
        &build(2, &[edge(0, 1, 1.0)]),
        &[v(0), v(1)],
        &any(),
        &Budget::generous(),
    );
    assert!(empty.found.is_empty());
    assert!(
        empty.is_complete(),
        "an empty complete answer and an empty truncated one must differ"
    );
}

#[test]
fn a_cycle_result_carries_the_cost_of_going_round() {
    let graph = build(3, &[edge(0, 1, 2.0), edge(1, 2, 3.0), edge(2, 0, 4.0)]);
    let found = cycles(&graph, &[v(0)], &any(), &Budget::generous());
    let cycle = found.found.first().expect("one cycle");
    assert_eq!(cycle.cost, 9.0, "2 + 3 + 4 all the way round");
}
