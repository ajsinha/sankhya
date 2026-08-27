//! Cube navigation from SQL, and the columns that must survive a projection.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use datafusion::prelude::SessionContext;
use sankhya_cube::cells::Cells;
use sankhya_cube::overlay::{Adjustment, Overlay};
use sankhya_cube::{Definition, Dimension, Level};
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use sankhya_cube_sql::catalog::{CubeCatalog, Published};
use sankhya_cube_sql::register;
use std::sync::Arc;

const AMOUNT: Measure = Measure {
    name: "amount",
    rules: &[
        Along { dimension: "region", rule: Rule::Sum },
        Along { dimension: "period", rule: Rule::Sum },
    ],
};

/// Declares that it composes along nothing over time --- a ratio, which is not derivable
/// from its own values at a finer grain.
///
/// It has to *declare* that. A measure saying nothing about `period` cannot reach SQL at
/// all: §11.1 refuses the definition, so the cube never exists. That is the refusal working
/// one layer earlier than this test first assumed.
const RATIO: Measure = Measure {
    name: "ratio",
    rules: &[
        Along { dimension: "region", rule: Rule::Sum },
        Along { dimension: "period", rule: Rule::None },
    ],
};

fn address(members: &[&str]) -> Vec<String> {
    members.iter().map(|m| (*m).to_string()).collect()
}

/// A session with one cube published and one overlay registered.
fn session() -> (SessionContext, Arc<CubeCatalog>) {
    let definition = Definition::new(
        "figures",
        "fact_figures",
        vec![
            Dimension::new("region", "dim_region", "region_key", vec![Level::new("id", "id")]),
            Dimension::new("period", "dim_period", "period_key", vec![Level::new("id", "id")]),
        ],
        vec![AMOUNT, RATIO],
    );
    let cube = Arc::new(definition.validate().expect("well-formed"));

    let mut cells = Cells::over(vec!["region".to_string(), "period".to_string()]);
    for (region, period, value) in [
        ("north", "jan", 30.0),
        ("north", "feb", 70.0),
        ("south", "jan", 50.0),
    ] {
        cells.add(address(&[region, period]), value).expect("well-formed");
    }

    let catalog = Arc::new(CubeCatalog::new());
    catalog.publish(
        "figures",
        Published {
            cube: Arc::clone(&cube),
            cells: Arc::new(cells),
            snapshot: 4_242,
        },
    );

    let mut overlay = Overlay::named("budget-2027", cube.version());
    overlay.record(
        vec!["region".to_string(), "period".to_string()],
        address(&["north", "jan"]),
        Adjustment::Set(100.0),
    );
    catalog.register_overlay(Arc::new(overlay));

    let context = SessionContext::new();
    register(&context, Arc::clone(&catalog));
    (context, catalog)
}

async fn rows(context: &SessionContext, sql: &str) -> Vec<String> {
    let batches = context.sql(sql).await.expect("planned").collect().await.expect("ran");
    let mut out = Vec::new();
    for batch in &batches {
        for row in 0..batch.num_rows() {
            let mut cells = Vec::new();
            for column in batch.columns() {
                cells.push(format!("{:?}", datafusion::common::ScalarValue::try_from_array(
                    column, row
                )
                .expect("scalar")));
            }
            out.push(cells.join(" | "));
        }
    }
    out
}

// --- the operations are expressible from SQL ----------------------------

#[tokio::test]
async fn a_rollup_is_a_table_function_joinable_like_any_other() {
    let (context, _) = session();
    let found = rows(
        &context,
        "SELECT region, amount FROM cube_rollup('figures', 'amount', 'by=region') \
         ORDER BY region",
    )
    .await;
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0].contains("north") && found[0].contains("100"), "{found:?}");
    assert!(found[1].contains("south") && found[1].contains("50"), "{found:?}");
}

#[tokio::test]
async fn a_slice_fixes_a_member_and_drops_the_axis() {
    let (context, _) = session();
    let found = rows(
        &context,
        "SELECT period, amount FROM cube_slice('figures', 'amount', 'where=region:north') \
         ORDER BY period",
    )
    .await;
    assert_eq!(found.len(), 2, "{found:?}");
}

#[tokio::test]
async fn a_restriction_narrows_the_result() {
    let (context, _) = session();
    let found = rows(
        &context,
        "SELECT region, amount FROM cube_rollup('figures', 'amount', \
         'by=region, where=period:jan')",
    )
    .await;
    assert_eq!(found.len(), 2, "both regions have january: {found:?}");
}

// --- provenance survives because it is columns --------------------------

#[tokio::test]
async fn every_row_carries_the_definition_version_and_snapshot() {
    // So a cube figure can be reconciled with a relational one taken at a different moment.
    let (context, _) = session();
    let found = rows(
        &context,
        "SELECT snapshot, materialised, from_cuboid FROM cube_rollup('figures', 'amount', \
         'by=region')",
    )
    .await;
    assert!(found.iter().all(|r| r.contains("4242")), "{found:?}");
}

#[tokio::test]
async fn an_overlaid_row_names_its_scenario_in_a_column() {
    // The one decision in this crate. A qualification outside the rows is dropped by the
    // first SELECT that does not mention it, and here that turns a what-if into a fact.
    let (context, _) = session();
    let found = rows(
        &context,
        "SELECT region, amount, overlay FROM cube_rollup('figures', 'amount', \
         'by=region, overlay=budget-2027') ORDER BY region",
    )
    .await;
    assert!(found[0].contains("budget-2027"), "{found:?}");
    assert!(found[0].contains("170"), "100 replacing 30, plus 70: {found:?}");
}

#[tokio::test]
async fn published_rows_say_so_with_a_null_overlay() {
    let (context, _) = session();
    let found = rows(
        &context,
        "SELECT overlay FROM cube_rollup('figures', 'amount', 'by=region')",
    )
    .await;
    assert!(found.iter().all(|r| r.contains("NULL")), "{found:?}");

    // And the column is usable as a filter, which is the point of it being a column.
    let filtered = rows(
        &context,
        "SELECT region FROM cube_rollup('figures', 'amount', 'by=region') \
         WHERE overlay IS NULL",
    )
    .await;
    assert_eq!(filtered.len(), 2);
}

// --- refusals -----------------------------------------------------------

#[tokio::test]
async fn an_unknown_cube_is_refused_rather_than_answered_with_no_rows() {
    // A typo and a cube with no data are not the same fact.
    let (context, _) = session();
    let refused = context
        .sql("SELECT * FROM cube_rollup('figrues', 'amount', 'by=region')")
        .await
        .expect_err("planned a cube that does not exist");
    let message = refused.to_string();
    assert!(message.contains("no cube named 'figrues'"), "{message}");
    assert!(message.contains("figures"), "it names what does exist: {message}");
}

#[tokio::test]
async fn a_declared_but_unpublished_cube_is_a_wait_not_a_correction() {
    let (context, catalog) = session();
    catalog.declare("pending");
    let refused = context
        .sql("SELECT * FROM cube_rollup('pending', 'amount', 'by=region')")
        .await
        .expect_err("answered from nothing");
    assert!(
        refused.to_string().contains("wait-and-retry"),
        "{refused}"
    );
}

#[tokio::test]
async fn a_misspelled_option_is_refused_rather_than_taking_its_default() {
    // `by=regoin` silently ignored gives a grand total labelled as a breakdown, and nobody
    // reviewing the SQL would catch it.
    let (context, _) = session();
    let refused = context
        .sql("SELECT * FROM cube_rollup('figures', 'amount', 'bye=region')")
        .await
        .expect_err("accepted an unknown option");
    assert!(refused.to_string().contains("not an option"), "{refused}");
}

#[tokio::test]
async fn a_restriction_on_a_dimension_the_cube_lacks_is_refused() {
    let (context, _) = session();
    let refused = context
        .sql("SELECT * FROM cube_rollup('figures', 'amount', 'by=region, where=product:x')")
        .await
        .expect_err("ignored a restriction");
    assert!(refused.to_string().contains("Refused rather than ignored"), "{refused}");
}

#[tokio::test]
async fn rolling_up_a_measure_that_does_not_compose_is_refused_at_planning_time() {
    // M7's exit criterion 2, at the surface a caller uses: rejected while planning, not
    // computed and then explained afterwards.
    let (context, _) = session();
    let refused = context
        .sql("SELECT * FROM cube_rollup('figures', 'ratio', 'by=region')")
        .await
        .expect_err("rolled a ratio across time");
    assert!(
        refused.to_string().contains("cannot be rolled up along 'period'"),
        "{refused}"
    );
    assert!(
        refused.to_string().contains("plausible and wrong"),
        "the message says why, not just that: {refused}"
    );
}

#[tokio::test]
async fn an_unknown_measure_names_the_ones_that_exist() {
    let (context, _) = session();
    let refused = context
        .sql("SELECT * FROM cube_rollup('figures', 'amuont', 'by=region')")
        .await
        .expect_err("planned an unknown measure");
    assert!(refused.to_string().contains("amount"), "{refused}");
}

#[tokio::test]
async fn an_unknown_overlay_is_refused_rather_than_silently_not_applied() {
    // A named scenario that did not apply puts a published figure under a what-if's label.
    let (context, _) = session();
    let refused = context
        .sql("SELECT * FROM cube_rollup('figures', 'amount', 'by=region, overlay=nope')")
        .await
        .expect_err("ignored a scenario");
    assert!(refused.to_string().contains("no overlay named 'nope'"), "{refused}");
}

#[tokio::test]
async fn a_completeness_threshold_below_which_the_query_fails() {
    // FR-QUERY-13 at the surface. A threshold of 1.1 cannot be met by anything.
    let (context, _) = session();
    let refused = context
        .sql("SELECT * FROM cube_rollup('figures', 'amount', 'by=region, min_completeness=1.1')")
        .await
        .expect_err("accepted an impossible threshold");
    assert!(refused.to_string().contains("not a fraction"), "{refused}");
}
