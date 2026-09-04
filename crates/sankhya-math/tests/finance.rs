//! Money over time, and risk from a distribution of outcomes.
//!
//! # Where the expected numbers come from
//!
//! Worked examples and spreadsheet results, because that is what somebody checking this will
//! have open. Where a convention could go either way — when discounting starts, which sign a
//! value-at-risk carries — the test asserts the choice **and** the alternative it is not, so a
//! change of convention fails here rather than in somebody's month-end.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::indexing_slicing
)]

use sankhya_math::finance::{
    black_scholes_call, black_scholes_delta, black_scholes_put, black_scholes_vega,
    expected_shortfall, future_value, internal_rate_of_return, net_present_value,
    net_present_value_from_now, payment, present_value, sharpe, sortino, straight_line,
    sum_of_years, value_at_risk,
};
use sankhya_math::timeseries::{
    autocorrelation, cumulative_return, drawdown, ewma, log_returns, max_drawdown, rolling_max,
    rolling_mean, rolling_min, simple_returns,
};

fn near(actual: f64, expected: f64, tolerance: f64) {
    assert!((actual - expected).abs() <= tolerance, "expected {expected}, got {actual}");
}

// --- money over time ------------------------------------------------------

#[test]
fn net_present_value_discounts_from_period_one_and_says_so() {
    // Excel's convention, and the one that surprises people. `NPV(0.1, 100)` is `100/1.1`, not
    // `100` --- so both are asserted, because a change of convention must fail here rather
    // than in somebody's reconciliation.
    near(net_present_value(0.1, &[100.0]).expect("a value"), 90.909_090_909, 1e-6);
    near(net_present_value_from_now(0.1, &[100.0]).expect("a value"), 100.0, 1e-12);

    // A worked example: three flows of 100 at ten per cent.
    near(
        net_present_value(0.1, &[100.0, 100.0, 100.0]).expect("a value"),
        248.685_199_098,
        1e-6,
    );
}

#[test]
fn an_internal_rate_of_return_is_the_rate_that_zeroes_the_value() {
    // A hundred out, then sixty for three years: about 36.3%.
    let flows = vec![-100.0, 60.0, 60.0, 60.0];
    let rate = internal_rate_of_return(&flows).expect("a rate");
    near(rate, 0.363_096_5, 1e-6);

    // The defining property, which a table of expected values does not check: at that rate the
    // present value is zero.
    near(net_present_value_from_now(rate, &flows).expect("a value"), 0.0, 1e-9);
}

#[test]
fn a_cash_flow_of_one_sign_has_no_internal_rate_and_is_refused() {
    // There is no rate at which a sequence of inflows is worth nothing, and returning the
    // nearest iterate would be a rate at which the value is not zero.
    assert!(internal_rate_of_return(&[100.0, 100.0, 100.0]).is_err());
    assert!(internal_rate_of_return(&[-100.0, -100.0]).is_err());

    let said = internal_rate_of_return(&[10.0, 20.0]).expect_err("no sign change").to_string();
    assert!(said.contains("one inflow and one outflow"), "{said}");
}

#[test]
fn an_annuity_agrees_with_itself_in_both_directions() {
    // The property that catches a sign or an exponent error in either function: a present value
    // taken forward is the future value.
    let (rate, periods, pmt) = (0.05, 10.0, -1000.0);
    let present = present_value(rate, periods, pmt, 0.0).expect("a value");
    near(present, 7721.734_929, 1e-5);

    // Taken forward with **no further payments**, a present value becomes its compounded self:
    // that is the identity between the two, and passing the payment again would compound the
    // annuity twice --- which is what the first version of this test did, and it asserted the
    // result was zero.
    let compounded = future_value(rate, periods, 0.0, -present).expect("a value");
    near(compounded, present * (1.0 + rate).powf(periods), 1e-6);

    // And the payment that amortises it is the one it was built from.
    near(payment(rate, periods, present, 0.0).expect("a payment"), pmt, 1e-6);
}

#[test]
fn a_zero_rate_is_an_ordinary_case_rather_than_a_division_by_zero() {
    // The general form divides by the rate, and a caller writes zero. The limit is taken
    // rather than reached.
    near(present_value(0.0, 5.0, -100.0, 0.0).expect("a value"), 500.0, 1e-12);
    near(future_value(0.0, 5.0, -100.0, 0.0).expect("a value"), 500.0, 1e-12);
    near(payment(0.0, 5.0, 500.0, 0.0).expect("a payment"), -100.0, 1e-12);
}

#[test]
fn depreciation_spreads_exactly_what_was_spent() {
    // Straight-line over five years, and the sum-of-years schedule over the same asset. Both
    // must total the depreciable amount --- which is the check a per-period expected value
    // would not make.
    let (cost, salvage, life) = (10_000.0, 1_000.0, 5.0);
    near(straight_line(cost, salvage, life).expect("a charge"), 1800.0, 1e-12);

    let total: f64 = (1..=5)
        .map(|period| sum_of_years(cost, salvage, life, f64::from(period)).expect("a charge"))
        .sum();
    near(total, cost - salvage, 1e-9);

    // And the schedule falls, which is what distinguishes it from the straight line.
    let first = sum_of_years(cost, salvage, life, 1.0).expect("a charge");
    let last = sum_of_years(cost, salvage, life, 5.0).expect("a charge");
    assert!(first > last, "{first} then {last}");

    assert!(sum_of_years(cost, salvage, life, 6.0).is_err(), "a period outside the life");
}

// --- risk -----------------------------------------------------------------

#[test]
fn a_value_at_risk_is_negative_for_a_loss_and_stays_that_way() {
    // The convention, asserted rather than assumed: a loss is negative and is **not** flipped
    // to a positive "amount at risk". A value-at-risk quoted positive gets added to a profit
    // somewhere, and the sign is the only thing standing between a report and a number twice
    // as wrong as it looks.
    let outcomes: Vec<f64> = (-50..50).map(f64::from).collect();
    let var = value_at_risk(&outcomes, 0.05).expect("a quantile");
    assert!(var < 0.0, "a five per cent tail of a symmetric distribution is a loss: {var}");
    near(var, -45.05, 0.1);

    // And the shortfall is worse than the quantile, always --- it is the mean of what lies
    // beyond it.
    let shortfall = expected_shortfall(&outcomes, 0.05).expect("a shortfall");
    assert!(shortfall < var, "the shortfall must be worse than the quantile: {shortfall} against {var}");
}

#[test]
fn two_distributions_with_one_quantile_have_different_tails() {
    // Why the shortfall is reported beside the quantile rather than instead of it. These two
    // agree at the fifth percentile and do not agree at all about what lies past it.
    let mild: Vec<f64> = (0..100).map(|i| if i < 5 { -10.0 } else { f64::from(i) }).collect();
    let severe: Vec<f64> =
        (0..100).map(|i| if i < 5 { -10.0 - f64::from(i) * 40.0 } else { f64::from(i) }).collect();

    let mild_var = value_at_risk(&mild, 0.05).expect("a quantile");
    let severe_var = value_at_risk(&severe, 0.05).expect("a quantile");
    near(mild_var, severe_var, 12.0);

    let mild_tail = expected_shortfall(&mild, 0.05).expect("a shortfall");
    let severe_tail = expected_shortfall(&severe, 0.05).expect("a shortfall");
    assert!(
        severe_tail < mild_tail - 50.0,
        "the tails differ by far more than the quantiles: {mild_tail} against {severe_tail}"
    );
}

#[test]
fn a_ratio_with_no_deviation_is_refused_rather_than_infinite() {
    // An infinite Sharpe does not mark an excellent series; it marks a series whose returns do
    // not move. And a Sortino with no losses is not one of unbounded quality.
    assert!(sharpe(&[0.01; 10], 0.0).is_err());
    assert!(sortino(&[0.01; 10], 0.0).is_err());

    // A real one is a finite number.
    let returns = vec![0.02, -0.01, 0.03, 0.01, -0.02, 0.04, 0.00, 0.02];
    let ratio = sharpe(&returns, 0.001).expect("a ratio");
    assert!(ratio.is_finite() && ratio > 0.0, "{ratio}");

    // The Sortino counts only the downside, so a series with the same mean and larger *upside*
    // scores better on it and worse on the Sharpe --- which is the whole reason both exist.
    let spiky = vec![0.02, -0.01, 0.20, 0.01, -0.02, 0.04, 0.00, 0.02];
    assert!(
        sortino(&spiky, 0.0).expect("a ratio") > sortino(&returns, 0.0).expect("a ratio"),
        "a larger upside must not be punished by the Sortino"
    );
}

#[test]
fn black_scholes_satisfies_put_call_parity() {
    // The identity every implementation must satisfy, and one that a table of expected prices
    // does not check: `call - put = spot - strike * exp(-rate * time)`.
    let (spot, strike, rate, volatility, years) = (100.0, 95.0, 0.05, 0.2, 0.5);
    let call = black_scholes_call(spot, strike, rate, volatility, years).expect("a price");
    let put = black_scholes_put(spot, strike, rate, volatility, years).expect("a price");

    near(call - put, spot - strike * (-rate * years).exp(), 1e-9);
    // Checked by hand: d1 = 0.6102, d2 = 0.4688, N(d1) = 0.7291, N(d2) = 0.6804, so the call
    // is 100(0.7291) - 95 e^(-0.025)(0.6804) = 9.873. The first version of this test asserted
    // 10.4165 from memory and the implementation was right.
    near(call, 9.872_742, 1e-5);

    // A delta is a probability-like quantity between zero and one for a call, and a vega is
    // positive: an option is always worth more when the world is less certain.
    let delta = black_scholes_delta(spot, strike, rate, volatility, years).expect("a delta");
    assert!((0.0..=1.0).contains(&delta), "{delta}");
    assert!(black_scholes_vega(spot, strike, rate, volatility, years).expect("a vega") > 0.0);
}

#[test]
fn an_option_with_no_time_or_no_volatility_is_refused_rather_than_priced_at_zero() {
    // Both are a *different* calculation --- the intrinsic value --- and returning zero from
    // this one would be a price nobody could act on.
    assert!(black_scholes_call(100.0, 95.0, 0.05, 0.2, 0.0).is_err());
    assert!(black_scholes_call(100.0, 95.0, 0.05, 0.0, 0.5).is_err());
    assert!(black_scholes_call(-1.0, 95.0, 0.05, 0.2, 0.5).is_err());
}

// --- time series ----------------------------------------------------------

#[test]
fn a_rolling_window_reports_nothing_where_it_does_not_reach() {
    // Zero is a number somebody acts on; the series mean pretends to information that is not
    // there; repeating the first value makes a flat start that reads as low volatility. So the
    // leading positions are `None`.
    let values: Vec<f64> = (1..=6).map(f64::from).collect();
    let rolled = rolling_mean(&values, 3).expect("a rolling mean");

    assert_eq!(rolled[0], None);
    assert_eq!(rolled[1], None);
    near(rolled[2].expect("a value"), 2.0, 1e-12);
    near(rolled[5].expect("a value"), 5.0, 1e-12);

    assert_eq!(rolling_min(&values, 3).expect("a min")[2], Some(1.0));
    assert_eq!(rolling_max(&values, 3).expect("a max")[2], Some(3.0));

    // A window that never reaches is reported, not answered with nulls throughout: a column of
    // nulls reads as missing data, and this is a window that was never applicable.
    assert!(rolling_mean(&values, 20).is_err());
    assert!(rolling_mean(&values, 0).is_err());
}

#[test]
fn returns_compound_and_logarithms_add() {
    // The distinction that makes both worth having: a gain of ten per cent followed by a loss
    // of ten per cent is **not** zero, and summing simple returns says it is.
    let prices = vec![100.0, 110.0, 99.0];
    let simple = simple_returns(&prices).expect("returns");
    near(simple[0], 0.1, 1e-12);
    near(simple[1], -0.1, 1e-12);

    let summed: f64 = simple.iter().sum();
    near(summed, 0.0, 1e-12);
    near(cumulative_return(&simple).expect("a total"), -0.01, 1e-12);

    // Log returns add to the truth.
    let logs = log_returns(&prices).expect("returns");
    near(logs.iter().sum::<f64>(), (99.0f64 / 100.0).ln(), 1e-12);

    // A return from zero is undefined, not infinite, and an infinity propagates into every
    // total taken from the series.
    assert!(simple_returns(&[0.0, 10.0]).is_err());
    assert!(log_returns(&[-1.0, 10.0]).is_err());
}

#[test]
fn a_drawdown_is_measured_from_the_running_peak_and_is_negative() {
    let series = vec![100.0, 120.0, 90.0, 110.0, 80.0];
    let falls = drawdown(&series).expect("drawdowns");

    near(falls[0], 0.0, 1e-12);
    near(falls[1], 0.0, 1e-12);
    near(falls[2], -0.25, 1e-12);
    // From the peak of 120, not from the local high of 110.
    near(falls[4], -1.0 / 3.0, 1e-12);

    near(max_drawdown(&series).expect("the worst"), -1.0 / 3.0, 1e-12);
    assert!(falls.iter().all(|v| *v <= 0.0), "a drawdown is never positive: {falls:?}");
}

#[test]
fn an_exponential_average_weights_the_recent_and_starts_where_the_series_does() {
    let values = vec![10.0, 20.0, 30.0, 40.0];
    let smooth = ewma(&values, 0.5).expect("an average");

    near(smooth[0], 10.0, 1e-12);
    near(smooth[1], 15.0, 1e-12);
    near(smooth[2], 22.5, 1e-12);

    // At an alpha of one it is the series itself, which is the boundary the recurrence must
    // reach exactly.
    let sharp = ewma(&values, 1.0).expect("an average");
    for (a, b) in sharp.iter().zip(&values) {
        near(*a, *b, 1e-12);
    }

    // Outside `(0, 1]` the recurrence does not converge, and the result is not an average.
    assert!(ewma(&values, 0.0).is_err());
    assert!(ewma(&values, 1.5).is_err());
}

#[test]
fn autocorrelation_is_one_at_no_lag_and_falls_away() {
    let series: Vec<f64> = (0..40).map(|i| f64::from(i).sin()).collect();
    near(autocorrelation(&series, 0).expect("a correlation"), 1.0, 1e-9);

    let lagged = autocorrelation(&series, 1).expect("a correlation");
    assert!(lagged.abs() <= 1.000_001, "outside its range: {lagged}");

    // A constant has no autocorrelation. Refused rather than answered one: a constant is not
    // perfectly correlated with itself, it is uncorrelated with everything.
    assert!(autocorrelation(&[5.0; 10], 1).is_err());
    assert!(autocorrelation(&series, 40).is_err());
}

#[test]
fn a_long_flow_sequence_gives_the_rate_it_solves_for_or_refuses() {
    // `COR-07`, at the length the audit ran. A hundred flows --- ninety-eight monthly
    // receipts between an outlay and a decommissioning cost --- is an ordinary project, and
    // any monthly series over five years is long enough.
    //
    // Near a rate of minus one the discount factor underflows: at `-0.999999` and period 55
    // it is `1e-330`, which is zero, so a flow divided by it is an infinity and flows of both
    // signs give a `NaN`. `NaN` compares false against everything, so neither the "no rate
    // brings this to zero" refusal nor the bracket test inside the search could fire, and the
    // search marched to the top of the interval and returned it: `Ok(10.0)`, a rate of a
    // thousand per cent, where the value at that rate is `-989`.
    let mut flows = vec![-1000.0];
    flows.extend(std::iter::repeat_n(110.0, 98));
    flows.push(-500.0);

    match internal_rate_of_return(&flows) {
        Ok(rate) => {
            // Whatever it returns, it must solve the equation it is defined by.
            let at = net_present_value_from_now(rate, &flows).expect("a value");
            let scale = flows.iter().map(|f| f.abs()).fold(0.0f64, f64::max);
            assert!(
                at.abs() <= scale * 1e-6,
                "returned a rate of {rate} at which the sequence is worth {at}, not zero"
            );
            // And it must be the rate a person would recognise, not merely *a* root.
            assert!(
                (0.10..0.12).contains(&rate),
                "the true rate is about eleven per cent; got {rate}"
            );
        }
        Err(refusal) => panic!("a solvable sequence was refused: {refusal}"),
    }
}

#[test]
fn a_rate_that_does_not_zero_the_sequence_is_refused_rather_than_returned() {
    // The guard stated directly. Bisection converges on something whatever it is given, so
    // the answer is checked against the question before it is returned.
    let flows = vec![-1.0, 0.0, 0.0, 2.0];
    let rate = internal_rate_of_return(&flows).expect("a rate");
    let at = net_present_value_from_now(rate, &flows).expect("a value");
    assert!(at.abs() <= 1e-6, "the returned rate leaves {at} on the table");
}

#[test]
fn a_drawdown_against_a_non_positive_peak_is_refused() {
    // `COR-09`. `value / peak - 1.0` against a negative peak flips the sign, so `[-100, -200]`
    // reported `[0.0, 1.0]` --- a *positive* drawdown for a series that doubled its loss,
    // under a docstring promising a negative proportion. A peak of zero returned `0.0`, so a
    // series that only ever fell had a maximum drawdown of nothing.
    //
    // Both are the same mistake: a proportion of a non-positive base is a different question,
    // and answering it with a number is worse than declining.
    assert!(
        drawdown(&[-100.0, -200.0]).is_err(),
        "a proportion was reported against a negative peak"
    );
    assert!(
        max_drawdown(&[0.0, -50.0, -100.0]).is_err(),
        "a series that only fell reported no drawdown at all"
    );

    // A cumulative profit-and-loss curve crossing zero is the ordinary input this happens on.
    assert!(drawdown(&[10.0, 5.0, -5.0, -20.0]).is_ok(), "a positive peak still works");
    assert!(drawdown(&[-5.0, 10.0]).is_err(), "the peak is negative until the second point");
}
