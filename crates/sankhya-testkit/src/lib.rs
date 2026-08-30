//! Provoking a race on purpose, and being able to do it again.
//!
//! # Why this crate exists, stated as the defect it answers
//!
//! Four concurrency defects were found in this system on 2026-08-28 and 2026-08-29, and every
//! one had been invisible to seventeen hundred tests for the same reason: **every test had a
//! single writer.** A commit that is only ever made by one thread cannot lose a race, and a
//! file that is only ever read by one reader cannot be deleted underneath another.
//!
//! Writing the multi-threaded tests that found them exposed a second problem. The harness is
//! the same every time --- N threads, released together, results collected --- and writing it
//! by hand four times produced the same mistakes four times. One of them **hung instead of
//! failing**, because an assertion inside `std::thread::scope` left twelve threads spinning on
//! a stop flag nobody would ever set.
//!
//! # Luck is not evidence
//!
//! A race test that runs once has observed one interleaving. Run it enough and the bad one
//! happens --- but *enough* is not knowable in advance, and a test that passes by luck is
//! indistinguishable from one that passes because the code is right.
//!
//! So [`Hammer`] does two things a hand-rolled loop does not. It **oversubscribes**: more
//! threads than the machine has cores, so the scheduler preempts inside the windows a race
//! lives in rather than between them. And it **jitters deterministically** --- delays come from
//! a seeded generator, so a run that fails can be replayed exactly by its seed.
//!
//! That second property is the whole point. "We could not reproduce it" is what turns a
//! concurrency bug into a permanent resident, and a seed turns the same bug into a fixture.
//!
//! # The floor, measured rather than assumed
//!
//! Oversubscription and jitter widen the interleavings a run visits. They do not make an
//! **arbitrarily narrow** window reachable, and it is worth knowing where that stops.
//!
//! Pointed at `sankhya-leases`, this harness catches every defect whose window is a lock, a
//! syscall or a scan. It does **not** catch the one whose window is two instructions --- taking
//! an epoch before counting a reader --- and no amount of oversubscription changes that,
//! because the other participant's work does not fit inside the window however often the first
//! is descheduled.
//!
//! That is a limit of the technique, not of the effort spent on it. Provoking a window that
//! narrow needs a scheduler somebody controls rather than one under pressure, which is what
//! `loom` does: it explores interleavings exhaustively, substituting its own atomics under a
//! `cfg` so nothing ships carrying a hook. Adopting it is the next step for that class, and
//! saying so here is better than leaving a reader to conclude this harness covers everything.
//!
//! # What this must not become
//!
//! A place for helpers that build fixtures the product could build itself. The golden rule
//! stands --- no server functionality in test code --- and a testkit is the most tempting place
//! to break it, because breaking it there looks like sharing rather than duplicating.
//!
//! Nothing here knows what a warehouse, a commit or a cuboid is, and nothing here should.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

/// A deterministic source of small delays.
///
/// Seeded, and the seed is the reason this exists rather than `rand`: a race that fails under
/// one interleaving is worth nothing if the next run takes a different one. Printing the seed
/// on failure and passing it back reproduces the exact schedule.
///
/// The generator is a plain xorshift. It is not for cryptography and is not pretending to be;
/// it is for choosing between "yield now" and "spin a little" reproducibly.
#[derive(Debug)]
pub struct Jitter {
    state: AtomicU64,
}

impl Jitter {
    /// A jitter source with this seed.
    #[must_use]
    pub const fn seeded(seed: u64) -> Self {
        // Zero would leave xorshift stuck at zero for ever, which would silently turn every
        // delay into the same delay --- the opposite of what this is for.
        Self {
            state: AtomicU64::new(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed }),
        }
    }

    /// The next value in the sequence.
    pub fn next(&self) -> u64 {
        let mut x = self.state.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state.store(x, Ordering::Relaxed);
        x
    }

    /// Pause for a short, varying moment.
    ///
    /// Sometimes a yield, sometimes a few spins, sometimes nothing. The mixture matters: a
    /// yield lets another thread run and a spin keeps this one on the processor, and a race
    /// that only appears under one of them is missed by a harness that only does the other.
    pub fn pause(&self) {
        match self.next() % 4 {
            0 => std::thread::yield_now(),
            1 => {
                for _ in 0..(self.next() % 64) {
                    std::hint::spin_loop();
                }
            }
            2 => std::hint::spin_loop(),
            _ => {}
        }
    }
}

/// What a hammered run produced.
#[derive(Debug)]
pub struct Hammered<T> {
    /// What each worker returned, in worker order.
    pub outcomes: Vec<T>,
    /// The seed the run used, so a failure can be repeated.
    pub seed: u64,
}

impl<T> Hammered<T> {
    /// A message naming the seed, for an assertion that fails.
    ///
    /// Included in every failure this crate encourages, because a concurrency failure without
    /// its seed is a bug report saying "sometimes".
    #[must_use]
    pub fn replay_with(&self) -> String {
        format!("replay this exact schedule with seed {}", self.seed)
    }
}

/// Run one closure on many threads, released together.
///
/// # What it does that a hand-rolled loop does not
///
/// Threads are **released by a barrier**, so they are genuinely in the window at once rather
/// than merely started at similar times. The count **oversubscribes** the machine by default,
/// so the scheduler preempts inside short windows instead of between whole runs. And the
/// results come back *after* every thread has joined, which is what stops an assertion from
/// running while the other threads are still spinning --- the mistake that turned one test in
/// this repository into a hang rather than a failure.
#[derive(Debug)]
pub struct Hammer {
    workers: usize,
    rounds: usize,
    seed: u64,
}

impl Default for Hammer {
    fn default() -> Self {
        Self::new()
    }
}

impl Hammer {
    /// A hammer that oversubscribes the machine.
    #[must_use]
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
        Self {
            // Four times the cores. Enough that the scheduler must preempt to make progress,
            // which is what puts a thread to sleep in the middle of a two-instruction window.
            workers: cores.saturating_mul(4).max(8),
            rounds: 1,
            seed: 0x5DEE_CE66_D53A_1F27,
        }
    }

    /// Run with exactly this many workers.
    #[must_use]
    pub const fn workers(mut self, workers: usize) -> Self {
        self.workers = workers;
        self
    }

    /// Repeat the whole contended run this many times.
    ///
    /// Rounds are not the same as workers. More workers widens one interleaving; more rounds
    /// samples more of them, and a race whose window is narrow needs the second.
    #[must_use]
    pub const fn rounds(mut self, rounds: usize) -> Self {
        self.rounds = rounds;
        self
    }

    /// Use this seed, so a failing run can be repeated exactly.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// How many workers this will run.
    #[must_use]
    pub const fn worker_count(&self) -> usize {
        self.workers
    }

    /// Run `work` on every worker, once per round, and collect what the last round returned.
    ///
    /// `work` is given its worker index and a jitter source of its own. Each worker's jitter is
    /// seeded from the run's seed and the worker index, so the whole schedule is a function of
    /// one number.
    pub fn run<T, F>(&self, work: F) -> Hammered<T>
    where
        T: Send,
        F: Fn(usize, &Jitter) -> T + Sync,
    {
        let mut outcomes = Vec::new();
        for round in 0..self.rounds.max(1) {
            let gate = Arc::new(Barrier::new(self.workers));
            let work = &work;
            outcomes = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..self.workers)
                    .map(|worker| {
                        let gate = Arc::clone(&gate);
                        let seed = self
                            .seed
                            .wrapping_add((round as u64).wrapping_mul(0x9E37_79B9))
                            .wrapping_add(worker as u64);
                        scope.spawn(move || {
                            let jitter = Jitter::seeded(seed);
                            gate.wait();
                            work(worker, &jitter)
                        })
                    })
                    .collect();
                // Joined before anything is asserted. An assertion between the spawn and the
                // join leaves the other threads running with nobody to stop them.
                handles
                    .into_iter()
                    .filter_map(|handle| handle.join().ok())
                    .collect()
            });
        }
        Hammered {
            outcomes,
            seed: self.seed,
        }
    }
}

/// A flag that stops a set of workers, and cannot be forgotten.
///
/// The hang this prevents is specific and was real: a loop of `while !stop.load()` paired with
/// an assertion that fires *before* `stop.store(true)` leaves every worker spinning for ever,
/// and the test hangs rather than failing. Holding the flag in a guard that sets it on drop
/// means an early return or a panic still releases the workers.
#[derive(Debug)]
pub struct Until {
    stop: Arc<AtomicBool>,
}

impl Default for Until {
    fn default() -> Self {
        Self::new()
    }
}

impl Until {
    /// A flag that is not yet set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A handle workers check.
    #[must_use]
    pub fn handle(&self) -> Running {
        Running {
            stop: Arc::clone(&self.stop),
        }
    }

    /// Stop the workers now.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Until {
    fn drop(&mut self) {
        // Set on the way out, whatever the way out was. A panic between starting the workers
        // and stopping them would otherwise hang the test instead of failing it.
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A worker's view of whether it should keep going.
#[derive(Clone, Debug)]
pub struct Running {
    stop: Arc<AtomicBool>,
}

impl Running {
    /// Whether the work should continue.
    #[must_use]
    pub fn keep_going(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }
}
