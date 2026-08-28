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

const AMOUNT: Measure = Measure {
    name: "amount",
    rules: &[
        Along { dimension: "period", rule: Rule::Sum },
        Along { dimension: "entity", rule: Rule::Sum },
    ],
};

/// Declares a rule along `entity` only. Summing a balance across time is wrong.
const BALANCE: Measure = Measure {
    name: "balance",
    rules: &[Along { dimension: "entity", rule: Rule::Sum }],
};

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
    Definition::new("figures", "fact_figures", dimensions(), vec![AMOUNT])
}

// --- the default that produces wrong numbers ----------------------------

#[test]
fn a_measure_with_no_rule_for_a_dimension_is_refused_not_summed() {
    // The one that matters. Every comparable product defaults this to summation, and a
    // balance summed across time is plausible, wrong, and indistinguishable from correct.
    let bad = Definition::new("figures", "fact_figures", dimensions(), vec![BALANCE]);
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
    const NOTHING: Measure = Measure { name: "ratio", rules: &[] };
    let bad = Definition::new("figures", "fact_figures", dimensions(), vec![NOTHING]);
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
    const TYPO: Measure = Measure {
        name: "amount",
        rules: &[
            Along { dimension: "period", rule: Rule::Sum },
            Along { dimension: "entty", rule: Rule::Sum },
        ],
    };
    let refused = Definition::new("figures", "fact_figures", dimensions(), vec![TYPO])
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
    let refused = Definition::new("figures", "fact_figures", dims, vec![AMOUNT])
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
    assert!(Definition::new("figures", "fact_figures", dims, vec![AMOUNT])
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
    let refused = Definition::new("", "fact_figures", dims, vec![BALANCE])
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
    let no_dims = Definition::new("figures", "fact_figures", vec![], vec![AMOUNT]);
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
    let bad = Definition::new("figures", "fact_figures", dimensions(), vec![BALANCE]);
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
