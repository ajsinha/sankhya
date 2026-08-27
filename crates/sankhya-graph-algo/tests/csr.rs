//! The adjacency structure holds what was put into it, in the order traversal needs.
//!
//! Two properties carry most of the weight here. A vertex's edges must be **contiguous**,
//! or every traversal pays a filter per hop; and within that run they must be **ordered by
//! time**, or the binary search in `starting_from` returns the wrong slice --- silently,
//! and only for temporal queries.

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

use sankhya_graph_algo::csr::{Adjacency, AdjacencyBuilder, Edge, Validity};
use sankhya_graph_algo::ids::{EdgeMask, EdgeType, VertexId, VertexType};
use sankhya_graph_algo::Interner;

fn v(n: u32) -> VertexId {
    VertexId(n)
}

fn edge(source: u32, target: u32, kind: u16, from: i64) -> Edge {
    Edge {
        source: v(source),
        target: v(target),
        edge_type: EdgeType(kind),
        validity: Validity::from(from),
        weight: 1.0,
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

#[test]
fn a_vertexs_edges_are_contiguous_and_time_ordered() {
    // Deliberately offered out of time order, and interleaved with another vertex's, so
    // that a builder which merely appended would fail both halves of this.
    let graph = build(
        4,
        &[
            edge(0, 3, 0, 300),
            edge(1, 2, 0, 50),
            edge(0, 1, 0, 100),
            edge(0, 2, 0, 200),
        ],
    );

    let out = graph.out_edges(v(0), EdgeType(0));
    assert_eq!(out.len(), 3, "vertex 0 has three out-edges");
    let starts: Vec<i64> = out.iter().map(|a| a.validity.from).collect();
    assert_eq!(
        starts,
        vec![100, 200, 300],
        "a vertex's run must ascend in validity start, or the binary search in \
         `starting_from` returns the wrong slice"
    );
    let targets: Vec<u32> = out.targets().iter().map(|t| t.0).collect();
    assert_eq!(targets, vec![1, 2, 3]);
}

#[test]
fn the_reverse_index_finds_edges_by_their_target() {
    let graph = build(3, &[edge(0, 2, 0, 10), edge(1, 2, 0, 20)]);

    let incoming = graph.in_edges(v(2), EdgeType(0));
    assert_eq!(incoming.len(), 2, "two edges arrive at vertex 2");
    let sources: Vec<u32> = incoming.targets().iter().map(|t| t.0).collect();
    assert_eq!(
        sources,
        vec![0, 1],
        "the reverse index's far end is, for an in-edge, the source"
    );
    assert_eq!(graph.in_edges(v(0), EdgeType(0)).len(), 0);
}

#[test]
fn each_edge_type_gets_its_own_segment() {
    // The same pair connected two different ways. A traversal restricted to one type must
    // see one edge, not two, and must not have to filter to get there.
    let graph = build(2, &[edge(0, 1, 0, 10), edge(0, 1, 1, 20)]);

    assert_eq!(graph.edge_type_count(), 2);
    assert_eq!(graph.out_edges(v(0), EdgeType(0)).len(), 1);
    assert_eq!(graph.out_edges(v(0), EdgeType(1)).len(), 1);
    assert_eq!(
        graph.out_degree(v(0), &EdgeMask::of([EdgeType(0)])),
        1,
        "a mask of one type must not count the other type's edges"
    );
    assert_eq!(graph.out_degree(v(0), &graph.all_edge_types()), 2);
}

#[test]
fn an_empty_mask_permits_nothing_rather_than_everything() {
    // The failure this guards is a caller who forgets to populate the mask. Reading empty
    // as "unrestricted" turns that mistake into a full traversal that looks like an answer.
    let graph = build(2, &[edge(0, 1, 0, 10)]);
    let empty = EdgeMask::default();

    assert!(empty.is_empty());
    assert!(!empty.permits(EdgeType(0)));
    assert_eq!(graph.out_degree(v(0), &empty), 0);
}

#[test]
fn edges_after_an_instant_are_a_slice_not_a_filter() {
    let graph = build(
        5,
        &[
            edge(0, 1, 0, 100),
            edge(0, 2, 0, 200),
            edge(0, 3, 0, 300),
            edge(0, 4, 0, 400),
        ],
    );
    let out = graph.out_edges(v(0), EdgeType(0));

    let after: Vec<u32> = out.starting_from(250).map(|a| a.target.0).collect();
    assert_eq!(
        after,
        vec![3, 4],
        "only the edges beginning at or after 250"
    );

    let before: Vec<u32> = out.starting_by(250).map(|a| a.target.0).collect();
    assert_eq!(before, vec![1, 2], "and its complement");

    // The boundary belongs to `starting_from`: an edge beginning exactly when a path
    // arrives is usable by it.
    let inclusive: Vec<u32> = out.starting_from(300).map(|a| a.target.0).collect();
    assert_eq!(inclusive, vec![3, 4]);
}

#[test]
fn an_edge_is_live_up_to_but_not_at_its_end() {
    // Half-open. A closed interval gives an instant where an ending edge and a starting one
    // are both live, and a time-respecting traversal hops straight through it.
    let live = Validity {
        from: 10,
        until: 20,
    };
    assert!(live.contains(10), "live at its start");
    assert!(live.contains(19));
    assert!(!live.contains(20), "not live at its end");

    let ends_at_20 = Validity { from: 0, until: 20 };
    let starts_at_20 = Validity {
        from: 20,
        until: 30,
    };
    assert!(
        !ends_at_20.overlaps(starts_at_20.from, starts_at_20.until),
        "an edge ending exactly when another starts must not overlap it"
    );
}

#[test]
fn an_edge_to_a_vertex_that_does_not_exist_is_refused() {
    // Refused at build rather than ignored at traversal. Ignoring it makes the graph
    // answer 'not connected' for a pair the source data connects.
    let mut b = AdjacencyBuilder::new(2);
    let Err(out_of_range) = b.push(edge(0, 7, 0, 10)) else {
        panic!("an edge naming vertex 7 in a two-vertex graph must be refused");
    };

    assert_eq!(out_of_range.vertex, v(7));
    assert!(out_of_range.to_string().contains("not connected"));
}

#[test]
fn the_same_edges_in_any_order_build_the_same_structure() {
    // The property the incremental-hydration test depends on: a rebuild must be
    // byte-identical, so a difference between incremental and full is a real difference.
    let edges = [
        edge(0, 1, 0, 100),
        edge(0, 2, 0, 100),
        edge(1, 2, 1, 50),
        edge(2, 0, 0, 75),
    ];
    let forward = build(3, &edges);
    let mut reversed = edges;
    reversed.reverse();
    let backward = build(3, &reversed);

    for vertex in 0..3u32 {
        for kind in 0..2u16 {
            let a: Vec<u32> = forward
                .out_edges(v(vertex), EdgeType(kind))
                .targets()
                .iter()
                .map(|t| t.0)
                .collect();
            let b: Vec<u32> = backward
                .out_edges(v(vertex), EdgeType(kind))
                .targets()
                .iter()
                .map(|t| t.0)
                .collect();
            assert_eq!(
                a, b,
                "vertex {vertex} type {kind} must not depend on input order"
            );
        }
    }
}

#[test]
fn two_edges_at_the_same_instant_still_order_totally() {
    // Ties broken by target. Without a total order the arrays differ between builds of the
    // same data, and the incremental-equals-full test fails for a reason that is not a bug.
    let graph = build(
        4,
        &[edge(0, 3, 0, 100), edge(0, 1, 0, 100), edge(0, 2, 0, 100)],
    );
    let targets: Vec<u32> = graph
        .out_edges(v(0), EdgeType(0))
        .targets()
        .iter()
        .map(|t| t.0)
        .collect();
    assert_eq!(targets, vec![1, 2, 3]);
}

#[test]
fn a_vertex_with_no_edges_and_one_that_does_not_exist_both_read_empty() {
    let graph = build(3, &[edge(0, 1, 0, 10)]);
    assert_eq!(graph.out_edges(v(2), EdgeType(0)).len(), 0, "no edges");
    assert_eq!(graph.out_edges(v(99), EdgeType(0)).len(), 0, "no vertex");
    assert_eq!(graph.out_edges(v(0), EdgeType(99)).len(), 0, "no such type");
}

#[test]
fn a_key_offered_under_two_types_is_a_conflict_rather_than_last_writer_wins() {
    // A vertex whose type depends on scan order makes every type-filtered traversal
    // non-deterministic, which is a wrong answer that changes between runs.
    let mut interner = Interner::new();
    assert_eq!(interner.intern(b"a", VertexType(0)), Ok(VertexId(0)));
    assert_eq!(
        interner.intern(b"a", VertexType(0)),
        Ok(VertexId(0)),
        "the same key and type is the same vertex"
    );

    let Err(conflict) = interner.intern(b"a", VertexType(1)) else {
        panic!("the same key under a second type must be refused");
    };
    assert_eq!(conflict.recorded, VertexType(0));
    assert_eq!(conflict.offered, VertexType(1));
}

#[test]
fn ids_are_dense_and_map_back_to_their_keys() {
    let mut interner = Interner::new();
    for (i, key) in [b"alpha".as_slice(), b"beta", b"gamma"].iter().enumerate() {
        assert_eq!(
            interner.intern(key, VertexType(0)),
            Ok(VertexId(u32::try_from(i).unwrap())),
            "ids must be handed out densely from zero, or they cannot index the arrays"
        );
    }
    assert_eq!(interner.len(), 3);
    assert_eq!(interner.key(VertexId(1)), Some(b"beta".as_slice()));
    assert_eq!(interner.lookup(b"gamma"), Some(VertexId(2)));
    assert_eq!(interner.lookup(b"absent"), None);
}
