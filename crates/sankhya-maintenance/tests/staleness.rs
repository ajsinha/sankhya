//! When a materialised cuboid may still be used, and when a refresh is due.
//!
//! `target_lag` is a **staleness target, not a schedule** — Snowflake's framing for dynamic
//! tables, adopted in [ADR-0009](../../../docs/adr/0009-the-cube-lifecycle.md) because a
//! schedule is wrong in both directions: it refreshes when nothing has changed, and fails to
//! refresh when a build outlasts its interval.
//!
//! What makes the target checkable here rather than estimated is that a cuboid is keyed by
//! the snapshot it was computed at. Staleness is the distance from the table's current
//! version — an integer, known without reading a clock.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_maintenance::cuboid::{lag, refresh_due, unmeetable, within_target};

#[test]
fn staleness_is_the_distance_from_the_table() {
    assert_eq!(lag(10, 10), 0, "computed at the current version");
    assert_eq!(lag(10, 13), 3, "three commits have landed since");
}

#[test]
fn a_cuboid_ahead_of_its_table_is_not_negative() {
    // The table was rewound, or the cuboid was written against a version since rolled back.
    // Saturating rather than wrapping: an underflow here would report a lag of eighteen
    // quintillion and refresh everything forever.
    assert_eq!(lag(20, 10), 0);
}

#[test]
fn a_cube_within_its_target_is_served_from_its_cuboid() {
    assert!(within_target(Some(5), 0), "current");
    assert!(within_target(Some(5), 5), "at the target, which is within it");
    assert!(!within_target(Some(5), 6), "one past");
}

#[test]
fn a_zero_target_admits_only_the_current_version() {
    // The strictest maintained cube: a cuboid at any older version is refused.
    assert!(within_target(Some(0), 0));
    assert!(!within_target(Some(0), 1));
}

#[test]
fn a_cube_that_materialises_nothing_is_never_fresh() {
    // A Declared cube has nothing to be fresh. Answering `true` would make it look Maintained
    // to every caller that asks, and a caller that believes it will serve cells that do not
    // exist.
    assert!(!within_target(None, 0));
    assert!(!within_target(None, 1_000));
}

#[test]
fn a_cube_that_materialises_nothing_is_never_due_a_refresh() {
    // The other half, and it must agree. Refreshing a cube with no target would build
    // cuboids nobody declared and charge an operator storage they did not ask for.
    assert!(!refresh_due(None, 0));
    assert!(!refresh_due(None, 1_000));
}

#[test]
fn due_and_within_target_are_exact_complements_for_a_maintained_cube() {
    // Two functions answering one question in opposite directions. If they ever disagree,
    // a cuboid is both served and rebuilt, or neither — and the second is a cube that
    // silently stops updating.
    for target in [0_u64, 1, 5, 100] {
        for lag in 0_u64..200 {
            assert_ne!(
                within_target(Some(target), lag),
                refresh_due(Some(target), lag),
                "target {target}, lag {lag}"
            );
        }
    }
}

#[test]
fn a_target_a_refresh_cannot_meet_is_reported() {
    // If the table advances further during a build than the target allows, the cuboid is
    // stale before it is written. The system still answers correctly — that is what the
    // fallback is for — while spending storage and maintenance time on a cache that can
    // never be used. Silence there turns an SLA into a decoration.
    let complaint = unmeetable(Some(2), 7).expect("unmeetable");
    assert!(complaint.contains('7') && complaint.contains('2'), "{complaint}");
    assert!(
        complaint.contains("fall back to live aggregation"),
        "it says what will happen: {complaint}"
    );
    assert!(
        complaint.contains("raise the target") || complaint.contains("reduce what is"),
        "and names the thing to change: {complaint}"
    );
}

#[test]
fn a_target_a_refresh_can_meet_is_not_reported() {
    // A standing alarm that fires when nothing is wrong is an alarm nobody reads.
    assert!(unmeetable(Some(5), 5).is_none(), "exactly at the target");
    assert!(unmeetable(Some(5), 1).is_none());
    assert!(unmeetable(None, 1_000).is_none(), "nothing is materialised");
}
