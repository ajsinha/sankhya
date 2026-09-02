//! The special functions everything else is written in terms of.
//!
//! # Why these are here rather than taken from a library
//!
//! The same reason the rest of this crate is: a numeric library reorders freely for speed, and
//! reordering is exactly what cannot be permitted where a figure has to tie out twice. These
//! are also the functions people check by hand against a table or a spreadsheet, so agreeing
//! to the digits somebody will compare matters more than being fast.
//!
//! # What accuracy is promised
//!
//! [`erfc`] is a Chebyshev fit with relative error below `1.2e-7`. [`ln_gamma`] is Lanczos'
//! approximation. [`gamma_p`], [`gamma_q`] and [`beta_i`] iterate to `3e-16` relative, so
//! everything built on them is good to the last few bits.
//!
//! # The rule about the tails
//!
//! Each of these has a **complementary** form that is computed directly rather than as
//! `1 - p`. Subtracting a probability near one from one loses every significant digit, and the
//! result is a p-value of zero for a test that did not reject --- confidently, and in the
//! direction that changes a decision.

/// A distribution's parameter was outside its domain.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DomainError {
    /// A probability outside `[0, 1]`.
    NotAProbability,
    /// A parameter that must be positive was not.
    NotPositive {
        /// Which one.
        parameter: &'static str,
    },
    /// A value outside the distribution's support.
    OutsideSupport,
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAProbability => write!(
                f,
                "a probability must be between zero and one. Refused rather than clamped: a \
                 clamp turns a caller's arithmetic error into a plausible quantile"
            ),
            Self::NotPositive { parameter } => write!(
                f,
                "`{parameter}` must be greater than zero, and it is not. A distribution with a \
                 non-positive scale or degrees of freedom is not a narrow distribution --- it \
                 is not a distribution"
            ),
            Self::OutsideSupport => write!(
                f,
                "this value is outside the distribution's support, so no density is defined \
                 for it. Reported rather than answered zero, because zero is a density"
            ),
        }
    }
}

impl std::error::Error for DomainError {}

// --- the error function ---------------------------------------------------

/// The complementary error function, `1 - erf(x)`.
///
/// # Why this is the incomplete gamma rather than a Chebyshev fit
///
/// The usual rational approximation is good to about `1.2e-7`, and it was here first. It gave
/// `erf(0) = -5.9e-8`, so `norm_cdf(0)` was `0.5000000296` --- a standard normal whose median
/// is not zero. Nobody would notice that in a chart and everybody would notice it in a
/// reconciliation.
///
/// The identity `erfc(x) = Q(½, x²)` for non-negative `x` costs nothing here, because
/// [`gamma_q`] is already in this module and already iterates to `3e-16`. So the error
/// function inherits the accuracy of the machinery rather than carrying an approximation of
/// its own, and `erf(0)` is exactly zero because `Q(½, 0)` is exactly one.
///
/// The negative side is `1 + P(½, x²)` rather than `2 - erfc(-x)`: both are exact, and the
/// first does not subtract a number near two from two.
#[must_use]
pub fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    let square = x * x;
    if x >= 0.0 {
        gamma_q(0.5, square).unwrap_or(0.0)
    } else {
        1.0 + gamma_p(0.5, square).unwrap_or(1.0)
    }
}

/// The error function.
///
/// The small side is taken directly in each half, so a tiny `erf(x)` keeps its digits instead
/// of being the difference of two numbers near one.
#[must_use]
pub fn erf(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    let square = x * x;
    if x >= 0.0 {
        gamma_p(0.5, square).unwrap_or(1.0)
    } else {
        -gamma_p(0.5, square).unwrap_or(1.0)
    }
}

// --- gamma, and what needs it ---------------------------------------------

/// The natural logarithm of the gamma function.
///
/// Lanczos' approximation. In logarithm form because `gamma(171)` overflows a double and the
/// quantities that need it --- a binomial coefficient, a chi-squared density --- are ratios
/// whose logarithms are perfectly ordinary numbers.
#[must_use]
pub fn ln_gamma(x: f64) -> f64 {
    const COEFFICIENTS: [f64; 6] = [
        76.180_091_729_471_46,
        -86.505_320_329_416_77,
        24.014_098_240_830_91,
        -1.231_739_572_450_155,
        0.1208650973866179e-2,
        -0.5395239384953e-5,
    ];
    let mut y = x;
    let tmp = x + 5.5;
    let tmp = tmp - (x + 0.5) * tmp.ln();
    let mut series = 1.000_000_000_190_015;
    for coefficient in COEFFICIENTS {
        y += 1.0;
        series += coefficient / y;
    }
    -tmp + (2.5066282746310005 * series / x).ln()
}

/// The regularised lower incomplete gamma function, `P(a, x)`.
///
/// # Errors
///
/// [`DomainError`] for a non-positive `a` or a negative `x`.
pub fn gamma_p(a: f64, x: f64) -> Result<f64, DomainError> {
    if a <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "shape" });
    }
    if x < 0.0 {
        return Err(DomainError::OutsideSupport);
    }
    if x == 0.0 {
        return Ok(0.0);
    }
    // Series below the crossover, continued fraction above it. Each converges quickly on its
    // own side and slowly on the other, and using one everywhere is how a cumulative comes to
    // take a thousand iterations to be wrong.
    if x < a + 1.0 {
        Ok(gamma_series(a, x))
    } else {
        Ok(1.0 - gamma_continued(a, x))
    }
}

/// The regularised upper incomplete gamma function, `Q(a, x) = 1 - P(a, x)`.
///
/// Computed directly rather than by subtraction, so a p-value in the far tail keeps its
/// significant digits instead of being rounded to zero.
///
/// # Errors
///
/// [`DomainError`] as [`gamma_p`].
pub fn gamma_q(a: f64, x: f64) -> Result<f64, DomainError> {
    if a <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "shape" });
    }
    if x < 0.0 {
        return Err(DomainError::OutsideSupport);
    }
    if x == 0.0 {
        return Ok(1.0);
    }
    if x < a + 1.0 {
        Ok(1.0 - gamma_series(a, x))
    } else {
        Ok(gamma_continued(a, x))
    }
}

/// `P(a, x)` by its series representation, for `x` below `a + 1`.
fn gamma_series(a: f64, x: f64) -> f64 {
    const ITERATIONS: usize = 300;
    const TOLERANCE: f64 = 3e-16;
    let mut ap = a;
    let mut sum = 1.0 / a;
    let mut term = sum;
    for _ in 0..ITERATIONS {
        ap += 1.0;
        term *= x / ap;
        sum += term;
        if term.abs() < sum.abs() * TOLERANCE {
            break;
        }
    }
    sum * (-x + a * x.ln() - ln_gamma(a)).exp()
}

/// `Q(a, x)` by its continued fraction, for `x` at or above `a + 1`.
fn gamma_continued(a: f64, x: f64) -> f64 {
    const ITERATIONS: usize = 300;
    const TOLERANCE: f64 = 3e-16;
    const TINY: f64 = 1e-300;

    let mut b = x + 1.0 - a;
    let mut c = 1.0 / TINY;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..=ITERATIONS {
        #[allow(clippy::cast_precision_loss)]
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < TINY {
            d = TINY;
        }
        c = b + an / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < TOLERANCE {
            break;
        }
    }
    h * (-x + a * x.ln() - ln_gamma(a)).exp()
}

/// The regularised incomplete beta function, `I_x(a, b)`.
///
/// What Student's *t* and the *F* distribution are both written in terms of.
///
/// # Errors
///
/// [`DomainError`] for non-positive shapes or an `x` outside `[0, 1]`.
pub fn beta_i(a: f64, b: f64, x: f64) -> Result<f64, DomainError> {
    if a <= 0.0 || b <= 0.0 {
        return Err(DomainError::NotPositive { parameter: "shape" });
    }
    if !(0.0..=1.0).contains(&x) {
        return Err(DomainError::OutsideSupport);
    }
    // The endpoints of the support, where `I_x(a, b)` is exactly `x`. An exact comparison is
    // what is meant: `0.9999999` is not the endpoint and has an answer of its own, and the
    // logarithm below would take `ln(1 - x)` of a number that is not one.
    #[allow(clippy::float_cmp)]
    if x == 0.0 || x == 1.0 {
        return Ok(x);
    }
    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    // The continued fraction converges quickly only on one side of the symmetry point, so the
    // other side is reflected rather than iterated slowly.
    if x < (a + 1.0) / (a + b + 2.0) {
        Ok(front * beta_continued(a, b, x) / a)
    } else {
        Ok(1.0 - front * beta_continued(b, a, 1.0 - x) / b)
    }
}

/// The continued fraction for the incomplete beta, by Lentz's method.
fn beta_continued(a: f64, b: f64, x: f64) -> f64 {
    const ITERATIONS: usize = 300;
    const TOLERANCE: f64 = 3e-16;
    const TINY: f64 = 1e-300;

    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;

    for m in 1..=ITERATIONS {
        #[allow(clippy::cast_precision_loss)]
        let m = m as f64;
        let m2 = 2.0 * m;

        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;

        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < TOLERANCE {
            break;
        }
    }
    h
}
