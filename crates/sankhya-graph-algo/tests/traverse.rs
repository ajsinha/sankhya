//! Expansion is bounded, and a temporal path is only a path if time permits it.
//!
//! The test that matters most here is
//! `a_static_path_that_time_forbids_is_not_returned`. Static reachability over a temporal
//! graph over-reports, always in the same direction, and the over-report looks exactly like
//! a finding. Everything else in this file is a bound doing its job.

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
use sankhya_graph_algo::traverse::{reachable, time_respecting, TimeConstraints};

fn v(n: u32) -> VertexId {
    VertexId(n)
}

fn at(source: u32, target: u32, from: i64) -> Edge {
    weighted(source, target, from, 1.0)
}

fn weighted(source: u32, target: u32, from: i64, weight: f64) -> Edge {
    Edge {
        source: v(source),
        target: v(target),
        edge_type: EdgeType(0),
        validity: Validity::from(from),
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

fn found(result: &[sankhya_graph_algo::traverse::Reached]) -> Vec<u32> {
    let mut ids: Vec<u32> = result.iter().map(|r| r.vertex.0).collect();
    ids.sort_unstable();
    ids
}

#[test]
fn expansion_reports_the_shortest_hop_count_to_each_vertex() {
    // A diamond: 3 is reachable in two hops by either arm. Breadth-first must report the
    // smaller depth, not whichever arm it happened to walk first.
    let graph = build(4, &[at(0, 1, 10), at(0, 2, 10), at(1, 3, 20), at(2, 3, 20)]);
    let out = reachable(&graph, &[v(0)], &any(), &Budget::generous());

    assert!(out.is_complete(), "nothing here should hit a bound");
    assert_eq!(found(&out.found), vec![0, 1, 2, 3]);
    let three = out.found.iter().find(|r| r.vertex == v(3)).unwrap();
    assert_eq!(three.depth, 2);
    let zero = out.found.iter().find(|r| r.vertex == v(0)).unwrap();
    assert_eq!(zero.depth, 0);
    assert_eq!(zero.via, None, "a seed was not reached from anywhere");
}

#[test]
fn a_static_path_that_time_forbids_is_not_returned() {
    // 0 -> 1 happens at t=100. 1 -> 2 happened at t=50, *before* it. The edges exist, so a
    // static traversal reports 2 as reachable from 0. Nothing could have travelled that
    // route: it would have had to leave 1 fifty units before it arrived.
    //
    // This is the whole reason `time_respecting` is a separate function rather than a flag.
    let graph = build(3, &[at(0, 1, 100), at(1, 2, 50)]);

    let static_walk = reachable(&graph, &[v(0)], &any(), &Budget::generous());
    assert_eq!(
        found(&static_walk.found),
        vec![0, 1, 2],
        "static reachability follows both edges, because both exist"
    );

    let timed = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none(),
        &Budget::generous(),
    );
    assert_eq!(
        found(&timed.found),
        vec![0, 1],
        "vertex 2 is not reachable in time: the onward edge fired before the inbound one"
    );
}

#[test]
fn a_vertex_is_reported_at_its_earliest_arrival() {
    // Two routes to 3: a direct late one and an indirect early one. Arriving earlier can
    // only permit more onward edges, so the earliest is the one worth keeping.
    let graph = build(4, &[at(0, 3, 900), at(0, 1, 10), at(1, 3, 20)]);
    let out = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none(),
        &Budget::generous(),
    );

    let three = out.found.iter().find(|r| r.vertex == v(3)).unwrap();
    assert_eq!(three.at, 20, "the earlier arrival, not the direct one");
    assert_eq!(three.via, Some(v(1)));
}

#[test]
fn an_edge_outside_the_window_is_not_usable() {
    let graph = build(3, &[at(0, 1, 50), at(1, 2, 5_000)]);
    let out = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::window(0, 1_000),
        &Budget::generous(),
    );

    assert_eq!(
        found(&out.found),
        vec![0, 1],
        "the second edge fires after the window closes"
    );
}

#[test]
fn a_pause_longer_than_the_dwell_bound_breaks_the_path() {
    // Without an upper dwell bound, two events years apart chain into one path. The bound
    // is what says "these are related" rather than "these are both in the database".
    let graph = build(3, &[at(0, 1, 100), at(1, 2, 100_000)]);

    let unbounded = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none(),
        &Budget::generous(),
    );
    assert_eq!(found(&unbounded.found), vec![0, 1, 2]);

    let bounded = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none().with_max_dwell(1_000),
        &Budget::generous(),
    );
    assert_eq!(
        found(&bounded.found),
        vec![0, 1],
        "a pause of 99,900 exceeds a dwell bound of 1,000"
    );
}

#[test]
fn a_minimum_dwell_excludes_a_hop_taken_in_the_same_instant() {
    // Where timestamps are coarse, arriving and leaving at the same instant is usually an
    // artefact of granularity rather than an observed sequence.
    let graph = build(3, &[at(0, 1, 100), at(1, 2, 100)]);

    let permissive = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none(),
        &Budget::generous(),
    );
    assert_eq!(found(&permissive.found), vec![0, 1, 2]);

    let strict = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none().with_min_dwell(1),
        &Budget::generous(),
    );
    assert_eq!(found(&strict.found), vec![0, 1]);
}

#[test]
fn conservation_refuses_a_hop_that_carries_almost_nothing_onward() {
    // A large edge chained onto a negligible one is ordered in time and means nothing. The
    // conservation bound is what separates a route along which something moved from a pair
    // of unrelated edges that happen to be ordered.
    let graph = build(3, &[weighted(0, 1, 100, 1_000.0), weighted(1, 2, 200, 3.0)]);

    let unconstrained = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none(),
        &Budget::generous(),
    );
    assert_eq!(found(&unconstrained.found), vec![0, 1, 2]);

    let conserving = time_respecting(
        &graph,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none().with_conservation(0.9),
        &Budget::generous(),
    );
    assert_eq!(
        found(&conserving.found),
        vec![0, 1],
        "3 is not 90% of 1000, so the onward edge does not continue the route"
    );

    // And a hop that does conserve is still followed.
    let intact = build(
        3,
        &[weighted(0, 1, 100, 1_000.0), weighted(1, 2, 200, 995.0)],
    );
    let kept = time_respecting(
        &intact,
        &[v(0)],
        &any(),
        0,
        &TimeConstraints::none().with_conservation(0.9),
        &Budget::generous(),
    );
    assert_eq!(found(&kept.found), vec![0, 1, 2]);
}

#[test]
fn the_depth_bound_reports_that_it_stopped_early() {
    let graph = build(4, &[at(0, 1, 10), at(1, 2, 20), at(2, 3, 30)]);
    let out = reachable(
        &graph,
        &[v(0)],
        &any(),
        &Budget::generous().with_max_depth(1),
    );

    assert_eq!(found(&out.found), vec![0, 1]);
    assert!(!out.is_complete(), "there was more graph beyond the bound");
    assert!(out.truncation.by_depth);
    assert!(out
        .truncation
        .explain()
        .unwrap_or_default()
        .contains("depth limit"));
}

#[test]
fn a_bound_reached_exactly_at_the_edge_of_the_graph_is_not_truncation() {
    // Depth 1 on a graph one hop deep saw everything. Reporting truncation here would make
    // every complete answer look partial, and callers would stop believing the flag.
    let graph = build(2, &[at(0, 1, 10)]);
    let out = reachable(
        &graph,
        &[v(0)],
        &any(),
        &Budget::generous().with_max_depth(1),
    );

    assert_eq!(found(&out.found), vec![0, 1]);
    assert!(
        out.is_complete(),
        "the depth limit coincided with the end of the graph; nothing was cut off"
    );
}

#[test]
fn a_high_degree_vertex_is_suppressed_and_named() {
    // In a power-law network expanding one hub touches most of the graph and returns paths
    // that mean nothing. Which hub was skipped is frequently the finding, so it is listed
    // rather than counted.
    let mut edges = vec![at(0, 1, 10)];
    for target in 2..40u32 {
        edges.push(at(1, target, 20));
    }
    let graph = build(40, &edges);
    let out = reachable(
        &graph,
        &[v(0)],
        &any(),
        &Budget::generous().with_max_degree(10),
    );

    assert_eq!(found(&out.found), vec![0, 1], "the hub itself is reported");
    assert_eq!(out.truncation.suppressed, vec![v(1)]);
    assert!(
        !out.is_complete(),
        "a suppressed hub makes the result a lower bound, not an answer"
    );
}

#[test]
fn a_truncated_result_never_looks_like_an_empty_one() {
    // The rule the whole `Bounded` type exists for. `complete_only` is the door anything
    // used as evidence must come through.
    let graph = build(3, &[at(0, 1, 10), at(1, 2, 20)]);
    let cut = reachable(
        &graph,
        &[v(0)],
        &any(),
        &Budget::generous().with_max_results(1),
    );

    assert!(!cut.is_complete());
    assert!(cut.truncation.by_results);
    assert_eq!(
        cut.complete_only(),
        None,
        "a partial result must not be readable as a complete one"
    );

    let whole = reachable(&graph, &[v(0)], &any(), &Budget::generous());
    assert!(whole.complete_only().is_some());
}

#[test]
fn a_cycle_terminates_rather_than_revisiting_forever() {
    let graph = build(3, &[at(0, 1, 10), at(1, 2, 20), at(2, 0, 30)]);
    let out = reachable(&graph, &[v(0)], &any(), &Budget::generous());

    assert_eq!(found(&out.found), vec![0, 1, 2]);
    assert_eq!(out.found.len(), 3, "each vertex reported exactly once");
}

#[test]
fn a_seed_outside_the_graph_is_ignored_rather_than_fatal() {
    let graph = build(2, &[at(0, 1, 10)]);
    let out = reachable(&graph, &[v(0), v(99)], &any(), &Budget::generous());
    assert_eq!(found(&out.found), vec![0, 1]);
}

#[test]
fn a_mask_that_permits_nothing_returns_only_the_seeds() {
    let graph = build(2, &[at(0, 1, 10)]);
    let out = reachable(&graph, &[v(0)], &EdgeMask::default(), &Budget::generous());
    assert_eq!(found(&out.found), vec![0]);
}
