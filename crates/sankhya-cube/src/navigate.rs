//! Slice, dice, roll up, drill down, pivot.
//!
//! # These are one structure, navigated --- not five unrelated queries
//!
//! `GROUP BY ROLLUP(year, quarter, month)` enumerates combinations of three column names.
//! It does not know that a month is inside a quarter, that a balance may not be summed
//! across time, or that the previous statement and this one are two views of one thing.
//! Every operation here takes a cube and returns a cube, so they compose: dice, then roll
//! up, then drill into the outlier, and the result is still addressable.
//!
//! # Roll-up is the one that can be wrong
//!
//! Slicing, dicing and pivoting only ever remove or rearrange cells. They cannot produce a
//! number that was not already there, so the worst they do is return nothing.
//!
//! Roll-up computes. It takes cells at one grain and produces cells at a coarser one, and
//! whether that is legitimate depends on the measure --- `FR-QUERY-12`. Summing a closing
//! balance across time gives a figure that is plausible and wrong; averaging an average
//! gives one that is subtly wrong in a way nobody notices for a quarter. So [`roll_up`]
//! consults the measure and **refuses**, naming the measure and the dimension, rather than
//! computing something it cannot justify.
//!
//! This is the same rule as [`crate::validate`]'s, at the other end of the system: there it
//! stops a cube being defined without an answer, here it stops a query assuming one.
//!
//! # The operator is the measure's, not the caller's
//!
//! A first version of this module merged the contributing cells and left the caller to
//! choose how to reduce them. That is how a closing balance gets summed: the measure
//! declares `Last` along time, the caller asks for a sum, and every part of the answer is
//! real. So [`roll_up`] reduces **eagerly, under the rule the measure declares for the
//! dimension being rolled away**, and each cell of the result holds one value. There is
//! nothing left for a later call to reduce differently.
//!
//! # `First` and `Last` need to know what order the members are in
//!
//! And this is the trap underneath that one. A semi-additive measure names a *position* ---
//! the closing balance, the opening headcount --- which is meaningless over an unordered
//! bag of contributions. The obvious implementation takes them in whatever order the cells
//! were visited, which for a sorted address map is **lexicographic by member name**.
//!
//! `"feb" < "jan"`, so the closing balance of the first quarter is January's.
//!
//! What makes this worth a type rather than a comment is that it survives testing.
//! ISO-8601 dates sort correctly, so a system tested with `2026-01`, `2026-02` never
//! exhibits it, and the first wrong number appears against member names somebody chose for
//! a report. So the order is a parameter: [`Ordered::By`] states it, and a `First` or `Last`
//! roll-up without one is refused rather than guessed at.

use crate::cells::{Address, Cells, Contributions};
use sankhya_cube_algo::measure::{Measure, Rule};
use sankhya_math::Exact;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Why a roll-up did not happen.
///
/// Two variants rather than one, mirroring [`sankhya_cube_algo::ancestor::Answerable`]. A
/// measure that *declares* it does not compose along a dimension is a correct model of an
/// awkward quantity, and the refusal is the system working. A measure that says **nothing**
/// about the dimension is a definition error --- somebody has not decided --- and telling
/// them "cannot be rolled up" sends them to argue with a rule that was never written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Refused {
    /// The measure declares a rule that does not compose along this dimension.
    NotComposable {
        /// The measure.
        measure: String,
        /// The dimension it may not be rolled up along.
        dimension: String,
    },
    /// The measure declares no rule at all along this dimension.
    Undeclared {
        /// The measure.
        measure: String,
        /// The dimension it says nothing about.
        dimension: String,
    },
    /// The rule names a position --- `First` or `Last` --- and no member order was given.
    ///
    /// Guessing costs nothing to implement and is wrong whenever member names do not sort
    /// into their real sequence, which is most of the time and never in a test.
    OrderRequired {
        /// The measure.
        measure: String,
        /// The dimension whose members have no stated order.
        dimension: String,
        /// The rule that needs one.
        rule: Rule,
    },
    /// A member appears in the data and not in the stated order.
    ///
    /// Placing it first or last would be a guess about the one thing the order exists to
    /// settle, and dropping it would lose facts from a total.
    MemberNotOrdered {
        /// The dimension.
        dimension: String,
        /// The member with no stated position.
        member: String,
    },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotComposable { measure, dimension } => write!(
                f,
                "measure '{measure}' cannot be rolled up along '{dimension}': its value at \
                 the coarser grain is not derivable from its values at the finer one, so \
                 any figure produced here would be plausible and wrong"
            ),
            Self::Undeclared { measure, dimension } => write!(
                f,
                "measure '{measure}' declares no aggregation rule along '{dimension}', so \
                 whether this roll-up is valid has not been decided — this is a gap in the \
                 cube definition, not a property of the measure"
            ),
            Self::OrderRequired { measure, dimension, rule } => write!(
                f,
                "measure '{measure}' reduces along '{dimension}' by {rule}, which names a \
                 position, and the members of '{dimension}' have no stated order. Sorting \
                 them by name would make the closing balance of a quarter January's"
            ),
            Self::MemberNotOrdered { dimension, member } => write!(
                f,
                "member '{member}' of '{dimension}' has no place in the stated order — \
                 placing it first or last would guess at the one thing the order settles"
            ),
        }
    }
}

impl std::error::Error for Refused {}

/// Whether a measure may be aggregated along a dimension.
///
/// # Errors
/// [`Refused`], distinguishing a declared refusal from an undeclared one.
fn permits(measure: &Measure, dimension: &str) -> Result<Rule, Refused> {
    match measure.rule(dimension) {
        Some(rule) if rule.composes() => Ok(rule),
        Some(_) => Err(Refused::NotComposable {
            measure: measure.name.to_string(),
            dimension: dimension.to_string(),
        }),
        None => Err(Refused::Undeclared {
            measure: measure.name.to_string(),
            dimension: dimension.to_string(),
        }),
    }
}

/// Keep only the cells whose member on one dimension is the named one, and drop that
/// dimension.
///
/// The dimension goes because it no longer distinguishes anything --- every remaining cell
/// has the same member on it. Keeping it produces a cube with a degenerate axis, and a
/// subsequent roll-up along it silently does nothing.
#[must_use]
pub fn slice(cells: &Cells, dimension: &str, member: &str) -> Cells {
    let Some(axis) = cells.axis(dimension) else {
        return Cells::over(cells.dimensions().to_vec());
    };
    let remaining: Vec<String> = cells
        .dimensions()
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != axis)
        .map(|(_, name)| name.clone())
        .collect();

    let mut out = Cells::over(remaining);
    for address in cells.addresses() {
        if address.get(axis).map(String::as_str) != Some(member) {
            continue;
        }
        let narrowed: Address = address
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != axis)
            .map(|(_, member)| member.clone())
            .collect();
        copy_cell(cells, address, &mut out, narrowed);
    }
    out
}

/// Keep only the cells whose members fall within the named sets, on every dimension named.
///
/// Unlike [`slice`], every dimension survives: a dice narrows a cube without reducing its
/// rank, which is what makes it composable with a later roll-up.
///
/// A dimension named in `restrictions` that the cube does not have restricts nothing. That
/// is deliberate --- a cube may legitimately have been sliced already --- but it means the
/// result can be wider than the caller expected, so the accepted dimensions are returned.
#[must_use]
pub fn dice(cells: &Cells, restrictions: &[(&str, &[&str])]) -> Diced {
    let mut applied: Vec<(usize, BTreeSet<String>)> = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    for (dimension, members) in restrictions {
        match cells.axis(dimension) {
            Some(axis) => applied.push((
                axis,
                members.iter().map(|m| (*m).to_string()).collect(),
            )),
            None => ignored.push((*dimension).to_string()),
        }
    }

    let mut out = Cells::over(cells.dimensions().to_vec());
    for address in cells.addresses() {
        let kept = applied.iter().all(|(axis, members)| {
            address.get(*axis).is_some_and(|member| members.contains(member))
        });
        if kept {
            copy_cell(cells, address, &mut out, address.clone());
        }
    }
    Diced { cells: out, ignored }
}

/// A diced cube, and the restrictions that did not apply to it.
#[derive(Clone, PartialEq, Debug)]
pub struct Diced {
    /// The narrowed cube.
    pub cells: Cells,
    /// Dimensions named in the restriction that this cube does not have.
    ///
    /// Reported rather than dropped: a caller who dices on a dimension that is not there
    /// gets back more than they asked for, and a filter that silently did not apply is the
    /// kind of thing found in a reconciliation months later.
    pub ignored: Vec<String>,
}

/// What order the members of a dimension are in.
///
/// Only `First` and `Last` need this --- a sum, a maximum and a minimum give the same answer
/// however the contributions are ordered, and asking for an order there would be ceremony.
/// Positional rules give a *different* answer, so for them it is the whole question.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ordered<'a> {
    /// No order stated. A positional rule is refused rather than guessed at.
    Unstated,
    /// The members, first to last.
    By(&'a [&'a str]),
}

impl Ordered<'_> {
    /// Where a member sits, if the order says.
    fn position(&self, member: &str) -> Option<usize> {
        match self {
            Self::Unstated => None,
            Self::By(members) => members.iter().position(|m| *m == member),
        }
    }
}

/// Aggregate a dimension away, producing a cube one rank coarser.
///
/// The result holds **one value per cell**, already reduced under the rule the measure
/// declares for `dimension`. The caller does not choose the operator, because the caller
/// choosing it is how a closing balance gets summed.
///
/// # Errors
/// [`Refused`] when the measure cannot be aggregated along this dimension, declares nothing
/// about it, or reduces by position without a stated member order. Each names the measure
/// and the dimension: "this roll-up is invalid" leaves somebody reading a whole definition.
pub fn roll_up(
    cells: &Cells,
    dimension: &str,
    measure: &Measure,
    order: Ordered<'_>,
) -> Result<Cells, Refused> {
    let rule = permits(measure, dimension)?;
    let Some(axis) = cells.axis(dimension) else {
        return Ok(cells.clone());
    };
    let positional = matches!(rule, Rule::First | Rule::Last);
    if positional && order == Ordered::Unstated {
        return Err(Refused::OrderRequired {
            measure: measure.name.to_string(),
            dimension: dimension.to_string(),
            rule,
        });
    }

    // Gather each coarser cell's contributions, tagged with where the rolled-away member
    // sits, so a positional rule reduces along the dimension rather than along whatever
    // order the addresses happened to be visited in.
    let mut gathered: BTreeMap<Address, Vec<(usize, Exact)>> = BTreeMap::new();
    for address in cells.addresses() {
        let member = address.get(axis).map_or("", String::as_str);
        let at = if positional {
            match order.position(member) {
                Some(at) => at,
                None => {
                    return Err(Refused::MemberNotOrdered {
                        dimension: dimension.to_string(),
                        member: member.to_string(),
                    })
                }
            }
        } else {
            0
        };
        let Some(contributions) = cells.contributions(address) else {
            continue;
        };
        let coarser: Address = address
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != axis)
            .map(|(_, member)| member.clone())
            .collect();
        // The cell's exact sum when it holds one, so a partial aggregate rolls up further
        // without being rounded a second time.
        let slot = gathered.entry(coarser).or_default();
        if contributions.rule_used().is_some() {
            slot.push((at, contributions.exact_sum()));
        } else {
            for value in contributions.values() {
                slot.push((at, Exact::of(&[*value])));
            }
        }
    }

    let remaining: Vec<String> = cells
        .dimensions()
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != axis)
        .map(|(_, name)| name.clone())
        .collect();
    let mut out = Cells::over(remaining);
    for (coarser, mut values) in gathered {
        // Stable by position, so facts within one member keep their arrival order and a
        // `Last` picks the last fact of the last member.
        if positional {
            values.sort_by_key(|(at, _)| *at);
        }
        if rule == Rule::Sum {
            // Exactly, and stored unrounded. Rounding here is what makes an answer from a
            // materialised cuboid differ from the same answer computed from the base ---
            // see the module comment on `crate::cells`.
            let mut exact = Exact::zero();
            for (_, contribution) in &values {
                exact.combine(contribution);
            }
            let _ = out.add_reduced(coarser, rule, exact);
            continue;
        }
        let mut contributions = Contributions::none();
        for (_, contribution) in values {
            // A non-summing rule reduces over the facts themselves, so the expansion is
            // flattened back to the value it represents.
            contributions.push(contribution.to_f64());
        }
        if let Some(reduced) = contributions.reduce(rule) {
            let _ = out.add(coarser, reduced);
        }
    }
    Ok(out)
}

/// Reorder the dimensions, keeping every cell.
///
/// Presentation only: no cell appears, disappears or changes value, and the same cube
/// pivoted twice back is the same cube. A dimension the cube does not have is ignored, and
/// any dimension left unnamed keeps its relative order after the named ones --- so a
/// partial pivot is a partial pivot rather than a silent loss of an axis.
#[must_use]
pub fn pivot(cells: &Cells, order: &[&str]) -> Cells {
    let mut axes: Vec<usize> = Vec::new();
    for name in order {
        if let Some(axis) = cells.axis(name) {
            if !axes.contains(&axis) {
                axes.push(axis);
            }
        }
    }
    for axis in 0..cells.dimensions().len() {
        if !axes.contains(&axis) {
            axes.push(axis);
        }
    }

    let dimensions: Vec<String> = axes
        .iter()
        .filter_map(|axis| cells.dimensions().get(*axis).cloned())
        .collect();
    let mut out = Cells::over(dimensions);
    for address in cells.addresses() {
        let reordered: Address = axes
            .iter()
            .filter_map(|axis| address.get(*axis).cloned())
            .collect();
        copy_cell(cells, address, &mut out, reordered);
    }
    out
}

/// Move every cell's member on one dimension to its parent, per a supplied mapping.
///
/// The rank is unchanged --- this is a drill *along* a hierarchy, not an aggregation away
/// of the axis --- so it is the operation a user performs by clicking a `+` on a row header.
/// It computes, so it takes the same refusal as [`roll_up`]: consolidating members is
/// summing at a coarser grain whatever the rank says.
///
/// A member with no parent in the mapping stays where it is. That is what makes a **ragged**
/// hierarchy work: a branch that reaches its top three levels below another one is at its
/// top, and inventing a parent for it would put a member in the result that does not exist.
///
/// # Errors
/// [`Refused`], as [`roll_up`]. Consolidating members merges cells, so the reduction is the
/// measure's and a positional rule needs the member order for the same reason.
pub fn consolidate_along(
    cells: &Cells,
    dimension: &str,
    parents: &dyn Fn(&str) -> Option<String>,
    measure: &Measure,
    order: Ordered<'_>,
) -> Result<Cells, Refused> {
    let rule = permits(measure, dimension)?;
    if matches!(rule, Rule::First | Rule::Last) && order == Ordered::Unstated {
        return Err(Refused::OrderRequired {
            measure: measure.name.to_string(),
            dimension: dimension.to_string(),
            rule,
        });
    }
    let Some(axis) = cells.axis(dimension) else {
        return Ok(cells.clone());
    };

    // Two passes, because merged cells must reduce once over the union rather than combine
    // two partial answers. For a sum the two agree; for a mean, a maximum or a closing
    // balance they do not.
    let mut gathered: BTreeMap<Address, Vec<Exact>> = BTreeMap::new();
    for address in cells.addresses() {
        let mut moved = address.clone();
        if let Some(member) = address.get(axis) {
            if let Some(parent) = parents(member) {
                if let Some(slot) = moved.get_mut(axis) {
                    *slot = parent;
                }
            }
        }
        let Some(contributions) = cells.contributions(address) else {
            continue;
        };
        let slot = gathered.entry(moved).or_default();
        if contributions.rule_used().is_some() {
            slot.push(contributions.exact_sum());
        } else {
            for value in contributions.values() {
                slot.push(Exact::of(&[*value]));
            }
        }
    }

    let mut out = Cells::over(cells.dimensions().to_vec());
    for (moved, partials) in gathered {
        if rule == Rule::Sum {
            let mut exact = Exact::zero();
            for partial in &partials {
                exact.combine(partial);
            }
            let _ = out.add_reduced(moved, rule, exact);
            continue;
        }
        let mut contributions = Contributions::none();
        for partial in partials {
            contributions.push(partial.to_f64());
        }
        if let Some(reduced) = contributions.reduce(rule) {
            let _ = out.add(moved, reduced);
        }
    }
    Ok(out)
}

/// Move one cell's contributions into another cube, keeping them unreduced.
///
/// Contributions rather than a reduced value, because two cells merging into one must
/// reduce **once over the union**, not combine two partial answers. For a sum the two agree;
/// for a mean, a maximum or a closing balance they do not, and an average of averages is
/// wrong by an amount that depends on how many rows happened to be in each cell.
fn copy_cell(from: &Cells, at: &Address, into: &mut Cells, to: Address) {
    let Some(contributions) = from.contributions(at) else {
        return;
    };
    for value in contributions.values() {
        // The width is this cube's own, so it cannot be wrong; if it somehow is, dropping
        // the fact is better than filing it at an address nobody asked for.
        let _ = into.add(to.clone(), *value);
    }
}
