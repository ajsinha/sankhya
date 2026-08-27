//! Stopping work, and stopping it within a stated time.
//!
//! # Why "cancellable" is not the property that matters
//!
//! Almost everything is cancellable if you wait long enough. The property worth having
//! is that cancellation takes effect **within a bound you can state** — because the
//! reason to cancel is usually that something else needs the resources now, and a query
//! that stops eventually is holding them until it does.
//!
//! So the bound is not a hope about how the code is written. It is a number: work is
//! done in units, the token is checked every *N* units, and the worst case is one unit
//! plus the check. Stating it that way makes it testable, and makes "we should check
//! more often" a measurement rather than an opinion.
//!
//! # Why a deadline and a cancellation are different things
//!
//! Both stop the work; they mean different things to whoever asked for it. A deadline
//! passing says the answer arrived too late to be useful, and the same query might
//! succeed with a longer one. An explicit cancellation says nobody wants the answer any
//! more. A client that cannot tell them apart cannot decide whether to retry, so they
//! are separate variants rather than one "stopped" error.
//!
//! # Why time is a parameter
//!
//! The clock is passed in rather than read. A deadline that reads the wall clock cannot
//! be tested without sleeping, and a test that sleeps is a test nobody runs in a loop.
//! The production wrapper reads a monotonic clock once and passes the tick down.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// When a query must be finished.
///
/// Ticks are whatever unit the caller counts in; the type does not care, and the
/// production wrapper uses milliseconds since the query started.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Deadline {
    at: u64,
}

impl Deadline {
    #[must_use]
    pub const fn at(tick: u64) -> Self {
        Self { at: tick }
    }

    /// A deadline `after` ticks from `now`.
    #[must_use]
    pub const fn after(now: u64, ticks: u64) -> Self {
        Self {
            at: now.saturating_add(ticks),
        }
    }

    /// A deadline that never passes.
    ///
    /// Named rather than expressed as `None`, so a caller that genuinely wants unbounded
    /// work has to say so. An `Option<Deadline>` invites forgetting to set one.
    #[must_use]
    pub const fn never() -> Self {
        Self { at: u64::MAX }
    }

    #[must_use]
    pub const fn expired_at(self, now: u64) -> bool {
        now >= self.at
    }

    #[must_use]
    pub const fn remaining(self, now: u64) -> u64 {
        self.at.saturating_sub(now)
    }
}

/// Why work stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stopped {
    /// The deadline passed.
    ///
    /// The answer would have arrived too late to be useful. The same query may succeed
    /// with a longer deadline, which is the thing a client needs to know.
    DeadlineExceeded { at: u64, now: u64 },
    /// Someone asked for it to stop.
    ///
    /// Nobody wants the answer. Retrying is pointless unless whatever cancelled it has
    /// changed its mind.
    Cancelled,
}

impl Stopped {
    /// Whether the same work submitted again could succeed unchanged.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::DeadlineExceeded { .. })
    }
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeadlineExceeded { at, now } => write!(
                f,
                "the deadline was {at} and it is now {now}; the answer would have \
                 arrived too late to be useful, and the same query may succeed with a \
                 longer deadline"
            ),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for Stopped {}

/// A shared, one-way switch.
///
/// Cloning shares the switch; there is no way to un-cancel. That asymmetry is
/// deliberate: work that observed a cancellation may already have released resources or
/// discarded partial state, so a token that could be reset would let a caller resume
/// something that is no longer resumable.
#[derive(Clone, Debug, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
}

impl Cancel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask everything holding this token to stop.
    ///
    /// Idempotent, and safe from any thread.
    pub fn cancel(&self) {
        // Release, so anything the canceller wrote before this is visible to a worker
        // that sees the flag.
        self.flag.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

/// A deadline and a cancellation together, with a stated checking interval.
#[derive(Clone, Debug)]
pub struct Budget {
    deadline: Deadline,
    cancel: Cancel,
    /// Units of work between checks.
    ///
    /// This is the bound. Checking every unit would be exact and would cost an atomic
    /// load per row; checking rarely is cheap and slow to stop. The number is a
    /// deliberate compromise, and naming it here is what makes the bound something a
    /// test can assert rather than something a reader has to infer from a loop.
    check_every: u64,
}

impl Budget {
    #[must_use]
    pub fn new(deadline: Deadline, cancel: Cancel, check_every: u64) -> Self {
        Self {
            deadline,
            cancel,
            // Zero would mean never checking, which is the one value that must not be
            // expressible.
            check_every: check_every.max(1),
        }
    }

    /// A budget with no deadline, for work that must run to completion.
    #[must_use]
    pub fn unbounded() -> Self {
        Self::new(Deadline::never(), Cancel::new(), 1)
    }

    #[must_use]
    pub const fn check_every(&self) -> u64 {
        self.check_every
    }

    #[must_use]
    pub const fn deadline(&self) -> Deadline {
        self.deadline
    }

    #[must_use]
    pub fn cancel_token(&self) -> Cancel {
        self.cancel.clone()
    }

    /// Whether work should stop, checked unconditionally.
    ///
    /// # Errors
    ///
    /// Returns [`Stopped`] when the deadline has passed or the token is set. Cancellation
    /// is reported in preference to the deadline when both apply: an explicit
    /// cancellation is a decision somebody made, and reporting a deadline instead would
    /// tell them to retry something they deliberately stopped.
    pub fn check(&self, now: u64) -> Result<(), Stopped> {
        if self.cancel.is_cancelled() {
            return Err(Stopped::Cancelled);
        }
        if self.deadline.expired_at(now) {
            return Err(Stopped::DeadlineExceeded {
                at: self.deadline.at,
                now,
            });
        }
        Ok(())
    }

    /// Whether work should stop, checked only every `check_every` units.
    ///
    /// `units_done` is the caller's own counter — rows, batches, traversal steps. The
    /// worst case between a cancellation and its observation is `check_every` units plus
    /// however long one unit takes, and that is the whole of the bound.
    ///
    /// # Errors
    ///
    /// As [`check`](Self::check), on the units where it looks.
    pub fn check_periodically(&self, units_done: u64, now: u64) -> Result<(), Stopped> {
        if units_done % self.check_every != 0 {
            return Ok(());
        }
        self.check(now)
    }
}
