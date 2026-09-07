//! The decompositions, against matrices whose answers are known.
//!
//! # What these assert, and why it is properties rather than numbers
//!
//! A decomposition has a defining identity --- `L·Lᵀ = A`, `Q·R = A`, `A·v = λ·v` --- and the
//! identity is a far stronger assertion than a table of expected entries, because a factor
//! that is subtly wrong satisfies no identity while still looking like a matrix.
//!
//! Where a published answer exists it is checked too, because an identity is satisfied by the
//! trivial decomposition of the wrong matrix if the reconstruction is also wrong.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_math::decompose::{
    cholesky, eigen_symmetric, eigenvalues_symmetric, is_positive_definite, is_symmetric, qr,
    singular_values,
};

fn near(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected}, got {actual}"
    );
}

/// `A · B` for row-major matrices.
fn multiply(a: &[f64], ar: usize, ac: usize, b: &[f64], bc: usize) -> Vec<f64> {
    let mut out = vec![0.0; ar * bc];
    for i in 0..ar {
        for j in 0..bc {
            let mut sum = 0.0;
            for k in 0..ac {
                sum += a[i * ac + k] * b[k * bc + j];
            }
            out[i * bc + j] = sum;
        }
    }
    out
}

fn transpose(a: &[f64], rows: usize, columns: usize) -> Vec<f64> {
    let mut out = vec![0.0; rows * columns];
    for i in 0..rows {
        for j in 0..columns {
            out[j * rows + i] = a[i * columns + j];
        }
    }
    out
}

// --- Cholesky -------------------------------------------------------------

#[test]
fn a_cholesky_factor_multiplies_back_to_the_matrix() {
    // A covariance matrix, which is what this is for.
    let a = vec![4.0, 12.0, -16.0, 12.0, 37.0, -43.0, -16.0, -43.0, 98.0];
    let l = cholesky(&a, 3).expect("a positive-definite matrix factors");

    // The published factor for this textbook example, so an identity satisfied by the wrong
    // factor of the wrong matrix cannot pass.
    near(l[0], 2.0, 1e-12);
    near(l[3], 6.0, 1e-12);
    near(l[4], 1.0, 1e-12);
    near(l[6], -8.0, 1e-12);
    near(l[7], 5.0, 1e-12);
    near(l[8], 3.0, 1e-12);

    // And the identity.
    let reconstructed = multiply(&l, 3, 3, &transpose(&l, 3, 3), 3);
    for (got, want) in reconstructed.iter().zip(&a) {
        near(*got, *want, 1e-10);
    }

    // Upper triangle is zero, which is what "lower-triangular" has to mean.
    near(l[1], 0.0, 0.0);
    near(l[2], 0.0, 0.0);
    near(l[5], 0.0, 0.0);
}

#[test]
fn a_matrix_no_data_could_have_produced_is_refused_rather_than_factored() {
    // A "correlation matrix" somebody assembled by hand: every pair correlated at -0.9, which
    // three variables cannot simultaneously be. Cholesky failing IS the test --- it succeeds
    // exactly on the positive-definite matrices, so the refusal names a real impossibility
    // rather than a numerical inconvenience.
    let impossible = vec![1.0, -0.9, -0.9, -0.9, 1.0, -0.9, -0.9, -0.9, 1.0];
    assert!(cholesky(&impossible, 3).is_err());
    assert!(!is_positive_definite(&impossible, 3));

    // And a genuine correlation matrix passes, so the check is not simply strict.
    let real = vec![1.0, 0.5, 0.3, 0.5, 1.0, 0.2, 0.3, 0.2, 1.0];
    assert!(is_positive_definite(&real, 3));
}

#[test]
fn a_matrix_that_is_not_symmetric_is_refused_rather_than_symmetrised() {
    let lopsided = vec![1.0, 2.0, 3.0, 4.0];
    assert!(!is_symmetric(&lopsided, 2));

    // And the refusal says **symmetry**, not shape. It said "needs a square matrix and was
    // given 2 by 2" until the column soak read one back --- a refusal telling somebody their
    // square matrix is not square, which sends them to check the shape and find nothing wrong.
    let said = cholesky(&lopsided, 2).expect_err("not symmetric").to_string();
    assert!(said.contains("symmetric"), "{said}");
    assert!(!said.contains("needs a square matrix"), "the message contradicts itself: {said}");
    assert!(said.contains("answer a question about a different matrix"), "{said}");

    assert!(eigen_symmetric(&lopsided, 2).is_err());

    // Symmetric to the last bit but not exactly, which is what a covariance matrix assembled
    // by summing products actually looks like. Refusing this would refuse the common input.
    let almost = vec![1.0, 0.5, 0.5 + 1e-16, 1.0];
    assert!(is_symmetric(&almost, 2), "a last-bit difference is not asymmetry");
}

// --- QR -------------------------------------------------------------------

#[test]
fn a_qr_decomposition_reconstructs_and_its_q_is_orthogonal() {
    let a = vec![12.0, -51.0, 4.0, 6.0, 167.0, -68.0, -4.0, 24.0, -41.0];
    let (q, r) = qr(&a, 3, 3).expect("a decomposition");

    // `Q·R = A`, the defining identity.
    let reconstructed = multiply(&q, 3, 3, &r, 3);
    for (got, want) in reconstructed.iter().zip(&a) {
        near(*got, *want, 1e-9);
    }

    // `QᵀQ = I`, which is what Householder buys over Gram-Schmidt and is the property that
    // degrades first when an implementation is subtly wrong.
    let identity = multiply(&transpose(&q, 3, 3), 3, 3, &q, 3);
    for i in 0..3 {
        for j in 0..3 {
            near(identity[i * 3 + j], if i == j { 1.0 } else { 0.0 }, 1e-12);
        }
    }

    // `R` is upper-triangular, exactly rather than nearly: the sub-diagonal is set rather
    // than left as rounding dust, so anything checking triangularity gets a clean answer.
    near(r[3], 0.0, 0.0);
    near(r[6], 0.0, 0.0);
    near(r[7], 0.0, 0.0);
}

#[test]
fn a_tall_matrix_decomposes_too() {
    // Overdetermined, which is the shape a least-squares fit has.
    let a = vec![1.0, 1.0, 1.0, 2.0, 1.0, 3.0, 1.0, 4.0];
    let (q, r) = qr(&a, 4, 2).expect("a decomposition");
    let reconstructed = multiply(&q, 4, 4, &r, 2);
    for (got, want) in reconstructed.iter().zip(&a) {
        near(*got, *want, 1e-9);
    }
}

// --- the symmetric eigenproblem -------------------------------------------

#[test]
fn eigenvalues_of_a_known_matrix_match_and_come_back_descending() {
    // A matrix whose eigenvalues are exactly 1, 2 and 5.
    let a = vec![2.0, 0.0, 0.0, 0.0, 3.0, 4.0, 0.0, 4.0, 9.0];
    let values = eigenvalues_symmetric(&a, 3).expect("eigenvalues");
    assert_eq!(values.len(), 3);
    near(values[0], 11.0, 1e-10);
    near(values[1], 2.0, 1e-10);
    near(values[2], 1.0, 1e-10);

    // Descending, and fixed --- so two callers cannot disagree about which is the first.
    assert!(values[0] >= values[1] && values[1] >= values[2]);
}

#[test]
fn every_eigenpair_satisfies_its_defining_identity() {
    // `A·v = λ·v` for each pair. The identity a table of expected values does not check, and
    // the one a decomposition with the vectors beside the wrong eigenvalues fails.
    let a = vec![4.0, 1.0, 2.0, 1.0, 5.0, 3.0, 2.0, 3.0, 6.0];
    let (values, vectors) = eigen_symmetric(&a, 3).expect("a decomposition");

    for (column, &lambda) in values.iter().enumerate() {
        let v: Vec<f64> = (0..3).map(|row| vectors[row * 3 + column]).collect();
        let av = multiply(&a, 3, 3, &v, 1);
        for row in 0..3 {
            near(av[row], lambda * v[row], 1e-9);
        }
        // And each is a unit vector, which Jacobi's rotations preserve.
        let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        near(norm, 1.0, 1e-12);
    }

    // The trace is the sum of the eigenvalues, which is a check on all three at once.
    near(values.iter().sum::<f64>(), 4.0 + 5.0 + 6.0, 1e-10);
}

#[test]
fn an_eigenvector_has_a_fixed_sign_rather_than_whichever_the_rotations_produced() {
    // An eigenvector is determined only up to sign, so leaving the sign to the arithmetic
    // means the same matrix gives the same numbers with different signs on two machines. The
    // largest-magnitude entry of each is made positive.
    //
    // **This test used to check `[[2,1],[1,2]]` and proved nothing**: raw Jacobi happens to
    // return positive-dominant vectors for it, so removing the normalisation changed no
    // assertion. Searched over twenty thousand random symmetric matrices, the rotations alone
    // produce a negative dominant entry for about *half* of all eigenvectors --- so the
    // property is real and the old fixture was simply on the lucky side of it.
    //
    // The matrices below are ones where the rotations produce a negative, checked by removing
    // the normalisation and watching this fail.
    for (matrix, size) in [
        (vec![2.0, 1.0, 1.0, 2.0], 2usize),
        (vec![-3.0, 4.0, 4.0, 3.0], 2),
        (vec![1.0, 2.0, 3.0, 2.0, 4.0, 5.0, 3.0, 5.0, 6.0], 3),
        (vec![0.0, -1.0, -1.0, 0.0], 2),
        (vec![5.0, -2.0, 1.0, -2.0, 3.0, -4.0, 1.0, -4.0, 7.0], 3),
    ] {
        let (_, vectors) = eigen_symmetric(&matrix, size).expect("a decomposition");
        for column in 0..size {
            let v: Vec<f64> = (0..size).map(|row| vectors[row * size + column]).collect();
            let largest =
                v.iter().copied().fold(0.0f64, |m, x| if x.abs() > m.abs() { x } else { m });
            assert!(
                largest > 0.0,
                "an eigenvector's dominant entry is negative: {v:?} of {matrix:?}"
            );
        }
    }
}

#[test]
fn the_householder_sign_is_chosen_away_from_the_head() {
    // The one place QR can lose its accuracy. The reflection vector's first entry is
    // `head - alpha`, and choosing `alpha` with the same sign as `head` makes that a
    // subtraction of two nearly equal numbers when the column is dominated by its first entry.
    //
    // Measured on a matrix built for it: the sign chosen away from the head holds
    // `|QᵀQ - I|` below `2e-24`; chosen toward it, the same matrix gives `4.4e-16` --- eight
    // orders of magnitude of orthogonality, thrown away by a sign.
    let a = vec![1.0, 0.0, 0.0, 1e-8, 1.0, 0.0, 0.0, 1e-8, 1.0];
    let (q, _) = qr(&a, 3, 3).expect("a decomposition");
    let identity = multiply(&transpose(&q, 3, 3), 3, 3, &q, 3);

    let mut worst = 0.0f64;
    for i in 0..3 {
        for j in 0..3 {
            let expected = if i == j { 1.0 } else { 0.0 };
            worst = worst.max((identity[i * 3 + j] - expected).abs());
        }
    }
    assert!(
        worst < 1e-18,
        "orthogonality degraded to {worst:.3e}, which is the sign chosen toward the head"
    );
}

#[test]
fn the_identity_matrix_is_its_own_eigenbasis() {
    // The degenerate case, where every eigenvalue is equal and any basis is valid. A sweep
    // that divided by the difference of two diagonal entries would fail here.
    let identity = vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let (values, _) = eigen_symmetric(&identity, 3).expect("a decomposition");
    for value in values {
        near(value, 1.0, 1e-12);
    }
}

// --- singular values ------------------------------------------------------

#[test]
fn singular_values_of_a_diagonal_matrix_are_its_entries() {
    let a = vec![3.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0];
    let values = singular_values(&a, 3, 3).expect("singular values");
    near(values[0], 3.0, 1e-10);
    near(values[1], 2.0, 1e-10);
    near(values[2], 1.0, 1e-10);
}

#[test]
fn a_rank_deficient_matrix_has_a_zero_singular_value() {
    // The second row is twice the first, so the matrix has rank one and one singular value
    // must vanish. A tiny negative eigenvalue of the Gram matrix is rounding, and its square
    // root is zero rather than a NaN --- which is the case this asserts.
    let a = vec![1.0, 2.0, 2.0, 4.0];
    let values = singular_values(&a, 2, 2).expect("singular values");
    near(values[0], 5.0, 1e-9);
    near(values[1], 0.0, 1e-7);
    assert!(values.iter().all(|v| v.is_finite()), "{values:?}");
}

// --- what a NaN and a small magnitude do -----------------------------------------------

#[test]
fn a_covariance_matrix_with_a_missing_price_will_not_factor() {
    // The whole point of `cholesky`, per this module: a covariance matrix that will not factor
    // is not a numerical accident, it is a statement about the data.
    //
    // Three comparisons used to be NaN-blind in the same direction. `f64::max` returns the
    // non-NaN operand, so a NaN never reached the symmetry scale; `NaN > x` is false, so the
    // symmetry test passed; and `NaN <= 0.0` is false, so the non-positive-pivot refusal never
    // fired and `sqrt(NaN)` was stored. A matrix with one missing price was pronounced
    // symmetric, factored, and positive definite --- so
    // `WHERE mat_is_positive_definite(cov) = 1` passed on exactly the input it exists to
    // catch, and the correlated draws seeded from that factor are all NaN.
    let with_a_gap = [4.0, 2.0, 1.0, 2.0, f64::NAN, 0.5, 1.0, 0.5, 3.0];

    assert!(!is_symmetric(&with_a_gap, 3), "a NaN is not symmetric with anything");
    assert!(cholesky(&with_a_gap, 3).is_err(), "a matrix holding a NaN did not factor");
    assert!(!is_positive_definite(&with_a_gap, 3), "and it is not positive definite");

    // The same matrix with the gap filled does factor, so the refusal is about the NaN and
    // not about the shape.
    let filled = [4.0, 2.0, 1.0, 2.0, 3.0, 0.5, 1.0, 0.5, 3.0];
    assert!(is_positive_definite(&filled, 3), "the same shape, with a number in it");
}

#[test]
fn a_small_matrix_is_rotated_rather_than_declared_already_diagonal() {
    // The convergence bound is relative to the matrix, and this is why.
    //
    // As an absolute `1e-15` against the unscaled off-diagonal norm, any matrix whose entries
    // were already that small was declared converged before a single rotation ran, and the
    // input's diagonal came back as its eigenvalues. Here that gave `5e-16` and `1e-16`
    // against a true `6.6056e-16` and `-6.0555e-17`: the larger 24% low, and **the smaller
    // with the wrong sign**, so an indefinite matrix passes an all-non-negative test.
    //
    // The true values are the roots of `x² - 6e-16·x - (5e-32 - 9e-32)`, computed by hand.
    let small = [1e-16, 3e-16, 3e-16, 5e-16];
    let values = eigenvalues_symmetric(&small, 2).expect("a symmetric 2x2");

    let (low, high) = (values[0].min(values[1]), values[0].max(values[1]));
    near(high, 6.605_551_275_463_989e-16, 1e-28);
    near(low, -6.055_512_754_639_893e-17, 1e-28);
    assert!(low < 0.0, "this matrix is indefinite and must not report as positive semidefinite");

    // And the scale-freedom that a relative bound buys: the same matrix multiplied by 1e16 has
    // the same eigenvalues multiplied by 1e16.
    let scaled: Vec<f64> = small.iter().map(|v| v * 1e16).collect();
    let big = eigenvalues_symmetric(&scaled, 2).expect("a symmetric 2x2");
    let (blow, bhigh) = (big[0].min(big[1]), big[0].max(big[1]));
    near(bhigh / 1e16, high, 1e-28);
    near(blow / 1e16, low, 1e-28);
}
