//! Probability distributions: density, cumulative, survival and inverse.
//!
//! Every one of these is a consequence of [`crate::special`], and none approximates anything
//! on its own --- so their accuracy is that module's, which is the last few bits.
//!
//! # Every distribution offers its upper tail by name
//!
//! `sf` is not `1 - cdf`. A p-value of `1e-20` subtracted from one is zero, and a test
//! reporting `p = 0` where the truth is `1e-20` has thrown away the only digits anybody was
//! going to read. Where a distribution has a symmetry that gives the upper tail directly, that
//! is the route taken.
//!
//! # Every inverse is a bracketed root, not a Newton step
//!
//! Bisection with a bracket that is widened until it provably contains the answer. Newton
//! would be faster and its step is enormous where the density is near zero --- and an inverse
//! that overshoots into the far tail returns a critical value wrong in the direction of *not*
//! rejecting, which is the failure nobody notices, because the test simply says no.

// A probability of exactly zero or exactly one is a **boundary**, not a value near one, and
// the quantile at each is an infinity. Comparing "within a margin of error" would give the
// infinite answer for `0.9999999`, whose quantile is a perfectly ordinary number --- so the
// exact comparison is what is meant here, and the lint is asking for the wrong thing.
#![allow(clippy::float_cmp)]

use crate::special::{beta_i, erfc, gamma_p, gamma_q, ln_gamma, DomainError};
use std::f64::consts::{PI, SQRT_2};

/// How many halvings an inverse takes before it stops.
///
/// A hundred is far past the sixty or so a double needs, and the cost of the extra iterations
/// is nothing next to being asked why a critical value has four correct digits.
const HALVINGS: usize = 200;

// --- the normal distribution ----------------------------------------------

/// The standard normal density at `x`.
#[must_use]
pub fn norm_pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() / (2.0 * PI).sqrt()
}

/// The standard normal cumulative up to `x`.
///
/// Computed through [`erfc`] on the side that keeps its digits: for negative `x` the answer is
/// a small tail and is produced directly, and for positive `x` it is one minus a small tail,
/// which is exact because the subtraction is from one rather than of one.
#[must_use]
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / SQRT_2)
}

/// The value below which a standard normal falls with probability `p`.
///
/// # Errors
///
/// [`DomainError::NotAProbability`] outside `[0, 1]`. Zero and one give the infinities, which
/// are the honest answers rather than the largest representable numbers.
pub fn norm_inv(p: f64) -> Result<f64, DomainError> {
    if !(0.0..=1.0).contains(&p) || p.is_nan() {
        return Err(DomainError::NotAProbability);
    }
    if p == 0.0 {
        return Ok(f64::NEG_INFINITY);
    }
    if p == 1.0 {
        return Ok(f64::INFINITY);
    }

    // Acklam's rational approximation, then one Halley step. The approximation alone is good
    // to about `1.15e-9`; the refinement takes it to the last bit, which is what a caller
    // comparing against a spreadsheet is entitled to.
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_690e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    const LOW: f64 = 0.024_25;

    let mut x = if p < LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };

    // One Halley step against the cumulative, which is where the last digits come from.
    let error = norm_cdf(x) - p;
    let density = norm_pdf(x);
    if density > 0.0 {
        let u = error / density;
        x -= u / (1.0 + 0.5 * x * u);
    }
    Ok(x)
}

/// A normal with a mean and a standard deviation, rather than the standard one.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for a non-positive deviation.
pub fn normal_cdf(x: f64, mean: f64, deviation: f64) -> Result<f64, DomainError> {
    if deviation <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "deviation" });
    }
    Ok(norm_cdf((x - mean) / deviation))
}

/// The density of a normal with a mean and a standard deviation.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for a non-positive deviation.
pub fn normal_pdf(x: f64, mean: f64, deviation: f64) -> Result<f64, DomainError> {
    if deviation <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "deviation" });
    }
    Ok(norm_pdf((x - mean) / deviation) / deviation)
}

/// The quantile of a normal with a mean and a standard deviation.
///
/// # Errors
///
/// [`DomainError`] as [`norm_inv`], plus a non-positive deviation.
pub fn normal_inv(p: f64, mean: f64, deviation: f64) -> Result<f64, DomainError> {
    if deviation <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "deviation" });
    }
    Ok(mean + deviation * norm_inv(p)?)
}

/// The lognormal cumulative.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for a non-positive deviation.
pub fn lognormal_cdf(x: f64, mean: f64, deviation: f64) -> Result<f64, DomainError> {
    if deviation <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "deviation" });
    }
    // Zero and below carry no probability, and saying so is not the same as an error: a
    // lognormal is defined there, and its answer is zero.
    if x <= 0.0 {
        return Ok(0.0);
    }
    Ok(norm_cdf((x.ln() - mean) / deviation))
}

/// The lognormal quantile.
///
/// # Errors
///
/// [`DomainError`] as [`normal_inv`].
pub fn lognormal_inv(p: f64, mean: f64, deviation: f64) -> Result<f64, DomainError> {
    Ok(normal_inv(p, mean, deviation)?.exp())
}

// --- chi-squared ----------------------------------------------------------

/// The chi-squared cumulative with `freedom` degrees of freedom.
///
/// # Errors
///
/// [`DomainError`] for non-positive freedom, or a negative `x`.
pub fn chisq_cdf(x: f64, freedom: f64) -> Result<f64, DomainError> {
    if freedom <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    gamma_p(freedom / 2.0, x / 2.0)
}

/// The chi-squared **upper** tail, which is the p-value of a chi-squared test.
///
/// Computed directly rather than as `1 - cdf`, because a p-value of `1e-20` subtracted from
/// one is zero, and a test reporting `p = 0` where the truth is `1e-20` has thrown away the
/// only digits anybody was going to read.
///
/// # Errors
///
/// [`DomainError`] as [`chisq_cdf`].
pub fn chisq_sf(x: f64, freedom: f64) -> Result<f64, DomainError> {
    if freedom <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    gamma_q(freedom / 2.0, x / 2.0)
}

/// The chi-squared quantile.
///
/// # Errors
///
/// [`DomainError`] for a probability outside `[0, 1]` or non-positive freedom.
pub fn chisq_inv(p: f64, freedom: f64) -> Result<f64, DomainError> {
    if freedom <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    invert(p, 0.0, |x| chisq_cdf(x, freedom), || {
        // The bracket: the mean is `freedom` and the distribution is right-skewed, so
        // doubling from there reaches any quantile in a few steps and no guess is needed.
        freedom.max(1.0)
    })
}

// --- Student's t ----------------------------------------------------------

/// Student's *t* density.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for non-positive freedom.
pub fn t_pdf(x: f64, freedom: f64) -> Result<f64, DomainError> {
    if freedom <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    let half = (freedom + 1.0) / 2.0;
    let normaliser =
        (ln_gamma(half) - ln_gamma(freedom / 2.0) - 0.5 * (freedom * std::f64::consts::PI).ln())
            .exp();
    Ok(normaliser * (1.0 + x * x / freedom).powf(-half))
}

/// Student's *t* cumulative.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for non-positive freedom.
pub fn t_cdf(x: f64, freedom: f64) -> Result<f64, DomainError> {
    if freedom <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    let tail = 0.5 * beta_i(freedom / 2.0, 0.5, freedom / (freedom + x * x))?;
    // The symmetric halves, each computed on the side where it is a small number.
    Ok(if x >= 0.0 { 1.0 - tail } else { tail })
}

/// The two-sided p-value of a *t* statistic.
///
/// The quantity a *t*-test actually reports, and it is offered by name rather than left as
/// `2 * (1 - t_cdf(|t|))` --- which is the expression somebody writes and gets wrong by
/// forgetting the two, or by subtracting from one in the tail where that costs every digit.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for non-positive freedom.
pub fn t_two_sided(t: f64, freedom: f64) -> Result<f64, DomainError> {
    if freedom <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    beta_i(freedom / 2.0, 0.5, freedom / (freedom + t * t))
}

/// Student's *t* quantile.
///
/// # Errors
///
/// [`DomainError`] for a probability outside `[0, 1]` or non-positive freedom.
pub fn t_inv(p: f64, freedom: f64) -> Result<f64, DomainError> {
    if freedom <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    invert_symmetric(p, |x| t_cdf(x, freedom))
}

// --- the F distribution ---------------------------------------------------

/// The *F* cumulative with two degrees of freedom.
///
/// # Errors
///
/// [`DomainError`] for non-positive freedoms or a negative `x`.
pub fn f_cdf(x: f64, numerator: f64, denominator: f64) -> Result<f64, DomainError> {
    if numerator <= 0.0 || denominator <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    if x <= 0.0 {
        return Ok(0.0);
    }
    beta_i(
        numerator / 2.0,
        denominator / 2.0,
        numerator * x / (numerator * x + denominator),
    )
}

/// The *F* upper tail, which is the p-value of an *F* test.
///
/// # Errors
///
/// [`DomainError`] as [`f_cdf`].
pub fn f_sf(x: f64, numerator: f64, denominator: f64) -> Result<f64, DomainError> {
    if numerator <= 0.0 || denominator <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    if x <= 0.0 {
        return Ok(1.0);
    }
    // The complement taken through the beta's own symmetry rather than by subtraction, for
    // the reason `chisq_sf` gives.
    beta_i(
        denominator / 2.0,
        numerator / 2.0,
        denominator / (numerator * x + denominator),
    )
}

/// The *F* quantile.
///
/// # Errors
///
/// [`DomainError`] for a probability outside `[0, 1]` or non-positive freedoms.
pub fn f_inv(p: f64, numerator: f64, denominator: f64) -> Result<f64, DomainError> {
    if numerator <= 0.0 || denominator <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "freedom" });
    }
    invert(p, 0.0, |x| f_cdf(x, numerator, denominator), || 1.0)
}

// --- the discrete ones ----------------------------------------------------

/// The binomial probability of exactly `successes` in `trials`.
///
/// # Errors
///
/// [`DomainError`] for a probability outside `[0, 1]`, or more successes than trials.
pub fn binomial_pmf(successes: u64, trials: u64, probability: f64) -> Result<f64, DomainError> {
    if !(0.0..=1.0).contains(&probability) {
        return Err(DomainError::NotAProbability);
    }
    if successes > trials {
        return Err(DomainError::OutsideSupport);
    }
    #[allow(clippy::cast_precision_loss)]
    let (k, n) = (successes as f64, trials as f64);
    // In logarithms, because `choose(1000, 500)` overflows a double by three hundred orders of
    // magnitude while the probability it appears in is an ordinary small number.
    let ln_choose = ln_gamma(n + 1.0) - ln_gamma(k + 1.0) - ln_gamma(n - k + 1.0);
    let ln_p = if probability == 0.0 {
        if successes == 0 {
            return Ok(1.0);
        }
        return Ok(0.0);
    } else {
        k * probability.ln()
    };
    let ln_q = if probability == 1.0 {
        if successes == trials {
            return Ok(1.0);
        }
        return Ok(0.0);
    } else {
        (n - k) * (1.0 - probability).ln()
    };
    Ok((ln_choose + ln_p + ln_q).exp())
}

/// The binomial cumulative: the probability of at most `successes`.
///
/// # Errors
///
/// [`DomainError`] as [`binomial_pmf`].
pub fn binomial_cdf(successes: u64, trials: u64, probability: f64) -> Result<f64, DomainError> {
    if !(0.0..=1.0).contains(&probability) {
        return Err(DomainError::NotAProbability);
    }
    if successes >= trials {
        return Ok(1.0);
    }
    // Through the incomplete beta rather than by summing terms: the sum is `O(n)` and loses
    // accuracy for large `n`, and the identity is exact.
    #[allow(clippy::cast_precision_loss)]
    let (k, n) = (successes as f64, trials as f64);
    beta_i(n - k, k + 1.0, 1.0 - probability)
}

/// The Poisson probability of exactly `events`.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for a non-positive rate.
pub fn poisson_pmf(events: u64, rate: f64) -> Result<f64, DomainError> {
    if rate <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "rate" });
    }
    #[allow(clippy::cast_precision_loss)]
    let k = events as f64;
    Ok((k * rate.ln() - rate - ln_gamma(k + 1.0)).exp())
}

/// The Poisson cumulative: the probability of at most `events`.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for a non-positive rate.
pub fn poisson_cdf(events: u64, rate: f64) -> Result<f64, DomainError> {
    if rate <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "rate" });
    }
    #[allow(clippy::cast_precision_loss)]
    let k = events as f64;
    gamma_q(k + 1.0, rate)
}

// --- the simple continuous ones -------------------------------------------

/// The exponential cumulative.
///
/// # Errors
///
/// [`DomainError::NotPositive`] for a non-positive rate.
pub fn exponential_cdf(x: f64, rate: f64) -> Result<f64, DomainError> {
    if rate <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "rate" });
    }
    if x <= 0.0 {
        return Ok(0.0);
    }
    // `-expm1` rather than `1 - exp`, which loses every digit for a small `x`.
    Ok(-(-rate * x).exp_m1())
}

/// The gamma cumulative.
///
/// # Errors
///
/// [`DomainError`] for non-positive shape or scale.
pub fn gamma_cdf(x: f64, shape: f64, scale: f64) -> Result<f64, DomainError> {
    if scale <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "scale" });
    }
    gamma_p(shape, x / scale)
}

/// The beta cumulative.
///
/// # Errors
///
/// [`DomainError`] for non-positive shapes or an `x` outside `[0, 1]`.
pub fn beta_cdf(x: f64, a: f64, b: f64) -> Result<f64, DomainError> {
    beta_i(a, b, x)
}

/// The uniform cumulative on `[low, high]`.
///
/// # Errors
///
/// [`DomainError::NotPositive`] when the interval has no width.
pub fn uniform_cdf(x: f64, low: f64, high: f64) -> Result<f64, DomainError> {
    if high <= low {
        return Err(DomainError::NotPositive { parameter: "width" });
    }
    Ok(((x - low) / (high - low)).clamp(0.0, 1.0))
}

// --- inversion ------------------------------------------------------------

/// Invert a cumulative on `[floor, ∞)` by bisection.
///
/// `spread` gives a starting scale; the bracket doubles from there until it contains the
/// answer, which terminates for any cumulative that reaches one.
fn invert(
    p: f64,
    floor: f64,
    cdf: impl Fn(f64) -> Result<f64, DomainError>,
    spread: impl Fn() -> f64,
) -> Result<f64, DomainError> {
    if !(0.0..=1.0).contains(&p) || p.is_nan() {
        return Err(DomainError::NotAProbability);
    }
    if p == 0.0 {
        return Ok(floor);
    }
    if p == 1.0 {
        return Ok(f64::INFINITY);
    }

    let mut low = floor;
    let mut high = floor + spread();
    let mut widened = 0;
    while cdf(high)? < p {
        high = floor + (high - floor) * 2.0;
        widened += 1;
        if widened > HALVINGS {
            return Ok(high);
        }
    }
    for _ in 0..HALVINGS {
        let middle = 0.5 * (low + high);
        if cdf(middle)? < p {
            low = middle;
        } else {
            high = middle;
        }
    }
    Ok(0.5 * (low + high))
}

/// Invert a symmetric cumulative on the whole line.
fn invert_symmetric(
    p: f64,
    cdf: impl Fn(f64) -> Result<f64, DomainError>,
) -> Result<f64, DomainError> {
    if !(0.0..=1.0).contains(&p) || p.is_nan() {
        return Err(DomainError::NotAProbability);
    }
    if p == 0.0 {
        return Ok(f64::NEG_INFINITY);
    }
    if p == 1.0 {
        return Ok(f64::INFINITY);
    }
    if p == 0.5 {
        return Ok(0.0);
    }

    let mut low = -1.0f64;
    let mut high = 1.0f64;
    let mut widened = 0;
    while cdf(low)? > p {
        low *= 2.0;
        widened += 1;
        if widened > HALVINGS {
            return Ok(low);
        }
    }
    while cdf(high)? < p {
        high *= 2.0;
        widened += 1;
        if widened > HALVINGS {
            return Ok(high);
        }
    }
    for _ in 0..HALVINGS {
        let middle = 0.5 * (low + high);
        if cdf(middle)? < p {
            low = middle;
        } else {
            high = middle;
        }
    }
    Ok(0.5 * (low + high))
}
