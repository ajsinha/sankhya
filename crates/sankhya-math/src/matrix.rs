//! Linear algebra over flat row-major slices.
//!
//! # The representation
//!
//! A matrix is `rows * columns` values in row-major order — element `(i, j)` at
//! `i * columns + j`. That is what Arrow's `arrow.fixed_shape_tensor` extension stores, and
//! it is what a `FixedSizeList<Float64, rows * columns>` column holds contiguously. So a
//! column of a million matrices is one flat buffer and each row is a slice of it.
//!
//! # Determinism, again
//!
//! Matrix multiplication is a grid of dot products, so it inherits
//! [`crate::vector::dot`]'s order-fixed summation and is bit-identical run to run.
//!
//! Elimination is different and worth being explicit about. LU decomposition's arithmetic
//! is a fixed sequence once the pivots are chosen, so the only freedom is *which* pivot ---
//! and this picks the largest magnitude with the **lowest row index** breaking ties. Without
//! that tie-break, two rows of equal magnitude could be chosen differently by two builds
//! and the factorisation would differ in the last bits. Choosing by magnitude alone is the
//! usual formulation and is not quite deterministic.
//!
//! # What is here and what is not
//!
//! Multiplication, transpose, trace, identity; and determinant, inverse and solve via LU
//! with partial pivoting.
//!
//! QR, SVD and eigendecomposition are in [`crate::decompose`], not here --- and this comment
//! said they did not exist at all until `FEA-05` was raised against it. They are a different
//! discipline and they live in a different module for that reason; what was wrong was the
//! word "not".
//!
//! The split matters for a second reason. Everything in *this* module that reduces goes
//! through [`crate::deterministic_sum`]; nothing in `decompose` does. Both are reproducible
//! run to run, and only these are compensated.

use crate::vector::{dot, VectorError};
use std::fmt;

/// Why a matrix operation could not be performed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MatrixError {
    /// The value count does not match the declared shape.
    ///
    /// Refused rather than reshaped. A matrix read at the wrong shape produces numbers from
    /// values that were never in the same row, and every one of them looks ordinary.
    ShapeMismatch {
        /// How many values there are.
        values: usize,
        /// How many the shape requires.
        expected: usize,
    },
    /// Two matrices whose inner dimensions do not agree.
    NotConformable {
        /// The left operand's columns.
        left_columns: usize,
        /// The right operand's rows.
        right_rows: usize,
    },
    /// An operation requiring a square matrix was given a rectangular one.
    NotSquare {
        /// Its rows.
        rows: usize,
        /// Its columns.
        columns: usize,
    },
    /// The matrix is square and is not symmetric.
    ///
    /// # Why this is not [`Self::NotSquare`]
    ///
    /// It was, and the message it produced was *"this operation needs a square matrix and was
    /// given 8 by 8"* --- which is a refusal telling somebody their 8x8 matrix is not square.
    /// Found by the column soak, where a Cholesky of a stored column refused with it.
    ///
    /// A message that contradicts itself is worse than a vague one: it sends the reader to
    /// check the shape, which is correct, and they find nothing wrong with it.
    NotSymmetric {
        /// The order of the matrix that was given.
        size: usize,
    },
    /// The matrix has no inverse.
    ///
    /// Reported rather than approximated. A near-singular matrix inverted anyway produces
    /// enormous values that are arithmetic noise, and they propagate into everything
    /// downstream looking like results.
    Singular,
    /// A dimension of zero.
    Degenerate,
}

impl fmt::Display for MatrixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShapeMismatch { values, expected } => write!(
                f,
                "a matrix of {values} values cannot have a shape requiring {expected}. \
                 Refusing rather than reshaping: a matrix read at the wrong shape produces \
                 numbers from values that were never in the same row"
            ),
            Self::NotConformable {
                left_columns,
                right_rows,
            } => write!(
                f,
                "cannot multiply a matrix of {left_columns} columns by one of {right_rows} \
                 rows: the inner dimensions must agree"
            ),
            Self::NotSquare { rows, columns } => write!(
                f,
                "this operation needs a square matrix and was given {rows} by {columns}"
            ),
            Self::NotSymmetric { size } => write!(
                f,
                "this operation is defined on a symmetric matrix, and this {size} by {size} \
                 one is not --- it does not equal its own transpose. Refused rather than \
                 symmetrised: the eigenvalues of `(A + At)/2` answer a question about a \
                 different matrix"
            ),
            Self::Singular => f.write_str(
                "the matrix is singular and has no inverse. Refusing rather than \
                 approximating: an almost-singular matrix inverted anyway produces enormous \
                 values that are arithmetic noise, and they propagate downstream looking \
                 like results",
            ),
            Self::Degenerate => f.write_str("a matrix cannot have a dimension of zero"),
        }
    }
}

impl std::error::Error for MatrixError {}

impl From<VectorError> for MatrixError {
    fn from(error: VectorError) -> Self {
        match error {
            VectorError::LengthMismatch { left, right } => Self::NotConformable {
                left_columns: left,
                right_rows: right,
            },
            VectorError::Empty | VectorError::ZeroMagnitude | VectorError::Refused(_) => {
                Self::Degenerate
            }
        }
    }
}

/// Check a flat slice matches a declared shape.
fn check(values: &[f64], rows: usize, columns: usize) -> Result<(), MatrixError> {
    if rows == 0 || columns == 0 {
        return Err(MatrixError::Degenerate);
    }
    let expected = rows.saturating_mul(columns);
    if values.len() == expected {
        Ok(())
    } else {
        Err(MatrixError::ShapeMismatch {
            values: values.len(),
            expected,
        })
    }
}

/// One row of a row-major matrix.
fn row(values: &[f64], columns: usize, index: usize) -> &[f64] {
    let from = index.saturating_mul(columns);
    values.get(from..from + columns).unwrap_or(&[])
}

/// Multiply two matrices.
///
/// Each output element is a dot product, so the result is bit-identical run to run. The
/// right operand is transposed first so that both operands of every dot product are
/// contiguous --- the transpose costs one pass and the alternative is a strided gather per
/// element, which is far worse on anything above a handful of columns.
pub fn multiply(
    left: &[f64],
    left_rows: usize,
    left_columns: usize,
    right: &[f64],
    right_rows: usize,
    right_columns: usize,
) -> Result<Vec<f64>, MatrixError> {
    check(left, left_rows, left_columns)?;
    check(right, right_rows, right_columns)?;
    if left_columns != right_rows {
        return Err(MatrixError::NotConformable {
            left_columns,
            right_rows,
        });
    }

    let transposed = transpose(right, right_rows, right_columns)?;
    let mut out = Vec::with_capacity(left_rows.saturating_mul(right_columns));
    for i in 0..left_rows {
        let a = row(left, left_columns, i);
        for j in 0..right_columns {
            let b = row(&transposed, right_rows, j);
            out.push(dot(a, b)?);
        }
    }
    Ok(out)
}

/// Transpose a matrix.
pub fn transpose(values: &[f64], rows: usize, columns: usize) -> Result<Vec<f64>, MatrixError> {
    check(values, rows, columns)?;
    let mut out = vec![0.0; values.len()];
    for i in 0..rows {
        for j in 0..columns {
            if let (Some(source), Some(slot)) =
                (values.get(i * columns + j), out.get_mut(j * rows + i))
            {
                *slot = *source;
            }
        }
    }
    Ok(out)
}

/// The sum of the diagonal.
pub fn trace(values: &[f64], rows: usize, columns: usize) -> Result<f64, MatrixError> {
    check(values, rows, columns)?;
    if rows != columns {
        return Err(MatrixError::NotSquare { rows, columns });
    }
    let diagonal: Vec<f64> = (0..rows)
        .filter_map(|i| values.get(i * columns + i).copied())
        .collect();
    Ok(crate::deterministic_sum(&diagonal))
}

/// The identity matrix of a given size.
#[must_use]
pub fn identity(size: usize) -> Vec<f64> {
    let mut out = vec![0.0; size.saturating_mul(size)];
    for i in 0..size {
        if let Some(slot) = out.get_mut(i * size + i) {
            *slot = 1.0;
        }
    }
    out
}

/// An LU decomposition with partial pivoting.
///
/// Returns the combined factors in place, the permutation, and the sign of that permutation.
/// The pivot is the largest magnitude in the column, **with the lowest row index breaking
/// ties** --- without that, two rows of equal magnitude could be chosen differently by two
/// builds, and the factorisation would differ in its last bits.
fn lu(values: &[f64], size: usize) -> Result<(Vec<f64>, Vec<usize>, f64), MatrixError> {
    let mut a = values.to_vec();
    let mut permutation: Vec<usize> = (0..size).collect();
    let mut sign = 1.0f64;

    for column in 0..size {
        // Choose the pivot: largest magnitude, lowest row index on a tie. `>` rather than
        // `>=` is what makes the tie-break the lowest index, and it is the whole of the
        // determinism guarantee here.
        let mut pivot = column;
        let mut best = a.get(column * size + column).map_or(0.0, |v| v.abs());
        for candidate in column + 1..size {
            let magnitude = a.get(candidate * size + column).map_or(0.0, |v| v.abs());
            if magnitude > best {
                best = magnitude;
                pivot = candidate;
            }
        }

        // A pivot of zero means the column is linearly dependent on those before it.
        // Reporting it is the point: continuing divides by zero and fills the rest of the
        // matrix with infinities that look like very large numbers.
        if best == 0.0 {
            return Err(MatrixError::Singular);
        }

        if pivot != column {
            for j in 0..size {
                a.swap(column * size + j, pivot * size + j);
            }
            permutation.swap(column, pivot);
            sign = -sign;
        }

        let diagonal = a.get(column * size + column).copied().unwrap_or(0.0);
        for i in column + 1..size {
            let factor = a.get(i * size + column).copied().unwrap_or(0.0) / diagonal;
            if let Some(slot) = a.get_mut(i * size + column) {
                *slot = factor;
            }
            for j in column + 1..size {
                let subtrahend = factor * a.get(column * size + j).copied().unwrap_or(0.0);
                if let Some(slot) = a.get_mut(i * size + j) {
                    *slot -= subtrahend;
                }
            }
        }
    }
    Ok((a, permutation, sign))
}

/// The determinant.
///
/// By LU rather than by cofactor expansion: cofactors are `O(n!)` and are exact only for
/// tiny matrices, where they are also unnecessary.
pub fn determinant(values: &[f64], rows: usize, columns: usize) -> Result<f64, MatrixError> {
    check(values, rows, columns)?;
    if rows != columns {
        return Err(MatrixError::NotSquare { rows, columns });
    }
    match lu(values, rows) {
        // A singular matrix has determinant zero, exactly. This is the one place the
        // factorisation's refusal is an answer rather than a failure.
        Err(MatrixError::Singular) => Ok(0.0),
        Err(other) => Err(other),
        Ok((factors, _, sign)) => {
            let mut product = sign;
            for i in 0..rows {
                product *= factors.get(i * rows + i).copied().unwrap_or(0.0);
            }
            Ok(product)
        }
    }
}

/// Solve `A x = b` for `x`.
///
/// By forward and back substitution on the LU factors, which is what an inverse would do
/// anyway --- and computing an inverse to solve one system does more arithmetic and loses
/// more precision than solving it directly.
pub fn solve(matrix: &[f64], size: usize, rhs: &[f64]) -> Result<Vec<f64>, MatrixError> {
    check(matrix, size, size)?;
    if rhs.len() != size {
        return Err(MatrixError::ShapeMismatch {
            values: rhs.len(),
            expected: size,
        });
    }
    let (factors, permutation, _) = lu(matrix, size)?;

    // Forward substitution through the unit-lower factor.
    let mut y = vec![0.0; size];
    for i in 0..size {
        let mut total = rhs
            .get(permutation.get(i).copied().unwrap_or(0))
            .copied()
            .unwrap_or(0.0);
        for j in 0..i {
            total -= factors.get(i * size + j).copied().unwrap_or(0.0)
                * y.get(j).copied().unwrap_or(0.0);
        }
        if let Some(slot) = y.get_mut(i) {
            *slot = total;
        }
    }

    // Back substitution through the upper factor.
    let mut x = vec![0.0; size];
    for i in (0..size).rev() {
        let mut total = y.get(i).copied().unwrap_or(0.0);
        for j in i + 1..size {
            total -= factors.get(i * size + j).copied().unwrap_or(0.0)
                * x.get(j).copied().unwrap_or(0.0);
        }
        let diagonal = factors.get(i * size + i).copied().unwrap_or(0.0);
        if diagonal == 0.0 {
            return Err(MatrixError::Singular);
        }
        if let Some(slot) = x.get_mut(i) {
            *slot = total / diagonal;
        }
    }
    Ok(x)
}

/// The inverse.
///
/// Solved column by column against the identity. Offered because it is asked for, with the
/// note that solving a system directly is both faster and more accurate than inverting and
/// multiplying --- an inverse is rarely the thing actually wanted.
pub fn inverse(values: &[f64], rows: usize, columns: usize) -> Result<Vec<f64>, MatrixError> {
    check(values, rows, columns)?;
    if rows != columns {
        return Err(MatrixError::NotSquare { rows, columns });
    }
    let mut out = vec![0.0; values.len()];
    for column in 0..rows {
        let mut unit = vec![0.0; rows];
        if let Some(slot) = unit.get_mut(column) {
            *slot = 1.0;
        }
        let solved = solve(values, rows, &unit)?;
        for i in 0..rows {
            if let Some(slot) = out.get_mut(i * rows + column) {
                *slot = solved.get(i).copied().unwrap_or(0.0);
            }
        }
    }
    Ok(out)
}
