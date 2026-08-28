//! Resolving `${...}` references.
//!
//! # An unresolved reference is a refusal, not a placeholder
//!
//! This is the one place this design departs from the configurator it was modelled on, and
//! the departure is deliberate.
//!
//! That implementation leaves an unresolvable `${DB_HOST}` in the value "so the problem is
//! visible". It is visible in a configuration dump. It is **not** visible in a connection
//! string, a file path or a bucket name, which is where the value actually goes — and a
//! process that starts and then fails to connect to a host literally named `${DB_HOST}` has
//! turned a configuration error into a runtime one, at a distance from its cause.
//!
//! So an unresolved reference with no default fails the load, naming the key and the
//! reference. That is the same rule as a measure with no aggregation rule, a batch with a
//! null date, and a metric with an undeclared label: **refuse rather than produce something
//! plausible.**
//!
//! # `${NAME:default}`
//!
//! A default makes a reference optional, and is the honest way to express "use this if the
//! deployment does not say otherwise". Everything after the first colon is the default,
//! including further references, so `${HOST:${FALLBACK_HOST}}` works.
//!
//! # Cycles
//!
//! `a = ${b}` and `b = ${a}` is a modelling error somebody can fix in a minute, and an
//! unbounded loop otherwise. The cycle is reported with its path rather than as its
//! existence, for the same reason a hierarchy cycle is.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Why a value could not be resolved.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unresolved {
    /// A reference names nothing, and has no default.
    NoSuchKey {
        /// The key whose value holds the reference.
        key: String,
        /// The reference that could not be resolved.
        reference: String,
    },
    /// References form a cycle.
    Cycle {
        /// The path, in the order it was followed, returning to where it began.
        path: Vec<String>,
    },
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchKey { key, reference } => write!(
                f,
                "`{key}` refers to `${{{reference}}}`, which is not set anywhere and has no \
                 default. Refused rather than left in the value: a host literally named \
                 `${{{reference}}}` turns a configuration error into a connection failure \
                 somewhere else, at a distance from its cause. Write `${{{reference}:some \
                 default}}` if it is genuinely optional"
            ),
            Self::Cycle { path } => write!(
                f,
                "these settings refer to each other in a circle: {}",
                path.join(" → ")
            ),
        }
    }
}

impl std::error::Error for Unresolved {}

/// Resolve every reference in every value.
///
/// `lookup` is consulted before `values`, so a reference reaches whatever the caller
/// considers higher precedence --- an environment variable, a command-line argument ---
/// before it reaches the files.
///
/// # Errors
/// [`Unresolved`] on the first reference that names nothing, or the first cycle. One at a
/// time rather than all of them, because a cycle makes every value on its path unresolvable
/// and reporting twelve consequences of one mistake buries the mistake.
pub fn all(
    values: &BTreeMap<String, String>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, Unresolved> {
    let mut out = BTreeMap::new();
    for key in values.keys() {
        let mut path = Vec::new();
        let resolved = one(key, values, lookup, &mut BTreeSet::new(), &mut path)?;
        out.insert(key.clone(), resolved);
    }
    Ok(out)
}

/// Resolve one key's value.
fn one(
    key: &str,
    values: &BTreeMap<String, String>,
    lookup: &dyn Fn(&str) -> Option<String>,
    visiting: &mut BTreeSet<String>,
    path: &mut Vec<String>,
) -> Result<String, Unresolved> {
    let raw = lookup(key)
        .or_else(|| values.get(key).cloned())
        .unwrap_or_default();
    substitute(key, &raw, values, lookup, visiting, path)
}

/// Replace every reference in one string.
fn substitute(
    key: &str,
    raw: &str,
    values: &BTreeMap<String, String>,
    lookup: &dyn Fn(&str) -> Option<String>,
    visiting: &mut BTreeSet<String>,
    path: &mut Vec<String>,
) -> Result<String, Unresolved> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = matching_brace(after) else {
            // An unterminated `${` is text, not a reference. Treating it as one would refuse
            // a value that may be perfectly deliberate.
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let token = &after[..end];
        rest = &after[end + 1..];

        let (name, default) = match token.split_once(':') {
            Some((name, default)) => (name.trim(), Some(default)),
            None => (token.trim(), None),
        };

        if visiting.contains(name) {
            let mut cycle = path.clone();
            cycle.push(name.to_string());
            return Err(Unresolved::Cycle { path: cycle });
        }

        let replacement = if let Some(value) = lookup(name) {
            value
        } else if let Some(value) = values.get(name) {
            visiting.insert(name.to_string());
            path.push(name.to_string());
            let resolved = substitute(name, value, values, lookup, visiting, path)?;
            path.pop();
            visiting.remove(name);
            resolved
        } else if let Some(default) = default {
            // The default may itself hold references.
            substitute(key, default, values, lookup, visiting, path)?
        } else {
            return Err(Unresolved::NoSuchKey {
                key: key.to_string(),
                reference: name.to_string(),
            });
        };
        out.push_str(&replacement);
    }
    out.push_str(rest);
    Ok(out)
}

/// The index of the `}` closing a reference that begins at the start of `text`.
///
/// Counts nesting, so `${HOST:${FALLBACK}}` closes at the outer brace rather than the inner
/// one --- which would otherwise split a default in half and refuse a valid value.
fn matching_brace(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes.get(index) {
            Some(b'{') => depth += 1,
            Some(b'}') if depth == 0 => return Some(index),
            Some(b'}') => depth -= 1,
            _ => {}
        }
        index += 1;
    }
    None
}
