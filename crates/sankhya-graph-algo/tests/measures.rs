//! Components, centrality, communities and multiplicative influence.
//!
//! Two properties recur and matter more than the numbers: every result is **reproducible**
//! --- the same graph gives the same answer, including the ordering of ties --- and every
//! measure says what it is rather than what a caller might hope. `rank` is a damped random
//! walk, `betweenness_estimate` is a sample, and `influence` multiplies where every other
//! routine in the crate adds.

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
use sankhya_graph_algo::centrality::{betweenness_estimate, degree, rank, Direction};
use sankhya_graph_algo::community::{detect, modularity};
use sankhya_graph_algo::components::{strongly_connected, weakly_connected};
use sankhya_graph_algo::csr::{Adjacency, AdjacencyBuilder, Edge, Validity};
use sankhya_graph_algo::ids::{EdgeMask, EdgeType, VertexId};
use sankhya_graph_algo::product::{influence, influence_between, Damping};

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

// --- components -----------------------------------------------------------

#[test]
fn weak_components_ignore_direction_and_strong_ones_do_not() {
    // 0 -> 1 -> 2 is one weak component and three strong ones: nothing can get back.
    let graph = build(3, &[edge(0, 1, 1.0), edge(1, 2, 1.0)]);

    let weak = weakly_connected(&graph, &any(), &Budget::generous());
    assert_eq!(
        weak.found.count(),
        1,
        "following edges either way, all one group"
    );

    let strong = strongly_connected(&graph, &any(), &Budget::generous());
    assert_eq!(
        strong.found.count(),
        3,
        "mutual reachability requires a way back, and there is none"
    );
}

#[test]
fn a_cycle_is_one_strong_component() {
    let graph = build(
        5,
        &[
            edge(0, 1, 1.0),
            edge(1, 2, 1.0),
            edge(2, 0, 1.0),
            edge(2, 3, 1.0),
            edge(3, 4, 1.0),
        ],
    );
    let strong = strongly_connected(&graph, &any(), &Budget::generous());

    assert_eq!(strong.found.count(), 3, "the triangle, then 3, then 4");
    let triangle = strong.found.label_of(v(0));
    assert_eq!(strong.found.label_of(v(1)), triangle);
    assert_eq!(strong.found.label_of(v(2)), triangle);
    assert_ne!(strong.found.label_of(v(3)), triangle);
}

#[test]
fn component_labels_are_ordered_by_their_smallest_member() {
    // Reproducibility. An arbitrary labelling makes two hydrations of identical data
    // compare unequal, and the incremental-equals-full test then fails for no reason.
    let graph = build(
        4,
        &[
            edge(2, 3, 1.0),
            edge(3, 2, 1.0),
            edge(0, 1, 1.0),
            edge(1, 0, 1.0),
        ],
    );
    let strong = strongly_connected(&graph, &any(), &Budget::generous());

    assert_eq!(strong.found.label_of(v(0)), Some(0));
    assert_eq!(strong.found.label_of(v(1)), Some(0));
    assert_eq!(strong.found.label_of(v(2)), Some(1));
    assert_eq!(strong.found.label_of(v(3)), Some(1));
}

#[test]
fn an_isolated_vertex_is_its_own_component() {
    let graph = build(3, &[edge(0, 1, 1.0), edge(1, 0, 1.0)]);
    let weak = weakly_connected(&graph, &any(), &Budget::generous());
    assert_eq!(weak.found.count(), 2);
    assert_eq!(weak.found.largest(), 2);
}

#[test]
fn a_long_chain_does_not_overflow_the_stack() {
    // Tarjan's recursive form dies here. Real graphs have long chains, so the iterative
    // form is not a refinement.
    let n = 50_000usize;
    let edges: Vec<Edge> = (0..n.saturating_sub(1))
        .map(|i| {
            edge(
                u32::try_from(i).unwrap_or(0),
                u32::try_from(i + 1).unwrap_or(0),
                1.0,
            )
        })
        .collect();
    let graph = build(n, &edges);

    let strong = strongly_connected(
        &graph,
        &any(),
        &Budget::generous().with_max_visits(usize::MAX),
    );
    assert_eq!(strong.found.count(), u32::try_from(n).unwrap_or(0));
}

// --- centrality -----------------------------------------------------------

#[test]
fn degree_counts_the_direction_it_is_asked_for() {
    let graph = build(
        4,
        &[
            edge(1, 0, 1.0),
            edge(2, 0, 1.0),
            edge(3, 0, 1.0),
            edge(0, 1, 1.0),
        ],
    );

    let inbound = degree(&graph, &any(), Direction::In);
    assert_eq!(inbound.first().map(|s| s.vertex), Some(v(0)));
    assert_eq!(inbound.first().map(|s| s.value), Some(3.0));

    let outbound = degree(&graph, &any(), Direction::Out);
    assert_eq!(
        outbound.first().map(|s| s.value),
        Some(1.0),
        "everyone has one"
    );

    let both = degree(&graph, &any(), Direction::Both);
    assert_eq!(both.first().map(|s| s.value), Some(4.0));
}

#[test]
fn rank_scores_sum_to_one_even_with_a_dangling_vertex() {
    // A vertex with no out-edges has mass and nowhere to send it. Discarding that mass
    // makes the scores stop summing to one, and two runs over graphs with different
    // dangling counts stop being comparable.
    let graph = build(
        4,
        &[
            edge(0, 1, 1.0),
            edge(1, 2, 1.0),
            edge(2, 0, 1.0),
            edge(0, 3, 1.0),
        ],
    );
    let scored = rank(&graph, &any(), 0.85, 60, &Budget::generous());

    let total: f64 = scored.found.iter().map(|s| s.value).sum();
    assert!(
        (total - 1.0).abs() < 1e-9,
        "scores must sum to one; they sum to {total}"
    );
}

#[test]
fn rank_puts_the_most_pointed_at_vertex_first() {
    let graph = build(
        5,
        &[
            edge(1, 0, 1.0),
            edge(2, 0, 1.0),
            edge(3, 0, 1.0),
            edge(4, 0, 1.0),
        ],
    );
    let scored = rank(&graph, &any(), 0.85, 50, &Budget::generous());
    assert_eq!(scored.found.first().map(|s| s.vertex), Some(v(0)));
}

#[test]
fn rank_is_reproducible_including_its_ties() {
    // Four symmetric vertices tie exactly. Without a tie-break on the vertex id, "the top
    // three" depends on iteration order and changes between runs.
    let graph = build(
        5,
        &[
            edge(1, 0, 1.0),
            edge(2, 0, 1.0),
            edge(3, 0, 1.0),
            edge(4, 0, 1.0),
        ],
    );
    let first = rank(&graph, &any(), 0.85, 40, &Budget::generous());
    let second = rank(&graph, &any(), 0.85, 40, &Budget::generous());

    let order = |b: &sankhya_graph_algo::Bounded<Vec<sankhya_graph_algo::centrality::Score>>| {
        b.found.iter().map(|s| s.vertex).collect::<Vec<_>>()
    };
    assert_eq!(order(&first), order(&second));
    assert_eq!(
        order(&first),
        vec![v(0), v(1), v(2), v(3), v(4)],
        "ties broken by ascending vertex"
    );
}

#[test]
fn betweenness_finds_the_intermediary_not_the_endpoints() {
    // The measure's whole reason for existing. Vertex 2 is on every route across the
    // bridge; degree does not distinguish it from its neighbours.
    let graph = build(
        5,
        &[
            edge(0, 1, 1.0),
            edge(1, 2, 1.0),
            edge(2, 3, 1.0),
            edge(3, 4, 1.0),
        ],
    );
    let sources: Vec<VertexId> = (0..5).map(v).collect();
    let scored = betweenness_estimate(&graph, &any(), &sources, &Budget::generous());

    assert_eq!(
        scored.found.first().map(|s| s.vertex),
        Some(v(2)),
        "the middle of a chain lies on the most routes"
    );
    let endpoints: Vec<f64> = scored
        .found
        .iter()
        .filter(|s| s.vertex == v(0) || s.vertex == v(4))
        .map(|s| s.value)
        .collect();
    assert_eq!(endpoints, vec![0.0, 0.0], "an endpoint is between nothing");
}

#[test]
fn a_sampled_betweenness_says_it_sampled() {
    let graph = build(4, &[edge(0, 1, 1.0), edge(1, 2, 1.0), edge(2, 3, 1.0)]);
    let sources: Vec<VertexId> = (0..4).map(v).collect();
    let cut = betweenness_estimate(
        &graph,
        &any(),
        &sources,
        &Budget::generous().with_max_visits(2),
    );
    assert!(
        !cut.is_complete(),
        "an estimate from part of the source set must not present as exact"
    );
}

// --- communities ----------------------------------------------------------

#[test]
fn two_dense_clusters_joined_by_one_edge_are_two_communities() {
    let graph = build(
        6,
        &[
            // A triangle.
            edge(0, 1, 1.0),
            edge(1, 2, 1.0),
            edge(2, 0, 1.0),
            // Another.
            edge(3, 4, 1.0),
            edge(4, 5, 1.0),
            edge(5, 3, 1.0),
            // One thin bridge.
            edge(2, 3, 1.0),
        ],
    );
    let found = detect(&graph, &any(), 50, &Budget::generous());

    assert_eq!(
        found.found.count(),
        2,
        "the bridge is not enough to merge them"
    );
    assert_eq!(found.found.label_of(v(0)), found.found.label_of(v(1)));
    assert_ne!(found.found.label_of(v(0)), found.found.label_of(v(4)));
}

#[test]
fn the_monster_community_does_not_form() {
    // The failure that label propagation could not avoid, and the reason this is
    // modularity optimisation instead. Two triangles joined by one edge: propagation
    // collapses them into one group because nothing in its rule prefers keeping them
    // apart. Modularity *falls* when they merge, so the objective resists it.
    let graph = build(
        6,
        &[
            edge(0, 1, 1.0),
            edge(1, 2, 1.0),
            edge(2, 0, 1.0),
            edge(3, 4, 1.0),
            edge(4, 5, 1.0),
            edge(5, 3, 1.0),
            edge(2, 3, 1.0),
        ],
    );
    let split = detect(&graph, &any(), 50, &Budget::generous());
    assert_eq!(split.found.count(), 2);

    // And the grouping it chose genuinely scores better than the collapsed one, which is
    // the claim the algorithm rests on rather than an accident of where it stopped.
    let collapsed = weakly_connected(&graph, &any(), &Budget::generous());
    assert_eq!(
        collapsed.found.count(),
        1,
        "everything is one weak component"
    );
    assert!(
        modularity(&graph, &any(), &split.found) > modularity(&graph, &any(), &collapsed.found),
        "the split grouping must score above the collapsed one: {} vs {}",
        modularity(&graph, &any(), &split.found),
        modularity(&graph, &any(), &collapsed.found)
    );
}

#[test]
fn a_grouping_with_no_information_scores_near_zero() {
    // Modularity is what lets a caller say whether communities are a finding or noise. On
    // a complete graph every grouping is arbitrary and the score should say so.
    let mut edges = Vec::new();
    for i in 0..6u32 {
        for j in 0..6u32 {
            if i != j {
                edges.push(edge(i, j, 1.0));
            }
        }
    }
    let graph = build(6, &edges);
    let found = detect(&graph, &any(), 50, &Budget::generous());
    let score = modularity(&graph, &any(), &found.found);

    assert!(
        score.abs() < 0.15,
        "a complete graph has no community structure; scored {score}"
    );
}

#[test]
fn community_detection_gives_the_same_answer_twice() {
    // Label propagation is order-dependent in its usual randomised form, and a community
    // that appears in one run and not the next is worse than none, because someone acts
    // on it. The visiting order here is fixed and ties go to the smallest label.
    let graph = build(
        6,
        &[
            edge(0, 1, 1.0),
            edge(1, 2, 1.0),
            edge(2, 0, 1.0),
            edge(3, 4, 1.0),
            edge(4, 5, 1.0),
            edge(5, 3, 1.0),
            edge(2, 3, 1.0),
        ],
    );
    let first = detect(&graph, &any(), 50, &Budget::generous());
    let second = detect(&graph, &any(), 50, &Budget::generous());
    assert_eq!(first.found, second.found);
}

// --- multiplicative influence ---------------------------------------------

#[test]
fn influence_multiplies_along_a_chain_rather_than_adding() {
    // The distinction the module exists for. 0.5 then 0.4 is 0.2, not 0.9. A shortest-path
    // routine would report 0.9 and the number would mean nothing.
    let graph = build(3, &[edge(0, 1, 0.5), edge(1, 2, 0.4)]);
    let got = influence_between(
        &graph,
        v(0),
        v(2),
        &any(),
        Damping::none(),
        &Budget::generous(),
    );

    assert!(
        (got.found - 0.2).abs() < 1e-12,
        "two hops of 0.5 and 0.4 give {}, expected 0.2",
        got.found
    );
}

#[test]
fn two_routes_to_the_same_place_add_their_contributions() {
    // Distinct routes contribute independently: 30% by one chain and 25% by another is 55%.
    let graph = build(
        4,
        &[
            edge(0, 1, 0.6),
            edge(1, 3, 0.5),
            edge(0, 2, 0.5),
            edge(2, 3, 0.5),
        ],
    );
    let got = influence_between(
        &graph,
        v(0),
        v(3),
        &any(),
        Damping::none(),
        &Budget::generous(),
    );

    assert!(
        (got.found - 0.55).abs() < 1e-12,
        "0.6*0.5 + 0.5*0.5 = 0.55, got {}",
        got.found
    );
}

#[test]
fn damping_makes_a_long_chain_count_for_less_than_a_short_one() {
    let long = build(4, &[edge(0, 1, 1.0), edge(1, 2, 1.0), edge(2, 3, 1.0)]);
    let short = build(4, &[edge(0, 3, 1.0)]);

    let damping = Damping::of(0.5, 1e-9);
    let far = influence_between(&long, v(0), v(3), &any(), damping, &Budget::generous());
    let near = influence_between(&short, v(0), v(3), &any(), damping, &Budget::generous());

    assert!(
        far.found < near.found,
        "three damped hops ({}) must count for less than one ({})",
        far.found,
        near.found
    );
}

#[test]
fn a_cycle_terminates_because_of_the_pruning_floor() {
    // Without a floor above zero, a cycle is extended until the visit budget ends it, and
    // every result becomes truncated. The floor is the termination condition, not an
    // optimisation.
    let graph = build(3, &[edge(0, 1, 0.9), edge(1, 2, 0.9), edge(2, 0, 0.9)]);
    let got = influence(
        &graph,
        &[v(0)],
        &any(),
        Damping::of(1.0, 0.01),
        &Budget::generous(),
    );

    assert!(
        got.is_complete(),
        "the floor should end the search before any bound does: {:?}",
        got.truncation
    );
    assert!(!got.found.is_empty());
}

#[test]
fn a_seed_is_not_counted_as_influencing_itself() {
    let graph = build(2, &[edge(0, 1, 0.5)]);
    let got = influence(
        &graph,
        &[v(0)],
        &any(),
        Damping::none(),
        &Budget::generous(),
    );

    assert_eq!(
        got.found.iter().find(|s| s.vertex == v(0)).map(|s| s.value),
        None,
        "the seed's own starting weight is not influence upon itself"
    );
    assert_eq!(got.found.first().map(|s| s.vertex), Some(v(1)));
}
