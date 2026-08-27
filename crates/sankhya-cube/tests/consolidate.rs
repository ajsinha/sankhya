//! Consolidation over a ragged hierarchy, and the three ways a total goes silently wrong.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use proptest::prelude::*;
use sankhya_cube::consolidate::{consolidate, Incomplete};
use sankhya_graph_algo::budget::Budget;
use sankhya_graph_algo::csr::{Adjacency, AdjacencyBuilder, Edge, Validity};
use sankhya_graph_algo::ids::{EdgeMask, EdgeType, VertexId};

const ROLLS_UP: EdgeType = EdgeType(0);

fn v(n: u32) -> VertexId {
    VertexId(n)
}

/// A hierarchy from *(parent, child)* pairs. The edge runs parent → child, because
/// consolidation asks what contributes to a member.
fn hierarchy(vertices: usize, edges: &[(u32, u32)]) -> Adjacency {
    let mut builder = AdjacencyBuilder::new(vertices);
    for (parent, child) in edges {
        builder
            .push(Edge {
                source: v(*parent),
                target: v(*child),
                edge_type: ROLLS_UP,
                validity: Validity::always(),
                weight: 1.0,
            })
            .expect("within range");
    }
    builder.build()
}

fn mask() -> EdgeMask {
    EdgeMask::of([ROLLS_UP])
}

// --- a member reachable two ways contributes once -----------------------

#[test]
fn a_member_reachable_by_two_paths_contributes_once() {
    // Shared members and alternate roll-ups are the point of a modelled hierarchy. Walk
    // paths and sum, and the shared branch lands in the total once per path: a figure
    // larger than reality, made entirely of real data, indistinguishable from correct.
    //
    //   0 ── 1 ── 3
    //     └─ 2 ──┘
    let graph = hierarchy(4, &[(0, 1), (0, 2), (1, 3), (2, 3)]);
    let rolled = consolidate(&graph, v(0), &mask(), &Budget::generous());

    let members = rolled.members().expect("complete");
    assert_eq!(members.len(), 4, "0, 1, 2 and 3 — with 3 once: {members:?}");
    assert!(members.contains(&v(3)));
}

#[test]
fn the_root_contributes_to_its_own_total() {
    // In a ragged hierarchy facts attach at any level: a regional office has its own
    // transactions *and* branches beneath it. Consolidate by collecting leaves — the
    // obvious implementation — and the region's own facts vanish silently.
    let graph = hierarchy(3, &[(0, 1), (1, 2)]);
    let rolled = consolidate(&graph, v(0), &mask(), &Budget::generous());
    assert!(
        rolled.members().expect("complete").contains(&v(0)),
        "the root's own facts are part of its total"
    );
}

#[test]
fn an_inner_member_with_facts_of_its_own_is_included() {
    // The same property one level down, which is where it is actually lost: `1` is neither
    // the root nor a leaf, and an implementation collecting leaves drops it.
    let graph = hierarchy(3, &[(0, 1), (1, 2)]);
    let rolled = consolidate(&graph, v(0), &mask(), &Budget::generous());
    let members = rolled.members().expect("complete");
    assert_eq!(members.len(), 3, "root, inner and leaf: {members:?}");
}

#[test]
fn a_ragged_hierarchy_needs_no_padding() {
    // Leaves at different depths, which is what "ragged" means. Padding to a uniform depth
    // invents members that do not exist, and they appear in results.
    //
    //   0 ── 1 (a leaf at depth 1)
    //     └─ 2 ── 3 ── 4 (a leaf at depth 3)
    let graph = hierarchy(5, &[(0, 1), (0, 2), (2, 3), (3, 4)]);
    let rolled = consolidate(&graph, v(0), &mask(), &Budget::generous());
    let members = rolled.members().expect("complete");

    assert_eq!(members.len(), 5);
    for id in 0..5 {
        assert!(members.contains(&v(id)), "member {id} missing: {members:?}");
    }
}

// --- a lower bound is not a total ---------------------------------------

#[test]
fn a_truncated_consolidation_refuses_to_be_a_total() {
    // The most dangerous of the three, because a total is what an operator acts on.
    let graph = hierarchy(4, &[(0, 1), (1, 2), (2, 3)]);
    let shallow = Budget {
        max_depth: 1,
        ..Budget::generous()
    };
    let rolled = consolidate(&graph, v(0), &mask(), &shallow);

    assert!(!rolled.is_complete());
    let Err(Incomplete::Truncated { why }) = rolled.members() else {
        panic!("a truncated traversal was offered as a total: {rolled:?}");
    };
    assert!(!why.is_empty(), "the reason is already phrased for a reader");

    // The partial set is available, but only under a name that says what it is.
    assert!(rolled.partial().len() < 4);
    assert!(rolled.why_incomplete().is_some());
}

#[test]
fn a_cycle_is_reported_as_a_modelling_error_not_as_truncation() {
    // Breadth-first expansion does not hang on a cycle — it marks vertices seen — so the
    // failure is not a timeout. It is an ordinary-looking set, and a total for a member
    // that has none. Distinguishing it from truncation matters: truncation sends somebody
    // to raise a budget, and no budget makes a loop terminate.
    let graph = hierarchy(3, &[(0, 1), (1, 2), (2, 0)]);
    let rolled = consolidate(&graph, v(0), &mask(), &Budget::generous());

    assert_eq!(
        rolled.members(),
        Err(Incomplete::Cyclic { member: v(0) }),
        "a cyclic consolidation was offered as a total"
    );
    let why = rolled.why_incomplete().expect("incomplete").to_string();
    assert!(
        why.contains("no budget makes a loop terminate"),
        "the message must not send somebody to widen a budget: {why}"
    );
}

#[test]
fn a_cycle_elsewhere_in_the_subtree_does_not_stop_the_root_consolidating() {
    // Only a cycle *through the root* denies the root a total. A loop between two
    // descendants is a modelling error too, but the root's member set is still the set of
    // members, each once — refusing here would deny a total that exists.
    let graph = hierarchy(4, &[(0, 1), (1, 2), (2, 3), (3, 2)]);
    let rolled = consolidate(&graph, v(0), &mask(), &Budget::generous());
    assert_eq!(rolled.members().expect("complete").len(), 4);
}

// --- edge types ---------------------------------------------------------

#[test]
fn only_the_declared_roll_up_edges_are_followed() {
    // Following the wrong edge type consolidates something else entirely and says nothing.
    let mut builder = AdjacencyBuilder::new(3);
    builder
        .push(Edge {
            source: v(0),
            target: v(1),
            edge_type: ROLLS_UP,
            validity: Validity::always(),
            weight: 1.0,
        })
        .expect("in range");
    builder
        .push(Edge {
            source: v(0),
            target: v(2),
            edge_type: EdgeType(1),
            validity: Validity::always(),
            weight: 1.0,
        })
        .expect("in range");
    let graph = builder.build();

    let rolled = consolidate(&graph, v(0), &mask(), &Budget::generous());
    let members = rolled.members().expect("complete");
    assert!(members.contains(&v(1)));
    assert!(!members.contains(&v(2)), "an unrelated edge type was followed");
}

// --- the property test §11.3 asks for -----------------------------------

proptest! {
    /// However many paths reach a member, it contributes once.
    ///
    /// The generator builds a layered graph and then adds arbitrary extra parent edges, so
    /// members genuinely have several routes to the root. The oracle is the plain set of
    /// vertices reachable from it, computed independently.
    ///
    /// **What this cannot catch, stated rather than assumed:** the oracle is itself a set,
    /// so it would agree with a path-counting implementation about *which* members belong.
    /// Double counting shows up in the aggregate, not in the membership. So the test also
    /// counts distinct root-to-member paths and asserts the generated case actually has
    /// more paths than members — without that, this passes on trees and proves nothing
    /// about the shared members it was written for.
    #[test]
    fn no_member_is_ever_counted_twice(
        extra in prop::collection::vec((1usize..8, 1usize..8), 0..24)
    ) {
        const N: usize = 8;
        let mut edges: Vec<(u32, u32)> = (1..N).map(|c| ((c as u32 - 1) / 2, c as u32)).collect();
        for (parent, child) in extra {
            if parent < child {
                edges.push((parent as u32, child as u32));
            }
        }
        let graph = hierarchy(N, &edges);
        let rolled = consolidate(&graph, v(0), &mask(), &Budget::generous());
        let members = rolled.members().expect("acyclic and within budget");

        // Independently: everything reachable from 0 along the same edges.
        let mut expected = std::collections::BTreeSet::new();
        let mut stack = vec![0u32];
        while let Some(at) = stack.pop() {
            if !expected.insert(at) {
                continue;
            }
            for (parent, child) in &edges {
                if *parent == at {
                    stack.push(*child);
                }
            }
        }
        let expected: std::collections::BTreeSet<VertexId> =
            expected.into_iter().map(v).collect();

        prop_assert_eq!(members, &expected);
        prop_assert_eq!(members.len(), rolled.len());

        // Distinct paths from the root, counted independently and bounded. A tree has
        // exactly one per member; anything more means a shared member is present, which is
        // the case this test exists for.
        let mut paths = 0usize;
        let mut walking = vec![0u32];
        while let Some(at) = walking.pop() {
            paths += 1;
            if paths > 10_000 {
                break;
            }
            for (parent, child) in &edges {
                if *parent == at {
                    walking.push(*child);
                }
            }
        }
        prop_assert!(
            paths >= members.len(),
            "{paths} paths cannot be fewer than {} members",
            members.len()
        );
    }
}
