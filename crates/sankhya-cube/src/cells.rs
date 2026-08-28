//! A sparse cube, and the difference between nothing and zero.
//!
//! # Sparse is not an optimisation
//!
//! Six dimensions of a thousand members each is 10^18 cells. A dense cube of any realistic
//! shape does not fit anywhere, and the fraction of it that holds data is usually well under
//! a percent --- most account/period/product combinations simply never happened. So cells
//! are held by address and the absent ones are absent.
//!
//! # Absent is not zero, and reporting it as zero is a lie an operator acts on
//!
//! This is the part that gets built wrong. Once cells are sparse, the convenient reading of
//! a missing cell is `0.0`: it makes every array the same length, every chart complete, and
//! every total easy. It is also false. **"No transactions in this period" and "transactions
//! that net to zero" are different facts**, and the first one printed as `0` is
//! indistinguishable from the second.
//!
//! The two lead to opposite actions. A netting-to-zero cell is a reconciled position. An
//! empty cell is missing data --- a feed that did not arrive, a filter that excluded
//! everything, a join that matched nothing. Rendering it as `0` turns a data-availability
//! incident into a clean report, and nobody investigates a clean report.
//!
//! So [`Cells::get`] returns `Option<f64>`, there is no `unwrap_or(0.0)` anywhere in this
//! crate, and a cell that genuinely aggregated to zero is [`Some(0.0)`](Some) --- present,
//! and distinguishable.
//!
//! # Determinism
//!
//! `FR-QUERY-10` requires two runs over the same snapshot to be bit-identical, and floating
//! point addition is not associative: summing the same values in a different order gives a
//! different number. Partitioned work finishing in a different order is enough to change a
//! total. Every reduction here goes through [`sankhya_math::deterministic_sum`], which fixes the order
//! by magnitude and compensates --- the order gives reproducibility, the compensation gives
//! accuracy, and neither substitutes for the other.

use sankhya_cube_algo::measure::Rule;
use sankhya_math::{deterministic_sum, Exact};
use std::collections::BTreeMap;

/// Where a cell sits: one member per dimension, in the cube's dimension order.
///
/// A `Vec<String>` rather than a map, because the order is the cube's and a cell that
/// carries its own dimension names can disagree with the cube about which is which.
pub type Address = Vec<String>;

/// The facts contributing to one cell, before they are reduced.
///
/// Kept rather than folded on arrival, because the reduction depends on the measure's rule:
/// `Last` needs the ordering, `Mean` needs the count, and a running total cannot produce
/// either after the fact.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Contributions {
    values: Vec<f64>,
    /// Set when this cell holds an aggregate rather than raw facts.
    ///
    /// A reduced cell answers with the rule it was reduced under, whatever rule is asked
    /// for. That is not the accessor being lax --- the value was produced by the measure's
    /// declared rule, and there is no second reading of it. Asking a rolled-up sum for its
    /// maximum is a category error, and answering it with the maximum of the expansion
    /// components would be a number with no meaning at all.
    reduced_under: Option<Rule>,
    /// The unrounded total, when this cell holds a reduced sum.
    exact: Option<Exact>,
}

impl Contributions {
    /// No contributions --- which is not a contribution of zero.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Add a fact.
    pub fn push(&mut self, value: f64) {
        self.values.push(value);
    }

    /// A cell holding an aggregate that has already been reduced, kept unrounded.
    #[must_use]
    pub fn reduced(rule: Rule, exact: Exact) -> Self {
        Self {
            values: vec![exact.to_f64()],
            reduced_under: Some(rule),
            exact: Some(exact),
        }
    }

    /// The rule this cell was reduced under, if it holds an aggregate.
    #[must_use]
    pub const fn rule_used(&self) -> Option<Rule> {
        self.reduced_under
    }

    /// The unrounded total, when this cell holds a reduced sum.
    ///
    /// This is what a materialised cuboid must store. Storing `to_f64()` instead rounds at
    /// every level of the roll-up, and the fast path then disagrees with the slow one.
    #[must_use]
    pub const fn exact(&self) -> Option<&Exact> {
        self.exact.as_ref()
    }

    /// The exact sum of this cell's facts, unrounded.
    #[must_use]
    pub fn exact_sum(&self) -> Exact {
        match &self.exact {
            Some(exact) => exact.clone(),
            None => Exact::of(&self.values),
        }
    }

    /// How many facts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The values, in the order they arrived.
    #[must_use]
    pub fn values(&self) -> &[f64] {
        &self.values
    }

    /// Reduce them under a rule.
    ///
    /// `None` when there is nothing to reduce. A caller wanting a zero has to write one,
    /// which is the point: the substitution should appear in the code that decided it was
    /// safe, not in the code that could not know.
    #[must_use]
    pub fn reduce(&self, rule: Rule) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }
        // A reduced cell answers with the rule that produced it. See `reduced_under`.
        if self.reduced_under.is_some() {
            return match &self.exact {
                Some(exact) => Some(exact.to_f64()),
                None => self.values.first().copied(),
            };
        }
        match rule {
            Rule::Sum => Some(deterministic_sum(&self.values)),
            // `Exact` is used when a cell is *reduced*; a raw cell of facts is summed in
            // canonical order, which is the reproducible reading of one reduction.
            Rule::First => self.values.first().copied(),
            Rule::Last => self.values.last().copied(),
            Rule::Max => self.extreme(true),
            Rule::Min => self.extreme(false),
            Rule::Mean => {
                let total = deterministic_sum(&self.values);
                // The count is exact and the division is one operation, so this is as
                // reproducible as the sum it comes from.
                Some(total / self.values.len() as f64)
            }
            // A measure that composes along nothing has no reduction *from partials*. It
            // still has a value over the rows themselves, and that is not something this
            // type can compute --- it needs the rows, not their contributions.
            Rule::None => None,
        }
    }

    /// The largest or smallest, skipping `NaN` --- but not reporting a cell of nothing but
    /// `NaN` as absent.
    ///
    /// The skip is so one bad value cannot hide three good ones. The second half matters
    /// more and is easier to get wrong: if *every* contribution is `NaN` there is still
    /// data here, and returning `None` would file a data-quality problem under the same
    /// answer as a feed that never arrived. Those are the two facts this whole module exists
    /// to keep apart, so an all-`NaN` cell is `Some(NaN)` --- present, and visibly not a
    /// number.
    fn extreme(&self, largest: bool) -> Option<f64> {
        let mut best: Option<f64> = None;
        for value in &self.values {
            if value.is_nan() {
                continue;
            }
            best = Some(match best {
                None => *value,
                Some(current) if largest => current.max(*value),
                Some(current) => current.min(*value),
            });
        }
        // `self.values` is non-empty here — `reduce` returns early otherwise — so `None` at
        // this point means every value was `NaN`, not that there was nothing.
        best.or(Some(f64::NAN))
    }
}

/// A sparse cube: the cells that exist, and only those.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Cells {
    dimensions: Vec<String>,
    cells: BTreeMap<Address, Contributions>,
}

impl Cells {
    /// An empty cube over these dimensions, in this order.
    #[must_use]
    pub fn over(dimensions: Vec<String>) -> Self {
        Self {
            dimensions,
            cells: BTreeMap::new(),
        }
    }

    /// The dimensions, in the order an address is written.
    #[must_use]
    pub fn dimensions(&self) -> &[String] {
        &self.dimensions
    }

    /// Record an already-reduced aggregate at an address, unrounded.
    ///
    /// # Errors
    /// [`WrongWidth`], as [`Cells::add`].
    pub fn add_reduced(
        &mut self,
        address: Address,
        rule: Rule,
        exact: Exact,
    ) -> Result<(), WrongWidth> {
        self.check_width(&address)?;
        self.cells.insert(address, Contributions::reduced(rule, exact));
        Ok(())
    }

    /// Whether an address names one member per dimension.
    ///
    /// One copy, called by both writers. Two copies of a guard is one copy nothing tests,
    /// and the untested one is where a mutation survives.
    fn check_width(&self, address: &[String]) -> Result<(), WrongWidth> {
        if address.len() != self.dimensions.len() {
            return Err(WrongWidth {
                expected: self.dimensions.len(),
                found: address.len(),
            });
        }
        Ok(())
    }

    /// Record a fact at an address.
    ///
    /// # Errors
    /// [`WrongWidth`] when the address does not name one member per dimension. Silently
    /// padding or truncating would put facts in a cell nobody addressed.
    pub fn add(&mut self, address: Address, value: f64) -> Result<(), WrongWidth> {
        self.check_width(&address)?;
        self.cells.entry(address).or_default().push(value);
        Ok(())
    }

    /// What is at an address, reduced under a rule --- or `None` if nothing is.
    ///
    /// The `Option` is the whole design. See the module comment: an absent cell and a cell
    /// that nets to zero lead to opposite actions, and one printed as the other turns a
    /// missing feed into a clean report.
    #[must_use]
    pub fn get(&self, address: &[String], rule: Rule) -> Option<f64> {
        self.cells.get(address).and_then(|c| c.reduce(rule))
    }

    /// The raw contributions at an address.
    #[must_use]
    pub fn contributions(&self, address: &[String]) -> Option<&Contributions> {
        self.cells.get(address)
    }

    /// Every populated address, in a canonical order.
    ///
    /// Ordered because `FR-QUERY-10` asks for two runs to be bit-identical, and a result set
    /// whose row order varies is not identical however equal its contents are.
    pub fn addresses(&self) -> impl Iterator<Item = &Address> {
        self.cells.keys()
    }

    /// Every populated cell with its reduced value, in a canonical order.
    ///
    /// A cell whose rule yields nothing --- a non-composing measure --- is omitted rather
    /// than rendered, because there is no value to render.
    #[must_use]
    pub fn reduced(&self, rule: Rule) -> Vec<(&Address, f64)> {
        self.cells
            .iter()
            .filter_map(|(address, contributions)| {
                contributions.reduce(rule).map(|value| (address, value))
            })
            .collect()
    }

    /// How many cells hold anything.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether none do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The index of a dimension, if the cube has it.
    #[must_use]
    pub fn axis(&self, dimension: &str) -> Option<usize> {
        self.dimensions.iter().position(|d| d == dimension)
    }
}

/// An address that does not name one member per dimension.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WrongWidth {
    /// How many members the cube's dimensions require.
    pub expected: usize,
    /// How many the address gave.
    pub found: usize,
}

impl std::fmt::Display for WrongWidth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "an address naming {} member(s) cannot address a cube of {} dimension(s); \
             padding or truncating it would file the fact in a cell nobody addressed",
            self.found, self.expected
        )
    }
}

impl std::error::Error for WrongWidth {}
