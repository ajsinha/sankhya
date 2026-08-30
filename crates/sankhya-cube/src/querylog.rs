//! What people actually ask a cube for.
//!
//! # Why selection needs this and cannot be written without it
//!
//! §11.6's greedy selection spends an operator's storage budget on the cuboids worth holding,
//! and [`sankhya_cube_algo::lattice::select`] says what it needs in its own signature:
//! *"`queries` is what the selection is for — the cuboids people actually ask for, from a
//! query log. Selecting against the whole lattice instead optimises for queries nobody runs,
//! which is the same mistake as a person guessing, made faster."*
//!
//! The algorithm has existed and been tested since M7 began. Nothing recorded the signal, so
//! nothing could run it — and building the selector against a fabricated signal would have
//! repeated the error M7 already made once, when eight exit criteria passed while every one
//! supplied its own cells.
//!
//! # What is recorded, and what is deliberately not
//!
//! A **shape**: which cube, and which dimensions were grouped by. Not a value, not a member,
//! not a predicate, not a principal. `by=region` records `region` — the name of a column in a
//! definition anybody who may read the cube can already list with `cube_dimensions`.
//!
//! That is worth stating because a query log is the kind of thing that quietly becomes a
//! record of who asked what about whom. This one cannot: there is nowhere in it to put a
//! member, and no field for who was asking.
//!
//! # Why a ring, and why the repetition is the point
//!
//! The log holds the last *n* asks and forgets the rest. Two reasons, and the second is the
//! one that makes the code simple.
//!
//! **Bounded, because an unbounded log is the failure this warehouse keeps finding.** A
//! structure that grows once per query and is never trimmed is a leak with a business
//! justification.
//!
//! **And a ring is already the weighting.** `select` takes a slice of cuboids, so a cuboid
//! asked for ten times appears ten times and counts ten times — frequency needs no separate
//! tally, and recency falls out of old entries being overwritten. A dashboard nobody has
//! opened for a week stops pinning storage without anybody deciding it should.

use sankhya_cube_algo::lattice::Cuboid;
use std::collections::BTreeMap;
use std::sync::Arc;

/// How many asks are remembered per cube.
///
/// Enough that a handful of distinct shapes are all represented several times over, and small
/// enough that a cube nobody has queried today is described by what happened today rather than
/// by what happened last month.
pub const REMEMBERED: usize = 256;

/// The cuboids recently asked for, per cube.
///
/// # Why the map lock is only ever taken for a lookup
///
/// `record` runs on **every** cube query. Holding a write lock over the whole map to push one
/// entry made every cube's navigation serialize against every other cube's --- a choke point
/// on the hot path, and one no correctness test could see, because the answers were right.
///
/// Each cube's ring is behind its own small lock instead. Recording takes a *read* lock on the
/// map to find the ring, releases it, and takes the ring's own lock for the length of a push.
/// Two cubes never meet; two queries against one cube contend for a mutex held over a handful
/// of pointer writes.
///
/// The write lock is still taken the first time a cube is seen, which happens once.
#[derive(Debug, Default)]
pub struct QueryLog {
    asks: parking_lot::RwLock<BTreeMap<String, Arc<parking_lot::Mutex<Ring>>>>,
    /// How many asks are kept per cube.
    capacity: usize,
}

/// A bounded, oldest-first record of one cube's asks.
#[derive(Clone, Debug, Default)]
struct Ring {
    entries: Vec<Cuboid>,
    next: usize,
}

impl Ring {
    fn record(&mut self, cuboid: Cuboid, capacity: usize) {
        if self.entries.len() < capacity {
            self.entries.push(cuboid);
            return;
        }
        // Overwrite in place, oldest first. The wrap is what makes this forget, and
        // forgetting is what keeps yesterday's dashboard from pinning storage forever.
        if let Some(slot) = self.entries.get_mut(self.next) {
            *slot = cuboid;
        }
        self.next = (self.next + 1) % capacity.max(1);
    }
}

impl QueryLog {
    /// A log remembering [`REMEMBERED`] asks per cube.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(REMEMBERED)
    }

    /// A log remembering `capacity` asks per cube.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            asks: parking_lot::RwLock::new(BTreeMap::new()),
            capacity: capacity.max(1),
        }
    }

    /// Record that somebody asked this cube for this shape.
    pub fn record(&self, cube: &str, cuboid: Cuboid) {
        // The common path: a read lock, a lookup, and out.
        //
        // **Bound to a `let` before the ring is locked, and that is not a style preference.**
        // In edition 2021 a temporary in an `if let` scrutinee lives until the end of the whole
        // `if let` --- so writing this as `if let Some(ring) = self.asks.read()...` holds the
        // map's read lock across `ring.lock()`. The first version of this did exactly that,
        // under a comment claiming the opposite.
        //
        // Two costs, and the second is the one that matters. The map cannot be written while
        // any cube is recording, which is the contention this change existed to remove. And it
        // establishes a nested order --- map before ring --- that nothing else must ever
        // reverse. No path does today; every path that might is a deadlock waiting for the
        // load that makes it likely.
        let existing = self.asks.read().get(cube).map(Arc::clone);
        if let Some(ring) = existing {
            ring.lock().record(cuboid, self.capacity);
            return;
        }
        // First sight of this cube. Taken under a write lock, and re-checked because another
        // thread may have inserted it between the read above and this write.
        // Also bound first, for the same reason: `Arc::clone(self.asks.write().entry(..))`
        // keeps the write guard alive across the `ring.lock()` that follows it.
        let ring = {
            let mut asks = self.asks.write();
            Arc::clone(
                asks.entry(cube.to_string())
                    .or_insert_with(|| Arc::new(parking_lot::Mutex::new(Ring::default()))),
            )
        };
        ring.lock().record(cuboid, self.capacity);
    }

    /// What this cube has been asked for, most-asked shapes appearing most often.
    ///
    /// Returned with repetition rather than as a set, because that repetition *is* the
    /// weighting `select` reads: a shape asked ten times counts ten times.
    #[must_use]
    pub fn asked(&self, cube: &str) -> Vec<Cuboid> {
        let ring = self.asks.read().get(cube).map(Arc::clone);
        ring.map(|ring| ring.lock().entries.clone()).unwrap_or_default()
    }

    /// Which cubes have been asked about at all.
    #[must_use]
    pub fn cubes(&self) -> Vec<String> {
        self.asks.read().keys().cloned().collect()
    }

    /// How many asks are held for a cube.
    #[must_use]
    pub fn len(&self, cube: &str) -> usize {
        let ring = self.asks.read().get(cube).map(Arc::clone);
        ring.map_or(0, |ring| ring.lock().entries.len())
    }

    /// Whether nothing has been asked of any cube.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.asks.read().is_empty()
    }

    /// Forget everything recorded for a cube.
    ///
    /// For a definition that changed: the shapes asked of the old cube may name dimensions
    /// the new one does not have, and selecting against those would spend a budget on cuboids
    /// nothing can use.
    pub fn forget(&self, cube: &str) {
        self.asks.write().remove(cube);
    }
}
