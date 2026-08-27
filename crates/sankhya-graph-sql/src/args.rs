//! Reading a table function's arguments out of the expressions SQL gives us.
//!
//! Arguments arrive as a positional list of `Expr`, which is how SQL's function-call syntax
//! works. Everything here is about turning that into something a traversal can use without
//! guessing.
//!
//! Two rules. **A named argument is looked up by name**, never by position, so adding a
//! parameter cannot silently reassign an existing call's arguments. And **an argument that
//! cannot be read is an error rather than a default** --- a misspelled bound that quietly
//! becomes the default value produces a result that is wrong in a way the query text does
//! not reveal.

use datafusion::common::{plan_datafusion_err, plan_err, Result, ScalarValue};
use datafusion::logical_expr::Expr;
use sankhya_graph_algo::budget::Budget;
use std::collections::BTreeMap;

/// The arguments of one call, positional and named.
#[derive(Debug, Default)]
pub struct Arguments {
    positional: Vec<ScalarValue>,
    named: BTreeMap<String, ScalarValue>,
}

/// Every option a graph function accepts.
///
/// Listed exhaustively and checked against, so that a misspelled bound is an error rather
/// than a silent default. A query asking for `max_dpeth => 3` and getting the default six
/// is wrong in a way its own text does not reveal, and nobody reviewing it would catch that.
const KNOWN_OPTIONS: &[&str] = &[
    "max_depth",
    "max_results",
    "max_visits",
    "max_degree",
    "edge_types",
    "from",
    "until",
    "start_at",
    "max_dwell",
    "min_dwell",
    "min_conservation",
    "damping",
    "floor",
    "k",
];

impl Arguments {
    /// Split a call's expressions into `arity` positional arguments and an options string.
    ///
    /// # Why options are a string rather than named arguments
    ///
    /// SQL's `name => value` syntax is not available: DataFusion's planner rejects it for
    /// table functions outright, and the `name = value` form is resolved as a *column*
    /// against an empty schema and fails before the function is ever called. Only literals
    /// reach a table function.
    ///
    /// So the bounds arrive as one trailing string of `key=value` pairs:
    ///
    /// ```sql
    /// graph_reachable('payments', 'acct-1', 'max_depth=3, min_conservation=0.9')
    /// ```
    ///
    /// A traversal has a dozen tunable bounds, and the alternative --- a dozen positional
    /// arguments --- is unreadable and impossible to review. Every key is checked against
    /// the known set, so this is no less strict than named arguments would have been.
    pub fn parse(exprs: &[Expr], arity: usize) -> Result<Self> {
        let mut out = Self::default();
        for (index, expr) in exprs.iter().enumerate() {
            let value = literal(expr)?;
            if index < arity {
                out.positional.push(value);
                continue;
            }
            let text = match &value {
                ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => s.clone(),
                other => {
                    return plan_err!(
                        "the options argument must be a string of key=value pairs, not {other}"
                    )
                }
            };
            out.absorb_options(&text)?;
        }
        Ok(out)
    }

    /// Read `key=value, key=value` into the named map, refusing anything unrecognised.
    fn absorb_options(&mut self, text: &str) -> Result<()> {
        for pair in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((key, value)) = pair.split_once('=') else {
                return plan_err!(
                    "'{pair}' is not a key=value pair. Options are written as \
                     'max_depth=3, min_conservation=0.9'"
                );
            };
            let key = key.trim().to_lowercase();
            let value = value.trim();
            if !KNOWN_OPTIONS.contains(&key.as_str()) {
                return plan_err!(
                    "'{key}' is not an option any graph function accepts. Known options are \
                     {KNOWN_OPTIONS:?}. Refusing rather than ignoring it: a misspelled bound \
                     that quietly takes its default produces a result that is wrong in a way \
                     the query text does not reveal"
                );
            }
            self.named
                .insert(key, ScalarValue::Utf8(Some(value.to_string())));
        }
        Ok(())
    }

    /// The positional argument at `index`, as a string.
    pub fn string_at(&self, index: usize, what: &str) -> Result<String> {
        let Some(value) = self.positional.get(index) else {
            return plan_err!("a {what} is required as argument {}", index + 1);
        };
        match value {
            ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => Ok(s.clone()),
            other => plan_err!("the {what} must be a string, not {other}"),
        }
    }

    /// A named string option.
    #[must_use]
    pub fn string(&self, name: &str) -> Option<String> {
        match self.named.get(name) {
            Some(ScalarValue::Utf8(Some(s))) => Some(s.clone()),
            _ => None,
        }
    }

    /// A named integer option, refusing a value that is present but unreadable.
    ///
    /// The distinction matters: an absent bound takes its default, and a *malformed* one is
    /// a query the author did not write correctly. Treating the second as the first hides
    /// the mistake behind a plausible answer.
    pub fn integer(&self, name: &str) -> Result<Option<i64>> {
        let Some(text) = self.string(name) else {
            return Ok(None);
        };
        text.parse::<i64>().map(Some).map_err(|_| {
            plan_datafusion_err!("the '{name}' option must be an integer, and '{text}' is not")
        })
    }

    /// A named floating-point option.
    pub fn number(&self, name: &str) -> Result<Option<f64>> {
        let Some(text) = self.string(name) else {
            return Ok(None);
        };
        text.parse::<f64>().map(Some).map_err(|_| {
            plan_datafusion_err!("the '{name}' option must be a number, and '{text}' is not")
        })
    }

    /// A named list option, written as a `|`-separated string.
    ///
    /// Separated by `|` rather than `,` because the options string itself is comma
    /// separated, and nesting one inside the other is how a list silently truncates at its
    /// first element.
    #[must_use]
    pub fn list(&self, name: &str) -> Vec<String> {
        self.string(name)
            .map(|s| {
                s.split('|')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The traversal budget these arguments describe.
    ///
    /// Every bound has a default, and the defaults are the interactive ones --- shallow,
    /// narrow, quick to refuse. `FR-GRAPH-14` requires a hard result limit on every
    /// traversal, so there is no way to express "no bound" here: omitting an option selects
    /// a bound rather than removing one.
    pub fn budget(&self) -> Result<Budget> {
        let mut budget = Budget::interactive();
        if let Some(depth) = self.integer("max_depth")? {
            if depth < 0 {
                return plan_err!("'max_depth' cannot be negative");
            }
            budget.max_depth = u32::try_from(depth).unwrap_or(u32::MAX);
        }
        if let Some(results) = self.integer("max_results")? {
            if results < 0 {
                return plan_err!("'max_results' cannot be negative");
            }
            budget.max_results = usize::try_from(results).unwrap_or(usize::MAX);
        }
        if let Some(visits) = self.integer("max_visits")? {
            if visits < 0 {
                return plan_err!("'max_visits' cannot be negative");
            }
            budget.max_visits = usize::try_from(visits).unwrap_or(usize::MAX);
        }
        if let Some(degree) = self.integer("max_degree")? {
            if degree < 0 {
                return plan_err!("'max_degree' cannot be negative");
            }
            budget.max_degree = usize::try_from(degree).unwrap_or(usize::MAX);
        }
        Ok(budget)
    }
}

/// Read a literal out of an expression, refusing anything that is not one.
///
/// A column reference here would mean the caller wanted a per-row argument, which a table
/// function cannot provide --- it is called once, at planning time. Saying so is better than
/// evaluating it against nothing and producing a null.
fn literal(expr: &Expr) -> Result<ScalarValue> {
    match expr {
        Expr::Literal(value, _) => Ok(value.clone()),
        Expr::Cast(cast) => literal(&cast.expr),
        Expr::Column(column) => plan_err!(
            "'{}' is a column reference, and a graph table function is evaluated once at \
             planning time rather than per row. To drive a traversal from a column, pass \
             the seeds as a subquery",
            column.name
        ),
        other => plan_err!("expected a literal argument, found {other}"),
    }
}
