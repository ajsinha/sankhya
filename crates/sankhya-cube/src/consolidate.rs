//! Consolidating a parent-child hierarchy on the graph engine.
//!
//! # Why this runs on the graph engine and not on a `BTreeMap`
//!
//! [`sankhya_cube_algo::hierarchy`] holds a hierarchy somebody wrote into a definition ---
//! a few dozen declared roll-up edges. A real one is a dimension table: an organisation
//! chart, a chart of accounts, a product taxonomy, hundreds of thousands of members deep
//! and irregular. That is a graph, `SANKHYA` has a graph engine, and consolidation is a
//! bounded traversal over typed edges. Reimplementing it here would produce a second, worse
//! traversal with no budget and no truncation reporting.
//!
//! # Three ways consolidation silently returns a wrong total
//!
//! **A member reachable by two paths, counted twice.** Shared members and alternate
//! roll-ups are the entire point of a modelled hierarchy --- a branch that reports into
//! both a region and a business line, a product in two categories. Consolidate by walking
//! paths and summing, and that branch's facts land in the total once per path. The total is
//! larger than reality, every part of it is real data, and nothing distinguishes it from a
//! correct answer. So this returns a **set of members**, and the caller aggregates over the
//! set. Counting once is a property of the type, not of remembering.
//!
//! **A member that is both a leaf and an inner node, skipped.** In a ragged hierarchy facts
//! attach at any level: a regional office has its own transactions *and* branches beneath
//! it. Consolidate by collecting leaves --- the obvious implementation, and the one a
//! flattened level hierarchy forces --- and the region's own facts vanish. The total is
//! smaller than reality and, again, made entirely of real data. So the result includes
//! **every member of the subtree, the root included**, and `is_leaf` appears nowhere.
//!
//! **A truncated traversal presented as a total.** A budget exists because an unbounded
//! traversal over a cyclic or enormous hierarchy does not return. But a consolidation that
//! stopped early is a *lower bound*, and a lower bound rendered as a total is the most
//! dangerous of the three: it is what an operator acts on. [`Consolidation::members`]
//! therefore refuses when the traversal was truncated, and the partial set has to be asked
//! for by a name that says so.

use sankhya_graph_algo::budget::{Budget, Truncation};
use sankhya_graph_algo::csr::Adjacency;
use sankhya_graph_algo::ids::{EdgeMask, VertexId};
use sankhya_graph_algo::traverse::reachable;
use std::collections::BTreeSet;
use std::fmt;

/// Every member whose facts roll into one member, and whether that is all of them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Consolidation {
    root: VertexId,
    members: BTreeSet<VertexId>,
    truncation: Truncation,
    cycle_through_root: bool,
}

impl Consolidation {
    /// The member consolidated to.
    #[must_use]
    pub fn root(&self) -> VertexId {
        self.root
    }

    /// Every member contributing to the total, each exactly once, the root included.
    ///
    /// # Errors
    /// [`Incomplete`] when the traversal was truncated or the hierarchy consolidates the
    /// root into itself. Both make the set a lower bound, and a lower bound aggregated and
    /// labelled as a total is what an operator acts on.
    pub fn members(&self) -> Result<&BTreeSet<VertexId>, Incomplete> {
        if let Some(why) = self.why_incomplete() {
            return Err(why);
        }
        Ok(&self.members)
    }

    /// The members found, whether or not that is all of them.
    ///
    /// Named so that reading it is a decision. Aggregating this and calling the result a
    /// total is the mistake [`Consolidation::members`] exists to prevent, so a caller
    /// reaching for it should be showing the incompleteness alongside --- see
    /// [`Consolidation::why_incomplete`].
    #[must_use]
    pub fn partial(&self) -> &BTreeSet<VertexId> {
        &self.members
    }

    /// How many members were found.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether none were, which cannot happen: the root consolidates into itself.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Whether this is the whole answer.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.why_incomplete().is_none()
    }

    /// Why this is not the whole answer, or `None` if it is.
    #[must_use]
    pub fn why_incomplete(&self) -> Option<Incomplete> {
        if self.cycle_through_root {
            return Some(Incomplete::Cyclic { member: self.root });
        }
        self.truncation
            .explain()
            .map(|why| Incomplete::Truncated { why })
    }
}

/// Why a consolidation is a lower bound rather than a total.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Incomplete {
    /// The traversal stopped before it finished.
    Truncated {
        /// What stopped it, already phrased for a reader.
        why: String,
    },
    /// The member consolidates into itself.
    ///
    /// Distinguished from truncation because it is a modelling error somebody can fix,
    /// where truncation is a budget somebody can raise. Reporting both as "incomplete"
    /// sends an operator to widen a budget against a hierarchy that will never terminate
    /// its way out of a loop.
    Cyclic {
        /// The member the cycle runs through.
        member: VertexId,
    },
}

impl fmt::Display for Incomplete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { why } => write!(
                f,
                "this consolidation is a lower bound, not a total: {why}"
            ),
            Self::Cyclic { member } => write!(
                f,
                "member {} consolidates into itself, so it has no total — fix the \
                 hierarchy; no budget makes a loop terminate",
                member.0
            ),
        }
    }
}

impl std::error::Error for Incomplete {}

/// Every member whose facts roll into `root`.
///
/// `child_edges` selects the edge types that mean "rolls up into". Following the wrong edge
/// type consolidates something else entirely, so it is a parameter rather than an
/// assumption about edge zero.
///
/// The traversal runs **downward** --- from the root to its descendants --- because
/// consolidation asks what contributes to a member, not what it contributes to. The
/// direction is fixed here rather than offered as a flag: a flag defaulting either way
/// gives the caller who forgot it a well-formed total of the wrong thing.
#[must_use]
pub fn consolidate(
    graph: &Adjacency,
    root: VertexId,
    child_edges: &EdgeMask,
    budget: &Budget,
) -> Consolidation {
    let found = reachable(graph, &[root], child_edges, budget);

    // A set, not a list of paths. A member reachable two ways appears once, which is the
    // whole reason this returns what it returns.
    let members: BTreeSet<VertexId> = found.found.iter().map(|r| r.vertex).collect();

    Consolidation {
        root,
        cycle_through_root: consolidates_into_itself(graph, root, &members, child_edges),
        members,
        truncation: found.truncation,
    }
}

/// Whether any member reached from `root` rolls back up into it.
///
/// Breadth-first expansion terminates on a cycle --- it marks vertices seen --- so a cyclic
/// hierarchy does not hang. It returns a set that looks entirely ordinary instead, and the
/// total computed from it is a number for a member that has no total. Checking costs one
/// pass over the subtree's out-edges.
fn consolidates_into_itself(
    graph: &Adjacency,
    root: VertexId,
    members: &BTreeSet<VertexId>,
    child_edges: &EdgeMask,
) -> bool {
    for member in members {
        if *member == root {
            continue;
        }
        for edge_type in child_edges.types() {
            if graph
                .out_edges(*member, *edge_type)
                .iter()
                .any(|arc| arc.target == root)
            {
                return true;
            }
        }
    }
    false
}
