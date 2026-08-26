//! Exact order statistics, with the convention stated rather than assumed.
//!
//! # Why the convention has to be named
//!
//! "The 99th percentile" does not identify a number. There are several standard
//! definitions, they are all defensible, and **they disagree precisely at the tail** —
//! which is the only place anyone asks for a 99th percentile. Two systems can both be
//! correct, report different figures for the same data, and spend a week reconciling.
//!
//! So the convention is a required argument. There is no default, because a default is
//! how one becomes an accident.
//!
//! # Why approximation is not offered here
//!
//! Sketch-based quantiles are not merely approximate: they are **merge-order
//! dependent**, so the same query over the same data returns different values on
//! different runs depending on how the work was partitioned. An approximate answer can
//! be a defensible compromise; an answer that changes when the cluster is resized cannot,
//! and it is indefensible in anything that has to be reproduced later.

use std::cmp::Ordering;
use std::fmt;

/// How to resolve a quantile that falls between two observations.
///
/// The names are the ones the statistical literature uses, so a figure produced here can
/// be reproduced elsewhere without guessing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Convention {
    /// The smallest observation whose cumulative rank is at least `q`.
    ///
    /// Always returns a value that is actually in the data, which matters when the
    /// figure will be pointed at. Used by the classic definition of a percentile.
    NearestRank,
    /// Linear interpolation between the two neighbouring observations, `h = (n-1)q`.
    ///
    /// The default in most numerical libraries and spreadsheets, so it is usually the
    /// convention someone means when they have not said. Returns a value that need not
    /// appear in the data.
    LinearInterpolation,
    /// The largest observation at or below the quantile position.
    ///
    /// Conservative: never overstates. Chosen where a figure feeds a limit that must not
    /// be exceeded by an artefact of interpolation.
    Lower,
}

impl fmt::Display for Convention {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NearestRank => "nearest-rank",
            Self::LinearInterpolation => "linear-interpolation",
            Self::Lower => "lower",
        })
    }
}

/// Why a quantile could not be computed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum QuantileError {
    /// No observations.
    ///
    /// Distinct from zero: a quantile of nothing is undefined, and returning zero would
    /// put a plausible number where there is no answer.
    Empty,
    /// `q` is outside `[0, 1]`.
    OutOfRange,
    /// The input contains a value that cannot be ordered.
    ///
    /// A NaN makes the comparison a partial order. Any total order imposed on it is
    /// arbitrary — sorting NaN to the end is a common choice and silently shifts every
    /// quantile — so the input is refused instead.
    NotOrderable { at: usize },
}

impl fmt::Display for QuantileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str(
                "a quantile of no observations is undefined; returning zero would put a \
                 plausible number where there is no answer",
            ),
            Self::OutOfRange => f.write_str("a quantile must be between 0 and 1 inclusive"),
            Self::NotOrderable { at } => write!(
                f,
                "observation {at} is not orderable, so every quantile of this input \
                 would depend on where it was arbitrarily placed"
            ),
        }
    }
}

impl std::error::Error for QuantileError {}

fn total_order(a: &f64, b: &f64) -> Ordering {
    a.partial_cmp(b).unwrap_or(Ordering::Equal)
}

/// Select the `k`th smallest value in place, without fully sorting.
///
/// Quickselect: linear on average against a full sort's `n log n`. It still requires the
/// observations to be resident — see the module note in the crate documentation on where
/// that stops being acceptable.
fn select_nth(values: &mut [f64], k: usize) -> f64 {
    let (_, nth, _) = values.select_nth_unstable_by(k, total_order);
    *nth
}

/// The exact `q`-quantile of `values`, under a named convention.
///
/// `values` is reordered. That is deliberate rather than incidental: copying to preserve
/// the caller's order would double the memory for an operation already at the limit of
/// what fits.
///
/// # Errors
///
/// Refuses an empty input, a `q` outside `[0, 1]`, or any value that cannot be ordered.
pub fn quantile(values: &mut [f64], q: f64, convention: Convention) -> Result<f64, QuantileError> {
    if !(0.0..=1.0).contains(&q) {
        return Err(QuantileError::OutOfRange);
    }
    if values.is_empty() {
        return Err(QuantileError::Empty);
    }
    if let Some(at) = values.iter().position(|v| v.is_nan()) {
        return Err(QuantileError::NotOrderable { at });
    }

    let n = values.len();
    match convention {
        Convention::NearestRank => {
            // ceil(q * n), clamped into range, then converted to a zero-based index.
            let rank = (q * n as f64).ceil().max(1.0) as usize;
            Ok(select_nth(values, rank.min(n) - 1))
        }
        Convention::Lower => {
            let position = q * (n - 1) as f64;
            Ok(select_nth(values, position.floor() as usize))
        }
        Convention::LinearInterpolation => {
            let position = q * (n - 1) as f64;
            let lower_index = position.floor() as usize;
            let fraction = position - position.floor();
            let lower = select_nth(values, lower_index);
            if fraction == 0.0 || lower_index + 1 >= n {
                return Ok(lower);
            }
            // select_nth_unstable partitions around the index, so everything above it is
            // still on the right-hand side and the next order statistic is the minimum
            // of that side.
            let upper = select_nth(values, lower_index + 1);
            Ok(lower + (upper - lower) * fraction)
        }
    }
}

/// Element-wise sum of fixed-size vectors, then the quantile of the result.
///
/// # Why this is a function rather than a note in a document
///
/// The wrong version — take each vector's quantile, then add them up — is the natural
/// thing to write, reads correctly, and is wrong for every non-linear measure. Two
/// portfolios' worst cases do not occur in the same scenario, so adding their individual
/// worst cases overstates the combined worst case, usually by a lot.
///
/// Providing the correct composition as a named operation is more useful than warning
/// against the incorrect one.
///
/// # Errors
///
/// Refuses an empty input, vectors of differing lengths (the elements would no longer
/// correspond to the same scenario), or a `q` outside range.
pub fn quantile_of_sum(
    vectors: &[Vec<f64>],
    q: f64,
    convention: Convention,
) -> Result<f64, QuantileError> {
    if vectors.is_empty() {
        return Err(QuantileError::Empty);
    }
    let width = vectors[0].len();
    if width == 0 {
        return Err(QuantileError::Empty);
    }
    if vectors.iter().any(|v| v.len() != width) {
        // Element *i* of every vector must be the same scenario, or the sum is
        // meaningless. Padding or truncating would produce a number rather than an
        // error, which is worse.
        return Err(QuantileError::NotOrderable { at: 0 });
    }

    let mut totals = vec![0.0f64; width];
    for element in 0..width {
        let column: Vec<f64> = vectors.iter().map(|v| v[element]).collect();
        totals[element] = crate::deterministic_sum(&column);
    }

    quantile(&mut totals, q, convention)
}
