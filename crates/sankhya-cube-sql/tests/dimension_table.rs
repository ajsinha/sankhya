//! The dimension table is opened.
//!
//! `M23`. Until this existed, hydration read member keys from the **fact table's** join
//! column and that was all the cube knew: `DIMENSION city FROM dim_city ON city_key
//! (LEVEL area = area, LEVEL city = city)` never opened `dim_city`, so a star schema's
//! roll-up --- which is where a real warehouse keeps its hierarchy --- could not be used, and
//! a fact-table key with no dimension row became a member anyway.
//!
//! Every test here registers a *dimension table* alongside the facts and asserts something
//! that is only decidable by reading it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Array, Float64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_cube::{Definition, Dimension, Level};
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use sankhya_cube_sql::catalog::CubeCatalog;
use sankhya_cube_sql::{publish_from_fact_table, read_members, register};
use std::collections::BTreeSet;
use std::sync::Arc;

fn amount() -> Measure {
    Measure::new("amount", vec![Along::new("city", Rule::Sum)])
}

/// A cube over one dimension, so the assertions are about the hierarchy and nothing else.
fn cube_over(dimension: Dimension) -> Arc<sankhya_cube::model::Cube> {
    Arc::new(
        Definition::new("figures", "fact_figures", vec![dimension], vec![amount()])
            .validate()
            .expect("well-formed"),
    )
}

/// `LEVEL area = area, LEVEL country = country, LEVEL city = city` --- coarse to fine.
fn by_levels() -> Dimension {
    Dimension::new("city", "dim_city", "city_key", vec![
        Level::new("area", "area"),
        Level::new("country", "country"),
        Level::new("city", "city"),
    ])
}

/// `PARENT id TO parent_id` --- the ragged form, depth unknown until the table is read.
fn by_parent() -> Dimension {
    let mut dimension = Dimension::new("city", "dim_city", "city_key", vec![Level::new(
        "city", "id",
    )]);
    dimension.parent_child = Some(("id".to_string(), "parent_id".to_string()));
    dimension
}

fn with_facts(context: &SessionContext, cities: Vec<&str>, amounts: Vec<f64>) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("city_key", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
    ]));
    let batch = RecordBatch::try_new(schema, vec![
        Arc::new(StringArray::from(cities)),
        Arc::new(Float64Array::from(amounts)),
    ])
    .expect("well-formed");
    context.register_batch("fact_figures", batch).expect("registered");
}

/// `dim_city` in the level form. `None` in a column is a ragged branch.
fn with_levels(
    context: &SessionContext,
    areas: Vec<Option<&str>>,
    countries: Vec<Option<&str>>,
    cities: Vec<Option<&str>>,
) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("area", DataType::Utf8, true),
        Field::new("country", DataType::Utf8, true),
        Field::new("city", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(schema, vec![
        Arc::new(StringArray::from(areas)),
        Arc::new(StringArray::from(countries)),
        Arc::new(StringArray::from(cities)),
    ])
    .expect("well-formed");
    context.register_batch("dim_city", batch).expect("registered");
}

/// `dim_city` in the recursive form. A null parent is a root.
fn with_parents(context: &SessionContext, ids: Vec<&str>, parents: Vec<Option<&str>>) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, true),
        Field::new("parent_id", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(schema, vec![
        Arc::new(StringArray::from(ids)),
        Arc::new(StringArray::from(parents)),
    ])
    .expect("well-formed");
    context.register_batch("dim_city", batch).expect("registered");
}

/// Publish the cube, having read whatever dimension tables the session holds.
async fn publish(
    context: &SessionContext,
    catalog: &Arc<CubeCatalog>,
    cube: Arc<sankhya_cube::model::Cube>,
) {
    let members = Arc::new(read_members(context, &cube).await.expect("the dimension table reads"));
    publish_from_fact_table(context, catalog, "figures", cube, &amount(), 1, members)
        .await
        .expect("hydrated");
}

fn session() -> (SessionContext, Arc<CubeCatalog>) {
    let context = SessionContext::new();
    let catalog = Arc::new(CubeCatalog::new());
    register(
        &context,
        Arc::clone(&catalog),
        Arc::new(sankhya_cube::querylog::QueryLog::new()),
        None,
    );
    (context, catalog)
}

/// The members and the total a `cube_consolidate` produced.
async fn consolidated(context: &SessionContext, sql: &str) -> (BTreeSet<String>, f64) {
    let batches = context.sql(sql).await.expect("planned").collect().await.expect("ran");
    let mut members = BTreeSet::new();
    let mut total = 0.0;
    for batch in &batches {
        let city = batch.column_by_name("city").expect("a city column");
        let city = city.as_any().downcast_ref::<StringArray>().expect("utf8");
        let amount = batch.column_by_name("amount").expect("an amount column");
        let amount = amount.as_any().downcast_ref::<Float64Array>().expect("f64");
        for row in 0..batch.num_rows() {
            members.insert(city.value(row).to_string());
            if !amount.is_null(row) {
                total += amount.value(row);
            }
        }
    }
    (members, total)
}

#[tokio::test]
async fn a_hierarchy_in_the_dimension_table_consolidates() {
    // The star-schema shape, and the one this milestone exists for. Nothing is declared in the
    // cube: the roll-up is three columns of `dim_city`, which before `M23` were parsed,
    // fingerprinted and read by nothing at all.
    //
    // The invariant is the one that makes a consolidation a consolidation: for an additive
    // measure it **moves** facts between members and changes no total.
    let (context, catalog) = session();
    with_facts(&context, vec!["paris", "lyon", "berlin"], vec![1.0, 2.0, 4.0]);
    with_levels(
        &context,
        vec![Some("emea"), Some("emea"), Some("emea")],
        vec![Some("fr"), Some("fr"), Some("de")],
        vec![Some("paris"), Some("lyon"), Some("berlin")],
    );
    publish(&context, &catalog, cube_over(by_levels())).await;

    let (members, total) =
        consolidated(&context, "SELECT * FROM cube_consolidate('figures', 'amount', 'along=city')")
            .await;
    assert_eq!(
        members,
        ["de".to_string(), "fr".to_string()].into_iter().collect::<BTreeSet<String>>(),
        "each city was replaced by the country its dimension row names"
    );
    assert_eq!(total, 7.0, "consolidation moves facts; it must not change the total");
}

#[tokio::test]
async fn a_ragged_branch_joins_to_the_next_ancestor_it_actually_has() {
    // A city in no country --- a city-state, a territory, a row somebody has not filled in
    // yet. The convenient handling is to pad the gap with a placeholder, which invents a
    // member that then appears in results and in member counts; `FR-QUERY-11` forbids it.
    //
    // So the edge to the missing level is simply absent and the one below joins to whatever
    // the next non-null ancestor is. `monaco` therefore consolidates straight to `emea`, and
    // it is one step because `along=` is one step.
    let (context, catalog) = session();
    with_facts(&context, vec!["paris", "monaco"], vec![1.0, 2.0]);
    with_levels(
        &context,
        vec![Some("emea"), Some("emea")],
        vec![Some("fr"), None],
        vec![Some("paris"), Some("monaco")],
    );
    publish(&context, &catalog, cube_over(by_levels())).await;

    let (members, total) =
        consolidated(&context, "SELECT * FROM cube_consolidate('figures', 'amount', 'along=city')")
            .await;
    assert_eq!(
        members,
        ["emea".to_string(), "fr".to_string()].into_iter().collect::<BTreeSet<String>>(),
        "the ragged branch reached its area directly rather than a padded country: {members:?}"
    );
    assert_eq!(total, 3.0, "and nothing was invented or lost on the way");
}

#[tokio::test]
async fn a_recursive_dimension_table_drives_the_roll_up() {
    // `PARENT id TO parent_id`: the same question asked of the form whose depth is not known
    // until the table is read. `lyon -> fr -> emea`, and a root says so with a null parent.
    let (context, catalog) = session();
    with_facts(&context, vec!["paris", "lyon", "berlin"], vec![1.0, 2.0, 4.0]);
    with_parents(
        &context,
        vec!["emea", "fr", "de", "paris", "lyon", "berlin"],
        vec![None, Some("emea"), Some("emea"), Some("fr"), Some("fr"), Some("de")],
    );
    publish(&context, &catalog, cube_over(by_parent())).await;

    let (members, total) =
        consolidated(&context, "SELECT * FROM cube_consolidate('figures', 'amount', 'along=city')")
            .await;
    assert_eq!(
        members,
        ["de".to_string(), "fr".to_string()].into_iter().collect::<BTreeSet<String>>(),
        "one step up the parent column: {members:?}"
    );
    assert_eq!(total, 7.0, "and the total is unchanged");

    // And the whole subtree, through the set-valued walk `M22b` built. `emea` totals every
    // city under it however deep, each contributor once.
    let (members, total) = consolidated(
        &context,
        "SELECT * FROM cube_consolidate('figures', 'amount', 'along=city', 'to=emea')",
    )
    .await;
    assert_eq!(
        members,
        ["emea".to_string()].into_iter().collect::<BTreeSet<String>>(),
        "everything rolled into the root: {members:?}"
    );
    assert_eq!(total, 7.0, "and that is the grand total, once");
}

#[tokio::test]
async fn a_fact_key_the_dimension_table_does_not_have_is_refused_by_name() {
    // The referential check that did not exist. `atlantis` sold something and no dimension row
    // declares it, so it has no parent: `consolidate_along` leaves it where it is and it sits
    // at leaf grain **beside** the parents, in a result whose other rows are totals and which
    // says nothing about the difference.
    //
    // Refused, naming the member, because the fix is to load the missing dimension row and the
    // person doing that needs to know which one.
    let (context, catalog) = session();
    with_facts(&context, vec!["paris", "atlantis"], vec![1.0, 2.0]);
    with_levels(
        &context,
        vec![Some("emea")],
        vec![Some("fr")],
        vec![Some("paris")],
    );
    publish(&context, &catalog, cube_over(by_levels())).await;

    // The control: the cube answers at base grain, where an orphan is a member like any other
    // and the money against it is real. It is consolidation that cannot place it.
    let leaves = context
        .sql("SELECT * FROM cube_rollup('figures', 'amount', 'by=city')")
        .await
        .expect("planned")
        .collect()
        .await
        .expect("ran");
    assert!(!leaves.is_empty(), "the cube itself hydrates and answers");

    let refused = context
        .sql("SELECT * FROM cube_consolidate('figures', 'amount', 'along=city')")
        .await
        .err()
        .map(|why| why.to_string())
        .unwrap_or_default();
    assert!(
        refused.contains("atlantis"),
        "the refusal must name the member with no dimension row: {refused}"
    );
    assert!(
        refused.contains("dim_city"),
        "and the table whose rows are missing: {refused}"
    );
}

#[tokio::test]
async fn a_dimension_table_that_gives_no_parent_link_says_so() {
    // A single-level dimension is a legitimate table and an unusable hierarchy, and the
    // difference has to be sayable. Refusing with "declare a `ROLLUP`" when the dimension
    // table was read and simply flat sends the reader to the wrong file.
    let (context, catalog) = session();
    with_facts(&context, vec!["paris", "lyon"], vec![1.0, 2.0]);
    let schema = Arc::new(Schema::new(vec![Field::new("city", DataType::Utf8, true)]));
    let batch = RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec![
        Some("paris"),
        Some("lyon"),
    ]))])
    .expect("well-formed");
    context.register_batch("dim_city", batch).expect("registered");

    let flat = Dimension::new("city", "dim_city", "city_key", vec![Level::new("city", "city")]);
    publish(&context, &catalog, cube_over(flat)).await;

    let refused = context
        .sql("SELECT * FROM cube_consolidate('figures', 'amount', 'along=city')")
        .await
        .err()
        .map(|why| why.to_string())
        .unwrap_or_default();
    assert!(
        refused.contains("every member is a root"),
        "the refusal must say the table was read and was flat: {refused}"
    );
}

#[tokio::test]
async fn a_cycle_in_the_dimension_table_is_refused_at_hydration() {
    // Not at query time. A cycle reached by a traversal is an unbounded walk and a timeout
    // that names nothing; a cycle found while reading the table names the members.
    let (context, _catalog) = session();
    with_facts(&context, vec!["paris"], vec![1.0]);
    with_parents(&context, vec!["fr", "emea"], vec![Some("emea"), Some("fr")]);

    let refused = read_members(&context, &cube_over(by_parent()))
        .await
        .err()
        .map(|why| why.to_string())
        .unwrap_or_default();
    assert!(
        refused.contains("dim_city"),
        "the refusal must name the table the cycle is in: {refused}"
    );
}

#[tokio::test]
async fn a_declared_rollup_wins_over_the_dimension_table() {
    // Both present, and the definition decides. A `ROLLUP` typed into the cube is the more
    // deliberate statement, and it is the only way to express an alternate roll-up at all ---
    // a dimension table has one parent column and can say one thing per member.
    //
    // Asserted by making the two disagree, which is the only way this is decidable: agreeing
    // fixtures pass whichever source is used.
    let (context, catalog) = session();
    with_facts(&context, vec!["paris", "lyon"], vec![1.0, 2.0]);
    with_levels(
        &context,
        vec![Some("emea"), Some("emea")],
        vec![Some("fr"), Some("fr")],
        vec![Some("paris"), Some("lyon")],
    );
    let mut dimension = by_levels();
    let mut declared = sankhya_cube_algo::hierarchy::Hierarchy::new();
    declared.rolls_up("paris", "north");
    declared.rolls_up("lyon", "south");
    dimension.rollups = Some(declared);
    publish(&context, &catalog, cube_over(dimension)).await;

    let (members, total) =
        consolidated(&context, "SELECT * FROM cube_consolidate('figures', 'amount', 'along=city')")
            .await;
    assert_eq!(
        members,
        ["north".to_string(), "south".to_string()].into_iter().collect::<BTreeSet<String>>(),
        "the declared edges were used, not the country column: {members:?}"
    );
    assert_eq!(total, 3.0);
}
