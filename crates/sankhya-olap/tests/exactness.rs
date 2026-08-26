//! An approximate function cannot answer an exact question by accident.
//!
//! Every test here plans a real query against a real engine, because the thing being
//! asserted is that these functions are *findable* in a plan the engine produced — not
//! that a string appears in some SQL text.

use datafusion::prelude::SessionContext;
use sankhya_olap::{check_exactness, Exactness, Watermark};

async fn plan(sql: &str) -> datafusion::logical_expr::LogicalPlan {
    let ctx = SessionContext::new();
    ctx.sql(
        "CREATE TABLE readings AS \
         SELECT * FROM (VALUES (1, 10.0), (2, 20.0), (3, 30.0)) AS t(id, amount)",
    )
    .await
    .expect("creating a table")
    .collect()
    .await
    .expect("materialising");

    ctx.state()
        .create_logical_plan(sql)
        .await
        .expect("planning")
}

#[tokio::test]
async fn an_exact_query_passes_and_is_watermarked_exact() {
    let p = plan("SELECT SUM(amount), COUNT(DISTINCT id) FROM readings").await;
    let watermark = check_exactness(&p, Exactness::Required).expect("an exact query");
    assert!(watermark.is_exact());
    assert_eq!(format!("{watermark}"), "exact");
}

#[tokio::test]
async fn an_approximate_percentile_is_refused() {
    let p = plan("SELECT approx_percentile_cont(amount, 0.99) FROM readings").await;
    let err = check_exactness(&p, Exactness::Required).expect_err("must be refused");
    assert!(err.functions.contains("approx_percentile_cont"));
    assert!(format!("{err}").contains("merge-order dependent"));
}

#[tokio::test]
async fn an_approximate_distinct_is_refused() {
    let p = plan("SELECT approx_distinct(id) FROM readings").await;
    let err = check_exactness(&p, Exactness::Required).expect_err("must be refused");
    assert!(err.functions.contains("approx_distinct"));
}

#[tokio::test]
async fn an_approximate_median_is_refused() {
    let p = plan("SELECT approx_median(amount) FROM readings").await;
    let err = check_exactness(&p, Exactness::Required).expect_err("must be refused");
    assert!(err.functions.contains("approx_median"));
}

#[tokio::test]
async fn every_offending_function_is_named_at_once() {
    // Fixing one and re-running to discover the next is a worse experience than being
    // told once.
    let p = plan("SELECT approx_median(amount), approx_distinct(id) FROM readings").await;
    let err = check_exactness(&p, Exactness::Required).expect_err("must be refused");
    assert_eq!(err.functions.len(), 2);
}

#[tokio::test]
async fn approximation_hidden_in_a_subquery_is_still_found() {
    // A scalar subquery is exactly where one would go unnoticed: the outer query looks
    // entirely exact.
    let p = plan(
        "SELECT SUM(amount) FROM readings \
         WHERE amount > (SELECT approx_median(amount) FROM readings)",
    )
    .await;
    let err = check_exactness(&p, Exactness::Required).expect_err("must be refused");
    assert!(err.functions.contains("approx_median"));
}

#[tokio::test]
async fn approximation_in_a_having_clause_is_still_found() {
    let p = plan(
        "SELECT id, SUM(amount) FROM readings GROUP BY id \
         HAVING approx_median(amount) > 5",
    )
    .await;
    let err = check_exactness(&p, Exactness::Required).expect_err("must be refused");
    assert!(err.functions.contains("approx_median"));
}

#[tokio::test]
async fn a_permissive_session_allows_it_and_still_says_so() {
    // The mode where approximation is fine. It runs -- and the caller is told, because
    // silence here would make the two modes differ only in whether the query executes,
    // losing the information exactly where someone is about to write the number down.
    let p = plan("SELECT approx_median(amount) FROM readings").await;
    let watermark = check_exactness(&p, Exactness::Permitted).expect("permitted");

    assert!(!watermark.is_exact());
    assert!(watermark.approximate_functions.contains("approx_median"));
    assert!(format!("{watermark}").contains("not reproducible"));
}

#[tokio::test]
async fn exactness_is_required_by_default() {
    // The default has to be the one whose failure is loud. A query that is refused is a
    // question; a figure that is quietly approximate is an answer.
    assert_eq!(Exactness::default(), Exactness::Required);
}

#[tokio::test]
async fn the_watermark_is_stable_across_runs() {
    // A watermark that reordered itself would look like a different watermark, and the
    // whole point is to compare it against what a figure was published with.
    let p = plan("SELECT approx_median(amount), approx_distinct(id) FROM readings").await;
    let first = check_exactness(&p, Exactness::Permitted).expect("permitted");
    let second = check_exactness(&p, Exactness::Permitted).expect("permitted");
    assert_eq!(first, second);
    assert_eq!(
        first.approximate_functions.iter().collect::<Vec<_>>(),
        vec!["approx_distinct", "approx_median"]
    );
}

#[tokio::test]
async fn an_exact_query_is_watermarked_the_same_in_both_modes() {
    let p = plan("SELECT SUM(amount) FROM readings").await;
    assert_eq!(
        check_exactness(&p, Exactness::Required).expect("exact"),
        Watermark::default()
    );
    assert_eq!(
        check_exactness(&p, Exactness::Permitted).expect("exact"),
        Watermark::default()
    );
}
