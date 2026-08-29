//! Why a definition was refused.
//!
//! # Every reason, not the first one
//!
//! [`inspect`] returns a list. The alternative --- stopping at the first problem --- turns
//! fixing a cube into a sequence of builds, and a person doing that stops reading the
//! message somewhere around the third one and declares `Sum` for every measure to make it
//! go away. The refusal then produced exactly the wrong numbers it existed to prevent.
//!
//! # Naming the thing, not the category
//!
//! "this hierarchy has a cycle" sends somebody to read a hierarchy. `east → west → east`
//! sends them to two rows. The same is true of every variant here: each names the specific
//! measure, dimension or member, because a rejection an operator cannot act on is a
//! rejection they will route around.

use crate::model::Definition;
use sankhya_cube_algo::hierarchy::Cyclic;
use std::collections::BTreeSet;
use std::fmt;

/// One reason a definition is not a cube.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Rejection {
    /// A measure declares no aggregation rule for these dimensions.
    ///
    /// **This is the one that matters.** Defaulting it to summation is what every
    /// comparable product does, and it is the most productive source of wrong analytics
    /// figures there is: a balance summed across time, or a rate summed across anything,
    /// gives a number that is plausible, wrong, and indistinguishable from a correct one.
    ///
    /// Every undeclared dimension is named at once, because fixing them one build at a
    /// time is how somebody gives up and declares `Sum` everywhere.
    MeasureUndeclared {
        /// The measure.
        measure: String,
        /// Every dimension it failed to declare a rule for.
        dimensions: Vec<String>,
    },
    /// A measure declares a rule for a dimension the cube does not have.
    ///
    /// Almost always a misspelling, and worth its own variant because the rule it was meant
    /// to be does not exist --- so the same typo also produces a `MeasureUndeclared`, and an
    /// operator reading only that one would re-add the rule they already wrote.
    RuleForUnknownDimension {
        /// The measure.
        measure: String,
        /// The dimension it named.
        dimension: String,
    },
    /// A declared hierarchy consolidates in a cycle, which is named.
    HierarchyCycle {
        /// The dimension.
        dimension: String,
        /// The path, child to parent, returning to where it started.
        cycle: Vec<String>,
    },
    /// Two dimensions, measures or levels share a name.
    Duplicate {
        /// What kind of thing.
        what: &'static str,
        /// The name they share.
        name: String,
    },
    /// A name that has to identify something is blank.
    Blank {
        /// What kind of thing.
        what: &'static str,
        /// Where it was found.
        within: String,
    },
    /// A cube with no dimensions or no measures.
    ///
    /// Not pedantry: it is a table, and asking the cube machinery to serve it costs
    /// planning and materialisation for nothing. The definition is telling you it meant
    /// something else.
    Empty {
        /// What is missing.
        what: &'static str,
    },
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MeasureUndeclared { measure, dimensions } => write!(
                f,
                "measure '{}' declares no aggregation rule along {}. It is refused rather \
                 than summed: summing a balance across time, or a rate across anything, \
                 produces a figure that looks right and is not",
                measure,
                dimensions.join(", ")
            ),
            Self::RuleForUnknownDimension { measure, dimension } => write!(
                f,
                "measure '{measure}' declares a rule along '{dimension}', which is not a \
                 dimension of this cube — check the spelling against the dimension it was \
                 meant to be"
            ),
            Self::HierarchyCycle { dimension, cycle } => write!(
                f,
                "dimension '{}' consolidates in a cycle: {}",
                dimension,
                cycle.join(" → ")
            ),
            Self::Duplicate { what, name } => {
                write!(f, "two {what}s are named '{name}'")
            }
            Self::Blank { what, within } => {
                write!(f, "a {what} within '{within}' has a blank name")
            }
            Self::Empty { what } => write!(
                f,
                "a cube with no {what} is a table; define it as one, or say what was meant"
            ),
        }
    }
}

impl std::error::Error for Rejection {}

/// Every reason this definition is not a cube. Empty means it is one.
#[must_use]
pub fn inspect(definition: &Definition) -> Vec<Rejection> {
    let mut out = Vec::new();

    if definition.dimensions.is_empty() {
        out.push(Rejection::Empty { what: "dimension" });
    }
    if definition.measures.is_empty() {
        out.push(Rejection::Empty { what: "measure" });
    }
    if definition.name.trim().is_empty() {
        out.push(Rejection::Blank { what: "cube", within: definition.fact_table.clone() });
    }
    if definition.fact_table.trim().is_empty() {
        out.push(Rejection::Blank { what: "fact table", within: definition.name.clone() });
    }

    duplicates("dimension", definition.dimensions.iter().map(|d| d.name.as_str()), &mut out);
    duplicates("measure", definition.measures.iter().map(|m| m.name.as_str()), &mut out);

    let names: BTreeSet<&str> =
        definition.dimensions.iter().map(|d| d.name.as_str()).collect();

    for dimension in &definition.dimensions {
        if dimension.name.trim().is_empty() {
            out.push(Rejection::Blank { what: "dimension", within: definition.name.clone() });
        }
        if dimension.table.trim().is_empty() {
            out.push(Rejection::Blank { what: "table", within: dimension.name.clone() });
        }
        if dimension.joins_on.trim().is_empty() {
            out.push(Rejection::Blank { what: "join column", within: dimension.name.clone() });
        }
        if dimension.levels.is_empty() && dimension.parent_child.is_none() {
            out.push(Rejection::Empty { what: "level" });
        }
        for level in &dimension.levels {
            if level.name.trim().is_empty() {
                out.push(Rejection::Blank { what: "level", within: dimension.name.clone() });
            }
            if level.column.trim().is_empty() {
                out.push(Rejection::Blank { what: "level column", within: level.name.clone() });
            }
        }
        duplicates("level", dimension.levels.iter().map(|l| l.name.as_str()), &mut out);

        if let Some(hierarchy) = &dimension.rollups {
            if let Err(Cyclic { cycle }) = hierarchy.validate() {
                out.push(Rejection::HierarchyCycle {
                    dimension: dimension.name.clone(),
                    cycle,
                });
            }
        }
    }

    for measure in &definition.measures {
        let declared: Vec<&str> = definition
            .dimensions
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        if let Err(undeclared) = measure.covers(&declared) {
            out.push(Rejection::MeasureUndeclared {
                measure: undeclared.measure,
                dimensions: undeclared.dimensions,
            });
        }
        for rule in &measure.rules {
            if !names.contains(rule.dimension.as_str()) {
                out.push(Rejection::RuleForUnknownDimension {
                    measure: measure.name.to_string(),
                    dimension: rule.dimension.to_string(),
                });
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

/// Names appearing more than once, reported once each.
fn duplicates<'a>(
    what: &'static str,
    names: impl Iterator<Item = &'a str>,
    out: &mut Vec<Rejection>,
) {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut reported: BTreeSet<&str> = BTreeSet::new();
    for name in names {
        if !seen.insert(name) && reported.insert(name) {
            out.push(Rejection::Duplicate { what, name: name.to_string() });
        }
    }
}
