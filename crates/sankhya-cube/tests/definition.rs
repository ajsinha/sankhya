//! Validating a cube definition, and the default that would produce wrong numbers.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_cube::validate::Rejection;
use sankhya_cube::{Definition, Dimension, Level};
use sankhya_cube_algo::hierarchy::Hierarchy;
use sankhya_cube_algo::measure::{Along, Measure, Rule};

fn amount() -> Measure {
    Measure::new("amount", vec![
        Along::new("period", Rule::Sum),
        Along::new("entity", Rule::Sum),
    ])
}

/// Declares a rule along `entity` only. Summing a balance across time is wrong.
fn balance() -> Measure {
    Measure::new("balance", vec![Along::new("entity", Rule::Sum)])
}

fn dimensions() -> Vec<Dimension> {
    vec![
        Dimension::new(
            "period",
            "dim_period",
            "period_key",
            vec![Level::new("year", "year"), Level::new("day", "day")],
        ),
        Dimension::new("entity", "dim_entity", "entity_key", vec![Level::new("id", "id")]),
    ]
}

fn definition() -> Definition {
    Definition::new("figures", "fact_figures", dimensions(), vec![amount()])
}

// --- the default that produces wrong numbers ----------------------------

#[test]
fn a_measure_with_no_rule_for_a_dimension_is_refused_not_summed() {
    // The one that matters. Every comparable product defaults this to summation, and a
    // balance summed across time is plausible, wrong, and indistinguishable from correct.
    let bad = Definition::new("figures", "fact_figures", dimensions(), vec![balance()]);
    let refused = bad.validate().expect_err("a cube was built from an undeclared measure");

    assert_eq!(
        refused,
        vec![Rejection::MeasureUndeclared {
            measure: "balance".to_string(),
            dimensions: vec!["period".to_string()],
        }]
    );
    assert!(
        refused[0].to_string().contains("balance"),
        "the message names the measure: {}",
        refused[0]
    );
}

#[test]
fn every_undeclared_dimension_is_named_at_once() {
    // Reporting them one build at a time is how somebody stops reading and declares `Sum`
    // for everything, which is the outcome the refusal exists to prevent.
    fn nothing() -> Measure {
        Measure::new("ratio", vec![])
    }
    let bad = Definition::new("figures", "fact_figures", dimensions(), vec![nothing()]);
    let refused = bad.validate().expect_err("built anyway");

    let Rejection::MeasureUndeclared { dimensions, .. } = &refused[0] else {
        panic!("expected an undeclared measure: {refused:#?}");
    };
    // Declaration order, not alphabetical: somebody reading this is looking at their own
    // definition, and a list in a different order than the file is a list they have to sort.
    assert_eq!(dimensions, &["period".to_string(), "entity".to_string()]);
}

#[test]
fn a_rule_for_a_dimension_that_does_not_exist_is_its_own_rejection() {
    // A misspelled rule leaves the real dimension undeclared *and* adds a stray. Reporting
    // only the first would send somebody to re-add a rule they had already written.
    fn typo() -> Measure {
        Measure::new("amount", vec![
            Along::new("period", Rule::Sum),
            Along::new("entty", Rule::Sum),
        ])
    }
    let refused = Definition::new("figures", "fact_figures", dimensions(), vec![typo()])
        .validate()
        .expect_err("built anyway");

    assert!(refused.contains(&Rejection::RuleForUnknownDimension {
        measure: "amount".to_string(),
        dimension: "entty".to_string(),
    }));
    assert!(refused.iter().any(|r| matches!(
        r,
        Rejection::MeasureUndeclared { dimensions, .. }
            if dimensions == &["entity".to_string()]
    )));
}

// --- cycles -------------------------------------------------------------

#[test]
fn a_cycle_is_refused_with_the_path_named() {
    // "this hierarchy has a cycle" sends somebody to read a hierarchy. The path sends them
    // to two rows.
    let mut rollups = Hierarchy::new();
    rollups.rolls_up("east", "west");
    rollups.rolls_up("west", "east");

    let dims = vec![
        Dimension::new("period", "dim_period", "period_key", vec![Level::new("day", "day")]),
        Dimension::new("entity", "dim_entity", "entity_key", vec![Level::new("id", "id")])
            .rolling_up(rollups),
    ];
    let refused = Definition::new("figures", "fact_figures", dims, vec![amount()])
        .validate()
        .expect_err("a cyclic hierarchy was accepted");

    let Some(Rejection::HierarchyCycle { dimension, cycle }) = refused
        .iter()
        .find(|r| matches!(r, Rejection::HierarchyCycle { .. }))
    else {
        panic!("no cycle reported: {refused:#?}");
    };
    assert_eq!(dimension, "entity");
    assert!(cycle.len() >= 2, "the path is named, not just its existence: {cycle:?}");
    assert!(cycle.contains(&"east".to_string()) && cycle.contains(&"west".to_string()));
}

#[test]
fn an_acyclic_declared_hierarchy_is_accepted() {
    let mut rollups = Hierarchy::new();
    rollups.rolls_up("branch", "region");
    rollups.rolls_up("region", "total");

    let dims = vec![
        Dimension::new("period", "dim_period", "period_key", vec![Level::new("day", "day")]),
        Dimension::new("entity", "dim_entity", "entity_key", vec![Level::new("id", "id")])
            .rolling_up(rollups),
    ];
    assert!(Definition::new("figures", "fact_figures", dims, vec![amount()])
        .validate()
        .is_ok());
}

// --- everything at once -------------------------------------------------

#[test]
fn validation_reports_every_problem_not_the_first() {
    let dims = vec![
        Dimension::new("period", "dim_period", "period_key", vec![Level::new("day", "day")]),
        Dimension::new("period", "dim_other", "other_key", vec![Level::new("id", "id")]),
    ];
    let refused = Definition::new("", "fact_figures", dims, vec![balance()])
        .validate()
        .expect_err("built anyway");

    assert!(refused.len() >= 3, "one build should show every problem: {refused:#?}");
    assert!(refused.contains(&Rejection::Duplicate {
        what: "dimension",
        name: "period".to_string()
    }));
    assert!(refused.iter().any(|r| matches!(r, Rejection::Blank { what: "cube", .. })));
    assert!(refused
        .iter()
        .any(|r| matches!(r, Rejection::MeasureUndeclared { .. })));
}

#[test]
fn a_cube_with_no_dimensions_or_no_measures_is_a_table() {
    let no_dims = Definition::new("figures", "fact_figures", vec![], vec![amount()]);
    assert!(no_dims
        .validate()
        .expect_err("built anyway")
        .contains(&Rejection::Empty { what: "dimension" }));

    let no_measures = Definition::new("figures", "fact_figures", dimensions(), vec![]);
    assert!(no_measures
        .validate()
        .expect_err("built anyway")
        .contains(&Rejection::Empty { what: "measure" }));
}

#[test]
fn rejections_are_ordered_and_deduplicated() {
    // A refusal read by a person and diffed by a build should not depend on iteration order.
    let bad = Definition::new("figures", "fact_figures", dimensions(), vec![balance()]);
    let once = bad.clone().validate().expect_err("built anyway");
    let twice = bad.validate().expect_err("built anyway");
    assert_eq!(once, twice);

    let mut sorted = once.clone();
    sorted.sort();
    assert_eq!(once, sorted, "reported in a stable order");
}

// --- the validated cube -------------------------------------------------

#[test]
fn a_valid_definition_becomes_a_cube_that_answers_for_itself() {
    let cube = definition().validate().expect("a well-formed cube");
    assert_eq!(cube.name(), "figures");
    assert_eq!(cube.fact_table(), "fact_figures");
    assert_eq!(cube.dimension_names(), vec!["period", "entity"]);
    assert!(cube.dimension("period").is_some());
    assert!(cube.dimension("absent").is_none());
    assert!(cube.measure("amount").is_some());
    assert_eq!(cube.joins().get("entity"), Some(&"entity_key"));
    assert_eq!(
        cube.dimension("period").and_then(|d| d.level("year")).map(|l| l.column.as_str()),
        Some("year")
    );
}

#[test]
fn a_derived_result_over_a_bare_table_name_is_refused() {
    use sankhya_cube::model::Definition;
    use sankhya_cube::validate::Rejection;

    // Reached from the **catalogue**, not from the statement. `CREATE DERIVED` demands a
    // parenthesised query and refuses a name before validation ever runs --- so this guards the
    // other way in: a definition read back from disk, written by an older version or edited by
    // hand, and validated on adoption.
    //
    // A derived result over a table is that table with a second name, plus a catalogue entry, a
    // fingerprint and a dependency list to keep current for no gain.
    let over_a_table =
        Definition::derived("copy", "sales.orders", vec!["sales.orders".to_owned()]);
    let rejections = over_a_table.validate().expect_err("a name is not a query");
    assert!(
        rejections.iter().any(|r| matches!(r, Rejection::DerivedFromATable)),
        "refused for the reason it is wrong, not some other one: {rejections:?}"
    );

    // And the same definition over a query is accepted, which is what makes the refusal above
    // load-bearing rather than a rule against derived results.
    let over_a_query = Definition::derived(
        "regional",
        "(SELECT area FROM sales.regions)",
        vec!["sales.regions".to_owned()],
    );
    assert!(over_a_query.validate().is_ok(), "a query given a name is a derived result");
}
