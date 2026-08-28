//! Choosing what to materialise, and the constraint the textbook version omits.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_cube_algo::lattice::{benefit, select, worth_more, Cost, Cuboid, Lattice};
use sankhya_cube_algo::measure::{Along, Measure, Rule};

const DIMS: &[&str] = &["time", "account", "region"];

/// Additive along everything: every roll-up is permitted.
const AMOUNT: Measure = Measure {
    name: "amount",
    rules: &[
        Along { dimension: "time", rule: Rule::Sum },
        Along { dimension: "account", rule: Rule::Sum },
        Along { dimension: "region", rule: Rule::Sum },
    ],
};

/// Composes along nothing: no cuboid can serve any coarser query.
const DISTINCT: Measure = Measure {
    name: "distinct_customers",
    rules: &[
        Along { dimension: "time", rule: Rule::None },
        Along { dimension: "account", rule: Rule::None },
        Along { dimension: "region", rule: Rule::None },
    ],
};

/// Rows fall by a factor of ten for each dimension dropped.
struct ByWidth;

impl Cost for ByWidth {
    fn rows(&self, cuboid: &Cuboid) -> u64 {
        10_u64.pow(u32::try_from(cuboid.width()).unwrap_or(0) + 1)
    }
}

fn base() -> Cuboid {
    Cuboid::of(DIMS)
}

// --- the lattice --------------------------------------------------------

#[test]
fn the_lattice_is_every_subset_of_the_dimensions() {
    let lattice = Lattice::all(DIMS);
    assert_eq!(lattice.len(), 8, "2^3, including the empty grand total");
    assert!(lattice.cuboids().contains(&Cuboid::of::<&str>(&[])));
    assert!(lattice.cuboids().contains(&base()));
}

#[test]
fn two_cuboids_naming_the_same_dimensions_are_one_cuboid() {
    // Otherwise they are two entries competing for space to hold identical data.
    assert_eq!(Cuboid::of(&["time", "account"]), Cuboid::of(&["account", "time"]));
    assert_eq!(Cuboid::of(&["time", "time"]).width(), 1);
    assert_eq!(Lattice::over(vec![base(), base()]).len(), 1);
}

// --- what a cuboid can answer ------------------------------------------

#[test]
fn a_cuboid_answers_a_coarser_query_for_an_additive_measure() {
    let fine = Cuboid::of(&["time", "account"]);
    assert!(fine.answers(&Cuboid::of(&["account"]), &AMOUNT));
    assert!(fine.answers(&fine, &AMOUNT), "and itself");
}

#[test]
fn a_cuboid_answers_nothing_coarser_for_a_measure_that_composes_along_nothing() {
    // A distinct count is derivable from no partial aggregates, so a materialised cuboid
    // serves exactly one query — the one it is.
    let fine = Cuboid::of(&["time", "account"]);
    assert!(!fine.answers(&Cuboid::of(&["account"]), &DISTINCT));
    assert!(fine.answers(&fine, &DISTINCT), "rolling away nothing is always fine");
}

#[test]
fn a_cuboid_never_answers_a_query_needing_a_dimension_it_lacks() {
    let coarse = Cuboid::of(&["account"]);
    assert!(!coarse.answers(&Cuboid::of(&["time", "account"]), &AMOUNT));
}

// --- the constraint the textbook version omits --------------------------

#[test]
fn benefit_counts_only_the_queries_a_measure_permits() {
    // The subtle part, and where the two halves of this crate meet. A cuboid grouping by
    // time and account *looks* like it serves every coarser query; for a distinct count it
    // serves none of them.
    //
    // Count benefit without that test and the selection materialises cuboids whose value was
    // computed from roll-ups the planner will refuse: the storage is paid, the benefit never
    // arrives, and nothing reports it — the cube is simply slower than its own model says.
    let candidate = Cuboid::of(&["time", "account"]);
    let queries = vec![
        Cuboid::of(&["account"]),
        Cuboid::of(&["time"]),
        Cuboid::of(&["time", "account"]),
    ];

    let additive = benefit(&candidate, &queries, &AMOUNT, &ByWidth, &[], &base());
    assert!(additive > 0, "an additive measure benefits from all three");

    let non_additive = benefit(&candidate, &queries, &DISTINCT, &ByWidth, &[], &base());
    assert!(
        non_additive < additive,
        "a distinct count benefits only from the query the cuboid *is*: {non_additive} \
         against {additive}"
    );
}

#[test]
fn a_cuboid_that_serves_nothing_new_has_no_benefit() {
    // The grand total answers only itself for a non-composing measure, and if that is not
    // among the queries there is nothing to gain.
    let candidate = Cuboid::of(&["region"]);
    let queries = vec![Cuboid::of(&["time"])];
    assert_eq!(
        benefit(&candidate, &queries, &AMOUNT, &ByWidth, &[], &base()),
        0
    );
}

// --- selection ----------------------------------------------------------

#[test]
fn selection_takes_the_cuboids_that_pay_for_themselves() {
    let lattice = Lattice::all(DIMS);
    let queries = vec![Cuboid::of(&["account"]), Cuboid::of(&["time"])];
    let chosen = select(&lattice, &queries, &AMOUNT, &ByWidth, 10_000, &base());

    assert!(!chosen.is_empty(), "something was worth keeping");
    for pick in &chosen {
        assert!(pick.benefit > 0, "{pick:?} was chosen for no gain");
        assert_ne!(pick.cuboid, base(), "the base is not a choice");
    }
}

#[test]
fn selection_stays_inside_its_budget() {
    let lattice = Lattice::all(DIMS);
    let queries: Vec<Cuboid> = lattice.cuboids().to_vec();
    let budget = 150_u64;
    let chosen = select(&lattice, &queries, &AMOUNT, &ByWidth, budget, &base());

    let spent: u64 = chosen.iter().map(|c| c.cost).sum();
    assert!(spent <= budget, "spent {spent} of {budget}");
    assert!(!chosen.is_empty(), "a budget of 150 buys something");
}

#[test]
fn a_budget_of_nothing_selects_nothing() {
    let lattice = Lattice::all(DIMS);
    let queries = vec![Cuboid::of(&["account"])];
    assert!(select(&lattice, &queries, &AMOUNT, &ByWidth, 0, &base()).is_empty());
}

#[test]
fn nothing_is_selected_for_a_measure_that_composes_along_nothing() {
    // Every candidate serves only the query it is, so the only cuboids worth keeping are the
    // queried ones themselves — and none of them beats the base for anything else. This is
    // the honest outcome for a distinct count, and a selection ignoring additivity would
    // instead buy a pile of storage that answers nothing.
    let lattice = Lattice::all(DIMS);
    let queries = vec![Cuboid::of(&["account"]), Cuboid::of(&["time"])];
    let chosen = select(&lattice, &queries, &DISTINCT, &ByWidth, 10_000, &base());

    for pick in &chosen {
        assert!(
            queries.contains(&pick.cuboid),
            "{:?} was materialised for a measure it cannot roll up",
            pick.cuboid
        );
    }
}

#[test]
fn the_same_cuboid_is_never_chosen_twice() {
    let lattice = Lattice::all(DIMS);
    let queries: Vec<Cuboid> = lattice.cuboids().to_vec();
    let chosen = select(&lattice, &queries, &AMOUNT, &ByWidth, 100_000, &base());

    let mut seen: Vec<&Cuboid> = chosen.iter().map(|c| &c.cuboid).collect();
    let before = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), before);
}

#[test]
fn later_choices_account_for_earlier_ones() {
    // Greedy selection is only sound if benefit is recomputed against what is already held.
    // Otherwise two cuboids serving the same query are both credited with the full saving,
    // and the second is bought for a benefit the first already delivered.
    let lattice = Lattice::all(DIMS);
    let queries = vec![Cuboid::of(&["account"])];
    let chosen = select(&lattice, &queries, &AMOUNT, &ByWidth, 100_000, &base());

    assert_eq!(
        chosen.len(),
        1,
        "one query needs one cuboid, however much budget there is: {chosen:#?}"
    );
    assert_eq!(chosen[0].cuboid, Cuboid::of(&["account"]));
}

#[test]
fn a_larger_budget_never_selects_less() {
    // Not a strict property of greedy algorithms in general, but it must hold here or the
    // budget is not behaving as a bound.
    let lattice = Lattice::all(DIMS);
    let queries: Vec<Cuboid> = lattice.cuboids().to_vec();
    let small = select(&lattice, &queries, &AMOUNT, &ByWidth, 200, &base());
    let large = select(&lattice, &queries, &AMOUNT, &ByWidth, 100_000, &base());
    assert!(large.len() >= small.len(), "{} then {}", small.len(), large.len());
}

// --- the comparison that fails silently ---------------------------------

#[test]
fn benefit_per_row_is_compared_exactly_not_by_integer_division() {
    // `a/b > c/d` is compared as `a*d > c*b`. Integer division instead rounds two genuinely
    // different candidates to the same score, and the choice between them then falls to
    // iteration order.
    //
    // The wrong version still returns a legal selection — just not the best one — so nothing
    // downstream fails and nothing reports it. Ratios 1.9 and 1.1 both floor to 1:
    assert!(worth_more(19, 10, 11, 10), "1.9 beats 1.1");
    assert!(!worth_more(11, 10, 19, 10), "and not the other way round");

    // Equal ratios are not "more", so a tie leaves the incumbent standing and the selection
    // stays deterministic.
    assert!(!worth_more(20, 10, 2, 1), "2.0 does not beat 2.0");

    // Denominators are floored at one rather than guarded, so a free cuboid still compares.
    assert!(worth_more(5, 0, 4, 0));
    assert!(worth_more(1, 0, 1, 2), "free beats paid at equal benefit");

    // And the products cannot wrap: u64 benefit times u64 cost is computed in u128.
    assert!(worth_more(u64::MAX, 1, u64::MAX, 2));
    assert!(!worth_more(u64::MAX, 2, u64::MAX, 1));
}
