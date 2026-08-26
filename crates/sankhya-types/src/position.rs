//! Log positions and table versions.
//!
//! The log position is SANKHYA's universal ordering coordinate. It is a global,
//! transaction-consistent sequence supplied by the transactional store, and every
//! tier of the read path declares the position interval it covers. Splicing on this
//! coordinate — rather than on wall-clock time — is what preserves transactional
//! atomicity across a tiered query: a transaction touching several tables carries one
//! position, so it is either wholly visible or wholly invisible.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A position in the transactional store's write-ahead log.
///
/// Ordered, monotonically non-decreasing within a stream. Displayed in the
/// upstream `X/Y` convention so operators can correlate with database tooling.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Lsn(u64);

impl Lsn {
    /// The position before any change. Every tier covering `[0, x]` starts here.
    pub const ZERO: Lsn = Lsn(0);

    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Parse the upstream `X/Y` textual form, where both halves are hexadecimal.
    ///
    /// Returns `None` rather than panicking on malformed input: this parses values
    /// that arrive over a wire protocol, so malformed input is expected, not exceptional.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (hi, lo) = text.split_once('/')?;
        let hi = u64::from_str_radix(hi.trim(), 16).ok()?;
        let lo = u64::from_str_radix(lo.trim(), 16).ok()?;
        Some(Self((hi << 32) | lo))
    }
}

impl fmt::Display for Lsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:X}/{:X}", self.0 >> 32, self.0 & 0xFFFF_FFFF)
    }
}

impl fmt::Debug for Lsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lsn({self})")
    }
}

/// A half-open interval `(start, end]` of log positions.
///
/// Half-open at the start is deliberate. Tiers declare coverage as
/// `(previous_high_water, own_high_water]`, so adjacent tiers abut exactly with no
/// overlap and no gap. That property is what makes the read-path splice provably
/// free of double-counting rather than merely unlikely to double-count.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LsnRange {
    start_exclusive: Lsn,
    end_inclusive: Lsn,
}

impl LsnRange {
    /// Construct `(start, end]`. Returns `None` if the interval is inverted.
    #[must_use]
    pub fn new(start_exclusive: Lsn, end_inclusive: Lsn) -> Option<Self> {
        (start_exclusive <= end_inclusive).then_some(Self { start_exclusive, end_inclusive })
    }

    /// The interval `(0, end]` — everything a tier holds from the beginning.
    #[must_use]
    pub const fn up_to(end_inclusive: Lsn) -> Self {
        Self { start_exclusive: Lsn::ZERO, end_inclusive }
    }

    #[must_use]
    pub const fn start_exclusive(self) -> Lsn {
        self.start_exclusive
    }

    #[must_use]
    pub const fn end_inclusive(self) -> Lsn {
        self.end_inclusive
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start_exclusive.0 == self.end_inclusive.0
    }

    #[must_use]
    pub const fn contains(self, lsn: Lsn) -> bool {
        lsn.0 > self.start_exclusive.0 && lsn.0 <= self.end_inclusive.0
    }

    /// Whether `self` ends exactly where `next` begins, with no gap and no overlap.
    ///
    /// This is the predicate the read-path planner uses to prove a tier set is safe
    /// to splice.
    #[must_use]
    pub const fn abuts(self, next: LsnRange) -> bool {
        self.end_inclusive.0 == next.start_exclusive.0
    }

    #[must_use]
    pub const fn overlaps(self, other: LsnRange) -> bool {
        self.start_exclusive.0 < other.end_inclusive.0
            && other.start_exclusive.0 < self.end_inclusive.0
    }
}

/// A monotonic version of a table, opaque to clients.
///
/// Deliberately *not* a storage-format version identifier. Exposing a format-specific
/// value on the wire would make adding a second table format a breaking API change.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TableVersion(u64);

impl TableVersion {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for TableVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}
