//! Shortest paths, k-shortest *loopless* paths, and simple cycles.
//!
//! # Two names that mean the wrong thing
//!
//! The design notes record two hazards in the general-purpose graph literature, both of
//! which produce silently wrong analytics rather than errors. They are the reason every
//! function here is tested against an independent brute-force reference:
//!
//! **"Johnson's algorithm" names two unrelated things.** One is all-pairs shortest path via
//! reweighting; the other is simple-cycle enumeration. Reaching for the wrong one returns
//! a well-formed result to a question nobody asked. [`cycles`] enumerates circuits; the
//! shortest-path routines are named for distances and never for cycles.
//!
//! **"k shortest paths" is ambiguous between paths and walks.** The useful version returns
//! `k` distinct *loopless* paths between a specific pair. A common alternative returns the
//! `k` smallest walk *lengths* per vertex, where a walk may revisit vertices freely. The
//! second is cheaper, is what several libraries provide under the same name, and answers a
//! different question: a "route" that visits the same vertex three times is not a route.
//! [`k_shortest_loopless`] returns paths, and the property test asserts no vertex repeats.

use crate::budget::{Bounded, Budget, Truncation};
use crate::csr::Adjacency;
use crate::ids::{EdgeMask, VertexId};
use std::collections::BTreeSet;

/// A route through the graph, with what it cost.
#[derive(Clone, PartialEq, Debug)]
pub struct Path {
    /// The vertices in order, starting at the origin and ending at the destination.
    pub vertices: Vec<VertexId>,
    /// The sum of the edge weights along it.
    pub cost: f64,
}

impl Path {
    /// How many edges it traverses.
    #[must_use]
    pub fn hops(&self) -> usize {
        self.vertices.len().saturating_sub(1)
    }

    /// Whether any vertex appears twice.
    ///
    /// The distinguishing property between a path and a walk. A route that visits the same
    /// vertex three times is not a route, and the k-shortest variant that returns walks
    /// will happily produce one.
    #[must_use]
    pub fn is_loopless(&self) -> bool {
        let unique: BTreeSet<VertexId> = self.vertices.iter().copied().collect();
        unique.len() == self.vertices.len()
    }

    /// Order two paths by cost, then by their vertex sequence.
    ///
    /// Total, so that a rebuild of the same graph produces the same ordering. Ties on cost
    /// are common --- parallel routes of equal weight --- and leaving them unordered makes
    /// the k-shortest result depend on iteration order.
    fn order(a: &Self, b: &Self) -> std::cmp::Ordering {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.vertices.cmp(&b.vertices))
    }
}

/// The cheapest route from `from` to `to`, or `None` if there is none.
///
/// Dijkstra, so a negative edge anywhere in the graph is refused up front rather than
/// mishandled. The refusal is a `Result` and not a `None`, because "no route exists" and
/// "this routine cannot answer for this graph" are different facts and a caller acting on
/// the first when the second is true draws a conclusion the data does not support.
///
/// The check is on the graph rather than on the edges examined. Whether Dijkstra ever
/// relaxes the negative edge depends on which costs it meets first, so a check during
/// relaxation passes for some queries and fails for others on the same graph.
pub fn shortest_path(
    graph: &Adjacency,
    from: VertexId,
    to: VertexId,
    mask: &EdgeMask,
    budget: &Budget,
) -> Result<Bounded<Option<Path>>, NegativeWeight> {
    if graph.has_negative_weight() {
        return Err(NegativeWeight);
    }
    Ok(dijkstra(graph, from, to, mask, budget))
}

/// Dijkstra proper, with the precondition already established by the caller.
fn dijkstra(
    graph: &Adjacency,
    from: VertexId,
    to: VertexId,
    mask: &EdgeMask,
    budget: &Budget,
) -> Bounded<Option<Path>> {
    let mut best = vec![f64::INFINITY; graph.vertex_count()];
    let mut came_from: Vec<Option<VertexId>> = vec![None; graph.vertex_count()];
    let mut settled = vec![false; graph.vertex_count()];
    let mut truncation = Truncation::default();
    let mut visits = 0usize;

    if from.index() >= graph.vertex_count() || to.index() >= graph.vertex_count() {
        return Bounded::complete(None);
    }
    if let Some(slot) = best.get_mut(from.index()) {
        *slot = 0.0;
    }

    loop {
        // The unsettled vertex with the smallest tentative distance.
        let mut current: Option<VertexId> = None;
        let mut current_cost = f64::INFINITY;
        for v in 0..graph.vertex_count() {
            if settled.get(v).copied().unwrap_or(true) {
                continue;
            }
            let cost = best.get(v).copied().unwrap_or(f64::INFINITY);
            if cost < current_cost {
                current_cost = cost;
                current = Some(VertexId(u32::try_from(v).unwrap_or(u32::MAX)));
            }
        }
        let Some(current) = current else { break };
        if !current_cost.is_finite() {
            break;
        }
        if current == to {
            break;
        }

        visits = visits.saturating_add(1);
        if visits > budget.max_visits {
            truncation.by_visits = true;
            break;
        }
        if let Some(slot) = settled.get_mut(current.index()) {
            *slot = true;
        }
        if graph.out_degree(current, mask) > budget.max_degree {
            truncation.suppressed.push(current);
            continue;
        }

        for edge_type in mask.types() {
            for arc in graph.out_edges(current, *edge_type).iter() {
                let candidate = current_cost + arc.weight;
                if candidate
                    < best
                        .get(arc.target.index())
                        .copied()
                        .unwrap_or(f64::INFINITY)
                {
                    if let Some(slot) = best.get_mut(arc.target.index()) {
                        *slot = candidate;
                    }
                    if let Some(slot) = came_from.get_mut(arc.target.index()) {
                        *slot = Some(current);
                    }
                }
            }
        }
    }

    let cost = best.get(to.index()).copied().unwrap_or(f64::INFINITY);
    if !cost.is_finite() {
        return Bounded::truncated(None, truncation);
    }
    Bounded::truncated(
        reconstruct(&came_from, from, to).map(|vertices| Path { vertices, cost }),
        truncation,
    )
}

/// Walk the predecessor chain back from `to`, returning it forwards.
///
/// Bounded by the vertex count: a corrupt chain would otherwise loop forever, and a
/// predecessor array is exactly the kind of thing an off-by-one turns into a cycle.
fn reconstruct(
    came_from: &[Option<VertexId>],
    from: VertexId,
    to: VertexId,
) -> Option<Vec<VertexId>> {
    let mut reversed = vec![to];
    let mut at = to;
    for _ in 0..came_from.len() {
        if at == from {
            reversed.reverse();
            return Some(reversed);
        }
        let previous = came_from.get(at.index()).copied().flatten()?;
        reversed.push(previous);
        at = previous;
    }
    None
}

/// The `k` cheapest **loopless** paths from `from` to `to`, cheapest first.
///
/// Yen's algorithm: take the shortest path, then for each prefix of it, find the best
/// alternative that leaves the prefix by a different edge, with the prefix's interior
/// vertices banned so the result cannot fold back on itself.
///
/// Every returned path is loopless. That is the whole point of the routine and the
/// property test asserts it directly --- the cheaper "k shortest walks" variant that
/// several libraries offer under this name does not hold it.
pub fn k_shortest_loopless(
    graph: &Adjacency,
    from: VertexId,
    to: VertexId,
    k: usize,
    mask: &EdgeMask,
    budget: &Budget,
) -> Result<Bounded<Vec<Path>>, NegativeWeight> {
    if graph.has_negative_weight() {
        return Err(NegativeWeight);
    }
    let mut accepted: Vec<Path> = Vec::new();
    let mut candidates: Vec<Path> = Vec::new();
    let mut truncation = Truncation::default();

    let first = dijkstra(graph, from, to, mask, budget);
    let Some(first) = first.found else {
        return Ok(Bounded::truncated(Vec::new(), first.truncation));
    };
    accepted.push(first);

    let wanted = k.min(budget.max_results);
    while accepted.len() < wanted {
        let Some(previous) = accepted.last().cloned() else {
            break;
        };

        for spur_index in 0..previous.vertices.len().saturating_sub(1) {
            let Some(spur) = previous.vertices.get(spur_index).copied() else {
                continue;
            };
            let Some(root) = previous.vertices.get(..=spur_index) else {
                continue;
            };

            // Ban the edges that every already-accepted path sharing this root takes out of
            // the spur, so the alternative must genuinely diverge.
            let mut banned_edges: BTreeSet<(VertexId, VertexId)> = BTreeSet::new();
            for path in accepted.iter().chain(std::iter::once(&previous)) {
                if path.vertices.get(..=spur_index) == Some(root) {
                    if let (Some(a), Some(b)) = (
                        path.vertices.get(spur_index),
                        path.vertices.get(spur_index.saturating_add(1)),
                    ) {
                        banned_edges.insert((*a, *b));
                    }
                }
            }
            // Ban the root's interior, so the spur path cannot revisit it. This is what
            // makes the result loopless rather than merely distinct.
            let banned_vertices: BTreeSet<VertexId> = root
                .get(..spur_index)
                .unwrap_or(&[])
                .iter()
                .copied()
                .collect();

            let Some(spur_path) = constrained_shortest(
                graph,
                spur,
                to,
                mask,
                budget,
                &banned_edges,
                &banned_vertices,
            ) else {
                continue;
            };

            let mut vertices = root.get(..spur_index).unwrap_or(&[]).to_vec();
            vertices.extend_from_slice(&spur_path.vertices);
            let root_cost = path_cost(graph, root.get(..=spur_index).unwrap_or(&[]), mask);
            let whole = Path {
                cost: root_cost + spur_path.cost,
                vertices,
            };

            if !whole.is_loopless() {
                continue;
            }
            if accepted.contains(&whole) || candidates.contains(&whole) {
                continue;
            }
            candidates.push(whole);
        }

        if candidates.is_empty() {
            break;
        }
        candidates.sort_by(Path::order);
        if candidates.is_empty() {
            break;
        }
        accepted.push(candidates.remove(0));
    }

    if accepted.len() >= wanted && !candidates.is_empty() {
        truncation.by_results = true;
    }
    accepted.sort_by(Path::order);
    Ok(Bounded::truncated(accepted, truncation))
}

/// The graph has a negative edge, and the routine asked for assumes none.
///
/// Its own type rather than an `Option::None`, so that a caller cannot mistake "this
/// routine cannot answer for this graph" for "no route exists".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NegativeWeight;

impl std::fmt::Display for NegativeWeight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "this graph has an edge of negative weight, and shortest-path search assumes \
             none: settling a vertex would no longer fix its distance, so the result would \
             be wrong rather than slow",
        )
    }
}

impl std::error::Error for NegativeWeight {}

/// Dijkstra with some edges and vertices removed.
fn constrained_shortest(
    graph: &Adjacency,
    from: VertexId,
    to: VertexId,
    mask: &EdgeMask,
    budget: &Budget,
    banned_edges: &BTreeSet<(VertexId, VertexId)>,
    banned_vertices: &BTreeSet<VertexId>,
) -> Option<Path> {
    if banned_vertices.contains(&from) {
        return None;
    }
    let mut best = vec![f64::INFINITY; graph.vertex_count()];
    let mut came_from: Vec<Option<VertexId>> = vec![None; graph.vertex_count()];
    let mut settled = vec![false; graph.vertex_count()];
    if let Some(slot) = best.get_mut(from.index()) {
        *slot = 0.0;
    }

    loop {
        let mut current: Option<VertexId> = None;
        let mut current_cost = f64::INFINITY;
        for v in 0..graph.vertex_count() {
            if settled.get(v).copied().unwrap_or(true) {
                continue;
            }
            let cost = best.get(v).copied().unwrap_or(f64::INFINITY);
            if cost < current_cost {
                current_cost = cost;
                current = Some(VertexId(u32::try_from(v).unwrap_or(u32::MAX)));
            }
        }
        let Some(current) = current else { break };
        if !current_cost.is_finite() || current == to {
            break;
        }
        if let Some(slot) = settled.get_mut(current.index()) {
            *slot = true;
        }
        if graph.out_degree(current, mask) > budget.max_degree {
            continue;
        }

        for edge_type in mask.types() {
            for arc in graph.out_edges(current, *edge_type).iter() {
                if banned_vertices.contains(&arc.target)
                    || banned_edges.contains(&(current, arc.target))
                {
                    continue;
                }
                let candidate = current_cost + arc.weight;
                if candidate
                    < best
                        .get(arc.target.index())
                        .copied()
                        .unwrap_or(f64::INFINITY)
                {
                    if let Some(slot) = best.get_mut(arc.target.index()) {
                        *slot = candidate;
                    }
                    if let Some(slot) = came_from.get_mut(arc.target.index()) {
                        *slot = Some(current);
                    }
                }
            }
        }
    }

    let cost = best.get(to.index()).copied().unwrap_or(f64::INFINITY);
    if !cost.is_finite() {
        return None;
    }
    reconstruct(&came_from, from, to).map(|vertices| Path { vertices, cost })
}

/// The total weight of a vertex sequence, taking the cheapest edge between each pair.
fn path_cost(graph: &Adjacency, vertices: &[VertexId], mask: &EdgeMask) -> f64 {
    let mut total = 0.0;
    for window in vertices.windows(2) {
        let (Some(a), Some(b)) = (window.first(), window.get(1)) else {
            continue;
        };
        let mut cheapest = f64::INFINITY;
        for edge_type in mask.types() {
            for arc in graph.out_edges(*a, *edge_type).iter() {
                if arc.target == *b && arc.weight < cheapest {
                    cheapest = arc.weight;
                }
            }
        }
        if cheapest.is_finite() {
            total += cheapest;
        }
    }
    total
}

/// Every **simple cycle** reachable from `seeds`, up to the budget.
///
/// A simple cycle visits no vertex twice except the one it starts and ends on. Enumeration
/// is exponential in the worst case, which is why the budget is not optional and why the
/// truncation flag matters more here than anywhere else in this crate: "no cycles found"
/// and "stopped looking" are answers with opposite meanings.
///
/// Each cycle is reported once, rotated so its smallest vertex comes first. Without that
/// normalisation the same circuit appears once per starting point, and a count of cycles
/// becomes a count of cycles times their average length.
#[must_use]
pub fn cycles(
    graph: &Adjacency,
    seeds: &[VertexId],
    mask: &EdgeMask,
    budget: &Budget,
) -> Bounded<Vec<Path>> {
    let mut found: BTreeSet<Vec<VertexId>> = BTreeSet::new();
    let mut truncation = Truncation::default();
    let mut visits = 0usize;

    for seed in seeds {
        if seed.index() >= graph.vertex_count() {
            continue;
        }
        let mut stack = vec![*seed];
        walk_for_cycles(
            graph,
            mask,
            budget,
            *seed,
            *seed,
            &mut stack,
            &mut found,
            &mut truncation,
            &mut visits,
        );
        if truncation.by_results || truncation.by_visits {
            break;
        }
    }

    let paths: Vec<Path> = found
        .into_iter()
        .map(|vertices| {
            let cost = path_cost(graph, &vertices, mask);
            Path { vertices, cost }
        })
        .collect();
    Bounded::truncated(paths, truncation)
}

/// Depth-first search for circuits returning to `origin`.
#[allow(clippy::too_many_arguments)]
fn walk_for_cycles(
    graph: &Adjacency,
    mask: &EdgeMask,
    budget: &Budget,
    origin: VertexId,
    current: VertexId,
    stack: &mut Vec<VertexId>,
    found: &mut BTreeSet<Vec<VertexId>>,
    truncation: &mut Truncation,
    visits: &mut usize,
) {
    *visits = visits.saturating_add(1);
    if *visits > budget.max_visits {
        truncation.by_visits = true;
        return;
    }
    if found.len() >= budget.max_results {
        truncation.by_results = true;
        return;
    }
    if u32::try_from(stack.len()).unwrap_or(u32::MAX) > budget.max_depth {
        truncation.by_depth = true;
        return;
    }
    if graph.out_degree(current, mask) > budget.max_degree {
        if !truncation.suppressed.contains(&current) {
            truncation.suppressed.push(current);
        }
        return;
    }

    for edge_type in mask.types() {
        for target in graph.out_edges(current, *edge_type).targets() {
            if *target == origin && stack.len() > 1 {
                // A circuit. Normalise so the same one is not counted once per rotation.
                if let Some(normalised) = normalise(stack) {
                    found.insert(normalised);
                }
                continue;
            }
            // Only extend to vertices greater than the origin, and never revisit. The first
            // condition is what stops each cycle being rediscovered from every one of its
            // members: a cycle is enumerated only from its smallest vertex.
            if *target <= origin || stack.contains(target) {
                continue;
            }
            stack.push(*target);
            walk_for_cycles(
                graph, mask, budget, origin, *target, stack, found, truncation, visits,
            );
            stack.pop();
            if truncation.by_results || truncation.by_visits {
                return;
            }
        }
    }
}

/// Rotate a circuit so its smallest vertex leads, and close it by repeating that vertex.
fn normalise(stack: &[VertexId]) -> Option<Vec<VertexId>> {
    let smallest = stack.iter().copied().min()?;
    let at = stack.iter().position(|v| *v == smallest)?;
    let mut rotated: Vec<VertexId> = stack.get(at..)?.to_vec();
    rotated.extend_from_slice(stack.get(..at)?);
    rotated.push(smallest);
    Some(rotated)
}
