//! Hydrated cells, kept only as long as they are still an answer to the same question.
//!
//! # What the key has to be, and why
//!
//! Hydrating a cube reads its fact table, which is the cost the cube exists to avoid paying
//! per query. Caching it is therefore the whole point — and a cache of aggregates is a
//! disclosure waiting to happen, because an aggregate computed over the rows one principal
//! may read is not an answer for another.
//!
//! [ADR-0008](../../../docs/adr/0008-serving-cubes-under-policy.md) states the rule the whole
//! industry arrives at independently, most plainly in Cube's documentation: **anything that
//! scopes the query must also scope the cache key.** So the key here is every one of:
//!
//! - the **cube** and the **measure**, because cells hold one measure's values;
//! - the **definition version**, so an edited cube cannot hit a hit shaped by the old one;
//! - the **snapshot**, so a new commit cannot serve a stale figure — `FR-QUERY-20`;
//! - the **scope digest**, so a restricted principal is never served an unrestricted total.
//!
//! Miss on any of them and the cost is a hydration. Wrongly *hit* on the last one and the
//! cost is a number somebody acts on, with nothing in the result to say it was not theirs.
//!
//! # Why bounded, and why by entry rather than by byte
//!
//! An unbounded cache of cube cells is a memory leak with a business justification. The bound
//! is a count of entries rather than a size in bytes because a cell set's memory footprint is
//! not something this layer can measure honestly — and a limit computed from a bad estimate
//! is worse than a smaller limit that is exact.

use crate::catalog::Published;
use std::collections::BTreeMap;

/// What makes one set of hydrated cells the same as another.
///
/// Every field is load-bearing. See the module documentation for what each one prevents.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Key {
    /// The cube.
    pub cube: String,
    /// The measure whose values the cells hold.
    pub measure: String,
    /// The definition's fingerprint, so an edited cube does not hit the old cube's cells.
    pub definition_version: u64,
    /// The snapshot the fact table was read at.
    pub snapshot: u64,
    /// What the principal who caused this hydration was permitted to see.
    ///
    /// From `Guard::scope_digest`, which covers what is visible and deliberately not who is
    /// asking — so principals with equal entitlements share an entry.
    pub scope: u64,
    /// **What position this session reads from**, when it reads from one it chose.
    ///
    /// Zero for a session reading the present, and a digest of the `SET SNAPSHOT` and
    /// `SET VERSION OF` settings otherwise.
    ///
    /// # Why the snapshot field is not enough
    ///
    /// `snapshot` holds the table's *present* version, deliberately: keying on the configured
    /// `read_as_of` would make it a constant for the life of the process and the cache would
    /// serve its first hydration for ever.
    ///
    /// A session with a pin reads at a different position and gets different cells — and put
    /// them under the present version, which is what happened, and the next unpinned session
    /// looking up the same key is served them. `COR-20`. A pinned read is the one kind of read
    /// whose whole promise is that it does not move, and it was leaking into reads that
    /// promise the opposite.
    ///
    /// A digest rather than the versions themselves because a session may pin several tables
    /// separately, and the key has to be one value.
    pub pin: u64,
}

/// Hydrated cells, bounded, keyed by everything that makes them an answer.
#[derive(Debug)]
pub struct Hydrated {
    entries: parking_lot::RwLock<BTreeMap<Key, Published>>,
    /// How many sets of cells may be held.
    capacity: usize,
    /// How many lookups found what they wanted, and how many did not.
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

impl Hydrated {
    /// A cache holding at most `capacity` sets of cells.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: parking_lot::RwLock::new(BTreeMap::new()),
            capacity: capacity.max(1),
            hits: std::sync::atomic::AtomicU64::new(0),
            misses: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The cells for this key, if they are held.
    #[must_use]
    pub fn get(&self, key: &Key) -> Option<Published> {
        use std::sync::atomic::Ordering;
        let found = self.entries.read().get(key).cloned();
        if found.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        found
    }

    /// Hold these cells under this key.
    ///
    /// # What is evicted, and the honesty of it
    ///
    /// The lowest key, which is an arbitrary victim rather than the least recently used one.
    /// Said plainly because "LRU" is what a reader assumes: tracking recency needs a write on
    /// every *read*, which turns the read lock into a write lock and makes the cache a
    /// contention point on the path it exists to make fast.
    ///
    /// An arbitrary eviction costs one rehydration. Choosing the victim well is worth doing
    /// when there is a measurement saying which entries are worth keeping, and the hit and
    /// miss counters here are the beginning of that measurement.
    pub fn put(&self, key: Key, published: Published) {
        let mut entries = self.entries.write();
        if entries.len() >= self.capacity && !entries.contains_key(&key) {
            if let Some(victim) = entries.keys().next().cloned() {
                entries.remove(&victim);
            }
        }
        entries.insert(key, published);
    }

    /// Forget everything held for a cube, whatever scope or snapshot it was held under.
    ///
    /// For a definition that changed or a cube that was dropped. Scoped to the cube rather
    /// than clearing the cache, because one edited cube must not cost every other cube its
    /// hydration.
    pub fn forget(&self, cube: &str) {
        self.entries.write().retain(|key, _| key.cube != cube);
    }

    /// How many sets of cells are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Hits and misses since this cache was made.
    ///
    /// Exposed because a cache nobody can measure is a cache nobody can size. A hit rate near
    /// zero means the key is too specific — the scope digest varying per principal is the way
    /// that happens — and it is invisible without this.
    #[must_use]
    pub fn counts(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

impl Default for Hydrated {
    /// Sixty-four sets of cells.
    ///
    /// Enough for a handful of cubes across a handful of entitlement scopes, which is the
    /// shape a deployment actually has: cubes are declared deliberately and roles are few.
    fn default() -> Self {
        Self::with_capacity(64)
    }
}
