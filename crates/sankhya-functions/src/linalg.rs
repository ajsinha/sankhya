//! Linear algebra, named on the SQL surface.
//!
//! # How a matrix arrives
//!
//! Flat and row-major, as `ADR-0021` Decision 4 stores it. Every function here takes the
//! square root of the array's length as the order, which is exact for a square matrix and
//! refused for anything else — so `mat_eigenvalues` of a 3×4 is a refusal rather than an
//! answer about the 3×3 it could have been read as.
//!
//! # What returns a matrix returns it the same way
//!
//! A Cholesky factor comes back as a flat array of the same length, so it feeds straight into
//! another matrix function without being reshaped. Nothing here has a shape of its own.

// Every kernel below indexes its arguments positionally --- `a[0]`, `a[1]` --- and the arity
// is checked in the wrapper **before** the kernel runs, so an index out of bounds cannot
// occur. The alternative is a `get` and an `unwrap_or` per argument, which would turn a
// missing argument into a silent zero: exactly the wrong answer this catalogue is arranged
// against, traded for a lint the wrapper has already satisfied.
#![allow(clippy::indexing_slicing)]
use crate::scalar::Numeric;
use crate::series::Series;
use datafusion::logical_expr::ScalarUDF;
use sankhya_math::decompose;

/// The order of a square matrix held flat, or a refusal naming what it actually is.
fn order(values: &[f64]) -> Result<usize, String> {
    let length = values.len();
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let side = (length as f64).sqrt().round() as usize;
    if side * side != length || side == 0 {
        return Err(format!(
            "this operation needs a square matrix, and {length} values do not form one. A \
             matrix is stored flat and row-major, so its length is the square of its order"
        ));
    }
    Ok(side)
}

/// Every linear-algebra function, of both shapes.
#[must_use]
pub fn functions() -> Vec<ScalarUDF> {
    vec![
        // --- decompositions, each returning a flat matrix or a vector ---
        //
        // The Cholesky factor. Its *failure* is the useful part: it succeeds exactly on the
        // positive-definite matrices, so a covariance matrix that will not factor is not a
        // numerical accident --- it is one no data could have produced.
        ScalarUDF::from(Series::new("mat_cholesky", |values| {
            let size = order(values)?;
            decompose::cholesky(values, size).map_err(|error| error.to_string())
        })),
        // Eigenvalues, descending. Fixed order, so two callers cannot disagree about which is
        // the first --- and descending because that is what a principal-component analysis
        // wants and what every text prints.
        ScalarUDF::from(Series::new("mat_eigenvalues", |values| {
            let size = order(values)?;
            decompose::eigenvalues_symmetric(values, size).map_err(|error| error.to_string())
        })),
        // The eigenvectors, as the columns of a flat matrix. Each has its dominant entry made
        // positive, because an eigenvector is determined only up to sign and leaving that to
        // the arithmetic gives the same matrix different signs on two machines.
        ScalarUDF::from(Series::new("mat_eigenvectors", |values| {
            let size = order(values)?;
            decompose::eigen_symmetric(values, size)
                .map(|(_, vectors)| vectors)
                .map_err(|error| error.to_string())
        })),
        // Singular values, descending. Through the eigenvalues of `AᵀA`, whose known cost is
        // stated rather than hidden: forming the Gram matrix squares the condition number, so
        // a singular value below about `1e-8` of the largest is not resolved.
        ScalarUDF::from(Series::new("mat_singular_values", |values| {
            let size = order(values)?;
            decompose::singular_values(values, size, size).map_err(|error| error.to_string())
        })),
        // The QR factors, each returned flat. Two functions rather than one returning a pair,
        // because a composite type crosses this wire as a string a client then has to parse ---
        // and somebody who wants only `R`, which is the common case for a least-squares fit,
        // should not pay to compute and render `Q` as well.
        ScalarUDF::from(Series::new("mat_qr_q", |values| {
            let size = order(values)?;
            decompose::qr(values, size, size)
                .map(|(q, _)| q)
                .map_err(|error| error.to_string())
        })),
        ScalarUDF::from(Series::new("mat_qr_r", |values| {
            let size = order(values)?;
            decompose::qr(values, size, size)
                .map(|(_, r)| r)
                .map_err(|error| error.to_string())
        })),
        // --- properties, each one number ---
        //
    ]
}

/// The matrix properties, which take the matrix as an array rather than as numbers.
///
/// Separate from [`functions`] only because they read an array and return a number, which is
/// neither of the two shapes above. They register together.
#[must_use]
pub fn property_functions() -> Vec<ScalarUDF> {
    vec![
        // `1` and `0` rather than a boolean, because this wire renders a boolean as `t`/`f`
        // and these are asked inside arithmetic --- `CASE WHEN mat_is_positive_definite(c) = 1`
        // reads the same in every client, and a boolean would not.
        //
        // Registered here rather than through the numeric wrapper, which takes **numbers**: it
        // was, and so refused the array it exists to be asked about. Found by the parity soak,
        // because every path refused it identically and consistent refusal is still refusal.
        ScalarUDF::from(crate::property::Property::new("mat_is_square", |values| {
            Ok(f64::from(u8::from(order(values).is_ok())))
        })),
        // Whether a matrix equals its own transpose. Within a tolerance, deliberately: a
        // covariance matrix assembled by summing products is symmetric mathematically and
        // differs in the last bit arithmetically, and answering `false` for one would answer
        // `false` for the commonest input this exists for.
        ScalarUDF::from(crate::property::Property::new("mat_is_symmetric", |values| {
            let size = order(values)?;
            Ok(f64::from(u8::from(decompose::is_symmetric(values, size))))
        })),
        // Whether a matrix is positive definite, asked by attempting the factorisation ---
        // which is the definition rather than a proxy for it. A covariance matrix that answers
        // `0` here is not a numerical accident: it is one no data could have produced, usually
        // a correlation somebody interpolated by hand.
        ScalarUDF::from(crate::property::Property::new(
            "mat_is_positive_definite",
            |values| {
                let size = order(values)?;
                Ok(f64::from(u8::from(decompose::is_positive_definite(values, size))))
            },
        )),
    ]
}
