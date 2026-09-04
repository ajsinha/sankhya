//! Whether this harness can actually find a race, and repeat itself when it does.
//!
//! A test harness for concurrency has to be held to the standard it exists to enforce. The
//! question is not "does it compile" but **"would it have caught the bug"** --- so the subject
//! here is a deliberately broken counter, and the assertion is that the harness notices.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_testkit::{Hammer, Jitter, Until};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A counter with a deliberate race: read, pause, write back.
///
/// The pause is what makes the window wide enough to be found reliably. A real defect's window
/// is narrower, which is the argument for oversubscription and jitter rather than for hoping.
#[derive(Debug, Default)]
struct RacyCounter {
    value: AtomicU64,
}

impl RacyCounter {
    fn increment(&self, jitter: &Jitter) {
        let seen = self.value.load(Ordering::SeqCst);
        jitter.pause();
        self.value.store(seen + 1, Ordering::SeqCst);
    }
}

#[test]
fn the_harness_finds_a_lost_update() {
    // The property the whole crate is for. If this passes, the harness is not provoking
    // anything and every test written with it is decorative.
    const EACH: usize = 200;
    let counter = Arc::new(RacyCounter::default());
    let hammer = Hammer::new().workers(16);

    let run = hammer.run(|_worker, jitter| {
        for _ in 0..EACH {
            counter.increment(jitter);
        }
    });

    let expected = (hammer.worker_count() * EACH) as u64;
    let seen = counter.value.load(Ordering::SeqCst);
    assert!(
        seen < expected,
        "the harness ran {} workers and lost nothing --- it is not provoking the race it \
         exists to provoke ({}) ",
        hammer.worker_count(),
        run.replay_with()
    );
}

#[test]
fn a_correct_counter_survives_the_same_hammering() {
    // The other half, and the one that stops the test above from being satisfied by a harness
    // that simply breaks things. The same contention against a correct implementation must
    // lose nothing at all.
    const EACH: usize = 200;
    let counter = Arc::new(AtomicU64::new(0));
    let hammer = Hammer::new().workers(16);

    hammer.run(|_worker, jitter| {
        for _ in 0..EACH {
            counter.fetch_add(1, Ordering::SeqCst);
            jitter.pause();
        }
    });

    assert_eq!(
        counter.load(Ordering::SeqCst),
        (hammer.worker_count() * EACH) as u64,
        "an atomic increment lost an update, which would mean the hardware is wrong"
    );
}

#[test]
fn one_seed_is_one_schedule() {
    // The property that turns a failure into a fixture. Two runs with the same seed must make
    // the same sequence of choices --- otherwise "replay with seed N" is advice that does not
    // work, which is worse than not offering it.
    let first = Jitter::seeded(12_345);
    let second = Jitter::seeded(12_345);
    let a: Vec<u64> = (0..64).map(|_| first.next()).collect();
    let b: Vec<u64> = (0..64).map(|_| second.next()).collect();
    assert_eq!(a, b, "the same seed produced two different schedules");

    let other: Vec<u64> = {
        let third = Jitter::seeded(12_346);
        (0..64).map(|_| third.next()).collect()
    };
    assert_ne!(a, other, "two different seeds produced the same schedule");
}

#[test]
fn a_zero_seed_still_varies() {
    // Xorshift stuck at zero stays at zero for ever, which would silently make every delay the
    // same delay --- a harness that looks like it is jittering and is not.
    let jitter = Jitter::seeded(0);
    let values: Vec<u64> = (0..16).map(|_| jitter.next()).collect();
    assert!(
        values.iter().any(|value| *value != values[0]),
        "a zero seed produced a constant sequence"
    );
}

#[test]
fn workers_are_released_together_rather_than_merely_started() {
    // A barrier, not a spawn loop. Threads that begin whenever the scheduler gets to them are
    // rarely in the same window, and a harness that only starts them at similar times finds
    // races at a fraction of the rate.
    let inside = Arc::new(AtomicU64::new(0));
    let peak = Arc::new(AtomicU64::new(0));
    let hammer = Hammer::new().workers(12);
    let window = sankhya_testkit::capacity::Window::open(
        "workers_are_released_together_rather_than_merely_started",
        12,
    );

    hammer.run(|_worker, jitter| {
        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        jitter.pause();
        inside.fetch_sub(1, Ordering::SeqCst);
    });

    // Overlap is only observable on a machine with cores to overlap on. Under a loaded
    // build, twelve workers released together can still be scheduled one at a time, and this
    // assertion then reports a harness defect that is not there --- it failed in `check-all`
    // at load 60 and passed alone at load 8, in the same working tree.
    //
    // Asked of the machine rather than of the result: a skip decided by the outcome would
    // also swallow a barrier that genuinely stopped working, which is the whole point here.
    if window.is_some_and(|open| open.held()) {
        assert!(
            peak.load(Ordering::SeqCst) > 1,
            "no two workers were ever inside at once on a quiet machine, so the barrier is \
             not releasing them together"
        );
    } else {
        sankhya_testkit::skipped(
            "workers_are_released_together_rather_than_merely_started",
            "the machine had no spare cores, so two workers overlapping could not be \
             observed either way",
        );
    }
}

#[test]
fn the_stop_flag_is_set_even_when_nobody_remembers() {
    // The hang this prevents was real: an assertion firing before `stop.store(true)` left
    // twelve threads spinning on a flag nobody would ever set, and the test hung rather than
    // failing. Dropping the guard must release the workers.
    let running = {
        let until = Until::new();
        let handle = until.handle();
        assert!(handle.keep_going(), "workers run while the guard is alive");
        handle
        // `until` dropped here, without `stop()` being called.
    };
    assert!(
        !running.keep_going(),
        "the guard went away and the workers were never told to stop"
    );
}

#[test]
fn results_are_collected_after_every_worker_has_joined() {
    // A harness that returned results while threads were still running would encourage exactly
    // the assertion-inside-the-scope mistake it exists to prevent.
    let hammer = Hammer::new().workers(8);
    let run = hammer.run(|worker, _jitter| worker);
    let mut seen = run.outcomes.clone();
    seen.sort_unstable();
    assert_eq!(
        seen,
        (0..hammer.worker_count()).collect::<Vec<usize>>(),
        "every worker's result is present, so every worker finished"
    );
}
