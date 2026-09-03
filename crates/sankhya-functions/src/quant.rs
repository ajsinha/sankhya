//! Time series and finance, named on the SQL surface.
//!
//! # Why a rolling window returns a series with holes
//!
//! A rolling mean over ten observations with a window of thirty has no answer for the first
//! twenty-nine, and every plausible invention is wrong: zero is a number somebody acts on, the
//! series mean pretends to information that is not there, and repeating the first value makes a
//! flat start that reads as low volatility.
//!
//! So those positions arrive as **nulls inside the returned array**, which is what a
//! `List<Float64, nullable>` is for. A caller filtering them out is making a decision; a caller
//! handed a number would not know there was one to make.

#![allow(clippy::indexing_slicing)]

use crate::multi::{one, Multi};
use crate::property::Property;
use crate::series::Series;
use datafusion::logical_expr::ScalarUDF;
use sankhya_math::{finance, timeseries};

/// Turn a kernel's own refusal into the text a statement carries.
fn said<T>(outcome: Result<T, sankhya_math::vector::VectorError>) -> Result<T, String> {
    outcome.map_err(|error| error.to_string())
}

/// A window, from a number a statement supplied.
fn window(operands: &[Vec<f64>], at: usize) -> Result<usize, String> {
    let value = one(operands, at, "a window length")?;
    if value < 1.0 || value.fract() != 0.0 || !value.is_finite() {
        return Err(format!(
            "a window must be a whole number of observations, and `{value}` is not. Refused \
             rather than rounded: a rolling statistic over a different window is a different \
             statistic"
        ));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as usize)
}

/// Fill the positions a window does not reach.
///
/// The kernel returns `Option`s; the wire carries a `List` whose elements may be null. `NaN` is
/// used as the carrier here and turned back into a null by the builder --- see [`Series`]'s
/// note. A zero would be a number somebody acts on.
fn holes(values: Vec<Option<f64>>) -> Vec<f64> {
    values.into_iter().map(|value| value.unwrap_or(f64::NAN)).collect()
}

/// Every time-series and financial function.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn functions() -> Vec<ScalarUDF> {
    vec![
        // --- rolling statistics: a series and a window ---
        ScalarUDF::from(Multi::series("ts_rolling_mean", 2, |a| {
            let window = window(a, 1)?;
            said(timeseries::rolling_mean(&a[0], window)).map(holes)
        })),
        ScalarUDF::from(Multi::series("ts_rolling_std", 2, |a| {
            let window = window(a, 1)?;
            said(timeseries::rolling_deviation(&a[0], window)).map(holes)
        })),
        ScalarUDF::from(Multi::series("ts_rolling_min", 2, |a| {
            let window = window(a, 1)?;
            said(timeseries::rolling_min(&a[0], window)).map(holes)
        })),
        ScalarUDF::from(Multi::series("ts_rolling_max", 2, |a| {
            let window = window(a, 1)?;
            said(timeseries::rolling_max(&a[0], window)).map(holes)
        })),
        ScalarUDF::from(Multi::series("ts_ewma", 2, |a| {
            said(timeseries::ewma(&a[0], one(a, 1, "a smoothing factor")?))
        })),
        // --- returns, which are one shorter than their input ---
        ScalarUDF::from(Series::new("ts_returns", |values| {
            said(timeseries::simple_returns(values))
        })),
        ScalarUDF::from(Series::new("ts_log_returns", |values| {
            said(timeseries::log_returns(values))
        })),
        ScalarUDF::from(Series::new("ts_drawdown", |values| {
            said(timeseries::drawdown(values))
        })),
        ScalarUDF::from(Property::new("ts_max_drawdown", |values| {
            said(timeseries::max_drawdown(values))
        })),
        ScalarUDF::from(Property::new("ts_cumulative_return", |values| {
            said(timeseries::cumulative_return(values))
        })),
        ScalarUDF::from(Multi::new("ts_autocorrelation", 2, |a| {
            let lag = one(a, 1, "a lag")?;
            if lag < 0.0 || lag.fract() != 0.0 {
                return Err("a lag must be a whole number of observations".to_owned());
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            said(timeseries::autocorrelation(&a[0], lag as usize))
        })),
        // --- money over time ---
        //
        // Both discounting conventions are named. A boolean argument deciding which of two
        // definitions applies is a boolean somebody passes wrongly, and the number is plausible.
        ScalarUDF::from(Multi::new("npv", 2, |a| {
            said(finance::net_present_value(one(a, 0, "a rate")?, &a[1]))
        })),
        ScalarUDF::from(Multi::new("npv_from_now", 2, |a| {
            said(finance::net_present_value_from_now(one(a, 0, "a rate")?, &a[1]))
        })),
        ScalarUDF::from(Property::new("irr", |flows| {
            said(finance::internal_rate_of_return(flows))
        })),
        ScalarUDF::from(crate::scalar::Numeric::new("pv", 4, |a| {
            said(finance::present_value(a[0], a[1], a[2], a[3]))
        })),
        ScalarUDF::from(crate::scalar::Numeric::new("fv", 4, |a| {
            said(finance::future_value(a[0], a[1], a[2], a[3]))
        })),
        ScalarUDF::from(crate::scalar::Numeric::new("pmt", 4, |a| {
            said(finance::payment(a[0], a[1], a[2], a[3]))
        })),
        ScalarUDF::from(crate::scalar::Numeric::new("sln", 3, |a| {
            said(finance::straight_line(a[0], a[1], a[2]))
        })),
        ScalarUDF::from(crate::scalar::Numeric::new("syd", 4, |a| {
            said(finance::sum_of_years(a[0], a[1], a[2], a[3]))
        })),
        // --- risk, from a distribution of outcomes ---
        //
        // `outcomes` are profits and losses, so a loss is negative and the answer is negative
        // too. A value-at-risk quoted positive gets added to a profit somewhere.
        ScalarUDF::from(Multi::new("var_historical", 2, |a| {
            said(finance::value_at_risk(&a[0], one(a, 1, "a tail probability")?))
        })),
        ScalarUDF::from(Multi::new("expected_shortfall", 2, |a| {
            said(finance::expected_shortfall(&a[0], one(a, 1, "a tail probability")?))
        })),
        ScalarUDF::from(Multi::new("sharpe", 2, |a| {
            said(finance::sharpe(&a[0], one(a, 1, "a riskless rate")?))
        })),
        ScalarUDF::from(Multi::new("sortino", 2, |a| {
            said(finance::sortino(&a[0], one(a, 1, "a target return")?))
        })),
        // --- options ---
        ScalarUDF::from(crate::scalar::Numeric::new("black_scholes_call", 5, |a| {
            said(finance::black_scholes_call(a[0], a[1], a[2], a[3], a[4]))
        })),
        ScalarUDF::from(crate::scalar::Numeric::new("black_scholes_put", 5, |a| {
            said(finance::black_scholes_put(a[0], a[1], a[2], a[3], a[4]))
        })),
        ScalarUDF::from(crate::scalar::Numeric::new("greeks_delta", 5, |a| {
            said(finance::black_scholes_delta(a[0], a[1], a[2], a[3], a[4]))
        })),
        ScalarUDF::from(crate::scalar::Numeric::new("greeks_vega", 5, |a| {
            said(finance::black_scholes_vega(a[0], a[1], a[2], a[3], a[4]))
        })),
    ]
}
