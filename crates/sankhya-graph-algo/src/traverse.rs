//! Bounded expansion from a set of seeds, static and time-respecting.
//!
//! # Why the time-respecting variant is not an option on the static one
//!
//! A static path says the edges exist. A time-respecting path says they could have been
//! *used in order*. Over a temporal graph the two differ enormously, and always in the same
//! direction: static reachability finds paths that time forbids, so it over-reports.
//!
//! That asymmetry is why [`time_respecting`] is a separate function rather than a flag.
//! A flag defaulting to off means every caller who forgot it gets the optimistic answer,
//! and the optimistic answer looks exactly like the correct one.
//!
//! # The two constraints that make a path mean something
//!
//! Beyond ordering, two further constraints turn "these edges could be used in order" into
//! "this is a plausible route for something to have moved along":
//!
//! - **Dwell**: how long a path may pause at a vertex. Without an upper bound, two
//!   unrelated events years apart join into one path. Without a lower bound, a path can
//!   arrive and leave in the same instant, which is frequently an artefact of timestamp
//!   granularity rather than a real sequence.
//! - **Conservation**: how much an edge's weight may differ from the one before it. A route
//!   only makes sense if what leaves is related to what arrived; without this, a path can
//!   chain a large edge onto a tiny one and call it a route.
//!
//! Both are in `FR-GRAPH-03`, and both are off by default only in the sense that
//! [`TimeConstraints::none`] must be asked for by name.

use crate::budget::{Bounded, Budget, Truncation};
use crate::csr::Adjacency;
use crate::ids::{EdgeMask, VertexId};
use std::collections::VecDeque;

/// How a vertex was reached.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Reached {
    /// The vertex.
    pub vertex: VertexId,
    /// How many edges from the nearest seed.
    pub depth: u32,
    /// The vertex it was reached from, or `None` for a seed.
    pub via: Option<VertexId>,
    /// When it was reached, for a time-respecting expansion. `i64::MIN` for a static one.
    pub at: i64,
}

/// Bounds on how a time-respecting path may move through time and value.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TimeConstraints {
    /// The earliest instant a path may use an edge.
    pub from: i64,
    /// The latest.
    pub until: i64,
    /// The longest a path may wait at a vertex before continuing.
    ///
    /// `i64::MAX` allows any pause, which joins events of arbitrary separation into one
    /// path. That is occasionally what is wanted and usually not.
    pub max_dwell: i64,
    /// The shortest it must wait.
    ///
    /// Zero permits arriving and leaving in the same instant. Where timestamps are coarse
    /// --- dated rather than stamped --- that admits sequences the data cannot actually
    /// order, and a value of one excludes them.
    pub min_dwell: i64,
    /// How much of an incoming edge's weight must carry into the outgoing one.
    ///
    /// Expressed as a fraction in `[0, 1]`: `0.9` requires the next edge to be at least
    /// nine tenths of the one before. `0.0` disables the check. This is what distinguishes
    /// a route along which something moved from a chain of unrelated edges that happen to
    /// be ordered in time.
    pub min_conservation: f64,
}

impl TimeConstraints {
    /// Ordering only: any window, any pause, any weight.
    ///
    /// Named rather than defaulted. A caller reaching for this is saying the ordering is
    /// the only thing they need, which is a claim worth making out loud.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            from: i64::MIN,
            until: i64::MAX,
            max_dwell: i64::MAX,
            min_dwell: 0,
            min_conservation: 0.0,
        }
    }

    /// Ordering within a window.
    #[must_use]
    pub const fn window(from: i64, until: i64) -> Self {
        Self {
            from,
            until,
            ..Self::none()
        }
    }

    /// The same constraints, with a longest permitted pause at a vertex.
    #[must_use]
    pub const fn with_max_dwell(mut self, dwell: i64) -> Self {
        self.max_dwell = dwell;
        self
    }

    /// The same constraints, with a shortest required pause.
    #[must_use]
    pub const fn with_min_dwell(mut self, dwell: i64) -> Self {
        self.min_dwell = dwell;
        self
    }

    /// The same constraints, requiring the outgoing edge to retain this fraction of the
    /// incoming one's weight.
    #[must_use]
    pub const fn with_conservation(mut self, fraction: f64) -> Self {
        self.min_conservation = fraction;
        self
    }

    /// Whether a hop arriving at `arrived_at` carrying `arrived_with` may leave on an edge
    /// starting at `leaves_at` carrying `leaves_with`.
    ///
    /// `arrived_with` is `None` at a seed, which has no inbound edge. Conservation and
    /// dwell are both relations *between* two edges, so neither can be evaluated on the
    /// first hop, and a sentinel weight standing in for the missing edge would silently
    /// decide them --- an infinite one refuses every first hop, a zero one permits every
    /// hop thereafter.
    fn permits_hop(
        self,
        arrived_at: i64,
        arrived_with: Option<f64>,
        leaves_at: i64,
        leaves_with: f64,
    ) -> bool {
        if leaves_at < self.from || leaves_at >= self.until {
            return false;
        }
        let Some(arrived_with) = arrived_with else {
            // A seed. There is no prior edge, so only the window applies.
            return true;
        };
        let dwell = leaves_at.saturating_sub(arrived_at);
        if dwell < self.min_dwell || dwell > self.max_dwell {
            return false;
        }
        if self.min_conservation > 0.0 && arrived_with > 0.0 {
            // `>=` against a product rather than a ratio, so a zero incoming weight cannot
            // divide, and so the comparison never needs an equality test on floats.
            return leaves_with >= arrived_with * self.min_conservation;
        }
        true
    }
}

impl Default for TimeConstraints {
    fn default() -> Self {
        Self::none()
    }
}

/// Expand outward from `seeds`, ignoring time.
///
/// Breadth-first, so `depth` is the true shortest hop count from the nearest seed. Each
/// vertex is reported once, at the smallest depth that reaches it.
///
/// Use this only when the edges genuinely have no temporal meaning. If they do, and this is
/// used anyway, the result will include routes that time forbids --- and will not say so.
#[must_use]
pub fn reachable(
    graph: &Adjacency,
    seeds: &[VertexId],
    mask: &EdgeMask,
    budget: &Budget,
) -> Bounded<Vec<Reached>> {
    let mut seen = vec![false; graph.vertex_count()];
    let mut found = Vec::new();
    let mut truncation = Truncation::default();
    let mut queue: VecDeque<Reached> = VecDeque::new();
    let mut visits = 0usize;

    for seed in seeds {
        if seed.index() >= graph.vertex_count() {
            continue;
        }
        if seen.get(seed.index()).copied().unwrap_or(true) {
            continue;
        }
        if let Some(slot) = seen.get_mut(seed.index()) {
            *slot = true;
        }
        queue.push_back(Reached {
            vertex: *seed,
            depth: 0,
            via: None,
            at: i64::MIN,
        });
    }

    while let Some(current) = queue.pop_front() {
        visits = visits.saturating_add(1);
        if visits > budget.max_visits {
            truncation.by_visits = true;
            break;
        }
        if found.len() >= budget.max_results {
            truncation.by_results = true;
            break;
        }
        found.push(current);

        if current.depth >= budget.max_depth {
            // Only truncation if there was in fact somewhere further to go.
            if graph.out_degree(current.vertex, mask) > 0 {
                truncation.by_depth = true;
            }
            continue;
        }
        let degree = graph.out_degree(current.vertex, mask);
        if degree > budget.max_degree {
            truncation.suppressed.push(current.vertex);
            continue;
        }

        for edge_type in mask.types() {
            for target in graph.out_edges(current.vertex, *edge_type).targets() {
                if seen.get(target.index()).copied().unwrap_or(true) {
                    continue;
                }
                if let Some(slot) = seen.get_mut(target.index()) {
                    *slot = true;
                }
                queue.push_back(Reached {
                    vertex: *target,
                    depth: current.depth.saturating_add(1),
                    via: Some(current.vertex),
                    at: i64::MIN,
                });
            }
        }
    }

    Bounded::truncated(found, truncation)
}

/// Expand outward from `seeds`, following only edges that could have been used in order.
///
/// A vertex is reported at the *earliest* time it can be reached, because reaching it
/// earlier can only permit more onward edges --- a later arrival never opens a route an
/// earlier one closes. That makes earliest-arrival the right frontier to keep, and it makes
/// the expansion a Dijkstra over time rather than a breadth-first walk.
///
/// `start_at` is when the seeds become available. Edges before it cannot be used.
#[must_use]
pub fn time_respecting(
    graph: &Adjacency,
    seeds: &[VertexId],
    mask: &EdgeMask,
    start_at: i64,
    constraints: &TimeConstraints,
    budget: &Budget,
) -> Bounded<Vec<Reached>> {
    // Earliest known arrival per vertex. The weight a path arrived carrying travels with
    // the frontier entry rather than in a per-vertex array: two paths can reach the same
    // vertex carrying different weights, and the one that matters is the one attached to
    // the arrival being expanded.
    let mut earliest = vec![i64::MAX; graph.vertex_count()];
    let mut found = Vec::new();
    let mut truncation = Truncation::default();
    let mut visits = 0usize;

    // Ordered by arrival instant. A binary heap would need a `Reverse` wrapper and an `Ord`
    // on a struct holding a float; a sorted vector of the frontier is simpler to reason
    // about and the frontier is bounded by the visit budget anyway.
    let mut frontier: Vec<(i64, Reached, Option<f64>)> = Vec::new();
    for seed in seeds {
        if seed.index() >= graph.vertex_count() {
            continue;
        }
        if start_at < earliest.get(seed.index()).copied().unwrap_or(i64::MIN) {
            if let Some(slot) = earliest.get_mut(seed.index()) {
                *slot = start_at;
            }
            frontier.push((
                start_at,
                Reached {
                    vertex: *seed,
                    depth: 0,
                    via: None,
                    at: start_at,
                },
                None,
            ));
        }
    }

    while !frontier.is_empty() {
        // Take the earliest arrival still pending.
        let mut best = 0usize;
        for (i, entry) in frontier.iter().enumerate() {
            if let (Some(candidate), Some(incumbent)) = (frontier.get(i), frontier.get(best)) {
                if candidate.0 < incumbent.0 {
                    best = i;
                }
            }
            let _ = entry;
        }
        let (at, current, with) = frontier.swap_remove(best);

        // A better arrival was recorded after this entry was queued.
        if at
            > earliest
                .get(current.vertex.index())
                .copied()
                .unwrap_or(i64::MAX)
        {
            continue;
        }

        visits = visits.saturating_add(1);
        if visits > budget.max_visits {
            truncation.by_visits = true;
            break;
        }
        if found.len() >= budget.max_results {
            truncation.by_results = true;
            break;
        }
        found.push(current);

        if current.depth >= budget.max_depth {
            if graph.out_degree(current.vertex, mask) > 0 {
                truncation.by_depth = true;
            }
            continue;
        }
        if graph.out_degree(current.vertex, mask) > budget.max_degree {
            truncation.suppressed.push(current.vertex);
            continue;
        }

        for edge_type in mask.types() {
            // `starting_from` is the binary search `FR-GRAPH-04` exists for: edges usable
            // after this arrival are a contiguous suffix of the vertex's run.
            for arc in graph
                .out_edges(current.vertex, *edge_type)
                .starting_from(at)
            {
                if !constraints.permits_hop(at, with, arc.validity.from, arc.weight) {
                    continue;
                }
                // The edge must still be live when taken.
                if arc.validity.from >= arc.validity.until {
                    continue;
                }
                let arrival = arc.validity.from;
                let known = earliest
                    .get(arc.target.index())
                    .copied()
                    .unwrap_or(i64::MAX);
                if arrival >= known {
                    continue;
                }
                if let Some(slot) = earliest.get_mut(arc.target.index()) {
                    *slot = arrival;
                }
                frontier.push((
                    arrival,
                    Reached {
                        vertex: arc.target,
                        depth: current.depth.saturating_add(1),
                        via: Some(current.vertex),
                        at: arrival,
                    },
                    Some(arc.weight),
                ));
            }
        }
    }

    Bounded::truncated(found, truncation)
}
