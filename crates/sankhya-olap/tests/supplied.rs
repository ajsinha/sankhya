//! A user's own aggregation, called from SQL over a real table.
//!
//! # The claim these make
//!
//! That a declared aggregation is an aggregate like any other: it works in a `GROUP BY`, it
//! partitions and recombines, and the number it gives over groups equals the number it gives
//! over the whole. The last of those is what `merge` claims and what would otherwise be
//! believed.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
#![allow(clippy::print_stdout)]

use arrow_array::{Array, Float64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_olap::supplied::Supplied;
use sankhya_udf::Worker;
use std::path::Path;
use std::sync::Arc;

/// A weighted mean: exactly the rule a cube cannot express and a firm always has.
const WEIGHTED: &str = r#"
def initial():
    return {"total": 0.0, "weight": 0.0}

def accumulate(state, values):
    for i in range(0, len(values) - 1, 2):
        state["total"] += values[i] * values[i + 1]
        state["weight"] += values[i + 1]
    return state

def merge(a, b):
    return {"total": a["total"] + b["total"], "weight": a["weight"] + b["weight"]}

def finish(state):
    return state["total"] / state["weight"] if state["weight"] else 0.0
"#;

/// Four rows in two groups, with weights that make an unweighted mean a different number.
fn table() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("area", DataType::Utf8, false),
        Field::new("rate", DataType::Float64, false),
        Field::new("weight", DataType::Float64, false),
    ]));
    RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(vec!["north", "north", "south", "south"])),
            Arc::new(Float64Array::from(vec![10.0, 20.0, 4.0, 8.0])),
            Arc::new(Float64Array::from(vec![1.0, 3.0, 3.0, 1.0])),
        ],
    )
    .expect("a valid batch")
}

/// A session with the aggregation registered, or `None` where the boundary cannot be built.
async fn session() -> Option<SessionContext> {
    let python = Path::new("/usr/bin/python3");
    if !python.exists() {
        println!("SKIPPED: no /usr/bin/python3");
        return None;
    }
    let worker = match Worker::start(python) {
        Ok(worker) => Arc::new(worker),
        Err(refused) => {
            println!("SKIPPED: {refused}");
            return None;
        }
    };
    let declared = worker.declare("weighted_mean", WEIGHTED).expect("it is deterministic");

    let context = SessionContext::new();
    context.register_udaf(Supplied::new(Arc::new(declared), worker));
    context.register_batch("readings", table()).expect("a table");
    Some(context)
}

async fn numbers(context: &SessionContext, sql: &str) -> Vec<(String, f64)> {
    let batches = context.sql(sql).await.expect("it plans").collect().await.expect("it runs");
    let mut out = Vec::new();
    for batch in batches {
        let keys = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("a key column")
            .clone();
        let values = batch
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("a numeric answer")
            .clone();
        for row in 0..batch.num_rows() {
            out.push((keys.value(row).to_owned(), values.value(row)));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_aggregation_answers_a_group_by() {
    let Some(context) = session().await else { return };
    let answers = numbers(
        &context,
        "SELECT area, weighted_mean(rate, weight) FROM readings GROUP BY area",
    )
    .await;

    // north: (10*1 + 20*3) / 4 = 17.5   --- an unweighted mean would be 15
    // south: (4*3 + 8*1) / 4  = 5.0     --- an unweighted mean would be 6
    assert_eq!(
        answers,
        vec![("north".to_owned(), 17.5), ("south".to_owned(), 5.0)],
        "the weighted mean per group, and deliberately different from the unweighted one --- a \
         fixture where the two agreed would pass for an implementation that ignored the weights"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_whole_equals_what_its_parts_merge_to() {
    // What a declared `merge` claims. DataFusion computes a grouped aggregate by partitioning,
    // accumulating each partition apart and merging the partials --- so this is not a
    // hypothetical roll-up, it is what the plan already does.
    let Some(context) = session().await else { return };

    let grouped = numbers(
        &context,
        "SELECT area, weighted_mean(rate, weight) FROM readings GROUP BY area",
    )
    .await;
    let whole = numbers(
        &context,
        "SELECT 'all' AS area, weighted_mean(rate, weight) FROM readings",
    )
    .await;

    // (10*1 + 20*3 + 4*3 + 8*1) / 8 = 90/8 = 11.25
    assert_eq!(whole, vec![("all".to_owned(), 11.25)]);

    // And the ungrouped answer is what the two groups' states merge to, which is the property
    // that makes it safe to answer a coarse question from a fine cuboid.
    let north = grouped.first().map(|(_, v)| *v).unwrap_or_default();
    let south = grouped.get(1).map(|(_, v)| *v).unwrap_or_default();
    assert!(
        (north - 17.5).abs() < f64::EPSILON && (south - 5.0).abs() < f64::EPSILON,
        "the groups: {grouped:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_null_argument_skips_the_whole_row() {
    // A weighted mean handed a null weight and a present value must not be told the weight was
    // zero. Zero is a number and is not what a missing weight means, and the difference is a
    // figure that looks ordinary.
    let Some(context) = session().await else { return };
    let schema = Arc::new(Schema::new(vec![
        Field::new("area", DataType::Utf8, false),
        Field::new("rate", DataType::Float64, true),
        Field::new("weight", DataType::Float64, true),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(vec!["north", "north", "north", "north"])),
            // The fourth row's *value* is missing, not its weight, and that is the row that
            // makes this test able to fail. A null weight beside a present value reads as a
            // weight of zero, which contributes nothing and leaves the answer unchanged --- so
            // a fixture with only that row would pass for an implementation that ignored nulls
            // entirely. A null *value* beside a present weight would add five to the divisor
            // and nothing to the numerator, and 17.5 would quietly become 70/9.
            Arc::new(Float64Array::from(vec![Some(10.0), Some(20.0), Some(99.0), None])),
            Arc::new(Float64Array::from(vec![Some(1.0), Some(3.0), None, Some(5.0)])),
        ],
    )
    .expect("a valid batch");
    context.register_batch("holed", batch).expect("a table");

    let answers = numbers(
        &context,
        "SELECT area, weighted_mean(rate, weight) FROM holed GROUP BY area",
    )
    .await;
    assert_eq!(
        answers,
        vec![("north".to_owned(), 17.5)],
        "a row with a missing argument contributes nothing at all. Read as zero instead, the \
         missing *weight* would leave this answer unchanged and the missing *value* would make \
         it 70/9 --- which is why the fixture has both"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_with_no_rows_is_null_rather_than_zero() {
    // Every aggregate in this system keeps "nothing arrived" and "it summed to zero" apart.
    let Some(context) = session().await else { return };
    let batches = context
        .sql("SELECT weighted_mean(rate, weight) FROM readings WHERE area = 'nowhere'")
        .await
        .expect("it plans")
        .collect()
        .await
        .expect("it runs");
    let column = batches
        .first()
        .map(|batch| Arc::clone(batch.column(0)))
        .expect("one row of answer");
    let values = column
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("a numeric answer");
    assert!(values.is_null(0), "no rows means no answer, not zero");
}
