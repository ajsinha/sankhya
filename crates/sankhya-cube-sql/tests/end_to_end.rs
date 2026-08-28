//! A cube built from a table the session can read, not from a fixture handed to the library.
//!
//! This is the test whose absence let "M7 complete" be claimed while nothing could read a
//! published table. Every other test supplies its own cells; this one supplies a *table*,
//! and the cube has to find it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Float64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_cube::{Definition, Dimension, Level};
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use sankhya_cube_sql::catalog::CubeCatalog;
use sankhya_cube_sql::{publish_from_fact_table, register};
use std::sync::Arc;

const AMOUNT: Measure = Measure {
    name: "amount",
    rules: &[
        Along { dimension: "region", rule: Rule::Sum },
        Along { dimension: "period", rule: Rule::Sum },
    ],
};

fn cube() -> Arc<sankhya_cube::model::Cube> {
    Arc::new(
        Definition::new(
            "figures",
            "fact_figures",
            vec![
                Dimension::new("region", "dim_region", "region_key", vec![Level::new("id", "id")]),
                Dimension::new("period", "dim_period", "period_key", vec![Level::new("id", "id")]),
            ],
            vec![AMOUNT],
        )
        .validate()
        .expect("well-formed"),
    )
}

/// Register `fact_figures` as an ordinary table in the session.
fn with_facts(
    regions: Vec<Option<&str>>,
    periods: Vec<Option<&str>>,
    amounts: Vec<Option<f64>>,
) -> SessionContext {
    let schema = Arc::new(Schema::new(vec![
        Field::new("region_key", DataType::Utf8, true),
        Field::new("period_key", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(regions)),
            Arc::new(StringArray::from(periods)),
            Arc::new(Float64Array::from(amounts)),
        ],
    )
    .expect("well-formed");

    let context = SessionContext::new();
    context.register_batch("fact_figures", batch).expect("registered");
    context
}

async fn scalar(context: &SessionContext, sql: &str) -> String {
    let batches = context.sql(sql).await.expect("planned").collect().await.expect("ran");
    let batch = &batches[0];
    format!(
        "{:?}",
        datafusion::common::ScalarValue::try_from_array(batch.column(0), 0).expect("scalar")
    )
}

#[tokio::test]
async fn a_cube_is_built_from_the_table_its_definition_names_and_then_queried() {
    let context = with_facts(
        vec![Some("north"), Some("north"), Some("south")],
        vec![Some("jan"), Some("feb"), Some("jan")],
        vec![Some(1.0), Some(2.0), Some(4.0)],
    );
    let catalog = Arc::new(CubeCatalog::new());
    register(&context, Arc::clone(&catalog));

    let absorbed = publish_from_fact_table(&context, &catalog, "figures", cube(), &AMOUNT, 11)
        .await
        .expect("hydrated");
    assert_eq!(absorbed.placed, 3);
    assert_eq!(absorbed.unplaced, 0);

    // And the cube answers, over data nothing in this test shaped into cells.
    let total = scalar(
        &context,
        "SELECT sum(amount) FROM cube_rollup('figures', 'amount', 'by=region')",
    )
    .await;
    assert!(total.contains('7'), "1 + 2 + 4: {total}");

    let north = scalar(
        &context,
        "SELECT amount FROM cube_rollup('figures', 'amount', \
         'by=region, where=region:north')",
    )
    .await;
    assert!(north.contains('3'), "1 + 2: {north}");
}

#[tokio::test]
async fn rows_that_could_not_be_placed_reach_the_completeness_column() {
    // The property that makes the column mean anything. A query counting the rows that
    // survived would report every result complete for ever — the trap
    // `sankhya_cube::complete` documents, and the one this surface walked into first time.
    let context = with_facts(
        vec![Some("north"), None, Some("south"), Some("south")],
        vec![Some("jan"), Some("jan"), Some("jan"), Some("feb")],
        vec![Some(1.0), Some(2.0), Some(4.0), None],
    );
    let catalog = Arc::new(CubeCatalog::new());
    register(&context, Arc::clone(&catalog));

    let absorbed = publish_from_fact_table(&context, &catalog, "figures", cube(), &AMOUNT, 11)
        .await
        .expect("hydrated");
    assert_eq!(absorbed.rows, 4);
    assert_eq!(absorbed.placed, 2, "a null key and a null measure are both unplaced");
    assert_eq!(absorbed.unplaced, 2);

    let completeness = scalar(
        &context,
        "SELECT completeness FROM cube_rollup('figures', 'amount', 'by=region') LIMIT 1",
    )
    .await;
    assert!(completeness.contains("0.5"), "half the rows reached the cube: {completeness}");

    let withheld = scalar(
        &context,
        "SELECT withheld FROM cube_rollup('figures', 'amount', 'by=region') LIMIT 1",
    )
    .await;
    assert!(withheld.contains('2'), "{withheld}");
}

#[tokio::test]
async fn a_completeness_threshold_refuses_a_cube_that_lost_rows() {
    // FR-QUERY-13 end to end: the query fails rather than returning a flattering result.
    let context = with_facts(
        vec![Some("north"), None],
        vec![Some("jan"), Some("jan")],
        vec![Some(1.0), Some(2.0)],
    );
    let catalog = Arc::new(CubeCatalog::new());
    register(&context, Arc::clone(&catalog));
    publish_from_fact_table(&context, &catalog, "figures", cube(), &AMOUNT, 11)
        .await
        .expect("hydrated");

    let refused = context
        .sql(
            "SELECT * FROM cube_rollup('figures', 'amount', \
             'by=region, min_completeness=0.95')",
        )
        .await
        .expect_err("returned a partial total");
    assert!(refused.to_string().contains("50.0%"), "{refused}");
}

#[tokio::test]
async fn a_fact_table_missing_a_dimension_column_is_refused_at_hydration() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("region_key", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![Some("north")])),
            Arc::new(Float64Array::from(vec![Some(1.0)])),
        ],
    )
    .expect("well-formed");
    let context = SessionContext::new();
    context.register_batch("fact_figures", batch).expect("registered");
    let catalog = Arc::new(CubeCatalog::new());

    let refused = publish_from_fact_table(&context, &catalog, "figures", cube(), &AMOUNT, 1)
        .await
        .expect_err("hydrated a table missing a dimension");
    assert!(refused.to_string().contains("period_key"), "{refused}");
}

#[tokio::test]
async fn a_definition_naming_a_table_that_does_not_exist_is_refused() {
    // The failure that would otherwise be an empty cube: a cube over nothing looks exactly
    // like a cube whose facts have not arrived.
    let context = SessionContext::new();
    let catalog = Arc::new(CubeCatalog::new());
    assert!(
        publish_from_fact_table(&context, &catalog, "figures", cube(), &AMOUNT, 1)
            .await
            .is_err()
    );
}
