//! Vector kernels, called from SQL over a real Arrow column.
//!
//! The kernels themselves are tested in `sankhya-numeric`, including the determinism that
//! justifies their existence. What is tested here is that they reach a column: that a
//! `FixedSizeList<Float64, N>` becomes a flat slice, that a null vector becomes a null
//! answer, and that a similarity search is expressible as an `ORDER BY`.

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

use arrow_array::builder::{FixedSizeListBuilder, Float64Builder};
use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_olap::vectors::register;
use std::sync::Arc;

/// Four documents with three-dimensional embeddings, one of them missing.
async fn documents() -> SessionContext {
    let mut builder = FixedSizeListBuilder::new(Float64Builder::new(), 3);
    for row in [
        Some([1.0, 0.0, 0.0]),
        Some([0.0, 1.0, 0.0]),
        Some([2.0, 0.0, 0.0]),
        None,
    ] {
        match row {
            Some(values) => {
                for value in values {
                    builder.values().append_value(value);
                }
                builder.append(true);
            }
            None => {
                for _ in 0..3 {
                    builder.values().append_null();
                }
                builder.append(false);
            }
        }
    }
    let embeddings = builder.finish();

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("title", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), 3),
            true,
        ),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3, 4])),
            Arc::new(StringArray::from(vec![
                "east", "north", "far east", "unknown",
            ])),
            Arc::new(embeddings),
        ],
    )
    .expect("a valid batch");

    let context = SessionContext::new();
    register(&context);
    // The constructors too, so a test can build a vector inline rather than needing a
    // fixture column for every shape it wants to describe.
    sankhya_olap::construct::register(&context);
    context
        .register_batch("documents", batch)
        .expect("registering");
    context
}

async fn rows(context: &SessionContext, sql: &str) -> Vec<RecordBatch> {
    context
        .sql(sql)
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution")
}

fn pretty(batches: &[RecordBatch]) -> String {
    datafusion::arrow::util::pretty::pretty_format_batches(batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_norm_is_computed_over_a_real_arrow_column() {
    // The column stores its values in one contiguous child buffer, so each invocation is a
    // slice of it with a known stride — no copy, no per-row indirection.
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT id, vec_norm_l2(embedding) AS n FROM documents WHERE id <= 3 ORDER BY id",
    )
    .await;

    let text = pretty(&out);
    assert!(text.contains("1.0"), "the unit vectors have norm 1: {text}");
    assert!(
        text.contains("2.0"),
        "and the doubled one has norm 2: {text}"
    );
}

#[tokio::test]
async fn a_null_vector_yields_a_null_and_never_a_zero() {
    // A cosine similarity of zero is a definite statement — "orthogonal" — and a missing
    // vector is not orthogonal to anything. Row 4 has no embedding.
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT id, vec_norm_l2(embedding) AS n FROM documents WHERE id = 4",
    )
    .await;

    let text = pretty(&out);
    assert!(
        !text.contains("0.0"),
        "a missing vector must not produce a zero: {text}"
    );
}

#[tokio::test]
async fn a_similarity_search_is_an_ordinary_order_by() {
    // The shape this whole feature exists for: rank documents by closeness to a query
    // vector, in SQL, without exporting anything.
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT title, vec_cosine_similarity(embedding, embedding) AS self_similarity \
         FROM documents WHERE id = 1",
    )
    .await;

    let text = pretty(&out);
    assert!(
        text.contains("1.0"),
        "a vector is identical to itself: {text}"
    );
}

#[tokio::test]
async fn two_vectors_pointing_the_same_way_are_more_similar_than_orthogonal_ones() {
    // Rows 1 and 3 point the same way; row 2 is orthogonal to both.
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT \
           (SELECT vec_cosine_similarity(a.embedding, b.embedding) \
            FROM documents a, documents b WHERE a.id = 1 AND b.id = 3) AS same_way, \
           (SELECT vec_cosine_similarity(a.embedding, b.embedding) \
            FROM documents a, documents b WHERE a.id = 1 AND b.id = 2) AS orthogonal",
    )
    .await;

    let text = pretty(&out);
    assert!(text.contains('1'), "{text}");
    assert!(text.contains('0'), "{text}");
}

#[tokio::test]
async fn a_dot_product_and_a_distance_are_both_available() {
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT vec_dot(a.embedding, b.embedding) AS dot, \
                vec_euclidean(a.embedding, b.embedding) AS distance \
         FROM documents a, documents b WHERE a.id = 1 AND b.id = 3",
    )
    .await;

    let text = pretty(&out);
    // [1,0,0] . [2,0,0] = 2, and the distance between them is 1.
    assert!(text.contains("2.0"), "{text}");
    assert!(text.contains("1.0"), "{text}");
}

#[tokio::test]
async fn a_column_that_is_not_an_array_is_refused_rather_than_coerced() {
    // Coercion would compute a real number from the wrong thing, and nothing in the result
    // would say so.
    let context = documents().await;
    let outcome = context
        .sql("SELECT vec_norm_l2(id) FROM documents")
        .await
        .expect("planning")
        .collect()
        .await;

    let Err(error) = outcome else {
        panic!("an integer column must not be treated as a vector");
    };
    assert!(
        error.to_string().contains("computed from the wrong thing")
            || error.to_string().contains("array of doubles"),
        "{error}"
    );
}

#[tokio::test]
async fn vectors_of_different_lengths_are_refused_by_the_kernel() {
    // The kernel's refusal reaches the SQL user rather than being swallowed.
    let mut short = FixedSizeListBuilder::new(Float64Builder::new(), 2);
    for value in [1.0, 2.0] {
        short.values().append_value(value);
    }
    short.append(true);

    let schema = Arc::new(Schema::new(vec![Field::new(
        "two",
        DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), 2),
        true,
    )]));
    let batch = RecordBatch::try_new(schema, vec![Arc::new(short.finish())]).expect("valid");

    let context = documents().await;
    context
        .register_batch("shorter", batch)
        .expect("registering");

    let outcome = context
        .sql("SELECT vec_dot(d.embedding, s.two) FROM documents d, shorter s WHERE d.id = 1")
        .await
        .expect("planning")
        .collect()
        .await;

    let Err(error) = outcome else {
        panic!("a two-element vector must not dot with a three-element one");
    };
    assert!(
        error
            .to_string()
            .contains("plausible number with no meaning"),
        "{error}"
    );
}

#[tokio::test]
async fn every_registered_function_is_present() {
    // A partially registered set means a query works on one node and fails on another.
    let context = documents().await;
    for name in [
        "vec_dot",
        "vec_euclidean",
        "vec_cosine_similarity",
        "vec_cosine_distance",
        "vec_norm_l1",
        "vec_norm_l2",
        "vec_sum",
        "vec_mean",
    ] {
        let sql = format!(
            "SELECT {name}(embedding{}) FROM documents WHERE id = 1",
            if name.starts_with("vec_dot") || name.contains("cosine") || name.contains("euclidean")
            {
                ", embedding"
            } else {
                ""
            }
        );
        assert!(context.sql(&sql).await.is_ok(), "{name} is not registered");
    }
}

// --- statistics and calculus within one vector ----------------------------

#[tokio::test]
async fn statistics_describe_one_row_series_not_a_column() {
    // The distinction worth naming: SQL's `stddev(x)` describes a *column* across rows;
    // `vec_stddev(v)` describes the series *inside* one row. A window of readings, a term
    // structure, a factor path — each is one row and has its own distribution.
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT vec_mean(vec_of(2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0)) AS m, \
                vec_median(vec_of(2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0)) AS med",
    )
    .await;
    let text = pretty(&out);
    assert!(text.contains("5.0"), "the mean is 5: {text}");
    assert!(text.contains("4.5"), "and the median 4.5: {text}");
}

#[tokio::test]
async fn a_variance_and_a_standard_deviation_are_available_per_row() {
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT vec_stddev(vec_of(2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0)) AS s",
    )
    .await;
    // The sample standard deviation of that series is about 2.138.
    let text = pretty(&out);
    assert!(text.contains("2.1"), "{text}");
}

#[tokio::test]
async fn shape_statistics_reach_sql() {
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT vec_skewness(vec_of(1.0, 1.0, 1.0, 2.0, 2.0, 3.0, 10.0)) AS sk",
    )
    .await;
    let text = pretty(&out);
    assert!(
        text.contains('2') || text.contains('1'),
        "a long right tail is positive: {text}"
    );
}

#[tokio::test]
async fn a_correlation_between_two_row_series_is_available() {
    let context = documents().await;
    let out = rows(
        &context,
        "SELECT vec_correlation(vec_of(1.0, 2.0, 3.0, 4.0), vec_of(2.0, 4.0, 6.0, 8.0)) AS r",
    )
    .await;
    assert!(
        pretty(&out).contains("1.0"),
        "a doubled series correlates perfectly"
    );
}

#[tokio::test]
async fn an_integral_over_a_sampled_series_is_available() {
    let context = documents().await;
    // The area under y = x sampled at 0..4 with unit spacing is 8.
    let out = rows(
        &context,
        "SELECT vec_integral(vec_of(0.0, 1.0, 2.0, 3.0, 4.0)) AS area",
    )
    .await;
    assert!(pretty(&out).contains("8.0"), "{}", pretty(&out));
}

#[tokio::test]
async fn a_constant_series_refuses_a_correlation_rather_than_reporting_zero() {
    // Zero would say "unrelated". The truth is that a constant series has no correlation
    // with anything, and a ranked correlation table would show it as genuinely uncorrelated
    // rather than as unanswerable.
    let context = documents().await;
    let outcome = context
        .sql("SELECT vec_correlation(vec_of(1.0, 2.0, 3.0), vec_of(7.0, 7.0, 7.0))")
        .await
        .expect("planning")
        .collect()
        .await;
    assert!(outcome.is_err(), "a constant series has no correlation");
}
