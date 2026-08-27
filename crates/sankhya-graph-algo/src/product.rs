//! Aggregated influence along multiplicative paths.
//!
//! Some edge weights compose by **multiplying** rather than adding. Where an edge means
//! "this much of that", a two-hop chain of 0.5 and 0.4 means 0.2 --- not 0.9. Every
//! shortest-path routine in this crate adds, so none of them answers this question, and
//! using one that adds produces a number with no meaning rather than an error.
//!
//! The aggregate over all paths between a pair is their **sum**, because distinct routes
//! contribute independently: holding 30% by one chain and 25% by another is 55%.
//!
//! Two bounds make this computable. **Damping** multiplies each additional hop by a factor
//! below one, so a long chain contributes less than a short one of the same product.
//! **A pruning threshold** stops extending a path once its running product falls below a
//! floor, which is what makes the search finite on a cyclic graph --- without it, a cycle
//! whose product is close to one is explored until the budget runs out.

use crate::budget::{Bounded, Budget, Truncation};
use crate::csr::Adjacency;
use crate::ids::{EdgeMask, VertexId};

/// How to aggregate, and when to stop.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Damping {
    /// Multiplied into the running product at each hop.
    ///
    /// One means an unattenuated chain: a route of twenty hops counts as much as the direct
    /// edge with the same product. That is occasionally right and usually not.
    pub per_hop: f64,
    /// The running product below which a path stops being extended.
    ///
    /// This is what makes the search terminate on a cyclic graph. It must be above zero:
    /// with a threshold of zero and a cycle, extension never stops on its own and only the
    /// visit budget ends it --- turning every result into a truncated one.
    pub floor: f64,
}

impl Damping {
    /// No attenuation, pruning below one part in ten thousand.
    ///
    /// The floor rather than the damping is what usually wants tuning: a contribution of
    /// 0.0001 is below the resolution of anything the result will be compared against.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            per_hop: 1.0,
            floor: 0.0001,
        }
    }

    /// Attenuating each hop by `per_hop`.
    #[must_use]
    pub const fn of(per_hop: f64, floor: f64) -> Self {
        Self { per_hop, floor }
    }
}

/// Total influence of `seeds` on every vertex they can reach.
///
/// Each seed starts with a weight of one. Along an edge, the running product is multiplied
/// by the edge's weight and by the damping factor; where paths converge their contributions
/// are summed. A vertex's score is therefore the damped sum over all paths from any seed.
///
/// Paths are not required to be loopless, but the pruning floor bounds a cycle's
/// contribution: going round multiplies the product by the cycle's own product, which is
/// below one for any meaningful weighting, so a cycle contributes a convergent series
/// rather than an infinite one.
#[must_use]
pub fn influence(
    graph: &Adjacency,
    seeds: &[VertexId],
    mask: &EdgeMask,
    damping: Damping,
    budget: &Budget,
) -> Bounded<Vec<crate::centrality::Score>> {
    let n = graph.vertex_count();
    let mut total = vec![0.0f64; n];
    let mut truncation = Truncation::default();
    let mut visits = 0usize;

    // A vertex may be entered many times by different routes, so this is a work list of
    // (vertex, running product, depth) rather than a visited set. The floor is what keeps
    // it finite.
    let mut work: Vec<(VertexId, f64, u32)> = Vec::new();
    for seed in seeds {
        if seed.index() < n {
            work.push((*seed, 1.0, 0));
        }
    }

    while let Some((current, carried, depth)) = work.pop() {
        visits = visits.saturating_add(1);
        if visits > budget.max_visits {
            truncation.by_visits = true;
            break;
        }
        if let Some(slot) = total.get_mut(current.index()) {
            *slot += carried;
        }
        if depth >= budget.max_depth {
            if graph.out_degree(current, mask) > 0 {
                truncation.by_depth = true;
            }
            continue;
        }
        if graph.out_degree(current, mask) > budget.max_degree {
            if !truncation.suppressed.contains(&current) {
                truncation.suppressed.push(current);
            }
            continue;
        }

        for edge_type in mask.types() {
            for arc in graph.out_edges(current, *edge_type).iter() {
                let next = carried * arc.weight * damping.per_hop;
                // Below the floor the path stops. This is the termination condition on a
                // cyclic graph, not an optimisation.
                if next < damping.floor {
                    continue;
                }
                work.push((arc.target, next, depth.saturating_add(1)));
            }
        }
    }

    // A seed's own starting weight is not influence upon itself.
    for seed in seeds {
        if let Some(slot) = total.get_mut(seed.index()) {
            *slot = (*slot - 1.0).max(0.0);
        }
    }

    let mut out: Vec<crate::centrality::Score> = total
        .into_iter()
        .enumerate()
        .map(|(index, value)| crate::centrality::Score {
            vertex: VertexId(u32::try_from(index).unwrap_or(u32::MAX)),
            value,
        })
        .filter(|s| s.value > 0.0)
        .collect();
    out.sort_by(|a, b| {
        b.value
            .partial_cmp(&a.value)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.vertex.cmp(&b.vertex))
    });
    Bounded::truncated(out, truncation)
}

/// The aggregated product from one specific origin to one specific target.
///
/// The pairwise form of [`influence`], for when the question names both ends.
#[must_use]
pub fn influence_between(
    graph: &Adjacency,
    from: VertexId,
    to: VertexId,
    mask: &EdgeMask,
    damping: Damping,
    budget: &Budget,
) -> Bounded<f64> {
    let whole = influence(graph, &[from], mask, damping, budget);
    let value = whole
        .found
        .iter()
        .find(|s| s.vertex == to)
        .map_or(0.0, |s| s.value);
    Bounded::truncated(value, whole.truncation)
}
