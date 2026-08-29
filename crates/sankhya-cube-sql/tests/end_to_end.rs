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

fn amount() -> Measure {
    Measure::new("amount", vec![
        Along::new("region", Rule::Sum),
        Along::new("period", Rule::Sum),
    ])
}

fn cube() -> Arc<sankhya_cube::model::Cube> {
    Arc::new(
        Definition::new(
            "figures",
            "fact_figures",
            vec![
                Dimension::new("region", "dim_region", "region_key", vec![Level::new("id", "id")]),
                Dimension::new("period", "dim_period", "period_key", vec![Level::new("id", "id")]),
            ],
            vec![amount()],
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
    register(&context, Arc::clone(&catalog), Arc::new(sankhya_cube::querylog::QueryLog::new()));

    let absorbed = publish_from_fact_table(&context, &catalog, "figures", cube(), &amount(), 11)
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
    register(&context, Arc::clone(&catalog), Arc::new(sankhya_cube::querylog::QueryLog::new()));

    let absorbed = publish_from_fact_table(&context, &catalog, "figures", cube(), &amount(), 11)
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
    register(&context, Arc::clone(&catalog), Arc::new(sankhya_cube::querylog::QueryLog::new()));
    publish_from_fact_table(&context, &catalog, "figures", cube(), &amount(), 11)
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

    let refused = publish_from_fact_table(&context, &catalog, "figures", cube(), &amount(), 1)
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
        publish_from_fact_table(&context, &catalog, "figures", cube(), &amount(), 1)
            .await
            .is_err()
    );
}

// --- the cross-check: two engines, one answer ---------------------------

/// A fact table of `n` rows over three dimensions.
///
/// Values are small integers held as `f64`, so every partial sum is exactly representable
/// and the comparison below can be by equality rather than by tolerance. That is deliberate:
/// a tolerance would hide precisely the one-ULP class of defect that exact summation exists
/// to prevent, and this test would then agree with a broken cube.
fn many_facts(n: usize) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("region_key", DataType::Utf8, true),
        Field::new("period_key", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
    ]));
    const REGIONS: [&str; 4] = ["north", "south", "east", "west"];
    const PERIODS: [&str; 3] = ["jan", "feb", "mar"];

    let regions: Vec<&str> = (0..n).map(|i| REGIONS[i % REGIONS.len()]).collect();
    let periods: Vec<&str> = (0..n).map(|i| PERIODS[i % PERIODS.len()]).collect();
    #[allow(clippy::cast_precision_loss)]
    let amounts: Vec<f64> = (0..n).map(|i| ((i % 977) + 1) as f64).collect();

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(regions)),
            Arc::new(StringArray::from(periods)),
            Arc::new(Float64Array::from(amounts)),
        ],
    )
    .expect("well-formed")
}

#[tokio::test]
async fn the_cubes_totals_agree_with_plain_sql_over_the_same_table() {
    // The strongest check available here: the same question answered twice, by two
    // different paths through the engine. The cube hydrates, aggregates through its own
    // exact reduction and its own address map; the relational query aggregates the table
    // directly. Nothing is shared between them but the rows.
    //
    // A cube that agrees with itself proves nothing. A cube that agrees with a completely
    // separate implementation over fifty thousand rows is evidence.
    const ROWS: usize = 50_000;
    let context = SessionContext::new();
    context
        .register_batch("fact_figures", many_facts(ROWS))
        .expect("registered");
    let catalog = Arc::new(CubeCatalog::new());
    register(&context, Arc::clone(&catalog), Arc::new(sankhya_cube::querylog::QueryLog::new()));

    let absorbed = publish_from_fact_table(&context, &catalog, "figures", cube(), &amount(), 1)
        .await
        .expect("hydrated");
    assert_eq!(absorbed.placed as usize, ROWS, "every row reached the cube");

    // The grand total, both ways.
    let relational = scalar(&context, "SELECT sum(amount) FROM fact_figures").await;
    let through_cube = scalar(
        &context,
        "SELECT sum(amount) FROM cube_rollup('figures', 'amount', 'by=region')",
    )
    .await;
    assert_eq!(relational, through_cube, "the grand total disagrees between engines");

    // And per region, which is where an address-mapping fault would show and a grand total
    // would not: misfiling every row into the wrong region leaves the sum unchanged.
    for region in ["north", "south", "east", "west"] {
        let relational = scalar(
            &context,
            &format!(
                "SELECT sum(amount) FROM fact_figures WHERE region_key = '{region}'"
            ),
        )
        .await;
        let through_cube = scalar(
            &context,
            &format!(
                "SELECT amount FROM cube_rollup('figures', 'amount', \
                 'by=region, where=region:{region}')"
            ),
        )
        .await;
        assert_eq!(relational, through_cube, "region {region} disagrees");
    }
}

#[tokio::test]
async fn a_two_dimensional_breakdown_agrees_with_the_equivalent_group_by() {
    // The same check one grain finer, against SQL's own GROUP BY.
    const ROWS: usize = 20_000;
    let context = SessionContext::new();
    context
        .register_batch("fact_figures", many_facts(ROWS))
        .expect("registered");
    let catalog = Arc::new(CubeCatalog::new());
    register(&context, Arc::clone(&catalog), Arc::new(sankhya_cube::querylog::QueryLog::new()));
    publish_from_fact_table(&context, &catalog, "figures", cube(), &amount(), 1)
        .await
        .expect("hydrated");

    let relational = rows_of(
        &context,
        "SELECT region_key, period_key, sum(amount) FROM fact_figures \
         GROUP BY region_key, period_key ORDER BY region_key, period_key",
    )
    .await;
    let through_cube = rows_of(
        &context,
        "SELECT region, period, amount FROM cube_rollup('figures', 'amount', \
         'by=region|period') ORDER BY region, period",
    )
    .await;

    // Non-vacuous: four regions by three periods, and every group populated. A comparison
    // of two empty lists is the way this kind of test passes while proving nothing.
    assert_eq!(relational.len(), 12, "{relational:#?}");
    assert_eq!(relational, through_cube, "the breakdown disagrees between engines");
}

async fn rows_of(context: &SessionContext, sql: &str) -> Vec<String> {
    let batches = context.sql(sql).await.expect("planned").collect().await.expect("ran");
    let mut out = Vec::new();
    for batch in &batches {
        for row in 0..batch.num_rows() {
            let mut cells = Vec::new();
            for column in batch.columns() {
                cells.push(format!(
                    "{:?}",
                    datafusion::common::ScalarValue::try_from_array(column, row)
                        .expect("scalar")
                ));
            }
            out.push(cells.join(" | "));
        }
    }
    out
}
