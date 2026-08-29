//! Building a cube from published data, and the rows that cannot be placed.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_cube::hydrate::{absorb, empty_for, NotHydratable};
use sankhya_cube::{Definition, Dimension, Level};
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use std::sync::Arc;

fn amount() -> Measure {
    Measure::new("amount", vec![
        Along::new("region", Rule::Sum),
        Along::new("period", Rule::Sum),
    ])
}

fn cube() -> sankhya_cube::model::Cube {
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
    .expect("well-formed")
}

fn batch(
    regions: Vec<Option<&str>>,
    periods: Vec<Option<&str>>,
    amounts: Vec<Option<f64>>,
) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("region_key", DataType::Utf8, true),
        Field::new("period_key", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(regions)),
            Arc::new(StringArray::from(periods)),
            Arc::new(Float64Array::from(amounts)),
        ],
    )
    .expect("well-formed batch")
}

// --- the ordinary case --------------------------------------------------

#[test]
fn a_batch_of_facts_becomes_cells() {
    let cube = cube();
    let mut cells = empty_for(&cube);
    let absorbed = absorb(
        &cube,
        &amount(),
        &batch(
            vec![Some("north"), Some("north"), Some("south")],
            vec![Some("jan"), Some("feb"), Some("jan")],
            vec![Some(1.0), Some(2.0), Some(4.0)],
        ),
        &mut cells,
    )
    .expect("hydratable");

    assert_eq!(absorbed.rows, 3);
    assert_eq!(absorbed.placed, 3);
    assert_eq!(absorbed.unplaced, 0);
    assert!(absorbed.completeness().is_complete());
    assert_eq!(cells.len(), 3);
    assert_eq!(
        cells.get(
            &["north".to_string(), "jan".to_string()],
            Rule::Sum
        ),
        Some(1.0)
    );
}

#[test]
fn the_cells_are_over_the_cubes_dimensions_in_the_cubes_order() {
    let cube = cube();
    assert_eq!(empty_for(&cube).dimensions(), ["region", "period"]);
}

// --- rows that cannot be placed -----------------------------------------

#[test]
fn a_row_with_a_null_key_is_counted_not_dropped() {
    // Skipping it leaves a cube whose totals are quietly short by however many rows were
    // skipped: every figure wrong, plausible, and made of real data. The same failure as a
    // policy-filtered total presented as complete, so it gets the same machinery.
    let cube = cube();
    let mut cells = empty_for(&cube);
    let absorbed = absorb(
        &cube,
        &amount(),
        &batch(
            vec![Some("north"), None, Some("south")],
            vec![Some("jan"), Some("jan"), Some("jan")],
            vec![Some(1.0), Some(2.0), Some(4.0)],
        ),
        &mut cells,
    )
    .expect("hydratable");

    assert_eq!(absorbed.placed, 2);
    assert_eq!(absorbed.unplaced, 1);
    assert!(!absorbed.completeness().is_complete());
    assert_eq!(absorbed.completeness().fraction(), Some(2.0 / 3.0));
}

#[test]
fn a_null_member_is_not_a_member_named_empty_string() {
    // Placing it under "" invents a member that is not in the dimension table, and it then
    // appears in results and reconciliations as a real thing with real money against it.
    let cube = cube();
    let mut cells = empty_for(&cube);
    absorb(
        &cube,
        &amount(),
        &batch(vec![None], vec![Some("jan")], vec![Some(9.0)]),
        &mut cells,
    )
    .expect("hydratable");

    assert!(cells.is_empty(), "a member was invented: {cells:?}");
    assert_eq!(cells.get(&[String::new(), "jan".to_string()], Rule::Sum), None);
}

#[test]
fn a_row_with_one_null_key_of_several_is_placed_nowhere() {
    // All-or-nothing: a partial address files the row in some other cell's total.
    let cube = cube();
    let mut cells = empty_for(&cube);
    absorb(
        &cube,
        &amount(),
        &batch(vec![Some("north")], vec![None], vec![Some(9.0)]),
        &mut cells,
    )
    .expect("hydratable");
    assert!(cells.is_empty());
}

#[test]
fn a_null_measure_is_unplaced_rather_than_zero() {
    // A fact with no amount is not a fact with an amount of nothing, and the difference
    // reaches the total.
    let cube = cube();
    let mut cells = empty_for(&cube);
    let absorbed = absorb(
        &cube,
        &amount(),
        &batch(vec![Some("north")], vec![Some("jan")], vec![None]),
        &mut cells,
    )
    .expect("hydratable");

    assert_eq!(absorbed.unplaced, 1);
    assert!(cells.is_empty(), "a zero was invented");
}

// --- refusals -----------------------------------------------------------

#[test]
fn a_missing_dimension_column_is_refused_not_skipped() {
    // Skipping it groups every row under one member, and the total is then untraceable.
    let cube = cube();
    let mut cells = empty_for(&cube);
    let schema = Arc::new(Schema::new(vec![
        Field::new("region_key", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
    ]));
    let missing = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![Some("north")])),
            Arc::new(Float64Array::from(vec![Some(1.0)])),
        ],
    )
    .expect("well-formed");

    let refused = absorb(&cube, &amount(), &missing, &mut cells).expect_err("hydrated anyway");
    let NotHydratable::MissingColumn { dimension, column, found } = &refused else {
        panic!("wrong refusal: {refused:?}");
    };
    assert_eq!(dimension, "period");
    assert_eq!(column, "period_key");
    assert!(found.contains(&"region_key".to_string()), "it names what is there");
}

#[test]
fn a_missing_measure_column_is_refused() {
    let cube = cube();
    let mut cells = empty_for(&cube);
    let schema = Arc::new(Schema::new(vec![
        Field::new("region_key", DataType::Utf8, true),
        Field::new("period_key", DataType::Utf8, true),
    ]));
    let missing = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![Some("north")])),
            Arc::new(StringArray::from(vec![Some("jan")])),
        ],
    )
    .expect("well-formed");
    assert!(matches!(
        absorb(&cube, &amount(), &missing, &mut cells),
        Err(NotHydratable::MissingMeasure { .. })
    ));
}

#[test]
fn a_non_numeric_measure_column_is_refused() {
    let cube = cube();
    let mut cells = empty_for(&cube);
    let schema = Arc::new(Schema::new(vec![
        Field::new("region_key", DataType::Utf8, true),
        Field::new("period_key", DataType::Utf8, true),
        Field::new("amount", DataType::Utf8, true),
    ]));
    let wrong = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![Some("north")])),
            Arc::new(StringArray::from(vec![Some("jan")])),
            Arc::new(StringArray::from(vec![Some("a lot")])),
        ],
    )
    .expect("well-formed");
    assert!(matches!(
        absorb(&cube, &amount(), &wrong, &mut cells),
        Err(NotHydratable::UnreadableMeasure { .. })
    ));
}

// --- types --------------------------------------------------------------

#[test]
fn integer_keys_and_integer_measures_are_read() {
    let cube = cube();
    let mut cells = empty_for(&cube);
    let schema = Arc::new(Schema::new(vec![
        Field::new("region_key", DataType::Int64, true),
        Field::new("period_key", DataType::Int64, true),
        Field::new("amount", DataType::Int64, true),
    ]));
    let numeric = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![Some(7)])),
            Arc::new(Int64Array::from(vec![Some(1)])),
            Arc::new(Int64Array::from(vec![Some(5)])),
        ],
    )
    .expect("well-formed");

    let absorbed = absorb(&cube, &amount(), &numeric, &mut cells).expect("hydratable");
    assert_eq!(absorbed.placed, 1);
    assert_eq!(cells.get(&["7".to_string(), "1".to_string()], Rule::Sum), Some(5.0));
}

// --- many batches -------------------------------------------------------

#[test]
fn several_batches_accumulate_and_their_completeness_combines() {
    let cube = cube();
    let mut cells = empty_for(&cube);
    let first = absorb(
        &cube,
        &amount(),
        &batch(vec![Some("north")], vec![Some("jan")], vec![Some(1.0)]),
        &mut cells,
    )
    .expect("hydratable");
    let second = absorb(
        &cube,
        &amount(),
        &batch(
            vec![Some("north"), None],
            vec![Some("jan"), Some("jan")],
            vec![Some(2.0), Some(4.0)],
        ),
        &mut cells,
    )
    .expect("hydratable");

    let total = first.and(second);
    assert_eq!(total.rows, 3);
    assert_eq!(total.placed, 2);
    assert_eq!(total.unplaced, 1);
    assert_eq!(
        cells.get(&["north".to_string(), "jan".to_string()], Rule::Sum),
        Some(3.0),
        "both placed rows landed in the same cell"
    );
}
