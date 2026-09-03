//! Numerical differentiation and integration, checked against exact answers.
//!
//! Every test uses a function whose derivative or integral is known in closed form, so the
//! error being measured is the method's and not the fixture's.

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

use sankhya_math::calculus::{
    cumulative_integral, cumulative_sum, derivative, differences, integrate_simpson,
    integrate_trapezoid, second_derivative,
};
use sankhya_math::vector::VectorError;

/// `f(x) = x²` sampled on `[0, 1]`, whose integral is exactly 1/3 and derivative 2x.
fn parabola(samples: usize) -> (Vec<f64>, f64) {
    let n = samples - 1;
    #[allow(clippy::cast_precision_loss)]
    let spacing = 1.0 / n as f64;
    let values = (0..samples)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let x = i as f64 * spacing;
            x * x
        })
        .collect();
    (values, spacing)
}

#[test]
fn differences_are_one_shorter_and_that_is_deliberate() {
    // Padding to the original length invents a difference that was never observed, and it
    // sits at whichever end the padding chose.
    let out = differences(&[1.0, 4.0, 9.0, 16.0]).expect("enough values");
    assert_eq!(out, vec![3.0, 5.0, 7.0]);
    assert_eq!(differences(&[1.0]), Err(VectorError::Empty));
}

#[test]
fn a_derivative_recovers_the_slope_of_a_straight_line_exactly() {
    // Central differences are exact for anything linear, at every point including the ends.
    let line: Vec<f64> = (0..10).map(|i| 3.0 * f64::from(i) + 2.0).collect();
    for slope in derivative(&line, 1.0).expect("enough values") {
        assert!((slope - 3.0).abs() < 1e-12, "got {slope}");
    }
}

#[test]
fn a_derivative_of_x_squared_is_close_to_two_x() {
    let (values, spacing) = parabola(101);
    let slopes = derivative(&values, spacing).expect("enough values");

    // Interior points are second-order accurate; the ends are one-sided and less so, which
    // is a property of having no neighbour rather than a shortcoming.
    for (i, slope) in slopes.iter().enumerate().skip(1).take(99) {
        #[allow(clippy::cast_precision_loss)]
        let x = i as f64 * spacing;
        assert!((slope - 2.0 * x).abs() < 1e-9, "at x={x}: {slope}");
    }
}

#[test]
fn a_second_derivative_of_x_squared_is_close_to_two() {
    let (values, spacing) = parabola(101);
    let curvature = second_derivative(&values, spacing).expect("enough values");
    for value in &curvature {
        assert!((value - 2.0).abs() < 1e-6, "got {value}");
    }
}

#[test]
fn a_spacing_of_zero_is_refused_rather_than_producing_infinities() {
    // Refusing says which input was impossible. Infinities say only that something went
    // wrong somewhere.
    assert_eq!(
        derivative(&[1.0, 2.0, 3.0], 0.0),
        Err(VectorError::ZeroMagnitude)
    );
    assert_eq!(
        second_derivative(&[1.0, 2.0, 3.0], 0.0),
        Err(VectorError::ZeroMagnitude)
    );
}

#[test]
fn the_trapezoid_is_exact_for_a_straight_line() {
    // Exact for anything linear between samples, which is its defining property.
    let line: Vec<f64> = (0..=10).map(f64::from).collect();
    // The area under y = x from 0 to 10 is 50.
    let area = integrate_trapezoid(&line, 1.0).expect("enough values");
    assert!((area - 50.0).abs() < 1e-12, "got {area}");
}

#[test]
fn simpson_beats_the_trapezoid_on_a_curve_at_the_same_spacing() {
    // Fourth-order against second-order. This is the whole reason both are offered.
    let (values, spacing) = parabola(101);
    let exact = 1.0 / 3.0;

    let trapezoid = (integrate_trapezoid(&values, spacing).expect("enough") - exact).abs();
    let simpson = (integrate_simpson(&values, spacing).expect("enough") - exact).abs();

    assert!(
        simpson < trapezoid,
        "Simpson ({simpson}) should beat the trapezoid ({trapezoid})"
    );
    assert!(
        simpson < 1e-12,
        "and be essentially exact for a parabola: {simpson}"
    );
}

#[test]
fn simpson_refuses_an_even_sample_count_rather_than_dropping_one() {
    // Silently dropping the last sample or falling back to the trapezoid both change the
    // answer, and neither says so. The refusal names the count that would work.
    let (values, spacing) = parabola(100); // 100 samples, 99 intervals — odd, so invalid
    let Err(error) = integrate_simpson(&values, spacing) else {
        panic!("Simpson's rule needs an even number of intervals");
    };
    // The **rendered** message, not the variant. This asserted the two fields of a
    // `LengthMismatch` and called it "the message says what would work" --- and the message it
    // rendered was "cannot combine vectors of length 100 and 101", about a second vector the
    // caller never passed. A test that reads the fields cannot see that.
    let said = error.to_string();
    assert!(
        said.contains("odd number of samples") && said.contains("101 samples would work"),
        "the refusal must say what is wrong and what would work: {said}"
    );
    assert!(
        !said.contains("combine"),
        "and must not describe combining two vectors, when one was passed: {said}"
    );
}

#[test]
fn a_running_total_agrees_with_summing_each_prefix_directly() {
    // The property this costs O(n²) to have. Accumulating forward is O(n) and gives a
    // different answer, which means a running total and a windowed total over the same
    // values would disagree — and somebody would eventually reconcile the two.
    let values: Vec<f64> = (0..60)
        .map(|i| 10f64.powi(i % 12 - 6) * f64::from(i % 5 + 1))
        .collect();
    let running = cumulative_sum(&values);

    for (k, total) in running.iter().enumerate() {
        let directly = sankhya_math::deterministic_sum(&values[..=k]);
        assert_eq!(
            total.to_bits(),
            directly.to_bits(),
            "the running total at {k} differs from summing that prefix directly"
        );
    }
}

#[test]
fn a_cumulative_integral_starts_at_zero_and_ends_at_the_whole_area() {
    let (values, spacing) = parabola(101);
    let running = cumulative_integral(&values, spacing).expect("enough values");

    assert_eq!(running.len(), values.len());
    assert_eq!(
        running.first().copied(),
        Some(0.0),
        "no area at the first point"
    );
    let whole = integrate_trapezoid(&values, spacing).expect("enough values");
    assert!(
        (running.last().copied().unwrap_or(0.0) - whole).abs() < 1e-12,
        "the last cumulative value must be the whole integral"
    );
}

#[test]
fn integration_needs_at_least_two_points() {
    assert_eq!(integrate_trapezoid(&[1.0], 1.0), Err(VectorError::Empty));
    assert_eq!(cumulative_integral(&[1.0], 1.0), Err(VectorError::Empty));
}
