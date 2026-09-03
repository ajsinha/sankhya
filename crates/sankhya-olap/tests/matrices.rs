//! Linear algebra called from SQL over real matrix columns.
//!
//! The kernels are tested in `sankhya-numeric`. What is tested here is that a *column* of
//! matrices works: that the shape reaches the function from field metadata, that a column
//! without a shape is refused rather than guessed at, and that a null matrix produces a null
//! rather than a zero one.

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
use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_olap::matrices::{register, tensor_metadata};
use std::sync::Arc;

/// A column of `rows × columns` matrices, carrying its shape the way a publisher would.
fn matrix_column(
    name: &str,
    rows: usize,
    columns: usize,
    values: &[Option<Vec<f64>>],
) -> (Field, Arc<dyn arrow_array::Array>) {
    let width = i32::try_from(rows * columns).unwrap_or(0);
    let mut builder = FixedSizeListBuilder::new(Float64Builder::new(), width);
    for row in values {
        match row {
            Some(cells) => {
                for cell in cells {
                    builder.values().append_value(*cell);
                }
                builder.append(true);
            }
            None => {
                for _ in 0..width {
                    builder.values().append_null();
                }
                builder.append(false);
            }
        }
    }
    let field = Field::new(
        name,
        DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), width),
        true,
    )
    .with_metadata(tensor_metadata(rows, columns));
    (field, Arc::new(builder.finish()))
}

/// Two 2×2 matrices, one of them missing.
async fn systems() -> SessionContext {
    let (a_field, a) = matrix_column(
        "a",
        2,
        2,
        &[
            Some(vec![1.0, 2.0, 3.0, 4.0]),
            Some(vec![4.0, 7.0, 2.0, 6.0]),
            None,
        ],
    );
    let (b_field, b) = matrix_column(
        "b",
        2,
        2,
        &[
            Some(vec![5.0, 6.0, 7.0, 8.0]),
            Some(vec![1.0, 0.0, 0.0, 1.0]),
            Some(vec![1.0, 0.0, 0.0, 1.0]),
        ],
    );
    let (v_field, v) = matrix_column(
        "v",
        2,
        1,
        &[
            Some(vec![1.0, 1.0]),
            Some(vec![1.0, 1.0]),
            Some(vec![1.0, 1.0]),
        ],
    );

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        a_field,
        b_field,
        v_field,
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(Int64Array::from(vec![1i64, 2, 3])), a, b, v],
    )
    .expect("a valid batch");

    let context = SessionContext::new();
    register(&context);
    sankhya_olap::vectors::register(&context);
    context
        .register_batch("systems", batch)
        .expect("registering");
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

#[tokio::test]
async fn a_determinant_is_computed_over_a_matrix_column() {
    let context = systems().await;
    let out = text(
        &context,
        "SELECT id, mat_determinant(a) AS d FROM systems WHERE id <= 2 ORDER BY id",
    )
    .await;
    // 1*4 - 2*3 = -2, and 4*6 - 7*2 = 10.
    assert!(out.contains("-2"), "{out}");
    assert!(out.contains("10"), "{out}");
}

#[tokio::test]
async fn a_product_is_computed_from_two_matrix_columns() {
    // The second operand's shape comes from its own metadata: without it there is no way to
    // know whether a flat run of four values is 2×2, 1×4 or 4×1.
    let context = systems().await;
    let out = text(
        &context,
        "SELECT mat_multiply(a, b) AS product FROM systems WHERE id = 1",
    )
    .await;
    // [1 2; 3 4] × [5 6; 7 8] = [19 22; 43 50]
    for expected in ["19", "22", "43", "50"] {
        assert!(out.contains(expected), "missing {expected}: {out}");
    }
}

#[tokio::test]
async fn an_inverse_and_a_transpose_come_back_as_arrays() {
    let context = systems().await;
    let inverse = text(
        &context,
        "SELECT mat_inverse(a) AS inv FROM systems WHERE id = 2",
    )
    .await;
    assert!(inverse.contains('['), "an array result: {inverse}");

    let transposed = text(
        &context,
        "SELECT mat_transpose(a) AS t FROM systems WHERE id = 1",
    )
    .await;
    // [1 2; 3 4] transposed is [1 3; 2 4].
    assert!(transposed.contains("1.0"), "{transposed}");
    assert!(transposed.contains("3.0"), "{transposed}");
}

#[tokio::test]
async fn a_solve_and_a_matvec_are_available() {
    let context = systems().await;
    let solved = text(
        &context,
        "SELECT mat_solve(a, v) AS x FROM systems WHERE id = 2",
    )
    .await;
    assert!(solved.contains('['), "{solved}");

    let applied = text(
        &context,
        "SELECT mat_vec(a, v) AS y FROM systems WHERE id = 1",
    )
    .await;
    // [1 2; 3 4] × [1; 1] = [3; 7]
    assert!(
        applied.contains("3.0") && applied.contains("7.0"),
        "{applied}"
    );
}

#[tokio::test]
async fn a_trace_needs_a_square_matrix_and_gets_one() {
    let context = systems().await;
    let out = text(
        &context,
        "SELECT mat_trace(a) AS t FROM systems WHERE id = 1",
    )
    .await;
    assert!(out.contains('5'), "1 + 4 = 5: {out}");
}

#[tokio::test]
async fn a_null_matrix_yields_a_null_and_not_a_zero_one() {
    // A zero matrix is a definite thing. A missing one is not it.
    let context = systems().await;
    let out = text(
        &context,
        "SELECT mat_determinant(a) AS d FROM systems WHERE id = 3",
    )
    .await;
    assert!(
        !out.contains("0.0"),
        "a missing matrix must not read as zero: {out}"
    );
}

#[tokio::test]
async fn a_column_with_no_declared_shape_is_deduced_only_where_a_guess_is_impossible() {
    // **This test asserted the opposite until 2026-09-02**, and the reasoning it carried was
    // sound for the case it had in mind: assuming a matrix is square is wrong for every
    // rectangular one, and produces numbers from values that were never in the same row.
    //
    // What that reasoning missed is that a **stored** matrix column carries no shape. A
    // `FixedSizeList` read back from Parquet has no tensor metadata, so requiring one made
    // `mat_determinant`, `mat_trace`, `mat_inverse` and `mat_solve` unusable on real data ---
    // reachable only through `mat_of(4, 4, ...)`, which takes sixteen scalar arguments and so
    // cannot name a column at all.
    //
    // The distinction that resolves it is not "declared or not" but **whether a guess is
    // possible**. A determinant is defined only on a square matrix, so four values are a 2x2
    // or they are not a determinant's argument; there is nothing to guess wrong. A transpose
    // is defined on both, so sixteen values are a 4x4 or a 2x8 and the guess is real.
    let unshaped = |width: i32, values: Vec<f64>| {
        let mut builder = FixedSizeListBuilder::new(Float64Builder::new(), width);
        for value in values {
            builder.values().append_value(value);
        }
        builder.append(true);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "unshaped",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), width),
            true,
        )]));
        RecordBatch::try_new(schema, vec![Arc::new(builder.finish())]).expect("valid")
    };

    let context = systems().await;
    context
        .register_batch("unshaped", unshaped(4, vec![1.0, 2.0, 3.0, 4.0]))
        .expect("registering");

    // Square-only: deduced, because 1x4 is not a determinant's argument either.
    let answered = context
        .sql("SELECT mat_determinant(unshaped) FROM unshaped")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("a determinant of the only shape these values could have");
    let rendered = datafusion::arrow::util::pretty::pretty_format_batches(&answered)
        .map(|d| d.to_string())
        .unwrap_or_default();
    assert!(rendered.contains("-2"), "1*4 - 2*3 is -2, and this gave {rendered}");

    // Rectangular: still refused, and the refusal still says why the guess is unsafe.
    //
    // Refused at **planning**, because a transpose returns an array and its return type
    // depends on the shape --- so the shape is needed before a row is read. Both stages are
    // accepted here: which one refuses is an implementation detail, and a test that pinned it
    // would fail the day the check moved without anything being wrong.
    let planned = context.sql("SELECT mat_transpose(unshaped) FROM unshaped").await;
    let error = match planned {
        Err(error) => error,
        Ok(frame) => match frame.collect().await {
            Err(error) => error,
            Ok(_) => {
                panic!("a transpose of a column with no shape must be refused: the guess is real")
            }
        },
    };
    assert!(
        error.to_string().contains("no matrix shape"),
        "the refusal must say what is missing: {error}"
    );
}

#[tokio::test]
async fn a_singular_matrix_is_refused_by_the_kernel_and_the_refusal_reaches_sql() {
    let (field, values) = matrix_column("s", 2, 2, &[Some(vec![1.0, 2.0, 2.0, 4.0])]);
    let schema = Arc::new(Schema::new(vec![field]));
    let batch = RecordBatch::try_new(schema, vec![values]).expect("valid");

    let context = systems().await;
    context
        .register_batch("singular", batch)
        .expect("registering");

    let outcome = context
        .sql("SELECT mat_inverse(s) FROM singular")
        .await
        .expect("planning")
        .collect()
        .await;

    let Err(error) = outcome else {
        panic!("a singular matrix must not be inverted anyway");
    };
    assert!(error.to_string().contains("arithmetic noise"), "{error}");
    // And the determinant of the same matrix is zero, which is an answer rather than a
    // failure — the one place the factorisation's refusal means something.
    assert!(text(&context, "SELECT mat_determinant(s) FROM singular")
        .await
        .contains('0'));
}

#[tokio::test]
async fn every_matrix_function_is_registered() {
    // A partially registered set means a query works on one node and fails on another.
    let context = systems().await;
    for (name, arguments) in [
        ("mat_determinant", "a"),
        ("mat_trace", "a"),
        ("mat_transpose", "a"),
        ("mat_inverse", "a"),
        ("mat_multiply", "a, b"),
        ("mat_solve", "a, v"),
        ("mat_vec", "a, v"),
    ] {
        let sql = format!("SELECT {name}({arguments}) FROM systems WHERE id = 2");
        assert!(context.sql(&sql).await.is_ok(), "{name} is not registered");
    }
}
