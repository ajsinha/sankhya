//! The cache that makes cubes affordable, and the key that makes it safe.
//!
//! Hydration reads the fact table, which is the cost a cube exists to avoid paying per query,
//! so caching it is the point. It is also a cache of *aggregates*, and an aggregate computed
//! over the rows one principal may read is not an answer for another — so every field of the
//! key is preventing something specific, and a test per field is the only way to know they
//! are all still there.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_cube::cells::Cells;
use sankhya_cube::complete::Completeness;
use sankhya_cube::model::{Definition, Dimension, Level};
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use sankhya_cube_sql::catalog::Published;
use sankhya_cube_sql::hydrated::{Hydrated, Key};
use std::sync::Arc;

fn cube() -> Arc<sankhya_cube::model::Cube> {
    Arc::new(
        Definition::new(
            "figures",
            "facts",
            vec![Dimension {
                name: "region".to_string(),
                table: "dim_region".to_string(),
                joins_on: "region".to_string(),
                levels: vec![Level::new("id", "id")],
                rollups: None,
                parent_child: None,
            }],
            vec![Measure::new("amount", vec![Along::new("region", Rule::Sum)])],
        )
        .validate()
        .expect("a valid cube"),
    )
}

fn published(measure: &str, snapshot: u64) -> Published {
    Published {
        // A fixture builds its own cells, which is the opposite of reading a cuboid.
        from_cuboid: false,
        cube: cube(),
        cells: Arc::new(Cells::over(vec!["region".to_string()])),
        measure: measure.to_string(),
        snapshot,
        completeness: Completeness::complete(1),
    }
}

fn key(cube: &str, measure: &str, version: u64, snapshot: u64, scope: u64) -> Key {
    Key {
        cube: cube.to_string(),
        measure: measure.to_string(),
        definition_version: version,
        snapshot,
        scope,
        // Unpinned, which is what every existing test here means.
        pin: 0,
    }
}

/// The same question asked by a session reading from a position it chose.
fn pinned(cube: &str, measure: &str, version: u64, snapshot: u64, scope: u64, pin: u64) -> Key {
    Key { pin, ..key(cube, measure, version, snapshot, scope) }
}

#[test]
fn cells_are_found_again_under_the_same_key() {
    let cache = Hydrated::default();
    cache.put(key("figures", "amount", 1, 7, 99), published("amount", 7));

    let found = cache
        .get(&key("figures", "amount", 1, 7, 99))
        .expect("the same question has the same answer");
    assert_eq!(found.measure, "amount");
    assert_eq!(cache.counts(), (1, 0));
}

#[test]
fn a_different_scope_is_a_miss() {
    // The one that matters. Two principals with different entitlements see different rows, so
    // an aggregate held for one must not be handed to the other — and unlike a wrong row, a
    // wrong total carries nothing in it to notice.
    let cache = Hydrated::default();
    cache.put(key("figures", "amount", 1, 7, 99), published("amount", 7));

    assert!(
        cache.get(&key("figures", "amount", 1, 7, 100)).is_none(),
        "a restricted principal must never be served an unrestricted total"
    );
}

#[test]
fn a_new_snapshot_is_a_miss() {
    // FR-QUERY-20: a new commit must not produce a stale hit. Serving the old figure is how a
    // dashboard comes to disagree with the table it is drawn from.
    let cache = Hydrated::default();
    cache.put(key("figures", "amount", 1, 7, 99), published("amount", 7));

    assert!(cache.get(&key("figures", "amount", 1, 8, 99)).is_none());
}

#[test]
fn an_edited_definition_is_a_miss() {
    // The fingerprint changes when the cube does. Hitting the old cells would answer today's
    // question with yesterday's shape — a measure that was removed, a hierarchy that changed.
    let cache = Hydrated::default();
    cache.put(key("figures", "amount", 1, 7, 99), published("amount", 7));

    assert!(cache.get(&key("figures", "amount", 2, 7, 99)).is_none());
}

#[test]
fn a_different_measure_is_a_miss() {
    // Cells hold one measure's values. A hit across measures would apply one measure's rule
    // to another's numbers, which is the defect this crate already fixed once.
    let cache = Hydrated::default();
    cache.put(key("figures", "amount", 1, 7, 99), published("amount", 7));

    assert!(cache.get(&key("figures", "closing_balance", 1, 7, 99)).is_none());
}

#[test]
fn the_cache_is_bounded() {
    // An unbounded cache of cube cells is a memory leak with a business justification.
    let cache = Hydrated::with_capacity(2);
    for scope in 0..5 {
        cache.put(key("figures", "amount", 1, 7, scope), published("amount", 7));
    }
    assert!(
        cache.len() <= 2,
        "held {} sets of cells against a capacity of 2",
        cache.len()
    );
}

#[test]
fn re_putting_a_key_does_not_grow_the_cache() {
    // Rehydrating the same question must replace rather than accumulate, or a cube refreshed
    // on a timer fills the cache with copies of itself and evicts everything else.
    let cache = Hydrated::with_capacity(2);
    for _ in 0..5 {
        cache.put(key("figures", "amount", 1, 7, 99), published("amount", 7));
    }
    assert_eq!(cache.len(), 1);
}

#[test]
fn forgetting_a_cube_leaves_the_others_alone() {
    // A definition that changed must not cost every other cube its hydration.
    let cache = Hydrated::default();
    cache.put(key("figures", "amount", 1, 7, 99), published("amount", 7));
    cache.put(key("other", "amount", 1, 7, 99), published("amount", 7));

    cache.forget("figures");

    assert!(cache.get(&key("figures", "amount", 1, 7, 99)).is_none());
    assert!(
        cache.get(&key("other", "amount", 1, 7, 99)).is_some(),
        "forgetting one cube must not clear the cache"
    );
}

#[test]
fn forgetting_a_cube_forgets_every_scope_of_it() {
    // A changed definition invalidates the cube for everybody, not for whoever happened to
    // hydrate it last.
    let cache = Hydrated::default();
    for scope in 0..3 {
        cache.put(key("figures", "amount", 1, 7, scope), published("amount", 7));
    }
    cache.forget("figures");
    assert!(cache.is_empty());
}

#[test]
fn hits_and_misses_are_countable() {
    // A cache nobody can measure is a cache nobody can size. A hit rate near zero means the
    // key is too specific — a scope digest varying per principal is how that happens — and it
    // is invisible without a counter.
    let cache = Hydrated::default();
    cache.put(key("figures", "amount", 1, 7, 99), published("amount", 7));

    let _ = cache.get(&key("figures", "amount", 1, 7, 99));
    let _ = cache.get(&key("figures", "amount", 1, 7, 99));
    let _ = cache.get(&key("figures", "amount", 1, 7, 12));

    assert_eq!(cache.counts(), (2, 1));
}

#[test]
fn a_pinned_sessions_cells_are_not_served_to_an_unpinned_one() {
    // `COR-20`. The key held the table's *present* version — deliberately, so that a
    // configured `read_as_of` could not freeze the cache for the life of the process — and
    // nothing in it said whether the session that filled it was reading from a position it had
    // chosen. So a pinned session's cells went in under the present version, and the next
    // unpinned session looking up that key was served them.
    //
    // A pinned read's whole promise is that it does not move. It was leaking into reads whose
    // promise is the opposite, and neither session could tell.
    let cache = Hydrated::default();
    cache.put(pinned("figures", "amount", 1, 7, 99, 0xdead_beef), published("amount", 7));

    assert!(
        cache.get(&key("figures", "amount", 1, 7, 99)).is_none(),
        "an unpinned session was served a pinned session's cells"
    );
    assert_eq!(cache.counts(), (0, 1), "and it was recorded as a miss");
}

#[test]
fn two_sessions_pinned_to_the_same_position_share_one_entry() {
    // The control, and the reason the pin is a digest of the settings rather than a session
    // identifier: two sessions that asked for the same position are reading the same thing,
    // and a key that separated them would make every pinned read a cold one.
    let cache = Hydrated::default();
    cache.put(pinned("figures", "amount", 1, 7, 99, 0xdead_beef), published("amount", 7));

    assert!(
        cache.get(&pinned("figures", "amount", 1, 7, 99, 0xdead_beef)).is_some(),
        "two sessions pinned to the same position missed each other"
    );
}

#[test]
fn two_different_pins_are_two_entries() {
    // And the other direction: `SET SNAPSHOT = 'eod'` and `SET SNAPSHOT = 'month_end'` are
    // different positions, so serving one for the other is the same defect one step along.
    let cache = Hydrated::default();
    cache.put(pinned("figures", "amount", 1, 7, 99, 1), published("amount", 7));

    assert!(
        cache.get(&pinned("figures", "amount", 1, 7, 99, 2)).is_none(),
        "one pinned position's cells were served to a session pinned elsewhere"
    );
}
