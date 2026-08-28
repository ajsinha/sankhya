//! Projections, and the far more important refusals to make one.
//!
//! The tests that carry this file are the ones asserting **no date is given**. A diagnostic
//! that invents a projection from one sample produces a number with a date attached, and a
//! date is exactly what gets believed and scheduled around.

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

use sankhya_diagnostic::projection::{
    human_duration, Concern, Confidence, Observation, Projection, Trend, Unknown,
    EXTRAPOLATION_FACTOR, FIRM_OBSERVATIONS,
};

const SECOND: i64 = 1_000_000;
const HOUR: i64 = 3_600 * SECOND;
const DAY: i64 = 24 * HOUR;

/// A series growing by `per_day` each day, sampled daily.
fn growing(from: f64, per_day: f64, days: usize) -> Trend {
    Trend::of((0..days).map(|d| {
        #[allow(clippy::cast_precision_loss)]
        Observation::new(d as i64 * DAY, from + per_day * d as f64)
    }))
}

// --- the refusals ---------------------------------------------------------

#[test]
fn one_observation_yields_no_date() {
    // The first run of a diagnostic is always this. Saying so is the honest answer, and it
    // is uncomfortable precisely because an operator wants a number.
    let trend = Trend::of([Observation::new(0, 400.0)]);
    let projection = trend.time_until(1_000.0, Concern::RisingTo, 0);

    assert_eq!(
        projection,
        Projection::Unknown {
            reason: Unknown::TooFewObservations { have: 1 }
        }
    );
    assert!(!projection.is_actionable());
    assert!(
        projection.describe().contains("a time needs a rate"),
        "the refusal must say what is missing: {}",
        projection.describe()
    );
}

#[test]
fn no_observations_yields_no_date_either() {
    assert!(matches!(
        Trend::default().time_until(100.0, Concern::RisingTo, 0),
        Projection::Unknown {
            reason: Unknown::TooFewObservations { have: 0 }
        }
    ));
}

#[test]
fn a_sawtooth_is_refused_rather_than_projected_through() {
    // Debt accumulating and being compacted away fits a line badly *by construction*. A
    // date drawn through it reports where in the cycle the samples happened to fall, which
    // is a different quantity that looks identical.
    let sawtooth = Trend::of((0..12).map(|d| {
        #[allow(clippy::cast_precision_loss)]
        let value = if d % 4 == 3 { 50.0 } else { 200.0 + (d % 4) as f64 * 200.0 };
        Observation::new(d * DAY, value)
    }));

    let projection = sawtooth.time_until(1_000.0, Concern::RisingTo, 0);
    let Projection::Unknown {
        reason: Unknown::NotLinear { fit },
    } = projection
    else {
        panic!("a sawtooth must not yield a date, and gave {projection:?}");
    };
    assert!(fit < 0.8, "the fit is {fit}");
    assert!(projection.describe().contains("sawtooth"));
}

#[test]
fn observations_all_at_one_instant_yield_no_date() {
    let stacked = Trend::of([
        Observation::new(1_000, 10.0),
        Observation::new(1_000, 20.0),
        Observation::new(1_000, 30.0),
    ]);
    assert!(matches!(
        stacked.time_until(100.0, Concern::RisingTo, 1_000),
        Projection::Unknown {
            reason: Unknown::NoElapsedTime
        }
    ));
}

// --- the projections ------------------------------------------------------

#[test]
fn a_steady_rise_gives_the_date_the_arithmetic_implies() {
    // 400 files, growing by 100 a day, threshold 1,000. Six days.
    let trend = growing(400.0, 100.0, 6);
    let now = 5 * DAY; // the last observation

    let Projection::Crossing { seconds, .. } = trend.time_until(1_000.0, Concern::RisingTo, now) else {
        panic!("a steady rise must yield a date");
    };
    let days = seconds / 86_400;
    assert_eq!(days, 1, "at 900 and rising 100 a day, one day remains");
}

/// Two observations five days apart, the second 100 below the threshold.
fn two_apart() -> Trend {
    Trend::of([Observation::new(0, 400.0), Observation::new(5 * DAY, 900.0)])
}

#[test]
fn two_observations_are_reported_but_marked_weak() {
    // Reported, because an operator with two samples still wants the estimate. Marked,
    // because a slope through two points is a slope through two pieces of noise as readily
    // as through a trend — and acting on it as firm is how a maintenance window is booked
    // for the wrong week.
    let Projection::Crossing { confidence, .. } =
        two_apart().time_until(1_000.0, Concern::RisingTo, 5 * DAY)
    else {
        panic!("two observations give a slope");
    };
    assert_eq!(confidence, Confidence::Weak);

    let firm = growing(400.0, 100.0, FIRM_OBSERVATIONS);
    let Projection::Crossing { confidence, .. } =
        firm.time_until(1_000.0, Concern::RisingTo, (FIRM_OBSERVATIONS as i64 - 1) * DAY)
    else {
        panic!("five observations give a slope");
    };
    assert_eq!(confidence, Confidence::Firm);
}

#[test]
fn a_weak_projection_says_so_in_words() {
    let described = two_apart()
        .time_until(1_000.0, Concern::RisingTo, 5 * DAY)
        .describe();
    assert!(
        described.contains("on only two observations"),
        "an operator must see the hedge without reading a field: {described}"
    );
}

#[test]
fn a_threshold_already_crossed_is_distinct_from_crossing_in_zero_seconds() {
    // The response differs: one is a warning and the other is an incident.
    let trend = growing(900.0, 100.0, 4);
    assert_eq!(trend.time_until(1_000.0, Concern::RisingTo, 3 * DAY), Projection::Already);
    assert!(trend.time_until(1_000.0, Concern::RisingTo, 3 * DAY).is_actionable());
}

#[test]
fn a_falling_measure_can_cross_a_threshold_from_above() {
    // Free space running out is the mirror of debt building up, and both occur.
    let shrinking = Trend::of((0..6).map(|d| {
        #[allow(clippy::cast_precision_loss)]
        Observation::new(d * DAY, 600.0 - 100.0 * d as f64)
    }));
    let Projection::Crossing { seconds, .. } = shrinking.time_until(0.0, Concern::FallingTo, 5 * DAY) else {
        panic!("a falling series must reach zero");
    };
    assert_eq!(seconds / 86_400, 1);
}

#[test]
fn a_measure_moving_away_from_the_threshold_is_not_a_finding() {
    let shrinking = Trend::of((0..6).map(|d| {
        #[allow(clippy::cast_precision_loss)]
        Observation::new(d * DAY, 900.0 - 100.0 * d as f64)
    }));
    let projection = shrinking.time_until(1_000.0, Concern::RisingTo, 5 * DAY);
    assert_eq!(projection, Projection::Receding);
    assert!(!projection.is_actionable());
}

#[test]
fn a_flat_measure_is_receding_rather_than_crossing_at_infinity() {
    let flat = Trend::of((0..6).map(|d| Observation::new(d * DAY, 500.0)));
    assert_eq!(flat.time_until(1_000.0, Concern::RisingTo, 5 * DAY), Projection::Receding);
}

// --- the rate itself ------------------------------------------------------

#[test]
fn a_rate_is_none_rather_than_zero_when_it_cannot_be_known() {
    // "Not changing" and "cannot tell" are different facts, and only one of them justifies
    // doing nothing.
    assert_eq!(Trend::default().rate_per_second(), None);
    assert_eq!(
        Trend::of([Observation::new(0, 1.0)]).rate_per_second(),
        None
    );

    let flat = Trend::of((0..4).map(|d| Observation::new(d * DAY, 7.0)));
    assert_eq!(flat.rate_per_second(), Some(0.0), "flat is a known rate of zero");
}

#[test]
fn observations_out_of_order_still_give_the_slope_the_data_has() {
    // Otherwise the sign of the trend depends on insertion order, which is how a growing
    // measure gets reported as shrinking.
    let forwards = growing(0.0, 100.0, 5);
    let backwards = Trend::of((0..5).rev().map(|d| {
        #[allow(clippy::cast_precision_loss)]
        Observation::new(d * DAY, 100.0 * d as f64)
    }));
    assert_eq!(forwards.rate_per_second(), backwards.rate_per_second());
    assert!(forwards.rate_per_second().unwrap_or(0.0) > 0.0);
}

// --- how it reads ---------------------------------------------------------

#[test]
fn durations_are_rounded_to_the_units_a_person_plans_in() {
    // A projection accurate to the second implies a precision the estimate does not have,
    // and an operator reading "9 days, 4 hours and 12 minutes" reasonably assumes somebody
    // knows that much.
    assert_eq!(human_duration(30), "less than a minute");
    assert_eq!(human_duration(600), "10 minutes");
    // Singular, because "in 1 days" costs more trust than it saves characters.
    assert_eq!(human_duration(60), "1 minute");
    assert_eq!(human_duration(3_600), "1 hour");
    assert_eq!(human_duration(86_400), "1 day");
    assert_eq!(human_duration(90 * 86_400), "3 months");
    assert_eq!(human_duration(7_200), "2 hours");
    assert_eq!(human_duration(9 * 86_400), "9 days");
    assert_eq!(human_duration(200 * 86_400), "6 months");
}

// --- the horizon ----------------------------------------------------------

#[test]
fn a_crossing_further_out_than_the_data_supports_gets_no_date() {
    // Four days of samples, a crossing 175 days away. The arithmetic is sound and the claim
    // is not: nothing in four days says the rate survives six months, and "about 6 months"
    // is a figure an operator plans around.
    let creeping = Trend::of((0..5).map(|d| {
        #[allow(clippy::cast_precision_loss)]
        Observation::new(d * DAY, 10.0 + 20.0 * d as f64)
    }));
    let projection = creeping.time_until(3_600.0, Concern::RisingTo, 4 * DAY);
    let Projection::Beyond { horizon_seconds } = projection else {
        panic!("a distant crossing must not be dated, and gave {projection:?}");
    };
    assert_eq!(
        horizon_seconds,
        4 * 86_400 * EXTRAPOLATION_FACTOR,
        "the horizon is the observed span times the factor"
    );
    assert!(!projection.is_actionable());
    assert!(projection.describe().contains("arithmetic rather than evidence"));
}

#[test]
fn the_horizon_widens_as_the_observation_window_grows() {
    // The same rate and the same threshold, watched for longer, eventually earns the date.
    // Otherwise the horizon would be a permanent gag rather than a statement about evidence.
    let rate_per_day = 20.0;
    let distant = |days: i64| {
        Trend::of((0..=days).map(|d| {
            #[allow(clippy::cast_precision_loss)]
            Observation::new(d * DAY, 10.0 + rate_per_day * d as f64)
        }))
    };
    assert!(matches!(
        distant(4).time_until(400.0, Concern::RisingTo, 4 * DAY),
        Projection::Beyond { .. }
    ));
    assert!(matches!(
        distant(15).time_until(400.0, Concern::RisingTo, 15 * DAY),
        Projection::Crossing { .. }
    ));
}

#[test]
fn the_horizon_sorts_below_a_date_but_above_nothing_at_all() {
    // Beyond the horizon is still a direction of travel, and "no rate at all" is not. An
    // operator triaging the tail of a report should meet them in that order.
    let beyond = Projection::Beyond {
        horizon_seconds: 86_400,
    };
    let nothing = Projection::Unknown {
        reason: Unknown::TooFewObservations { have: 1 },
    };
    assert!(!beyond.is_actionable() && !nothing.is_actionable());
    assert_ne!(beyond, nothing);
}
