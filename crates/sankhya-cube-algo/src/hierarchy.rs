//! Consolidation paths, and the member that must contribute once.
//!
//! # The classic silent cube defect
//!
//! A member reachable from an ancestor by **two paths** must contribute to that ancestor
//! **once**. Alternate roll-ups and shared members are ordinary modelling — a product that
//! belongs to two categories, a cost centre reporting into a region and a business line, an
//! account rolling up both legally and managerially — and a consolidation that walks paths
//! rather than members counts such a leaf as many times as there are routes to it.
//!
//! The result is a total that is too large, with every constituent correct, by an amount
//! that is a plausible size. Nobody reconciles a subtotal, and the figure that is wrong is
//! usually the one at the top that somebody reports.
//!
//! So consolidation is defined over the **set** of reachable leaves rather than over paths.
//! That is the whole of it, and it is one line — but it is a line that has to be written
//! deliberately, because the natural recursive formulation sums over children and is wrong
//! the first time two paths meet.
//!
//! # A cycle is refused when the hierarchy is defined, not when a query runs
//!
//! A parent-child hierarchy with a cycle makes consolidation unbounded. Discovered during a
//! query it presents as a statement that never returns, which is diagnosed as a performance
//! problem and investigated as one. Discovered at definition time it presents as a named
//! cycle, which is a modelling error somebody can fix in a minute.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A parent-child hierarchy over member names.
///
/// Parent-child rather than fixed levels, because ragged is the general case: an
/// organisation where one branch is four deep and another is seven is not unusual, and
/// padding the short branch to match invents members that do not exist. They then appear in
/// results, in member counts, and in drill-downs, and a user asked why a division shows up at
/// four levels of the tree has been handed an implementation detail as their problem.
#[derive(Clone, Debug, Default)]
pub struct Hierarchy {
    /// Child to parents. **Parents**, plural: a shared member has more than one.
    parents: BTreeMap<String, BTreeSet<String>>,
    /// Parent to children, for walking downward.
    children: BTreeMap<String, BTreeSet<String>>,
}

impl Hierarchy {
    /// An empty hierarchy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `child` rolls up into `parent`.
    ///
    /// Adding the same edge twice is not an error and does not double anything --- the
    /// structure is a set of edges, and a definition that lists a relationship twice means
    /// the same thing as one that lists it once.
    pub fn rolls_up(&mut self, child: impl Into<String>, parent: impl Into<String>) {
        let child = child.into();
        let parent = parent.into();
        self.parents
            .entry(child.clone())
            .or_default()
            .insert(parent.clone());
        self.children.entry(parent).or_default().insert(child);
    }

    /// Every member named anywhere.
    #[must_use]
    pub fn members(&self) -> BTreeSet<&str> {
        self.parents
            .keys()
            .chain(self.children.keys())
            .map(String::as_str)
            .collect()
    }

    /// The members directly under `member`.
    #[must_use]
    pub fn children_of(&self, member: &str) -> BTreeSet<&str> {
        self.children
            .get(member)
            .map(|set| set.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// Members with no children: the grain a consolidation actually sums.
    #[must_use]
    pub fn leaves(&self) -> BTreeSet<&str> {
        self.members()
            .into_iter()
            .filter(|member| self.children_of(member).is_empty())
            .collect()
    }

    /// Members with no parent: the tops of the tree.
    #[must_use]
    pub fn roots(&self) -> BTreeSet<&str> {
        self.members()
            .into_iter()
            .filter(|member| !self.parents.contains_key(*member))
            .collect()
    }

    /// The **set** of leaves that consolidate into `member`.
    ///
    /// A set, and that is the entire point. A member reachable by two paths appears here
    /// once, so a consolidation defined over this cannot double-count however many alternate
    /// roll-ups exist above it.
    ///
    /// Returns the member itself when it is a leaf, so a caller need not special-case the
    /// bottom of the tree --- which is where an off-by-one in a consolidation usually is.
    ///
    /// # Errors
    ///
    /// [`Cyclic`] when the hierarchy has a cycle. This should have been refused at
    /// definition time by [`Hierarchy::validate`]; it is checked here too because a function
    /// that recurses over caller-supplied structure must not depend on somebody else having
    /// validated it.
    pub fn consolidates<'a>(&'a self, member: &str) -> Result<BTreeSet<&'a str>, Cyclic> {
        let mut reached = BTreeSet::new();
        let mut on_path = Vec::new();
        // Resolved to a name borrowed from the hierarchy, so every string in the result
        // outlives the caller's argument rather than borrowing from it.
        let start = self.name_of(member);
        if start.is_empty() {
            return Ok(reached);
        }
        self.walk(start, &mut reached, &mut on_path)?;
        Ok(reached)
    }

    fn walk<'a>(
        &'a self,
        member: &'a str,
        reached: &mut BTreeSet<&'a str>,
        on_path: &mut Vec<&'a str>,
    ) -> Result<(), Cyclic> {
        if on_path.contains(&member) {
            let mut cycle: Vec<String> = on_path.iter().map(|m| (*m).to_string()).collect();
            cycle.push(member.to_string());
            return Err(Cyclic { cycle });
        }
        let children = self.children_of(member);
        if children.is_empty() {
            reached.insert(member);
            return Ok(());
        }
        on_path.push(member);
        for child in children {
            self.walk(self.name_of(child), reached, on_path)?;
        }
        on_path.pop();
        Ok(())
    }

    /// The borrowed name for a member, so lifetimes tie to the hierarchy rather than the
    /// caller's string.
    fn name_of<'a>(&'a self, member: &str) -> &'a str {
        self.children
            .get_key_value(member)
            .or_else(|| self.parents.get_key_value(member))
            .map_or("", |(name, _)| name.as_str())
    }

    /// Whether the hierarchy is usable.
    ///
    /// # Errors
    ///
    /// [`Cyclic`] naming the cycle. Naming it matters: "this hierarchy has a cycle" sends
    /// somebody to read the whole definition, and a named path sends them to one edge.
    pub fn validate(&self) -> Result<(), Cyclic> {
        for root in self.roots() {
            self.consolidates(root)?;
        }
        // A hierarchy that is *entirely* a cycle has no roots, so the loop above visits
        // nothing and reports nothing. Every member is checked when there are none.
        if self.roots().is_empty() && !self.members().is_empty() {
            for member in self.members() {
                self.consolidates(member)?;
            }
        }
        Ok(())
    }

    /// How many distinct paths lead from `member` down to `leaf`.
    ///
    /// Not used by consolidation --- which is exactly the point. It exists so a test can
    /// demonstrate that a leaf with several paths still contributes once, and so a modeller
    /// can find shared members deliberately rather than discovering them in a total.
    #[must_use]
    pub fn paths_to(&self, member: &str, leaf: &str) -> usize {
        if member == leaf {
            return 1;
        }
        self.children_of(member)
            .into_iter()
            .map(|child| self.paths_to(child, leaf))
            .sum()
    }
}

/// A hierarchy that consolidates forever.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cyclic {
    /// The path that closes, in order, ending where it began.
    pub cycle: Vec<String>,
}

impl fmt::Display for Cyclic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "this hierarchy consolidates in a cycle: {}. Refused when the hierarchy is \
             defined rather than when a query runs — during a query it presents as a \
             statement that never returns, which gets diagnosed as a performance problem and \
             investigated as one",
            self.cycle.join(" → ")
        )
    }
}

impl std::error::Error for Cyclic {}
