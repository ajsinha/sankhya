//! Descriptive statistics, all of them order-independent.
//!
//! # Why every one of these goes through the deterministic sum
//!
//! A mean is a sum. A variance is a sum of squares. A correlation is three sums. So every
//! function here inherits the reason [`crate::deterministic_sum`] exists: a total whose
//! order depends on how work was partitioned returns a different number when the machine is
//! busier, and the difference is too small to notice and too large to reconcile.
//!
//! # The two-pass rule
//!
//! Variance is computed in two passes --- mean first, then squared deviations --- rather than
//! by the textbook one-pass identity `E[x²] - E[x]²`. That identity is algebraically correct
//! and numerically disastrous: for values with a large mean and small spread it subtracts
//! two nearly equal large numbers, and catastrophic cancellation can produce a *negative*
//! variance. The two-pass form costs one extra traversal and cannot do that.

use crate::deterministic_sum;
use crate::vector::VectorError;

/// Whether a sample or a whole population is being described.
///
/// The distinction is the divisor --- `n - 1` for a sample, `n` for a population --- and it
/// is a decision about what the data *is*, so there is no default. A sample variance
/// reported as a population variance understates the spread, systematically, and by more
/// the smaller the sample.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Population {
    /// The values are the whole population. Divide by `n`.
    Whole,
    /// The values are a sample of a larger population. Divide by `n - 1`.
    Sample,
}

impl Population {
    /// The divisor for `n` values.
    const fn divisor(self, n: usize) -> Option<usize> {
        match self {
            Self::Whole => {
                if n == 0 {
                    None
                } else {
                    Some(n)
                }
            }
            // A sample of one has no spread to estimate: the divisor would be zero, and any
            // number returned would be invented.
            Self::Sample => {
                if n < 2 {
                    None
                } else {
                    Some(n - 1)
                }
            }
        }
    }
}

/// The arithmetic mean.
pub fn mean(values: &[f64]) -> Result<f64, VectorError> {
    crate::vector::mean(values)
}

/// The variance.
///
/// Two passes, deliberately. See the module comment: the one-pass identity can return a
/// negative variance for values with a large mean and small spread.
pub fn variance(values: &[f64], of: Population) -> Result<f64, VectorError> {
    let Some(divisor) = of.divisor(values.len()) else {
        return Err(VectorError::Empty);
    };
    let centre = mean(values)?;
    let squares: Vec<f64> = values.iter().map(|x| (x - centre) * (x - centre)).collect();
    #[allow(clippy::cast_precision_loss)]
    Ok(deterministic_sum(&squares) / divisor as f64)
}

/// The standard deviation.
pub fn standard_deviation(values: &[f64], of: Population) -> Result<f64, VectorError> {
    Ok(variance(values, of)?.sqrt())
}

/// The covariance of two equal-length series.
pub fn covariance(a: &[f64], b: &[f64], of: Population) -> Result<f64, VectorError> {
    if a.len() != b.len() {
        return Err(VectorError::LengthMismatch {
            left: a.len(),
            right: b.len(),
        });
    }
    let Some(divisor) = of.divisor(a.len()) else {
        return Err(VectorError::Empty);
    };
    let (ma, mb) = (mean(a)?, mean(b)?);
    let products: Vec<f64> = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).collect();
    #[allow(clippy::cast_precision_loss)]
    Ok(deterministic_sum(&products) / divisor as f64)
}

/// Pearson's correlation coefficient.
///
/// # Errors
///
/// Refuses a series with no variation. A constant series has no correlation with anything ---
/// not zero, which would say "unrelated", but undefined. Returning zero would place a
/// constant column at a definite relationship with every other one, and it would show up in
/// every ranked correlation table as genuinely uncorrelated rather than as unanswerable.
pub fn correlation(a: &[f64], b: &[f64]) -> Result<f64, VectorError> {
    let (sa, sb) = (
        standard_deviation(a, Population::Sample)?,
        standard_deviation(b, Population::Sample)?,
    );
    if sa == 0.0 || sb == 0.0 {
        return Err(VectorError::ZeroMagnitude);
    }
    Ok(covariance(a, b, Population::Sample)? / (sa * sb))
}

/// The median.
///
/// The average of the middle two for an even count, which is the convention almost everyone
/// means. [`crate::quantile`] offers the others explicitly.
pub fn median(values: &[f64]) -> Result<f64, VectorError> {
    if values.is_empty() {
        return Err(VectorError::Empty);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = sorted.len() / 2;
    Ok(if sorted.len() % 2 == 1 {
        sorted.get(middle).copied().unwrap_or(0.0)
    } else {
        let lower = sorted.get(middle - 1).copied().unwrap_or(0.0);
        let upper = sorted.get(middle).copied().unwrap_or(0.0);
        // Halve each and add, rather than add and halve: the sum of two values near the
        // representable maximum overflows to infinity, and the median of two finite numbers
        // is never infinite.
        lower / 2.0 + upper / 2.0
    })
}

/// The smallest and largest values.
///
/// `None` for an empty series. A minimum of nothing is not zero and not infinity, and both
/// are values somebody would act on.
#[must_use]
pub fn range(values: &[f64]) -> Option<(f64, f64)> {
    let mut iter = values.iter().copied().filter(|v| !v.is_nan());
    let first = iter.next()?;
    Some(iter.fold((first, first), |(lo, hi), v| (lo.min(v), hi.max(v))))
}

/// The skewness: how asymmetric the distribution is.
///
/// Positive means a longer right tail. The adjusted Fisher-Pearson form, which is what
/// spreadsheets and most statistics packages report --- an unadjusted figure differs enough
/// on small samples to look like a different dataset.
pub fn skewness(values: &[f64]) -> Result<f64, VectorError> {
    let n = values.len();
    if n < 3 {
        // The adjustment divides by (n - 1)(n - 2). Below three values there is no shape to
        // describe and the formula has no answer rather than a small one.
        return Err(VectorError::Empty);
    }
    let centre = mean(values)?;
    let spread = standard_deviation(values, Population::Sample)?;
    if spread == 0.0 {
        return Err(VectorError::ZeroMagnitude);
    }
    let cubes: Vec<f64> = values
        .iter()
        .map(|x| ((x - centre) / spread).powi(3))
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let n = n as f64;
    Ok(n / ((n - 1.0) * (n - 2.0)) * deterministic_sum(&cubes))
}

/// The excess kurtosis: how heavy the tails are, relative to a normal distribution.
///
/// Zero for a normal distribution, because the three is already subtracted. Reporting raw
/// kurtosis instead is a common and confusing choice --- a reader seeing 3.0 cannot tell
/// whether it means "normal" or "quite heavy-tailed" without knowing which convention was
/// used, and both are plausible.
pub fn excess_kurtosis(values: &[f64]) -> Result<f64, VectorError> {
    let count = values.len();
    if count < 4 {
        return Err(VectorError::Empty);
    }
    let centre = mean(values)?;
    let spread = standard_deviation(values, Population::Sample)?;
    if spread == 0.0 {
        return Err(VectorError::ZeroMagnitude);
    }
    let fourths: Vec<f64> = values
        .iter()
        .map(|x| ((x - centre) / spread).powi(4))
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let n = count as f64;
    let raw = n * (n + 1.0) / ((n - 1.0) * (n - 2.0) * (n - 3.0)) * deterministic_sum(&fourths);
    let correction = 3.0 * (n - 1.0) * (n - 1.0) / ((n - 2.0) * (n - 3.0));
    Ok(raw - correction)
}

/// Standardise a series to zero mean and unit variance.
///
/// The transformation almost every model wants first. Refuses a constant series: dividing
/// by a zero spread would produce infinities, and substituting zeroes would say every value
/// is exactly average, which is true and useless and indistinguishable from real data.
pub fn standardise(values: &[f64], of: Population) -> Result<Vec<f64>, VectorError> {
    let centre = mean(values)?;
    let spread = standard_deviation(values, of)?;
    if spread == 0.0 {
        return Err(VectorError::ZeroMagnitude);
    }
    Ok(values.iter().map(|x| (x - centre) / spread).collect())
}

/// Ordinary least squares against one predictor: slope, intercept, and the fit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LinearFit {
    /// The slope.
    pub slope: f64,
    /// The intercept.
    pub intercept: f64,
    /// The coefficient of determination, between zero and one.
    pub r_squared: f64,
}

/// Fit `y = slope * x + intercept` by least squares.
///
/// # Errors
///
/// Refuses a predictor with no variation. A vertical line has infinite slope, and a fit
/// through a single x-value is not a line --- it is a claim about a relationship that the
/// data cannot support.
pub fn linear_fit(x: &[f64], y: &[f64]) -> Result<LinearFit, VectorError> {
    if x.len() != y.len() {
        return Err(VectorError::LengthMismatch {
            left: x.len(),
            right: y.len(),
        });
    }
    let variance_x = variance(x, Population::Sample)?;
    if variance_x == 0.0 {
        return Err(VectorError::ZeroMagnitude);
    }
    let slope = covariance(x, y, Population::Sample)? / variance_x;
    let intercept = mean(y)? - slope * mean(x)?;

    // R² from the correlation, which is exact for a single predictor and avoids a second
    // pass over the residuals.
    let r_squared = match correlation(x, y) {
        Ok(r) => r * r,
        // A constant response has no variation to explain. Zero is the honest figure: the
        // fit explains none of a variance that is itself zero.
        Err(VectorError::ZeroMagnitude) => 0.0,
        Err(other) => return Err(other),
    };
    Ok(LinearFit {
        slope,
        intercept,
        r_squared,
    })
}
