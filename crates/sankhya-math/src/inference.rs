//! Hypothesis tests and regression, each reporting what a decision needs.
//!
//! # Why every result carries more than a number
//!
//! A slope with no standard error is a number nobody can act on, and a test statistic with no
//! p-value is a number somebody will compare against a threshold they half-remember. So each
//! of these returns a small record --- the estimate, its uncertainty, the statistic and the
//! tail --- and the SQL surface names the parts separately rather than rendering a struct as
//! text a client has to parse.
//!
//! # Why the p-values come from the survival functions
//!
//! Every tail here is taken from `chisq_sf`, `f_sf` or `t_two_sided` rather than as
//! `1 - cdf`. A p-value of `1e-20` subtracted from one is zero, and a test reporting `p = 0`
//! where the truth is `1e-20` has thrown away the only digits anybody was going to read ---
//! and in the direction that makes a finding look stronger than it is.

use crate::distribution::{chisq_sf, f_sf, t_two_sided};
use crate::reduce::deterministic_sum;
use crate::vector::VectorError;

/// A test that could not be computed from what it was given.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InferenceError {
    /// Not enough observations for the test to have any degrees of freedom.
    TooFew {
        /// How many arrived.
        given: usize,
        /// How many this test needs.
        needs: usize,
    },
    /// Two samples of different lengths where the test pairs them.
    Unpaired {
        /// The first sample's length.
        left: usize,
        /// The second's.
        right: usize,
    },
    /// A sample with no variation, so the statistic has a zero denominator.
    NoVariation {
        /// Which sample.
        which: &'static str,
    },
    /// A distribution parameter was outside its domain.
    Domain(String),
    /// An expected count of zero, which a chi-squared statistic divides by.
    ZeroExpected {
        /// Which cell.
        at: usize,
    },
    /// A `NaN` or an infinity where a finite number was required.
    ///
    /// Refused rather than propagated. A `NaN` travelling through a fit reaches the
    /// significance test as a `NaN` statistic, and `!t.is_finite()` is a condition an infinite
    /// statistic also meets --- for which a p-value of zero is correct. So a single missing
    /// value reported **every** coefficient as significant at `p = 0`, which is the most
    /// confident possible statement about the least information.
    ///
    /// A null is filtered before it reaches here; a `NaN` is not a null, and a column that has
    /// been through a divide-by-zero upstream carries them.
    NotFinite {
        /// Which input.
        which: &'static str,
    },
}

impl std::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFew { given, needs } => write!(
                f,
                "this test needs at least {needs} observations and was given {given}. Refused \
                 rather than answered: a test with no degrees of freedom returns a statistic \
                 that cannot reject anything, which reads as evidence of no effect"
            ),
            Self::Unpaired { left, right } => write!(
                f,
                "a paired test needs samples of equal length, and these are {left} and \
                 {right}. Truncating to the shorter would pair observations that are not \
                 pairs, and the result would be a test of nothing in particular"
            ),
            Self::NoVariation { which } => write!(
                f,
                "the {which} sample has no variation, so this statistic divides by zero. \
                 Reported rather than returned as infinity: an infinite t is not overwhelming \
                 evidence, it is a sample somebody needs to look at"
            ),
            Self::Domain(detail) => write!(f, "{detail}"),
            Self::NotFinite { which } => write!(
                f,
                "the {which} holds a value that is not a finite number. Refused rather than \
                 propagated: a NaN reaching the significance test makes every coefficient \
                 report a p-value of zero, which is the most confident possible claim made \
                 from the least information"
            ),
            Self::ZeroExpected { at } => write!(
                f,
                "the expected count in cell {at} is zero, which a chi-squared statistic \
                 divides by. A cell nobody expects to be filled cannot contribute evidence, \
                 and pretending it contributes infinity is not the same as it contributing a \
                 lot"
            ),
        }
    }
}

impl std::error::Error for InferenceError {}

impl From<crate::special::DomainError> for InferenceError {
    fn from(error: crate::special::DomainError) -> Self {
        Self::Domain(error.to_string())
    }
}

impl From<VectorError> for InferenceError {
    fn from(error: VectorError) -> Self {
        Self::Domain(error.to_string())
    }
}

/// What a test reports.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TestResult {
    /// The statistic.
    pub statistic: f64,
    /// Its degrees of freedom.
    pub freedom: f64,
    /// The two-sided p-value, or the upper tail for a one-sided test.
    pub p_value: f64,
}

/// The mean and the sample variance of a slice, in one pass over the sorted sum.
fn moments(values: &[f64]) -> (f64, f64) {
    #[allow(clippy::cast_precision_loss)]
    let n = values.len() as f64;
    let mean = deterministic_sum(values) / n;
    let squares: Vec<f64> = values.iter().map(|x| (x - mean) * (x - mean)).collect();
    let variance = if values.len() > 1 {
        deterministic_sum(&squares) / (n - 1.0)
    } else {
        0.0
    };
    (mean, variance)
}

/// A one-sample *t*-test against a hypothesised mean.
///
/// # Errors
///
/// [`InferenceError`] for fewer than two observations or a sample with no variation.
pub fn ttest_one_sample(values: &[f64], hypothesised: f64) -> Result<TestResult, InferenceError> {
    if values.len() < 2 {
        return Err(InferenceError::TooFew { given: values.len(), needs: 2 });
    }
    let (mean, variance) = moments(values);
    if variance <= 0.0 {
        return Err(InferenceError::NoVariation { which: "only" });
    }
    #[allow(clippy::cast_precision_loss)]
    let n = values.len() as f64;
    let statistic = (mean - hypothesised) / (variance / n).sqrt();
    let freedom = n - 1.0;
    Ok(TestResult { statistic, freedom, p_value: t_two_sided(statistic, freedom)? })
}

/// Welch's two-sample *t*-test, which does **not** assume equal variances.
///
/// # Why Welch rather than Student's pooled form
///
/// The pooled test assumes the two populations share a variance, and when they do not it is
/// wrong in the direction of rejecting too often --- it finds differences that are not there.
/// Welch costs a slightly awkward degrees-of-freedom formula and is correct whether or not the
/// variances match, so there is no case where the pooled form is the better default and a
/// caller has to know which they have.
///
/// # Errors
///
/// [`InferenceError`] for fewer than two observations in either sample, or no variation.
pub fn ttest_two_sample(left: &[f64], right: &[f64]) -> Result<TestResult, InferenceError> {
    if left.len() < 2 {
        return Err(InferenceError::TooFew { given: left.len(), needs: 2 });
    }
    if right.len() < 2 {
        return Err(InferenceError::TooFew { given: right.len(), needs: 2 });
    }
    let (mean_left, var_left) = moments(left);
    let (mean_right, var_right) = moments(right);
    if var_left <= 0.0 && var_right <= 0.0 {
        return Err(InferenceError::NoVariation { which: "both" });
    }
    #[allow(clippy::cast_precision_loss)]
    let (n_left, n_right) = (left.len() as f64, right.len() as f64);

    let se_left = var_left / n_left;
    let se_right = var_right / n_right;
    let standard_error = (se_left + se_right).sqrt();
    let statistic = (mean_left - mean_right) / standard_error;

    // Welch-Satterthwaite. Not an integer, and deliberately not rounded to one: rounding down
    // is conservative and rounding to nearest is not, and both change a p-value somebody will
    // compare against 0.05.
    let freedom = (se_left + se_right).powi(2)
        / (se_left * se_left / (n_left - 1.0) + se_right * se_right / (n_right - 1.0));
    Ok(TestResult { statistic, freedom, p_value: t_two_sided(statistic, freedom)? })
}

/// A paired *t*-test, on the differences.
///
/// # Errors
///
/// [`InferenceError::Unpaired`] for samples of different lengths, which is the mistake this
/// test exists to be careful about: two unpaired samples given to a paired test produce a
/// number, and the number is a test of nothing.
pub fn ttest_paired(left: &[f64], right: &[f64]) -> Result<TestResult, InferenceError> {
    if left.len() != right.len() {
        return Err(InferenceError::Unpaired { left: left.len(), right: right.len() });
    }
    let differences: Vec<f64> = left.iter().zip(right).map(|(a, b)| a - b).collect();
    ttest_one_sample(&differences, 0.0)
}

/// Pearson's chi-squared goodness-of-fit statistic.
///
/// # Errors
///
/// [`InferenceError`] for mismatched lengths, fewer than two cells, or a zero expected count.
pub fn chisq_goodness(observed: &[f64], expected: &[f64]) -> Result<TestResult, InferenceError> {
    if observed.len() != expected.len() {
        return Err(InferenceError::Unpaired { left: observed.len(), right: expected.len() });
    }
    if observed.len() < 2 {
        return Err(InferenceError::TooFew { given: observed.len(), needs: 2 });
    }
    let mut terms = Vec::with_capacity(observed.len());
    for (at, (o, e)) in observed.iter().zip(expected).enumerate() {
        if *e == 0.0 {
            return Err(InferenceError::ZeroExpected { at });
        }
        terms.push((o - e) * (o - e) / e);
    }
    let statistic = deterministic_sum(&terms);
    #[allow(clippy::cast_precision_loss)]
    let freedom = observed.len() as f64 - 1.0;
    // The **upper** tail, which is the p-value: a chi-squared test rejects for large
    // statistics, so `1 - cdf` is the quantity, and taking it by subtraction loses it.
    Ok(TestResult { statistic, freedom, p_value: chisq_sf(statistic, freedom)? })
}

/// An *F*-test comparing two variances.
///
/// # Errors
///
/// [`InferenceError`] for fewer than two observations, or a second sample with no variation.
pub fn f_test(left: &[f64], right: &[f64]) -> Result<TestResult, InferenceError> {
    if left.len() < 2 {
        return Err(InferenceError::TooFew { given: left.len(), needs: 2 });
    }
    if right.len() < 2 {
        return Err(InferenceError::TooFew { given: right.len(), needs: 2 });
    }
    let (_, var_left) = moments(left);
    let (_, var_right) = moments(right);
    if var_right <= 0.0 {
        return Err(InferenceError::NoVariation { which: "second" });
    }
    let statistic = var_left / var_right;
    #[allow(clippy::cast_precision_loss)]
    let (df_left, df_right) = (left.len() as f64 - 1.0, right.len() as f64 - 1.0);
    // Two-sided, which is what a comparison of variances almost always means: the statistic
    // is as surprising when it is small as when it is large, and reporting only the upper tail
    // halves the p-value of a variance that is smaller rather than larger.
    let upper = f_sf(statistic, df_left, df_right)?;
    // The lower tail through the *F* distribution's own reciprocal symmetry, not as
    // `1.0 - upper`.
    //
    // The module header states the rule --- *"every tail here is taken from `chisq_sf`, `f_sf`
    // or `t_two_sided` rather than as `1 - cdf`"* --- and this line broke it. A double near one
    // has no bits left below about `1e-16`, so every lower-tail probability smaller than that
    // was reported as **zero**: in the direction that makes a finding look stronger than it is,
    // which is the direction nobody checks.
    //
    // `F(d1, d2)` and `1 / F(d2, d1)` are the same distribution, so the lower tail of the
    // statistic is the upper tail of its reciprocal with the freedoms exchanged --- computed,
    // like every other tail here, by the routine that computes tails.
    let lower = if statistic > 0.0 {
        f_sf(1.0 / statistic, df_right, df_left)?
    } else {
        // A first sample with no variation at all. The lower tail is zero exactly, and the
        // reciprocal would be an infinity.
        0.0
    };
    let p_value = (2.0 * upper.min(lower)).min(1.0);
    Ok(TestResult { statistic, freedom: df_left, p_value })
}

/// The Jarque-Bera test for normality, from the skewness and kurtosis.
///
/// # Errors
///
/// [`InferenceError`] for fewer than four observations, which is the fewest that has a
/// kurtosis at all.
pub fn jarque_bera(values: &[f64]) -> Result<TestResult, InferenceError> {
    if values.len() < 4 {
        return Err(InferenceError::TooFew { given: values.len(), needs: 4 });
    }
    let skew = crate::stats::skewness(values)?;
    let kurtosis = crate::stats::excess_kurtosis(values)?;
    #[allow(clippy::cast_precision_loss)]
    let n = values.len() as f64;
    let statistic = n / 6.0 * (skew * skew + kurtosis * kurtosis / 4.0);
    // Two degrees of freedom, always: the test has two terms whatever the sample size.
    Ok(TestResult { statistic, freedom: 2.0, p_value: chisq_sf(statistic, 2.0)? })
}
