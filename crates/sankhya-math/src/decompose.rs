//! Matrix decompositions: Cholesky, QR, the symmetric eigenproblem, and the SVD.
//!
//! # Why Jacobi rather than the faster algorithms
//!
//! The standard route to eigenvalues is a tridiagonal reduction followed by a shifted QR
//! iteration, and it is several times faster than the Jacobi rotations used here. It is also
//! **order-dependent in a way that shows**: the shift strategy chooses a pivot from the
//! current iterate, so a matrix perturbed in its last bit can take a different number of
//! iterations and converge to eigenvalues differing in their last several.
//!
//! This crate exists because a figure that changes when the machine is busier is expensive
//! (see [`crate::reduce`]), and an eigenvalue is a figure. Jacobi sweeps a fixed pattern of
//! index pairs, so the same matrix gives the same rotations in the same order on every machine
//! and every run --- and it is unconditionally accurate for symmetric matrices, including the
//! small eigenvalues that the faster methods lose.
//!
//! # What is symmetric, and what happens when it is not
//!
//! [`eigen_symmetric`] refuses a matrix that is not symmetric rather than symmetrising it. A
//! non-symmetric matrix has complex eigenvalues in general, and silently returning the
//! eigenvalues of `(A + Aᵀ)/2` answers a question about a different matrix.

use crate::matrix::MatrixError;

/// How close to zero an off-diagonal must be, **relative to the matrix**, before a sweep stops.
///
/// Relative rather than absolute, and that is the whole of it. As an absolute bound this read
/// `1e-15` against the unscaled off-diagonal norm, so any matrix whose entries were already
/// that small was declared converged before a single rotation ran --- and `eigen_symmetric`
/// returned the input's diagonal with the identity as its basis, reporting `Ok`.
///
/// For `[[1e-16, 3e-16], [3e-16, 5e-16]]` that gives eigenvalues `5e-16` and `1e-16` against a
/// true `6.6056e-16` and `-6.0555e-17`: the larger is 24% low and **the smaller has the wrong
/// sign**, so an indefinite matrix passes an "all eigenvalues are non-negative" test.
/// `singular_values` builds `AᵀA` and squares the scale, which makes the early exit far easier
/// to reach and leaves a small singular value 2.4× too large --- a condition number wrong in
/// the reassuring direction.
///
/// A relative bound is also scale-free, which is what the constant was always meant to be.
const CONVERGED: f64 = 1e-15;

/// How many sweeps before Jacobi gives up.
///
/// Jacobi converges quadratically and a dozen sweeps is ample for any size that fits in
/// memory; fifty is a backstop against a matrix carrying values that will not settle, so the
/// failure is a refusal rather than a hang.
const SWEEPS: usize = 50;

/// The element at `(row, column)` of a row-major matrix.
fn at(values: &[f64], columns: usize, row: usize, column: usize) -> f64 {
    values.get(row * columns + column).copied().unwrap_or(0.0)
}

/// Set the element at `(row, column)`.
fn put(values: &mut [f64], columns: usize, row: usize, column: usize, value: f64) {
    if let Some(slot) = values.get_mut(row * columns + column) {
        *slot = value;
    }
}

/// Whether a square matrix equals its own transpose, within a tolerance.
///
/// A tolerance rather than exact equality, because a covariance matrix assembled by summing
/// products is symmetric mathematically and differs in the last bit arithmetically --- and
/// refusing it would refuse the most common input this is for.
#[must_use]
pub fn is_symmetric(values: &[f64], size: usize) -> bool {
    if values.len() != size * size {
        return false;
    }
    // A `NaN` is not symmetric with anything, including itself.
    //
    // Every comparison below is NaN-blind in the same direction: `f64::max` returns the
    // non-NaN operand so a `NaN` never affects the scale, and `NaN > x` is false so the
    // difference test passes. A covariance matrix with one missing price was therefore judged
    // symmetric, factored by `cholesky`, and pronounced positive definite --- and this
    // module's own header makes the failure of that factorisation the *definition* of a usable
    // covariance matrix. `mat_is_positive_definite(cov) = 1` was a guard that passed on
    // exactly the input it exists to catch, and the correlated draws seeded from that factor
    // are all `NaN`.
    if values.iter().any(|v| !v.is_finite()) {
        return false;
    }
    let scale = values.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1.0);
    for row in 0..size {
        for column in (row + 1)..size {
            let upper = at(values, size, row, column);
            let lower = at(values, size, column, row);
            if (upper - lower).abs() > 1e-12 * scale {
                return false;
            }
        }
    }
    true
}

/// The Cholesky factor `L` with `L·Lᵀ = A`, returned lower-triangular and row-major.
///
/// # Why a failure here is information rather than an inconvenience
///
/// Cholesky succeeds exactly when the matrix is positive definite, so the failure **is** the
/// test: a covariance matrix that will not factor is not a numerical accident, it is a
/// covariance matrix that no data could have produced --- usually an interpolated correlation
/// somebody assembled by hand. Reporting which pivot went non-positive names where.
///
/// # Errors
///
/// [`MatrixError::NotSymmetric`] when it is not symmetric, [`MatrixError::ShapeMismatch`] for
/// the wrong length, and
/// [`MatrixError::Singular`] when a pivot is not positive.
pub fn cholesky(values: &[f64], size: usize) -> Result<Vec<f64>, MatrixError> {
    if values.len() != size * size {
        return Err(MatrixError::ShapeMismatch { values: values.len(), expected: size * size });
    }
    if size == 0 {
        return Err(MatrixError::NotSquare { rows: 0, columns: 0 });
    }
    if !is_symmetric(values, size) {
        return Err(MatrixError::NotSymmetric { size });
    }

    let mut lower = vec![0.0f64; size * size];
    for row in 0..size {
        for column in 0..=row {
            let mut sum = at(values, size, row, column);
            for k in 0..column {
                sum -= at(&lower, size, row, k) * at(&lower, size, column, k);
            }
            if row == column {
                // `!(sum > 0.0)` rather than `sum <= 0.0`, so a `NaN` pivot is refused.
                // `NaN <= 0.0` is false, so the old test let `sqrt(NaN)` be stored and the
                // factorisation reported success on a matrix it had not factored.
                if !(sum > 0.0) {
                    return Err(MatrixError::Singular);
                }
                put(&mut lower, size, row, column, sum.sqrt());
            } else {
                let pivot = at(&lower, size, column, column);
                if !pivot.is_finite() || pivot == 0.0 {
                    return Err(MatrixError::Singular);
                }
                put(&mut lower, size, row, column, sum / pivot);
            }
        }
    }
    Ok(lower)
}

/// Whether a symmetric matrix is positive definite.
///
/// Asked by attempting the factorisation, because that is the definition rather than a proxy
/// for it. An eigenvalue test would need the eigenvalues, which cost more and answer less.
#[must_use]
pub fn is_positive_definite(values: &[f64], size: usize) -> bool {
    cholesky(values, size).is_ok()
}

/// The QR decomposition by Householder reflections, returning `(Q, R)` row-major.
///
/// `Q` is `rows × rows` and orthogonal; `R` is `rows × columns` and upper-triangular.
///
/// Householder rather than Gram-Schmidt: the classical Gram-Schmidt loses orthogonality
/// catastrophically on an ill-conditioned matrix, and the modified form loses it slowly. A
/// reflection is orthogonal to machine precision by construction, whatever it is applied to.
///
/// # Errors
///
/// [`MatrixError::ShapeMismatch`] when the values do not fill the shape.
pub fn qr(values: &[f64], rows: usize, columns: usize) -> Result<(Vec<f64>, Vec<f64>), MatrixError> {
    if values.len() != rows * columns || rows == 0 || columns == 0 {
        return Err(MatrixError::ShapeMismatch { values: values.len(), expected: rows * columns });
    }

    let mut r = values.to_vec();
    let mut q = vec![0.0f64; rows * rows];
    for i in 0..rows {
        put(&mut q, rows, i, i, 1.0);
    }

    for column in 0..columns.min(rows.saturating_sub(1)) {
        // The reflection that zeroes everything below the diagonal in this column.
        let mut norm = 0.0f64;
        for row in column..rows {
            let value = at(&r, columns, row, column);
            norm += value * value;
        }
        norm = norm.sqrt();
        if norm == 0.0 {
            continue;
        }
        let head = at(&r, columns, column, column);
        // The sign is chosen away from the head, so the subtraction that forms the vector is
        // never between two nearly equal numbers --- the one place this algorithm can lose
        // its accuracy, and the reason the choice is not arbitrary.
        let alpha = if head >= 0.0 { -norm } else { norm };

        let mut v = vec![0.0f64; rows];
        put(&mut v, 1, column, 0, head - alpha);
        for row in (column + 1)..rows {
            put(&mut v, 1, row, 0, at(&r, columns, row, column));
        }
        let v_norm: f64 = v.iter().map(|x| x * x).sum();
        if v_norm == 0.0 {
            continue;
        }

        // Apply to R, then accumulate into Q.
        for j in 0..columns {
            let mut dot = 0.0;
            for i in column..rows {
                dot += at(&v, 1, i, 0) * at(&r, columns, i, j);
            }
            let factor = 2.0 * dot / v_norm;
            for i in column..rows {
                let updated = at(&r, columns, i, j) - factor * at(&v, 1, i, 0);
                put(&mut r, columns, i, j, updated);
            }
        }
        for j in 0..rows {
            let mut dot = 0.0;
            for i in column..rows {
                dot += at(&v, 1, i, 0) * at(&q, rows, j, i);
            }
            let factor = 2.0 * dot / v_norm;
            for i in column..rows {
                let updated = at(&q, rows, j, i) - factor * at(&v, 1, i, 0);
                put(&mut q, rows, j, i, updated);
            }
        }
    }

    // The sub-diagonal is zero by construction; setting it removes the rounding dust that
    // would otherwise make `R` look not-quite-triangular to anything that checked.
    for row in 1..rows {
        for column in 0..row.min(columns) {
            put(&mut r, columns, row, column, 0.0);
        }
    }
    Ok((q, r))
}

/// The eigenvalues and eigenvectors of a symmetric matrix, by cyclic Jacobi rotations.
///
/// Returns the eigenvalues **descending** and the eigenvectors as the columns of the returned
/// matrix, row-major. Descending because that is the order a principal-component analysis
/// wants and the order every text prints; fixed, so two callers cannot disagree about which
/// eigenvector is the first.
///
/// # Errors
///
/// [`MatrixError::NotSymmetric`] for a matrix that is not symmetric --- refused rather than
/// symmetrised, because the eigenvalues of `(A + Aᵀ)/2` answer a question about a different
/// matrix. [`MatrixError::Singular`] if the sweeps do not converge.
pub fn eigen_symmetric(
    values: &[f64],
    size: usize,
) -> Result<(Vec<f64>, Vec<f64>), MatrixError> {
    if values.len() != size * size {
        return Err(MatrixError::ShapeMismatch { values: values.len(), expected: size * size });
    }
    if size == 0 {
        return Err(MatrixError::NotSquare { rows: 0, columns: 0 });
    }
    if !is_symmetric(values, size) {
        return Err(MatrixError::NotSymmetric { size });
    }

    let mut a = values.to_vec();
    let mut vectors = vec![0.0f64; size * size];
    for i in 0..size {
        put(&mut vectors, size, i, i, 1.0);
    }

    // The scale the convergence bound is relative to, taken once from the input rather than
    // from the iterate: a threshold that moved as the matrix was rotated would be a different
    // question asked at every sweep.
    //
    // Deliberately **not** floored at one. A floor of one is what an absolute bound is, for
    // every matrix smaller than the floor --- which is the entire defect --- and the first
    // attempt at this fix carried one, so it changed nothing for exactly the inputs it was
    // written for. The zero matrix it was meant to protect is handled by comparing with `<=`
    // below: zero off-diagonal against a zero bound is converged, and correctly so.
    let scale = a.iter().fold(0.0f64, |m, v| m + v * v).sqrt();
    let mut converged = false;
    for _ in 0..SWEEPS {
        // The magnitude still off the diagonal. A sweep stops when there is nothing left to
        // rotate away, which is a statement about the matrix rather than an iteration count.
        let mut off = 0.0f64;
        for p in 0..size {
            for q in (p + 1)..size {
                let value = at(&a, size, p, q);
                off += value * value;
            }
        }
        if off.sqrt() <= CONVERGED * scale {
            converged = true;
            break;
        }

        // A fixed cyclic order, which is what makes this reproducible: no pivot is chosen
        // from the current iterate, so nothing depends on rounding.
        for p in 0..size {
            for q in (p + 1)..size {
                let apq = at(&a, size, p, q);
                if apq.abs() < f64::MIN_POSITIVE {
                    continue;
                }
                let app = at(&a, size, p, p);
                let aqq = at(&a, size, q, q);
                let theta = (aqq - app) / (2.0 * apq);
                // The smaller root, taken in the form that avoids cancellation for a large
                // `theta` --- the standard trick, and the reason this is written out.
                let t = if theta >= 0.0 {
                    1.0 / (theta + (1.0 + theta * theta).sqrt())
                } else {
                    -1.0 / (-theta + (1.0 + theta * theta).sqrt())
                };
                let c = 1.0 / (1.0 + t * t).sqrt();
                let s = t * c;

                for k in 0..size {
                    let akp = at(&a, size, k, p);
                    let akq = at(&a, size, k, q);
                    put(&mut a, size, k, p, c * akp - s * akq);
                    put(&mut a, size, k, q, s * akp + c * akq);
                }
                for k in 0..size {
                    let apk = at(&a, size, p, k);
                    let aqk = at(&a, size, q, k);
                    put(&mut a, size, p, k, c * apk - s * aqk);
                    put(&mut a, size, q, k, s * apk + c * aqk);
                }
                for k in 0..size {
                    let vkp = at(&vectors, size, k, p);
                    let vkq = at(&vectors, size, k, q);
                    put(&mut vectors, size, k, p, c * vkp - s * vkq);
                    put(&mut vectors, size, k, q, s * vkp + c * vkq);
                }
            }
        }
    }
    if !converged {
        return Err(MatrixError::Singular);
    }

    // Sorted descending, with the eigenvectors carried along. A sort by value alone would
    // leave the vectors beside the wrong eigenvalues, which is a wrong answer that still
    // looks like a decomposition.
    let mut order: Vec<usize> = (0..size).collect();
    order.sort_by(|&i, &j| {
        at(&a, size, j, j)
            .partial_cmp(&at(&a, size, i, i))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let eigenvalues: Vec<f64> = order.iter().map(|&i| at(&a, size, i, i)).collect();
    let mut sorted = vec![0.0f64; size * size];
    for (column, &source) in order.iter().enumerate() {
        // Each eigenvector's sign is fixed by making its largest-magnitude entry positive.
        // An eigenvector is only determined up to sign, and leaving the sign to the rotations
        // means the same matrix gives the same numbers with different signs on two machines.
        let mut largest = 0.0f64;
        let mut sign = 1.0f64;
        for row in 0..size {
            let value = at(&vectors, size, row, source);
            if value.abs() > largest {
                largest = value.abs();
                sign = if value < 0.0 { -1.0 } else { 1.0 };
            }
        }
        for row in 0..size {
            put(&mut sorted, size, row, column, sign * at(&vectors, size, row, source));
        }
    }
    Ok((eigenvalues, sorted))
}

/// The eigenvalues of a symmetric matrix, descending.
///
/// # Errors
///
/// [`MatrixError`] as [`eigen_symmetric`].
pub fn eigenvalues_symmetric(values: &[f64], size: usize) -> Result<Vec<f64>, MatrixError> {
    Ok(eigen_symmetric(values, size)?.0)
}

/// The singular values of a matrix, descending.
///
/// Obtained as the square roots of the eigenvalues of `AᵀA`, which is exact for the values
/// this system's matrices carry and is the route that reuses the reproducible eigensolver.
/// The known cost is stated rather than hidden: forming `AᵀA` squares the condition number,
/// so a singular value below about `1e-8` of the largest is not resolved. [`rank`] and
/// [`condition_number`] are written in terms of this and inherit the same limit.
///
/// # Errors
///
/// [`MatrixError`] for the wrong shape or a failure to converge.
pub fn singular_values(
    values: &[f64],
    rows: usize,
    columns: usize,
) -> Result<Vec<f64>, MatrixError> {
    if values.len() != rows * columns || rows == 0 || columns == 0 {
        return Err(MatrixError::ShapeMismatch { values: values.len(), expected: rows * columns });
    }
    // `AᵀA`, which is symmetric by construction.
    let mut gram = vec![0.0f64; columns * columns];
    for i in 0..columns {
        for j in 0..columns {
            let mut sum = 0.0;
            for k in 0..rows {
                sum += at(values, columns, k, i) * at(values, columns, k, j);
            }
            put(&mut gram, columns, i, j, sum);
        }
    }
    let eigen = eigenvalues_symmetric(&gram, columns)?;
    // A tiny negative eigenvalue is rounding rather than a complex singular value, and its
    // square root is zero rather than a NaN.
    Ok(eigen.into_iter().map(|value| value.max(0.0).sqrt()).collect())
}
