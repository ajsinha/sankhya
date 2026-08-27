//! The brake, and where it is set.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_governor::{assess_memory, BrakeLimits, Pressure};

const MIB: usize = 1024 * 1024;

fn limits() -> BrakeLimits {
    BrakeLimits {
        warn_bytes: 700 * MIB,
        shed_bytes: 850 * MIB,
    }
}

#[test]
fn a_quiet_machine_is_clear() {
    assert_eq!(assess_memory(100 * MIB, &limits()), Pressure::Clear);
    assert!(!assess_memory(100 * MIB, &limits()).shedding());
}

#[test]
fn approaching_the_limit_warns_without_refusing_anything() {
    // There is a difference between "this is going badly" and "stop". Collapsing them
    // means either warning too late to act on or shedding work that would have finished.
    let p = assess_memory(750 * MIB, &limits());
    assert!(matches!(p, Pressure::Warning { .. }));
    assert!(!p.shedding());
}

#[test]
fn passing_the_limit_sheds_and_explains_why() {
    let p = assess_memory(900 * MIB, &limits());
    assert!(p.shedding());
    assert!(format!("{p}").contains("takes every other query with it"));
}

#[test]
fn the_boundaries_are_inclusive() {
    // At exactly the threshold the limit has been reached. Excluding it would leave the
    // brake firing one byte later than configured, which is a limit nobody set.
    assert!(matches!(
        assess_memory(700 * MIB, &limits()),
        Pressure::Warning { .. }
    ));
    assert!(assess_memory(850 * MIB, &limits()).shedding());
}

#[test]
fn shedding_outranks_warning() {
    // Both thresholds are passed. Reporting the warning would describe a machine about
    // to be killed as one that is merely busy.
    let p = assess_memory(usize::MAX, &limits());
    assert!(p.shedding());
}

#[test]
fn the_brake_is_expected_to_sit_below_the_machines_real_limit() {
    // Not a property of the code -- a property of any sane configuration, asserted so
    // that a configuration which is not sane is a failing test rather than an outage.
    //
    // By the time the operating system is involved there is no decision left to make.
    // Shedding a query is a choice; being killed is not.
    let machine = 1024 * MIB;
    let limits = limits();
    assert!(
        limits.shed_bytes < machine,
        "the brake fires at or above the machine's capacity, which means it never fires"
    );
    assert!(
        limits.warn_bytes < limits.shed_bytes,
        "the warning is not before the brake"
    );
}
