//! Who matters in a network, by four different definitions of mattering.
//!
//! Degree counts connections. Rank scores a vertex by the rank of what points at it.
//! Betweenness counts how often a vertex sits on a shortest route between others --- the
//! measure that finds intermediaries rather than endpoints, and the one that is too
//! expensive to compute exactly at scale.
//!
//! Exact betweenness is `O(V·E)`. On a graph of ten million edges that is not a slow query,
//! it is an impossible one, so only the sampled estimate is offered and it is named for
//! what it is. `FR-GRAPH-18` requires the exact variants to be documented as out of scope
//! rather than quietly provided and quietly timing out.

use crate::budget::{Bounded, Budget, Truncation};
use crate::csr::Adjacency;
use crate::ids::{EdgeMask, VertexId};
use std::collections::VecDeque;

/// A vertex and its score.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Score {
    /// The vertex.
    pub vertex: VertexId,
    /// Its score under whichever measure produced it.
    pub value: f64,
}

/// Order by descending score, then ascending vertex.
///
/// The tie-break is what makes the ordering total. Scores tie constantly --- every vertex
/// of the same degree, every vertex a rank iteration has not separated --- and an unordered
/// tie makes "the top ten" depend on iteration order.
fn rank_order(a: &Score, b: &Score) -> std::cmp::Ordering {
    b.value
        .partial_cmp(&a.value)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.vertex.cmp(&b.vertex))
}

/// How many edges touch each vertex, by direction.
#[must_use]
pub fn degree(graph: &Adjacency, mask: &EdgeMask, direction: Direction) -> Vec<Score> {
    let mut out: Vec<Score> = (0..graph.vertex_count())
        .map(|index| {
            let vertex = VertexId(u32::try_from(index).unwrap_or(u32::MAX));
            let value = match direction {
                Direction::Out => graph.out_degree(vertex, mask),
                Direction::In => graph.in_degree(vertex, mask),
                Direction::Both => graph
                    .out_degree(vertex, mask)
                    .saturating_add(graph.in_degree(vertex, mask)),
            };
            Score {
                vertex,
                #[allow(clippy::cast_precision_loss)]
                value: value as f64,
            }
        })
        .collect();
    out.sort_by(rank_order);
    out
}

/// Which way to count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Edges leaving.
    Out,
    /// Edges arriving.
    In,
    /// Both, added.
    Both,
}

/// Score each vertex by the scores of the vertices pointing at it.
///
/// The standard damped-random-walk formulation. `damping` is the probability of following
/// an edge rather than restarting; 0.85 is conventional and the value most published
/// comparisons assume.
///
/// Dangling vertices --- those with no out-edges --- have their mass redistributed rather
/// than discarded. Discarding it makes the scores stop summing to one, and every downstream
/// comparison between two runs on graphs with different dangling counts becomes meaningless.
#[must_use]
pub fn rank(
    graph: &Adjacency,
    mask: &EdgeMask,
    damping: f64,
    iterations: u32,
    budget: &Budget,
) -> Bounded<Vec<Score>> {
    let n = graph.vertex_count();
    if n == 0 {
        return Bounded::complete(Vec::new());
    }
    #[allow(clippy::cast_precision_loss)]
    let count = n as f64;
    let mut score = vec![1.0 / count; n];
    let mut truncation = Truncation::default();

    for iteration in 0..iterations {
        if u64::from(iteration) > budget.max_visits as u64 {
            truncation.by_visits = true;
            break;
        }
        let mut next = vec![0.0f64; n];

        // Mass sitting on vertices with nowhere to send it.
        let mut dangling = 0.0f64;
        for index in 0..n {
            let vertex = VertexId(u32::try_from(index).unwrap_or(u32::MAX));
            let out = graph.out_degree(vertex, mask);
            let mass = score.get(index).copied().unwrap_or(0.0);
            if out == 0 {
                dangling += mass;
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let share = mass / out as f64;
            for edge_type in mask.types() {
                for target in graph.out_edges(vertex, *edge_type).targets() {
                    if let Some(slot) = next.get_mut(target.index()) {
                        *slot += share;
                    }
                }
            }
        }

        let restart = (1.0 - damping) / count + damping * dangling / count;
        for slot in next.iter_mut() {
            *slot = restart + damping * *slot;
        }
        score = next;
    }

    let mut out: Vec<Score> = score
        .into_iter()
        .enumerate()
        .map(|(index, value)| Score {
            vertex: VertexId(u32::try_from(index).unwrap_or(u32::MAX)),
            value,
        })
        .collect();
    out.sort_by(rank_order);
    Bounded::truncated(out, truncation)
}

/// How often each vertex lies on a shortest route between two others, **estimated**.
///
/// Brandes' algorithm run from a sample of sources rather than from all of them, and scaled
/// by the sampling fraction. The estimate is unbiased but noisy, and the noise is largest
/// exactly where it matters least --- on the low-scoring tail.
///
/// There is deliberately no exact variant. `O(V·E)` is not a slow query on a real graph,
/// it is an impossible one, and offering it would mean offering something that always times
/// out. `FR-GRAPH-18` requires that to be said rather than discovered.
#[must_use]
pub fn betweenness_estimate(
    graph: &Adjacency,
    mask: &EdgeMask,
    sources: &[VertexId],
    budget: &Budget,
) -> Bounded<Vec<Score>> {
    let n = graph.vertex_count();
    let mut total = vec![0.0f64; n];
    let mut truncation = Truncation::default();
    let mut visits = 0usize;
    let mut sampled = 0usize;

    for source in sources {
        if source.index() >= n {
            continue;
        }
        visits = visits.saturating_add(1);
        if visits > budget.max_visits {
            truncation.by_visits = true;
            break;
        }
        sampled = sampled.saturating_add(1);
        accumulate_dependency(graph, mask, *source, &mut total);
    }

    if sampled < sources.len() {
        truncation.by_results = true;
    }

    // Scale by how much of the source set was actually walked, so an estimate from ten
    // sources is comparable with one from a thousand.
    #[allow(clippy::cast_precision_loss)]
    let scale = if sampled == 0 {
        0.0
    } else {
        n as f64 / sampled as f64
    };

    let mut out: Vec<Score> = total
        .into_iter()
        .enumerate()
        .map(|(index, value)| Score {
            vertex: VertexId(u32::try_from(index).unwrap_or(u32::MAX)),
            value: value * scale,
        })
        .collect();
    out.sort_by(rank_order);
    Bounded::truncated(out, truncation)
}

/// One source's contribution to betweenness, by Brandes' back-propagation.
fn accumulate_dependency(graph: &Adjacency, mask: &EdgeMask, source: VertexId, total: &mut [f64]) {
    let n = graph.vertex_count();
    let mut predecessors: Vec<Vec<VertexId>> = vec![Vec::new(); n];
    let mut path_count = vec![0.0f64; n];
    let mut distance = vec![-1i64; n];
    let mut order: Vec<VertexId> = Vec::new();
    let mut queue: VecDeque<VertexId> = VecDeque::new();

    if let Some(slot) = path_count.get_mut(source.index()) {
        *slot = 1.0;
    }
    if let Some(slot) = distance.get_mut(source.index()) {
        *slot = 0;
    }
    queue.push_back(source);

    while let Some(current) = queue.pop_front() {
        order.push(current);
        let current_distance = distance.get(current.index()).copied().unwrap_or(0);
        let current_paths = path_count.get(current.index()).copied().unwrap_or(0.0);

        for edge_type in mask.types() {
            for target in graph.out_edges(current, *edge_type).targets() {
                if distance.get(target.index()).copied().unwrap_or(-1) < 0 {
                    if let Some(slot) = distance.get_mut(target.index()) {
                        *slot = current_distance.saturating_add(1);
                    }
                    queue.push_back(*target);
                }
                if distance.get(target.index()).copied().unwrap_or(-1)
                    == current_distance.saturating_add(1)
                {
                    if let Some(slot) = path_count.get_mut(target.index()) {
                        *slot += current_paths;
                    }
                    if let Some(slot) = predecessors.get_mut(target.index()) {
                        slot.push(current);
                    }
                }
            }
        }
    }

    // Walk back, furthest first, accumulating each vertex's share of its successors'.
    let mut dependency = vec![0.0f64; n];
    for vertex in order.iter().rev() {
        let vertex_paths = path_count.get(vertex.index()).copied().unwrap_or(0.0);
        let vertex_dependency = dependency.get(vertex.index()).copied().unwrap_or(0.0);
        for predecessor in predecessors
            .get(vertex.index())
            .map_or(&[][..], Vec::as_slice)
        {
            let predecessor_paths = path_count.get(predecessor.index()).copied().unwrap_or(0.0);
            if vertex_paths > 0.0 {
                let share = predecessor_paths / vertex_paths * (1.0 + vertex_dependency);
                if let Some(slot) = dependency.get_mut(predecessor.index()) {
                    *slot += share;
                }
            }
        }
        if *vertex != source {
            if let Some(slot) = total.get_mut(vertex.index()) {
                *slot += vertex_dependency;
            }
        }
    }
}
