//! The scheduler arbitrates; it does not negotiate.
//!
//! The assertions worth reading are the ones about what the scheduler refuses to be
//! talked into: promoting starved work above a safety emergency, starting a job that
//! cannot finish, and — structurally — destroying retained history.

use sankhya_maintenance::{schedule, Class, Deferral, Job, SystemState};

fn job(name: &str, class: Class, ticks: u64) -> Job {
    Job {
        name: name.to_string(),
        class,
        estimated_ticks: ticks,
        resumable: true,
        ticks_deferred: 0,
        ticks_to_visible: None,
    }
}

fn idle() -> SystemState {
    SystemState {
        in_maintenance_window: false,
        queries_running: 0,
        duty_cycle_ticks_remaining: 100,
    }
}

fn names(s: &sankhya_maintenance::Schedule) -> Vec<&str> {
    s.run.iter().map(|r| r.job.name.as_str()).collect()
}

#[test]
fn classes_run_in_strict_order() {
    let jobs = vec![
        job("recluster", Class::Optional, 1),
        job("compact", Class::Performance, 1),
        job("freeze", Class::Safety, 1),
        job("expire", Class::Housekeeping, 1),
        job("reclaim", Class::Availability, 1),
    ];
    let state = SystemState {
        in_maintenance_window: true,
        ..idle()
    };

    let s = schedule(&jobs, &state);
    assert_eq!(
        names(&s),
        vec!["freeze", "reclaim", "compact", "expire", "recluster"]
    );
}

#[test]
fn safety_work_preempts_queries_and_ignores_the_budget() {
    // The exception the whole design exists for. The duty cycle is exhausted and
    // queries are running, and freeze remediation goes anyway: a stopped database is
    // worse than a slow query.
    let jobs = vec![job("freeze", Class::Safety, 10_000)];
    let state = SystemState {
        in_maintenance_window: false,
        queries_running: 12,
        duty_cycle_ticks_remaining: 0,
    };

    let s = schedule(&jobs, &state);
    assert_eq!(names(&s), vec!["freeze"]);
    assert!(s.preempts_queries());
    assert!(s.deferred.is_empty());
}

#[test]
fn availability_work_preempts_but_is_audited() {
    // Preemption is permitted and recorded. Taking capacity from a user's query is a
    // decision someone may later need to account for.
    let jobs = vec![job("reclaim", Class::Availability, 10_000)];
    let state = SystemState {
        queries_running: 3,
        duty_cycle_ticks_remaining: 0,
        ..idle()
    };

    let s = schedule(&jobs, &state);
    assert_eq!(s.run.len(), 1);
    assert!(s.run[0].preempts_queries);
    assert!(s.run[0].audited, "preempting a query must be auditable");
}

#[test]
fn performance_work_never_preempts_a_query() {
    // Compaction matters. It does not matter more than the query someone is waiting on.
    let jobs = vec![job("compact", Class::Performance, 1)];
    let state = SystemState {
        queries_running: 50,
        ..idle()
    };

    let s = schedule(&jobs, &state);
    assert_eq!(s.run.len(), 1);
    assert!(!s.run[0].preempts_queries);
    assert!(!s.run[0].audited);
}

#[test]
fn the_duty_cycle_bounds_the_lower_classes() {
    let jobs = vec![
        job("a", Class::Performance, 60),
        job("b", Class::Performance, 60),
        job("c", Class::Housekeeping, 10),
    ];
    let s = schedule(&jobs, &idle());

    assert_eq!(names(&s), vec!["a", "c"], "b does not fit and c does");
    assert_eq!(s.deferred.len(), 1);
    assert!(matches!(s.deferred[0].1, Deferral::BudgetExhausted { .. }));
}

#[test]
fn a_job_that_cannot_checkpoint_is_refused_rather_than_started() {
    // Starting it would spend the rest of the duty cycle and finish nothing, then do
    // the same tomorrow. A job that can only run to completion never completes on a
    // busy system, and the reason names the actual remedy.
    let mut j = job("recluster-whole-table", Class::Performance, 500);
    j.resumable = false;

    let s = schedule(&[j], &idle());

    assert!(s.run.is_empty());
    assert!(matches!(s.deferred[0].1, Deferral::WouldNotFinish { .. }));
    assert!(format!("{}", s.deferred[0].1).contains("finish nothing"));
}

#[test]
fn a_resumable_job_too_large_for_the_budget_is_deferred_differently() {
    // Same shortfall, different diagnosis. This one will run tomorrow; the other never
    // will. Collapsing the two into one message would hide a job that is permanently
    // stuck behind a queue that is merely busy.
    let j = job("compact", Class::Performance, 500);
    let s = schedule(&[j], &idle());

    assert!(matches!(s.deferred[0].1, Deferral::BudgetExhausted { .. }));
}

#[test]
fn optional_work_waits_for_a_window() {
    let jobs = vec![job("recluster", Class::Optional, 1)];

    let outside = schedule(&jobs, &idle());
    assert!(outside.run.is_empty());
    assert!(matches!(outside.deferred[0].1, Deferral::OutsideWindow));

    let inside = schedule(
        &jobs,
        &SystemState {
            in_maintenance_window: true,
            ..idle()
        },
    );
    assert_eq!(names(&inside), vec!["recluster"]);
}

#[test]
fn waiting_does_not_promote_a_job_out_of_its_class() {
    // The decision this scheduler refuses to make. Compaction has waited a week and
    // freeze remediation arrived a moment ago; freeze still goes first. Ageing work
    // upward would eventually put a compaction ahead of a wraparound emergency, which
    // is exactly what the class ordering exists to prevent.
    let mut starved = job("compact", Class::Performance, 1);
    starved.ticks_deferred = 10_000;
    let fresh = job("freeze", Class::Safety, 1);

    let s = schedule(&[starved, fresh], &idle());
    assert_eq!(names(&s), vec!["freeze", "compact"]);
}

#[test]
fn within_a_class_the_job_about_to_hurt_someone_goes_first() {
    // Not the largest backlog — the soonest consequence. "Latency on this table doubles
    // in two days" outranks "this partition has more files", however many more.
    let mut soon = job("small-but-urgent", Class::Performance, 1);
    soon.ticks_to_visible = Some(2);
    let mut later = job("large-but-not-urgent", Class::Performance, 1);
    later.ticks_to_visible = Some(200);
    let never = job("no-consequence", Class::Performance, 1);

    let s = schedule(&[never, later, soon], &idle());
    assert_eq!(
        names(&s),
        vec!["small-but-urgent", "large-but-not-urgent", "no-consequence"],
        "a job with no user-visible consequence must sort last, not first"
    );
}

#[test]
fn within_a_class_and_equally_urgent_the_longer_wait_goes_first() {
    let mut old = job("waited", Class::Performance, 1);
    old.ticks_deferred = 500;
    let new = job("arrived", Class::Performance, 1);

    let s = schedule(&[new, old], &idle());
    assert_eq!(names(&s), vec!["waited", "arrived"]);
}

#[test]
fn deferral_is_reported_so_a_capacity_problem_is_visible() {
    // Strict priority starves the bottom of the queue by design. What makes that
    // acceptable is that the starvation is measured: a compaction deferred for a week
    // is a capacity problem, and promoting one job would hide it rather than solve it.
    let mut starved = job("compact", Class::Performance, 500);
    starved.ticks_deferred = 10_000;
    starved.ticks_to_visible = Some(3);

    let mut minor = job("expire", Class::Housekeeping, 500);
    minor.ticks_deferred = 5;

    let s = schedule(&[starved, minor], &idle());

    let (job, waited) = s.longest_deferral().expect("something was deferred");
    assert_eq!(job.name, "compact");
    assert_eq!(waited, 10_000);

    let (soonest, ticks) = s
        .soonest_visible_deferral()
        .expect("one deferred job has a visible consequence");
    assert_eq!(soonest.name, "compact");
    assert_eq!(ticks, 3);
}

#[test]
fn an_empty_queue_schedules_nothing_and_reports_nothing() {
    let s = schedule(&[], &idle());
    assert!(s.run.is_empty());
    assert!(s.deferred.is_empty());
    assert!(!s.preempts_queries());
    assert!(s.longest_deferral().is_none());
}

#[test]
fn the_ladder_has_exactly_five_classes_and_none_of_them_erases() {
    // A guard on the type rather than on behaviour.
    //
    // The architecture requires that the ordinary maintenance scheduler be
    // *structurally* incapable of destroying retained history: erasure is a different
    // job class with a different authorization path, not the bottom rung of this
    // ladder. The failure that prevents — a misconfigured retention default quietly
    // deleting records that were legally required to persist — is both unrecoverable
    // and silent, so a review comment is not sufficient protection.
    //
    // This test exists to fail if someone adds a variant, so the addition has to be
    // deliberate and has to be explained here.
    let all = [
        Class::Safety,
        Class::Availability,
        Class::Performance,
        Class::Housekeeping,
        Class::Optional,
    ];
    assert_eq!(all.len(), 5);

    // Exhaustive: adding a variant stops this compiling.
    for class in all {
        match class {
            Class::Safety | Class::Availability => assert!(class.may_preempt_queries()),
            Class::Performance | Class::Housekeeping | Class::Optional => {
                assert!(!class.may_preempt_queries());
                assert!(class.bounded_by_duty_cycle());
            }
        }
    }
}

#[test]
fn preemption_is_a_property_of_the_class_not_of_the_load() {
    // With no queries running, safety work still runs — it simply preempts nothing.
    // Conflating "may preempt" with "is preempting" would make the audit record depend
    // on load, which is not what an auditor is asking about.
    let jobs = vec![job("freeze", Class::Safety, 1)];
    let s = schedule(&jobs, &idle());

    assert_eq!(s.run.len(), 1);
    assert!(!s.run[0].preempts_queries, "there was nothing to preempt");
    assert!(Class::Safety.may_preempt_queries());
}

use proptest::prelude::*;

fn any_class() -> impl Strategy<Value = Class> {
    prop_oneof![
        Just(Class::Safety),
        Just(Class::Availability),
        Just(Class::Performance),
        Just(Class::Housekeeping),
        Just(Class::Optional),
    ]
}

fn any_job() -> impl Strategy<Value = Job> {
    (
        any_class(),
        0u64..200,
        any::<bool>(),
        0u64..1000,
        prop::option::of(0u64..1000),
        0usize..40,
    )
        .prop_map(
            |(class, estimated_ticks, resumable, ticks_deferred, ticks_to_visible, id)| Job {
                name: format!("job-{id:03}"),
                class,
                estimated_ticks,
                resumable,
                ticks_deferred,
                ticks_to_visible,
            },
        )
}

fn any_state() -> impl Strategy<Value = SystemState> {
    (any::<bool>(), 0usize..50, 0u64..500).prop_map(
        |(in_maintenance_window, queries_running, duty_cycle_ticks_remaining)| SystemState {
            in_maintenance_window,
            queries_running,
            duty_cycle_ticks_remaining,
        },
    )
}

proptest! {
    /// Every job is decided exactly once.
    ///
    /// A job that appears in neither list has been silently dropped, which on a
    /// maintenance queue means work that will never happen and nothing to say so.
    #[test]
    fn every_job_is_either_run_or_deferred(
        jobs in prop::collection::vec(any_job(), 0..25),
        state in any_state(),
    ) {
        let s = schedule(&jobs, &state);
        prop_assert_eq!(s.run.len() + s.deferred.len(), jobs.len());

        let mut seen: Vec<&str> = s.run.iter().map(|r| r.job.name.as_str()).collect();
        seen.extend(s.deferred.iter().map(|(j, _)| j.name.as_str()));
        seen.sort_unstable();

        let mut expected: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        expected.sort_unstable();

        prop_assert_eq!(seen, expected);
    }

    /// The run order never inverts the class ladder.
    #[test]
    fn the_run_order_is_never_out_of_class_order(
        jobs in prop::collection::vec(any_job(), 0..25),
        state in any_state(),
    ) {
        let s = schedule(&jobs, &state);
        for pair in s.run.windows(2) {
            prop_assert!(
                pair[0].job.class <= pair[1].job.class,
                "{} ran before {}",
                pair[1].job.class,
                pair[0].job.class
            );
        }
    }

    /// Bounded classes never collectively exceed the duty cycle.
    ///
    /// The budget is the whole reason a lower class can be deferred; a scheduler that
    /// overspends it has no budget at all.
    #[test]
    fn the_duty_cycle_is_never_overspent(
        jobs in prop::collection::vec(any_job(), 0..25),
        state in any_state(),
    ) {
        let s = schedule(&jobs, &state);
        let spent: u64 = s
            .run
            .iter()
            .filter(|r| r.job.class.bounded_by_duty_cycle())
            .map(|r| r.job.estimated_ticks)
            .sum();
        prop_assert!(
            spent <= state.duty_cycle_ticks_remaining,
            "spent {} of {}",
            spent,
            state.duty_cycle_ticks_remaining
        );
    }

    /// Safety and availability work is never deferred, whatever the state.
    ///
    /// There is no budget, window or load under which a freeze emergency waits. If this
    /// can be made to fail, the scheduler has acquired a way to express the one
    /// decision it must not be able to make.
    #[test]
    fn safety_work_is_never_deferred(
        jobs in prop::collection::vec(any_job(), 0..25),
        state in any_state(),
    ) {
        let s = schedule(&jobs, &state);
        for (job, reason) in &s.deferred {
            prop_assert!(
                !job.class.may_preempt_queries(),
                "{} work was deferred: {}",
                job.class,
                reason
            );
        }
    }

    /// Optional work outside a window is always deferred, whatever else is true.
    #[test]
    fn optional_work_outside_a_window_never_runs(
        jobs in prop::collection::vec(any_job(), 0..25),
        state in any_state(),
    ) {
        let s = schedule(&jobs, &state);
        if !state.in_maintenance_window {
            for scheduled in &s.run {
                prop_assert!(scheduled.job.class != Class::Optional);
            }
        }
    }
}
