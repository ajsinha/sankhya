//! Which tier a statement belongs to.
//!
//! # The decision this implements
//!
//! `ADR-0020` Decision 2, and its invariant first:
//!
//! > A user does not know whether their query was answered by the transactional tier or the
//! > analytical one, and **must never need to know**. Every built-in works on every query.
//!
//! `ARCHITECTURE` §5.7 routes by query shape: a point lookup by key goes to the transactional
//! store, because a b-tree probe against the authoritative copy is simultaneously fastest and
//! freshest. That store is PostgreSQL, and **a Rust UDF registered in DataFusion does not
//! exist there**.
//!
//! So a statement calling a built-in is one more shape, and it routes to the analytical path.
//! The user writes the function and it works; they are never told which tier answered, because
//! it does not change the answer.
//!
//! # Why this is built before the router it is for
//!
//! Nothing routes to PostgreSQL today --- `sankhya-oltp-pg` is built and unwired. This is
//! written now because a rule of this kind is unaffordable to retrofit: by the time both tiers
//! answer queries, the second implementation of every function is already written, and the day
//! two implementations disagree the answer depends on a routing decision nobody can see.
//!
//! It is also **testable now**, which the router is not. Whether a statement names a built-in
//! is a property of the text, and it is the half that will be wrong.

use crate::entry::Entry;

/// Where a statement must be answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Either tier may answer; the router decides on shape as it always has.
    Either,
    /// The analytical path, because the statement calls a built-in that lives only there.
    Analytical,
}

/// Whether a statement calls any function in `catalogue`, and so must go analytical.
///
/// # Why the match is on a call and not on the bare name
///
/// A column called `erf`, a table called `functions`, a string literal containing `norm_cdf` ---
/// none of those is a call, and routing on them would send an ordinary point lookup down the
/// analytical path for nothing. The name must be followed by an opening parenthesis, and
/// preceded by something that is not part of an identifier, so `my_erf(x)` is not `erf(x)`.
///
/// # What it deliberately does not do
///
/// It does not parse. A parser here would be a second SQL grammar to keep in step with the
/// engine's, and the cost of being wrong in the safe direction is one query on the slower path.
/// Being wrong in the *other* direction --- missing a call and routing to a tier that cannot
/// answer it --- is a refusal for a function the catalogue says exists, so the match is
/// deliberately eager: a string literal that happens to read like a call routes analytical, and
/// nothing about the answer changes.
#[must_use]
pub fn tier_for(sql: &str, catalogue: &[Entry]) -> Tier {
    let lowered = sql.to_ascii_lowercase();
    for entry in catalogue {
        if calls(&lowered, entry.name) {
            return Tier::Analytical;
        }
    }
    Tier::Either
}

/// Which built-ins a statement names, for a plan or an explanation.
///
/// Sorted and deduplicated, so two calls give the same answer and an operator reading it can
/// tell one statement's functions from another's.
#[must_use]
pub fn built_ins_in(sql: &str, catalogue: &[Entry]) -> Vec<&'static str> {
    let lowered = sql.to_ascii_lowercase();
    let mut found: Vec<&'static str> = catalogue
        .iter()
        .filter(|entry| calls(&lowered, entry.name))
        .map(|entry| entry.name)
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Whether `sql` contains a **call** to `name`.
///
/// The boundary on each side is what stops `my_erf(x)` matching `erf` and `erfc(x)` matching
/// `erf` --- the second being the one that would actually happen, since every family here has
/// names that are prefixes of each other.
fn calls(lowered: &str, name: &str) -> bool {
    let mut from = 0usize;
    while let Some(at) = lowered[from..].find(name) {
        let start = from + at;
        let end = start + name.len();

        let before_ok = start == 0
            || lowered[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');

        // Whitespace between the name and the parenthesis is legal SQL and somebody writes it.
        let after = lowered[end..].trim_start();
        let after_ok = after.starts_with('(');

        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}
