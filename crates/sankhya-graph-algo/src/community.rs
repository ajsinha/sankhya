//! Grouping vertices that are more connected to each other than to the rest.
//!
//! # Why this is not label propagation
//!
//! Label propagation --- repeatedly adopt the commonest label among your neighbours --- is
//! the obvious near-linear choice, and it was the first thing built here. It fails on the
//! smallest interesting case: two dense clusters joined by a single edge collapse into one
//! community. The failure has a name, the *monster community*, and it is not a tuning
//! problem. Nothing in the propagation rule prefers keeping two groups apart, so once a
//! label crosses the bridge it keeps going.
//!
//! Making the visiting order deterministic, which reproducibility demands, makes it worse
//! rather than better: the randomised form at least sometimes stalls before the collapse.
//!
//! So this optimises **modularity** instead --- how much more internal edge weight a
//! grouping has than the same degrees would give by chance. That quantity *falls* when two
//! dense clusters merge across one thin bridge, so the objective resists the collapse
//! rather than relying on the search to stop in time.
//!
//! # Reproducibility
//!
//! Vertices are visited in ascending order and a move is taken only on strict improvement,
//! so the same graph always yields the same grouping. Randomised implementations get some
//! ensemble averaging from their shuffling; a caller wanting that should run several
//! deliberately permuted passes and compare, which is at least visible in the query.

use crate::budget::{Bounded, Budget, Truncation};
use crate::components::Components;
use crate::csr::Adjacency;
use crate::ids::{EdgeMask, VertexId};
use std::collections::BTreeMap;

/// Group vertices by local modularity optimisation.
///
/// `rounds` bounds the sweeps. Convergence is usually within a handful; a graph still
/// moving after the bound is oscillating between two groupings rather than approaching one,
/// and the result reports that rather than running longer.
///
/// Edges are treated as undirected: community membership is not a direction-respecting
/// notion, and a pair connected both ways is one relationship, not two.
#[must_use]
pub fn detect(
    graph: &Adjacency,
    mask: &EdgeMask,
    rounds: u32,
    budget: &Budget,
) -> Bounded<Components> {
    let n = graph.vertex_count();
    let mut truncation = Truncation::default();
    if n == 0 {
        return Bounded::complete(crate::components::from_groups(&[], 0));
    }

    // Undirected weighted neighbourhood, built once. Every sweep reads it many times, and
    // recomputing the union of in- and out-edges per visit dominates otherwise.
    let mut neighbours: Vec<Vec<(VertexId, f64)>> = vec![Vec::new(); n];
    let mut strength = vec![0.0f64; n];
    let mut total_weight = 0.0f64;
    for index in 0..n {
        let vertex = VertexId(u32::try_from(index).unwrap_or(u32::MAX));
        let mut merged: BTreeMap<VertexId, f64> = BTreeMap::new();
        for edge_type in mask.types() {
            for arc in graph.out_edges(vertex, *edge_type).iter() {
                *merged.entry(arc.target).or_insert(0.0) += arc.weight;
            }
            for arc in graph.in_edges(vertex, *edge_type).iter() {
                *merged.entry(arc.target).or_insert(0.0) += arc.weight;
            }
        }
        let degree: f64 = merged.values().copied().sum();
        if let Some(slot) = strength.get_mut(index) {
            *slot = degree;
        }
        total_weight += degree;
        if let Some(slot) = neighbours.get_mut(index) {
            *slot = merged.into_iter().collect();
        }
    }
    // `total_weight` counted each undirected edge from both ends, so it is already `2m`.
    let two_m = total_weight;
    if two_m <= 0.0 {
        // No edges: everyone is their own community, and modularity is undefined rather
        // than zero. Returning singletons is the only defensible answer.
        return Bounded::complete(singletons(n));
    }

    let mut community: Vec<u32> = (0..n)
        .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
        .collect();
    // Total strength of each community, kept incrementally: recomputing it per candidate
    // move is what makes the naive form quadratic.
    let mut community_strength: Vec<f64> = strength.clone();
    let mut visits = 0usize;
    let mut settled = false;

    for _ in 0..rounds {
        let mut moved = false;
        for index in 0..n {
            visits = visits.saturating_add(1);
            if visits > budget.max_visits {
                truncation.by_visits = true;
                return Bounded::truncated(relabel(&community, n), truncation);
            }
            let vertex = VertexId(u32::try_from(index).unwrap_or(u32::MAX));
            let Some(adjacency) = neighbours.get(index) else {
                continue;
            };
            if adjacency.len() > budget.max_degree {
                if !truncation.suppressed.contains(&vertex) {
                    truncation.suppressed.push(vertex);
                }
                continue;
            }

            let own = community.get(index).copied().unwrap_or(0);
            let k = strength.get(index).copied().unwrap_or(0.0);

            // Edge weight from this vertex into each neighbouring community.
            let mut into: BTreeMap<u32, f64> = BTreeMap::new();
            for (neighbour, weight) in adjacency {
                if neighbour.index() == index {
                    continue;
                }
                if let Some(label) = community.get(neighbour.index()) {
                    *into.entry(*label).or_insert(0.0) += *weight;
                }
            }

            // Leaving costs what staying was worth, so it is subtracted from every
            // candidate on equal terms and never needs to be computed separately.
            let staying = into.get(&own).copied().unwrap_or(0.0);
            let own_rest = community_strength.get(own as usize).copied().unwrap_or(0.0) - k;
            let baseline = staying - own_rest * k / two_m;

            let mut best_label = own;
            let mut best_gain = 0.0f64;
            for (candidate, shared) in &into {
                if *candidate == own {
                    continue;
                }
                let candidate_strength = community_strength
                    .get(*candidate as usize)
                    .copied()
                    .unwrap_or(0.0);
                let gain = (*shared - candidate_strength * k / two_m) - baseline;
                // Strict improvement only, and ties keep the current label. `BTreeMap`
                // iterates ascending, so this is reproducible without comparing floats for
                // equality.
                if gain > best_gain {
                    best_gain = gain;
                    best_label = *candidate;
                }
            }

            if best_label != own {
                if let Some(slot) = community_strength.get_mut(own as usize) {
                    *slot -= k;
                }
                if let Some(slot) = community_strength.get_mut(best_label as usize) {
                    *slot += k;
                }
                if let Some(slot) = community.get_mut(index) {
                    *slot = best_label;
                }
                moved = true;
            }
        }
        if !moved {
            settled = true;
            break;
        }
    }

    if !settled {
        // Oscillating rather than approaching. Say so rather than presenting the last
        // sweep as a converged answer.
        truncation.by_depth = true;
    }
    Bounded::truncated(relabel(&community, n), truncation)
}

/// How much better a grouping is than the same degrees arranged at random.
///
/// Positive means more internal weight than chance would give; around zero means the
/// grouping carries no information. Exposed because a caller presenting communities as a
/// finding should be able to say whether the grouping is better than noise, and the number
/// is cheap once the grouping exists.
#[must_use]
pub fn modularity(graph: &Adjacency, mask: &EdgeMask, grouping: &Components) -> f64 {
    let n = graph.vertex_count();
    let mut internal: BTreeMap<u32, f64> = BTreeMap::new();
    let mut total: BTreeMap<u32, f64> = BTreeMap::new();
    let mut two_m = 0.0f64;

    for index in 0..n {
        let vertex = VertexId(u32::try_from(index).unwrap_or(u32::MAX));
        let Some(own) = grouping.label_of(vertex) else {
            continue;
        };
        for edge_type in mask.types() {
            // Both directions, for both terms. Counting the degree term from both ends
            // and the internal term from one makes a single all-encompassing community
            // score -0.5 where it must score 0 --- the two halves must be measured the
            // same way or the quantity is not modularity.
            for arc in graph
                .out_edges(vertex, *edge_type)
                .iter()
                .chain(graph.in_edges(vertex, *edge_type).iter())
            {
                two_m += arc.weight;
                *total.entry(own).or_insert(0.0) += arc.weight;
                if grouping.label_of(arc.target) == Some(own) {
                    *internal.entry(own).or_insert(0.0) += arc.weight;
                }
            }
        }
    }
    if two_m <= 0.0 {
        return 0.0;
    }

    total
        .iter()
        .map(|(label, degree)| {
            let inside = internal.get(label).copied().unwrap_or(0.0);
            inside / two_m - (degree / two_m).powi(2)
        })
        .sum()
}

/// Every vertex alone.
fn singletons(n: usize) -> Components {
    let groups: Vec<Vec<VertexId>> = (0..n)
        .map(|i| vec![VertexId(u32::try_from(i).unwrap_or(u32::MAX))])
        .collect();
    crate::components::from_groups(&groups, n)
}

/// Compact arbitrary labels into dense ones ordered by each group's smallest member.
fn relabel(label: &[u32], n: usize) -> Components {
    let mut groups: BTreeMap<u32, Vec<VertexId>> = BTreeMap::new();
    for (index, raw) in label.iter().enumerate() {
        groups
            .entry(*raw)
            .or_default()
            .push(VertexId(u32::try_from(index).unwrap_or(u32::MAX)));
    }
    let mut ordered: Vec<Vec<VertexId>> = groups.into_values().collect();
    ordered.sort_by_key(|g| g.first().copied().unwrap_or(VertexId(u32::MAX)));
    crate::components::from_groups(&ordered, n)
}
