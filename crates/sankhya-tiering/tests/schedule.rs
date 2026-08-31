//! A schedule that is off until somebody turns it on, stops before the irreversible half, and
//! is watched by a guard that compares a run against the runs before it.
//!
//! # What the anomaly guard is really looking for
//!
//! `FR-TIER-29` names three failures — a clock error, a timezone defect, a mis-edited policy —
//! and they share a shape. **Nothing is broken.** The code is correct, the policy is valid, the
//! schedule fires on time, and the number of rows in scope is wrong by orders of magnitude
//! because a boundary moved. No correctness check can see that. Only the size can.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_tiering::schedule::{BlastRadius, Halted, Limit, Schedule, Stage};
use sankhya_tiering::verify::Hash;

fn approved(history: &[usize]) -> Schedule {
    let mut schedule = Schedule::new("nightly-archive");
    schedule.enabled = true;
    schedule.approved = Some(Hash::from_bytes([1; 32]));
    schedule.history = history.to_vec();
    schedule
}

#[test]
fn a_new_schedule_is_off_unapproved_and_stops_at_archive() {
    // `FR-TIER-28` and `FR-TIER-31`. Each of these is the default because the safe
    // configuration should be what somebody gets by not deciding.
    let fresh = Schedule::new("nightly-archive");
    assert!(!fresh.enabled);
    assert!(fresh.approved.is_none());
    assert_eq!(fresh.stage, Stage::Archive);
    assert_eq!(Stage::default(), Stage::Archive);
}

#[test]
fn an_unapproved_schedule_does_not_run_however_enabled_it_is() {
    let mut schedule = Schedule::new("nightly-archive");
    schedule.enabled = true;

    let halt = schedule.admit(2, 0, None).expect_err("never approved");
    assert_eq!(halt, Halted::NotApproved { schedule: "nightly-archive".to_string() });
    assert!(halt.to_string().contains("plan mode first"));
}

#[test]
fn only_the_third_gate_cannot_be_undone() {
    // Archive is a no-op on the source; purge is undone by re-attaching; drop is undone by
    // nothing. Separating them, and time-delaying the third, is what makes this safe to operate.
    assert!(!Stage::Archive.removes_from_source());
    assert!(Stage::Purge.removes_from_source());
    assert!(Stage::Archive.reversible());
    assert!(Stage::Purge.reversible());
    assert!(!Stage::Drop.reversible());
    assert_eq!(Stage::ALL.iter().filter(|stage| !stage.reversible()).count(), 1);
}

#[test]
fn a_kill_switch_stops_a_run_before_it_starts() {
    // `FR-TIER-32`: a kill switch stops new phases and never aborts a job mid-detach, which is
    // why this is a reason a run did not start rather than a way to interrupt one.
    let halt = approved(&[2, 2, 2]).admit(2, 0, Some("ops-freeze")).expect_err("killed");
    assert_eq!(halt, Halted::Killed { switch: "ops-freeze".to_string() });
    assert!(halt.to_string().contains("never interrupted"));
}

#[test]
fn the_kill_switch_is_checked_before_anything_else() {
    // An unapproved schedule with a switch set reports the switch, because an operator who has
    // just thrown one wants to be told it took effect rather than told about something else.
    let mut schedule = Schedule::new("nightly-archive");
    schedule.enabled = false;
    let halt = schedule.admit(2, 0, Some("ops-freeze")).expect_err("killed");
    assert!(matches!(halt, Halted::Killed { .. }));
}

#[test]
fn a_run_far_larger_than_its_history_halts_for_a_person() {
    // A year archived in one pass because a timezone moved a boundary looks exactly like this.
    let halt = approved(&[2, 2, 3, 2]).admit(400, 0, None).expect_err("anomalous");

    assert_eq!(
        halt,
        Halted::Anomalous {
            schedule: "nightly-archive".to_string(),
            candidates: 400,
            median: 2,
            factor: 3
        }
    );
    assert!(halt.to_string().contains("nothing is broken"));
}

#[test]
fn the_comparison_is_against_the_median_and_not_the_mean() {
    // The mean is moved by the very outlier being looked for: one enormous run drags the average
    // up and makes the next enormous run look ordinary. Here the mean is 102 and the median is
    // 2, so a factor of three admits 300 against the mean and refuses 7 against the median.
    let schedule = approved(&[2, 2, 2, 402]);
    assert_eq!(schedule.trailing_median(), Some(2));

    assert!(schedule.admit(6, 0, None).is_ok(), "three times the median is admitted");
    assert!(schedule.admit(7, 0, None).is_err(), "past it is not");
}

#[test]
fn an_even_history_takes_the_midpoint_of_the_two_middle_runs() {
    assert_eq!(approved(&[2, 4]).trailing_median(), Some(3));
    assert_eq!(approved(&[1, 2, 3, 10]).trailing_median(), Some(2));
}

#[test]
fn a_schedules_first_live_run_is_not_halted_for_having_no_history() {
    // There is nothing to compare against, and refusing would mean no schedule could ever have
    // a second run. The approval of the plan digest is what stands in for the comparison there.
    let schedule = approved(&[]);
    assert_eq!(schedule.trailing_median(), None);
    assert!(schedule.admit(3, 0, None).is_ok());
}

#[test]
fn a_history_of_empty_runs_does_not_refuse_every_future_run() {
    // A median of zero times any factor is zero, so a naive comparison would halt on the first
    // range a schedule ever moved after a quiet week.
    let schedule = approved(&[0, 0, 0]);
    assert_eq!(schedule.trailing_median(), Some(0));
    assert!(schedule.admit(3, 0, None).is_ok());
}

#[test]
fn a_run_stops_cleanly_at_the_per_run_limit_rather_than_refusing() {
    // A schedule that refuses outright when it is one range over makes no progress at all, and
    // an operator who has to raise a limit to get any work done raises it too far.
    let allowed = approved(&[4, 4, 4]).admit(10, 0, None).unwrap();
    assert_eq!(allowed.ranges, 4);
    assert_eq!(allowed.stopped_by, Some(Limit::Run { limit: 4 }));
    assert!(allowed.stopped_by.unwrap().to_string().contains("stopped cleanly"));
}

#[test]
fn the_daily_limit_binds_across_runs_that_each_respect_the_per_run_one() {
    // Per-run alone is defeated by a schedule that fires hourly. Two runs of four are inside the
    // per-run limit and exactly at the daily one; the third gets nothing.
    let schedule = approved(&[4, 4, 4]);
    let radius = BlastRadius::default();
    assert_eq!(radius.ranges_per_run, 4);
    assert_eq!(radius.ranges_per_day, 8);

    assert_eq!(schedule.admit(4, 0, None).unwrap().ranges, 4);
    assert_eq!(schedule.admit(4, 4, None).unwrap().ranges, 4);

    let exhausted = schedule.admit(4, 8, None).unwrap();
    assert_eq!(exhausted.ranges, 0);
    assert_eq!(exhausted.stopped_by, Some(Limit::Day { limit: 8, already: 8 }));
}

#[test]
fn a_run_inside_both_limits_reports_nothing_stopping_it() {
    let allowed = approved(&[4, 4, 4]).admit(3, 0, None).unwrap();
    assert_eq!(allowed.ranges, 3);
    assert_eq!(allowed.stopped_by, None);
}

#[test]
fn the_daily_limit_is_named_when_it_is_the_tighter_of_the_two() {
    // Both limits would cut the run short; the report names the one that actually did, because
    // an operator raising the wrong number learns nothing.
    let radius = BlastRadius { ranges_per_run: 4, ranges_per_day: 8 };
    let allowed = radius.allow(10, 6);
    assert_eq!(allowed.ranges, 2);
    assert_eq!(allowed.stopped_by, Some(Limit::Day { limit: 8, already: 6 }));
}
