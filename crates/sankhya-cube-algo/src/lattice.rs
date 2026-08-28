//! The lattice of cuboids, and choosing which to keep.
//!
//! # Nobody can choose these by hand
//!
//! A cube with *n* dimensions has 2^n cuboids before hierarchies are considered, and
//! ∏(levels + 1) with them --- past a thousand at six dimensions of three levels. Materialise
//! all of them and the storage exceeds the fact table many times over. Materialise none and
//! every query pays full price.
//!
//! Choosing well is a known problem with a known answer: **greedy selection under a space
//! budget**, repeatedly taking the cuboid with the greatest benefit per unit of space. It is
//! within a constant factor of optimal, and — more usefully — it can be driven by the queries
//! actually being run rather than by somebody's guess about which will be.
//!
//! A person cannot do this. Nobody looks at a thousand-node lattice and picks the forty that
//! pay for themselves, and the attempt produces a cube tuned for the queries somebody
//! imagined.
//!
//! # Benefit counts only what the cuboid may *legally* answer
//!
//! This is the subtle part, and it is where the two halves of this crate meet.
//!
//! The textbook benefit of a cuboid is the saving it brings to every query it could serve.
//! But whether it can serve one is not a question about dimensions alone --- it is
//! [`answerable_from`], and it depends on the **measure**. A cuboid grouping by time and
//! account looks like it serves every coarser query; for a distinct count it serves none of
//! them, because a distinct count composes along nothing.
//!
//! Count benefit without that test and the selection materialises cuboids whose value was
//! computed from roll-ups the planner will refuse. The storage is paid, the benefit never
//! arrives, and nothing reports it: the cube is simply slower than its own model says, in a
//! way that looks like the model being optimistic rather than wrong.

use crate::ancestor::{answerable_from, rolled_away};
use crate::measure::Measure;
use std::collections::BTreeSet;

/// One node of the lattice: the dimensions a cuboid groups by.
///
/// Sorted and deduplicated on construction, so two cuboids naming the same dimensions in
/// different orders are the same cuboid. They are otherwise two entries competing for space
/// to hold identical data.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Cuboid {
    dimensions: Vec<String>,
}

impl Cuboid {
    /// A cuboid grouping by these dimensions.
    #[must_use]
    pub fn of<S: AsRef<str>>(dimensions: &[S]) -> Self {
        let unique: BTreeSet<String> = dimensions
            .iter()
            .map(|d| d.as_ref().to_string())
            .collect();
        Self {
            dimensions: unique.into_iter().collect(),
        }
    }

    /// The dimensions, in a stable order.
    #[must_use]
    pub fn dimensions(&self) -> Vec<&str> {
        self.dimensions.iter().map(String::as_str).collect()
    }

    /// How many dimensions it groups by.
    #[must_use]
    pub fn width(&self) -> usize {
        self.dimensions.len()
    }

    /// Whether this cuboid can answer `query` for `measure`.
    ///
    /// Both halves: it must hold every dimension the query needs, **and** the measure must
    /// compose along everything being rolled away.
    #[must_use]
    pub fn answers(&self, query: &Self, measure: &Measure) -> bool {
        let held = self.dimensions();
        let wanted = query.dimensions();
        rolled_away(&wanted, &held)
            .is_some_and(|away| answerable_from(measure, &away).permitted())
    }
}

/// Every cuboid of a cube, and what each costs to keep.
#[derive(Clone, Debug)]
pub struct Lattice {
    cuboids: Vec<Cuboid>,
}

impl Lattice {
    /// Every subset of `dimensions`, which is every cuboid.
    ///
    /// Exponential by nature. A caller with more dimensions than it wants to enumerate
    /// should supply the candidates it cares about via [`Lattice::over`] rather than expect
    /// this to be clever --- there is no clever, the lattice is that size.
    #[must_use]
    pub fn all<S: AsRef<str>>(dimensions: &[S]) -> Self {
        let names: Vec<&str> = dimensions.iter().map(AsRef::as_ref).collect();
        let mut cuboids = Vec::with_capacity(1 << names.len().min(20));
        for mask in 0..(1_u32 << names.len().min(20)) {
            let chosen: Vec<&str> = names
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, name)| *name)
                .collect();
            cuboids.push(Cuboid::of(&chosen));
        }
        cuboids.sort();
        cuboids.dedup();
        Self { cuboids }
    }

    /// A lattice over exactly these cuboids.
    #[must_use]
    pub fn over(cuboids: Vec<Cuboid>) -> Self {
        let mut cuboids = cuboids;
        cuboids.sort();
        cuboids.dedup();
        Self { cuboids }
    }

    /// The cuboids, in a stable order.
    #[must_use]
    pub fn cuboids(&self) -> &[Cuboid] {
        &self.cuboids
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cuboids.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cuboids.is_empty()
    }
}

/// What a query costs to answer from a given cuboid, and what a cuboid costs to keep.
///
/// A trait rather than a row count, because the two are different quantities and a caller
/// that has real statistics should use them. The default reading --- cost is the cuboid's
/// cardinality --- is the one the literature uses and is good enough to choose between
/// cuboids, which is all this needs.
pub trait Cost {
    /// How many rows a cuboid holds.
    fn rows(&self, cuboid: &Cuboid) -> u64;
}

/// A chosen cuboid and why it was chosen.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chosen {
    /// The cuboid.
    pub cuboid: Cuboid,
    /// Rows saved across the queries it newly serves.
    ///
    /// Reported rather than discarded, because "why is this materialised?" is the question
    /// an operator asks about storage they did not choose, and a number they can compare is
    /// a better answer than a policy name.
    pub benefit: u64,
    /// Rows it costs to hold.
    pub cost: u64,
}

/// Choose cuboids greedily under a row budget.
///
/// `queries` is what the selection is *for* --- the cuboids people actually ask for, from a
/// query log. Selecting against the whole lattice instead optimises for queries nobody runs,
/// which is the same mistake as a person guessing, made faster.
///
/// The base cuboid --- the finest one, which answers everything --- is assumed present and
/// free: it is the fact data, it is not optional, and counting it against the budget would
/// make the budget a statement about the table rather than about the materialisation.
#[must_use]
pub fn select(
    lattice: &Lattice,
    queries: &[Cuboid],
    measure: &Measure,
    cost: &dyn Cost,
    budget_rows: u64,
    base: &Cuboid,
) -> Vec<Chosen> {
    let mut chosen: Vec<Chosen> = Vec::new();
    let mut spent = 0_u64;

    loop {
        let mut best: Option<Chosen> = None;
        for candidate in lattice.cuboids() {
            // The `chosen` test is redundant *given* how benefit is computed --- a cuboid
            // already held is in `already`, so it saves nothing against itself and is
            // skipped by the zero-gain test below. It is kept because that argument is
            // non-local: it depends on `benefit` continuing to include the held set, and a
            // future change there would turn an infinite loop into the failure mode.
            if candidate == base || chosen.iter().any(|c| &c.cuboid == candidate) {
                continue;
            }
            let price = cost.rows(candidate);
            if spent.saturating_add(price) > budget_rows {
                continue;
            }
            let gain = benefit(candidate, queries, measure, cost, &chosen, base);
            if gain == 0 {
                continue;
            }
            let better = best
                .as_ref()
                .is_none_or(|current| worth_more(gain, price, current.benefit, current.cost));
            if better {
                best = Some(Chosen {
                    cuboid: candidate.clone(),
                    benefit: gain,
                    cost: price,
                });
            }
        }
        let Some(winner) = best else { return chosen };
        spent = spent.saturating_add(winner.cost);
        chosen.push(winner);
    }
}

/// Whether one candidate's benefit per row beats another's.
///
/// Extracted and named rather than written inline, because it is a numeric comparison whose
/// wrong version produces a valid answer. **`a/b > c/d` is compared as `a*d > c*b`**, which
/// is exact for positive denominators; integer division rounds two genuinely different
/// candidates to the same score, and the choice between them then falls to iteration order.
///
/// The result is still a legal selection — just not the best one — so nothing downstream
/// fails and nothing reports it. That is also why it was extracted: buried in a loop it could
/// only be tested through a fixture engineered to distinguish 1.9 from 1.1 by their
/// benefits, and a property this subtle should be testable in one line.
#[must_use]
pub fn worth_more(benefit: u64, cost: u64, than_benefit: u64, than_cost: u64) -> bool {
    u128::from(benefit) * u128::from(than_cost.max(1))
        > u128::from(than_benefit) * u128::from(cost.max(1))
}

/// Rows saved across the queries this candidate would newly serve.
///
/// Only queries the measure permits it to answer --- see the module comment. A benefit
/// counted over roll-ups the planner will refuse buys storage and delivers nothing.
#[must_use]
pub fn benefit(
    candidate: &Cuboid,
    queries: &[Cuboid],
    measure: &Measure,
    cost: &dyn Cost,
    already: &[Chosen],
    base: &Cuboid,
) -> u64 {
    let mut saved = 0_u64;
    for query in queries {
        if !candidate.answers(query, measure) {
            continue;
        }
        // What answering it costs today: the cheapest thing already available that may
        // legally serve it, falling back to the base.
        let current = already
            .iter()
            .map(|c| &c.cuboid)
            .chain(std::iter::once(base))
            .filter(|held| held.answers(query, measure))
            .map(|held| cost.rows(held))
            .min()
            .unwrap_or_else(|| cost.rows(base));
        saved = saved.saturating_add(current.saturating_sub(cost.rows(candidate)));
    }
    saved
}
