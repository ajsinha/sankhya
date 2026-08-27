//! The allocator counts what is actually allocated.
//!
//! Installed as the global allocator for this test binary, because an allocator tested
//! through a wrapper that is not actually allocating is testing the wrapper.

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

use sankhya_alloc::Counting;
use std::alloc::System;

#[global_allocator]
static ALLOC: Counting<System> = Counting::new(System);

/// Big enough that the allocator cannot satisfy it from anything it was already holding,
/// and that ambient noise from the test harness is small beside it.
const CHUNK: usize = 8 * 1024 * 1024;

/// Serialises the tests that measure a change in the total.
///
/// A global allocator is global: the test harness runs tests on several threads, and one
/// test's eight-megabyte vector lands in another test's measurement. That is not a
/// defect in the allocator — it is the allocator working — but it makes a delta
/// meaningless unless only one test is producing deltas at a time.
///
/// Tests that assert a *property* rather than a delta do not take it.
static MEASURING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// How far a measurement may drift and still mean what it says.
///
/// Even serialised, the surrounding machinery allocates and frees while a test runs --
/// the mutex guard, the panic hook, the harness itself. The observed drift is tens of
/// bytes. A quarter of a megabyte is far beyond that and far below any accounting error
/// worth catching, which would be off by a factor rather than by a rounding.
const NOISE: usize = 256 * 1024;

fn measuring() -> std::sync::MutexGuard<'static, ()> {
    MEASURING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn an_allocation_is_counted_and_a_release_is_uncounted() {
    let _measuring = measuring();
    let before = ALLOC.in_use();

    let held: Vec<u8> = vec![7u8; CHUNK];
    let during = ALLOC.in_use();
    let moved = during.saturating_sub(before);
    assert!(
        moved.abs_diff(CHUNK) < NOISE,
        "{CHUNK} bytes were allocated and the total moved by {moved}"
    );

    drop(held);
    let after = ALLOC.in_use();
    assert!(
        after < before + CHUNK,
        "the total stayed at {after} after the allocation was released"
    );
}

#[test]
fn growing_a_vector_counts_the_difference_and_not_the_whole_block() {
    let _measuring = measuring();
    // A reallocation that counted the new size again would inflate the total on every
    // vector that grows -- which is every vector -- and the brake would fire on a
    // workload that fits comfortably.
    let before = ALLOC.in_use();

    let mut v: Vec<u8> = Vec::with_capacity(CHUNK);
    v.resize(CHUNK, 1);
    let after_first = ALLOC.in_use() - before;

    // Force a reallocation to roughly double.
    v.reserve_exact(CHUNK);
    v.resize(2 * CHUNK, 1);
    let after_growth = ALLOC.in_use() - before;

    assert!(
        after_growth < after_first * 3,
        "the total went from {after_first} to {after_growth}; a realloc is being counted \
         as a fresh allocation"
    );
    drop(v);
}

#[test]
fn the_peak_remembers_what_the_current_total_forgets() {
    let _measuring = measuring();
    // A limit has to be set against a peak. An average says nothing about whether a
    // workload fits, because the moment it does not fit is a peak.
    ALLOC.reset_peak();
    let baseline = ALLOC.peak();

    {
        let _held: Vec<u8> = vec![0u8; CHUNK];
        assert!(
            ALLOC.peak() + NOISE >= baseline + CHUNK,
            "the peak is {} and {CHUNK} bytes are held above a baseline of {baseline}",
            ALLOC.peak()
        );
    }

    // Released, so the current total has come back down and the peak has not.
    assert!(
        ALLOC.peak() + NOISE >= baseline + CHUNK,
        "the peak fell when memory was released"
    );

    // And it still has not after further allocation, which is where a peak that is
    // merely "the last total recorded" would give itself away. The peak is only ever
    // written on growth, so checking it immediately after a release cannot distinguish a
    // real high-water mark from a stale one.
    let small: Vec<u8> = vec![0u8; 4096];
    std::hint::black_box(&small);
    assert!(
        ALLOC.peak() + NOISE >= baseline + CHUNK,
        "the peak was overwritten by a smaller total: {} against a high-water mark of \
         at least {}",
        ALLOC.peak(),
        baseline + CHUNK
    );
}

#[test]
fn resetting_the_peak_keeps_the_current_total() {
    let _measuring = measuring();
    let _held: Vec<u8> = vec![0u8; CHUNK];
    ALLOC.reset_peak();
    assert!(
        ALLOC.peak() >= ALLOC.in_use(),
        "the peak was reset below what is currently allocated"
    );
}

#[test]
fn the_count_does_not_wrap_when_releases_outnumber_allocations() {
    // Underflow would report a colossal total and trip the brake permanently. A count
    // that drifts low degrades the brake; a count that wraps disables the system.
    //
    // Exercised by churning, which is where a mismatched pair would show up.
    for _ in 0..200 {
        let v: Vec<u8> = vec![3u8; 64 * 1024];
        std::hint::black_box(&v);
    }
    assert!(
        ALLOC.in_use() < usize::MAX / 2,
        "the total is {}, which means it wrapped",
        ALLOC.in_use()
    );
}

#[test]
fn allocations_from_several_threads_are_all_counted() {
    let _measuring = measuring();
    // The counters are shared and the allocator is called from everywhere. A count that
    // lost updates under contention would understate exactly when the system is busiest.
    const THREADS: usize = 8;
    const EACH: usize = 1024 * 1024;

    ALLOC.reset_peak();
    let before = ALLOC.in_use();

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            std::thread::spawn(|| {
                let v: Vec<u8> = vec![1u8; EACH];
                // Hold it while the others allocate, so the peak sees all of them.
                std::thread::sleep(std::time::Duration::from_millis(20));
                std::hint::black_box(v.len())
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("a thread panicked");
    }

    assert!(
        ALLOC.peak() + NOISE >= before + THREADS * EACH,
        "peak was {} and {THREADS} threads each held {EACH} bytes",
        ALLOC.peak().saturating_sub(before)
    );
}
