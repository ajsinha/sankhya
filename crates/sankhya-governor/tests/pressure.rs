//! The ladder, and the rung nothing but the source may reach.

use proptest::prelude::*;
use sankhya_governor::{assess, Level, Signals, Thresholds};

fn quiet() -> Signals {
    Signals::default()
}

#[test]
fn a_quiet_system_is_normal_and_names_nothing() {
    let a = assess(&quiet(), &Thresholds::default());
    assert_eq!(a.level, Level::Normal);
    assert_eq!(a.driver, "none");
    assert!(a.level.admits_queries());
    assert!(a.level.runs_optional_maintenance());
}

#[test]
fn the_ladder_climbs_with_the_retained_log() {
    let t = Thresholds::default();
    for (value, expected) in [
        (0.1, Level::Normal),
        (0.5, Level::Watch),
        (0.7, Level::Constrain),
        (0.85, Level::Protect),
        (0.95, Level::Sacrifice),
        (1.5, Level::Sacrifice),
    ] {
        let a = assess(
            &Signals {
                retained_log: value,
                ..quiet()
            },
            &t,
        );
        assert_eq!(a.level, expected, "at {value}");
        if expected != Level::Normal {
            assert_eq!(a.driver, "retained log");
        }
    }
}

#[test]
fn the_last_rung_belongs_to_the_source_alone() {
    // Sacrificing continuity means a recorded gap and a re-snapshot. It is only ever
    // worth that to save the source, so no amount of any other pressure may reach it.
    let t = Thresholds::default();

    for (label, signals) in [
        (
            "arrival buffer",
            Signals {
                arrival_buffer: 1.0,
                ..quiet()
            },
        ),
        (
            "capture lag",
            Signals {
                lag: 1.0,
                ..quiet()
            },
        ),
        (
            "compaction debt",
            Signals {
                compaction_debt: 1.0,
                ..quiet()
            },
        ),
    ] {
        let a = assess(&signals, &t);
        assert_ne!(a.level, Level::Sacrifice, "{label} reached the last rung");
        assert!(!a.level.loses_continuity());
    }

    // Both source signals can.
    for signals in [
        Signals {
            retained_log: 1.0,
            ..quiet()
        },
        Signals {
            freeze_age: 1.0,
            ..quiet()
        },
    ] {
        assert_eq!(assess(&signals, &t).level, Level::Sacrifice);
    }
}

#[test]
fn compaction_debt_never_stops_a_query() {
    // Compaction debt makes queries slower. Refusing them to fix that is refusing to do
    // the thing in order to do it faster.
    let a = assess(
        &Signals {
            compaction_debt: 3.0,
            ..quiet()
        },
        &Thresholds::default(),
    );
    assert_eq!(a.level, Level::Watch);
    assert!(a.level.admits_queries());
}

#[test]
fn capture_lag_constrains_but_does_not_stop() {
    // Lag is the analytical tier falling behind, which is what the arrival tier and a
    // longer commit interval exist to absorb. It is not a reason to stop serving.
    let a = assess(
        &Signals {
            lag: 5.0,
            ..quiet()
        },
        &Thresholds::default(),
    );
    assert_eq!(a.level, Level::Constrain);
    assert!(a.level.admits_queries());
}

#[test]
fn a_filling_arrival_buffer_can_stop_admission_but_not_continuity() {
    // Publication has stalled and memory is the constraint. Serious enough to stop
    // admitting; not a reason to lose data that is still recoverable.
    let a = assess(
        &Signals {
            arrival_buffer: 0.9,
            ..quiet()
        },
        &Thresholds::default(),
    );
    assert_eq!(a.level, Level::Protect);
    assert!(!a.level.admits_queries());
    assert!(!a.level.loses_continuity());
}

#[test]
fn the_highest_pressure_wins_and_is_named() {
    // The levels describe what the system will refuse to do, and a refusal justified by
    // any one pressure is justified.
    let a = assess(
        &Signals {
            retained_log: 0.86,
            compaction_debt: 0.99,
            lag: 0.75,
            ..quiet()
        },
        &Thresholds::default(),
    );
    assert_eq!(a.level, Level::Protect);
    assert_eq!(a.driver, "retained log");
}

#[test]
fn the_sacrifice_threshold_fires_before_the_source_acts() {
    // The rung exists because the alternative is worse, and it only works if it happens
    // first. At 1.0 the database has already invalidated the slot, the slot cannot be
    // resumed, and every replicated table needs a complete re-snapshot -- the same bad
    // outcome, unbounded and unannounced.
    let t = Thresholds::default();
    assert!(
        t.sacrifice < 1.0,
        "the ladder must act while there is still room"
    );

    let a = assess(
        &Signals {
            retained_log: t.sacrifice,
            ..quiet()
        },
        &t,
    );
    assert_eq!(a.level, Level::Sacrifice);
}

#[test]
fn the_rungs_are_strictly_ordered() {
    assert!(Level::Normal < Level::Watch);
    assert!(Level::Watch < Level::Constrain);
    assert!(Level::Constrain < Level::Protect);
    assert!(Level::Protect < Level::Sacrifice);
}

#[test]
fn paging_starts_where_the_system_stops_serving() {
    // An operator should hear about it at the point users do, not before and not after.
    assert!(!Level::Normal.pages());
    assert!(!Level::Watch.pages());
    assert!(!Level::Constrain.pages());
    assert!(Level::Protect.pages());
    assert!(Level::Sacrifice.pages());
}

proptest! {
    /// The level never falls when a pressure rises.
    ///
    /// A ladder that can be walked *down* by adding pressure has a hole in it, and the
    /// hole would only appear under a combination nobody thought to try.
    #[test]
    fn raising_any_signal_never_lowers_the_level(
        base in (0.0f64..1.2, 0.0f64..1.2, 0.0f64..1.2, 0.0f64..1.2, 0.0f64..1.2),
        bump in 0.0f64..0.5,
        which in 0usize..5,
    ) {
        let signals = Signals {
            retained_log: base.0,
            freeze_age: base.1,
            arrival_buffer: base.2,
            lag: base.3,
            compaction_debt: base.4,
        };
        let before = assess(&signals, &Thresholds::default()).level;

        let mut raised = signals;
        match which {
            0 => raised.retained_log += bump,
            1 => raised.freeze_age += bump,
            2 => raised.arrival_buffer += bump,
            3 => raised.lag += bump,
            _ => raised.compaction_debt += bump,
        }
        let after = assess(&raised, &Thresholds::default()).level;

        prop_assert!(after >= before, "{before} became {after}");
    }

    /// Nothing but a source signal ever loses continuity.
    #[test]
    fn continuity_is_only_ever_lost_to_save_the_source(
        arrival in 0.0f64..5.0,
        lag in 0.0f64..5.0,
        debt in 0.0f64..5.0,
    ) {
        let a = assess(
            &Signals {
                arrival_buffer: arrival,
                lag,
                compaction_debt: debt,
                ..Signals::default()
            },
            &Thresholds::default(),
        );
        prop_assert!(!a.level.loses_continuity());
    }

    /// The named driver is a signal that actually reached the reported level.
    #[test]
    fn the_driver_explains_the_level(
        signals in (0.0f64..1.5, 0.0f64..1.5, 0.0f64..1.5, 0.0f64..1.5, 0.0f64..1.5),
    ) {
        let s = Signals {
            retained_log: signals.0,
            freeze_age: signals.1,
            arrival_buffer: signals.2,
            lag: signals.3,
            compaction_debt: signals.4,
        };
        let a = assess(&s, &Thresholds::default());

        if a.level == Level::Normal {
            prop_assert_eq!(a.driver, "none");
        } else {
            prop_assert_ne!(a.driver, "none");
            // The reported value must be the signal the driver names.
            let named = match a.driver {
                "retained log" => s.retained_log,
                "freeze age" => s.freeze_age,
                "arrival buffer" => s.arrival_buffer,
                "capture lag" => s.lag,
                "compaction debt" => s.compaction_debt,
                other => panic!("unknown driver {other}"),
            };
            prop_assert_eq!(a.value, named);
        }
    }
}

#[test]
fn the_ladder_drives_admission() {
    // The two halves of the governor, connected. Leaving this to each caller would make
    // the ladder's most consequential effect a convention rather than a value.
    use sankhya_governor::{admit, Decision, Demand, PoolState, Posture, TenantLimits};

    let empty_pool = PoolState {
        total_bytes: 1_000,
        in_use_bytes: 0,
        per_tenant: std::collections::BTreeMap::new(),
        queued: 0,
        max_queue_depth: 4,
    };
    let generous = TenantLimits {
        floor_bytes: 1_000,
        cap_bytes: 1_000,
    };
    let small = Demand {
        tenant: "a".to_string(),
        estimated_bytes: 1,
    };

    // A query that trivially fits is admitted or refused purely by the level.
    for level in [Level::Normal, Level::Watch, Level::Constrain] {
        assert_eq!(level.posture(), Posture::Admitting);
        assert_eq!(
            admit(&small, &empty_pool, &generous, level.posture()),
            Decision::Admit,
            "{level} should still serve"
        );
    }

    for level in [Level::Protect, Level::Sacrifice] {
        assert_eq!(level.posture(), Posture::Shedding);
        assert!(
            !admit(&small, &empty_pool, &generous, level.posture()).admitted(),
            "{level} should not admit, however much is free"
        );
    }
}

#[test]
fn a_source_emergency_stops_queries_even_with_an_idle_machine() {
    // The ordering rule, end to end: the source outranks the analytical tier. An idle
    // pool is not a reason to keep serving while the slot is about to be invalidated.
    use sankhya_governor::{admit, Demand, PoolState, TenantLimits};

    let level = assess(
        &Signals {
            retained_log: 0.9,
            ..quiet()
        },
        &Thresholds::default(),
    )
    .level;
    assert_eq!(level, Level::Protect);

    let decision = admit(
        &Demand {
            tenant: "a".to_string(),
            estimated_bytes: 1,
        },
        &PoolState {
            total_bytes: 1_000_000,
            in_use_bytes: 0,
            per_tenant: std::collections::BTreeMap::new(),
            queued: 0,
            max_queue_depth: 8,
        },
        &TenantLimits {
            floor_bytes: 1_000_000,
            cap_bytes: 1_000_000,
        },
        level.posture(),
    );

    assert!(!decision.admitted());
}
