//! Statistics, checked against figures anyone can verify.
//!
//! The tests worth reading are the refusals and the numerical ones. A variance computed the
//! textbook one-pass way can come out **negative**, and a correlation against a constant
//! series has no answer that is not a lie.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_math::stats::{
    correlation, covariance, excess_kurtosis, linear_fit, mean, median, range, skewness,
    standard_deviation, standardise, variance, Population,
};
use sankhya_math::vector::VectorError;

fn close(got: f64, want: f64) {
    assert!((got - want).abs() < 1e-9, "{got} != {want}");
}

#[test]
fn the_basics_agree_with_the_textbook() {
    let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
    close(mean(&values).expect("non-empty"), 5.0);
    close(
        variance(&values, Population::Whole).expect("non-empty"),
        4.0,
    );
    close(
        standard_deviation(&values, Population::Whole).expect("non-empty"),
        2.0,
    );
    close(median(&values).expect("non-empty"), 4.5);
    assert_eq!(range(&values), Some((2.0, 9.0)));
}

#[test]
fn a_sample_variance_is_larger_than_a_population_one() {
    // The distinction is the divisor and it is a decision about what the data *is*. A
    // sample variance reported as a population variance understates the spread
    // systematically, and by more the smaller the sample.
    let values = [1.0, 2.0, 3.0, 4.0];
    let whole = variance(&values, Population::Whole).expect("non-empty");
    let sample = variance(&values, Population::Sample).expect("enough values");
    assert!(sample > whole);
    close(whole, 1.25);
    close(sample, 5.0 / 3.0);
}

#[test]
fn a_variance_computed_the_naive_way_would_have_come_out_negative_here() {
    // The reason for two passes. Values with a large mean and a tiny spread make the
    // one-pass identity E[x²] - E[x]² subtract two nearly equal large numbers, and
    // catastrophic cancellation can produce a negative variance — which is impossible, and
    // which every downstream square root turns into a NaN.
    let values = [1e9 + 4.0, 1e9 + 7.0, 1e9 + 13.0, 1e9 + 16.0];

    let naive = {
        let n = values.len() as f64;
        let sum: f64 = values.iter().sum();
        let sum_squares: f64 = values.iter().map(|x| x * x).sum();
        sum_squares / n - (sum / n) * (sum / n)
    };
    let ours = variance(&values, Population::Whole).expect("non-empty");

    close(ours, 22.5);
    assert!(ours > 0.0, "a variance cannot be negative");
    assert!(
        (naive - ours).abs() > 1e-3,
        "the fixture is not adversarial enough: the naive form gave {naive}, ours gave \
         {ours}, so this test proves nothing about why two passes are used"
    );
}

#[test]
fn variance_is_bit_identical_under_permutation() {
    let values: Vec<f64> = (0..301)
        .map(|i| 10f64.powi(i % 20 - 10) * f64::from(i % 7 + 1))
        .collect();
    let reference = variance(&values, Population::Sample).expect("enough values");

    // `(i * by) % n` is a permutation only when `by` and `n` are coprime — otherwise it
    // repeats values and the "permuted" array holds different data, which is a test that
    // proves nothing. 301 is 7 x 43, so these strides are all coprime with it.
    for by in [3usize, 11, 101, 200] {
        let n = values.len();
        assert_eq!(
            n, 301,
            "the strides below are chosen coprime with this length"
        );
        let permuted: Vec<f64> = (0..n).map(|i| values[(i * by + 5) % n]).collect();
        let mut check = permuted.clone();
        check.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut original = values.clone();
        original.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(
            check, original,
            "stride {by} is not a permutation of the input"
        );
        assert_eq!(
            variance(&permuted, Population::Sample)
                .expect("enough values")
                .to_bits(),
            reference.to_bits(),
            "permutation by {by} changed the variance"
        );
    }
}

#[test]
fn covariance_and_correlation_agree_on_a_perfect_line() {
    let x = [1.0, 2.0, 3.0, 4.0, 5.0];
    let doubled: Vec<f64> = x.iter().map(|v| v * 2.0).collect();
    close(correlation(&x, &doubled).expect("varying"), 1.0);

    let reversed: Vec<f64> = x.iter().map(|v| -v).collect();
    close(correlation(&x, &reversed).expect("varying"), -1.0);
    assert!(covariance(&x, &reversed, Population::Sample).expect("varying") < 0.0);
}

#[test]
fn a_correlation_against_a_constant_series_is_refused_rather_than_called_zero() {
    // A constant series has no correlation with anything — not zero, which would say
    // "unrelated", but undefined. Returning zero would place a constant column at a
    // definite relationship with every other one, and it would appear in a ranked
    // correlation table as genuinely uncorrelated rather than as unanswerable.
    let varying = [1.0, 2.0, 3.0, 4.0];
    let constant = [7.0, 7.0, 7.0, 7.0];
    assert_eq!(
        correlation(&varying, &constant),
        Err(VectorError::ZeroMagnitude)
    );
}

#[test]
fn skewness_and_kurtosis_have_the_signs_they_should() {
    // A long right tail is positive skew.
    let right_tailed = [1.0, 1.0, 1.0, 2.0, 2.0, 3.0, 10.0];
    assert!(skewness(&right_tailed).expect("enough values") > 0.0);

    let left_tailed: Vec<f64> = right_tailed.iter().map(|v| -v).collect();
    assert!(skewness(&left_tailed).expect("enough values") < 0.0);

    // A symmetric series has essentially none.
    let symmetric = [1.0, 2.0, 3.0, 4.0, 5.0];
    assert!(skewness(&symmetric).expect("enough values").abs() < 1e-9);
}

#[test]
fn kurtosis_is_excess_so_a_normal_distribution_reads_zero() {
    // Reporting raw kurtosis instead is a common and confusing choice: a reader seeing 3.0
    // cannot tell whether it means "normal" or "quite heavy-tailed" without knowing which
    // convention was used, and both are plausible.
    let heavy = [-10.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 10.0];
    let light = [-1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    assert!(
        excess_kurtosis(&heavy).expect("enough values")
            > excess_kurtosis(&light).expect("enough values"),
        "a series with outliers must have heavier tails than one without"
    );
    assert!(excess_kurtosis(&light).expect("enough values") < 0.0);
}

#[test]
fn too_few_values_for_a_shape_statistic_is_refused() {
    // Below three values there is no shape to describe, and the adjustment divides by
    // (n-1)(n-2).
    assert_eq!(skewness(&[1.0, 2.0]), Err(VectorError::Empty));
    assert_eq!(excess_kurtosis(&[1.0, 2.0, 3.0]), Err(VectorError::Empty));
    assert_eq!(
        variance(&[1.0], Population::Sample),
        Err(VectorError::Empty)
    );
    // But a population variance of one value is legitimately zero.
    close(variance(&[1.0], Population::Whole).expect("one value"), 0.0);
}

#[test]
fn standardising_gives_zero_mean_and_unit_variance() {
    let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
    let z = standardise(&values, Population::Whole).expect("varying");
    close(mean(&z).expect("non-empty"), 0.0);
    close(variance(&z, Population::Whole).expect("non-empty"), 1.0);
}

#[test]
fn standardising_a_constant_series_is_refused() {
    // Substituting zeroes would say every value is exactly average — true, useless, and
    // indistinguishable from real data.
    assert_eq!(
        standardise(&[3.0, 3.0, 3.0], Population::Whole),
        Err(VectorError::ZeroMagnitude)
    );
}

#[test]
fn a_least_squares_fit_recovers_a_line_it_was_given() {
    // y = 3x + 2, exactly.
    let x = [1.0, 2.0, 3.0, 4.0, 5.0];
    let y: Vec<f64> = x.iter().map(|v| 3.0 * v + 2.0).collect();
    let fit = linear_fit(&x, &y).expect("varying");

    close(fit.slope, 3.0);
    close(fit.intercept, 2.0);
    close(fit.r_squared, 1.0);
}

#[test]
fn a_noisy_fit_has_an_r_squared_below_one() {
    let x = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let y = [2.1, 3.9, 6.2, 7.8, 10.1, 11.9];
    let fit = linear_fit(&x, &y).expect("varying");
    assert!(
        fit.r_squared > 0.99 && fit.r_squared < 1.0,
        "{}",
        fit.r_squared
    );
    assert!((fit.slope - 2.0).abs() < 0.1);
}

#[test]
fn a_fit_through_a_single_x_value_is_refused() {
    // A vertical line has infinite slope, and a fit through one x is a claim about a
    // relationship the data cannot support.
    assert_eq!(
        linear_fit(&[3.0, 3.0, 3.0], &[1.0, 2.0, 3.0]),
        Err(VectorError::ZeroMagnitude)
    );
}

#[test]
fn a_median_of_two_enormous_values_does_not_overflow() {
    // Halving each and adding, rather than adding and halving: the sum of two values near
    // the representable maximum is infinity, and the median of two finite numbers is not.
    let huge = f64::MAX / 1.5;
    let got = median(&[huge, huge]).expect("non-empty");
    assert!(got.is_finite(), "the median overflowed to {got}");
    close(got, huge);
}

#[test]
fn an_empty_series_has_no_range_rather_than_a_zero_one() {
    // A minimum of nothing is not zero and not infinity, and both are values somebody would
    // act on.
    assert_eq!(range(&[]), None);
    assert_eq!(mean(&[]), Err(VectorError::Empty));
}
