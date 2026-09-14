//! What a cube says it reads.
//!
//! `reads` is not a convenience list. It is what a principal must be allowed to read before
//! the cube is registered for them, what the snapshot is taken across, and part of the
//! fingerprint that decides whether a materialised cuboid still answers for this definition.
//! A table missing from it is a table outside all three.
//!
//! It held only the **fact table** until `M24b`. That was right while nothing opened a
//! dimension table; `M23` made hydration open them, and `GUIDE.md` had been saying for months
//! that *"a cube named in a `CREATE` whose fact table or dimension tables you cannot read is
//! refused"* — a claim the code did not make good.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_cube::algo::{Along, Measure, Rule};
use sankhya_cube::{Definition, Dimension, Level};

fn measure() -> Measure {
    Measure::new("amount", vec![
        Along::new("region", Rule::Sum),
        Along::new("period", Rule::Sum),
    ])
}

fn over(region_table: &str, period_table: &str) -> Definition {
    Definition::new(
        "sales",
        "orders",
        vec![
            Dimension::new("region", region_table, "region_key", vec![Level::new("r", "r")]),
            Dimension::new("period", period_table, "period_key", vec![Level::new("p", "p")]),
        ],
        vec![measure()],
    )
}

#[test]
fn a_cube_reads_its_dimension_tables_as_well_as_its_facts() {
    let cube = over("sales.regions", "sales.periods").validate().expect("well-formed");
    assert_eq!(
        cube.reads(),
        ["orders", "sales.regions", "sales.periods"],
        "the facts first, then the dimensions in declaration order"
    );
}

#[test]
fn a_dimension_over_the_fact_table_is_not_listed_twice() {
    // `DIMENSION region FROM orders ON region` is an ordinary thing to declare — the members
    // are the distinct values of a fact column. Listed twice, that table's scope folds into
    // the authorization digest twice, and the digest is what decides whether two principals
    // share a cache entry.
    let cube = over("orders", "orders").validate().expect("well-formed");
    assert_eq!(cube.reads(), ["orders"]);
}

#[test]
fn the_fingerprint_changes_when_a_dimension_moves_to_another_table() {
    // `reads` feeds the definition version, which keys every materialised cuboid. A cube whose
    // members now come from somewhere else is a different cube, and a cuboid built from the
    // old one must miss rather than answer.
    let here = over("sales.regions", "orders").validate().expect("well-formed");
    let there = over("reference.regions", "orders").validate().expect("well-formed");
    assert_ne!(here.version(), there.version());
}

#[test]
fn a_cube_over_a_query_reads_its_dimension_tables_too() {
    // The query's own tables are resolved by the caller, which cannot see the dimensions:
    // a cube over `(SELECT … FROM orders)` joined to `sales.regions` opens `sales.regions` at
    // hydration whether or not any `FROM` clause mentions it.
    let cube = over("sales.regions", "orders")
        .over_query("(SELECT * FROM orders)", vec!["sales.orders".to_string()])
        .validate()
        .expect("well-formed");
    assert_eq!(cube.reads(), ["sales.orders", "sales.regions", "orders"]);
}

#[test]
fn a_dimension_moved_after_the_definition_was_built_is_still_read() {
    // **The list is derived at `validate`, not cached at `new`.** Every fixture in this
    // repository builds a definition and then edits its dimensions, and `into_definition`
    // restores a stored `reads` over whatever was computed --- so a list fixed at
    // construction is a list that goes quietly stale, naming the table a dimension used to
    // sit on. That is a cube authorized against the wrong table, which is worse than one
    // authorized against too few.
    let mut definition = over("orders", "orders");
    definition.dimensions[0].table = "reference.regions".to_string();
    let cube = definition.validate().expect("well-formed");
    assert_eq!(cube.reads(), ["orders", "reference.regions"]);
}

#[test]
fn a_catalogue_written_before_this_gains_the_dimension_tables_on_load() {
    // A stored definition carries the `reads` that was current when it was saved, and
    // `into_definition` restores it --- so every cube already on disk names only its fact
    // table. Folding the dimensions in at `validate` is what repairs them, without a
    // migration and without a format change.
    let cube = over("sales.regions", "orders").validate().expect("well-formed");
    let stored = sankhya_cube::catalogue::Stored::of(cube.definition());
    let reloaded = stored.into_definition().expect("a usable cube").validate().expect("valid");
    assert_eq!(reloaded.reads(), ["orders", "sales.regions"]);
}
