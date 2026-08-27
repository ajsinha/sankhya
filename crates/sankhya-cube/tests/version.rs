//! The version nobody has to remember to bump.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_cube::version::fingerprint;
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

/// The same measure, aggregated differently along time. A cuboid built under one of these
/// answers a different question than a cuboid built under the other.
const AMOUNT_LAST: Measure = Measure {
    name: "amount",
    rules: &[
        Along { dimension: "period", rule: Rule::Last },
        Along { dimension: "entity", rule: Rule::Sum },
    ],
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

fn version_of(measures: Vec<Measure>) -> u64 {
    Definition::new("figures", "fact_figures", dimensions(), measures)
        .validate()
        .expect("well-formed")
        .version()
}

#[test]
fn the_same_definition_always_fingerprints_the_same() {
    // Or a restart invalidates every materialised cuboid, and the cost of the cube is paid
    // again on every deployment for no gain anybody can see.
    assert_eq!(version_of(vec![AMOUNT]), version_of(vec![AMOUNT]));
}

#[test]
fn changing_how_a_measure_aggregates_changes_the_version() {
    // The property the whole materialisation key rests on. A cuboid built when `amount` was
    // summed across time answers a different question once it is a closing balance, and
    // serving the old rows is a correct-looking number from a definition that no longer
    // exists.
    assert_ne!(version_of(vec![AMOUNT]), version_of(vec![AMOUNT_LAST]));
}

#[test]
fn reordering_levels_changes_the_version() {
    // Levels are coarse-to-fine, so their order *is* the drill-down path.
    let reversed = vec![
        Dimension::new(
            "period",
            "dim_period",
            "period_key",
            vec![Level::new("day", "day"), Level::new("year", "year")],
        ),
        Dimension::new("entity", "dim_entity", "entity_key", vec![Level::new("id", "id")]),
    ];
    let a = Definition::new("figures", "fact_figures", dimensions(), vec![AMOUNT])
        .validate()
        .expect("well-formed");
    let b = Definition::new("figures", "fact_figures", reversed, vec![AMOUNT])
        .validate()
        .expect("well-formed");
    assert_ne!(a.version(), b.version());
}

#[test]
fn a_field_boundary_cannot_be_moved_without_changing_the_version() {
    // Fields are length-prefixed. Without that, a dimension named `ab` on column `c`
    // fingerprints identically to one named `a` on column `bc`, and two different cubes
    // share a materialisation key.
    //
    // Against `fingerprint` rather than through `validate`, because a valid definition must
    // declare a rule per dimension — and those rules name the dimensions, so the two cubes
    // would differ for a second reason and the test would pass without the length prefix.
    const NONE: Measure = Measure { name: "n", rules: &[] };
    let one = Definition::new(
        "c",
        "f",
        vec![Dimension::new("ab", "t", "c", vec![Level::new("l", "x")])],
        vec![NONE],
    );
    let two = Definition::new(
        "c",
        "f",
        vec![Dimension::new("a", "t", "bc", vec![Level::new("l", "x")])],
        vec![NONE],
    );
    assert_ne!(fingerprint(&one), fingerprint(&two));

    // The same trap one field along, to show the prefix is on every field and not just the
    // one that happened to be tested.
    let three = Definition::new(
        "c",
        "f",
        vec![Dimension::new("d", "t", "c", vec![Level::new("lx", "")])],
        vec![NONE],
    );
    let four = Definition::new(
        "c",
        "f",
        vec![Dimension::new("d", "t", "c", vec![Level::new("l", "x")])],
        vec![NONE],
    );
    assert_ne!(fingerprint(&three), fingerprint(&four));
}

#[test]
fn declaring_the_same_rollups_in_a_different_order_is_the_same_cube() {
    // A hierarchy is a set of edges. Two definitions declaring the same ones mean the same
    // thing, and rebuilding every cuboid because somebody sorted a config file is a cost
    // with no corresponding risk.
    let mut forwards = Hierarchy::new();
    forwards.rolls_up("branch", "region");
    forwards.rolls_up("region", "total");

    let mut backwards = Hierarchy::new();
    backwards.rolls_up("region", "total");
    backwards.rolls_up("branch", "region");

    let build = |rollups: Hierarchy| {
        let dims = vec![
            Dimension::new("period", "dim_period", "period_key", vec![Level::new("day", "d")]),
            Dimension::new("entity", "dim_entity", "entity_key", vec![Level::new("id", "id")])
                .rolling_up(rollups),
        ];
        Definition::new("figures", "fact_figures", dims, vec![AMOUNT])
            .validate()
            .expect("well-formed")
            .version()
    };
    assert_eq!(build(forwards), build(backwards));
}

#[test]
fn adding_a_rollup_edge_changes_the_version() {
    let mut small = Hierarchy::new();
    small.rolls_up("branch", "region");

    let mut large = Hierarchy::new();
    large.rolls_up("branch", "region");
    large.rolls_up("region", "total");

    let build = |rollups: Hierarchy| {
        let dims = vec![
            Dimension::new("period", "dim_period", "period_key", vec![Level::new("day", "d")]),
            Dimension::new("entity", "dim_entity", "entity_key", vec![Level::new("id", "id")])
                .rolling_up(rollups),
        ];
        Definition::new("figures", "fact_figures", dims, vec![AMOUNT])
            .validate()
            .expect("well-formed")
            .version()
    };
    assert_ne!(build(small), build(large));
}

#[test]
fn renaming_the_cube_or_the_fact_table_changes_the_version() {
    let base = Definition::new("figures", "fact_figures", dimensions(), vec![AMOUNT])
        .validate()
        .expect("well-formed");
    let renamed = Definition::new("totals", "fact_figures", dimensions(), vec![AMOUNT])
        .validate()
        .expect("well-formed");
    let retabled = Definition::new("figures", "fact_totals", dimensions(), vec![AMOUNT])
        .validate()
        .expect("well-formed");

    assert_ne!(base.version(), renamed.version());
    assert_ne!(base.version(), retabled.version());
    assert_ne!(renamed.version(), retabled.version());
}
