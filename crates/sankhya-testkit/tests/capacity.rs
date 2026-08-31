//! Whether a machine can host a throughput measurement, and what it does when it cannot.
//!
//! # Why this decision is worth a test of its own
//!
//! The three concurrency criteria in `ADR-0013` are measurements taken against a control
//! serialized through one mutex in the same run. That control answers *"is this path
//! serialized?"* and cannot answer *"could anything have scaled here?"* — and `check-all` runs
//! `cargo test --workspace`, which is dozens of test binaries holding every core.
//!
//! Two versions of this guard have now been wrong. The first compared how a workload that
//! shares nothing scaled on one thread against `n`, which reports near-linear scaling however
//! busy the machine is, because fair scheduling gives every runnable thread an equal share. The
//! second sampled free capacity once, before the arms ran, and missed a machine that became
//! busy *during* them — which is how C3 failed on 2026-08-31 with the first fix already in
//! place.
//!
//! The sampling itself cannot be driven from a test without the test becoming the load it is
//! measuring, which is the problem the mechanism exists to solve. So it is verified by hand
//! under an oversubscribed machine, and the part with a rule in it is verified here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_testkit::capacity::enough;

#[test]
fn a_machine_with_the_cores_to_spare_may_measure() {
    assert!(enough(Some(8.0), 8));
    assert!(enough(Some(23.4), 8));
}

#[test]
fn a_machine_one_core_short_may_not() {
    // The comparison is `>=` rather than `>`: needing eight and having exactly eight is enough,
    // and a boundary that went the other way would refuse the quiet machine this is meant to
    // permit.
    assert!(!enough(Some(7.9), 8));
    assert!(!enough(Some(0.0), 8));
}

#[test]
fn a_platform_that_cannot_be_asked_lets_the_measurement_speak_for_itself() {
    // `None` is not a reason to skip. Refusing everywhere free capacity is unreadable would
    // silently stop measuring the criteria on every platform that does not publish it, which is
    // worse than the flakiness being fixed — and it is how every platform behaved before this
    // guard existed.
    assert!(enough(None, 8));
    assert!(enough(None, 1_000));
}

#[test]
fn a_measurement_needing_nothing_is_always_permitted() {
    assert!(enough(Some(0.0), 0));
}
