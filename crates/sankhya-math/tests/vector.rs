//! Vector kernels, and the determinism that is the reason they exist.
//!
//! The tests that carry this file are the ones asserting **bit-identical** results under
//! permutation. Everything else is arithmetic anybody could check; determinism is the
//! property that made writing these rather than calling a library the right decision, and
//! it is the one that would break silently.

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

use sankhya_math::vector::{
    add, cosine_distance, cosine_similarity, divide, dot, euclidean, matvec, mean, multiply,
    norm_l1, norm_l2, row_of, scale, subtract, sum, VectorError,
};

// --- determinism: the reason this module exists ---------------------------

/// Values spanning many orders of magnitude, which is where non-associativity bites.
fn adversarial() -> (Vec<f64>, Vec<f64>) {
    let mut a = Vec::new();
    let mut b = Vec::new();
    for i in 0..400 {
        let scale = 10f64.powi(i % 40 - 20);
        a.push(scale * f64::from(i % 7 + 1));
        b.push(scale * f64::from((i % 11) as i32 - 5));
    }
    (a, b)
}

/// Permute a pair identically, so the mathematical answer is unchanged.
///
/// `(i * by) % n` is a permutation only when `by` and `n` are coprime. Otherwise it repeats
/// indices, the "permuted" array holds different data, and the test proves nothing — which
/// is exactly what happened to the variance test in `stats.rs` before this guard existed.
fn permuted(a: &[f64], b: &[f64], by: usize) -> (Vec<f64>, Vec<f64>) {
    let n = a.len();
    let order: Vec<usize> = (0..n).map(|i| (i * by + 7) % n).collect();
    let mut distinct = order.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        n,
        "stride {by} is not coprime with {n}, so this is not a permutation"
    );
    (
        order.iter().map(|i| a[*i]).collect(),
        order.iter().map(|i| b[*i]).collect(),
    )
}

#[test]
fn a_dot_product_is_bit_identical_under_permutation() {
    // The whole argument of ADR-0005. A naive or library dot product sums in traversal
    // order, so the same data partitioned differently returns a different number — small
    // enough not to notice and large enough not to reconcile.
    let (a, b) = adversarial();
    let reference = dot(&a, &b).expect("same length");

    for by in [1usize, 3, 7, 11, 13, 17, 101, 199] {
        let (pa, pb) = permuted(&a, &b, by);
        assert_eq!(
            dot(&pa, &pb).expect("same length").to_bits(),
            reference.to_bits(),
            "permutation by {by} changed the dot product"
        );
    }
}

#[test]
fn a_norm_is_bit_identical_under_permutation() {
    let (a, _) = adversarial();
    let reference = norm_l2(&a);
    for by in [3usize, 7, 13, 101] {
        let (pa, _) = permuted(&a, &a, by);
        assert_eq!(norm_l2(&pa).to_bits(), reference.to_bits(), "by {by}");
    }
    let l1 = norm_l1(&a);
    for by in [3usize, 7, 13] {
        let (pa, _) = permuted(&a, &a, by);
        assert_eq!(norm_l1(&pa).to_bits(), l1.to_bits());
    }
}

#[test]
fn a_euclidean_distance_is_bit_identical_under_permutation() {
    let (a, b) = adversarial();
    let reference = euclidean(&a, &b).expect("same length");
    for by in [3usize, 11, 199] {
        let (pa, pb) = permuted(&a, &b, by);
        assert_eq!(
            euclidean(&pa, &pb).expect("same length").to_bits(),
            reference.to_bits()
        );
    }
}

#[test]
fn a_naive_sum_would_have_failed_that_test() {
    // Establishing that the tests above are not vacuous. If a straightforward left-to-right
    // sum gave the same answer under permutation, the deterministic machinery would be
    // buying nothing and these tests would prove nothing.
    let (a, b) = adversarial();
    let naive = |x: &[f64], y: &[f64]| -> f64 {
        x.iter()
            .zip(y)
            .map(|(p, q)| p * q)
            .fold(0.0, |acc, v| acc + v)
    };

    let reference = naive(&a, &b);
    let differs = [1usize, 3, 7, 11, 13, 17, 101, 199].iter().any(|by| {
        let (pa, pb) = permuted(&a, &b, *by);
        naive(&pa, &pb).to_bits() != reference.to_bits()
    });
    assert!(
        differs,
        "the fixture is not adversarial enough: a naive sum was already order-independent \
         on it, so the determinism tests above prove nothing"
    );
}

// --- elementwise ----------------------------------------------------------

#[test]
fn elementwise_operations_do_what_they_say() {
    let a = [1.0, 2.0, 4.0];
    let b = [0.5, 2.0, 8.0];

    assert_eq!(add(&a, &b).expect("same length"), vec![1.5, 4.0, 12.0]);
    assert_eq!(subtract(&a, &b).expect("same length"), vec![0.5, 0.0, -4.0]);
    assert_eq!(multiply(&a, &b).expect("same length"), vec![0.5, 4.0, 32.0]);
    assert_eq!(divide(&a, &b).expect("same length"), vec![2.0, 1.0, 0.5]);
    assert_eq!(scale(&a, 3.0), vec![3.0, 6.0, 12.0]);
}

#[test]
fn dividing_one_element_by_zero_does_not_lose_the_other_answers() {
    // Deliberately unlike the scalar case. An elementwise operation over a thousand
    // elements should not fail entirely because one of them is zero, and the infinity is
    // visible in the result where a refusal would hide the other 999.
    let out = divide(&[1.0, 2.0, 3.0], &[1.0, 0.0, 3.0]).expect("elementwise");
    assert_eq!(out[0], 1.0);
    assert!(out[1].is_infinite());
    assert_eq!(out[2], 1.0);
}

// --- refusals -------------------------------------------------------------

#[test]
fn combining_vectors_of_different_lengths_is_refused_not_truncated() {
    // A dot product over the first `min(a, b)` elements is a plausible number with no
    // meaning, which is the worst kind of answer this system can produce.
    let short = [1.0, 2.0];
    let long = [1.0, 2.0, 3.0];

    for outcome in [
        dot(&short, &long).err(),
        add(&short, &long).err(),
        euclidean(&short, &long).err(),
        cosine_similarity(&short, &long).err(),
    ] {
        assert_eq!(
            outcome,
            Some(VectorError::LengthMismatch { left: 2, right: 3 })
        );
    }
    assert!(VectorError::LengthMismatch { left: 2, right: 3 }
        .to_string()
        .contains("plausible number with no meaning"));
}

#[test]
fn a_mean_of_nothing_is_refused_because_zero_is_a_value_somebody_acts_on() {
    assert_eq!(mean(&[]), Err(VectorError::Empty));
    assert!(mean(&[]).unwrap_err().to_string().contains("is not zero"));
    // A sum of nothing genuinely is zero, and is offered.
    assert_eq!(sum(&[]), 0.0);
}

#[test]
fn cosine_against_a_zero_vector_is_refused_rather_than_answered() {
    // The angle to the origin is undefined — not zero and not one. Returning either places
    // that vector at a definite similarity to everything, sorting it to the top or the
    // bottom of every ranked result.
    let zero = [0.0, 0.0, 0.0];
    let real = [1.0, 2.0, 3.0];

    assert_eq!(
        cosine_similarity(&zero, &real),
        Err(VectorError::ZeroMagnitude)
    );
    assert_eq!(
        cosine_similarity(&real, &zero),
        Err(VectorError::ZeroMagnitude)
    );
    assert!(VectorError::ZeroMagnitude
        .to_string()
        .contains("definite similarity to everything"));
}

// --- the arithmetic -------------------------------------------------------

#[test]
fn cosine_similarity_is_one_for_the_same_direction_and_minus_one_for_the_opposite() {
    let a = [3.0, 4.0];
    assert!((cosine_similarity(&a, &a).expect("valid") - 1.0).abs() < 1e-12);
    assert!((cosine_similarity(&a, &scale(&a, 10.0)).expect("valid") - 1.0).abs() < 1e-12);
    assert!((cosine_similarity(&a, &scale(&a, -1.0)).expect("valid") + 1.0).abs() < 1e-12);
    // Orthogonal.
    assert!(
        cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])
            .expect("valid")
            .abs()
            < 1e-12
    );
}

#[test]
fn cosine_distance_is_one_minus_the_similarity_and_not_the_other_way_round() {
    // The two are constantly confused, and a ranking sorted by the wrong one is reversed —
    // which looks like a working ranking of the least similar things.
    let a = [1.0, 0.0];
    assert!(cosine_distance(&a, &a).expect("valid").abs() < 1e-12);
    assert!((cosine_distance(&a, &[-1.0, 0.0]).expect("valid") - 2.0).abs() < 1e-12);
}

#[test]
fn the_norms_and_the_distance_agree_with_the_textbook() {
    assert_eq!(norm_l2(&[3.0, 4.0]), 5.0);
    assert_eq!(norm_l1(&[3.0, -4.0]), 7.0);
    assert_eq!(euclidean(&[0.0, 0.0], &[3.0, 4.0]).expect("valid"), 5.0);
    assert_eq!(
        dot(&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]).expect("valid"),
        32.0
    );
    assert_eq!(mean(&[1.0, 2.0, 3.0]).expect("valid"), 2.0);
}

// --- matrices and flat columns --------------------------------------------

#[test]
fn a_matrix_times_a_vector_is_a_dot_product_per_row() {
    // Row-major and flat, which is how a fixed-shape tensor stores one.
    let matrix = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]; // 2 rows, 3 columns
    let vector = [1.0, 0.0, -1.0];
    assert_eq!(
        matvec(&matrix, 2, 3, &vector).expect("valid"),
        vec![-2.0, -2.0]
    );
}

#[test]
fn a_matrix_whose_length_does_not_match_its_shape_is_refused() {
    // Otherwise it reads past a row boundary and produces a number.
    assert!(matvec(&[1.0, 2.0, 3.0], 2, 3, &[1.0, 1.0, 1.0]).is_err());
    assert!(matvec(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2, 3, &[1.0]).is_err());
}

#[test]
fn a_row_of_a_flat_column_is_the_slice_arrow_stores() {
    // An Arrow FixedSizeList<Float64, N> column of `rows` vectors is `rows * N` values
    // contiguously, so this is the whole cost of reaching one: no copy, no allocation.
    let column = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    assert_eq!(row_of(&column, 3, 0), Some(&column[0..3]));
    assert_eq!(row_of(&column, 3, 1), Some(&column[3..6]));
    assert_eq!(row_of(&column, 3, 2), None, "past the end");
    assert_eq!(row_of(&column, 0, 0), None, "a zero width is not a vector");
}

#[test]
fn a_row_past_the_end_is_none_rather_than_a_short_slice() {
    // A truncated vector produces a plausible number from a kernel that has no way to know
    // it was truncated.
    let column = [1.0, 2.0, 3.0, 4.0, 5.0];
    assert_eq!(
        row_of(&column, 3, 1),
        None,
        "the second row would run past the end, so there is no second row"
    );
}
