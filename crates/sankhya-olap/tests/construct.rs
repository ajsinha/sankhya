//! Building vectors and matrices from SQL, and refusing what cannot be built.
//!
//! The interesting half is the refusals, and they happen at **planning time**. A matrix's
//! shape is part of its type, so a wrong element count is a type error rather than a data
//! error — and catching it when the query is planned is the difference between a query that
//! never starts and one that fails partway through a scan, after work has been done.

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

use datafusion::prelude::SessionContext;

fn session() -> SessionContext {
    let context = SessionContext::new();
    sankhya_olap::construct::register(&context);
    sankhya_olap::matrices::register(&context);
    sankhya_olap::vectors::register(&context);
    context
}

async fn text(context: &SessionContext, sql: &str) -> String {
    let batches = context
        .sql(sql)
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");
    datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

// --- vectors --------------------------------------------------------------

#[tokio::test]
async fn a_vector_can_be_built_from_its_elements() {
    let out = text(&session(), "SELECT vec_of(1.0, 2.0, 3.0) AS v").await;
    assert!(
        out.contains("1.0") && out.contains("2.0") && out.contains("3.0"),
        "{out}"
    );
}

#[tokio::test]
async fn a_constructed_vector_is_fixed_length_so_the_kernels_take_it() {
    // Fixed rather than variable because the width is knowable at planning time — it is the
    // argument count — which makes it a schema-level guarantee rather than a per-row fact.
    let out = text(&session(), "SELECT vec_norm_l2(vec_of(3.0, 4.0)) AS n").await;
    assert!(out.contains("5.0"), "the norm of (3,4) is 5: {out}");
}

#[tokio::test]
async fn integers_widen_into_a_vector() {
    let out = text(
        &session(),
        "SELECT vec_dot(vec_of(1, 2, 3), vec_of(4, 5, 6)) AS d",
    )
    .await;
    assert!(out.contains("32"), "1*4 + 2*5 + 3*6 = 32: {out}");
}

#[tokio::test]
async fn a_null_element_makes_the_whole_vector_null() {
    // There is no reading of a vector with a hole in it: the element is not zero, and
    // shortening it moves every element after it to the wrong position.
    let out = text(
        &session(),
        "SELECT vec_norm_l2(vec_of(1.0, CAST(NULL AS DOUBLE), 3.0)) AS n",
    )
    .await;
    assert!(
        !out.contains("1.0"),
        "a null element must not become a zero: {out}"
    );
}

#[tokio::test]
async fn text_is_refused_as_a_vector_element_rather_than_parsed() {
    // A text column silently parsed into numbers produces a vector from values nobody meant
    // as numbers.
    let outcome = session()
        .sql("SELECT vec_of('one', 'two')")
        .await
        .expect("planning")
        .collect()
        .await;
    let Err(error) = outcome else {
        panic!("text must not be parsed into a vector");
    };
    assert!(
        error.to_string().contains("nobody meant as numbers"),
        "{error}"
    );
}

// --- matrices -------------------------------------------------------------

#[tokio::test]
async fn a_matrix_can_be_built_and_immediately_operated_on() {
    // The constructor emits its own field metadata, so the shape reaches the matrix
    // functions without a column ever being stored.
    let out = text(
        &session(),
        "SELECT mat_determinant(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0)) AS d",
    )
    .await;
    assert!(out.contains("-2"), "1*4 - 2*3 = -2: {out}");
}

#[tokio::test]
async fn a_rectangular_matrix_keeps_its_shape() {
    // 2×3 transposed is 3×2, which only works if the shape travelled with it.
    let out = text(
        &session(),
        "SELECT mat_transpose(mat_of(2, 3, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0)) AS t",
    )
    .await;
    // Row-major [1 2 3; 4 5 6] transposed is [1 4; 2 5; 3 6].
    assert!(out.contains("1.0") && out.contains("4.0"), "{out}");
}

#[tokio::test]
async fn two_constructed_matrices_multiply() {
    let out = text(
        &session(),
        "SELECT mat_multiply(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0), \
                             mat_of(2, 2, 5.0, 6.0, 7.0, 8.0)) AS p",
    )
    .await;
    for expected in ["19", "22", "43", "50"] {
        assert!(out.contains(expected), "missing {expected}: {out}");
    }
}

#[tokio::test]
async fn an_identity_matrix_can_be_constructed() {
    let out = text(&session(), "SELECT mat_trace(mat_identity(4)) AS t").await;
    assert!(out.contains('4'), "the trace of a 4×4 identity is 4: {out}");
}

#[tokio::test]
async fn multiplying_by_a_constructed_identity_changes_nothing() {
    let out = text(
        &session(),
        "SELECT mat_determinant(mat_multiply(mat_of(2, 2, 4.0, 7.0, 2.0, 6.0), \
                                             mat_identity(2))) AS d",
    )
    .await;
    // 4*6 - 7*2 = 10, unchanged by the identity.
    assert!(out.contains("10"), "{out}");
}

// --- refusals at planning time --------------------------------------------

#[tokio::test]
async fn a_wrong_element_count_is_refused_when_the_query_is_planned() {
    // Not partway through a scan, after work has been done and — in a longer pipeline —
    // after rows have been written.
    let outcome = session()
        .sql("SELECT mat_of(2, 3, 1.0, 2.0, 3.0, 4.0, 5.0)")
        .await;
    let Err(error) = outcome else {
        panic!("a 2 by 3 matrix needs six values, not five");
    };
    let message = error.to_string();
    assert!(message.contains("needs 6 values"), "{message}");
    assert!(message.contains("given 5"), "{message}");
    assert!(message.contains("partway through the scan"), "{message}");
}

#[tokio::test]
async fn a_shape_that_is_not_a_literal_is_refused_with_the_reason() {
    // The shape is part of the result's type, and a type cannot depend on a value that
    // varies row to row.
    let context = session();
    let outcome = context
        .sql("SELECT mat_of(id, 2, 1.0, 2.0) FROM (SELECT 2 AS id)")
        .await;
    let Err(error) = outcome else {
        panic!("a per-row shape must be refused");
    };
    assert!(
        error.to_string().contains("varies row to row"),
        "the refusal must say why: {error}"
    );
}

#[tokio::test]
async fn a_dimension_of_zero_or_less_is_refused() {
    for sql in [
        "SELECT mat_of(0, 2)",
        "SELECT mat_of(2, -1, 1.0, 2.0)",
        "SELECT mat_identity(0)",
    ] {
        assert!(session().sql(sql).await.is_err(), "'{sql}' must be refused");
    }
}

#[tokio::test]
async fn a_constructed_matrix_that_is_singular_is_still_refused_by_the_kernel() {
    // The constructor builds it happily — it is a perfectly good matrix — and the operation
    // that cannot be done on it is the one that refuses.
    let context = session();
    assert!(
        text(
            &context,
            "SELECT mat_determinant(mat_of(2, 2, 1.0, 2.0, 2.0, 4.0)) AS d"
        )
        .await
        .contains('0'),
        "a singular matrix has determinant zero, which is an answer"
    );

    let outcome = context
        .sql("SELECT mat_inverse(mat_of(2, 2, 1.0, 2.0, 2.0, 4.0))")
        .await
        .expect("planning")
        .collect()
        .await;
    assert!(outcome.is_err(), "and it has no inverse");
}

#[tokio::test]
async fn every_constructor_is_registered() {
    let context = session();
    for sql in [
        "SELECT vec_of(1.0)",
        "SELECT mat_of(1, 1, 1.0)",
        "SELECT mat_identity(1)",
    ] {
        assert!(context.sql(sql).await.is_ok(), "{sql} is not registered");
    }
}
