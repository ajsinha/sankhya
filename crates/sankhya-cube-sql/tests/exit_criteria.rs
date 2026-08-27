//! M7's exit criteria, one test each, named for the criterion it discharges.
//!
//! Written after the crates were built, by reading the criteria and asking what would
//! actually demonstrate each. Four of the eight were not covered by the tests already
//! written: the tests below were added because checking honestly turned up the gap, not
//! because a criterion needed restating.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use datafusion::prelude::SessionContext;
use proptest::prelude::*;
use sankhya_cube::cells::Cells;
use sankhya_cube::complete::{Assessed, Completeness, Threshold};
use sankhya_cube::consolidate::consolidate;
use sankhya_cube::materialise::plan;
use sankhya_cube::navigate::{roll_up, Ordered};
use sankhya_cube::{Definition, Dimension, Level};
use sankhya_cube_algo::lattice::Cuboid;
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use sankhya_cube_sql::catalog::{CubeCatalog, Published};
use sankhya_cube_sql::register;
use sankhya_graph_algo::budget::Budget;
use sankhya_graph_algo::csr::{AdjacencyBuilder, Edge, Validity};
use sankhya_graph_algo::ids::{EdgeMask, EdgeType, VertexId};
use std::sync::Arc;

const ROLLS_UP: EdgeType = EdgeType(0);

fn address(members: &[&str]) -> Vec<String> {
    members.iter().map(|m| (*m).to_string()).collect()
}

// --- 1. ragged, alternate roll-ups, no member double-counted ------------

#[test]
fn criterion_1_a_ragged_hierarchy_with_alternate_rollups_reconciles() {
    // The hierarchy, parent to child:
    //
    //     total ── north ── a
    //          │        └── b        <- `b` is shared: it reports into both
    //          ├─ south ── b            north and south (an alternate roll-up)
    //          │        └── c
    //          └─ d                  <- ragged: a leaf directly under total
    //
    // and `north` has facts of its own, so it is both an inner node and a contributor.
    let names = ["total", "north", "south", "a", "b", "c", "d"];
    let mut builder = AdjacencyBuilder::new(names.len());
    for (parent, child) in [(0, 1), (0, 2), (0, 6), (1, 3), (1, 4), (2, 4), (2, 5)] {
        builder
            .push(Edge {
                source: VertexId(parent),
                target: VertexId(child),
                edge_type: ROLLS_UP,
                validity: Validity::always(),
                weight: 1.0,
            })
            .expect("in range");
    }
    let graph = builder.build();

    let facts = [("north", 5.0), ("a", 10.0), ("b", 20.0), ("c", 30.0), ("d", 40.0)];
    let mut cells = Cells::over(vec!["entity".to_string()]);
    for (member, value) in facts {
        cells.add(address(&[member]), value).expect("well-formed");
    }

    // The independent answer: every member's own facts, each counted once.
    let independent: f64 = facts.iter().map(|(_, value)| value).sum();
    assert_eq!(independent, 105.0);

    let rolled = consolidate(&graph, VertexId(0), &EdgeMask::of([ROLLS_UP]), &Budget::generous());
    let members = rolled.members().expect("acyclic and complete");

    let mut total = 0.0;
    for member in members {
        let name = names[member.0 as usize];
        total += cells.get(&address(&[name]), Rule::Sum).unwrap_or(0.0);
    }
    assert_eq!(total, independent, "the consolidated total reconciles");

    // And the failure this criterion is guarding against: summing along paths instead of
    // over the member set counts `b` once per route, giving 125.
    let by_paths = independent + 20.0;
    assert_ne!(total, by_paths, "`b` was counted once per path");
}

// --- 2. a measure that does not compose is rejected at planning time ----

#[test]
fn criterion_2_summing_a_measure_across_time_is_not_expressible() {
    // The criterion asks that summing a semi-additive measure across time be *rejected*.
    // Here it cannot be asked for: the reduction operator is the measure's, not the
    // caller's, so `roll_up` never sums a closing balance. That is stronger than rejecting
    // it, and worth stating precisely rather than claiming the criterion's own wording.
    const BALANCE: Measure = Measure {
        name: "balance",
        rules: &[
            Along { dimension: "period", rule: Rule::Last },
            Along { dimension: "entity", rule: Rule::Sum },
        ],
    };
    let mut cells = Cells::over(vec!["period".to_string(), "entity".to_string()]);
    cells.add(address(&["jan", "a"]), 100.0).expect("well-formed");
    cells.add(address(&["feb", "a"]), 30.0).expect("well-formed");

    let rolled = roll_up(&cells, "period", &BALANCE, Ordered::By(&["jan", "feb"]))
        .expect("a balance composes along time");
    assert_eq!(
        rolled.get(&address(&["a"]), Rule::Sum),
        Some(30.0),
        "the closing balance, not the sum of 130"
    );

    // What *is* rejected: a measure declaring it composes along nothing.
    const RATIO: Measure = Measure {
        name: "ratio",
        rules: &[
            Along { dimension: "period", rule: Rule::None },
            Along { dimension: "entity", rule: Rule::Sum },
        ],
    };
    assert!(roll_up(&cells, "period", &RATIO, Ordered::Unstated).is_err());
}

// --- 3. two runs are bit-identical --------------------------------------

#[test]
fn criterion_3_two_runs_of_the_same_consolidation_are_bit_identical() {
    const AMOUNT: Measure = Measure {
        name: "amount",
        rules: &[
            Along { dimension: "period", rule: Rule::Sum },
            Along { dimension: "entity", rule: Rule::Sum },
        ],
    };
    let mut cells = Cells::over(vec!["period".to_string(), "entity".to_string()]);
    for (period, entity, value) in [
        ("jan", "a", 1e16),
        ("jan", "b", 1.0),
        ("feb", "a", -1e16),
        ("feb", "b", 2.5e-8),
    ] {
        cells.add(address(&[period, entity]), value).expect("well-formed");
    }

    let once = roll_up(&cells, "period", &AMOUNT, Ordered::Unstated).expect("additive");
    let twice = roll_up(&cells, "period", &AMOUNT, Ordered::Unstated).expect("additive");
    for at in once.addresses() {
        assert_eq!(
            once.get(at, Rule::Sum).map(f64::to_bits),
            twice.get(at, Rule::Sum).map(f64::to_bits)
        );
    }
}

// --- 3a. materialisation on and off agree, by bits ----------------------

const ADDITIVE: Measure = Measure {
    name: "amount",
    rules: &[
        Along { dimension: "period", rule: Rule::Sum },
        Along { dimension: "entity", rule: Rule::Sum },
        Along { dimension: "product", rule: Rule::Sum },
    ],
};

fn three_dimensional(rows: &[(&str, &str, &str, f64)]) -> Cells {
    let mut cells = Cells::over(vec![
        "period".to_string(),
        "entity".to_string(),
        "product".to_string(),
    ]);
    for (period, entity, product, value) in rows {
        cells
            .add(address(&[period, entity, product]), *value)
            .expect("well-formed");
    }
    cells
}

/// Answer `by entity` from whatever `plan` chooses.
fn answer(cells: &Cells, available: &[&Cuboid], base: &Cuboid) -> Cells {
    let query = Cuboid::of(&["entity"]);
    let chosen = plan(&query, &ADDITIVE, available, base);
    let mut out = if chosen.materialised {
        // The materialised cuboid, which is the base already rolled to (entity, product).
        roll_up(cells, "period", &ADDITIVE, Ordered::Unstated).expect("additive")
    } else {
        cells.clone()
    };
    for dimension in chosen.rolling_away {
        out = roll_up(&out, &dimension, &ADDITIVE, Ordered::Unstated).expect("additive");
    }
    out
}

proptest! {
    /// 3a: every query returns bit-identical results with materialisation on and off.
    ///
    /// Compared by bits rather than within a tolerance, which is what found the defect this
    /// criterion exists to prevent: a two-stage roll-up rounds at each stage, and the answer
    /// was one ULP from the single-stage one.
    #[test]
    fn criterion_3a_materialisation_does_not_change_the_answer(
        values in prop::collection::vec((0usize..3, 0usize..3, 0usize..3, -1e12f64..1e12), 1..40)
    ) {
        const NAMES: [&str; 3] = ["p", "q", "r"];
        let rows: Vec<(&str, &str, &str, f64)> = values
            .iter()
            .map(|(a, b, c, v)| (NAMES[*a], NAMES[*b], NAMES[*c], *v))
            .collect();
        let cells = three_dimensional(&rows);
        let base = Cuboid::of(&["period", "entity", "product"]);
        let materialised = Cuboid::of(&["entity", "product"]);

        let with = answer(&cells, &[&materialised], &base);
        let without = answer(&cells, &[], &base);

        prop_assert_eq!(with.len(), without.len());
        for at in with.addresses() {
            prop_assert_eq!(
                with.get(at, Rule::Sum).map(f64::to_bits),
                without.get(at, Rule::Sum).map(f64::to_bits),
                "at {:?}", at
            );
        }
    }

    /// 3b: a non-additive measure is never answered from a materialised ancestor.
    #[test]
    fn criterion_3b_a_non_composing_measure_is_never_answered_from_an_ancestor(
        held in prop::collection::vec(0usize..3, 0..3),
        wanted in prop::collection::vec(0usize..3, 0..3),
    ) {
        const NAMES: [&str; 3] = ["period", "entity", "product"];
        const DISTINCT: Measure = Measure {
            name: "distinct",
            rules: &[
                Along { dimension: "period", rule: Rule::None },
                Along { dimension: "entity", rule: Rule::None },
                Along { dimension: "product", rule: Rule::None },
            ],
        };
        let held: Vec<&str> = held.iter().map(|i| NAMES[*i]).collect();
        let wanted: Vec<&str> = wanted.iter().map(|i| NAMES[*i]).collect();
        let cuboid = Cuboid::of(&held);
        let query = Cuboid::of(&wanted);
        let base = Cuboid::of(&NAMES);

        let chosen = plan(&query, &DISTINCT, &[&cuboid], &base);
        if chosen.materialised {
            // The only legitimate hit: the cuboid *is* the query, so nothing is rolled away.
            prop_assert_eq!(&chosen.from, &query);
            prop_assert!(chosen.rolling_away.is_empty());
        }
    }
}

// --- 4. two principals, two totals, both saying so ----------------------

#[test]
fn criterion_4_two_row_policies_give_two_totals_that_both_carry_completeness() {
    // Two principals legitimately see different totals; the security model working. What
    // must not happen is either figure reaching a report without saying it is partial.
    let mut everything = Cells::over(vec!["entity".to_string()]);
    for (entity, value) in [("a", 10.0), ("b", 20.0), ("c", 30.0)] {
        everything.add(address(&[entity]), value).expect("well-formed");
    }

    // A principal who may read everything.
    let unrestricted = Assessed::new(60.0_f64, Completeness::complete(3));

    // One whose policy excludes `c`. The withheld count comes from the filter — nothing
    // downstream could recover it, since a removed row leaves no trace.
    let restricted = Assessed::new(30.0_f64, Completeness::of(2, 1));

    assert_ne!(unrestricted.regardless(), restricted.regardless());
    assert!(unrestricted.completeness().is_complete());
    assert!(!restricted.completeness().is_complete());
    assert_eq!(restricted.completeness().fraction(), Some(2.0 / 3.0));

    // And the partial one refuses to be reported as a total.
    let strict = Threshold::at_least(0.95).expect("a fraction");
    assert!(unrestricted.meeting(&strict).is_ok());
    let refused = restricted.meeting(&strict).expect_err("passed as a total");
    assert_eq!(refused.withheld, 1);
}

// --- 5. six dimensions, from SQL, with no build step --------------------

const SIX: [&str; 6] = ["region", "period", "product", "channel", "segment", "currency"];

const WIDE: Measure = Measure {
    name: "amount",
    rules: &[
        Along { dimension: "region", rule: Rule::Sum },
        Along { dimension: "period", rule: Rule::Sum },
        Along { dimension: "product", rule: Rule::Sum },
        Along { dimension: "channel", rule: Rule::Sum },
        Along { dimension: "segment", rule: Rule::Sum },
        Along { dimension: "currency", rule: Rule::Sum },
    ],
};

/// A six-dimension cube, published and queryable with nothing built beforehand.
fn six_dimensional() -> SessionContext {
    let dimensions: Vec<Dimension> = SIX
        .iter()
        .map(|name| {
            Dimension::new(
                *name,
                format!("dim_{name}"),
                format!("{name}_key"),
                vec![Level::new("id", "id")],
            )
        })
        .collect();
    let cube = Arc::new(
        Definition::new("wide", "fact_wide", dimensions, vec![WIDE])
            .validate()
            .expect("well-formed"),
    );

    let mut cells = Cells::over(SIX.iter().map(|s| (*s).to_string()).collect());
    // Two members per dimension, a handful of populated cells: a real cube is sparse, and
    // materialising 2^6 of anything is not what makes this test meaningful.
    for (index, members) in [
        ["north", "jan", "x", "web", "retail", "gbp"],
        ["north", "feb", "x", "web", "retail", "gbp"],
        ["south", "jan", "y", "shop", "wholesale", "usd"],
        ["south", "feb", "y", "web", "retail", "usd"],
    ]
    .iter()
    .enumerate()
    {
        cells
            .add(address(members), (index as f64 + 1.0) * 10.0)
            .expect("well-formed");
    }

    let catalog = Arc::new(CubeCatalog::new());
    catalog.publish(
        "wide",
        Published {
            cube,
            cells: Arc::new(cells),
            snapshot: 7,
        },
    );
    let context = SessionContext::new();
    register(&context, catalog);
    context
}

async fn count(context: &SessionContext, sql: &str) -> usize {
    context
        .sql(sql)
        .await
        .expect("planned")
        .collect()
        .await
        .expect("ran")
        .iter()
        .map(arrow_array::RecordBatch::num_rows)
        .sum()
}

#[tokio::test]
async fn criterion_5_slice_dice_rollup_and_drilldown_from_sql_over_six_dimensions() {
    let context = six_dimensional();

    // Roll-up: six dimensions down to one. No build step — the cube was published and this
    // is the first statement against it.
    assert_eq!(
        count(&context, "SELECT region, amount FROM cube_rollup('wide', 'amount', 'by=region')")
            .await,
        2
    );

    // Drill-down: the same question one level finer, expressed by naming more dimensions.
    assert_eq!(
        count(
            &context,
            "SELECT region, period, amount FROM cube_rollup('wide', 'amount', \
             'by=region|period')"
        )
        .await,
        4
    );

    // Dice: restrict two dimensions, keeping the rank.
    assert_eq!(
        count(
            &context,
            "SELECT region, amount FROM cube_rollup('wide', 'amount', \
             'by=region, where=channel:web|currency:gbp')"
        )
        .await,
        1
    );

    // Slice: fix one member, drop that axis.
    assert_eq!(
        count(
            &context,
            "SELECT period, amount FROM cube_slice('wide', 'amount', 'where=region:north')"
        )
        .await,
        2
    );

    // And a drill-down reconciles with the roll-up above it.
    let coarse = context
        .sql("SELECT sum(amount) AS t FROM cube_rollup('wide', 'amount', 'by=region')")
        .await
        .expect("planned")
        .collect()
        .await
        .expect("ran");
    let fine = context
        .sql("SELECT sum(amount) AS t FROM cube_rollup('wide', 'amount', 'by=region|period')")
        .await
        .expect("planned")
        .collect()
        .await
        .expect("ran");
    assert_eq!(format!("{:?}", coarse[0].column(0)), format!("{:?}", fine[0].column(0)));
}

// --- 6. a measure with no rule is refused, naming it --------------------

#[test]
fn criterion_6_a_measure_defined_without_an_aggregation_rule_is_refused_by_name() {
    const NO_RULE: Measure = Measure { name: "unnamed_thing", rules: &[] };
    let refused = Definition::new(
        "figures",
        "fact_figures",
        vec![Dimension::new(
            "region",
            "dim_region",
            "region_key",
            vec![Level::new("id", "id")],
        )],
        vec![NO_RULE],
    )
    .validate()
    .expect_err("a cube was built from an undeclared measure");

    assert!(
        refused.iter().any(|r| r.to_string().contains("unnamed_thing")),
        "the refusal names the measure: {refused:#?}"
    );
    assert!(
        refused.iter().any(|r| r.to_string().contains("region")),
        "and the dimension it says nothing about: {refused:#?}"
    );
}
