//! Series that are ordered in time, where the order is part of the meaning.
//!
//! # What separates these from [`crate::vector`]
//!
//! A vector's elements are a set with positions; a time series' elements are a *sequence*, and
//! reversing it changes what every one of these functions returns. A rolling mean of a reversed
//! series is not the reverse of its rolling mean, and a drawdown of a reversed series is a
//! different number entirely.
//!
//! That is why they are named separately rather than added to the vector module. A caller who
//! reaches for `ts_drawdown` has already asserted that their series is in time order, and a
//! caller who reaches for `vec_mean` has asserted nothing.
//!
//! # Why a window shorter than its own span reports rather than pads
//!
//! A rolling mean over ten observations with a window of thirty has **no** answer for the first
//! twenty-nine, and every plausible way to invent one is wrong: zero is a number somebody acts
//! on, the series mean pretends to information that is not there, and repeating the first value
//! makes a flat start that reads as low volatility. So the leading positions are `None`, and a
//! caller decides what to do about them.

use crate::reduce::deterministic_sum;
use crate::vector::VectorError;

/// A window that cannot be applied to the series it was given.
fn check_window(values: &[f64], window: usize) -> Result<(), VectorError> {
    if window == 0 {
        return Err(VectorError::Refused(
            "a rolling window must cover at least one observation. A window of zero is not a \
             narrow window --- it is a request for the statistic of nothing"
                .to_owned(),
        ));
    }
    if window > values.len() {
        return Err(VectorError::Refused(format!(
            "a window of {window} over {} observation(s) has no answer anywhere. Reported \
             rather than answered with nulls throughout: a column of nulls reads as missing \
             data, and this is a window that was never applicable",
            values.len()
        )));
    }
    Ok(())
}

/// The rolling mean, with `None` for the positions the window does not cover.
///
/// # Errors
///
/// [`VectorError`] for a window of zero or one longer than the series.
pub fn rolling_mean(values: &[f64], window: usize) -> Result<Vec<Option<f64>>, VectorError> {
    check_window(values, window)?;
    #[allow(clippy::cast_precision_loss)]
    let divisor = window as f64;
    Ok((0..values.len())
        .map(|at| {
            if at + 1 < window {
                return None;
            }
            values
                .get(at + 1 - window..=at)
                .map(|slice| deterministic_sum(slice) / divisor)
        })
        .collect())
}

/// The rolling sample deviation.
///
/// # Errors
///
/// [`VectorError`] for a window below two, or longer than the series.
pub fn rolling_deviation(values: &[f64], window: usize) -> Result<Vec<Option<f64>>, VectorError> {
    if window < 2 {
        return Err(VectorError::Refused(
            "a sample deviation needs at least two observations, so a rolling one needs a \
             window of at least two. A window of one has a deviation of zero everywhere, which \
             is a statement about the window rather than about the data"
                .to_owned(),
        ));
    }
    check_window(values, window)?;
    #[allow(clippy::cast_precision_loss)]
    let divisor = window as f64 - 1.0;
    Ok((0..values.len())
        .map(|at| {
            if at + 1 < window {
                return None;
            }
            let slice = values.get(at + 1 - window..=at)?;
            #[allow(clippy::cast_precision_loss)]
            let mean = deterministic_sum(slice) / window as f64;
            let squares: Vec<f64> = slice.iter().map(|v| (v - mean) * (v - mean)).collect();
            Some((deterministic_sum(&squares) / divisor).sqrt())
        })
        .collect())
}

/// The rolling smallest value.
///
/// # Errors
///
/// [`VectorError`] as [`rolling_mean`].
pub fn rolling_min(values: &[f64], window: usize) -> Result<Vec<Option<f64>>, VectorError> {
    check_window(values, window)?;
    Ok(rolling_extreme(values, window, f64::min))
}

/// The rolling largest value.
///
/// # Errors
///
/// [`VectorError`] as [`rolling_mean`].
pub fn rolling_max(values: &[f64], window: usize) -> Result<Vec<Option<f64>>, VectorError> {
    check_window(values, window)?;
    Ok(rolling_extreme(values, window, f64::max))
}

/// A rolling extreme, given the comparison.
fn rolling_extreme(
    values: &[f64],
    window: usize,
    choose: fn(f64, f64) -> f64,
) -> Vec<Option<f64>> {
    (0..values.len())
        .map(|at| {
            if at + 1 < window {
                return None;
            }
            let slice = values.get(at + 1 - window..=at)?;
            slice.iter().copied().reduce(choose)
        })
        .collect()
}

/// The exponentially weighted moving average.
///
/// # Why the smoothing factor rather than a span
///
/// A span and a half-life are both conveniences that reduce to `alpha`, and offering three
/// spellings of one parameter means three chances for a caller to believe they set one when
/// they set another. `alpha` is the quantity the recurrence actually uses.
///
/// # Errors
///
/// [`VectorError`] for an `alpha` outside `(0, 1]`, which is not a slow average --- it is not
/// an average.
pub fn ewma(values: &[f64], alpha: f64) -> Result<Vec<f64>, VectorError> {
    if !(alpha > 0.0 && alpha <= 1.0) {
        return Err(VectorError::Refused(format!(
            "the smoothing factor must be greater than zero and at most one, and `{alpha}` is \
             not. Outside that range the recurrence does not converge and the result is not an \
             average of anything"
        )));
    }
    if values.is_empty() {
        return Err(VectorError::Empty);
    }
    let mut out = Vec::with_capacity(values.len());
    let mut level = *values.first().unwrap_or(&0.0);
    out.push(level);
    for value in values.iter().skip(1) {
        level = alpha * value + (1.0 - alpha) * level;
        out.push(level);
    }
    Ok(out)
}

/// Simple returns: the proportional change from each observation to the next.
///
/// # Errors
///
/// [`VectorError`] for a series shorter than two, or one containing a zero --- a return from
/// zero is infinite, and reporting it as such puts an infinity into a total somebody will read.
pub fn simple_returns(values: &[f64]) -> Result<Vec<f64>, VectorError> {
    if values.len() < 2 {
        return Err(VectorError::Refused(
            "a return needs two observations to be a change between".to_owned(),
        ));
    }
    let mut out = Vec::with_capacity(values.len() - 1);
    for pair in values.windows(2) {
        let (previous, current) = (pair.first().copied().unwrap_or(0.0), pair.get(1).copied().unwrap_or(0.0));
        if previous == 0.0 {
            return Err(VectorError::Refused(
                "a return from zero is undefined, not infinite and not one hundred per cent. \
                 Refused rather than answered: an infinity in a series of returns propagates \
                 into every total taken from it"
                    .to_owned(),
            ));
        }
        out.push(current / previous - 1.0);
    }
    Ok(out)
}

/// Logarithmic returns.
///
/// # Why these are offered beside the simple ones
///
/// They add, where simple returns compound: the log return over a month is the sum of its
/// days', which is what makes them the ones to aggregate. They are also not interchangeable,
/// and a caller who sums simple returns has an answer that is close and wrong.
///
/// # Errors
///
/// [`VectorError`] for a series shorter than two, or one with a non-positive value --- the
/// logarithm of which is not a number.
pub fn log_returns(values: &[f64]) -> Result<Vec<f64>, VectorError> {
    if values.len() < 2 {
        return Err(VectorError::Refused(
            "a return needs two observations to be a change between".to_owned(),
        ));
    }
    let mut out = Vec::with_capacity(values.len() - 1);
    for pair in values.windows(2) {
        let (previous, current) =
            (pair.first().copied().unwrap_or(0.0), pair.get(1).copied().unwrap_or(0.0));
        if previous <= 0.0 || current <= 0.0 {
            return Err(VectorError::Refused(
                "a logarithmic return needs positive prices at both ends. Refused rather than \
                 answered `NaN`, which travels silently into every sum it reaches"
                    .to_owned(),
            ));
        }
        out.push((current / previous).ln());
    }
    Ok(out)
}

/// The drawdown at each point: how far below the running peak the series has fallen.
///
/// Reported as a **negative proportion**, so a fall of a fifth is `-0.2`. Signed rather than
/// absolute, because a drawdown that is reported positive gets summed with returns by somebody
/// eventually.
///
/// # Errors
///
/// [`VectorError::Empty`] for an empty series.
pub fn drawdown(values: &[f64]) -> Result<Vec<f64>, VectorError> {
    if values.is_empty() {
        return Err(VectorError::Empty);
    }
    let mut peak = f64::NEG_INFINITY;
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        if *value > peak {
            peak = *value;
        }
        out.push(if peak != 0.0 { value / peak - 1.0 } else { 0.0 });
    }
    Ok(out)
}

/// The worst drawdown the series reached.
///
/// # Errors
///
/// [`VectorError::Empty`] for an empty series.
pub fn max_drawdown(values: &[f64]) -> Result<f64, VectorError> {
    Ok(drawdown(values)?.into_iter().fold(0.0f64, f64::min))
}

/// The cumulative return of a series of period returns.
///
/// Compounded rather than summed, which is the whole difference between this and a total: a
/// gain of ten per cent followed by a loss of ten per cent is not zero.
///
/// # Errors
///
/// [`VectorError::Empty`] for an empty series.
pub fn cumulative_return(returns: &[f64]) -> Result<f64, VectorError> {
    if returns.is_empty() {
        return Err(VectorError::Empty);
    }
    Ok(returns.iter().fold(1.0, |total, r| total * (1.0 + r)) - 1.0)
}

/// The autocorrelation at a given lag.
///
/// # Errors
///
/// [`VectorError`] when the lag leaves fewer than two overlapping observations, or when the
/// series has no variation --- a correlation of a constant is undefined, not one.
pub fn autocorrelation(values: &[f64], lag: usize) -> Result<f64, VectorError> {
    if lag >= values.len() || values.len() - lag < 2 {
        return Err(VectorError::Refused(format!(
            "a lag of {lag} leaves too little of a {}-observation series to correlate",
            values.len()
        )));
    }
    #[allow(clippy::cast_precision_loss)]
    let n = values.len() as f64;
    let mean = deterministic_sum(values) / n;

    let centred: Vec<f64> = values.iter().map(|v| v - mean).collect();
    let squares: Vec<f64> = centred.iter().map(|v| v * v).collect();
    let variance = deterministic_sum(&squares);
    if variance == 0.0 {
        return Err(VectorError::Refused(
            "a series with no variation has no autocorrelation. Refused rather than answered \
             one: a constant is not perfectly correlated with itself, it is uncorrelated with \
             everything"
                .to_owned(),
        ));
    }
    let products: Vec<f64> = (lag..values.len())
        .filter_map(|at| Some(centred.get(at)? * centred.get(at - lag)?))
        .collect();
    Ok(deterministic_sum(&products) / variance)
}
