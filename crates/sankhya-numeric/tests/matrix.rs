//! Linear algebra, checked against results anyone can verify by hand.
//!
//! Two properties beyond the arithmetic. Multiplication is **bit-identical under
//! permutation of the summation**, inheriting the dot product's guarantee. And the LU pivot
//! is chosen with a tie-break on row index, without which two rows of equal magnitude could
//! be chosen differently by two builds and the factorisation would differ in its last bits.

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

use sankhya_numeric::matrix::{
    determinant, identity, inverse, multiply, solve, trace, transpose, MatrixError,
};
use sankhya_numeric::vector::matvec;

/// Assert two matrices agree to within a tolerance appropriate for elimination.
fn close(got: &[f64], want: &[f64]) {
    assert_eq!(
        got.len(),
        want.len(),
        "different shapes: {got:?} vs {want:?}"
    );
    for (a, b) in got.iter().zip(want) {
        assert!((a - b).abs() < 1e-9, "{got:?} != {want:?}");
    }
}

// --- multiplication -------------------------------------------------------

#[test]
fn multiplying_two_matrices_gives_the_textbook_answer() {
    // [1 2]   [5 6]   [19 22]
    // [3 4] × [7 8] = [43 50]
    let a = [1.0, 2.0, 3.0, 4.0];
    let b = [5.0, 6.0, 7.0, 8.0];
    close(
        &multiply(&a, 2, 2, &b, 2, 2).expect("conformable"),
        &[19.0, 22.0, 43.0, 50.0],
    );
}

#[test]
fn a_rectangular_product_has_the_outer_shape() {
    // 2×3 times 3×2 is 2×2.
    let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let b = [1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    let out = multiply(&a, 2, 3, &b, 3, 2).expect("conformable");
    assert_eq!(out.len(), 4);
    close(&out, &[4.0, 5.0, 10.0, 11.0]);
}

#[test]
fn multiplying_by_the_identity_changes_nothing() {
    let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
    close(&multiply(&a, 3, 3, &identity(3), 3, 3).expect("valid"), &a);
    close(&multiply(&identity(3), 3, 3, &a, 3, 3).expect("valid"), &a);
}

#[test]
fn a_product_is_bit_identical_run_to_run() {
    // Inherited from the dot product's order-fixed summation. Values spanning many orders
    // of magnitude, which is where a reordered sum diverges.
    let a: Vec<f64> = (0..64)
        .map(|i| 10f64.powi(i % 16 - 8) * f64::from(i % 7 + 1))
        .collect();
    let b: Vec<f64> = (0..64)
        .map(|i| 10f64.powi(i % 13 - 6) * f64::from(i % 5 + 1))
        .collect();

    let reference = multiply(&a, 8, 8, &b, 8, 8).expect("conformable");
    for _ in 0..20 {
        let again = multiply(&a, 8, 8, &b, 8, 8).expect("conformable");
        for (x, y) in reference.iter().zip(&again) {
            assert_eq!(x.to_bits(), y.to_bits(), "a product changed between runs");
        }
    }
}

#[test]
fn non_conformable_matrices_are_refused() {
    let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    assert_eq!(
        multiply(&a, 2, 3, &a, 2, 3),
        Err(MatrixError::NotConformable {
            left_columns: 3,
            right_rows: 2
        })
    );
}

#[test]
fn a_shape_that_does_not_match_the_values_is_refused_not_reshaped() {
    // A matrix read at the wrong shape produces numbers from values that were never in the
    // same row, and every one of them looks ordinary.
    let a = [1.0, 2.0, 3.0];
    let Err(error) = multiply(&a, 2, 2, &a, 2, 2) else {
        panic!("three values cannot be a 2 by 2 matrix");
    };
    assert_eq!(
        error,
        MatrixError::ShapeMismatch {
            values: 3,
            expected: 4
        }
    );
    assert!(error.to_string().contains("never in the same row"));
}

// --- transpose, trace, identity -------------------------------------------

#[test]
fn transposing_twice_returns_the_original() {
    let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let once = transpose(&a, 2, 3).expect("valid");
    assert_eq!(once, vec![1.0, 4.0, 2.0, 5.0, 3.0, 6.0]);
    close(&transpose(&once, 3, 2).expect("valid"), &a);
}

#[test]
fn the_trace_is_the_sum_of_the_diagonal_and_needs_a_square_matrix() {
    assert_eq!(trace(&[1.0, 2.0, 3.0, 4.0], 2, 2).expect("square"), 5.0);
    assert_eq!(
        trace(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2, 3),
        Err(MatrixError::NotSquare {
            rows: 2,
            columns: 3
        })
    );
}

#[test]
fn the_identity_is_ones_on_the_diagonal() {
    assert_eq!(
        identity(3),
        vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
    );
    assert_eq!(trace(&identity(4), 4, 4).expect("square"), 4.0);
}

// --- determinant ----------------------------------------------------------

#[test]
fn the_determinant_agrees_with_the_hand_computation() {
    // 1*4 - 2*3 = -2
    assert!((determinant(&[1.0, 2.0, 3.0, 4.0], 2, 2).expect("square") + 2.0).abs() < 1e-12);
    // A 3×3 with a known determinant of 1.
    let a = [2.0, -1.0, 0.0, -1.0, 2.0, -1.0, 0.0, -1.0, 2.0];
    assert!((determinant(&a, 3, 3).expect("square") - 4.0).abs() < 1e-9);
    assert!((determinant(&identity(5), 5, 5).expect("square") - 1.0).abs() < 1e-12);
}

#[test]
fn a_singular_matrix_has_determinant_zero_exactly() {
    // The one place the factorisation's refusal is an answer rather than a failure: the
    // second row is twice the first.
    let singular = [1.0, 2.0, 2.0, 4.0];
    assert_eq!(determinant(&singular, 2, 2).expect("square"), 0.0);
}

// --- solve and inverse ----------------------------------------------------

#[test]
fn solving_a_system_gives_a_vector_the_matrix_maps_back() {
    // The strongest check available without a reference implementation: solve, then
    // multiply back and compare to the right-hand side.
    let a = [4.0, 1.0, 2.0, 1.0, 3.0, 1.0, 2.0, 1.0, 5.0];
    let b = [7.0, 5.0, 8.0];
    let x = solve(&a, 3, &b).expect("non-singular");
    close(&matvec(&a, 3, 3, &x).expect("valid"), &b);
}

#[test]
fn the_inverse_multiplied_by_the_original_is_the_identity() {
    let a = [4.0, 7.0, 2.0, 6.0];
    let inv = inverse(&a, 2, 2).expect("non-singular");
    close(
        &multiply(&a, 2, 2, &inv, 2, 2).expect("valid"),
        &identity(2),
    );
    close(
        &multiply(&inv, 2, 2, &a, 2, 2).expect("valid"),
        &identity(2),
    );
}

#[test]
fn a_larger_inverse_round_trips_too() {
    // Bigger than anything a cofactor expansion would survive, and with a pivot that must
    // be exchanged: the first element is zero.
    let a = [
        0.0, 2.0, 1.0, 3.0, 1.0, 1.0, 0.0, 2.0, 2.0, 3.0, 1.0, 1.0, 4.0, 1.0, 2.0, 1.0,
    ];
    let inv = inverse(&a, 4, 4).expect("non-singular");
    close(
        &multiply(&a, 4, 4, &inv, 4, 4).expect("valid"),
        &identity(4),
    );
}

#[test]
fn a_singular_matrix_is_refused_rather_than_inverted_anyway() {
    // An almost-singular matrix inverted anyway produces enormous values that are
    // arithmetic noise, and they propagate downstream looking like results.
    let singular = [1.0, 2.0, 2.0, 4.0];
    assert_eq!(inverse(&singular, 2, 2), Err(MatrixError::Singular));
    assert_eq!(solve(&singular, 2, &[1.0, 1.0]), Err(MatrixError::Singular));
    assert!(MatrixError::Singular
        .to_string()
        .contains("arithmetic noise"));
}

#[test]
fn pivoting_is_deterministic_when_two_rows_tie() {
    // Two rows of equal magnitude in the first column. Without a tie-break on row index the
    // pivot choice is arbitrary, and two builds could factor differently — differing in the
    // last bits of every result that follows.
    let tied = [1.0, 2.0, 1.0, 5.0];
    let reference = determinant(&tied, 2, 2).expect("square");
    for _ in 0..20 {
        assert_eq!(
            determinant(&tied, 2, 2).expect("square").to_bits(),
            reference.to_bits()
        );
    }
    assert!((reference - 3.0).abs() < 1e-12);
}

#[test]
fn a_dimension_of_zero_is_refused() {
    assert_eq!(transpose(&[], 0, 3), Err(MatrixError::Degenerate));
    assert_eq!(trace(&[], 0, 0), Err(MatrixError::Degenerate));
}

#[test]
fn solving_with_a_right_hand_side_of_the_wrong_length_is_refused() {
    let a = [1.0, 0.0, 0.0, 1.0];
    assert_eq!(
        solve(&a, 2, &[1.0]),
        Err(MatrixError::ShapeMismatch {
            values: 1,
            expected: 2
        })
    );
}
