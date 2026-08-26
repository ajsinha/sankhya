//! Reconciliation: turning "zero data loss" into a measured quantity.
//!
//! # Why a bespoke digest rather than a row-by-row comparison
//!
//! Comparing two datasets row by row requires both in memory and in the same order.
//! A digest lets each side be computed independently, in streaming fashion, in any
//! order, and compared as a single value.
//!
//! # The trap this design avoids
//!
//! The obvious way to combine per-row hashes order-independently is to XOR them. **That
//! is wrong here**, and subtly so: XOR cancels duplicate pairs. A pipeline that wrote
//! every row exactly twice would produce a digest identical to one that wrote each row
//! once — and duplication from at-least-once delivery is precisely the defect this
//! harness exists to detect.
//!
//! Wrapping addition is order-independent *and* duplicate-sensitive: adding a row twice
//! moves the sum, adding rows in a different order does not.
//!
//! # Why the comparison must be against an independent model
//!
//! If the expected side were produced by querying the source through the same reader
//! the pipeline uses, a defect in that reader would appear on both sides and cancel
//! out. The harness would pass while the data was wrong. So the expected side comes
//! from a source that shares no code with the pipeline.

use std::collections::BTreeMap;
use std::fmt;

/// A single row's contribution to a digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RowDigest(u128);

impl RowDigest {
    /// Hash a row's canonical encoding.
    ///
    /// The encoding must be canonical — the same logical value must produce the same
    /// bytes on both sides — or the digests will differ for reasons that have nothing
    /// to do with data loss. Null is distinguished from the empty string by a distinct
    /// marker, because conflating them would make a null-clobbering defect invisible.
    #[must_use]
    pub fn of(values: &[Option<&str>]) -> Self {
        // FNV-1a over the canonical encoding, widened to 128 bits by hashing forward
        // and backward. Not cryptographic — this detects accident, not attack.
        const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
        const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

        let mut hash = OFFSET;
        let mut feed = |byte: u8| {
            hash ^= u128::from(byte);
            hash = hash.wrapping_mul(PRIME);
        };

        for value in values {
            match value {
                // Distinct markers, so a null and an empty string never collide.
                None => feed(0x00),
                Some(text) => {
                    feed(0x01);
                    // Length-prefixed, so ("ab", "c") and ("a", "bc") differ.
                    for b in (text.len() as u64).to_be_bytes() {
                        feed(b);
                    }
                    for b in text.as_bytes() {
                        feed(*b);
                    }
                }
            }
            feed(0xff); // field separator
        }
        Self(hash)
    }

    #[must_use]
    pub const fn get(self) -> u128 {
        self.0
    }
}

/// An accumulated digest over a set of rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TableDigest {
    rows: u64,
    /// Wrapping sum of row digests.
    ///
    /// Addition rather than XOR, deliberately. See the module documentation: XOR would
    /// make an exactly-doubled dataset indistinguishable from a correct one.
    checksum: u128,
}

impl TableDigest {
    #[must_use]
    pub const fn empty() -> Self {
        Self { rows: 0, checksum: 0 }
    }

    /// Add a row.
    pub fn add(&mut self, digest: RowDigest) {
        self.rows = self.rows.saturating_add(1);
        self.checksum = self.checksum.wrapping_add(digest.get());
    }

    /// Add a row from its values directly.
    pub fn add_values(&mut self, values: &[Option<&str>]) {
        self.add(RowDigest::of(values));
    }

    #[must_use]
    pub const fn rows(self) -> u64 {
        self.rows
    }

    #[must_use]
    pub const fn checksum(self) -> u128 {
        self.checksum
    }

    /// Combine two partial digests.
    ///
    /// Associative and commutative, so a digest may be computed in parallel over
    /// arbitrary partitions and combined in any order.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            rows: self.rows.saturating_add(other.rows),
            checksum: self.checksum.wrapping_add(other.checksum),
        }
    }
}

/// How two digests differ.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Discrepancy {
    /// The expected side has rows the observed side lacks.
    MissingRows { expected: u64, observed: u64 },
    /// The observed side has more rows than expected.
    ///
    /// Usually duplication from replayed delivery, which is exactly what the
    /// duplicate-sensitive combiner exists to reveal.
    ExtraRows { expected: u64, observed: u64 },
    /// The counts agree but the contents do not.
    ///
    /// The most alarming outcome: the same number of rows carrying different values.
    /// A count-only check would have passed.
    ContentMismatch { expected: u128, observed: u128 },
}

impl fmt::Display for Discrepancy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRows { expected, observed } => write!(
                f,
                "{} rows missing: expected {expected}, observed {observed}",
                expected.saturating_sub(*observed)
            ),
            Self::ExtraRows { expected, observed } => write!(
                f,
                "{} extra rows: expected {expected}, observed {observed}. \
                 Usually replayed delivery applied more than once",
                observed.saturating_sub(*expected)
            ),
            Self::ContentMismatch { .. } => write!(
                f,
                "row counts agree but contents differ; a count-only check would have \
                 passed while the data was wrong"
            ),
        }
    }
}

/// The result of comparing an expectation against an observation.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Reconciliation {
    /// Per table, in name order.
    pub tables: BTreeMap<String, Result<TableDigest, Discrepancy>>,
}

impl Reconciliation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Compare one table.
    pub fn compare(&mut self, table: impl Into<String>, expected: TableDigest, observed: TableDigest) {
        let outcome = if expected.rows() > observed.rows() {
            Err(Discrepancy::MissingRows { expected: expected.rows(), observed: observed.rows() })
        } else if observed.rows() > expected.rows() {
            Err(Discrepancy::ExtraRows { expected: expected.rows(), observed: observed.rows() })
        } else if expected.checksum() != observed.checksum() {
            Err(Discrepancy::ContentMismatch {
                expected: expected.checksum(),
                observed: observed.checksum(),
            })
        } else {
            Ok(observed)
        };
        self.tables.insert(table.into(), outcome);
    }

    /// Whether every table agreed.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.tables.values().all(Result::is_ok)
    }

    /// Tables that did not agree.
    #[must_use]
    pub fn discrepancies(&self) -> Vec<(&str, Discrepancy)> {
        self.tables
            .iter()
            .filter_map(|(name, outcome)| outcome.as_ref().err().map(|d| (name.as_str(), *d)))
            .collect()
    }

    /// Total rows reconciled across all agreeing tables.
    #[must_use]
    pub fn rows_reconciled(&self) -> u64 {
        self.tables
            .values()
            .filter_map(|o| o.as_ref().ok())
            .map(|d| d.rows())
            .sum()
    }
}

impl fmt::Display for Reconciliation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clean() {
            return write!(
                f,
                "reconciled: {} tables, {} rows, no discrepancies",
                self.tables.len(),
                self.rows_reconciled()
            );
        }
        writeln!(f, "RECONCILIATION FAILED")?;
        for (table, discrepancy) in self.discrepancies() {
            writeln!(f, "  {table}: {discrepancy}")?;
        }
        Ok(())
    }
}
