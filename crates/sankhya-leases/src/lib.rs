//! Nothing is deleted while somebody is reading it, and no reader ever waits to say so.
//!
//! # The problem this exists for
//!
//! A query resolves a table into a set of files and then reads them. Maintenance --- compaction,
//! orphan sweeping, cuboid retirement --- deletes files that the current log no longer
//! references. Between the resolve and the read there is a window, and a file deleted inside it
//! produces an error naming a path the caller never mentioned.
//!
//! Until this crate, three reclamation paths guarded that window with elapsed **ticks**,
//! elapsed **seconds** and elapsed **table versions**. Each is a proxy for "a reader might still
//! be holding this", each is documented as one, and version-space is the weakest of the three:
//! under continuous ingest a hundred versions can pass in seconds while an analytical scan runs
//! for minutes. The failure was observed during M7.
//!
//! # Why epochs and not a lock, a set, or a reference count
//!
//! The obvious implementation is a shared set of the files currently being read: a reader
//! inserts, a sweeper checks. It is correct and it is a **global mutex on the read path** ---
//! a GIL with a filesystem accent. It would satisfy every safety requirement and destroy the
//! thing the safety is for.
//!
//! So readers do not register *what* they hold. They announce *when* they started, into a slot
//! nobody else writes, with one relaxed atomic store. A sweeper reads every slot and takes the
//! oldest announcement. That is the whole mechanism:
//!
//! - a reader never takes a lock, never allocates, and never waits for a sweeper;
//! - a sweeper never waits for a reader --- it defers a deletion and returns;
//! - the cost of a reader is one store on entry and one on exit;
//! - the cost of a sweep is a scan of a fixed array, whatever the warehouse holds.
//!
//! # Why "when" is enough to be safe
//!
//! A reader pins **before** it resolves, so every file it can learn about is one the log named
//! after its pin began. A sweeper decides to delete a file only once the log no longer
//! references it. So the rule is:
//!
//! > A file that stopped being referenced at epoch *e* may be deleted once every reader that
//! > pinned before *e* has finished.
//!
//! A reader that pins after *e* resolves a log that does not name the file, so it cannot ask
//! for it. A reader that pinned before *e* might, and is waited for. Nothing needs to know
//! which files which reader holds --- which is what keeps the read path free of a registry.
//!
//! # What it costs to be wrong in each direction
//!
//! Slots are a fixed pool, so two readers can share one. Sharing makes a slot report the older
//! of the two announcements, which **delays** a deletion and never permits one. Every
//! imprecision in this crate is arranged to fall that way: a late reclamation costs disk, and
//! an early one costs a query.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// How many readers can announce independently before two share a slot.
///
/// Sharing is safe --- a shared slot reports the older announcement, which delays reclamation
/// and never permits it --- so this is a throughput figure, not a correctness one. Chosen well
/// above any plausible concurrent-query count so that sharing is rare rather than impossible.
pub const SLOTS: usize = 512;

/// Nothing is pinned in this slot.
const FREE: u64 = 0;

/// A slot, padded so that two readers announcing at once do not share a cache line.
///
/// Without the padding, sixty-four slots live in one line and every announcement invalidates
/// every neighbour's copy of it --- the readers do not block each other, and the hardware makes
/// them wait anyway. False sharing is the way a lock-free design quietly becomes a slow one.
#[repr(align(64))]
#[derive(Debug, Default)]
struct Slot(AtomicU64);

/// The epochs readers have announced, and the clock that orders them.
#[derive(Debug)]
pub struct Leases {
    /// Monotonic, and never reset. Starts at 1 so that zero can mean "free".
    now: AtomicU64,
    slots: Vec<Slot>,
    /// Readers that are inside and whose slot was already taken.
    ///
    /// **Without this the design is unsound**, and the unsoundness is quiet. A reader whose
    /// slot is occupied is not announced anywhere; if the older pin holding that slot is
    /// released while this reader is still inside, every slot reads free and a sweeper
    /// concludes the warehouse is idle. It would then delete a file being read.
    ///
    /// So an unannounced reader is counted, and while the count is non-zero nothing drains at
    /// all. That is maximally conservative --- one unlucky reader stalls all reclamation until
    /// it finishes --- and it is the right trade: with the default slot count a collision needs
    /// hundreds of concurrent queries, and the cost of being wrong the other way is a query
    /// failing on a file that vanished.
    unannounced: AtomicUsize,
}

impl Default for Leases {
    fn default() -> Self {
        Self::new()
    }
}

impl Leases {
    /// A registry with [`SLOTS`] independent slots.
    #[must_use]
    pub fn new() -> Self {
        Self::with_slots(SLOTS)
    }

    /// A registry with `slots` independent slots, for tests that want contention on purpose.
    #[must_use]
    pub fn with_slots(slots: usize) -> Self {
        Self {
            now: AtomicU64::new(1),
            slots: (0..slots.max(1)).map(|_| Slot::default()).collect(),
            unannounced: AtomicUsize::new(0),
        }
    }

    /// The current epoch, and the value a sweeper records against a file it wants to delete.
    ///
    /// Advances on every call, so two sweeps never share a deadline and a file marked by the
    /// later one is never released by the earlier one's readers draining.
    pub fn mark(&self) -> u64 {
        self.now.fetch_add(1, Ordering::SeqCst)
    }

    /// Announce that a reader is starting, and stop announcing when the guard is dropped.
    ///
    /// One relaxed-ordering store on entry and one on exit. There is no lock, no allocation and
    /// nothing that can fail, which is deliberate: a read path that can fail to pin has to
    /// decide what to do about it, and every available answer is worse than the problem.
    pub fn pin(&self) -> Pin<'_> {
        // **Counted before the epoch is taken, and this ordering is the whole of the
        // correctness.**
        //
        // Taking the epoch first leaves a window --- between `fetch_add` returning and the slot
        // being written --- in which a reader exists and is announced nowhere. A sweeper that
        // marks and asks inside that window is told the warehouse is idle, and deletes a file
        // the reader is about to open. The window is microseconds and the gap between a mark
        // and its drain check is seconds, so it is rare; a hammer test found it in twenty of
        // four hundred rounds, which is exactly the kind of odds that reaches production and
        // not a test suite.
        //
        // Counting first means a reader is conservative from the instant it begins.
        //
        // **This ordering is argued, not tested, and that is worth admitting.** Swapping these
        // two lines is a real defect and no test here catches it: the window it opens is two
        // instructions wide, while `drained` scans hundreds of slots, so a sweeper cannot fit
        // inside it however hard a test tries. It becomes reachable only when a reader is
        // preempted at exactly that point, which is a scheduler event and not something a
        // hammer loop can provoke.
        //
        // The mutation catalogue therefore does *not* carry an entry for it --- an entry whose
        // mutation survives is a claim of coverage that does not exist. Provoking it needs
        // deterministic preemption between two atomics, which is what `sankhya-testkit` is for
        // (M8 §12.1e), and this is the first thing that should be pointed at it.
        self.unannounced.fetch_add(1, Ordering::SeqCst);
        let at = self.now.fetch_add(1, Ordering::SeqCst);
        // A slot chosen by address rather than by search. Two readers can collide, and a
        // collision only ever delays a deletion --- so spending a scan to avoid one would be
        // paying on the hot path to save on the cold one.
        let index = Self::slot_for(at, self.slots.len());
        // Claimed if free; otherwise this reader is *counted* rather than announced. Keeping
        // the older announcement is right --- it is the conservative one --- but leaving the
        // younger reader untracked is not, because the older pin can be released first.
        let announced = self.slots.get(index).is_some_and(|slot| {
            slot.0
                .compare_exchange(FREE, at, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
        });
        if announced {
            // Announced in a slot, so it no longer needs the blanket count.
            self.unannounced.fetch_sub(1, Ordering::SeqCst);
        }
        Pin { leases: self, index, at, announced }
    }

    /// The oldest epoch any reader is still inside, or `None` when nobody is reading.
    #[must_use]
    pub fn oldest_active(&self) -> Option<u64> {
        // A reader that could not announce is older than any mark a sweeper will take next, so
        // it is reported as the earliest possible epoch and nothing drains while it is inside.
        if self.unannounced.load(Ordering::SeqCst) > 0 {
            return Some(0);
        }
        self.slots
            .iter()
            .map(|slot| slot.0.load(Ordering::SeqCst))
            .filter(|announced| *announced != FREE)
            .min()
    }

    /// Whether every reader that started before `marked` has finished.
    ///
    /// The one question a sweeper asks. `true` means a file that stopped being referenced at
    /// `marked` is now unreachable by anybody: a reader that started earlier has gone, and a
    /// reader that started later resolved a log that does not name it.
    #[must_use]
    pub fn drained(&self, marked: u64) -> bool {
        self.oldest_active().is_none_or(|oldest| oldest >= marked)
    }

    /// Which slot an epoch announces into.
    ///
    /// Taken modulo the **actual** slot count, not [`SLOTS`]. The first version of this used
    /// the constant, so every registry built with a different size indexed past its own array
    /// --- caught immediately by the tests that build small registries on purpose to force
    /// collisions, which is what those tests are for.
    fn slot_for(at: u64, slots: usize) -> usize {
        // Multiplicative hashing on the epoch itself: consecutive readers land far apart, which
        // is what keeps two simultaneous readers off one slot without a search.
        let scattered = at.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        usize::try_from(scattered >> 32).unwrap_or(0) % slots.max(1)
    }
}

/// A reader's announcement, released on drop.
#[derive(Debug)]
pub struct Pin<'a> {
    leases: &'a Leases,
    index: usize,
    at: u64,
    announced: bool,
}

impl Pin<'_> {
    /// The epoch this reader announced.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.at
    }
}

impl Drop for Pin<'_> {
    fn drop(&mut self) {
        if self.announced {
            // Released only if this pin is the one the slot is holding. Clearing somebody
            // else's announcement would report a reader as finished while it is still reading,
            // which is the one error this crate exists to make impossible.
            if let Some(slot) = self.leases.slots.get(self.index) {
                let _ = slot
                    .0
                    .compare_exchange(self.at, FREE, Ordering::SeqCst, Ordering::Relaxed);
            }
        } else {
            self.leases.unannounced.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
