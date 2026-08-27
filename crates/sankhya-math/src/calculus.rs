//! Numerical differentiation and integration over sampled data.
//!
//! # What "numerical" means here, and what it costs
//!
//! These operate on **samples**, not on functions. There is no symbolic derivative and no
//! closed form --- only values at points, and every result is an approximation whose error
//! depends on how closely those points are spaced.
//!
//! That is worth stating plainly because the error is invisible. A derivative of noisy data
//! is noisier than the data: differencing amplifies high-frequency error by roughly `1/h`,
//! so halving the spacing doubles the noise while halving the truncation error. There is a
//! spacing that minimises the total and it depends on the noise, which nothing here knows.
//!
//! # Determinism
//!
//! Integration is a weighted sum, so it goes through [`crate::deterministic_sum`] like
//! everything else. Differentiation is elementwise and needs nothing.

use crate::deterministic_sum;
use crate::vector::VectorError;

/// Successive differences: `y[i+1] - y[i]`.
///
/// One shorter than the input, which is a fact worth being deliberate about. Padding to the
/// original length --- with a zero, or by repeating the last value --- invents a difference
/// that was never observed, and it sits at whichever end the padding chose.
pub fn differences(values: &[f64]) -> Result<Vec<f64>, VectorError> {
    if values.len() < 2 {
        return Err(VectorError::Empty);
    }
    Ok(values
        .windows(2)
        .map(|w| w.get(1).copied().unwrap_or(0.0) - w.first().copied().unwrap_or(0.0))
        .collect())
}

/// The derivative at each point, by central differences where possible.
///
/// Central differences in the interior --- second-order accurate --- and one-sided at the
/// ends, where there is nothing on one side. The ends are therefore less accurate than the
/// middle, which is a property of the data rather than a shortcoming: nobody can do better
/// with a point that has no neighbour.
///
/// `spacing` is the distance between samples, assumed uniform.
pub fn derivative(values: &[f64], spacing: f64) -> Result<Vec<f64>, VectorError> {
    if values.len() < 2 {
        return Err(VectorError::Empty);
    }
    if spacing == 0.0 {
        // Every derivative would be infinite. Refusing says which input was impossible;
        // returning infinities says only that something went wrong somewhere.
        return Err(VectorError::ZeroMagnitude);
    }
    let n = values.len();
    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let slope = if i == 0 {
            (values.get(1).copied().unwrap_or(0.0) - values.first().copied().unwrap_or(0.0))
                / spacing
        } else if i == n - 1 {
            (values.get(n - 1).copied().unwrap_or(0.0) - values.get(n - 2).copied().unwrap_or(0.0))
                / spacing
        } else {
            (values.get(i + 1).copied().unwrap_or(0.0) - values.get(i - 1).copied().unwrap_or(0.0))
                / (2.0 * spacing)
        };
        out.push(slope);
    }
    Ok(out)
}

/// The second derivative, by central differences.
pub fn second_derivative(values: &[f64], spacing: f64) -> Result<Vec<f64>, VectorError> {
    if values.len() < 3 {
        return Err(VectorError::Empty);
    }
    if spacing == 0.0 {
        return Err(VectorError::ZeroMagnitude);
    }
    let n = values.len();
    let h2 = spacing * spacing;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // The ends reuse their neighbour's value: a one-sided second difference needs three
        // points on one side, and inventing them would be worse than repeating a figure the
        // data does support.
        let centre = i.clamp(1, n.saturating_sub(2));
        let left = values.get(centre - 1).copied().unwrap_or(0.0);
        let middle = values.get(centre).copied().unwrap_or(0.0);
        let right = values.get(centre + 1).copied().unwrap_or(0.0);
        out.push((right - 2.0 * middle + left) / h2);
    }
    Ok(out)
}

/// The integral by the trapezoidal rule.
///
/// Exact for anything linear between samples, and second-order accurate otherwise.
pub fn integrate_trapezoid(values: &[f64], spacing: f64) -> Result<f64, VectorError> {
    if values.len() < 2 {
        return Err(VectorError::Empty);
    }
    let mut terms: Vec<f64> = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        // The interior points count once, the endpoints half. Halving them here rather than
        // doubling the interior keeps every term the same order of magnitude, which is what
        // the deterministic sum handles best.
        let weight = if index == 0 || index == values.len() - 1 {
            0.5
        } else {
            1.0
        };
        terms.push(value * weight * spacing);
    }
    Ok(deterministic_sum(&terms))
}

/// The integral by Simpson's rule.
///
/// Exact for anything cubic between samples, and fourth-order accurate otherwise --- so for
/// smooth data it is far better than the trapezoid at the same spacing.
///
/// # Errors
///
/// Needs an odd number of samples, which is an even number of intervals. Refuses an even
/// count rather than silently dropping the last sample or falling back to the trapezoid:
/// both change the answer, and neither says so.
pub fn integrate_simpson(values: &[f64], spacing: f64) -> Result<f64, VectorError> {
    let n = values.len();
    if n < 3 || n % 2 == 0 {
        return Err(VectorError::LengthMismatch {
            left: n,
            // The nearest valid count, so the message says what would work.
            right: if n < 3 { 3 } else { n + 1 },
        });
    }
    let mut terms: Vec<f64> = Vec::with_capacity(n);
    for (index, value) in values.iter().enumerate() {
        let weight = if index == 0 || index == n - 1 {
            1.0
        } else if index % 2 == 1 {
            4.0
        } else {
            2.0
        };
        terms.push(value * weight * spacing / 3.0);
    }
    Ok(deterministic_sum(&terms))
}

/// The running total at each point.
///
/// Each prefix is summed deterministically rather than accumulated forward, so element `k`
/// is the same number whether it was computed alone or as part of this series. Accumulating
/// forward is `O(n)` and gives a different answer from summing the prefix directly, which
/// means a running total and a windowed total over the same values would disagree.
pub fn cumulative_sum(values: &[f64]) -> Vec<f64> {
    (1..=values.len())
        .map(|k| deterministic_sum(values.get(..k).unwrap_or(&[])))
        .collect()
}

/// The cumulative integral: the area under the curve up to each point.
pub fn cumulative_integral(values: &[f64], spacing: f64) -> Result<Vec<f64>, VectorError> {
    if values.len() < 2 {
        return Err(VectorError::Empty);
    }
    let mut out = Vec::with_capacity(values.len());
    out.push(0.0);
    for k in 2..=values.len() {
        out.push(integrate_trapezoid(
            values.get(..k).unwrap_or(&[]),
            spacing,
        )?);
    }
    Ok(out)
}
