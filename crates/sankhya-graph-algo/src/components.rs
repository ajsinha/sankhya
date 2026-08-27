//! Connected components, weak and strong.
//!
//! Weak treats every edge as undirected; strong requires mutual reachability. The two
//! answer different questions and the difference is not academic: in a directed network
//! almost everything lands in one weak component, so a weak component is rarely a finding
//! on its own, while a strong component of more than a handful of vertices usually is.

use crate::budget::{Bounded, Budget, Truncation};
use crate::csr::Adjacency;
use crate::ids::{EdgeMask, VertexId};

/// Which component each vertex belongs to.
///
/// Labels are dense from zero and assigned in ascending order of each component's smallest
/// vertex, so the same graph always produces the same labelling. An arbitrary labelling
/// would make two hydrations of identical data compare unequal.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Components {
    label: Vec<u32>,
    count: u32,
}

impl Components {
    /// The component containing `vertex`.
    #[must_use]
    pub fn label_of(&self, vertex: VertexId) -> Option<u32> {
        self.label.get(vertex.index()).copied()
    }

    /// How many components there are.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// The vertices of each component, ascending, ordered by component label.
    #[must_use]
    pub fn groups(&self) -> Vec<Vec<VertexId>> {
        let mut out = vec![Vec::new(); self.count as usize];
        for (index, label) in self.label.iter().enumerate() {
            if let Some(group) = out.get_mut(*label as usize) {
                group.push(VertexId(u32::try_from(index).unwrap_or(u32::MAX)));
            }
        }
        out
    }

    /// The size of the largest component.
    ///
    /// The number worth looking at first. In most real directed networks one weak component
    /// holds nearly everything, and knowing that is what stops "they are connected" being
    /// reported as a finding.
    #[must_use]
    pub fn largest(&self) -> usize {
        self.groups().iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// Components under the assumption that edges may be followed either way.
#[must_use]
pub fn weakly_connected(
    graph: &Adjacency,
    mask: &EdgeMask,
    budget: &Budget,
) -> Bounded<Components> {
    let n = graph.vertex_count();
    let mut label = vec![u32::MAX; n];
    let mut next = 0u32;
    let mut truncation = Truncation::default();
    let mut visits = 0usize;

    for start in 0..n {
        if label.get(start).copied().unwrap_or(0) != u32::MAX {
            continue;
        }
        let mut stack = vec![VertexId(u32::try_from(start).unwrap_or(u32::MAX))];
        if let Some(slot) = label.get_mut(start) {
            *slot = next;
        }
        while let Some(current) = stack.pop() {
            visits = visits.saturating_add(1);
            if visits > budget.max_visits {
                truncation.by_visits = true;
                return Bounded::truncated(
                    Components {
                        label,
                        count: next.saturating_add(1),
                    },
                    truncation,
                );
            }
            for edge_type in mask.types() {
                let both = graph
                    .out_edges(current, *edge_type)
                    .targets()
                    .iter()
                    .chain(graph.in_edges(current, *edge_type).targets().iter());
                for neighbour in both {
                    if label.get(neighbour.index()).copied().unwrap_or(0) != u32::MAX {
                        continue;
                    }
                    if let Some(slot) = label.get_mut(neighbour.index()) {
                        *slot = next;
                    }
                    stack.push(*neighbour);
                }
            }
        }
        next = next.saturating_add(1);
    }

    Bounded::truncated(Components { label, count: next }, truncation)
}

/// Components in which every vertex can reach every other by following edge direction.
///
/// Tarjan's algorithm, written iteratively. The recursive form is shorter and overflows the
/// stack on a graph with a long chain --- which is to say, on real data.
#[must_use]
pub fn strongly_connected(
    graph: &Adjacency,
    mask: &EdgeMask,
    budget: &Budget,
) -> Bounded<Components> {
    let n = graph.vertex_count();
    let mut index_of = vec![u32::MAX; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<VertexId> = Vec::new();
    let mut next_index = 0u32;
    let mut found: Vec<Vec<VertexId>> = Vec::new();
    let mut truncation = Truncation::default();
    let mut visits = 0usize;

    // Each frame is a vertex and how far through its neighbour list we have walked.
    let mut frames: Vec<(VertexId, usize)> = Vec::new();

    for start in 0..n {
        if index_of.get(start).copied().unwrap_or(0) != u32::MAX {
            continue;
        }
        frames.push((VertexId(u32::try_from(start).unwrap_or(u32::MAX)), 0));

        while let Some((current, position)) = frames.pop() {
            if position == 0 {
                visits = visits.saturating_add(1);
                if visits > budget.max_visits {
                    truncation.by_visits = true;
                    return Bounded::truncated(label_components(&found, n), truncation);
                }
                if let Some(slot) = index_of.get_mut(current.index()) {
                    *slot = next_index;
                }
                if let Some(slot) = low.get_mut(current.index()) {
                    *slot = next_index;
                }
                next_index = next_index.saturating_add(1);
                stack.push(current);
                if let Some(slot) = on_stack.get_mut(current.index()) {
                    *slot = true;
                }
            }

            let neighbours = successors(graph, current, mask);
            if let Some(neighbour) = neighbours.get(position).copied() {
                frames.push((current, position.saturating_add(1)));
                if index_of.get(neighbour.index()).copied().unwrap_or(0) == u32::MAX {
                    frames.push((neighbour, 0));
                } else if on_stack.get(neighbour.index()).copied().unwrap_or(false) {
                    let candidate = index_of.get(neighbour.index()).copied().unwrap_or(0);
                    if let Some(slot) = low.get_mut(current.index()) {
                        *slot = (*slot).min(candidate);
                    }
                }
                continue;
            }

            // Every neighbour explored. Propagate this vertex's low-link to its parent.
            if let Some((parent, _)) = frames.last().copied() {
                let child_low = low.get(current.index()).copied().unwrap_or(0);
                if let Some(slot) = low.get_mut(parent.index()) {
                    *slot = (*slot).min(child_low);
                }
            }

            if low.get(current.index()).copied() == index_of.get(current.index()).copied() {
                let mut group = Vec::new();
                while let Some(member) = stack.pop() {
                    if let Some(slot) = on_stack.get_mut(member.index()) {
                        *slot = false;
                    }
                    group.push(member);
                    if member == current {
                        break;
                    }
                }
                group.sort_unstable();
                found.push(group);
            }
        }
    }

    Bounded::truncated(label_components(&found, n), truncation)
}

/// Assign dense labels ordered by each group's smallest vertex.
///
/// Shared with community detection, which produces groups by a different route and needs
/// the same reproducible labelling.
pub(crate) fn from_groups(found: &[Vec<VertexId>], n: usize) -> Components {
    label_components(found, n)
}

/// Assign dense labels ordered by each group's smallest vertex.
fn label_components(found: &[Vec<VertexId>], n: usize) -> Components {
    let mut groups: Vec<&Vec<VertexId>> = found.iter().collect();
    groups.sort_by_key(|g| g.first().copied().unwrap_or(VertexId(u32::MAX)));

    let mut label = vec![u32::MAX; n];
    for (index, group) in groups.iter().enumerate() {
        for member in group.iter() {
            if let Some(slot) = label.get_mut(member.index()) {
                *slot = u32::try_from(index).unwrap_or(u32::MAX);
            }
        }
    }
    Components {
        label,
        count: u32::try_from(groups.len()).unwrap_or(u32::MAX),
    }
}

/// The out-neighbours of a vertex across every permitted type, deduplicated.
fn successors(graph: &Adjacency, vertex: VertexId, mask: &EdgeMask) -> Vec<VertexId> {
    let mut out: Vec<VertexId> = mask
        .types()
        .iter()
        .flat_map(|t| graph.out_edges(vertex, *t).targets().iter().copied())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}
