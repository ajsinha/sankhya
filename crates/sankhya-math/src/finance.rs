//! Money over time, and the risk measures taken from a distribution of outcomes.
//!
//! # Why the conventions are stated rather than assumed
//!
//! Almost every function here has two defensible definitions and a spreadsheet that picked one.
//! `NPV` discounts from period one or from period zero; a year fraction counts actual days or
//! thirty-day months; a value-at-risk is a quantile of losses or of returns, and is quoted
//! positive or negative.
//!
//! None of those is a matter of correctness and all of them change the number. So each is named
//! in the function's own documentation, and where this system's choice differs from a
//! spreadsheet's the function says so — because the person checking has the spreadsheet open.
//!
//! # Why an internal rate of return can fail
//!
//! It is a root of a polynomial, and a cash-flow sequence with more than one sign change can
//! have several. Returning the first one found would be answering a question that has more than
//! one answer, so a sequence with no sign change is refused outright and one that does not
//! converge is reported rather than approximated.

use crate::reduce::deterministic_sum;
use crate::vector::VectorError;

/// Net present value of cash flows at the end of each period.
///
/// # The convention, stated
///
/// Discounting begins at period **one**: the first flow is divided by `(1 + rate)`, not left
/// undiscounted. That is what Excel's `NPV` does, and it surprises people who expect the first
/// flow to be "today" — for that, add it separately or use [`net_present_value_from_now`].
///
/// # Errors
///
/// [`VectorError`] for a rate of exactly minus one, which divides by zero.
pub fn net_present_value(rate: f64, flows: &[f64]) -> Result<f64, VectorError> {
    // The guard is written on the divisor rather than on the rate: `1 + rate` is what
    // every period divides by, and it is zero exactly when the rate is minus one.
    if 1.0 + rate == 0.0 {
        return Err(VectorError::Refused(
            "a discount rate of minus one hundred per cent divides by zero in every period"
                .to_owned(),
        ));
    }
    let terms: Vec<f64> = flows
        .iter()
        .enumerate()
        .map(|(period, flow)| {
            #[allow(clippy::cast_precision_loss)]
            let exponent = period as i32 + 1;
            flow / (1.0 + rate).powi(exponent)
        })
        .collect();
    Ok(deterministic_sum(&terms))
}

/// Net present value with the **first flow undiscounted**.
///
/// The other convention, named rather than left as a flag. A boolean argument deciding which of
/// two definitions applies is a boolean somebody passes wrongly, and the resulting number is
/// plausible.
///
/// # Errors
///
/// [`VectorError`] as [`net_present_value`].
pub fn net_present_value_from_now(rate: f64, flows: &[f64]) -> Result<f64, VectorError> {
    // The guard is written on the divisor rather than on the rate: `1 + rate` is what
    // every period divides by, and it is zero exactly when the rate is minus one.
    if 1.0 + rate == 0.0 {
        return Err(VectorError::Refused(
            "a discount rate of minus one hundred per cent divides by zero in every period"
                .to_owned(),
        ));
    }
    let terms: Vec<f64> = flows
        .iter()
        .enumerate()
        .map(|(period, flow)| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let exponent = period as i32;
            flow / (1.0 + rate).powi(exponent)
        })
        .collect();
    Ok(deterministic_sum(&terms))
}

/// The internal rate of return: the rate at which the present value is zero.
///
/// # Why this refuses more than it converges
///
/// It is a root of a polynomial in the discount factor, and a cash-flow sequence with several
/// sign changes has several roots — all of them correct, none of them *the* answer. So a
/// sequence with no sign change at all is refused (there is no root), and one that does not
/// converge is reported rather than having its last iterate returned.
///
/// Found by bisection on a bracketed interval rather than by Newton, for the reason every
/// inverse in this crate uses bisection: Newton's step near a flat region is enormous, and an
/// overshoot returns a rate that is wrong in a direction nobody checks.
///
/// # Errors
///
/// [`VectorError`] for a sequence with no sign change, or one where no rate in the searched
/// range brings the value to zero.
pub fn internal_rate_of_return(flows: &[f64]) -> Result<f64, VectorError> {
    let positive = flows.iter().any(|f| *f > 0.0);
    let negative = flows.iter().any(|f| *f < 0.0);
    if !(positive && negative) {
        return Err(VectorError::Refused(
            "an internal rate of return needs at least one inflow and one outflow. A sequence \
             of one sign has no rate at which its value is zero, and answering would be \
             inventing one"
                .to_owned(),
        ));
    }

    // The bracket, widened from a rate of zero. Bounded well below `-1`, where the discount
    // factor changes sign and the function is no longer monotone.
    let value = |rate: f64| net_present_value_from_now(rate, flows).unwrap_or(f64::NAN);
    let mut low = -0.999_999;
    let mut high = 10.0;
    let (mut at_low, mut at_high) = (value(low), value(high));

    let mut widened = 0;
    while at_low * at_high > 0.0 && widened < 60 {
        high *= 2.0;
        at_high = value(high);
        widened += 1;
    }
    if at_low * at_high > 0.0 {
        return Err(VectorError::Refused(
            "no rate between minus one and a very large one brings this sequence to zero. \
             Reported rather than answered with the nearest iterate, which would be a rate at \
             which the value is not zero"
                .to_owned(),
        ));
    }

    for _ in 0..200 {
        let middle = 0.5 * (low + high);
        let at_middle = value(middle);
        if at_low * at_middle <= 0.0 {
            high = middle;
        } else {
            low = middle;
            at_low = at_middle;
        }
    }
    Ok(0.5 * (low + high))
}

/// The present value of a level annuity.
///
/// # Errors
///
/// [`VectorError`] for a negative number of periods.
pub fn present_value(rate: f64, periods: f64, payment: f64, future: f64) -> Result<f64, VectorError> {
    if periods < 0.0 || !periods.is_finite() {
        return Err(VectorError::Refused(
            "a number of periods must not be negative".to_owned(),
        ));
    }
    if rate == 0.0 {
        // The limit as the rate approaches zero, taken rather than reached: the general form
        // divides by the rate, and a zero rate is an ordinary case that a caller writes.
        return Ok(-(payment * periods + future));
    }
    let growth = (1.0 + rate).powf(periods);
    Ok(-(future + payment * (growth - 1.0) / rate) / growth)
}

/// The future value of a level annuity.
///
/// # Errors
///
/// [`VectorError`] as [`present_value`].
pub fn future_value(rate: f64, periods: f64, payment: f64, present: f64) -> Result<f64, VectorError> {
    if periods < 0.0 || !periods.is_finite() {
        return Err(VectorError::Refused(
            "a number of periods must not be negative".to_owned(),
        ));
    }
    if rate == 0.0 {
        return Ok(-(present + payment * periods));
    }
    let growth = (1.0 + rate).powf(periods);
    Ok(-(present * growth + payment * (growth - 1.0) / rate))
}

/// The level payment that amortises a present value over a number of periods.
///
/// # Errors
///
/// [`VectorError`] for a non-positive number of periods, which has no payment.
pub fn payment(rate: f64, periods: f64, present: f64, future: f64) -> Result<f64, VectorError> {
    if periods <= 0.0 || !periods.is_finite() {
        return Err(VectorError::Refused(
            "a payment needs at least one period to be spread over".to_owned(),
        ));
    }
    if rate == 0.0 {
        return Ok(-(present + future) / periods);
    }
    let growth = (1.0 + rate).powf(periods);
    Ok(-(present * growth + future) * rate / (growth - 1.0))
}

/// Straight-line depreciation.
///
/// # Errors
///
/// [`VectorError`] for a non-positive life.
pub fn straight_line(cost: f64, salvage: f64, life: f64) -> Result<f64, VectorError> {
    if life <= 0.0 {
        return Err(VectorError::Refused(
            "an asset must have a life of at least one period to be depreciated over"
                .to_owned(),
        ));
    }
    Ok((cost - salvage) / life)
}

/// Sum-of-years'-digits depreciation for one period.
///
/// # Errors
///
/// [`VectorError`] for a non-positive life, or a period outside it.
pub fn sum_of_years(cost: f64, salvage: f64, life: f64, period: f64) -> Result<f64, VectorError> {
    if life <= 0.0 {
        return Err(VectorError::Refused("an asset must have a positive life".to_owned()));
    }
    if period < 1.0 || period > life {
        return Err(VectorError::Refused(format!(
            "period {period} is outside an asset life of {life}"
        )));
    }
    Ok((cost - salvage) * (life - period + 1.0) * 2.0 / (life * (life + 1.0)))
}

// --- risk, from a distribution of outcomes --------------------------------

/// Historical value-at-risk: the quantile of the loss distribution.
///
/// # The convention, stated
///
/// `outcomes` are **profits and losses**, so a loss is negative. The result is the outcome at
/// the given tail probability and is therefore also **negative** for a loss — it is not flipped
/// to a positive "amount at risk".
///
/// That choice is deliberate: a value-at-risk quoted positive gets added to a profit somewhere,
/// and the sign is the only thing standing between a report and a number twice as wrong as it
/// looks.
///
/// # Errors
///
/// [`VectorError`] for an empty series, or a confidence outside `(0, 1)`.
pub fn value_at_risk(outcomes: &[f64], tail: f64) -> Result<f64, VectorError> {
    if outcomes.is_empty() {
        return Err(VectorError::Empty);
    }
    if !(tail > 0.0 && tail < 1.0) {
        return Err(VectorError::Refused(format!(
            "a tail probability must be between zero and one, and `{tail}` is not"
        )));
    }
    let mut sorted = outcomes.to_vec();
    crate::quantile(&mut sorted, tail, crate::Convention::LinearInterpolation)
        .map_err(|reason| VectorError::Refused(reason.to_string()))
}

/// Expected shortfall: the mean of the outcomes at or below the value-at-risk.
///
/// # Why this is reported beside the quantile rather than instead of it
///
/// A value-at-risk says how bad a day at the threshold is and says **nothing** about the days
/// beyond it — two series with the same quantile can have entirely different tails. The
/// shortfall is the average of what lies past it, and reporting only one of the pair is how a
/// tail risk goes unnoticed.
///
/// # Errors
///
/// [`VectorError`] as [`value_at_risk`], or when no outcome falls at or below the quantile.
pub fn expected_shortfall(outcomes: &[f64], tail: f64) -> Result<f64, VectorError> {
    let threshold = value_at_risk(outcomes, tail)?;
    let beyond: Vec<f64> = outcomes.iter().copied().filter(|v| *v <= threshold).collect();
    if beyond.is_empty() {
        return Err(VectorError::Refused(
            "no outcome falls at or below the value-at-risk, so there is no tail to average. \
             That happens when the tail probability is finer than the sample can resolve, and \
             it is a statement about the sample rather than about the risk"
                .to_owned(),
        ));
    }
    #[allow(clippy::cast_precision_loss)]
    Ok(deterministic_sum(&beyond) / beyond.len() as f64)
}

/// The Sharpe ratio: excess return per unit of its own deviation.
///
/// # Errors
///
/// [`VectorError`] for fewer than two returns, or returns with no variation --- an infinite
/// Sharpe does not mark an excellent series, it marks a series whose returns do not move.
pub fn sharpe(returns: &[f64], riskless: f64) -> Result<f64, VectorError> {
    if returns.len() < 2 {
        return Err(VectorError::Refused(
            "a Sharpe ratio needs at least two returns to have a deviation".to_owned(),
        ));
    }
    let excess: Vec<f64> = returns.iter().map(|r| r - riskless).collect();
    #[allow(clippy::cast_precision_loss)]
    let n = excess.len() as f64;
    let mean = deterministic_sum(&excess) / n;
    let squares: Vec<f64> = excess.iter().map(|v| (v - mean) * (v - mean)).collect();
    let deviation = (deterministic_sum(&squares) / (n - 1.0)).sqrt();
    if deviation == 0.0 {
        return Err(VectorError::Refused(
            "these returns have no deviation, so a Sharpe ratio would be infinite. Refused \
             rather than answered: an infinite Sharpe does not mark an excellent series, it \
             marks a series whose returns do not move"
                .to_owned(),
        ));
    }
    Ok(mean / deviation)
}

/// The Sortino ratio: excess return per unit of **downside** deviation.
///
/// Offered beside the Sharpe because they disagree exactly where it matters. A strategy with
/// large upside moves is punished by the Sharpe for volatility that nobody minds, and the
/// Sortino counts only the deviation below the target.
///
/// # Errors
///
/// [`VectorError`] for fewer than two returns, or none below the target --- in which case
/// there is no downside to measure and the ratio is undefined rather than infinite.
pub fn sortino(returns: &[f64], target: f64) -> Result<f64, VectorError> {
    if returns.len() < 2 {
        return Err(VectorError::Refused(
            "a Sortino ratio needs at least two returns".to_owned(),
        ));
    }
    let excess: Vec<f64> = returns.iter().map(|r| r - target).collect();
    #[allow(clippy::cast_precision_loss)]
    let n = excess.len() as f64;
    let mean = deterministic_sum(&excess) / n;

    let downside: Vec<f64> = excess.iter().filter(|v| **v < 0.0).map(|v| v * v).collect();
    if downside.is_empty() {
        return Err(VectorError::Refused(
            "no return fell below the target, so there is no downside deviation. Refused \
             rather than answered infinity: a series that has not yet fallen is not one of \
             unbounded quality"
                .to_owned(),
        ));
    }
    let deviation = (deterministic_sum(&downside) / n).sqrt();
    if deviation == 0.0 {
        return Err(VectorError::Refused("the downside deviation is zero".to_owned()));
    }
    Ok(mean / deviation)
}

/// The Black-Scholes price of a European call.
///
/// # Errors
///
/// [`VectorError`] for a non-positive spot, strike, volatility or time.
pub fn black_scholes_call(
    spot: f64,
    strike: f64,
    rate: f64,
    volatility: f64,
    years: f64,
) -> Result<f64, VectorError> {
    let (d1, d2) = black_scholes_terms(spot, strike, rate, volatility, years)?;
    Ok(spot * crate::distribution::norm_cdf(d1)
        - strike * (-rate * years).exp() * crate::distribution::norm_cdf(d2))
}

/// The Black-Scholes price of a European put.
///
/// # Errors
///
/// [`VectorError`] as [`black_scholes_call`].
pub fn black_scholes_put(
    spot: f64,
    strike: f64,
    rate: f64,
    volatility: f64,
    years: f64,
) -> Result<f64, VectorError> {
    let (d1, d2) = black_scholes_terms(spot, strike, rate, volatility, years)?;
    Ok(strike * (-rate * years).exp() * crate::distribution::norm_cdf(-d2)
        - spot * crate::distribution::norm_cdf(-d1))
}

/// The delta of a European call: how the price moves with the spot.
///
/// # Errors
///
/// [`VectorError`] as [`black_scholes_call`].
pub fn black_scholes_delta(
    spot: f64,
    strike: f64,
    rate: f64,
    volatility: f64,
    years: f64,
) -> Result<f64, VectorError> {
    let (d1, _) = black_scholes_terms(spot, strike, rate, volatility, years)?;
    Ok(crate::distribution::norm_cdf(d1))
}

/// The vega of a European option: how the price moves with volatility.
///
/// # Errors
///
/// [`VectorError`] as [`black_scholes_call`].
pub fn black_scholes_vega(
    spot: f64,
    strike: f64,
    rate: f64,
    volatility: f64,
    years: f64,
) -> Result<f64, VectorError> {
    let (d1, _) = black_scholes_terms(spot, strike, rate, volatility, years)?;
    Ok(spot * crate::distribution::norm_pdf(d1) * years.sqrt())
}

/// The two terms every Black-Scholes quantity is written in.
fn black_scholes_terms(
    spot: f64,
    strike: f64,
    rate: f64,
    volatility: f64,
    years: f64,
) -> Result<(f64, f64), VectorError> {
    if spot <= 0.0 || strike <= 0.0 {
        return Err(VectorError::Refused(
            "a spot and a strike must both be positive; the model is written in their \
             logarithm"
                .to_owned(),
        ));
    }
    if volatility <= 0.0 {
        return Err(VectorError::Refused(
            "a volatility must be positive. At zero the option is worth its intrinsic value \
             and the model divides by zero --- which is a different calculation, not this one \
             with a limit taken"
                .to_owned(),
        ));
    }
    if years <= 0.0 {
        return Err(VectorError::Refused(
            "an option with no time left is worth its intrinsic value, which this model does \
             not compute. Refused rather than answered zero"
                .to_owned(),
        ));
    }
    let root = volatility * years.sqrt();
    let d1 = ((spot / strike).ln() + (rate + 0.5 * volatility * volatility) * years) / root;
    Ok((d1, d1 - root))
}
