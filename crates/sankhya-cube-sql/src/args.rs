//! Reading a cube function's arguments out of the expressions SQL gives us.
//!
//! The same two rules as `sankhya-graph-sql`'s, and for the same reasons. **A named option
//! is looked up by name**, so adding one cannot silently reassign an existing call's
//! arguments. And **an option that cannot be read is an error rather than a default**: a
//! misspelled `by` that quietly becomes "roll up to nothing" returns a grand total where a
//! breakdown was asked for, and the query text does not reveal it.
//!
//! Options arrive as one trailing string of `key=value` pairs rather than SQL's `name =>
//! value`, which DataFusion's planner does not accept for table functions --- only literals
//! reach one.

use datafusion::common::{plan_datafusion_err, plan_err, Result, ScalarValue};
use datafusion::logical_expr::Expr;
use std::collections::BTreeMap;

/// Every option a cube function accepts.
///
/// Checked against, so a misspelling is refused. `by=regoin` silently ignored gives a grand
/// total labelled as a breakdown, and nobody reviewing the SQL would catch it.
const KNOWN_OPTIONS: &[&str] = &[
    "by",
    "where",
    "order",
    "overlay",
    "allocate",
    "materialise",
    "min_completeness",
];

/// The arguments of one call.
#[derive(Debug, Default)]
pub(crate) struct Arguments {
    positional: Vec<ScalarValue>,
    named: BTreeMap<String, String>,
}

impl Arguments {
    /// Split a call into `arity` positional arguments and an options string.
    ///
    /// # Errors
    /// A non-literal argument, a non-string options argument, or an unrecognised option.
    pub(crate) fn parse(exprs: &[Expr], arity: usize) -> Result<Self> {
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
            out.absorb(&text)?;
        }
        Ok(out)
    }

    /// Read `key=value, key=value`, refusing anything unrecognised.
    fn absorb(&mut self, text: &str) -> Result<()> {
        for pair in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((key, value)) = pair.split_once('=') else {
                return plan_err!(
                    "'{pair}' is not a key=value pair. Options are written as \
                     'by=region|branch, min_completeness=0.95'"
                );
            };
            let key = key.trim().to_lowercase();
            if !KNOWN_OPTIONS.contains(&key.as_str()) {
                return plan_err!(
                    "'{key}' is not an option any cube function accepts. Known options are \
                     {KNOWN_OPTIONS:?}. Refusing rather than ignoring it: a misspelled \
                     option that quietly takes its default produces a result that is wrong \
                     in a way the query text does not reveal"
                );
            }
            self.named.insert(key, value.trim().to_string());
        }
        Ok(())
    }

    /// The positional argument at `index`, as a string.
    ///
    /// # Errors
    /// A missing or non-string argument.
    pub(crate) fn string_at(&self, index: usize, what: &str) -> Result<String> {
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
    pub(crate) fn string(&self, name: &str) -> Option<String> {
        self.named.get(name).cloned()
    }

    /// A named list option, `|`-separated.
    ///
    /// Separated by `|` rather than `,`, because the options string is itself comma
    /// separated and nesting one inside the other is how a list silently truncates at its
    /// first element.
    #[must_use]
    pub(crate) fn list(&self, name: &str) -> Vec<String> {
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

    /// A named floating-point option.
    ///
    /// # Errors
    /// A value that is present and unreadable --- distinct from an absent one, which takes
    /// its default. Treating a malformed option as absent hides the mistake behind a
    /// plausible answer.
    pub(crate) fn number(&self, name: &str) -> Result<Option<f64>> {
        let Some(text) = self.string(name) else {
            return Ok(None);
        };
        text.parse::<f64>().map(Some).map_err(|_| {
            plan_datafusion_err!("the '{name}' option must be a number, and '{text}' is not")
        })
    }

    /// A named boolean option.
    ///
    /// # Errors
    /// Anything other than `true` or `false`. `materialise=maybe` taking its default is a
    /// query whose text does not describe what it did.
    pub(crate) fn boolean(&self, name: &str) -> Result<Option<bool>> {
        let Some(text) = self.string(name) else {
            return Ok(None);
        };
        match text.to_lowercase().as_str() {
            "true" | "yes" | "on" => Ok(Some(true)),
            "false" | "no" | "off" => Ok(Some(false)),
            other => plan_err!("the '{name}' option must be true or false, and '{other}' is not"),
        }
    }
}

/// A literal expression's value.
fn literal(expr: &Expr) -> Result<ScalarValue> {
    match expr {
        Expr::Literal(value, _) => Ok(value.clone()),
        other => plan_err!(
            "a cube function takes literal arguments; '{other}' is not one. A column \
             reference cannot be resolved here, because the function is called while the \
             statement is still being planned"
        ),
    }
}
