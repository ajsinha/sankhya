//! Reading a column of vectors, borrowed rather than copied.
//!
//! # Why this has tests of its own
//!
//! It is the hot path of every function that reads an array, and two of the three ways it can
//! be wrong return **numbers** rather than failing: a stride read from the wrong offset gives
//! another row's values, and a buffer that is not cleared between rows gives this row's values
//! appended to the last one's. Both look like answers.
//!
//! The third — treating a null row as an empty one — is the rule this whole system keeps
//! restating: an empty series is a definite statement, *nothing was measured*, and a missing
//! vector is not that.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use arrow_array::builder::{FixedSizeListBuilder, Float64Builder};
use arrow_array::{Array, ArrayRef};
use datafusion::prelude::SessionContext;
use sankhya_functions::rows::Vectors;
use std::sync::Arc;

/// A column of `rows` vectors of `width`, where row *i* holds `i·100 + 0..width`.
///
/// Values that say which row they came from, so a stride read from the wrong place is visible
/// in the number rather than only in a checksum.
fn column(rows: usize, width: usize, nulls_at: &[usize]) -> ArrayRef {
    let mut builder =
        FixedSizeListBuilder::new(Float64Builder::new(), i32::try_from(width).unwrap_or(0));
    for row in 0..rows {
        if nulls_at.contains(&row) {
            // A null at the list level: the child still holds values for the slot, which is
            // exactly why a reader that ignored the null mask would return them.
            for _ in 0..width {
                builder.values().append_value(-999.0);
            }
            builder.append(false);
        } else {
            for at in 0..width {
                #[allow(clippy::cast_precision_loss)]
                builder.values().append_value(row as f64 * 100.0 + at as f64);
            }
            builder.append(true);
        }
    }
    Arc::new(builder.finish())
}

#[test]
fn each_row_is_its_own_values_and_not_a_neighbours() {
    let array = column(5, 4, &[]);
    let mut vectors = Vectors::read(&array, "test").expect("a readable column");
    for row in 0..5 {
        let values = vectors.row(row).expect("a row");
        #[allow(clippy::cast_precision_loss)]
        let base = row as f64 * 100.0;
        assert_eq!(values, &[base, base + 1.0, base + 2.0, base + 3.0], "row {row}");
    }
}

#[test]
fn a_sliced_column_reads_the_rows_it_actually_holds() {
    // The failure that returns numbers. A column sliced by a `LIMIT` or a filter starts partway
    // into its child buffer, and a reader that assumed zero reads whichever rows happen to sit
    // at that offset --- with no error anywhere.
    let array = column(6, 3, &[]);
    let sliced = array.slice(2, 3);

    let mut vectors = Vectors::read(&sliced, "test").expect("a readable column");
    for (position, expected_row) in (2..5).enumerate() {
        let values = vectors.row(position).expect("a row");
        #[allow(clippy::cast_precision_loss)]
        let base = expected_row as f64 * 100.0;
        assert_eq!(
            values,
            &[base, base + 1.0, base + 2.0],
            "position {position} of the slice should be row {expected_row}"
        );
    }
}

#[test]
fn a_null_row_is_none_rather_than_the_values_underneath_it() {
    // A null at the list level leaves values in the child --- the builder wrote them. A reader
    // that only used the stride would return them, and they are not this row's data; they are
    // not anybody's.
    let array = column(4, 3, &[1, 3]);
    let mut vectors = Vectors::read(&array, "test").expect("a readable column");

    assert!(vectors.row(0).is_some());
    assert_eq!(vectors.row(1), None, "a null row returned values");
    assert!(vectors.row(2).is_some());
    assert_eq!(vectors.row(3), None);

    // And nothing returns the sentinel the builder wrote, which is what a stride-only read
    // would produce.
    for row in [0usize, 2] {
        let values = vectors.row(row).expect("a row");
        assert!(!values.contains(&-999.0), "row {row} read a null slot's values");
    }
}

#[test]
fn a_column_that_is_not_doubles_is_refused_by_name() {
    let ints: ArrayRef = Arc::new(arrow_array::Int64Array::from(vec![1_i64, 2, 3]));
    let said = match Vectors::read(&ints, "vec_sum") {
        Ok(_) => panic!("a column of integers was read as an array of doubles"),
        Err(refused) => refused.to_string(),
    };
    assert!(said.contains("vec_sum"), "{said}");
    assert!(said.contains("Refused rather than coerced"), "{said}");
}

// --- the same properties, reached through a statement ---------------------

fn session() -> SessionContext {
    let context = SessionContext::new();
    sankhya_functions::register(&context);
    sankhya_olap::register_constructors(&context);
    sankhya_olap::register_vector_functions(&context);
    context
}

async fn run(sql: &str) -> String {
    let batches = session()
        .sql(sql)
        .await
        .unwrap_or_else(|error| panic!("did not plan: {sql}\n{error}"))
        .collect()
        .await
        .unwrap_or_else(|error| panic!("did not run: {sql}\n{error}"));
    datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_many_argument_function_over_several_rows_does_not_carry_one_row_into_the_next() {
    // The buffers are reused across rows, which is the whole point --- and a buffer reused
    // without being cleared appends this row's values to the last one's. The regression would
    // then be fitted over a series that grows by one row every row, and every answer after the
    // first would be wrong while looking entirely plausible.
    //
    // **One batch**, from a registered table. Two earlier versions of this test could not fail:
    // the first used `vec_correlation`, which belongs to a different wrapper; the second used
    // `UNION ALL`, where each branch is its own batch, so the buffers were rebuilt between
    // rows and never accumulated.
    let context = session();
    let width = 4i32;
    let mut xs = FixedSizeListBuilder::new(Float64Builder::new(), width);
    let mut ys = FixedSizeListBuilder::new(Float64Builder::new(), width);
    // Three rows with **different** slopes, so accumulation changes the answer. Identical
    // slopes would fit the same line however the rows were run together, and the test would
    // pass by accident.
    for slope in [2.0f64, 3.0, 5.0] {
        for at in 1..=4 {
            let x = f64::from(at);
            xs.values().append_value(x);
            ys.values().append_value(slope * x);
        }
        xs.append(true);
        ys.append(true);
    }
    let batch = arrow_array::RecordBatch::try_new(
        Arc::new(arrow_schema::Schema::new(vec![
            arrow_schema::Field::new(
                "x",
                arrow_schema::DataType::FixedSizeList(
                    Arc::new(arrow_schema::Field::new("item", arrow_schema::DataType::Float64, true)),
                    width,
                ),
                false,
            ),
            arrow_schema::Field::new(
                "y",
                arrow_schema::DataType::FixedSizeList(
                    Arc::new(arrow_schema::Field::new("item", arrow_schema::DataType::Float64, true)),
                    width,
                ),
                false,
            ),
        ])),
        vec![Arc::new(xs.finish()), Arc::new(ys.finish())],
    )
    .expect("a batch");
    assert_eq!(batch.num_rows(), 3, "the whole point is several rows in one batch");
    context.register_batch("fits", batch).expect("registering");

    let frame = context
        .sql("SELECT regress_slope(x, y) AS r FROM fits")
        .await
        .expect("plans");
    let batches = frame.collect().await.expect("runs");
    let out = datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .map(|d| d.to_string())
        .unwrap_or_default();

    let answers: Vec<&str> = out
        .lines()
        .filter_map(|line| line.strip_prefix("| ").and_then(|l| l.split('|').next()))
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "r")
        .collect();
    assert_eq!(answers.len(), 3, "expected three rows: {out}");
    for (row, answer) in answers.iter().enumerate() {
        let value: f64 = answer.parse().unwrap_or(f64::NAN);
        #[allow(clippy::indexing_slicing)]
        let expected = [2.0, 3.0, 5.0][row];
        assert!(
            (value - expected).abs() < 1e-9,
            "row {row} should have slope {expected} and gave {answer}: {out}"
        );
    }
}

#[tokio::test]
async fn a_null_vector_gives_a_null_answer_through_a_statement_too() {
    let out = run(
        "SELECT jarque_bera_p(v) AS p FROM (
            SELECT CASE WHEN 1 = 0 THEN vec_of(1.0, 2.0, 3.0, 4.0) END AS v
         )",
    )
    .await;
    assert!(!out.contains("0.0"), "a null series became a number: {out}");
}
