//! Hypothesis tests and regression, named on the SQL surface.
//!
//! # Why each result is several functions rather than one returning a record
//!
//! A test reports a statistic, its degrees of freedom and a p-value; a regression reports a
//! coefficient, a standard error, a *t* and a p-value for each predictor. A composite return
//! crosses this wire as a string a client then has to parse — and the common case is wanting
//! one of them, so computing and rendering all four to get the p-value is waste.
//!
//! So each part is named. `ttest_1samp_t`, `ttest_1samp_p` and so on: the suffix says which
//! part, and a caller wanting all of them writes all of them.
//!
//! # Why a p-value is never assembled by the caller
//!
//! `2 * (1 - t_cdf(abs(t), df))` is what somebody writes, and it is wrong in the tail where
//! subtracting from one costs every digit. Every p-value here comes from a survival function
//! directly, and offering it by name is what stops the expression being written.

// Every kernel below indexes its arguments positionally --- `a[0]`, `a[1]` --- and the arity
// is checked in the wrapper **before** the kernel runs, so an index out of bounds cannot
// occur. The alternative is a `get` and an `unwrap_or` per argument, which would turn a
// missing argument into a silent zero: exactly the wrong answer this catalogue is arranged
// against, traded for a lint the wrapper has already satisfied.
#![allow(clippy::indexing_slicing)]
use crate::multi::Multi;
use crate::property::Property;
use crate::scalar::Numeric;
use datafusion::logical_expr::ScalarUDF;
use sankhya_math::{inference, regression};

/// Every test and regression function.
#[must_use]
pub fn functions() -> Vec<ScalarUDF> {
    vec![
        // --- one-sample t, over one row's series against a hypothesised mean ---
        ScalarUDF::from(Pair::statistic("ttest_1samp_t", |values, mean| {
            inference::ttest_one_sample(values, mean).map(|r| r.statistic)
        })),
        ScalarUDF::from(Pair::statistic("ttest_1samp_p", |values, mean| {
            inference::ttest_one_sample(values, mean).map(|r| r.p_value)
        })),
        ScalarUDF::from(Pair::statistic("ttest_1samp_df", |values, mean| {
            inference::ttest_one_sample(values, mean).map(|r| r.freedom)
        })),
        // --- Jarque-Bera, over one series ---
        ScalarUDF::from(Property::new("jarque_bera_t", |values| {
            inference::jarque_bera(values).map(|r| r.statistic).map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Property::new("jarque_bera_p", |values| {
            inference::jarque_bera(values).map(|r| r.p_value).map_err(|e| e.to_string())
        })),
        // --- two-sample tests, over two series ---
        ScalarUDF::from(Multi::new("ttest_2samp_t", 2, |a| {
            inference::ttest_two_sample(&a[0], &a[1])
                .map(|r| r.statistic)
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("ttest_2samp_p", 2, |a| {
            inference::ttest_two_sample(&a[0], &a[1])
                .map(|r| r.p_value)
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("ttest_2samp_df", 2, |a| {
            inference::ttest_two_sample(&a[0], &a[1])
                .map(|r| r.freedom)
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("ttest_paired_t", 2, |a| {
            inference::ttest_paired(&a[0], &a[1]).map(|r| r.statistic).map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("ttest_paired_p", 2, |a| {
            inference::ttest_paired(&a[0], &a[1]).map(|r| r.p_value).map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("chisq_test_t", 2, |a| {
            inference::chisq_goodness(&a[0], &a[1])
                .map(|r| r.statistic)
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("chisq_test_p", 2, |a| {
            inference::chisq_goodness(&a[0], &a[1])
                .map(|r| r.p_value)
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("f_test_t", 2, |a| {
            inference::f_test(&a[0], &a[1]).map(|r| r.statistic).map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("f_test_p", 2, |a| {
            inference::f_test(&a[0], &a[1]).map(|r| r.p_value).map_err(|e| e.to_string())
        })),
        // --- simple regression, one part per name ---
        //
        // A slope with no standard error is a number nobody can act on, so the uncertainty is
        // named beside it rather than left to a caller to derive.
        ScalarUDF::from(Multi::new("regress_slope", 2, |a| {
            regression::simple(&a[0], &a[1])
                .map(|f| f.coefficients.get(1).copied().unwrap_or(f64::NAN))
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("regress_intercept", 2, |a| {
            regression::simple(&a[0], &a[1])
                .map(|f| f.coefficients.first().copied().unwrap_or(f64::NAN))
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("regress_stderr", 2, |a| {
            regression::simple(&a[0], &a[1])
                .map(|f| f.standard_errors.get(1).copied().unwrap_or(f64::NAN))
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("regress_tstat", 2, |a| {
            regression::simple(&a[0], &a[1])
                .map(|f| f.t_statistics.get(1).copied().unwrap_or(f64::NAN))
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("regress_pvalue", 2, |a| {
            regression::simple(&a[0], &a[1])
                .map(|f| f.p_values.get(1).copied().unwrap_or(f64::NAN))
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::defined_sometimes("regress_r2", 2, |a| {
            regression::simple(&a[0], &a[1]).map(|f| f.r_squared).map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::defined_sometimes("regress_adj_r2", 2, |a| {
            regression::simple(&a[0], &a[1])
                .map(|f| f.adjusted_r_squared)
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("regress_residual_error", 2, |a| {
            regression::simple(&a[0], &a[1]).map(|f| f.residual_error).map_err(|e| e.to_string())
        })),
        // --- multiple regression: a flat design matrix, the targets, and the predictor count ---
        ScalarUDF::from(Multi::defined_sometimes("regress_multiple_r2", 3, |a| {
            let predictors = crate::multi::one(a, 2, "the number of predictors")?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let predictors = predictors as usize;
            regression::least_squares(&a[0], a[1].len(), predictors, &a[1])
                .map(|f| f.r_squared)
                .map_err(|e| e.to_string())
        })),
        ScalarUDF::from(Multi::new("regress_multiple_f_p", 3, |a| {
            let predictors = crate::multi::one(a, 2, "the number of predictors")?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let predictors = predictors as usize;
            regression::least_squares(&a[0], a[1].len(), predictors, &a[1])
                .map(|f| f.f_p_value)
                .map_err(|e| e.to_string())
        })),
        // --- ridge, whose coefficients are reported without a t-statistic ---
        //
        // Deliberately: the penalty invalidates the unpenalised standard errors, so offering a
        // significance test beside a shrunk coefficient would invite one the arithmetic does
        // not support.
        ScalarUDF::from(Multi::new("regress_ridge_norm", 4, |a| {
            let predictors = crate::multi::one(a, 2, "the number of predictors")?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let predictors = predictors as usize;
            let penalty = crate::multi::one(a, 3, "the penalty")?;
            regression::ridge(&a[0], a[1].len(), predictors, &a[1], penalty)
                .map(|(coefficients, _)| {
                    coefficients.iter().map(|c| c * c).sum::<f64>().sqrt()
                })
                .map_err(|e| e.to_string())
        })),
        // --- the degenerate arity guard, so a caller sees a name they can look up ---
        ScalarUDF::from(Numeric::new("ttest_df_welch", 4, |a| {
            // Welch-Satterthwaite from two variances and two counts, for a caller who has the
            // summary statistics rather than the samples --- which is what an aggregate query
            // leaves them with.
            let (v1, n1, v2, n2) = (a[0], a[1], a[2], a[3]);
            if n1 < 2.0 || n2 < 2.0 {
                return Err("each sample needs at least two observations".to_owned());
            }
            let (s1, s2) = (v1 / n1, v2 / n2);
            Ok((s1 + s2).powi(2) / (s1 * s1 / (n1 - 1.0) + s2 * s2 / (n2 - 1.0)))
        })),
    ]
}

/// A function of one series and one number.
///
/// The shape a one-sample test has: the observations, and the value they are tested against.
struct Pair;

impl Pair {
    /// Build one, given a kernel over the series and the scalar.
    fn statistic(
        name: &'static str,
        kernel: impl Fn(&[f64], f64) -> Result<f64, inference::InferenceError>
            + Send
            + Sync
            + 'static,
    ) -> crate::seriesnum::SeriesAndNumber {
        crate::seriesnum::SeriesAndNumber::new(name, move |values, number| {
            kernel(values, number).map_err(|error| error.to_string())
        })
    }
}
