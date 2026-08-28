//! What may be rolled up, and the far more important what may not.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_cube_algo::ancestor::{answerable_from, rolled_away, Answerable};
use sankhya_cube_algo::measure::{Along, Measure, Rule};

/// Transaction amount: adds along everything.
const AMOUNT: Measure = Measure {
    name: "amount",
    rules: &[
        Along {
            dimension: "time",
            rule: Rule::Sum,
        },
        Along {
            dimension: "account",
            rule: Rule::Sum,
        },
        Along {
            dimension: "region",
            rule: Rule::Sum,
        },
    ],
};

/// A closing balance: adds across accounts, takes the last across time.
///
/// The classic semi-additive measure, and the classic silent wrong answer.
const BALANCE: Measure = Measure {
    name: "closing_balance",
    rules: &[
        Along {
            dimension: "time",
            rule: Rule::Last,
        },
        Along {
            dimension: "account",
            rule: Rule::Sum,
        },
        Along {
            dimension: "region",
            rule: Rule::Sum,
        },
    ],
};

/// A distinct count: derivable from nothing but the base rows.
const DISTINCT_CUSTOMERS: Measure = Measure {
    name: "distinct_customers",
    rules: &[
        Along {
            dimension: "time",
            rule: Rule::None,
        },
        Along {
            dimension: "account",
            rule: Rule::None,
        },
        Along {
            dimension: "region",
            rule: Rule::None,
        },
    ],
};

/// A margin percentage: an average that must not be averaged.
const MARGIN: Measure = Measure {
    name: "margin_pct",
    rules: &[
        Along {
            dimension: "time",
            rule: Rule::Mean,
        },
        Along {
            dimension: "account",
            rule: Rule::Mean,
        },
        Along {
            dimension: "region",
            rule: Rule::Mean,
        },
    ],
};

const DIMENSIONS: &[&str] = &["time", "account", "region"];

// --- the declaration ----------------------------------------------------

#[test]
fn a_measure_with_no_rule_along_a_dimension_is_refused() {
    // Not defaulted to summation. The default is wrong for an entire class of measures and
    // wrong invisibly: a closing balance summed across twelve months gives the sum of twelve
    // month-end balances, which nobody wanted and which looks exactly like a number that
    // means something.
    const PARTIAL: Measure = Measure {
        name: "quantity",
        rules: &[Along {
            dimension: "time",
            rule: Rule::Sum,
        }],
    };
    let refused = PARTIAL.covers(DIMENSIONS).expect_err("two dimensions are undeclared");
    assert_eq!(refused.dimensions, ["account", "region"]);
    assert!(
        refused.to_string().contains("month-end balances"),
        "the refusal says why a default would be wrong: {refused}"
    );
}

#[test]
fn every_undeclared_dimension_is_named_not_just_the_first() {
    // An author fixing them one at a time learns about the next only after another load.
    const NONE: Measure = Measure {
        name: "mystery",
        rules: &[],
    };
    let refused = NONE.covers(DIMENSIONS).expect_err("nothing is declared");
    assert_eq!(refused.dimensions.len(), 3);
}

#[test]
fn a_fully_declared_measure_is_accepted() {
    assert!(AMOUNT.covers(DIMENSIONS).is_ok());
    assert!(BALANCE.covers(DIMENSIONS).is_ok());
}

#[test]
fn additive_everywhere_is_a_different_question_from_declared_everywhere() {
    assert!(AMOUNT.additive_everywhere());
    assert!(!BALANCE.additive_everywhere(), "it is not additive over time");
    assert_eq!(BALANCE.not_additive_along(), ["time"]);
    assert_eq!(AMOUNT.not_additive_along(), Vec::<&str>::new());
}

// --- the roll-up, which is where wrong answers come from ----------------

#[test]
fn an_additive_measure_rolls_up_along_anything() {
    assert_eq!(answerable_from(&AMOUNT, &["region"]), Answerable::Yes);
    assert_eq!(
        answerable_from(&AMOUNT, &["time", "account", "region"]),
        Answerable::Yes
    );
}

#[test]
fn a_semi_additive_measure_can_still_be_rolled_up_along_its_non_additive_axis() {
    // **Not additive** and **cannot be rolled up** are different properties, and conflating
    // them was a real error in the first version of these tests — written from the ADR's own
    // prose, which says a semi-additive measure "is correct along most dimensions and wrong
    // along exactly one". That describes what happens when an implementation *sums*
    // everywhere. It is not a statement about a correct one.
    //
    // A balance rolled up across time is the **last** balance, and that is a perfectly good
    // answer. `last(last(a, b), c) == last(a, b, c)` given an order, so the rule composes and
    // partial aggregates are sufficient.
    assert_eq!(answerable_from(&BALANCE, &["time"]), Answerable::Yes);
    assert_eq!(answerable_from(&BALANCE, &["account", "region"]), Answerable::Yes);
    assert_eq!(
        answerable_from(&BALANCE, &["time", "account", "region"]),
        Answerable::Yes
    );
}

#[test]
fn the_danger_of_a_semi_additive_measure_is_the_operator_not_the_axis() {
    // So where is the silent wrong answer everybody warns about? Not here. It is one layer
    // up, in the executor: rolling a balance across time is valid **if it applies `last`**
    // and wrong if it applies `sum`. The predicate below says the roll-up is permitted; what
    // it must be performed *with* is the rule, and that is what an executor has to honour.
    //
    // Stated as a test because the two get confused — this file's first version refused a
    // valid roll-up on the strength of the confusion, which would have sent every balance
    // query to base data for no reason.
    assert_eq!(BALANCE.rule("time"), Some(Rule::Last));
    assert_eq!(BALANCE.rule("account"), Some(Rule::Sum));
    assert!(
        !BALANCE.additive_everywhere(),
        "it is not additive over time — which is about the operator, not about permission"
    );
    assert_eq!(BALANCE.not_additive_along(), ["time"]);
}

#[test]
fn one_axis_that_does_not_compose_refuses_the_whole_roll_up() {
    // A roll-up valid along two axes and invalid along a third is invalid. This is the case
    // that survives casual checking, because most of the result is right.
    const MIXED: Measure = Measure {
        name: "mixed",
        rules: &[
            Along { dimension: "time", rule: Rule::Sum },
            Along { dimension: "account", rule: Rule::Sum },
            // A ratio over regions: derivable from nothing but base rows.
            Along { dimension: "region", rule: Rule::None },
        ],
    };
    assert_eq!(answerable_from(&MIXED, &["time", "account"]), Answerable::Yes);

    let refused = answerable_from(&MIXED, &["time", "account", "region"]);
    let Answerable::No { dimension, rule, .. } = &refused else {
        panic!("one bad axis refuses the roll-up: {refused:?}");
    };
    assert_eq!(dimension, "region", "and it names the axis that forbade it");
    assert_eq!(*rule, Rule::None);
    assert!(
        refused.to_string().contains("wrong, plausible"),
        "the message says what the failure looks like, because it looks like nothing: \
         {refused}"
    );
}

#[test]
fn a_non_additive_measure_rolls_up_along_nothing() {
    // Which is the honest outcome: every query goes to base data. Slow, and correct.
    for axis in DIMENSIONS {
        assert!(
            !answerable_from(&DISTINCT_CUSTOMERS, &[axis]).permitted(),
            "a distinct count cannot be derived from partial distinct counts along {axis}"
        );
    }
}

#[test]
fn a_mean_does_not_compose_even_though_it_is_a_perfectly_good_aggregate() {
    // An average of averages is not an average unless every group is the same size, and
    // groups are never the same size. Excluded from composition rather than from existence.
    assert!(!Rule::Mean.composes());
    assert!(Rule::Sum.composes());
    assert!(Rule::Last.composes());
    assert!(Rule::Max.composes());

    let refused = answerable_from(&MARGIN, &["region"]);
    assert!(!refused.permitted(), "{refused:?}");
}

#[test]
fn rolling_away_nothing_is_always_permitted() {
    // A query for exactly the cuboid that is materialised aggregates nothing, so no rule can
    // forbid it — not even for a measure that composes along no axis at all.
    assert_eq!(answerable_from(&DISTINCT_CUSTOMERS, &[]), Answerable::Yes);
    assert_eq!(answerable_from(&MARGIN, &[]), Answerable::Yes);
}

#[test]
fn an_undeclared_axis_is_distinct_from_a_forbidden_one() {
    // `No` means "this is not valid", which is a modelling answer. `Undeclared` means
    // "nobody said", which is a definition error that should have been caught at load. They
    // send somebody to different places.
    const PARTIAL: Measure = Measure {
        name: "quantity",
        rules: &[Along {
            dimension: "time",
            rule: Rule::Sum,
        }],
    };
    let verdict = answerable_from(&PARTIAL, &["region"]);
    let Answerable::Undeclared { dimension, .. } = &verdict else {
        panic!("an unknown axis is not the same as a forbidden one: {verdict:?}");
    };
    assert_eq!(dimension, "region");
    assert!(verdict.to_string().contains("refused to load"));
    assert!(!verdict.permitted());
}

// --- which cuboid can answer which -------------------------------------

#[test]
fn a_coarser_query_rolls_away_what_the_finer_cuboid_holds() {
    let away = rolled_away(&["time"], &["time", "account", "region"]).expect("finer");
    assert_eq!(away, ["account", "region"]);
}

#[test]
fn a_cuboid_missing_a_dimension_the_query_needs_cannot_answer_it_at_all() {
    // Not a roll-up question: no aggregation recovers a dimension that was already
    // collapsed. `None` rather than an empty list, because an empty list means "roll away
    // nothing", which is permitted.
    assert_eq!(rolled_away(&["time", "account"], &["time"]), None);
    assert_eq!(
        rolled_away(&["region"], &["time", "account"]),
        None,
        "sharing no dimensions is still not answerable"
    );
}

#[test]
fn a_cuboid_that_matches_the_query_exactly_rolls_away_nothing() {
    assert_eq!(
        rolled_away(&["time", "account"], &["time", "account"]),
        Some(Vec::new())
    );
}

#[test]
fn the_two_halves_compose_into_the_decision_a_planner_makes() {
    // What the planner actually asks: here is a query, here is something materialised — may
    // I use it?
    let may_use = |query: &[&str], materialised: &[&str], measure: &Measure| {
        rolled_away(query, materialised)
            .map(|away| answerable_from(measure, &away).permitted())
            .unwrap_or(false)
    };

    // A by-time-and-account cuboid answers a by-account question for an additive measure,
    // and for a balance too — rolling away time is `last`, which composes.
    assert!(may_use(&["account"], &["time", "account"], &AMOUNT));
    assert!(may_use(&["account"], &["time", "account"], &BALANCE));

    // A distinct count cannot be answered from any ancestor at all.
    assert!(!may_use(&["account"], &["time", "account"], &DISTINCT_CUSTOMERS));
    assert!(!may_use(&["time"], &["time", "account"], &MARGIN));

    // And nothing answers a query needing a dimension the cuboid does not hold: no
    // aggregation recovers an axis that was already collapsed.
    assert!(!may_use(&["time", "region"], &["time", "account"], &AMOUNT));
}
