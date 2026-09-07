//! The distributions, against values somebody can look up.
//!
//! # Why the assertions are published constants and not round trips
//!
//! A round trip proves a function is its own inverse, which a pair of consistently wrong
//! functions also satisfies. These are the numbers in a statistics table and in a spreadsheet,
//! because that is what somebody checking this will have open --- and a critical value that
//! disagrees with the table in the fourth digit is a test that rejects when it should not.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::float_cmp)]

use sankhya_math::distribution::{
    binomial_cdf, binomial_pmf, chisq_cdf, chisq_inv, chisq_sf, exponential_cdf, f_inv, f_sf, lognormal_cdf,
    norm_cdf, norm_inv, norm_pdf, poisson_cdf, poisson_pmf, t_cdf, t_inv, t_two_sided,
    uniform_cdf,
};
use sankhya_math::special::{beta_i, erf, erfc, gamma_p, gamma_q, ln_gamma};

/// Close enough that a person comparing against a printed table sees the same digits.
fn near(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected}, got {actual}, off by {}",
        (actual - expected).abs()
    );
}

// --- the normal -----------------------------------------------------------

#[test]
fn the_normal_cumulative_matches_the_table() {
    near(norm_cdf(0.0), 0.5, 1e-12);
    near(norm_cdf(1.0), 0.841_344_746, 1e-7);
    near(norm_cdf(1.96), 0.975_002_105, 1e-7);
    near(norm_cdf(2.575_829_304), 0.995, 1e-7);
    near(norm_cdf(-1.0), 0.158_655_254, 1e-7);
    // Symmetry, which is a property rather than a lookup and so cannot be satisfied by a
    // table that was transcribed wrongly.
    for x in [0.3, 1.1, 2.4, 3.7] {
        near(norm_cdf(x) + norm_cdf(-x), 1.0, 1e-12);
    }
}

#[test]
fn the_normal_quantile_matches_the_critical_values_everybody_knows() {
    // The four numbers that appear in every methods section.
    near(norm_inv(0.975).expect("a quantile"), 1.959_963_985, 1e-9);
    near(norm_inv(0.95).expect("a quantile"), 1.644_853_627, 1e-9);
    near(norm_inv(0.99).expect("a quantile"), 2.326_347_874, 1e-9);
    near(norm_inv(0.5).expect("a quantile"), 0.0, 1e-12);
}

#[test]
fn the_normal_quantile_inverts_its_own_cumulative_to_the_last_bits() {
    // The Halley refinement is what buys this. Without it the approximation alone is good to
    // about `1.15e-9`, which is fine for a chart and not for somebody reconciling.
    for p in [0.001, 0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99, 0.999] {
        let x = norm_inv(p).expect("a quantile");
        near(norm_cdf(x), p, 1e-9);
    }
}

#[test]
fn the_normal_density_integrates_to_one_over_the_bulk() {
    // A crude trapezoid over six deviations, which is enough to catch a normaliser that is
    // wrong by a constant --- the error a hand-written density most often has.
    let mut area = 0.0;
    let step = 0.001;
    let mut x = -6.0;
    while x < 6.0 {
        area += 0.5 * (norm_pdf(x) + norm_pdf(x + step)) * step;
        x += step;
    }
    near(area, 1.0, 1e-6);
}

#[test]
fn a_probability_outside_the_unit_interval_is_refused_rather_than_clamped() {
    // Clamping turns a caller's arithmetic error into a plausible quantile, which is the
    // wrong answer that looks most like a right one.
    assert!(norm_inv(-0.1).is_err());
    assert!(norm_inv(1.1).is_err());
    assert!(norm_inv(f64::NAN).is_err());

    // And the boundaries are the infinities, which are the honest answers.
    assert_eq!(norm_inv(0.0), Ok(f64::NEG_INFINITY));
    assert_eq!(norm_inv(1.0), Ok(f64::INFINITY));
}

// --- the special functions ------------------------------------------------

#[test]
fn the_error_function_matches_its_published_values() {
    near(erf(0.0), 0.0, 1e-12);
    near(erf(0.5), 0.520_499_878, 1e-7);
    near(erf(1.0), 0.842_700_793, 1e-7);
    near(erf(2.0), 0.995_322_265, 1e-7);
    for x in [0.2, 1.3, 2.9] {
        near(erf(x) + erfc(x), 1.0, 1e-12);
    }
}

#[test]
fn the_log_gamma_matches_the_factorials_it_generalises() {
    // `ln_gamma(n + 1) = ln(n!)`, which is a fact rather than a fit, so a wrong coefficient
    // shows up immediately.
    let mut factorial = 1.0f64;
    for n in 1..=15u64 {
        #[allow(clippy::cast_precision_loss)]
        {
            factorial *= n as f64;
            near(ln_gamma(n as f64 + 1.0), factorial.ln(), 1e-10);
        }
    }
}

#[test]
fn the_incomplete_gamma_halves_agree() {
    for (a, x) in [(0.5, 0.3), (1.0, 1.0), (3.0, 2.0), (10.0, 12.0), (50.0, 45.0)] {
        let p = gamma_p(a, x).expect("a lower tail");
        let q = gamma_q(a, x).expect("an upper tail");
        near(p + q, 1.0, 1e-12);
    }
}

#[test]
fn the_incomplete_beta_is_symmetric_in_the_way_it_must_be() {
    // `I_x(a, b) = 1 - I_(1-x)(b, a)`. The identity the continued fraction's reflection is
    // built on, so a reflection applied on the wrong side of the crossover fails here.
    for (a, b, x) in [(2.0, 3.0, 0.4), (0.5, 0.5, 0.7), (10.0, 2.0, 0.9), (1.0, 1.0, 0.25)] {
        let left = beta_i(a, b, x).expect("a beta");
        let right = beta_i(b, a, 1.0 - x).expect("a beta");
        near(left + right, 1.0, 1e-12);
    }
    // And the uniform case, where the answer is `x` exactly.
    near(beta_i(1.0, 1.0, 0.37).expect("a beta"), 0.37, 1e-12);
}

// --- Student's t ----------------------------------------------------------

#[test]
fn the_t_critical_values_match_the_table() {
    // The two-sided 95% critical values, from any statistics text.
    near(t_inv(0.975, 1.0).expect("a quantile"), 12.706_204_7, 1e-6);
    near(t_inv(0.975, 5.0).expect("a quantile"), 2.570_581_8, 1e-6);
    near(t_inv(0.975, 10.0).expect("a quantile"), 2.228_138_9, 1e-6);
    near(t_inv(0.975, 30.0).expect("a quantile"), 2.042_272_5, 1e-6);
    near(t_inv(0.975, 100.0).expect("a quantile"), 1.983_971_5, 1e-6);
}

#[test]
fn the_t_approaches_the_normal_as_freedom_grows() {
    // The property every textbook states, and a good check that the beta reflection is right
    // at both ends: at a thousand degrees of freedom the two agree to four decimals.
    let normal = norm_inv(0.975).expect("a quantile");
    let student = t_inv(0.975, 1000.0).expect("a quantile");
    near(student, normal, 5e-3);
    assert!(student > normal, "the t is always the wider of the two");
}

#[test]
fn the_two_sided_p_value_is_offered_by_name_and_agrees_with_the_long_way() {
    // Offered by name because the expression somebody writes --- `2 * (1 - t_cdf(|t|))` ---
    // is wrong in the tail, where subtracting from one costs every digit.
    for (t, freedom) in [(2.0, 10.0), (1.0, 5.0), (3.5, 20.0)] {
        let named = t_two_sided(t, freedom).expect("a p-value");
        let long_way = 2.0 * (1.0 - t_cdf(t.abs(), freedom).expect("a cumulative"));
        near(named, long_way, 1e-9);
    }

    // And in the far tail, where the long way has run out of digits and this has not.
    let far = t_two_sided(30.0, 10.0).expect("a p-value");
    assert!(far > 0.0, "the far tail was rounded to zero: {far}");
    assert!(far < 1e-9, "{far}");
}

// --- chi-squared and F ----------------------------------------------------

#[test]
fn the_chi_squared_critical_values_match_the_table() {
    near(chisq_inv(0.95, 1.0).expect("a quantile"), 3.841_458_8, 1e-5);
    near(chisq_inv(0.95, 2.0).expect("a quantile"), 5.991_464_5, 1e-5);
    near(chisq_inv(0.95, 10.0).expect("a quantile"), 18.307_038_1, 1e-5);
    near(chisq_inv(0.99, 5.0).expect("a quantile"), 15.086_272_5, 1e-5);
}

#[test]
fn the_chi_squared_upper_tail_keeps_its_digits_where_a_subtraction_would_not() {
    // A statistic far out in the tail: `1 - cdf` is exactly zero here, and the survival
    // function is not. That difference is the whole reason `sf` exists.
    let statistic = 200.0;
    let tail = chisq_sf(statistic, 5.0).expect("a tail");
    assert!(tail > 0.0, "the p-value was rounded to zero");
    assert!(tail < 1e-30, "{tail}");
}

#[test]
fn the_f_critical_values_match_the_table() {
    near(f_inv(0.95, 1.0, 1.0).expect("a quantile"), 161.447_6, 1e-2);
    near(f_inv(0.95, 3.0, 10.0).expect("a quantile"), 3.708_265, 1e-4);
    near(f_inv(0.95, 10.0, 20.0).expect("a quantile"), 2.347_878, 1e-4);
}

#[test]
fn an_f_statistic_of_one_is_the_middle_when_the_freedoms_match() {
    // A property with a known answer: with equal degrees of freedom the distribution is
    // symmetric in `F` against `1/F`, so `sf(1)` is a half.
    for freedom in [2.0, 5.0, 20.0] {
        near(f_sf(1.0, freedom, freedom).expect("a tail"), 0.5, 1e-9);
    }
}

// --- the discrete ---------------------------------------------------------

#[test]
fn the_binomial_sums_to_one_and_matches_by_hand() {
    // Ten fair coins: the probability of exactly five heads is 252/1024.
    near(binomial_pmf(5, 10, 0.5).expect("a probability"), 252.0 / 1024.0, 1e-12);
    near(binomial_pmf(0, 10, 0.5).expect("a probability"), 1.0 / 1024.0, 1e-12);
    near(binomial_pmf(10, 10, 0.5).expect("a probability"), 1.0 / 1024.0, 1e-12);

    let total: f64 = (0..=10).map(|k| binomial_pmf(k, 10, 0.3).expect("a probability")).sum();
    near(total, 1.0, 1e-12);

    // And the cumulative agrees with summing the terms, which is the identity the incomplete
    // beta is standing in for.
    for k in 0..10u64 {
        let summed: f64 =
            (0..=k).map(|i| binomial_pmf(i, 10, 0.3).expect("a probability")).sum();
        near(binomial_cdf(k, 10, 0.3).expect("a cumulative"), summed, 1e-10);
    }
}

#[test]
fn a_binomial_large_enough_to_overflow_a_factorial_still_answers() {
    // `choose(1000, 500)` is about `2.7e299`, and the probability it appears in is an
    // ordinary small number. Computed in logarithms, which is why this works at all.
    let probability = binomial_pmf(500, 1000, 0.5).expect("a probability");
    assert!(probability.is_finite() && probability > 0.0, "{probability}");
    near(probability, 0.025_225_018, 1e-8);
}

#[test]
fn the_poisson_sums_to_one_and_its_cumulative_agrees() {
    let total: f64 = (0..60).map(|k| poisson_pmf(k, 4.0).expect("a probability")).sum();
    near(total, 1.0, 1e-12);
    for k in 0..12u64 {
        let summed: f64 = (0..=k).map(|i| poisson_pmf(i, 4.0).expect("a probability")).sum();
        near(poisson_cdf(k, 4.0).expect("a cumulative"), summed, 1e-10);
    }
}

// --- the simple continuous ------------------------------------------------

#[test]
fn the_exponential_keeps_its_digits_for_a_tiny_argument() {
    // `1 - exp(-x)` loses every digit for a small `x`; `-expm1(-x)` does not. At `x = 1e-18`
    // the naive form returns exactly zero.
    let tiny = exponential_cdf(1e-18, 1.0).expect("a cumulative");
    assert!(tiny > 0.0, "a small probability was rounded to zero");
    near(tiny, 1e-18, 1e-30);

    near(exponential_cdf(1.0, 1.0).expect("a cumulative"), 0.632_120_559, 1e-9);
}

#[test]
fn the_uniform_and_lognormal_behave_at_their_edges() {
    near(uniform_cdf(5.0, 0.0, 10.0).expect("a cumulative"), 0.5, 1e-12);
    assert_eq!(uniform_cdf(-1.0, 0.0, 10.0), Ok(0.0));
    assert_eq!(uniform_cdf(11.0, 0.0, 10.0), Ok(1.0));
    assert!(uniform_cdf(1.0, 5.0, 5.0).is_err(), "an interval of no width is not a uniform");

    // A lognormal carries no probability at or below zero, and that is an answer rather than
    // an error --- the distribution is defined there.
    assert_eq!(lognormal_cdf(0.0, 0.0, 1.0), Ok(0.0));
    assert_eq!(lognormal_cdf(-3.0, 0.0, 1.0), Ok(0.0));
    near(lognormal_cdf(1.0, 0.0, 1.0).expect("a cumulative"), 0.5, 1e-9);
}

#[test]
fn a_non_positive_parameter_is_refused_by_every_distribution_that_has_one() {
    // A distribution with a non-positive scale or freedom is not a narrow distribution --- it
    // is not a distribution, and answering would be inventing one.
    assert!(t_cdf(1.0, 0.0).is_err());
    assert!(t_cdf(1.0, -1.0).is_err());
    assert!(chisq_sf(1.0, 0.0).is_err());
    assert!(f_sf(1.0, 0.0, 5.0).is_err());
    assert!(f_sf(1.0, 5.0, 0.0).is_err());
    assert!(exponential_cdf(1.0, 0.0).is_err());
    assert!(poisson_pmf(1, 0.0).is_err());
    assert!(binomial_pmf(1, 10, 1.5).is_err());
    assert!(binomial_pmf(11, 10, 0.5).is_err());
}

// --- the places a subtraction or a wrong branch costs every digit -----------

#[test]
fn the_error_function_of_a_tiny_argument_keeps_its_digits() {
    // `erf(x) ≈ x · 2/√π` for a small `x`, so `erf(1e-18)` is about `1.128e-18`. Computed as
    // `1 - erfc(x)` it is exactly **zero**: `erfc(1e-18)` is one to the last bit, and the
    // subtraction has nothing left to give.
    //
    // This is why both halves are taken from the incomplete gamma directly rather than one
    // from the other. A zero here is not a small answer --- it is the loss of the answer.
    let tiny = 1e-18;
    let value = erf(tiny);
    assert!(value > 0.0, "erf of a tiny argument was rounded to zero");
    near(value, tiny * 2.0 / std::f64::consts::PI.sqrt(), 1e-30);

    // And the same on the other side, where `erfc` is the small one.
    let far = erfc(10.0);
    assert!(far > 0.0, "the complementary tail was rounded to zero");
    assert!(far < 1e-44, "{far}");
}

#[test]
fn the_incomplete_gamma_switches_representation_rather_than_iterating_past_its_range() {
    // The series converges quickly below `a + 1` and slowly above it; the continued fraction
    // does the reverse. Using one everywhere is how a cumulative comes to take three hundred
    // iterations and still be wrong.
    //
    // `P(1, 200)` is one to every bit a double has. The series would need some four hundred
    // terms to get there and stops at three hundred, so it returns a number visibly below one.
    near(gamma_p(1.0, 200.0).expect("a cumulative"), 1.0, 1e-15);
    near(gamma_p(1.0, 50.0).expect("a cumulative"), 1.0, 1e-15);

    // And the upper tail on the same inputs is the tiny number it should be, computed by the
    // fraction rather than by subtracting from one.
    near(gamma_q(1.0, 200.0).expect("a tail"), (-200.0f64).exp(), 1e-100);
    near(gamma_q(1.0, 50.0).expect("a tail"), (-50.0f64).exp(), 1e-30);
}

#[test]
fn a_binomial_cumulative_over_a_thousand_trials_is_right_and_immediate() {
    // Through the incomplete beta rather than by summing terms. Five hundred additions would
    // also arrive, eventually and with five hundred roundings accumulated --- the identity is
    // one evaluation and is exact.
    near(binomial_cdf(500, 1000, 0.5).expect("a cumulative"), 0.512_612_509_2, 1e-9);
    // Symmetry of the fair binomial: `P(X ≤ 499) + P(X ≤ 500) = 1` for `n = 1000`.
    let below = binomial_cdf(499, 1000, 0.5).expect("a cumulative");
    let upto = binomial_cdf(500, 1000, 0.5).expect("a cumulative");
    near(below + upto, 1.0, 1e-12);
}

#[test]
fn the_chi_squared_series_converges_at_the_degrees_of_freedom_a_table_can_hold() {
    // `chisq_cdf(f, f)` is a shade above one half for every `f`, and stays there.
    //
    // `gamma_p` routes everything with `x < a + 1` through the series representation, which is
    // the whole lower half of every chi-squared and gamma distribution. The series needs about
    // `x` terms; it was capped at three hundred, and past the cap it fell out of the loop and
    // **returned the partial sum** with no error and no flag:
    //
    //   f = 10_000    returned 0.5018679  against 0.5018806   (2.5e-5)
    //   f = 100_000   returned 0.41100    against 0.50059     (18%)
    //   f = 1_000_000 returned 0.16483    against 0.50019     (67%)
    //
    // A 67% error reported as a probability, from a crate whose `lib.rs` promises that every
    // function "either produces an exact, reproducible answer or refuses".
    for freedom in [1_000.0, 10_000.0, 100_000.0, 1_000_000.0] {
        let p = chisq_cdf(freedom, freedom).expect("a positive shape and a positive value");
        assert!(
            (p - 0.5).abs() < 0.01,
            "chisq_cdf({freedom}, {freedom}) = {p}, which is not near one half"
        );
        // And its complement, which takes the other branch, agrees with it.
        let q = chisq_sf(freedom, freedom).expect("the same arguments");
        assert!((p + q - 1.0).abs() < 1e-9, "the two tails do not sum to one: {p} + {q}");
    }
}
