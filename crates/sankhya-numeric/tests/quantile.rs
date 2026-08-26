//! Exact quantiles, and the ways of being wrong that this crate refuses to be.

use sankhya_numeric::{quantile, quantile_of_sum, Convention, QuantileError};

fn v(values: &[f64]) -> Vec<f64> {
    values.to_vec()
}

#[test]
fn the_conventions_disagree_at_the_tail() {
    // The reason the convention is a required argument rather than a default. All three
    // answers are correct under their own definition; a system that picks one silently
    // will not tie out against a system that picked another, and the difference appears
    // exactly where the number matters.
    let data: Vec<f64> = (1..=100).map(f64::from).collect();

    let nearest = quantile(&mut v(&data), 0.99, Convention::NearestRank).expect("ok");
    let linear = quantile(&mut v(&data), 0.99, Convention::LinearInterpolation).expect("ok");
    let lower = quantile(&mut v(&data), 0.99, Convention::Lower).expect("ok");

    assert_eq!(nearest, 99.0);
    assert!((linear - 99.01).abs() < 1e-9, "{linear}");
    assert_eq!(lower, 99.0);

    // And at a position where interpolation genuinely bites.
    let small = [1.0, 2.0, 3.0, 4.0];
    assert_eq!(
        quantile(&mut v(&small), 0.5, Convention::NearestRank).expect("ok"),
        2.0
    );
    assert_eq!(
        quantile(&mut v(&small), 0.5, Convention::LinearInterpolation).expect("ok"),
        2.5
    );
    assert_eq!(
        quantile(&mut v(&small), 0.5, Convention::Lower).expect("ok"),
        2.0
    );
}

#[test]
fn the_extremes_are_the_extremes_under_every_convention() {
    let data: Vec<f64> = (1..=50).map(f64::from).collect();
    for convention in [
        Convention::NearestRank,
        Convention::LinearInterpolation,
        Convention::Lower,
    ] {
        assert_eq!(
            quantile(&mut v(&data), 0.0, convention).expect("ok"),
            1.0,
            "{convention} disagreed about the minimum"
        );
        assert_eq!(
            quantile(&mut v(&data), 1.0, convention).expect("ok"),
            50.0,
            "{convention} disagreed about the maximum"
        );
    }
}

#[test]
fn the_answer_does_not_depend_on_the_input_order() {
    let ascending: Vec<f64> = (1..=200).map(f64::from).collect();
    let descending: Vec<f64> = ascending.iter().rev().copied().collect();
    let shuffled: Vec<f64> = {
        // A fixed permutation rather than a random one, so a failure is reproducible.
        let mut out = ascending.clone();
        for i in 0..out.len() {
            out.swap(i, (i * 7 + 13) % 200);
        }
        out
    };

    for q in [0.0, 0.25, 0.5, 0.99, 1.0] {
        let a = quantile(&mut v(&ascending), q, Convention::LinearInterpolation).expect("ok");
        let d = quantile(&mut v(&descending), q, Convention::LinearInterpolation).expect("ok");
        let s = quantile(&mut v(&shuffled), q, Convention::LinearInterpolation).expect("ok");
        assert_eq!(a, d, "at q={q}");
        assert_eq!(a, s, "at q={q}");
    }
}

#[test]
fn an_unorderable_value_is_refused_rather_than_placed_somewhere() {
    // Sorting NaN to one end is the common choice and it silently shifts every quantile.
    // There is no correct place to put it, so there is no correct answer to give.
    let mut data = vec![1.0, 2.0, f64::NAN, 4.0];
    let err = quantile(&mut data, 0.5, Convention::LinearInterpolation)
        .expect_err("a NaN makes every quantile arbitrary");
    assert_eq!(err, QuantileError::NotOrderable { at: 2 });
    assert!(format!("{err}").contains("arbitrarily placed"));
}

#[test]
fn an_empty_input_is_refused_rather_than_answered_with_zero() {
    let err = quantile(&mut [], 0.5, Convention::NearestRank).expect_err("undefined");
    assert_eq!(err, QuantileError::Empty);
    assert!(format!("{err}").contains("plausible number"));
}

#[test]
fn a_quantile_outside_the_unit_interval_is_refused() {
    assert_eq!(
        quantile(&mut [1.0], 1.5, Convention::NearestRank),
        Err(QuantileError::OutOfRange)
    );
    assert_eq!(
        quantile(&mut [1.0], -0.1, Convention::NearestRank),
        Err(QuantileError::OutOfRange)
    );
}

#[test]
fn a_single_observation_is_every_quantile_of_itself() {
    for q in [0.0, 0.5, 1.0] {
        assert_eq!(
            quantile(&mut [42.0], q, Convention::LinearInterpolation).expect("ok"),
            42.0
        );
    }
}

#[test]
fn negative_values_order_correctly() {
    // Losses are negative, and the tail that matters is the negative one. Sorting by
    // magnitude somewhere in the implementation would put the worst loss in the middle.
    let mut data = vec![-100.0, -50.0, -1.0, 0.0, 1.0, 50.0];
    assert_eq!(
        quantile(&mut data, 0.0, Convention::NearestRank).expect("ok"),
        -100.0
    );
    let mut data = vec![-100.0, -50.0, -1.0, 0.0, 1.0, 50.0];
    assert_eq!(
        quantile(&mut data, 1.0, Convention::NearestRank).expect("ok"),
        50.0
    );
}

#[test]
fn the_quantile_of_a_sum_is_not_the_sum_of_quantiles() {
    // The composition that reads correctly and is wrong, demonstrated with numbers.
    //
    // Two exposures across four scenarios. Each has its worst outcome in a *different*
    // scenario, so adding their individual worst cases describes a world in which both
    // went wrong at once -- which is not one of the scenarios.
    let a = vec![-100.0, -10.0, 5.0, 20.0];
    let b = vec![20.0, -90.0, 5.0, -5.0];

    let correct =
        quantile_of_sum(&[a.clone(), b.clone()], 0.0, Convention::NearestRank).expect("ok");

    let naive = quantile(&mut v(&a), 0.0, Convention::NearestRank).expect("ok")
        + quantile(&mut v(&b), 0.0, Convention::NearestRank).expect("ok");

    // Summed element-wise: [-80, -100, 10, 15]. The worst combined outcome is -100.
    assert_eq!(correct, -100.0);
    // Adding the individual worst cases: -100 + -90 = -190.
    assert_eq!(naive, -190.0);
    assert!(
        naive < correct,
        "the naive composition must overstate the loss, or this example proves nothing"
    );
}

#[test]
fn vectors_of_different_widths_are_refused() {
    // Element *i* must be the same scenario in every vector. Padding or truncating would
    // return a number instead of an error, which is worse.
    let err = quantile_of_sum(
        &[vec![1.0, 2.0, 3.0], vec![1.0, 2.0]],
        0.5,
        Convention::NearestRank,
    )
    .expect_err("mismatched widths");
    assert!(matches!(err, QuantileError::NotOrderable { .. }));
}

#[test]
fn summing_one_vector_is_that_vector() {
    let only = vec![3.0, 1.0, 2.0];
    assert_eq!(
        quantile_of_sum(&[only], 0.5, Convention::NearestRank).expect("ok"),
        2.0
    );
}
