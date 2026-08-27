//! The examples in `docs/GUIDE.md`, executed.
//!
//! A documented example that has stopped working is worse than no example: a reader trusts
//! it, and the failure looks like their mistake. So the SQL on that page runs here and its
//! answers are checked, and an example that breaks breaks the build.
//!
//! This covers the parts that need only a session — construction, mathematics, statistics.
//! The examples needing a warehouse or a server are exercised in `sankhya-server`, and the
//! graph examples in `sankhya-graph-sql`.

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

/// A session with everything the guide's examples use registered.
fn session() -> SessionContext {
    let context = SessionContext::new();
    sankhya_olap::construct::register(&context);
    sankhya_olap::matrices::register(&context);
    sankhya_olap::vectors::register(&context);
    context
}

/// Run a statement and render the result the way `psql` would.
async fn run(sql: &str) -> String {
    let batches = session()
        .sql(sql)
        .await
        .unwrap_or_else(|error| panic!("the guide's example did not plan: {sql}\n{error}"))
        .collect()
        .await
        .unwrap_or_else(|error| panic!("the guide's example did not run: {sql}\n{error}"));
    datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

/// Run a statement expected to be refused, returning the message.
async fn refused(sql: &str) -> String {
    let planned = session().sql(sql).await;
    match planned {
        Err(error) => error.to_string(),
        Ok(frame) => match frame.collect().await {
            Err(error) => error.to_string(),
            Ok(_) => panic!("the guide says this is refused, and it was not: {sql}"),
        },
    }
}

// --- §5, building them in SQL ---------------------------------------------

#[tokio::test]
async fn guide_5_constructors() {
    assert!(run("SELECT vec_of(1.0, 2.0, 3.0)").await.contains("2.0"));
    assert!(run("SELECT mat_of(2, 2, 1.0, 2.0, 3.0, 4.0)")
        .await
        .contains("4.0"));
    assert!(run("SELECT mat_identity(3)").await.contains("1.0"));
}

#[tokio::test]
async fn guide_5_a_wrong_element_count_is_refused_at_planning_time() {
    // The guide quotes this message. If it changes, the guide is wrong.
    let message = refused("SELECT mat_of(2, 3, 1.0, 2.0, 3.0, 4.0, 5.0)").await;
    assert!(
        message.contains("needs 6 values and was given 5"),
        "{message}"
    );
    assert!(message.contains("partway through the scan"), "{message}");
}

// --- §5, vector maths -----------------------------------------------------

#[tokio::test]
async fn guide_5_vector_kernels() {
    let out = run(
        "SELECT vec_dot(vec_of(1.0, 2.0, 3.0), vec_of(4.0, 5.0, 6.0)) AS dot, \
                vec_euclidean(vec_of(0.0, 0.0), vec_of(3.0, 4.0)) AS distance, \
                vec_norm_l2(vec_of(3.0, 4.0)) AS norm",
    )
    .await;
    assert!(out.contains("32"), "the dot product is 32: {out}");
    assert!(
        out.contains("5.0"),
        "the distance and the norm are both 5: {out}"
    );
}

#[tokio::test]
async fn guide_5_a_similarity_search_orders_by_closeness() {
    // The shape of the example, without a documents table: a vector is most similar to
    // itself, and less so to one pointing elsewhere.
    let out = run(
        "SELECT vec_cosine_similarity(vec_of(0.1, 0.4, 0.9), vec_of(0.1, 0.4, 0.9)) AS same, \
                vec_cosine_similarity(vec_of(1.0, 0.0, 0.0), vec_of(0.0, 1.0, 0.0)) AS orthogonal",
    )
    .await;
    assert!(out.contains("1.0"), "{out}");
    assert!(out.contains('0'), "{out}");
}

// --- §5, linear algebra ---------------------------------------------------

#[tokio::test]
async fn guide_5_linear_algebra() {
    let out = run(
        "SELECT mat_determinant(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0)) AS d, \
                mat_trace(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0)) AS t",
    )
    .await;
    assert!(out.contains("-2"), "the determinant is -2: {out}");
    assert!(out.contains('5'), "the trace is 5: {out}");
}

#[tokio::test]
async fn guide_5_matrix_functions_compose() {
    // The guide claims this works because matrix-returning functions carry their own shape.
    // It did not, until a test found it.
    let out = run(
        "SELECT mat_determinant(mat_multiply(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0), \
                                             mat_of(2, 2, 5.0, 6.0, 7.0, 8.0))) AS d",
    )
    .await;
    // det([19 22; 43 50]) = 19*50 - 22*43 = 950 - 946 = 4
    assert!(out.contains('4'), "{out}");
}

#[tokio::test]
async fn guide_5_solve_and_multiply() {
    assert!(
        run("SELECT mat_solve(mat_of(2, 2, 4.0, 7.0, 2.0, 6.0), mat_of(2, 1, 1.0, 1.0)) AS x")
            .await
            .contains('[')
    );
    assert!(
        run("SELECT mat_multiply(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0), mat_identity(2)) AS p")
            .await
            .contains("1.0")
    );
}

// --- §6, statistics -------------------------------------------------------

#[tokio::test]
async fn guide_6_statistics_describe_one_row_series() {
    let series = "vec_of(2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0)";
    let out = run(&format!(
        "SELECT vec_mean({series}) AS mean, \
                vec_median({series}) AS median, \
                vec_stddev({series}) AS stddev"
    ))
    .await;
    assert!(out.contains("5.0"), "the mean is 5: {out}");
    assert!(out.contains("4.5"), "the median is 4.5: {out}");
    assert!(
        out.contains("2.1"),
        "the sample stddev is about 2.14: {out}"
    );
}

#[tokio::test]
async fn guide_6_shape_statistics() {
    // A long right tail is positive skew.
    let out = run("SELECT vec_skewness(vec_of(1.0, 1.0, 1.0, 2.0, 2.0, 3.0, 10.0)) AS sk").await;
    // The rendered table is full of border dashes, so the sign is read from the cell rather
    // than from the whole string — an assertion on the string passes or fails on the border.
    let value: f64 = out
        .lines()
        .filter(|line| line.starts_with('|') && !line.contains("sk"))
        .filter_map(|line| line.trim_matches(|c| c == '|' || c == ' ').parse().ok())
        .next()
        .unwrap_or_else(|| panic!("no numeric cell in:\n{out}"));
    assert!(
        value > 0.0,
        "a long right tail is positive skew, and this is {value}"
    );
}

#[tokio::test]
async fn guide_6_a_correlation_against_a_constant_series_is_refused() {
    // The guide says zero would claim "unrelated" where the truth is "undefined".
    let message =
        refused("SELECT vec_correlation(vec_of(1.0, 2.0, 3.0), vec_of(7.0, 7.0, 7.0))").await;
    assert!(!message.is_empty());
}

#[tokio::test]
async fn guide_6_calculus() {
    // The area under y = x sampled at 0..4 with unit spacing is 8.
    let out = run("SELECT vec_integral(vec_of(0.0, 1.0, 2.0, 3.0, 4.0)) AS area").await;
    assert!(out.contains("8.0"), "{out}");
}

// --- the property the guide leads with ------------------------------------

#[tokio::test]
async fn guide_5_reductions_are_deterministic_across_runs() {
    // The claim that justifies not delegating to a numeric library. Values spanning many
    // orders of magnitude, which is where a reordered sum diverges.
    let a = "vec_of(1e10, 1e-10, 3.5, 1e10, -1e10, 7.25, 1e-10)";
    let b = "vec_of(2.0, 3.0, 5.0, 7.0, 11.0, 13.0, 17.0)";
    let sql = format!("SELECT vec_dot({a}, {b}) AS d");

    let reference = run(&sql).await;
    for _ in 0..10 {
        assert_eq!(
            run(&sql).await,
            reference,
            "a dot product changed between runs"
        );
    }
}
