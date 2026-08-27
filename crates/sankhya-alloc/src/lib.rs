//! The counting allocator, and the only unsafe code in this system.
//!
//! # Why this is its own crate
//!
//! The workspace forbids unsafe code, and `forbid` cannot be relaxed locally — which is
//! the reason it is used rather than `deny`. So the one place unsafe is unavoidable is a
//! crate small enough to read in a sitting, and every other crate keeps the stronger
//! rule. A lint exception buried in a larger crate would be the same permission with
//! none of the visibility.
//!
//! # Why the query engine's own accounting is not enough
//!
//! The engine tracks the memory its operators reserve. That is most of what a query uses
//! and it is not all of it: decode buffers, network buffers, graph arenas and every
//! third-party allocation sit outside the pool. A query can stay within its reservation
//! and still exhaust the machine.
//!
//! Counting at the allocator catches all of it, because there is nowhere else for memory
//! to come from.
//!
//! # What this deliberately does not do
//!
//! It does not attribute memory to a query. A global allocator sees allocations, not
//! reasons, and threading a query identity through every allocation site is a cost paid
//! on every allocation to answer a question asked rarely. Attribution belongs to the
//! engine's own pool; this is the backstop that catches what the pool cannot see.

#![doc(html_root_url = "https://docs.rs/sankhya-alloc")]
// The whole reason this crate exists. `GlobalAlloc` cannot be implemented safely, and
// the alternative to implementing it is not knowing how much memory is in use.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};

/// An allocator that counts.
///
/// Wraps another allocator rather than replacing it: the counting is the point, and
/// writing an allocator is not.
///
/// Install it in a binary, never in a library — which allocator a program uses is the
/// program's decision, and a library that made it would take it away from every user.
pub struct Counting<A> {
    inner: A,
    in_use: AtomicUsize,
    peak: AtomicUsize,
}

impl<A> std::fmt::Debug for Counting<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The wrapped allocator is deliberately not shown: it is almost always the
        // system allocator and formatting it says nothing, while the two numbers are the
        // entire reason to look.
        f.debug_struct("Counting")
            .field("in_use", &self.in_use())
            .field("peak", &self.peak())
            .finish_non_exhaustive()
    }
}

impl<A> Counting<A> {
    pub const fn new(inner: A) -> Self {
        Self {
            inner,
            in_use: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    /// Bytes currently allocated.
    pub fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Relaxed)
    }

    /// The highest the total has ever been.
    ///
    /// What a limit has to be set against: an average tells you nothing about whether a
    /// workload fits, because the moment it does not fit is a peak.
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    /// Forget the peak, keeping the current total.
    ///
    /// For measuring one phase without the previous one's high-water mark.
    pub fn reset_peak(&self) {
        self.peak.store(self.in_use(), Ordering::Relaxed);
    }

    fn record_growth(&self, bytes: usize) {
        let now = self.in_use.fetch_add(bytes, Ordering::Relaxed) + bytes;
        // A plain compare-and-set loop rather than a max: the peak is read rarely and
        // written often, so contention here is the common case and a failed exchange
        // just means somebody else recorded a higher one.
        let mut seen = self.peak.load(Ordering::Relaxed);
        while now > seen {
            match self
                .peak
                .compare_exchange_weak(seen, now, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => seen = actual,
            }
        }
    }

    fn record_release(&self, bytes: usize) {
        // Saturating, because an underflow would report a colossal total and trip the
        // brake permanently. A count that drifts low degrades the brake; a count that
        // wraps disables the system.
        let mut current = self.in_use.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(bytes);
            match self.in_use.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }
}

// Safety: every method delegates to the wrapped allocator with the same arguments, and
// the counters are ordinary atomics that do not allocate. Nothing here can recurse into
// the allocator, which is the one thing a global allocator must not do.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { self.inner.alloc(layout) };
        if !ptr.is_null() {
            self.record_growth(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.inner.dealloc(ptr, layout) };
        self.record_release(layout.size());
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        if !ptr.is_null() {
            self.record_growth(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { self.inner.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            // The difference, not the new size. Counting the whole block again would
            // inflate the total on every vector that grows.
            if new_size >= layout.size() {
                self.record_growth(new_size - layout.size());
            } else {
                self.record_release(layout.size() - new_size);
            }
        }
        new_ptr
    }
}
