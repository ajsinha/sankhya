//! Refusing an approximate answer to a question that requires an exact one.
//!
//! # Why this is a planning-time check and not a convention
//!
//! `approx_percentile_cont`, `approx_median` and `approx_distinct` are one autocomplete
//! away from any query, read like their exact counterparts, and return numbers that look
//! right. Nothing about a result says which was used.
//!
//! They are also worse than "approximate" suggests. They are sketch-based, and the
//! sketches are **merge-order dependent**: the same query over the same data returns
//! different values depending on how the work was partitioned. So a figure computed this
//! way cannot be reproduced even by re-running it on a busier machine — which is
//! disqualifying for anything that has to be defended later, quite apart from accuracy.
//!
//! # Two modes, and why the permissive one still reports
//!
//! Where approximation is genuinely acceptable — an exploratory dashboard, a
//! cardinality estimate — the query is allowed, and the functions it used are
//! **reported back**. A caller that does not care can ignore the report; one that is
//! about to put the number in a document cannot claim it did not know.
//!
//! Silence in the permissive mode would make the two modes differ only in whether the
//! query runs, which loses the information exactly where it is most likely to matter.

use datafusion::common::tree_node::{TreeNode, TreeNodeRecursion};
use datafusion::logical_expr::{Expr, LogicalPlan};
use std::collections::BTreeSet;
use std::fmt;

/// Functions whose answers are approximate, merge-order dependent, or both.
///
/// Names rather than a trait check, because these are the engine's own built-ins and
/// there is no marker on them to test. The list is short and its members are stable; a
/// name that disappears from the engine simply stops matching, which fails open in the
/// permissive direction and is caught by the test that asserts each one is still
/// rejected.
const APPROXIMATE: &[&str] = &[
    "approx_percentile_cont",
    "approx_percentile_cont_with_weight",
    "approx_median",
    "approx_distinct",
];

/// Whether a session will accept an approximate answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Exactness {
    /// Approximation is refused at planning time.
    ///
    /// The default, because the failure of the other direction is silent. A query that
    /// is refused is a question; a figure that is quietly approximate is an answer.
    #[default]
    Required,
    /// Approximation is allowed, and reported.
    Permitted,
}

/// Approximate functions a query uses.
///
/// Attached to a result rather than logged, so it travels with the number it describes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Watermark {
    /// In a stable order, so two runs of the same query produce the same watermark.
    pub approximate_functions: BTreeSet<String>,
}

impl Watermark {
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.approximate_functions.is_empty()
    }
}

impl fmt::Display for Watermark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_exact() {
            return f.write_str("exact");
        }
        write!(
            f,
            "approximate, and not reproducible: computed with {}",
            self.approximate_functions
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Why a plan was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExactnessError {
    pub functions: BTreeSet<String>,
}

impl fmt::Display for ExactnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self.functions.iter().cloned().collect();
        write!(
            f,
            "this session requires exact results and the query uses {}, which are \
             sketch-based and merge-order dependent — the same query over the same data \
             returns different values depending on how the work was partitioned. Use the \
             exact equivalent, or set the session to permit approximation and accept the \
             watermark",
            names.join(", ")
        )
    }
}

impl std::error::Error for ExactnessError {}

/// Every approximate function a plan uses.
///
/// # Why the walk descends into subqueries explicitly
///
/// The ordinary plan walk visits plan nodes and their expressions. A scalar subquery is
/// an *expression* that contains a whole nested plan, so the ordinary walk sees the
/// expression and stops — and a query whose outer form is entirely exact would pass while
/// its `WHERE` clause computed an approximate median.
///
/// That is not a hypothetical: it is what the first version of this function did, and
/// the only reason it is not still doing it is that the test for it was written before
/// the implementation was trusted.
#[must_use]
pub fn approximate_functions(plan: &LogicalPlan) -> BTreeSet<String> {
    let mut found = BTreeSet::new();

    let _ = plan.apply_with_subqueries(|node| {
        for expr in node.expressions() {
            let _ = expr.apply(|e| {
                let name = match e {
                    Expr::AggregateFunction(f) => Some(f.func.name()),
                    Expr::ScalarFunction(f) => Some(f.func.name()),
                    Expr::WindowFunction(w) => Some(w.fun.name()),
                    _ => None,
                };
                if let Some(name) = name {
                    if APPROXIMATE.contains(&name) {
                        found.insert(name.to_string());
                    }
                }
                Ok(TreeNodeRecursion::Continue)
            });
        }
        Ok(TreeNodeRecursion::Continue)
    });

    found
}

/// Check a plan against a session's exactness requirement.
///
/// # Errors
///
/// Returns [`ExactnessError`] naming every offending function when the session requires
/// exactness. All of them, not the first: fixing one and re-running to discover the next
/// is a worse experience than being told once.
pub fn check_exactness(
    plan: &LogicalPlan,
    exactness: Exactness,
) -> Result<Watermark, ExactnessError> {
    let functions = approximate_functions(plan);

    if exactness == Exactness::Required && !functions.is_empty() {
        return Err(ExactnessError { functions });
    }

    Ok(Watermark {
        approximate_functions: functions,
    })
}
