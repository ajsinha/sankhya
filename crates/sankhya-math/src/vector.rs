//! Vector kernels over flat slices, deterministic where they reduce.
//!
//! # Why these are here rather than in a library
//!
//! Every useful vector kernel ends in a floating-point sum: a dot product, a norm, a mean,
//! a cosine distance. And floating-point addition is not associative, so a sum whose order
//! depends on how work was partitioned returns a different number when the machine is
//! busier or has more cores.
//!
//! This crate exists because of that. Bypassing it here would undo the decision in the
//! numerically worst place --- a dot product over widely-scaled factors is exactly where
//! non-associativity bites hardest, because the products span many orders of magnitude
//! before anything is added.
//!
//! So **every reducing kernel goes through [`crate::deterministic_sum`]**, and a library
//! that reorders freely for speed --- which is what a good numeric library does --- cannot
//! be used for this part.
//!
//! # What is fast and what is not
//!
//! Elementwise operations are exact, allocate once, and autovectorise: the compiler turns a
//! loop over two `&[f64]` slices into SIMD without help. Reductions do not vectorise,
//! because the order is the guarantee. That asymmetry is deliberate and it is the price of
//! a result that ties out.
//!
//! # Why slices and not arrays
//!
//! An Arrow `FixedSizeList<Float64, N>` stores its values in one contiguous child buffer,
//! so a column of `rows` vectors of dimension `N` is a flat `&[f64]` of length `rows * N`.
//! Taking slices means these kernels operate on that buffer directly, with no copy and no
//! Arrow dependency in this crate.

use crate::reduce::deterministic_sum;
use std::fmt;

/// Why a kernel could not be applied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VectorError {
    /// Two vectors of different lengths were combined.
    ///
    /// Refused rather than truncated to the shorter. Truncating produces a number, and a
    /// dot product over the first `min(a, b)` elements is not a dot product of anything ---
    /// it is a plausible value with no meaning, which is the worst kind.
    LengthMismatch {
        /// The left operand's length.
        left: usize,
        /// The right operand's length.
        right: usize,
    },
    /// An operation with no meaningful answer on an empty vector.
    ///
    /// A mean of nothing is not zero. A norm of nothing arguably is, and is offered; a
    /// mean is not, because zero is a value somebody will act on.
    Empty,
    /// A cosine distance against a vector of zero length.
    ///
    /// The angle to the origin is undefined, not zero and not one. Returning either would
    /// place the vector at a definite similarity to everything, which sorts it to the top
    /// or bottom of every ranked result.
    ZeroMagnitude,
}

impl fmt::Display for VectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthMismatch { left, right } => write!(
                f,
                "cannot combine vectors of length {left} and {right}. Refusing rather than \
                 truncating to the shorter: a dot product over the first few elements is a \
                 plausible number with no meaning"
            ),
            Self::Empty => f.write_str(
                "this operation has no answer for an empty vector. A mean of nothing is not \
                 zero, and returning zero gives somebody a value to act on",
            ),
            Self::ZeroMagnitude => f.write_str(
                "cosine similarity is undefined against a vector of zero magnitude: the \
                 angle to the origin is not zero and not one, and returning either places \
                 the vector at a definite similarity to everything",
            ),
        }
    }
}

impl std::error::Error for VectorError {}

/// Check two vectors can be combined.
fn same_length(a: &[f64], b: &[f64]) -> Result<(), VectorError> {
    if a.len() == b.len() {
        Ok(())
    } else {
        Err(VectorError::LengthMismatch {
            left: a.len(),
            right: b.len(),
        })
    }
}

// --- elementwise ----------------------------------------------------------
//
// Exact, order-independent by construction, and the loops autovectorise. Nothing here
// needs the deterministic machinery because nothing here reduces.

/// Elementwise sum.
pub fn add(a: &[f64], b: &[f64]) -> Result<Vec<f64>, VectorError> {
    same_length(a, b)?;
    Ok(a.iter().zip(b).map(|(x, y)| x + y).collect())
}

/// Elementwise difference.
pub fn subtract(a: &[f64], b: &[f64]) -> Result<Vec<f64>, VectorError> {
    same_length(a, b)?;
    Ok(a.iter().zip(b).map(|(x, y)| x - y).collect())
}

/// Elementwise product, sometimes called the Hadamard product.
pub fn multiply(a: &[f64], b: &[f64]) -> Result<Vec<f64>, VectorError> {
    same_length(a, b)?;
    Ok(a.iter().zip(b).map(|(x, y)| x * y).collect())
}

/// Elementwise quotient.
///
/// Division by zero yields an infinity rather than an error, deliberately and unlike the
/// scalar case: an elementwise operation over a thousand-element vector should not fail
/// entirely because one element is zero, and the infinity is visible in the result where a
/// refusal would hide the other 999 answers.
pub fn divide(a: &[f64], b: &[f64]) -> Result<Vec<f64>, VectorError> {
    same_length(a, b)?;
    Ok(a.iter().zip(b).map(|(x, y)| x / y).collect())
}

/// Multiply every element by a scalar.
#[must_use]
pub fn scale(a: &[f64], by: f64) -> Vec<f64> {
    a.iter().map(|x| x * by).collect()
}

// --- reductions -----------------------------------------------------------
//
// Every one of these goes through `deterministic_sum`. That is the whole reason this module
// is here rather than delegating to a numeric library.

/// The dot product.
///
/// The products are formed first --- exactly, since multiplication of two `f64` is a single
/// rounding --- and then summed deterministically. Summing as it goes would be faster and
/// would make the result depend on the traversal order.
pub fn dot(a: &[f64], b: &[f64]) -> Result<f64, VectorError> {
    same_length(a, b)?;
    let products: Vec<f64> = a.iter().zip(b).map(|(x, y)| x * y).collect();
    Ok(deterministic_sum(&products))
}

/// The sum of a vector's elements.
#[must_use]
pub fn sum(a: &[f64]) -> f64 {
    deterministic_sum(a)
}

/// The arithmetic mean.
///
/// # Errors
///
/// Refuses an empty vector. A mean of nothing is not zero, and zero is a value somebody
/// will act on.
pub fn mean(a: &[f64]) -> Result<f64, VectorError> {
    if a.is_empty() {
        return Err(VectorError::Empty);
    }
    #[allow(clippy::cast_precision_loss)]
    Ok(deterministic_sum(a) / a.len() as f64)
}

/// The L1 norm: the sum of absolute values.
#[must_use]
pub fn norm_l1(a: &[f64]) -> f64 {
    let magnitudes: Vec<f64> = a.iter().map(|x| x.abs()).collect();
    deterministic_sum(&magnitudes)
}

/// The L2 norm: the square root of the sum of squares.
///
/// Squares are formed first and summed deterministically, so two runs agree bit for bit.
/// The square root is a single rounding on top and adds no order dependence.
#[must_use]
pub fn norm_l2(a: &[f64]) -> f64 {
    let squares: Vec<f64> = a.iter().map(|x| x * x).collect();
    deterministic_sum(&squares).sqrt()
}

/// The Euclidean distance between two vectors.
pub fn euclidean(a: &[f64], b: &[f64]) -> Result<f64, VectorError> {
    same_length(a, b)?;
    let squares: Vec<f64> = a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).collect();
    Ok(deterministic_sum(&squares).sqrt())
}

/// Cosine similarity: the cosine of the angle between two vectors.
///
/// One for identical direction, zero for orthogonal, minus one for opposite.
///
/// # Errors
///
/// Refuses a vector of zero magnitude. The angle to the origin is undefined --- not zero and
/// not one --- and returning either places that vector at a definite similarity to
/// everything, which sorts it to the top or the bottom of every ranked result.
pub fn cosine_similarity(a: &[f64], b: &[f64]) -> Result<f64, VectorError> {
    same_length(a, b)?;
    let (left, right) = (norm_l2(a), norm_l2(b));
    if left == 0.0 || right == 0.0 {
        return Err(VectorError::ZeroMagnitude);
    }
    Ok(dot(a, b)? / (left * right))
}

/// Cosine *distance*: one minus the similarity.
///
/// Offered separately because the two are constantly confused, and a ranking sorted by the
/// wrong one is reversed --- which looks like a working ranking of the least similar things.
pub fn cosine_distance(a: &[f64], b: &[f64]) -> Result<f64, VectorError> {
    Ok(1.0 - cosine_similarity(a, b)?)
}

// --- matrices -------------------------------------------------------------

/// Multiply a matrix by a vector.
///
/// The matrix is row-major and flat: `rows * columns` values, which is how a
/// `FixedSizeList` of a fixed-shape tensor stores one. Each output element is a dot product
/// and is therefore deterministic.
///
/// # Errors
///
/// Refuses a matrix whose length is not `rows * columns`, and a vector whose length is not
/// `columns`. Both would otherwise read past a row boundary and produce a number.
pub fn matvec(
    matrix: &[f64],
    rows: usize,
    columns: usize,
    vector: &[f64],
) -> Result<Vec<f64>, VectorError> {
    if matrix.len() != rows.saturating_mul(columns) {
        return Err(VectorError::LengthMismatch {
            left: matrix.len(),
            right: rows.saturating_mul(columns),
        });
    }
    if vector.len() != columns {
        return Err(VectorError::LengthMismatch {
            left: vector.len(),
            right: columns,
        });
    }

    let mut out = Vec::with_capacity(rows);
    for row in 0..rows {
        let from = row.saturating_mul(columns);
        let slice = matrix.get(from..from + columns).unwrap_or(&[]);
        out.push(dot(slice, vector)?);
    }
    Ok(out)
}

/// One vector of a flat column of fixed-size vectors.
///
/// An Arrow `FixedSizeList<Float64, N>` column stores `rows * N` values contiguously, so
/// row `i` is the slice `[i * N, (i + 1) * N)`. Returns `None` for a row out of range rather
/// than a shorter slice: a truncated vector produces a plausible number from a kernel that
/// has no way to know it was truncated.
#[must_use]
pub fn row_of(values: &[f64], width: usize, row: usize) -> Option<&[f64]> {
    if width == 0 {
        return None;
    }
    let from = row.checked_mul(width)?;
    let to = from.checked_add(width)?;
    values.get(from..to)
}
