//! Tests and regression, against answers that can be checked by hand or by property.
//!
//! # Where the expected numbers come from
//!
//! Textbook worked examples, so somebody can find the same figures elsewhere. Where no worked
//! example fits, a **property** is asserted instead --- a paired test on identical samples has
//! a statistic of zero, a regression on an exact line has residuals of zero --- because a
//! property cannot be satisfied by a consistently wrong implementation the way a round trip
//! can.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::indexing_slicing
)]

use sankhya_math::inference::{
    chisq_goodness, f_test, jarque_bera, ttest_one_sample, ttest_paired, ttest_two_sample,
    InferenceError,
};
use sankhya_math::regression::{least_squares, ridge, simple};

fn near(actual: f64, expected: f64, tolerance: f64) {
    assert!((actual - expected).abs() <= tolerance, "expected {expected}, got {actual}");
}

// --- t-tests --------------------------------------------------------------


/// The `R²` of a fixture whose response varies, which every fixture here does.
///
/// `LinearFit::r_squared` and `Fit::r_squared` are `Option` because `R²` is `0/0` for a
/// constant response, and the crate stopped answering that with a number. A test that supplies
/// varying data is entitled to say so out loud.
fn defined(value: Option<f64>) -> f64 {
    value.expect("this fixture's response varies, so R² is defined")
}

#[test]
fn a_one_sample_t_test_matches_a_worked_example() {
    // Ten observations with mean 5.0 and sample deviation about 1.15, tested against 4.0.
    let values = vec![5.1, 4.9, 5.6, 4.2, 5.4, 6.1, 4.4, 5.3, 4.8, 5.2];
    let result = ttest_one_sample(&values, 4.0).expect("a test");

    near(result.freedom, 9.0, 0.0);
    assert!(result.statistic > 3.0, "{result:?}");
    assert!(result.p_value < 0.01, "a mean a full point away should reject: {result:?}");

    // Against its own mean the statistic is exactly zero and the p-value exactly one, which is
    // the degenerate case a wrong sign or a wrong denominator does not reproduce.
    let mean = values.iter().sum::<f64>() / 10.0;
    let none = ttest_one_sample(&values, mean).expect("a test");
    near(none.statistic, 0.0, 1e-12);
    near(none.p_value, 1.0, 1e-12);
}

#[test]
fn welchs_test_does_not_assume_the_variances_match() {
    // Two samples with very different spreads. The pooled test would use a single variance and
    // reject too readily; Welch's degrees of freedom fall well below `n₁ + n₂ - 2`, which is
    // the visible signature of it being Welch's at all.
    let tight = vec![10.0, 10.1, 9.9, 10.2, 9.8, 10.0, 10.1, 9.9];
    let loose = vec![5.0, 25.0, -5.0, 35.0, 0.0, 30.0, -10.0, 40.0];

    let result = ttest_two_sample(&tight, &loose).expect("a test");
    assert!(
        result.freedom < 14.0,
        "the pooled test would have 14 degrees of freedom; Welch's has {}",
        result.freedom
    );
    assert!(result.freedom > 6.0, "{result:?}");
    assert!(result.p_value > 0.05, "these means are not distinguishable: {result:?}");

    // And on samples that genuinely differ, it rejects.
    let low = vec![1.0, 2.0, 1.5, 1.8, 2.2, 1.1];
    let high = vec![9.0, 10.0, 9.5, 9.8, 10.2, 9.1];
    assert!(ttest_two_sample(&low, &high).expect("a test").p_value < 1e-6);
}

#[test]
fn a_paired_test_refuses_samples_that_are_not_pairs() {
    // The mistake this test exists to be careful about: two unpaired samples given to a paired
    // test produce a number, and the number is a test of nothing.
    let four = vec![1.0, 2.0, 3.0, 4.0];
    let three = vec![1.0, 2.0, 3.0];
    assert_eq!(
        ttest_paired(&four, &three),
        Err(InferenceError::Unpaired { left: 4, right: 3 })
    );

    // Identical samples have a difference of exactly zero everywhere, which has no variation
    // --- and that is reported rather than returned as an infinite statistic.
    assert!(matches!(
        ttest_paired(&four, &four),
        Err(InferenceError::NoVariation { .. })
    ));

    // A real pairing: every observation rose by about two.
    let before = vec![10.0, 12.0, 9.0, 11.0, 13.0, 10.5];
    let after = vec![12.1, 13.9, 11.2, 12.8, 15.3, 12.4];
    let result = ttest_paired(&after, &before).expect("a test");
    assert!(result.statistic > 5.0, "{result:?}");
    assert!(result.p_value < 0.01, "{result:?}");
}

// --- chi-squared and F ----------------------------------------------------

#[test]
fn a_goodness_of_fit_test_matches_a_worked_example() {
    // A die rolled sixty times. The statistic is `Σ (o - e)²/e` = `(4+1+4+1+9+9)/10` = `2.8`,
    // which is worth writing out because the first version of this test asserted `5.2` and the
    // implementation was right.
    let observed = vec![8.0, 9.0, 12.0, 11.0, 7.0, 13.0];
    let expected = vec![10.0; 6];
    let result = chisq_goodness(&observed, &expected).expect("a test");

    near(result.statistic, 2.8, 1e-12);
    near(result.freedom, 5.0, 0.0);
    assert!(result.p_value > 0.3, "a fair die should not reject: {result:?}");

    // A loaded one does.
    let loaded = vec![2.0, 3.0, 4.0, 5.0, 6.0, 40.0];
    assert!(chisq_goodness(&loaded, &expected).expect("a test").p_value < 1e-10);
}

#[test]
fn a_zero_expected_count_is_reported_rather_than_divided_by() {
    // A cell nobody expects to be filled cannot contribute evidence, and pretending it
    // contributes infinity is not the same as it contributing a lot.
    let observed = vec![5.0, 5.0, 5.0];
    let expected = vec![5.0, 0.0, 10.0];
    assert_eq!(
        chisq_goodness(&observed, &expected),
        Err(InferenceError::ZeroExpected { at: 1 })
    );
}

#[test]
fn an_f_test_is_two_sided_so_a_smaller_variance_is_as_surprising_as_a_larger_one() {
    let wide = vec![1.0, 10.0, -8.0, 15.0, -12.0, 20.0, -15.0, 9.0];
    let narrow = vec![5.0, 5.1, 4.9, 5.2, 4.8, 5.05, 4.95, 5.0];

    let bigger_first = f_test(&wide, &narrow).expect("a test");
    let smaller_first = f_test(&narrow, &wide).expect("a test");

    // Both reject, and by construction the statistics are reciprocals. Reporting only the
    // upper tail would halve the p-value of the second, which is the same evidence.
    assert!(bigger_first.p_value < 0.001, "{bigger_first:?}");
    assert!(smaller_first.p_value < 0.001, "{smaller_first:?}");
    near(bigger_first.statistic * smaller_first.statistic, 1.0, 1e-9);
}

#[test]
fn jarque_bera_accepts_a_symmetric_sample_and_rejects_a_skewed_one() {
    // A symmetric sample has near-zero skewness, so the statistic is small.
    let symmetric: Vec<f64> = (-20..=20).map(f64::from).collect();
    let calm = jarque_bera(&symmetric).expect("a test");
    assert!(calm.p_value > 0.01, "a symmetric sample was called non-normal: {calm:?}");

    // One long tail, which is what the test is for.
    let mut skewed: Vec<f64> = vec![1.0; 60];
    skewed.extend([50.0, 80.0, 120.0, 200.0]);
    let alarmed = jarque_bera(&skewed).expect("a test");
    assert!(alarmed.p_value < 0.001, "a heavily skewed sample passed: {alarmed:?}");
    near(alarmed.freedom, 2.0, 0.0);
}

// --- regression -----------------------------------------------------------

#[test]
fn a_regression_on_an_exact_line_recovers_it_with_no_residual() {
    // `y = 3 + 2x` exactly. Every residual is zero, `R²` is one, and the coefficients are the
    // ones written down --- the case where any error at all is visible.
    let x: Vec<f64> = (1..=10).map(f64::from).collect();
    let y: Vec<f64> = x.iter().map(|v| 3.0 + 2.0 * v).collect();
    let fit = simple(&x, &y).expect("a fit");

    near(fit.coefficients[0], 3.0, 1e-9);
    near(fit.coefficients[1], 2.0, 1e-9);
    near(defined(fit.r_squared), 1.0, 1e-12);
    for residual in &fit.residuals {
        near(*residual, 0.0, 1e-9);
    }
}

#[test]
fn a_regression_reports_the_uncertainty_a_slope_is_useless_without() {
    // A relationship with noise. The slope alone looks like a finding; the standard error and
    // the p-value are what say whether it is one.
    let x: Vec<f64> = (1..=20).map(f64::from).collect();
    let y: Vec<f64> = x
        .iter()
        .enumerate()
        .map(|(i, v)| 5.0 + 1.5 * v + if i % 2 == 0 { 0.4 } else { -0.4 })
        .collect();
    let fit = simple(&x, &y).expect("a fit");

    // Near `1.5` and **not exactly** it, which is the honest statistical point rather than a
    // slack tolerance: the alternating noise is not orthogonal to `x`, so least squares
    // recovers `1.4940` and is right to. An implementation that returned exactly `1.5` here
    // would be fitting something other than these observations.
    near(fit.coefficients[1], 1.494, 1e-3);
    assert!(
        (fit.coefficients[1] - 1.5).abs() > 1e-6,
        "the estimate is not the true slope, and pretending otherwise hides the noise"
    );
    // The standard errors, pinned to the value rather than to a sign.
    //
    // These read `> 0.0` and `t > 8.0`, and the comment beside them reasoned about `t = 8.7`
    // on eighteen degrees of freedom. `8.7` was the **defective** figure: the diagonal of
    // `(X'X)^-1` needs the squared norm of a *row* of `R^-1` and the code took a *column*, so
    // the slope's standard error came out 10.5x too large and the intercept's 2.08x too small.
    // The trace of the two is identical, which is why nothing summing them noticed, and a
    // threshold of `> 8.0` passes on both 8.7 and the true 91.7 --- so the assertion written to
    // pin this quantity was the thing keeping the defect in place.
    //
    // Pinned to four significant figures now. Derived independently: `SE(b) = s / sqrt(Sxx)`
    // with `s = 0.42005` (residual standard error, 18 d.f.) and `Sxx = 665`.
    near(fit.standard_errors[0], 0.195_1, 1e-3);
    near(fit.standard_errors[1], 0.016_29, 1e-4);
    near(fit.t_statistics[1], 91.72, 0.05);
    assert!(
        fit.t_statistics[1] > 50.0,
        "a slope this clean cannot have a single-digit t-statistic: {:?}",
        fit.t_statistics
    );
    assert!(fit.p_values[1] < 1e-20, "{:?}", fit.p_values);
    near(fit.freedom, 18.0, 0.0);
    assert!(fit.residual_error > 0.0, "{}", fit.residual_error);
}

#[test]
fn a_slope_that_is_not_there_is_not_reported_as_significant() {
    // The direction that matters. `y` here is unrelated to `x`, and a fit that reported a
    // small p-value would be manufacturing a finding.
    let x: Vec<f64> = (1..=12).map(f64::from).collect();
    let y = vec![7.0, 3.0, 9.0, 4.0, 8.0, 2.0, 9.5, 3.5, 7.5, 4.5, 8.5, 3.0];
    let fit = simple(&x, &y).expect("a fit");

    assert!(fit.p_values[1] > 0.2, "an absent relationship was called significant: {fit:?}");
    assert!(defined(fit.r_squared) < 0.3, "{}", defined(fit.r_squared));
}

#[test]
fn the_adjusted_r_squared_falls_when_a_useless_predictor_is_added() {
    // Plain `R²` never falls when a predictor is added, including a predictor of noise, so
    // comparing models by it always prefers the larger one. The adjusted form is what makes
    // that comparison mean something.
    // **With noise.** The first version of this test fitted an exact line, so `R²` was one
    // either way and nothing could fall --- the assertion held for a model that had no
    // unexplained variance to penalise. A test that cannot fail, in the shape this repository
    // keeps finding.
    let rows = 15usize;
    let y: Vec<f64> = (0..rows)
        .map(|i| {
            let t = f64::from(u8::try_from(i).unwrap_or(0));
            2.0 + 3.0 * t + if i % 3 == 0 { 2.5 } else { -1.25 }
        })
        .collect();

    let x: Vec<f64> = (0..rows).map(|i| f64::from(u8::try_from(i).unwrap_or(0))).collect();
    let one = simple(&x, &y).expect("a fit");

    // The same model plus a column of alternating noise.
    let mut design = Vec::with_capacity(rows * 3);
    for (i, value) in x.iter().enumerate() {
        design.push(1.0);
        design.push(*value);
        design.push(if i % 2 == 0 { 1.0 } else { -1.0 });
    }
    let two = least_squares(&design, rows, 3, &y).expect("a fit");

    assert!(
        defined(two.r_squared) >= defined(one.r_squared) - 1e-12,
        "plain R-squared fell, which it cannot"
    );
    assert!(
        defined(two.adjusted_r_squared) < defined(one.adjusted_r_squared),
        "the adjusted form rewarded a useless predictor: {} then {}",
        defined(one.adjusted_r_squared),
        defined(two.adjusted_r_squared)
    );
    // And there is genuinely something to penalise, so the comparison means something.
    assert!(defined(one.r_squared) < 0.999, "the fixture has no residual variance: {}", defined(one.r_squared));
}

#[test]
fn a_model_with_more_parameters_than_data_is_refused_rather_than_fitted() {
    // Such a model fits perfectly and predicts nothing, and returning its coefficients would
    // present that as a result.
    let design = vec![1.0, 1.0, 2.0, 1.0, 2.0, 4.0];
    let y = vec![1.0, 2.0];
    assert!(matches!(
        least_squares(&design, 2, 3, &y),
        Err(InferenceError::TooFew { .. })
    ));
}

#[test]
fn ridge_shrinks_the_coefficients_of_a_collinear_design() {
    // Two nearly identical predictors. Unpenalised, the coefficients are large and opposite;
    // the penalty is what makes them stable.
    let rows = 12usize;
    let mut design = Vec::with_capacity(rows * 3);
    let mut y = Vec::with_capacity(rows);
    for i in 0..rows {
        let t = f64::from(u8::try_from(i).unwrap_or(0));
        design.push(1.0);
        design.push(t);
        // Collinear with the second column but for a whisker.
        design.push(t + if i % 2 == 0 { 1e-6 } else { -1e-6 });
        y.push(3.0 + 2.0 * t);
    }

    let plain = least_squares(&design, rows, 3, &y).expect("a fit");
    let (penalised, residuals) = ridge(&design, rows, 3, &y, 1.0).expect("a ridge fit");

    let plain_size: f64 = plain.coefficients.iter().map(|c| c * c).sum();
    let ridge_size: f64 = penalised.iter().map(|c| c * c).sum();
    assert!(
        ridge_size < plain_size,
        "the penalty did not shrink anything: {plain_size} then {ridge_size}"
    );

    // The residuals reported are the observations', not the penalty's --- reporting the
    // augmented rows would report the penalty as unexplained variance.
    assert_eq!(residuals.len(), rows);

    // A negative penalty rewards large coefficients, which is the opposite of the point.
    assert!(ridge(&design, rows, 3, &y, -1.0).is_err());
}

#[test]
fn a_regression_on_a_badly_conditioned_design_still_recovers_its_coefficients() {
    // Why the solve goes through QR rather than the normal equations.
    //
    // The design is `[1, t, t²]` over a range that makes the columns wildly different in
    // scale --- entirely ordinary when one predictor is a level and another is its square.
    // Forming `XᵀX` **squares** the condition number, and a design at `1e8` becomes `1e16`,
    // which a double cannot resolve at all: the coefficients come back looking plausible and
    // are noise.
    //
    // QR keeps the conditioning as it was, at about twice the arithmetic. This test is what
    // says the choice is still being made.
    let rows = 25usize;
    let mut design = Vec::with_capacity(rows * 3);
    let mut y = Vec::with_capacity(rows);
    for i in 0..rows {
        let t = 1000.0 + f64::from(u8::try_from(i).unwrap_or(0));
        design.push(1.0);
        design.push(t);
        design.push(t * t);
        // `y = 7 - 2t + 0.5t²`, exactly.
        y.push(7.0 - 2.0 * t + 0.5 * t * t);
    }

    let fit = least_squares(&design, rows, 3, &y).expect("a fit");

    // The quadratic term is the one that survives; the intercept is the one that does not,
    // when the conditioning has been thrown away.
    near(fit.coefficients[2], 0.5, 1e-9);
    near(fit.coefficients[1], -2.0, 1e-4);
    assert!(
        defined(fit.r_squared) > 1.0 - 1e-12,
        "an exact quadratic was not fitted exactly: R² = {}",
        defined(fit.r_squared)
    );
    for residual in &fit.residuals {
        assert!(residual.abs() < 1e-4, "a residual of {residual} on exact data");
    }
}

#[test]
fn a_small_f_statistic_reports_a_lower_tail_rather_than_zero() {
    // `COR-10`. The module header states the rule --- every tail comes from `chisq_sf`, `f_sf`
    // or `t_two_sided` rather than as `1 - cdf` --- and `f_test` took the lower tail as
    // `1.0 - upper`. A double near one has no bits below about `1e-16`, so every lower-tail
    // probability smaller than that was reported as **zero**: in the direction that makes a
    // finding look stronger than it is.
    //
    // A variance ratio this extreme is what a comparison of a near-constant series against a
    // volatile one produces, which is an ordinary thing to test.
    // A variance ratio of a twentieth over sixty observations each. Chosen so the true lower
    // tail is about `8e-24` --- small, and comfortably representable --- while `1.0 - upper`
    // is **exactly zero**, because `upper` has rounded to one. That is the whole of the
    // defect: not an approximation, a total loss.
    let spread = 20.0f64.sqrt();
    let tight: Vec<f64> = (0..60).map(|i| 100.0 + f64::from(i % 2)).collect();
    let loose: Vec<f64> = (0..60).map(|i| 100.0 + f64::from(i % 2) * spread).collect();

    let result = sankhya_math::inference::f_test(&tight, &loose).expect("a test");
    let upper = sankhya_math::distribution::f_sf(result.statistic, 59.0, 59.0).expect("upper");
    assert_eq!(
        1.0 - upper,
        0.0,
        "the fixture must be one where the subtraction loses everything, or this test would \
         pass against the defect it is about"
    );
    assert!(
        result.p_value > 0.0,
        "a p-value of exactly zero is the subtraction this module forbids, and it overstates \
         the finding"
    );
    assert!(
        result.p_value < 1e-20,
        "the lower tail is about 8e-24, so a two-sided p-value near 1.5e-23 is the answer: {}",
        result.p_value
    );
}

#[test]
fn the_f_test_is_symmetric_in_the_way_the_distribution_is() {
    // Swapping the samples inverts the statistic, and a two-sided p-value must not care.
    // This is the property the reciprocal identity buys, and the property `1 - upper` broke
    // asymmetrically: it was accurate on one side and not the other.
    let left: Vec<f64> = (0..40).map(|i| f64::from(i % 7)).collect();
    let right: Vec<f64> = (0..40).map(|i| f64::from(i % 3) * 4.0).collect();

    let forward = sankhya_math::inference::f_test(&left, &right).expect("a test");
    let backward = sankhya_math::inference::f_test(&right, &left).expect("a test");
    assert!(
        (forward.p_value - backward.p_value).abs() < 1e-12,
        "swapping the samples changed the two-sided p-value: {} against {}",
        forward.p_value,
        backward.p_value
    );
}

#[test]
fn a_regression_over_a_column_holding_a_nan_is_refused_rather_than_declared_significant() {
    // The most confident possible claim, made from the least information.
    //
    // `!t.is_finite()` is true of an infinity and of a NaN, and only one of them has a p-value
    // of zero. A coefficient over a zero standard error really is infinitely significant; a
    // coefficient that is NaN because one observation was carries no information at all, and
    // the p-value arm reported `0.0` for both.
    //
    // Nulls are filtered upstream. A NaN is not a null, and a column that has been through a
    // divide-by-zero in an upstream job carries them.
    let x: Vec<f64> = (1..=10).map(f64::from).collect();
    let mut y: Vec<f64> = x.iter().map(|v| 2.0 * v + 1.0).collect();
    y[4] = f64::NAN;

    assert!(
        simple(&x, &y).is_err(),
        "a regression over a NaN reported a fit rather than refusing"
    );

    // The same data without the NaN fits, so the refusal is about the value and not the shape.
    let clean: Vec<f64> = x.iter().map(|v| 2.0 * v + 1.0).collect();
    let fit = simple(&x, &clean).expect("an exact line");
    near(fit.coefficients[1], 2.0, 1e-9);
}
