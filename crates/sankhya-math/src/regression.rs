//! Least-squares regression, reporting what a decision needs rather than only a slope.
//!
//! # Why the diagnostics are not optional
//!
//! A slope with no standard error is a number nobody can act on. `sankhya-math` already had
//! `linear_fit`, which returns a slope, an intercept and an `R²` --- and every one of those
//! can look entirely reasonable for a relationship that is not there. The standard error and
//! the *t*-statistic are what say whether the slope is distinguishable from zero, and they
//! cost one more pass over residuals that have already been formed.
//!
//! # Why the normal equations are solved by QR
//!
//! Forming `XᵀX` and inverting it squares the condition number: a design matrix with a
//! condition number of `1e8` --- entirely ordinary when one predictor is a level and another
//! is its square --- becomes `1e16`, which a double cannot resolve at all. The coefficients
//! come back looking plausible and are noise.
//!
//! QR on the design matrix directly costs about twice the arithmetic and keeps the condition
//! number as it was. That is the whole reason [`crate::decompose::qr`] exists.

use crate::decompose::qr;
use crate::distribution::{f_sf, t_two_sided};
use crate::inference::InferenceError;
use crate::reduce::deterministic_sum;

/// One element of a flat slice, or zero.
///
/// # Why zero rather than a panic
///
/// The workspace denies indexing because a server must not panic on data it did not choose,
/// and that rule reaches here: every slice below comes from a caller. Each index is inside its
/// bounds by a check made at the top of the function --- the shapes are validated before any
/// arithmetic starts --- so this returns zero for an index that cannot occur rather than
/// carrying a `Result` through the linear algebra to say so.
fn at(values: &[f64], index: usize) -> f64 {
    values.get(index).copied().unwrap_or(0.0)
}

/// Set one element, ignoring an index that cannot occur.
fn put(values: &mut [f64], index: usize, value: f64) {
    if let Some(slot) = values.get_mut(index) {
        *slot = value;
    }
}

/// A fitted linear model.
#[derive(Clone, PartialEq, Debug)]
pub struct Fit {
    /// The coefficients, the first being the intercept when one was fitted.
    pub coefficients: Vec<f64>,
    /// Each coefficient's standard error.
    pub standard_errors: Vec<f64>,
    /// Each coefficient's *t*-statistic against zero.
    pub t_statistics: Vec<f64>,
    /// Each coefficient's two-sided p-value.
    pub p_values: Vec<f64>,
    /// The residuals, in the order the observations arrived.
    pub residuals: Vec<f64>,
    /// The fraction of variance explained.
    pub r_squared: f64,
    /// `R²` penalised for the number of predictors.
    ///
    /// Reported beside `R²` because `R²` never falls when a predictor is added --- including a
    /// predictor of pure noise --- so comparing two models by `R²` alone always prefers the
    /// larger one.
    pub adjusted_r_squared: f64,
    /// The residual standard error.
    pub residual_error: f64,
    /// The *F* statistic for the model against an intercept alone.
    pub f_statistic: f64,
    /// That statistic's p-value.
    pub f_p_value: f64,
    /// Residual degrees of freedom.
    pub freedom: f64,
}

/// Fit `y` on the columns of `design`, which is `rows × predictors` and row-major.
///
/// An intercept is **not** added: a caller who wants one supplies a column of ones, so the
/// model that is fitted is the model that was written down. Adding one silently would make
/// `regress` and `regress with an intercept column` the same call with different answers.
///
/// # Errors
///
/// [`InferenceError`] when there are fewer observations than predictors --- a model with more
/// parameters than data fits perfectly and predicts nothing, and returning its coefficients
/// would present that as a result.
pub fn least_squares(
    design: &[f64],
    rows: usize,
    predictors: usize,
    y: &[f64],
) -> Result<Fit, InferenceError> {
    if y.len() != rows {
        return Err(InferenceError::Unpaired { left: rows, right: y.len() });
    }
    if rows <= predictors {
        return Err(InferenceError::TooFew { given: rows, needs: predictors + 1 });
    }
    if design.len() != rows * predictors {
        return Err(InferenceError::Unpaired { left: design.len(), right: rows * predictors });
    }

    // `A = QR`, so `Rβ = Qᵀy`. The condition number stays as the design matrix's rather than
    // being squared, which is the whole reason this is not the normal equations.
    let (q, r) = qr(design, rows, predictors).map_err(|e| InferenceError::Domain(e.to_string()))?;

    let mut qty = vec![0.0f64; predictors];
    for j in 0..predictors {
        let terms: Vec<f64> = (0..rows).map(|i| at(&q, i * rows + j) * at(y, i)).collect();
        put(&mut qty, j, deterministic_sum(&terms));
    }

    // Back-substitution through the upper triangle.
    let mut beta = vec![0.0f64; predictors];
    for j in (0..predictors).rev() {
        let mut sum = at(&qty, j);
        for k in (j + 1)..predictors {
            sum -= at(&r, j * predictors + k) * at(&beta, k);
        }
        let pivot = at(&r, j * predictors + j);
        if pivot.abs() < 1e-300 {
            return Err(InferenceError::NoVariation { which: "design" });
        }
        put(&mut beta, j, sum / pivot);
    }

    // Residuals, and the sums the diagnostics need.
    let mut residuals = Vec::with_capacity(rows);
    for i in 0..rows {
        let terms: Vec<f64> =
            (0..predictors).map(|j| at(design, i * predictors + j) * at(&beta, j)).collect();
        residuals.push(at(y, i) - deterministic_sum(&terms));
    }
    let residual_squares: Vec<f64> = residuals.iter().map(|e| e * e).collect();
    let rss = deterministic_sum(&residual_squares);

    #[allow(clippy::cast_precision_loss)]
    let n = rows as f64;
    #[allow(clippy::cast_precision_loss)]
    let p = predictors as f64;
    let mean_y = deterministic_sum(y) / n;
    let total_squares: Vec<f64> = y.iter().map(|v| (v - mean_y) * (v - mean_y)).collect();
    let tss = deterministic_sum(&total_squares);

    let freedom = n - p;
    let variance = rss / freedom;
    let residual_error = variance.sqrt();

    // `(XᵀX)⁻¹` from `R` alone: `XᵀX = RᵀR`, so its inverse is `R⁻¹R⁻ᵀ` and only the diagonal
    // is needed. Formed from `R` rather than from `X` for the same conditioning reason.
    let mut inverse_diagonal = vec![0.0f64; predictors];
    for j in 0..predictors {
        // Solve `R z = e_j` for the `j`-th column of `R⁻¹`, then take its squared norm.
        let mut z = vec![0.0f64; predictors];
        put(&mut z, j, 1.0);
        for k in (0..=j).rev() {
            let mut sum = at(&z, k);
            for m in (k + 1)..=j {
                sum -= at(&r, k * predictors + m) * at(&z, m);
            }
            let pivot = at(&r, k * predictors + k);
            if pivot.abs() < 1e-300 {
                return Err(InferenceError::NoVariation { which: "design" });
            }
            put(&mut z, k, sum / pivot);
        }
        let squares: Vec<f64> = z.iter().map(|v| v * v).collect();
        put(&mut inverse_diagonal, j, deterministic_sum(&squares));
    }

    let mut standard_errors = Vec::with_capacity(predictors);
    let mut t_statistics = Vec::with_capacity(predictors);
    let mut p_values = Vec::with_capacity(predictors);
    for j in 0..predictors {
        let error = (variance * at(&inverse_diagonal, j)).sqrt();
        standard_errors.push(error);
        let t = if error > 0.0 { at(&beta, j) / error } else { f64::INFINITY };
        t_statistics.push(t);
        p_values.push(if t.is_finite() { t_two_sided(t, freedom)? } else { 0.0 });
    }

    let r_squared = if tss > 0.0 { 1.0 - rss / tss } else { 0.0 };
    // Penalised for the predictor count. `R²` never falls when a predictor is added, so
    // comparing models by it always prefers the larger one --- including when the addition is
    // noise.
    let adjusted_r_squared = if freedom > 0.0 && tss > 0.0 {
        1.0 - (rss / freedom) / (tss / (n - 1.0))
    } else {
        0.0
    };

    // The model against an intercept alone.
    let model_freedom = (p - 1.0).max(1.0);
    let f_statistic = if rss > 0.0 {
        ((tss - rss) / model_freedom) / variance
    } else {
        f64::INFINITY
    };
    let f_p_value = if f_statistic.is_finite() {
        f_sf(f_statistic, model_freedom, freedom)?
    } else {
        0.0
    };

    Ok(Fit {
        coefficients: beta,
        standard_errors,
        t_statistics,
        p_values,
        residuals,
        r_squared,
        adjusted_r_squared,
        residual_error,
        f_statistic,
        f_p_value,
        freedom,
    })
}

/// Fit `y` on one predictor with an intercept, the common case.
///
/// # Errors
///
/// [`InferenceError`] as [`least_squares`].
pub fn simple(x: &[f64], y: &[f64]) -> Result<Fit, InferenceError> {
    if x.len() != y.len() {
        return Err(InferenceError::Unpaired { left: x.len(), right: y.len() });
    }
    let mut design = Vec::with_capacity(x.len() * 2);
    for value in x {
        design.push(1.0);
        design.push(*value);
    }
    least_squares(&design, x.len(), 2, y)
}

/// Ridge regression: least squares with a penalty on the coefficients' size.
///
/// # What the penalty buys and what it costs
///
/// A design matrix whose predictors are nearly collinear has coefficients that are enormous
/// and opposite, and they move wildly for a small change in the data. The penalty shrinks
/// them, which trades a little bias for a large reduction in variance.
///
/// It also makes the coefficients **not** interpretable as marginal effects, and the standard
/// errors from an unpenalised fit no longer apply --- so this returns the coefficients and the
/// residuals, and deliberately not the *t*-statistics that would invite a significance test the
/// penalty has invalidated.
///
/// # Errors
///
/// [`InferenceError`] for a non-positive penalty or a shape mismatch.
pub fn ridge(
    design: &[f64],
    rows: usize,
    predictors: usize,
    y: &[f64],
    penalty: f64,
) -> Result<(Vec<f64>, Vec<f64>), InferenceError> {
    if penalty < 0.0 || !penalty.is_finite() {
        return Err(InferenceError::Domain(format!(
            "a ridge penalty must not be negative, and `{penalty}` is. A negative penalty \
             rewards large coefficients, which is the opposite of what this is for"
        )));
    }
    if y.len() != rows || design.len() != rows * predictors {
        return Err(InferenceError::Unpaired { left: rows, right: y.len() });
    }

    // Augmented least squares: stack `√λ · I` beneath the design and zeroes beneath `y`. The
    // same answer as the penalised normal equations, obtained through QR so the conditioning
    // argument above still holds.
    let root = penalty.sqrt();
    let augmented_rows = rows + predictors;
    let mut augmented = Vec::with_capacity(augmented_rows * predictors);
    augmented.extend_from_slice(design);
    for j in 0..predictors {
        for k in 0..predictors {
            augmented.push(if j == k { root } else { 0.0 });
        }
    }
    let mut targets = y.to_vec();
    targets.extend(std::iter::repeat_n(0.0, predictors));

    let fit = least_squares(&augmented, augmented_rows, predictors, &targets)?;
    // Only the residuals of the real observations; the augmented rows are the penalty, not
    // data, and reporting their residuals would report the penalty as unexplained variance.
    let residuals = fit.residuals.into_iter().take(rows).collect();
    Ok((fit.coefficients, residuals))
}
