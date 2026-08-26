//! Stopping work, and the bound on how long that takes.
//!
//! "Cancellable" is not the property worth testing — almost everything is, given long
//! enough. The property is that cancellation takes effect within a stated bound, so the
//! tests here are about *when* work stops, not whether it does.

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

use sankhya_governor::{Budget, Cancel, Deadline, Stopped};

#[test]
fn work_continues_while_there_is_time_and_nobody_objects() {
    let budget = Budget::new(Deadline::at(100), Cancel::new(), 1);
    assert!(budget.check(0).is_ok());
    assert!(budget.check(99).is_ok());
}

#[test]
fn a_passed_deadline_stops_the_work_and_says_it_may_be_retried() {
    // The distinction a client acts on: the same query might succeed with longer to run.
    let budget = Budget::new(Deadline::at(100), Cancel::new(), 1);
    let Err(stopped) = budget.check(100) else {
        panic!("the deadline has passed");
    };
    assert!(matches!(stopped, Stopped::DeadlineExceeded { .. }));
    assert!(stopped.retryable());
    assert!(format!("{stopped}").contains("too late to be useful"));
}

#[test]
fn a_cancellation_stops_the_work_and_says_retrying_is_pointless() {
    let cancel = Cancel::new();
    let budget = Budget::new(Deadline::never(), cancel.clone(), 1);
    assert!(budget.check(0).is_ok());

    cancel.cancel();
    assert_eq!(budget.check(0), Err(Stopped::Cancelled));
    assert!(!Stopped::Cancelled.retryable());
}

#[test]
fn a_cancellation_is_reported_in_preference_to_a_deadline() {
    // Both apply. Reporting the deadline would tell somebody to retry work they
    // deliberately stopped.
    let cancel = Cancel::new();
    let budget = Budget::new(Deadline::at(10), cancel.clone(), 1);
    cancel.cancel();

    assert_eq!(budget.check(1_000), Err(Stopped::Cancelled));
}

#[test]
fn a_token_cannot_be_un_cancelled() {
    // Work that observed a cancellation may already have released resources or discarded
    // partial state. A token that could be reset would let a caller resume something
    // that is no longer resumable.
    let cancel = Cancel::new();
    cancel.cancel();
    cancel.cancel();
    assert!(cancel.is_cancelled());

    // And there is no method that could undo it -- checked by the type having none,
    // which this test documents rather than proves.
    let budget = Budget::new(Deadline::never(), cancel, 1);
    assert_eq!(budget.check(0), Err(Stopped::Cancelled));
}

#[test]
fn every_holder_of_a_token_observes_the_cancellation() {
    let cancel = Cancel::new();
    let holders: Vec<Cancel> = (0..8).map(|_| cancel.clone()).collect();
    assert!(holders.iter().all(|h| !h.is_cancelled()));

    holders[3].cancel();
    assert!(holders.iter().all(Cancel::is_cancelled));
    assert!(cancel.is_cancelled(), "including the original");
}

#[test]
fn cancellation_crosses_threads_within_the_stated_bound() {
    // The property that matters, exercised the way it actually happens: one thread
    // cancels, another is in a loop and must notice.
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    const CHECK_EVERY: u64 = 64;

    let cancel = Cancel::new();
    let budget = Budget::new(Deadline::never(), cancel.clone(), CHECK_EVERY);
    let observed_at = Arc::new(AtomicU64::new(0));

    let worker = {
        let budget = budget.clone();
        let observed_at = Arc::clone(&observed_at);
        std::thread::spawn(move || {
            // Bounded, and the bound is the point. A budget that never stops is a
            // defect, and a test that loops forever on it stalls the suite instead of
            // reporting it -- which is how a real defect gets discovered as a timeout in
            // someone else's pipeline rather than as a failure here.
            //
            // This is not hypothetical: removing the cancellation check made the first
            // version of this test hang for thirty minutes.
            const GIVE_UP_AFTER: u64 = 50_000_000;

            let mut units = 0u64;
            while units < GIVE_UP_AFTER {
                units += 1;
                if budget.check_periodically(units, 0).is_err() {
                    observed_at.store(units, Ordering::SeqCst);
                    return Some(units);
                }
                std::hint::black_box(units);
            }
            observed_at.store(u64::MAX, Ordering::SeqCst);
            None
        })
    };

    // Let it get going, then stop it.
    while observed_at.load(Ordering::SeqCst) == 0 && !cancel.is_cancelled() {
        cancel.cancel();
    }

    let stopped_at = worker
        .join()
        .expect("the worker panicked")
        .expect("the worker never observed the cancellation");
    assert!(stopped_at > 0);
    assert_eq!(
        stopped_at % CHECK_EVERY,
        0,
        "work stopped on a unit the budget does not check"
    );
}

#[test]
fn periodic_checking_observes_a_cancellation_within_its_interval() {
    // The bound, stated as arithmetic rather than as a hope about the code.
    const CHECK_EVERY: u64 = 100;
    let cancel = Cancel::new();
    let budget = Budget::new(Deadline::never(), cancel.clone(), CHECK_EVERY);

    // Cancelled part-way through an interval.
    cancel.cancel();

    let mut units = 51u64;
    let stopped_at = loop {
        if budget.check_periodically(units, 0).is_err() {
            break units;
        }
        units += 1;
        assert!(units < 1_000, "it never noticed");
    };

    assert!(
        stopped_at - 51 < CHECK_EVERY,
        "took {} units to notice, and the interval is {CHECK_EVERY}",
        stopped_at - 51
    );
}

#[test]
fn a_check_interval_of_zero_is_not_expressible() {
    // Zero would mean never checking, which is the one value that must not be
    // constructible -- it turns a bounded cancellation into an unbounded one silently.
    let budget = Budget::new(Deadline::at(1), Cancel::new(), 0);
    assert_eq!(budget.check_every(), 1);
    assert!(
        budget.check_periodically(1, 5).is_err(),
        "it must still check"
    );
}

#[test]
fn a_deadline_that_never_passes_has_to_be_asked_for() {
    // Named rather than expressed as an absent value, so unbounded work is a decision
    // somebody made rather than one they forgot to make.
    let budget = Budget::new(Deadline::never(), Cancel::new(), 1);
    assert!(budget.check(u64::MAX - 1).is_ok());
    assert_eq!(Deadline::never().remaining(0), u64::MAX);
}

#[test]
fn a_deadline_already_in_the_past_stops_immediately() {
    let budget = Budget::new(Deadline::at(5), Cancel::new(), 1);
    assert!(budget.check(5).is_err());
    assert!(budget.check(6).is_err());
    assert_eq!(Deadline::at(5).remaining(9), 0);
}

#[test]
fn a_relative_deadline_saturates_rather_than_wrapping() {
    // Wrapping would turn a very long deadline into one already in the past, which is
    // the worst possible direction for an arithmetic slip.
    let d = Deadline::after(u64::MAX - 1, 1_000);
    assert!(!d.expired_at(u64::MAX - 1));
}

#[test]
fn an_unbounded_budget_still_stops_when_cancelled() {
    // "No deadline" must not mean "no way to stop".
    let budget = Budget::unbounded();
    let cancel = budget.cancel_token();
    assert!(budget.check(u64::MAX - 1).is_ok());

    cancel.cancel();
    assert_eq!(budget.check(0), Err(Stopped::Cancelled));
}
