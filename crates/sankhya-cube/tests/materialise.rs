//! Where an answer comes from — and the criterion that it must not matter.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use proptest::prelude::*;
use sankhya_cube::cells::Cells;
use sankhya_cube::materialise::{plan, Key, Policy, Session};
use sankhya_cube::navigate::{roll_up, Ordered};
use sankhya_cube_algo::lattice::Cuboid;
use sankhya_cube_algo::measure::{Along, Measure, Rule};

fn amount() -> Measure {
    Measure::new("amount", vec![
        Along::new("period", Rule::Sum),
        Along::new("entity", Rule::Sum),
        Along::new("product", Rule::Sum),
    ])
}

/// Composes along nothing: no ancestor can serve any coarser query.
fn distinct() -> Measure {
    Measure::new("distinct", vec![
        Along::new("period", Rule::None),
        Along::new("entity", Rule::None),
        Along::new("product", Rule::None),
    ])
}

fn base() -> Cuboid {
    Cuboid::of(&["period", "entity", "product"])
}

// --- the key that cannot go stale ---------------------------------------

#[test]
fn a_new_snapshot_is_a_different_key_and_therefore_a_miss() {
    // FR-QUERY-20's whole invalidation story: files are immutable and every key embeds the
    // snapshot, so a new commit misses rather than hitting something stale.
    let cuboid = Cuboid::of(&["entity"]);
    let before = Key::unrestricted(0xabc, 100, cuboid.clone());
    let after = Key::unrestricted(0xabc, 101, cuboid);
    assert_ne!(before.table("figures"), after.table("figures"));
}

#[test]
fn a_new_definition_is_a_different_key_too() {
    // The half that has no log to derive it from — see `crate::version`.
    let cuboid = Cuboid::of(&["entity"]);
    assert_ne!(
        Key::unrestricted(1, 100, cuboid.clone()).table("figures"),
        Key::unrestricted(2, 100, cuboid).table("figures")
    );
}

#[test]
fn two_cuboids_cannot_share_a_table_by_splitting_a_name_differently() {
    // `a_b` + `c` against `a` + `b_c`: joined on a separator alone these render
    // identically, and the two cuboids then share storage — one cube's totals served for
    // another's query. A count of dimensions does not help; both have two.
    let one = Key::unrestricted(1, 1, Cuboid::of(&["a_b", "c"]));
    let two = Key::unrestricted(1, 1, Cuboid::of(&["a", "b_c"]));
    assert_ne!(one.table("figures"), two.table("figures"));

    // And the same trap on the cube name, which is prefixed for the same reason.
    assert_ne!(
        Key::unrestricted(1, 1, Cuboid::of(&["x"])).table("a_b"),
        Key::unrestricted(1, 1, Cuboid::of(&["b", "x"])).table("a")
    );
}

#[test]
fn the_same_key_always_renders_the_same_table() {
    let key = Key::unrestricted(7, 9, Cuboid::of(&["entity", "period"]));
    assert_eq!(key.table("figures"), key.table("figures"));
    assert_ne!(key.table("figures"), key.table("totals"), "and cubes do not share");
}

// --- the session level only narrows -------------------------------------

#[test]
fn a_session_can_ask_for_less_materialisation_and_never_for_more() {
    // A session that could raise the budget would be an unbounded storage grant to anybody
    // who can open one. The asymmetry is the point of having an operator level at all.
    let pinned = Cuboid::of(&["entity"]);
    let selected = Cuboid::of(&["period"]);
    let policy = Policy::new([pinned.clone()], 10_000);
    let available = vec![pinned.clone(), selected.clone()];

    assert_eq!(policy.usable(&available, Session::AsConfigured).len(), 2);
    assert_eq!(policy.usable(&available, Session::PinnedOnly), vec![&pinned]);
    assert!(policy.usable(&available, Session::Off).is_empty());

    // There is no session value that widens: the budget is reachable only from the policy.
    assert_eq!(policy.budget_rows(), 10_000);
}

#[test]
fn a_pinned_cuboid_that_is_not_yet_built_is_not_usable() {
    // Pinning states intent. Treating it as a promise about this instant would have the
    // planner read a table that does not exist.
    let policy = Policy::new([Cuboid::of(&["entity"])], 10_000);
    assert!(policy.usable(&[], Session::AsConfigured).is_empty());
    assert!(policy.usable(&[], Session::PinnedOnly).is_empty());
}

// --- planning -----------------------------------------------------------

#[test]
fn the_narrowest_usable_ancestor_is_chosen() {
    let wide = Cuboid::of(&["period", "entity"]);
    let narrow = Cuboid::of(&["entity"]);
    let chosen = plan(&Cuboid::of(&["entity"]), &amount(), &[&wide, &narrow], &base());

    assert!(chosen.materialised);
    assert_eq!(chosen.from, narrow);
    assert!(chosen.rolling_away.is_empty(), "the cuboid *is* the query");
}

#[test]
fn a_query_needing_a_dimension_no_cuboid_holds_falls_back_to_the_base() {
    let cuboid = Cuboid::of(&["entity"]);
    let chosen = plan(&Cuboid::of(&["product"]), &amount(), &[&cuboid], &base());
    assert!(!chosen.materialised);
    assert_eq!(chosen.from, base());
}

#[test]
fn no_ancestor_is_used_for_a_measure_that_composes_along_nothing() {
    // Skipping the additivity test is how materialisation starts changing answers, and the
    // change is invisible: the number is real, just computed from partials that do not
    // compose.
    let cuboid = Cuboid::of(&["period", "entity"]);
    let chosen = plan(&Cuboid::of(&["entity"]), &distinct(), &[&cuboid], &base());
    assert!(!chosen.materialised, "a distinct count was rolled up from an ancestor");
}

#[test]
fn a_cuboid_that_is_exactly_the_query_serves_a_non_composing_measure() {
    // Rolling away nothing is always permitted, so a materialised cuboid serves the one
    // query it is — which is the only thing worth materialising for a distinct count.
    let cuboid = Cuboid::of(&["entity"]);
    let chosen = plan(&Cuboid::of(&["entity"]), &distinct(), &[&cuboid], &base());
    assert!(chosen.materialised);
}

#[test]
fn the_plan_says_what_it_rolls_away() {
    // "Why was this fast?" and "why was this slow?" are the same question, and neither is
    // answerable from a plan that only names a table.
    let cuboid = Cuboid::of(&["period", "entity"]);
    let chosen = plan(&Cuboid::of(&["entity"]), &amount(), &[&cuboid], &base());
    assert_eq!(chosen.rolling_away, ["period"]);
    assert!(chosen.to_string().contains("rolling away period"), "{chosen}");
}

#[test]
fn the_plan_is_stable_across_runs() {
    // Two cuboids of equal width must not be chosen by iteration order, or a plan differs
    // between runs and every latency comparison is noise.
    let a = Cuboid::of(&["entity"]);
    let b = Cuboid::of(&["period"]);
    let query = Cuboid::of::<&str>(&[]);
    let forwards = plan(&query, &amount(), &[&a, &b], &base());
    let backwards = plan(&query, &amount(), &[&b, &a], &base());
    assert_eq!(forwards, backwards);
}

// --- exit criterion 3a --------------------------------------------------

/// A cube of facts over (period, entity, product).
fn facts(values: &[(&str, &str, &str, f64)]) -> Cells {
    let mut cells = Cells::over(vec![
        "period".to_string(),
        "entity".to_string(),
        "product".to_string(),
    ]);
    for (period, entity, product, value) in values {
        cells
            .add(
                vec![
                    (*period).to_string(),
                    (*entity).to_string(),
                    (*product).to_string(),
                ],
                *value,
            )
            .expect("well-formed");
    }
    cells
}

#[test]
fn an_answer_from_an_ancestor_equals_the_answer_from_the_base() {
    // M7's exit criterion 3a. A cache that changes results is not a cache, and the change
    // would be invisible — both figures are real numbers over real rows.
    let cells = facts(&[
        ("jan", "a", "x", 1.5),
        ("jan", "a", "y", 2.25),
        ("jan", "b", "x", 4.0),
        ("feb", "a", "x", 8.125),
        ("feb", "b", "y", 16.0),
    ]);

    // From the base: roll away product, then period.
    let direct = roll_up(&cells, "product", &amount(), Ordered::Unstated).expect("additive");
    let direct = roll_up(&direct, "period", &amount(), Ordered::Unstated).expect("additive");

    // From a materialised ancestor that already rolled product away.
    let ancestor = roll_up(&cells, "product", &amount(), Ordered::Unstated).expect("additive");
    let from_ancestor =
        roll_up(&ancestor, "period", &amount(), Ordered::Unstated).expect("additive");

    assert_eq!(direct.dimensions(), from_ancestor.dimensions());
    for address in direct.addresses() {
        let a = direct.get(address, Rule::Sum).expect("present");
        let b = from_ancestor.get(address, Rule::Sum).expect("present");
        assert_eq!(a.to_bits(), b.to_bits(), "bit-identical at {address:?}");
    }
}

proptest! {
    /// Rolling two dimensions away in either order gives bit-identical answers.
    ///
    /// Which is what makes an ancestor safe to answer from: the materialised cuboid is
    /// exactly "one of these roll-ups, done earlier".
    #[test]
    fn materialisation_order_does_not_change_the_answer(
        values in prop::collection::vec(
            (0usize..3, 0usize..3, 0usize..3, -1e9f64..1e9),
            1..40
        )
    ) {
        const NAMES: [&str; 3] = ["p", "q", "r"];
        let rows: Vec<(&str, &str, &str, f64)> = values
            .iter()
            .map(|(a, b, c, v)| (NAMES[*a], NAMES[*b], NAMES[*c], *v))
            .collect();
        let cells = facts(&rows);

        let product_first = roll_up(&cells, "product", &amount(), Ordered::Unstated).unwrap();
        let product_first = roll_up(&product_first, "period", &amount(), Ordered::Unstated).unwrap();

        let period_first = roll_up(&cells, "period", &amount(), Ordered::Unstated).unwrap();
        let period_first = roll_up(&period_first, "product", &amount(), Ordered::Unstated).unwrap();

        prop_assert_eq!(product_first.len(), period_first.len());
        for address in product_first.addresses() {
            let a = product_first.get(address, Rule::Sum).expect("present");
            let b = period_first.get(address, Rule::Sum).expect("present");
            prop_assert_eq!(a.to_bits(), b.to_bits(), "at {:?}", address);
        }
    }
}

// --- the scope is part of where a cuboid lives ---------------------------

#[test]
fn two_scopes_are_two_tables() {
    // The strongest form the separation can take. A materialised cuboid is a published
    // table, so two scopes are two files with two names — and a bug in the lookup logic
    // cannot serve one scope's rows to another, because the rows are not in the file being
    // read.
    let cuboid = Cuboid::of(&["region"]);
    let restricted = Key::new(1, 100, 0xdead_beef, cuboid.clone());
    let unrestricted = Key::unrestricted(1, 100, cuboid);

    assert_ne!(
        restricted.table("figures"),
        unrestricted.table("figures"),
        "a cuboid computed over one principal's rows must not be stored where another's \
         query will find it"
    );
}

#[test]
fn the_unrestricted_scope_is_named_rather_than_written_as_zero() {
    // A caller reaching for this is asserting the cells behind it were computed over every
    // row. That assertion should be legible at the call site rather than being a bare 0.
    let cuboid = Cuboid::of(&["region"]);
    assert_eq!(
        Key::unrestricted(1, 100, cuboid.clone()),
        Key::new(1, 100, 0, cuboid)
    );
}

#[test]
fn a_scope_cannot_be_confused_with_a_snapshot() {
    // Both are fixed-width hex in the rendered name, so a cuboid at snapshot 5 under scope 9
    // must not render like one at snapshot 9 under scope 5. Fixed width is what prevents it,
    // and the same reasoning as the length prefixes on dimensions.
    let cuboid = Cuboid::of(&["region"]);
    assert_ne!(
        Key::new(1, 5, 9, cuboid.clone()).table("figures"),
        Key::new(1, 9, 5, cuboid).table("figures")
    );
}

// --- a name that can be read back --------------------------------------------

#[test]
fn a_rendered_name_reads_back_as_the_key_that_wrote_it() {
    // Reversibility is what lets a sweep decide whether a cuboid on disk is still worth
    // keeping, without a side table recording what the name already says — and a side table
    // is one more thing that can disagree with the files.
    let key = Key::new(0xdead_beef, 42, 0x0bad_f00d, Cuboid::of(&["region", "period"]));
    let rendered = key.table("figures");

    let (cube, read) = sankhya_cube::materialise::parse(&rendered).expect("ours");
    assert_eq!(cube, "figures");
    assert_eq!(read, key);
}

#[test]
fn a_name_with_awkward_parts_still_reads_back() {
    // The length prefixes exist because `a_b` and `c` must not render like `a` and `b_c`.
    // The same prefixes are what make the rendering reversible, so the awkward cases are
    // exactly the ones worth checking.
    for (cube, dimensions) in [
        ("a_b", vec!["c"]),
        ("a", vec!["b_c"]),
        ("has_many_underscores", vec!["x_y", "z"]),
        ("", vec!["region"]),
    ] {
        let key = Key::new(1, 2, 3, Cuboid::of(&dimensions));
        let rendered = key.table(cube);
        let (read_cube, read_key) =
            sankhya_cube::materialise::parse(&rendered).expect("ours: {rendered}");
        assert_eq!(read_cube, cube, "{rendered}");
        assert_eq!(read_key, key, "{rendered}");
    }
}

#[test]
fn a_directory_that_is_not_ours_does_not_parse() {
    // The important half. Something a sweep cannot parse is something it must not delete,
    // and a warehouse holds directories this code did not write.
    for name in [
        "orders",
        "_delta_log",
        "__cube_",
        "__cube_5_short",
        "__cube_2_ab_notahexnumber_0000000000000000_0000000000000000",
        "__cube_2_ab_0000000000000001_0000000000000002_0000000000000003_trailing",
    ] {
        assert!(
            sankhya_cube::materialise::parse(name).is_none(),
            "`{name}` is not a cuboid and must not be read as one"
        );
    }
}

#[test]
fn a_cuboid_with_no_dimensions_reads_back() {
    // The grand total: one cell, no dimensions. A parser that required at least one would
    // refuse to collect the cuboid most likely to be materialised.
    let key = Key::new(1, 2, 3, Cuboid::of::<&str>(&[]));
    let rendered = key.table("figures");
    let (cube, read) = sankhya_cube::materialise::parse(&rendered).expect("ours");
    assert_eq!(cube, "figures");
    assert_eq!(read, key);
}
