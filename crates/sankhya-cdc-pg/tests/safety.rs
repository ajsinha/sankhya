//! Source-safety tests: INV-2.
//!
//! > SANKHYA can never bloat, wedge or exhaust the storage of the database it
//! > replicates from.
//!
//! These are pure, so every rung of the ladder is exercised without waiting for a real
//! stall — which is important, because the situations that matter most are precisely
//! the ones nobody wants to reproduce on demand.

use proptest::prelude::*;
use sankhya_cdc_pg::{assess, SafetyPolicy, Severity, SlotState, WalStatus};
use sankhya_types::Lsn;
use std::time::Duration;

const GB: u64 = 1024 * 1024 * 1024;

fn slot(retained: u64, status: WalStatus) -> SlotState {
    SlotState {
        name: "sankhya".into(),
        active: true,
        status,
        retained_bytes: retained,
        source_position: Lsn::new(retained),
        confirmed_position: Lsn::ZERO,
    }
}

#[test]
fn a_healthy_slot_needs_no_action() {
    let policy = SafetyPolicy::default();
    let escalation = assess(
        &policy,
        &slot(GB, WalStatus::Reserved),
        Duration::from_secs(1),
    );
    assert_eq!(escalation.severity, Severity::Normal);
    assert!(!escalation.defer_maintenance);
    assert!(escalation.severity.admits_queries());
    assert!(!escalation.severity.pages());
}

#[test]
fn the_ladder_climbs_in_order_as_retention_grows() {
    let policy = SafetyPolicy::default();
    let expected = [
        (1 * GB, Severity::Normal),
        (3 * GB, Severity::Watch),
        (4 * GB, Severity::Constrain),
        (6 * GB, Severity::Protect),
        (7 * GB, Severity::Sacrifice),
    ];
    for (retained, severity) in expected {
        let escalation = assess(
            &policy,
            &slot(retained, WalStatus::Reserved),
            Duration::ZERO,
        );
        assert_eq!(
            escalation.severity,
            severity,
            "at {} GiB retained, expected {severity} but got {}",
            retained / GB,
            escalation.severity
        );
    }
}

#[test]
fn sankhya_acts_before_the_source_does() {
    // The whole point of the ordering. If the source removes the log first, the slot is
    // destroyed and recovery is a full re-snapshot; if SANKHYA abandons it first, the
    // gap is deliberate, recorded, and bounded.
    let policy = SafetyPolicy::default();
    assert!(
        policy.is_well_ordered(),
        "the default policy must escalate strictly below the source's own limit"
    );
    assert!(
        policy.sacrifice_fraction_percent < 100,
        "sacrificing at or above the source's limit would let the source act first"
    );
}

#[test]
fn an_ill_ordered_policy_is_detectable() {
    // A policy that cannot protect the source should be caught at configuration time,
    // not discovered during an incident.
    let broken = SafetyPolicy {
        sacrifice_fraction_percent: 100,
        ..SafetyPolicy::default()
    };
    assert!(!broken.is_well_ordered());

    let inverted = SafetyPolicy {
        watch_fraction_percent: 90,
        constrain_fraction_percent: 40,
        ..SafetyPolicy::default()
    };
    assert!(!inverted.is_well_ordered());
}

#[test]
fn the_sources_own_warning_escalates_regardless_of_bytes() {
    // The source reporting `extended` means it has already begun protecting itself.
    // Trusting our own byte arithmetic over the source's own judgement would be
    // exactly the wrong instinct.
    let policy = SafetyPolicy::default();
    let escalation = assess(&policy, &slot(0, WalStatus::Extended), Duration::ZERO);
    assert_eq!(escalation.severity, Severity::Protect);
    assert!(escalation.refuse_queries);

    let escalation = assess(&policy, &slot(0, WalStatus::Unreserved), Duration::ZERO);
    assert_eq!(escalation.severity, Severity::Sacrifice);
}

#[test]
fn a_lost_slot_is_terminal_and_says_what_recovery_costs() {
    // Observed for real during development: a slot reported `lost` after a bulk load
    // exceeded the retention limit. There is nothing to salvage.
    let policy = SafetyPolicy::default();
    let escalation = assess(&policy, &slot(0, WalStatus::Lost), Duration::ZERO);
    assert_eq!(escalation.severity, Severity::Sacrifice);
    assert!(escalation.sacrifice_analytics);
    assert!(
        escalation.reason.contains("re-snapshotted"),
        "the operator must be told what recovery requires: {}",
        escalation.reason
    );
    assert!(!WalStatus::Lost.is_usable());
}

#[test]
fn lag_alone_escalates_even_when_volume_is_low() {
    // A quiet database can still have a stalled consumer. Waiting for bytes to
    // accumulate before noticing would delay the diagnosis by however long it takes
    // the workload to produce them.
    let policy = SafetyPolicy::default();
    let escalation = assess(
        &policy,
        &slot(0, WalStatus::Reserved),
        Duration::from_secs(120),
    );
    assert_eq!(escalation.severity, Severity::Watch);

    let escalation = assess(
        &policy,
        &slot(0, WalStatus::Reserved),
        Duration::from_secs(600),
    );
    assert_eq!(escalation.severity, Severity::Constrain);
}

#[test]
fn queries_are_refused_before_the_analytical_tier_is_sacrificed() {
    // Ordering matters: shedding load might let capture drain and avoid the sacrifice
    // entirely. Sacrificing first would discard data that did not need discarding.
    let policy = SafetyPolicy::default();
    let protect = assess(&policy, &slot(6 * GB, WalStatus::Reserved), Duration::ZERO);
    assert!(protect.refuse_queries);
    assert!(
        !protect.sacrifice_analytics,
        "refusing work must be tried before discarding data"
    );
}

#[test]
fn escalation_pages_only_when_a_human_is_needed() {
    assert!(!Severity::Normal.pages());
    assert!(!Severity::Watch.pages());
    assert!(!Severity::Constrain.pages());
    assert!(Severity::Protect.pages());
    assert!(Severity::Sacrifice.pages());
}

#[test]
fn unknown_status_text_is_treated_as_the_worst_case() {
    // A status we do not recognise may mean the source has a state we have not seen.
    // Assuming the best would be the one interpretation that could lose the slot.
    assert_eq!(WalStatus::parse("something-new"), WalStatus::Lost);
    assert_eq!(WalStatus::parse(""), WalStatus::Lost);
    assert_eq!(WalStatus::parse("reserved"), WalStatus::Reserved);
}

#[test]
fn slot_lag_arithmetic_never_underflows() {
    // A confirmed position ahead of the source is nonsensical but observable during a
    // failover. It must not wrap into an enormous apparent lag.
    let state = SlotState {
        name: "s".into(),
        active: true,
        status: WalStatus::Reserved,
        retained_bytes: 0,
        source_position: Lsn::new(100),
        confirmed_position: Lsn::new(200),
    };
    assert_eq!(state.behind_by(), 0);
    assert!(state.is_current());
}

proptest! {
    /// Severity is monotonic in retained volume: more log held can never mean less
    /// concern.
    #[test]
    fn severity_never_decreases_as_retention_grows(a in 0u64..16, b in 0u64..16) {
        prop_assume!(a <= b);
        let policy = SafetyPolicy::default();
        let low = assess(&policy, &slot(a * GB, WalStatus::Reserved), Duration::ZERO).severity;
        let high = assess(&policy, &slot(b * GB, WalStatus::Reserved), Duration::ZERO).severity;
        prop_assert!(high >= low, "{a} GiB gave {low}, {b} GiB gave {high}");
    }

    /// Severity is monotonic in lag too.
    #[test]
    fn severity_never_decreases_as_lag_grows(a in 0u64..1200, b in 0u64..1200) {
        prop_assume!(a <= b);
        let policy = SafetyPolicy::default();
        let low = assess(&policy, &slot(0, WalStatus::Reserved), Duration::from_secs(a)).severity;
        let high = assess(&policy, &slot(0, WalStatus::Reserved), Duration::from_secs(b)).severity;
        prop_assert!(high >= low);
    }

    /// Any assessment at or beyond the sacrifice threshold sacrifices, always.
    ///
    /// This is INV-2 stated as a property: there is no combination of inputs that
    /// leaves the source unprotected once its limit is in sight.
    #[test]
    fn the_source_is_always_protected_at_the_threshold(retained in 0u64..32, secs in 0u64..3600) {
        let policy = SafetyPolicy::default();
        let bytes = retained * GB;
        let escalation = assess(&policy, &slot(bytes, WalStatus::Reserved), Duration::from_secs(secs));
        let threshold = policy.retention_limit_bytes / 100 * u64::from(policy.sacrifice_fraction_percent);
        if bytes >= threshold {
            prop_assert!(
                escalation.sacrifice_analytics,
                "at {bytes} bytes (threshold {threshold}) the source must be protected"
            );
        }
    }

    /// Assessment never panics.
    #[test]
    fn assessment_never_panics(retained in any::<u64>(), secs in any::<u64>(), limit in any::<u64>()) {
        let policy = SafetyPolicy { retention_limit_bytes: limit, ..SafetyPolicy::default() };
        for status in [WalStatus::Reserved, WalStatus::Extended, WalStatus::Unreserved, WalStatus::Lost] {
            let _ = assess(&policy, &slot(retained, status), Duration::from_secs(secs));
        }
    }
}
