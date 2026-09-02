//! The distributions, named on the SQL surface.
//!
//! # The naming rule
//!
//! `<family>_pdf`, `<family>_cdf`, `<family>_sf` and `<family>_inv`. Four suffixes, the same
//! four everywhere, so a caller who has met one family can guess the rest — and `sf` is
//! present on every family that has a tail worth asking for, because `1 - cdf` in the tail has
//! no digits left and is the expression somebody writes instead.

// Every kernel below indexes its arguments positionally --- `a[0]`, `a[1]` --- and the arity
// is checked in the wrapper **before** the kernel runs, so an index out of bounds cannot
// occur. The alternative is a `get` and an `unwrap_or` per argument, which would turn a
// missing argument into a silent zero: exactly the wrong answer this catalogue is arranged
// against, traded for a lint the wrapper has already satisfied.
#![allow(clippy::indexing_slicing)]
use crate::scalar::Numeric;
use datafusion::logical_expr::ScalarUDF;
use sankhya_math::distribution as d;
use sankhya_math::special;

/// Turn a kernel's own error into the text a statement's refusal carries.
fn said<T>(outcome: Result<T, sankhya_math::DomainError>) -> Result<T, String> {
    outcome.map_err(|error| error.to_string())
}

/// Every distribution function.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn functions() -> Vec<ScalarUDF> {
    vec![
        // --- the normal, standard and general ---
        ScalarUDF::from(Numeric::new("norm_pdf", 1, |a| Ok(d::norm_pdf(a[0])))),
        ScalarUDF::from(Numeric::new("norm_cdf", 1, |a| Ok(d::norm_cdf(a[0])))),
        ScalarUDF::from(Numeric::new("norm_sf", 1, |a| Ok(d::norm_cdf(-a[0])))),
        ScalarUDF::from(Numeric::new("norm_inv", 1, |a| said(d::norm_inv(a[0])))),
        ScalarUDF::from(Numeric::new("normal_pdf", 3, |a| said(d::normal_pdf(a[0], a[1], a[2])))),
        ScalarUDF::from(Numeric::new("normal_cdf", 3, |a| said(d::normal_cdf(a[0], a[1], a[2])))),
        ScalarUDF::from(Numeric::new("normal_inv", 3, |a| said(d::normal_inv(a[0], a[1], a[2])))),
        ScalarUDF::from(Numeric::new("lognorm_cdf", 3, |a| {
            said(d::lognormal_cdf(a[0], a[1], a[2]))
        })),
        ScalarUDF::from(Numeric::new("lognorm_inv", 3, |a| {
            said(d::lognormal_inv(a[0], a[1], a[2]))
        })),
        // --- Student's t ---
        ScalarUDF::from(Numeric::new("t_pdf", 2, |a| said(d::t_pdf(a[0], a[1])))),
        ScalarUDF::from(Numeric::new("t_cdf", 2, |a| said(d::t_cdf(a[0], a[1])))),
        ScalarUDF::from(Numeric::new("t_sf", 2, |a| said(d::t_cdf(-a[0], a[1])))),
        ScalarUDF::from(Numeric::new("t_inv", 2, |a| said(d::t_inv(a[0], a[1])))),
        // The quantity a t-test reports, by name rather than as an expression somebody
        // assembles and gets wrong by forgetting the two or subtracting in the tail.
        ScalarUDF::from(Numeric::new("t_two_sided", 2, |a| said(d::t_two_sided(a[0], a[1])))),
        // --- chi-squared ---
        ScalarUDF::from(Numeric::new("chisq_cdf", 2, |a| said(d::chisq_cdf(a[0], a[1])))),
        ScalarUDF::from(Numeric::new("chisq_sf", 2, |a| said(d::chisq_sf(a[0], a[1])))),
        ScalarUDF::from(Numeric::new("chisq_inv", 2, |a| said(d::chisq_inv(a[0], a[1])))),
        // --- F ---
        ScalarUDF::from(Numeric::new("f_cdf", 3, |a| said(d::f_cdf(a[0], a[1], a[2])))),
        ScalarUDF::from(Numeric::new("f_sf", 3, |a| said(d::f_sf(a[0], a[1], a[2])))),
        ScalarUDF::from(Numeric::new("f_inv", 3, |a| said(d::f_inv(a[0], a[1], a[2])))),
        // --- the discrete ---
        ScalarUDF::from(Numeric::new("binom_pmf", 3, |a| {
            said(d::binomial_pmf(whole(a[0])?, whole(a[1])?, a[2]))
        })),
        ScalarUDF::from(Numeric::new("binom_cdf", 3, |a| {
            said(d::binomial_cdf(whole(a[0])?, whole(a[1])?, a[2]))
        })),
        ScalarUDF::from(Numeric::new("poisson_pmf", 2, |a| {
            said(d::poisson_pmf(whole(a[0])?, a[1]))
        })),
        ScalarUDF::from(Numeric::new("poisson_cdf", 2, |a| {
            said(d::poisson_cdf(whole(a[0])?, a[1]))
        })),
        // --- the simple continuous ---
        ScalarUDF::from(Numeric::new("expon_cdf", 2, |a| said(d::exponential_cdf(a[0], a[1])))),
        ScalarUDF::from(Numeric::new("gamma_cdf", 3, |a| said(d::gamma_cdf(a[0], a[1], a[2])))),
        ScalarUDF::from(Numeric::new("beta_cdf", 3, |a| said(d::beta_cdf(a[0], a[1], a[2])))),
        ScalarUDF::from(Numeric::new("uniform_cdf", 3, |a| said(d::uniform_cdf(a[0], a[1], a[2])))),
        // --- the special functions, which people ask for directly ---
        ScalarUDF::from(Numeric::new("erf", 1, |a| Ok(special::erf(a[0])))),
        ScalarUDF::from(Numeric::new("erfc", 1, |a| Ok(special::erfc(a[0])))),
        ScalarUDF::from(Numeric::new("gammaln", 1, |a| Ok(special::ln_gamma(a[0])))),
        ScalarUDF::from(Numeric::new("gamma_p", 2, |a| said(special::gamma_p(a[0], a[1])))),
        ScalarUDF::from(Numeric::new("gamma_q", 2, |a| said(special::gamma_q(a[0], a[1])))),
        ScalarUDF::from(Numeric::new("beta_i", 3, |a| said(special::beta_i(a[0], a[1], a[2])))),
    ]
}

/// A count, from a number a statement supplied.
///
/// Refuses a fraction rather than truncating it. `binom_pmf(2.7, 10, 0.5)` is not a question
/// with an answer, and truncating to two would answer a different one silently — which is the
/// class of wrong answer this whole system is arranged against.
fn whole(value: f64) -> Result<u64, String> {
    if value < 0.0 || value.fract() != 0.0 || !value.is_finite() {
        return Err(format!(
            "a count must be a whole number that is not negative, and `{value}` is not. \
             Refused rather than rounded: rounding answers a question about a different \
             number of events than the one asked about"
        ));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as u64)
}
