//! What a sweeper is allowed to conclude, and what it must not.
//!
//! Every test here is arranged so that the *unsafe* direction fails and the *wasteful* one
//! passes. A late reclamation costs disk; an early one costs a query, and the query fails
//! naming a file the caller never mentioned. Those are not symmetric and the tests should not
//! pretend they are.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_leases::Leases;
use std::sync::{Arc, Barrier};

#[test]
fn nobody_reading_means_everything_is_collectable() {
    let leases = Leases::new();
    let marked = leases.mark();
    assert!(leases.drained(marked), "an idle warehouse collects freely");
    assert_eq!(leases.oldest_active(), None);
}

#[test]
fn a_reader_that_started_first_holds_the_sweeper_off() {
    // The whole point. The reader pinned before the file stopped being referenced, so it may
    // be holding it, and the sweeper must wait.
    let leases = Leases::new();
    let reader = leases.pin();
    let marked = leases.mark();

    assert!(!leases.drained(marked), "a reader older than the mark blocks it");
    drop(reader);
    assert!(leases.drained(marked), "and stops blocking it when it finishes");
}

#[test]
fn a_reader_that_started_later_does_not() {
    // A reader that pinned after the file stopped being referenced resolved a log that does
    // not name it, so it cannot ask for it. Waiting for that reader would mean a busy
    // warehouse never reclaims anything --- the failure this system has met from the other
    // direction, when a soak filled a disk.
    let leases = Leases::new();
    let marked = leases.mark();
    let _later = leases.pin();

    assert!(leases.drained(marked), "a newer reader cannot be holding it");
}

#[test]
fn two_sweeps_do_not_release_each_other() {
    // Marks advance, so an older sweep's readers draining does not release a file the later
    // sweep marked. Sharing one deadline between sweeps would let the second delete a file
    // while a reader that arrived between them still held it.
    let leases = Leases::new();
    let first = leases.mark();
    let reader = leases.pin();
    let second = leases.mark();

    assert!(leases.drained(first), "nobody was reading when the first mark was taken");
    assert!(!leases.drained(second), "somebody was, when the second was");
    drop(reader);
    assert!(leases.drained(second));
}

#[test]
fn a_colliding_reader_never_shortens_the_wait() {
    // Slots are a fixed pool, so two readers can share one. The slot keeps the *older*
    // announcement and the younger reader's departure must not clear it.
    let leases = Leases::with_slots(1);
    let older = leases.pin();
    let younger = leases.pin();
    let marked = leases.mark();

    drop(younger);
    assert!(
        !leases.drained(marked),
        "the younger reader left and the older one is still reading"
    );
    drop(older);
    assert!(leases.drained(marked));
}

#[test]
fn a_reader_that_could_not_announce_still_holds_the_sweeper_off() {
    // The hole the design had before this: a reader whose slot was taken was announced
    // *nowhere*, so when the older pin holding that slot was released first, every slot read
    // free and a sweeper concluded the warehouse was idle --- while that reader was inside.
    //
    // Order matters and is the whole test: the older reader leaves **first**.
    let leases = Leases::with_slots(1);
    let older = leases.pin();
    let unannounced = leases.pin();
    let marked = leases.mark();

    drop(older);
    assert!(
        !leases.drained(marked),
        "the only announced reader left and an unannounced one is still inside"
    );
    drop(unannounced);
    assert!(leases.drained(marked), "and drains once it finishes");
}

#[test]
fn the_oldest_reader_is_the_one_that_counts() {
    let leases = Leases::new();
    let first = leases.pin();
    let second = leases.pin();
    let third = leases.pin();
    assert_eq!(leases.oldest_active(), Some(first.epoch()));

    drop(second);
    assert_eq!(leases.oldest_active(), Some(first.epoch()));
    drop(first);
    assert_eq!(leases.oldest_active(), Some(third.epoch()));
    drop(third);
    assert_eq!(leases.oldest_active(), None);
}

#[test]
fn a_sweeper_never_concludes_drained_while_a_reader_is_inside() {
    // The property, hammered. Readers arrive and leave continuously while a sweeper marks and
    // asks; every time it is told "drained", no reader that started before the mark may still
    // be running. Checked by the readers themselves, so the assertion is on the real
    // interleaving rather than on a snapshot taken afterwards.
    const READERS: usize = 12;
    const ROUNDS: usize = 400;
    // The default slot count, and readers that do a little work while pinned.
    //
    // An earlier version used eight slots and readers that pinned and released in a tight
    // loop. It found a real race --- and once that was fixed it also proved nothing, because
    // with twelve readers spinning against eight slots somebody is *always* mid-pin, so the
    // conservative path blocked every sweep and the run concluded nothing at all.
    //
    // That is worth keeping in view rather than tuning away: it is what starvation looks like,
    // and the small-slot case below still checks that it stays *safe* when it happens. This
    // test checks the other half --- that under an ordinary read load, reclamation actually
    // gets to run.
    let leases = Arc::new(Leases::new());
    let gate = Arc::new(Barrier::new(READERS + 1));
    // The epochs of readers currently inside, kept by the readers themselves.
    //
    // The property cannot be checked with two calls to the registry --- `drained` and then
    // `oldest_active` are two different observations, and a brand-new reader in its pending
    // window makes the second one report the most conservative possible answer. An earlier
    // version did exactly that and counted its own non-atomic pair as a violation.
    //
    // So the readers state what they are doing, and the sweeper checks *that*.
    let inside: Arc<std::sync::Mutex<std::collections::BTreeSet<u64>>> =
        Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (concluded, violated) = std::thread::scope(|scope| {
        for _ in 0..READERS {
            let leases = Arc::clone(&leases);
            let gate = Arc::clone(&gate);
            let inside = Arc::clone(&inside);
            let stop = Arc::clone(&stop);
            scope.spawn(move || {
                gate.wait();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let pin = leases.pin();
                    inside.lock().expect("not poisoned").insert(pin.epoch());
                    // A reader that holds its pin for as long as it takes to acquire it is not
                    // a reader; it is the acquisition. A real one resolves a table and scans
                    // files, so the pinned interval dominates.
                    for _ in 0..200 {
                        std::hint::spin_loop();
                    }
                    inside.lock().expect("not poisoned").remove(&pin.epoch());
                    drop(pin);
                }
            });
        }

        gate.wait();
        let mut concluded = 0usize;
        let mut violated = 0usize;
        // Mark, then check on a *later* pass --- which is how reclamation actually works and
        // what the earlier version of this loop got wrong. Marking and asking in the same
        // instant can essentially never drain while readers are churning, because some reader
        // always pinned a moment before the mark. Deferring the check is not a concession to
        // make the test pass; it is the design: a file is marked when it stops being
        // referenced and deleted on a later tick.
        for _ in 0..ROUNDS {
            let marked = leases.mark();
            let mut settled = false;
            for _ in 0..64 {
                if leases.drained(marked) {
                    settled = true;
                    break;
                }
                std::thread::yield_now();
            }
            if settled {
                concluded += 1;
                let held = inside.lock().expect("not poisoned");
                if held.iter().any(|epoch| *epoch < marked) {
                    violated += 1;
                }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        (concluded, violated)
    });

    assert_eq!(violated, 0, "a sweeper drained while an older reader was inside");
    assert!(concluded > 0, "the sweeper never concluded anything, so this proved nothing");
}

#[test]
fn a_starved_sweeper_is_still_a_safe_one() {
    // The adversarial case, kept deliberately. Twelve readers against four slots means the
    // conservative path is hit constantly and reclamation may make no progress at all.
    //
    // Progress is *not* asserted here, because under this load it genuinely may not happen ---
    // and a design that fails safe by never reclaiming is a disk filling up, which this
    // warehouse has already met once. What is asserted is that being starved never becomes
    // being wrong.
    //
    // The check is on the readers' own record, for the same reason as the test above: asking
    // the registry twice --- `drained`, then `oldest_active` --- observes two different
    // instants, and a reader in its pending window makes the second one answer conservatively.
    // Counting that as a violation made this test fail two runs in five, and the flakiness was
    // entirely mine.
    const READERS: usize = 12;
    const ROUNDS: usize = 400;
    let leases = Arc::new(Leases::with_slots(4));
    let gate = Arc::new(Barrier::new(READERS + 1));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let inside: Arc<std::sync::Mutex<std::collections::BTreeSet<u64>>> =
        Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));

    let violated = std::thread::scope(|scope| {
        for _ in 0..READERS {
            let leases = Arc::clone(&leases);
            let gate = Arc::clone(&gate);
            let stop = Arc::clone(&stop);
            let inside = Arc::clone(&inside);
            scope.spawn(move || {
                gate.wait();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let pin = leases.pin();
                    inside.lock().expect("not poisoned").insert(pin.epoch());
                    std::hint::spin_loop();
                    inside.lock().expect("not poisoned").remove(&pin.epoch());
                    drop(pin);
                }
            });
        }

        gate.wait();
        let mut violated = 0usize;
        for _ in 0..ROUNDS {
            let marked = leases.mark();
            if leases.drained(marked) {
                let held = inside.lock().expect("not poisoned");
                if held.iter().any(|epoch| *epoch < marked) {
                    violated += 1;
                }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        violated
    });

    assert_eq!(violated, 0, "starvation turned into an unsafe conclusion");
}

#[test]
fn a_reader_is_never_invisible_between_starting_and_announcing() {
    // The race that a deferred check cannot see, and the reason this test exists beside the
    // one above rather than inside it.
    //
    // A reader takes an epoch and writes it into a slot. In that order it is invisible in
    // between, and a sweeper that marks and asks inside the window is told the warehouse is
    // idle. Real reclamation marks now and checks seconds later, so no deferring test can see
    // it; the first version of the design had exactly that ordering.
    //
    // **The reader has to be the one that notices.** A version of this test that recorded each
    // reader in a shared set and had the *sweeper* check it could not catch the mutation
    // either, because the reader only reaches the set after `pin` has returned --- long after
    // the window has closed. So instead the sweeper publishes the highest mark it has ever
    // drained, and each reader checks that against its own epoch while it is inside: if a mark
    // above my epoch has already drained, somebody concluded I had finished while I had not.
    const READERS: usize = 12;
    const ROUNDS: usize = 20_000;
    let leases = Arc::new(Leases::new());
    let gate = Arc::new(Barrier::new(READERS + 1));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let highest_drained = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let violations = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    std::thread::scope(|scope| {
        for _ in 0..READERS {
            let leases = Arc::clone(&leases);
            let gate = Arc::clone(&gate);
            let stop = Arc::clone(&stop);
            let highest_drained = Arc::clone(&highest_drained);
            let violations = Arc::clone(&violations);
            scope.spawn(move || {
                gate.wait();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let pin = leases.pin();
                    if highest_drained.load(std::sync::atomic::Ordering::SeqCst) > pin.epoch() {
                        violations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    drop(pin);
                }
            });
        }

        gate.wait();
        for _ in 0..ROUNDS {
            let marked = leases.mark();
            if leases.drained(marked) {
                highest_drained.fetch_max(marked, std::sync::atomic::Ordering::SeqCst);
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    assert_eq!(
        violations.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a sweeper concluded a reader had finished while it was inside"
    );
}
