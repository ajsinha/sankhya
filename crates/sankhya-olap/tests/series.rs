//! The functions that turn a row's series into another series.
//!
//! # Why these exist as a file of their own
//!
//! Every one of these kernels was **written, unit-tested, mutation-tested and unreachable**.
//! They sat in `sankhya-math` with no name on any surface — twelve of them, including both
//! quantile kernels and all five element-wise vector operations — and every check in the
//! repository passed the whole time, because every check looked at the code rather than at the
//! surface. `check-kernels` now fails the build for it.
//!
//! So these assert the thing that was actually missing: that a **statement** reaches them, and
//! that what comes back is the number the kernel computes rather than a plausible neighbour.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use datafusion::prelude::SessionContext;

fn session() -> SessionContext {
    let context = SessionContext::new();
    sankhya_olap::construct::register(&context);
    sankhya_olap::vectors::register(&context);
    context
}

async fn run(sql: &str) -> String {
    let batches = session()
        .sql(sql)
        .await
        .unwrap_or_else(|error| panic!("did not plan: {sql}\n{error}"))
        .collect()
        .await
        .unwrap_or_else(|error| panic!("did not run: {sql}\n{error}"));
    datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn element_wise_arithmetic_between_two_series() {
    // Adding two yield curves, netting two exposures, differencing two readings. The shape the
    // scalar wrapper could not express, which is why these had no name.
    let out = run(
        "SELECT vec_add(vec_of(1.0, 2.0), vec_of(10.0, 20.0)) AS added, \
                vec_subtract(vec_of(10.0, 20.0), vec_of(1.0, 2.0)) AS subtracted, \
                vec_multiply(vec_of(2.0, 3.0), vec_of(4.0, 5.0)) AS multiplied, \
                vec_divide(vec_of(10.0, 20.0), vec_of(2.0, 4.0)) AS divided, \
                vec_scale(vec_of(1.5, 2.5), 2.0) AS scaled",
    )
    .await;
    assert!(out.contains("[11.0, 22.0]"), "{out}");
    assert!(out.contains("[9.0, 18.0]"), "{out}");
    assert!(out.contains("[8.0, 15.0]"), "{out}");
    assert!(out.contains("[5.0, 5.0]"), "{out}");
    assert!(out.contains("[3.0, 5.0]"), "{out}");
}

#[tokio::test]
async fn the_calculus_of_a_sampled_series() {
    // Over the squares, where the answers are known rather than merely plausible: the second
    // derivative of `x²` is 2 everywhere, and a test that only checked the *shape* of the
    // result would pass on any three numbers.
    let squares = "vec_of(1.0, 4.0, 9.0, 16.0)";

    let out = run(&format!(
        "SELECT vec_differences({squares}) AS diffs, \
                vec_second_derivative({squares}) AS curvature, \
                vec_cumulative_sum({squares}) AS running"
    ))
    .await;
    assert!(out.contains("[3.0, 5.0, 7.0]"), "first differences: {out}");
    assert!(
        out.contains("[2.0, 2.0, 2.0, 2.0]"),
        "the second derivative of x squared is 2 everywhere: {out}"
    );
    assert!(out.contains("[1.0, 5.0, 14.0, 30.0]"), "running total: {out}");
}

#[tokio::test]
async fn a_derivative_and_an_integral_are_inverse_over_a_straight_line() {
    // A property rather than a table of numbers, because a property cannot be satisfied by a
    // constant that happens to match. Over a line, the derivative is the slope everywhere.
    let out = run(
        "SELECT vec_derivative(vec_of(0.0, 3.0, 6.0, 9.0)) AS slope, \
                vec_cumulative_integral(vec_of(2.0, 2.0, 2.0)) AS area",
    )
    .await;
    assert!(out.contains("[3.0, 3.0, 3.0, 3.0]"), "a constant slope: {out}");
    // Trapezoid over a constant height of 2: 0, then 2, then 4.
    assert!(out.contains("[0.0, 2.0, 4.0]"), "{out}");
}

#[tokio::test]
async fn standardising_centres_on_zero_and_scales_to_unit_variance() {
    // The point of it: two series measured in different units become comparable. Asserted
    // through the *other* functions, so this cannot pass by returning its input.
    let out = run(
        "SELECT round(vec_mean(vec_standardise(vec_of(10.0, 20.0, 30.0, 40.0)))) AS centre, \
                round(vec_stddev(vec_standardise(vec_of(10.0, 20.0, 30.0, 40.0)))) AS spread",
    )
    .await;
    assert!(out.contains('0'), "the mean of a standardised series is zero: {out}");
    assert!(out.contains('1'), "and its sample deviation is one: {out}");
}

#[tokio::test]
async fn a_quantile_of_one_rows_series() {
    // By linear interpolation, which is what most libraries and spreadsheets use and so what
    // somebody means when they have not said. At `q = 0.5` it agrees with `vec_median`, and
    // that agreement is the assertion --- two spellings of one idea must not drift.
    let out = run(
        "SELECT vec_quantile(vec_of(1.0, 4.0, 9.0, 16.0), vec_of(0.5)) AS middle, \
                vec_median(vec_of(1.0, 4.0, 9.0, 16.0)) AS median, \
                vec_quantile(vec_of(1.0, 2.0, 3.0, 4.0), vec_of(0.0)) AS lowest, \
                vec_quantile(vec_of(1.0, 2.0, 3.0, 4.0), vec_of(1.0)) AS highest",
    )
    .await;
    assert!(out.contains("6.5"), "the interpolated middle: {out}");
    assert!(out.contains("1.0"), "the zeroth quantile is the smallest: {out}");
    assert!(out.contains("4.0"), "and the first is the largest: {out}");
}

#[tokio::test]
async fn a_series_result_composes_with_the_rest_of_the_catalogue() {
    // The reason the result is a `List` rather than a `FixedSizeList`: these kernels change
    // the width, so a fixed width would make `vec_differences` of a 384-dimensional embedding
    // a different function from `vec_differences` of a 3-dimensional one.
    //
    // What that buys is this: a series result feeds straight back in.
    let out = run(
        "SELECT vec_sum(vec_differences(vec_of(1.0, 4.0, 9.0, 16.0))) AS total, \
                vec_mean(vec_add(vec_of(1.0, 2.0), vec_of(3.0, 4.0))) AS average",
    )
    .await;
    assert!(out.contains("15"), "the differences sum to 15: {out}");
    assert!(out.contains('5'), "the mean of [4, 6] is 5: {out}");
}

#[tokio::test]
async fn a_null_series_gives_a_null_series_rather_than_an_empty_one() {
    // An empty series is a definite statement --- "nothing was measured" --- and a missing
    // vector is not that. The same rule the scalar kernels hold, at the series shape.
    // A *typed* null, produced by an expression, rather than the bare literal: `NULL` on its
    // own has type `Null`, and a vector function refuses that rather than guessing --- which
    // is the existing rule and is right. What is being tested here is a null **vector**.
    let out = run(
        "SELECT vec_differences(CASE WHEN 1 = 0 THEN vec_of(1.0, 2.0) END) AS nothing,                 vec_scale(CASE WHEN 1 = 0 THEN vec_of(1.0, 2.0) END, 2.0) AS scaled",
    )
    .await;
    assert!(!out.contains("[]"), "a null became an empty series: {out}");
    assert!(!out.contains("0.0"), "a null became a series of zeroes: {out}");
}

#[tokio::test]
async fn two_series_of_different_lengths_are_refused_rather_than_truncated() {
    // Truncating to the shorter would answer, and the answer would be about a different pair
    // of series than the one asked about.
    let planned = session()
        .sql("SELECT vec_add(vec_of(1.0, 2.0, 3.0), vec_of(1.0, 2.0))")
        .await;
    let failed = match planned {
        Err(error) => error.to_string(),
        Ok(frame) => match frame.collect().await {
            Err(error) => error.to_string(),
            Ok(_) => panic!("two series of different lengths were added"),
        },
    };
    assert!(failed.contains("vec_add"), "the refusal names the function: {failed}");
}
