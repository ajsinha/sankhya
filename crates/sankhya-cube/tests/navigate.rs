//! Slice, dice, roll up, drill down, pivot — and the one of them that can be wrong.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_cube::cells::Cells;
use sankhya_cube::navigate::{
    consolidate_along, dice, pivot, roll_up, slice, Ordered, Refused,
};
use sankhya_cube_algo::measure::{Along, Measure, Rule};

/// Composes along both: an ordinary additive amount.
fn amount() -> Measure {
    Measure::new("amount", vec![
        Along::new("period", Rule::Sum),
        Along::new("entity", Rule::Sum),
    ])
}

/// A closing balance: summing it across time is the classic wrong answer.
fn balance() -> Measure {
    Measure::new("balance", vec![
        Along::new("period", Rule::Last),
        Along::new("entity", Rule::Sum),
    ])
}

/// Declares nothing about `period` at all. A different failure from `balance()`.
fn silent() -> Measure {
    Measure::new("ratio", vec![Along::new("entity", Rule::Sum)])
}

fn address(members: &[&str]) -> Vec<String> {
    members.iter().map(|m| (*m).to_string()).collect()
}

/// A 2×2 cube over (period, entity).
fn cube() -> Cells {
    let mut cells = Cells::over(vec!["period".to_string(), "entity".to_string()]);
    for (period, entity, value) in [
        ("jan", "a", 1.0),
        ("jan", "b", 2.0),
        ("feb", "a", 4.0),
        ("feb", "b", 8.0),
    ] {
        cells.add(address(&[period, entity]), value).expect("well-formed");
    }
    cells
}

// --- roll-up is the one that can be wrong -------------------------------

#[test]
fn a_closing_balance_is_reduced_by_the_measures_rule_not_the_callers() {
    // A balance is not *additive* over time and it is *composable* over time: the closing
    // balance of a quarter is the closing balance of its last month. Additivity is about
    // the operator, composability about permission, and conflating them either refuses a
    // legitimate roll-up or performs an illegitimate one.
    //
    // What must not be possible is the caller choosing to sum it. The result holds one
    // value per cell, already reduced under `Last`, so there is nothing left to re-reduce.
    let rolled = roll_up(&cube(), "period", &balance(), Ordered::By(&["jan", "feb"]))
        .expect("a balance composes along time");

    assert_eq!(rolled.get(&address(&["a"]), Rule::Sum), Some(4.0), "february's, not 5.0");
    assert_eq!(rolled.get(&address(&["b"]), Rule::Sum), Some(8.0));
    assert_eq!(
        rolled.contributions(&address(&["a"])).map(|c| c.len()),
        Some(1),
        "one value per cell — asking for a different rule cannot change the answer"
    );
}

#[test]
fn a_positional_rule_without_a_member_order_is_refused_not_guessed() {
    // The trap underneath the one above, and the reason this is a parameter rather than a
    // comment. Taking contributions in visit order means lexicographic member order, and
    // `"feb" < "jan"` makes the closing balance of the first quarter January's.
    //
    // It survives testing because ISO-8601 dates sort correctly. A system tested with
    // `2026-01`, `2026-02` never exhibits it, and the first wrong number appears against
    // member names somebody chose for a report.
    let refused = roll_up(&cube(), "period", &balance(), Ordered::Unstated)
        .expect_err("a closing balance was computed from an unordered bag");
    assert_eq!(
        refused,
        Refused::OrderRequired {
            measure: "balance".to_string(),
            dimension: "period".to_string(),
            rule: Rule::Last,
        }
    );

    // And the wrong order is a different answer, which is the whole point.
    let backwards = roll_up(&cube(), "period", &balance(), Ordered::By(&["feb", "jan"]))
        .expect("stated, if wrong");
    assert_eq!(backwards.get(&address(&["a"]), Rule::Sum), Some(1.0));
}

#[test]
fn a_member_missing_from_the_stated_order_is_refused() {
    // Placing it first or last guesses at the one thing the order settles; dropping it
    // loses facts from a total.
    assert_eq!(
        roll_up(&cube(), "period", &balance(), Ordered::By(&["jan"])),
        Err(Refused::MemberNotOrdered {
            dimension: "period".to_string(),
            member: "feb".to_string(),
        })
    );
}

#[test]
fn an_order_independent_rule_needs_no_stated_order() {
    // A sum, a maximum and a minimum give the same answer however the contributions are
    // ordered. Demanding an order there would be ceremony, and ceremony gets worked around.
    assert!(roll_up(&cube(), "period", &amount(), Ordered::Unstated).is_ok());
}

#[test]
fn an_undeclared_dimension_is_a_different_refusal_from_a_declared_one() {
    // A measure that declares it does not compose is a correct model of an awkward
    // quantity — the refusal is the system working. A measure that says nothing is a gap
    // somebody must close, and telling them "cannot be rolled up" sends them to argue with
    // a rule that was never written.
    let refused =
        roll_up(&cube(), "period", &silent(), Ordered::Unstated).expect_err("computed anyway");
    assert_eq!(
        refused,
        Refused::Undeclared {
            measure: "ratio".to_string(),
            dimension: "period".to_string(),
        }
    );
    assert!(
        refused.to_string().contains("gap in the cube definition"),
        "the message must send somebody to the definition: {refused}"
    );
}

#[test]
fn rolling_an_additive_measure_aggregates_the_axis_away() {
    let rolled =
        roll_up(&cube(), "period", &amount(), Ordered::Unstated).expect("additive along time");
    assert_eq!(rolled.dimensions(), ["entity"]);
    assert_eq!(rolled.get(&address(&["a"]), Rule::Sum), Some(5.0));
    assert_eq!(rolled.get(&address(&["b"]), Rule::Sum), Some(10.0));
}

#[test]
fn a_balance_may_still_be_rolled_up_along_a_dimension_it_composes_on() {
    // Summed across entities, held as a closing figure across time: the rule is per
    // dimension, so one measure can do both.
    let rolled =
        roll_up(&cube(), "entity", &balance(), Ordered::Unstated).expect("composes along entity");
    assert_eq!(rolled.dimensions(), ["period"]);
    assert_eq!(rolled.get(&address(&["jan"]), Rule::Sum), Some(3.0));
}

#[test]
fn merging_cells_reduces_once_over_the_union_not_over_partial_answers() {
    // Two cells becoming one must reduce over every contributing fact, not combine two
    // partial answers. For a sum the two agree; for a mean they do not, and an average of
    // averages is wrong by an amount depending on how many rows fell in each cell.
    //
    // `Mean` does not compose, so that mistake is unrepresentable here rather than merely
    // avoided — but the union property is what makes the sum right too.
    let mut cells = Cells::over(vec!["period".to_string(), "entity".to_string()]);
    for (entity, value) in [("a", 1.0), ("a", 2.0), ("a", 3.0), ("b", 10.0)] {
        cells.add(address(&["jan", entity]), value).expect("well-formed");
    }
    let rolled = roll_up(&cells, "entity", &amount(), Ordered::Unstated).expect("additive");
    assert_eq!(rolled.get(&address(&["jan"]), Rule::Sum), Some(16.0));
    assert_eq!(
        rolled.contributions(&address(&["jan"])).map(|c| c.len()),
        Some(1),
        "reduced once, over all four facts"
    );
}

#[test]
fn a_reduced_cell_answers_with_the_rule_that_produced_it() {
    // The value was produced by the measure's declared rule and there is no second reading
    // of it. Asking a rolled-up sum for its maximum is a category error, and answering with
    // the maximum of an expansion's components would be a number with no meaning at all.
    let rolled = roll_up(&cube(), "entity", &amount(), Ordered::Unstated).expect("additive");
    let expected = rolled.get(&address(&["jan"]), Rule::Sum);
    assert_eq!(expected, Some(3.0));

    // `Rule::None` is the one that shows the difference. The others agree by accident,
    // because a reduced cell holds a single value and every rule over one value is that
    // value. A non-composing rule would make the cell vanish — and a cell that already
    // carries its answer does not depend on what rule the reader happens to name.
    for asked in [Rule::Max, Rule::Min, Rule::First, Rule::Last, Rule::Mean, Rule::None] {
        assert_eq!(
            rolled.get(&address(&["jan"]), asked),
            expected,
            "a reduced cell changed its answer when asked for {asked}"
        );
    }
    assert_eq!(
        rolled.contributions(&address(&["jan"])).and_then(|c| c.rule_used()),
        Some(Rule::Sum),
        "and it says which rule it used"
    );
}

// --- slice --------------------------------------------------------------

#[test]
fn slicing_fixes_a_member_and_drops_the_axis() {
    // The axis goes because it no longer distinguishes anything. Keeping it leaves a
    // degenerate dimension, and a later roll-up along it silently does nothing.
    let sliced = slice(&cube(), "period", "jan");
    assert_eq!(sliced.dimensions(), ["entity"]);
    assert_eq!(sliced.len(), 2);
    assert_eq!(sliced.get(&address(&["a"]), Rule::Sum), Some(1.0));
}

#[test]
fn slicing_to_a_member_that_does_not_exist_gives_an_empty_cube_not_zeros() {
    let sliced = slice(&cube(), "period", "mar");
    assert!(sliced.is_empty());
    assert_eq!(sliced.get(&address(&["a"]), Rule::Sum), None);
}

// --- dice ---------------------------------------------------------------

#[test]
fn dicing_narrows_without_reducing_rank() {
    // Which is what makes it composable with a later roll-up.
    let diced = dice(&cube(), &[("entity", &["a"][..])]);
    assert_eq!(diced.cells.dimensions(), ["period", "entity"]);
    assert_eq!(diced.cells.len(), 2);
    assert!(diced.ignored.is_empty());
}

#[test]
fn a_restriction_on_a_dimension_the_cube_lacks_is_reported_not_silently_dropped() {
    // A filter that silently did not apply is the kind of thing found in a reconciliation
    // months later. The caller gets back more than they asked for and is told so.
    let diced = dice(&cube(), &[("product", &["x"][..])]);
    assert_eq!(diced.ignored, ["product"]);
    assert_eq!(diced.cells.len(), 4, "nothing was filtered");
}

// --- pivot --------------------------------------------------------------

#[test]
fn pivoting_reorders_axes_and_changes_no_value() {
    let pivoted = pivot(&cube(), &["entity", "period"]);
    assert_eq!(pivoted.dimensions(), ["entity", "period"]);
    assert_eq!(pivoted.get(&address(&["b", "feb"]), Rule::Sum), Some(8.0));
    assert_eq!(pivoted.len(), cube().len());
}

#[test]
fn pivoting_twice_returns_the_original() {
    let there = pivot(&cube(), &["entity", "period"]);
    let back = pivot(&there, &["period", "entity"]);
    assert_eq!(back, cube());
}

#[test]
fn a_partial_pivot_keeps_the_unnamed_axes_rather_than_losing_them() {
    let mut cells = Cells::over(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    cells.add(address(&["1", "2", "3"]), 1.0).expect("well-formed");
    let pivoted = pivot(&cells, &["c"]);
    assert_eq!(pivoted.dimensions(), ["c", "a", "b"]);
    assert_eq!(pivoted.get(&address(&["3", "1", "2"]), Rule::Sum), Some(1.0));
}

// --- drill along a hierarchy --------------------------------------------

#[test]
fn consolidating_members_keeps_the_rank_and_merges_the_cells() {
    let parents = |member: &str| match member {
        "a" | "b" => Some("north".to_string()),
        _ => None,
    };
    let rolled = consolidate_along(&cube(), "entity", &parents, &amount(), Ordered::Unstated)
        .expect("additive along entity");

    assert_eq!(rolled.dimensions(), ["period", "entity"], "rank unchanged");
    assert_eq!(rolled.get(&address(&["jan", "north"]), Rule::Sum), Some(3.0));
}

#[test]
fn a_member_with_no_parent_stays_where_it_is() {
    // What makes a ragged hierarchy work. A branch that reaches its top three levels below
    // another one is at its top, and inventing a parent puts a member in the result that
    // does not exist.
    let parents = |member: &str| (member == "a").then(|| "north".to_string());
    let rolled = consolidate_along(&cube(), "entity", &parents, &amount(), Ordered::Unstated)
        .expect("additive");

    assert_eq!(rolled.get(&address(&["jan", "north"]), Rule::Sum), Some(1.0));
    assert_eq!(
        rolled.get(&address(&["jan", "b"]), Rule::Sum),
        Some(2.0),
        "`b` has no parent and was not invented one"
    );
}

#[test]
fn consolidating_by_position_without_an_order_is_refused() {
    // Consolidating members merges cells whatever the rank says, so it takes the same
    // refusals as a roll-up.
    let parents = |_: &str| Some("all".to_string());
    assert!(matches!(
        consolidate_along(&cube(), "period", &parents, &balance(), Ordered::Unstated),
        Err(Refused::OrderRequired { .. })
    ));
    assert!(matches!(
        consolidate_along(&cube(), "period", &parents, &silent(), Ordered::Unstated),
        Err(Refused::Undeclared { .. })
    ));
}

// --- composition --------------------------------------------------------

#[test]
fn the_operations_compose_because_each_returns_a_cube() {
    // Dice, then roll up, then read — one structure navigated, not three unrelated queries.
    let diced = dice(&cube(), &[("period", &["jan", "feb"][..])]);
    let rolled = roll_up(&diced.cells, "period", &amount(), Ordered::Unstated).expect("additive");
    let sliced = slice(&rolled, "entity", "b");
    assert_eq!(sliced.dimensions(), [] as [String; 0]);
    assert_eq!(sliced.get(&[], Rule::Sum), Some(10.0), "the grand total for b");
}
