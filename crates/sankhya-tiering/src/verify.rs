//! Exhaustive verification of an archive against the source it was copied from.
//!
//! `FR-TIER-09` names three checks and then says the thing that matters most: **count equality
//! alone is not evidence**. A partition of a million rows copied with every value replaced by
//! its default has the right count. So verification is a row count, primary-key set equality
//! via a Merkle digest over sorted blocks, *and* per-column checksums over the canonical byte
//! encoding --- all three, on every run, with no path that computes fewer.
//!
//! # Why the per-column checksums are taken in key order
//!
//! The obvious implementation checksums each column independently over its own sorted values.
//! It is order-independent, which is the property that seems to be wanted, and it cannot see
//! the corruption that matters most.
//!
//! Take two rows and exchange their `amount` values. Every column's multiset is unchanged, so
//! every independent per-column checksum matches. The row count matches. The primary-key set
//! matches. An archive in which two customers' balances have been swapped verifies as
//! faithful, and the source is then purged.
//!
//! So rows are sorted by their encoded primary key and every column is checksummed **in that
//! order**. The result is still independent of the order the rows were read in --- which is
//! the real requirement, because an archive scan and a source scan have no reason to agree on
//! it --- while remaining a check on the association between a key and its row.
//!
//! # Why the key digest is a Merkle tree rather than a hash of the sorted keys
//!
//! A single hash over all sorted keys answers "same or different" and nothing else. On a
//! partition of a hundred million rows, "different" with no other information is the beginning
//! of a manual investigation, during which the partition is detached and the operator is
//! holding data that is neither purged nor released.
//!
//! Blocks of [`BLOCK_ROWS`] give the mismatch a location: the block index is a range of the
//! key space, and re-scanning one block on both sides is a bounded operation. `FR-TIER-14`
//! makes verification failure terminal until an operator acts, and an operator who has to act
//! deserves to be told where.
//!
//! # Three ways a Merkle tree is built wrong
//!
//! **Leaves and internal nodes hashed alike.** If a leaf is `H(block)` and a node is
//! `H(left || right)`, then a block whose bytes happen to be two concatenated digests produces
//! the same root as the two-leaf tree above it. Second-preimage resistance of the hash does not
//! help, because nothing was broken --- the encoding was ambiguous. Every node here is prefixed
//! with a domain byte.
//!
//! **The odd node duplicated.** Promoting a lone right-hand node by hashing it with itself
//! makes a three-leaf tree and a four-leaf tree whose last leaf repeats produce the same root.
//! That is the defect Bitcoin shipped and had to work around. A lone node here is hashed under
//! its own domain byte instead, so the two shapes differ.
//!
//! **The shape left out of the answer.** Even with the above, the block count is carried in
//! [`KeyDigest`] and compared separately, so a disagreement about shape is reported as one
//! rather than relied upon to change a root.

use crate::encode::{self, Unencodable};
use arrow_array::RecordBatch;
use sha2::{Digest, Sha256};
use std::fmt;

/// Rows per Merkle leaf.
///
/// Small enough that re-scanning one block to locate a mismatch is cheap; large enough that the
/// tree over a large partition stays shallow. Fixed rather than configurable: it is part of
/// what a digest means, and two sides that chose differently would disagree for no reason.
pub const BLOCK_ROWS: usize = 4096;

/// A digest, as thirty-two bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hash([u8; 32]);

impl Hash {
    /// A digest read back from storage.
    ///
    /// The registry is written to write-once storage and read again years later, so a digest
    /// has to be reconstructible from its bytes. There is no parse from the hexadecimal form:
    /// a reader that can be handed a string can be handed the wrong string, and a fixed-width
    /// array cannot be the wrong length.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest's bytes.
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The full digest, not a prefix. A truncated digest in a failure report is how two
        // different values come to look like the same one in somebody's notes.
        write!(f, "{self}")
    }
}

/// Domain bytes. Every hash in this module opens with exactly one of them.
mod domain {
    /// A block of encoded keys.
    pub(super) const LEAF: u8 = 0x00;
    /// Two child digests.
    pub(super) const NODE: u8 = 0x01;
    /// A child with no sibling at this level.
    pub(super) const LONE: u8 = 0x02;
    /// No blocks at all.
    pub(super) const EMPTY: u8 = 0x03;
    /// A column's values, in key order.
    pub(super) const COLUMN: u8 = 0x04;
}

fn leaf(block: &[u8]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([domain::LEAF]);
    hasher.update(block);
    Hash(hasher.finalize().into())
}

fn node(left: Hash, right: Hash) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([domain::NODE]);
    hasher.update(left.0);
    hasher.update(right.0);
    Hash(hasher.finalize().into())
}

fn lone(only: Hash) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([domain::LONE]);
    hasher.update(only.0);
    Hash(hasher.finalize().into())
}

fn empty() -> Hash {
    Hash(Sha256::digest([domain::EMPTY]).into())
}

/// The Merkle root over `leaves`, or the empty digest.
fn root(mut level: Vec<Hash>) -> Hash {
    if level.is_empty() {
        return empty();
    }
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut pairs = level.chunks_exact(2);
        for pair in &mut pairs {
            if let [left, right] = pair {
                next.push(node(*left, *right));
            }
        }
        if let [only] = pairs.remainder() {
            next.push(lone(*only));
        }
        level = next;
    }
    // The loop leaves exactly one, and an empty level returned above.
    level.first().copied().unwrap_or_else(empty)
}

/// The primary-key set, as a digest that can be compared and then localised.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyDigest {
    /// The Merkle root over blocks of sorted, encoded keys.
    pub root: Hash,
    /// How many blocks the root covers.
    pub blocks: u64,
    /// Keys equal to the one before them in sorted order.
    ///
    /// A primary key with duplicates is not one, and set equality is not a question that has an
    /// answer over a bag. Reported rather than deduplicated: silently collapsing them would let
    /// an archive that duplicated every row verify against a source that did not.
    pub duplicates: u64,
}

/// One column's checksum, taken over its values in primary-key order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ColumnDigest {
    /// The column.
    pub column: String,
    /// The digest.
    pub digest: Hash,
}

/// Everything one side of a verification contributes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fingerprint {
    /// Rows seen.
    pub rows: u64,
    /// The primary-key set.
    pub keys: KeyDigest,
    /// Every non-key column, in the order the scan declared them.
    pub columns: Vec<ColumnDigest>,
}

/// Which side a finding is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// The system of record.
    Source,
    /// The published tier.
    Archive,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Source => "source",
            Self::Archive => "archive",
        })
    }
}

/// One way the two sides disagree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Discrepancy {
    /// Neither side has the number of rows the plan said the partition holds.
    ///
    /// # The hole this closes
    ///
    /// Two scans that both return nothing agree on the row count, agree on the empty key set,
    /// and agree on every column, because there is nothing to disagree about. A verification
    /// of source against archive alone therefore **passes when both scans are pointed at the
    /// wrong place** --- and the purge then detaches a partition nobody read.
    ///
    /// The plan already knows how many rows the partition holds; it is what the blast-radius
    /// limit is computed against. Comparing against it costs nothing and turns "both sides
    /// scanned nothing" from a pass into the loudest possible failure.
    PlannedRowCount {
        /// What the plan said.
        planned: u64,
        /// What the source scan found.
        source: u64,
    },
    /// Different row counts.
    RowCount {
        /// The source's count.
        source: u64,
        /// The archive's count.
        archive: u64,
    },
    /// Different primary-key sets.
    KeySet {
        /// The source's root.
        source: Hash,
        /// The archive's root.
        archive: Hash,
    },
    /// Different numbers of key blocks, which is a disagreement about shape.
    KeyBlocks {
        /// The source's block count.
        source: u64,
        /// The archive's block count.
        archive: u64,
    },
    /// One side's primary key is not a key.
    DuplicateKeys {
        /// Which side.
        side: Side,
        /// How many.
        count: u64,
    },
    /// A column present on one side and not the other.
    ColumnMissing {
        /// The column.
        column: String,
        /// The side it is absent from.
        side: Side,
    },
    /// A column whose values differ.
    Column {
        /// The column.
        column: String,
        /// The source's digest.
        source: Hash,
        /// The archive's digest.
        archive: Hash,
    },
}

impl fmt::Display for Discrepancy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlannedRowCount { planned, source } => write!(
                f,
                "the plan says the partition holds {planned} rows and the source scan found \
                 {source}; a scan that did not read what was planned has verified nothing"
            ),
            Self::RowCount { source, archive } => {
                write!(f, "row count: source {source}, archive {archive}")
            }
            Self::KeySet { source, archive } => {
                write!(f, "primary-key set: source {source}, archive {archive}")
            }
            Self::KeyBlocks { source, archive } => {
                write!(f, "key blocks: source {source}, archive {archive}")
            }
            Self::DuplicateKeys { side, count } => {
                write!(f, "{side} has {count} duplicate primary keys, so it has no key set")
            }
            Self::ColumnMissing { column, side } => {
                write!(f, "column `{column}` is absent from the {side}")
            }
            Self::Column { column, source, archive } => {
                write!(f, "column `{column}`: source {source}, archive {archive}")
            }
        }
    }
}

/// The result of comparing two fingerprints.
///
/// # Why this is not a `bool`
///
/// `FR-TIER-14` makes failure terminal until an operator acts, and an operator acting on
/// `false` has nothing to act on. Every discrepancy is carried, and every one is found ---
/// stopping at the first would report a row-count difference and hide that four columns also
/// disagree, which is the difference between "a batch was missed" and "the copy is wrong".
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Verification {
    /// Everything found. Empty is the only passing value.
    pub discrepancies: Vec<Discrepancy>,
}

impl Verification {
    /// Whether the archive is a faithful copy.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.discrepancies.is_empty()
    }

    /// The witness that verification passed, or nothing.
    ///
    /// # Why the witness exists
    ///
    /// `FR-TIER-15`: there is no flag that skips verification, and verification is
    /// *structurally* absent from every path that could bypass it. A [`Proof`] cannot be
    /// constructed anywhere else --- its field is private, it has no `new`, no `Default` and no
    /// `Clone` from thin air --- so a caller that wants one has to have run a comparison that
    /// found nothing. The compiler is what enforces the requirement, rather than a review.
    #[must_use]
    pub fn proof(&self) -> Option<Proof> {
        self.passed().then_some(Proof { _private: () })
    }
}

/// Evidence that an exhaustive verification passed.
///
/// Carried into the phase transition that records it, so entering `Verified` is impossible
/// without one. See [`Verification::proof`].
///
/// Deliberately **not** `Clone` or `Copy`. A proof is evidence about one comparison of one
/// partition, and a copyable one kept in a variable and passed again for the next partition
/// would be precisely the bypass `FR-TIER-15` forbids. Each partition earns its own.
#[derive(Debug)]
pub struct Proof {
    _private: (),
}

/// Compare a source fingerprint against an archive fingerprint, and both against the plan.
///
/// `planned` is the row count the purge plan recorded for this partition. It is not redundant
/// with the source-against-archive comparison: see [`Discrepancy::PlannedRowCount`].
///
/// Every check runs. There is no early return, because a caller cannot ask for fewer.
#[must_use]
pub fn compare(planned: u64, source: &Fingerprint, archive: &Fingerprint) -> Verification {
    let mut discrepancies = Vec::new();

    if source.rows != planned {
        discrepancies.push(Discrepancy::PlannedRowCount { planned, source: source.rows });
    }
    if source.rows != archive.rows {
        discrepancies.push(Discrepancy::RowCount { source: source.rows, archive: archive.rows });
    }
    if source.keys.duplicates > 0 {
        discrepancies.push(Discrepancy::DuplicateKeys {
            side: Side::Source,
            count: source.keys.duplicates,
        });
    }
    if archive.keys.duplicates > 0 {
        discrepancies.push(Discrepancy::DuplicateKeys {
            side: Side::Archive,
            count: archive.keys.duplicates,
        });
    }
    if source.keys.blocks != archive.keys.blocks {
        discrepancies.push(Discrepancy::KeyBlocks {
            source: source.keys.blocks,
            archive: archive.keys.blocks,
        });
    }
    if source.keys.root != archive.keys.root {
        discrepancies
            .push(Discrepancy::KeySet { source: source.keys.root, archive: archive.keys.root });
    }

    for column in &source.columns {
        match archive.columns.iter().find(|other| other.column == column.column) {
            None => discrepancies.push(Discrepancy::ColumnMissing {
                column: column.column.clone(),
                side: Side::Archive,
            }),
            Some(other) if other.digest != column.digest => {
                discrepancies.push(Discrepancy::Column {
                    column: column.column.clone(),
                    source: column.digest,
                    archive: other.digest,
                });
            }
            Some(_) => {}
        }
    }
    for column in &archive.columns {
        if !source.columns.iter().any(|other| other.column == column.column) {
            discrepancies.push(Discrepancy::ColumnMissing {
                column: column.column.clone(),
                side: Side::Source,
            });
        }
    }

    Verification { discrepancies }
}

/// Why a side could not be fingerprinted at all.
///
/// Distinct from a [`Discrepancy`]: a scan that could not run has demonstrated nothing, and
/// recording it as a mismatch would be as wrong as recording it as a pass. Same distinction
/// `Attestation::could_not_attempt` draws for the archive drill.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ScanFailure {
    /// A value could not be encoded.
    Value(Unencodable),
    /// A key column is not in the batch.
    ///
    /// Fatal rather than a discrepancy: without the key there is no set to compare, and the
    /// other two checks alone are the "count equality is not evidence" case `FR-TIER-09`
    /// refuses.
    MissingKeyColumn {
        /// The column.
        column: String,
    },
    /// A column present in one batch and absent from a later one.
    ///
    /// A digest folded over a column that appeared halfway through is a digest of nothing in
    /// particular.
    SchemaChanged {
        /// The column.
        column: String,
    },
    /// No key columns were named.
    ///
    /// A table with no declared primary key cannot have its key set compared, which is why
    /// `Policy::eligible` refuses one before a purge is ever planned.
    NoKeyColumns,
}

impl fmt::Display for ScanFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(why) => write!(f, "{why}"),
            Self::MissingKeyColumn { column } => {
                write!(f, "key column `{column}` is not in the batch, so there is no key set")
            }
            Self::SchemaChanged { column } => {
                write!(f, "column `{column}` appeared or vanished part-way through the scan")
            }
            Self::NoKeyColumns => f.write_str(
                "no key columns: primary-key set equality is not a question that can be asked",
            ),
        }
    }
}

/// Builds one side's [`Fingerprint`] from the batches it is fed.
///
/// # What it holds
///
/// Every row's encoded key and values, until [`Self::finish`]. The sort is what forces that:
/// per-column checksums are taken in key order, and key order is not known until the last row
/// has arrived. The unit of work is one partition, which is also the unit a purge detaches, so
/// the bound is the same bound the rest of the purge already has.
#[derive(Debug)]
pub struct Scan {
    keys: Vec<String>,
    values: Vec<String>,
    /// `None` until the first batch fixes which requested columns exist.
    present: Option<Vec<bool>>,
    rows: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
}

impl Scan {
    /// A scan of `keys` and `values`.
    ///
    /// # Errors
    ///
    /// [`ScanFailure::NoKeyColumns`] when `keys` is empty.
    pub fn new(keys: Vec<String>, values: Vec<String>) -> Result<Self, ScanFailure> {
        if keys.is_empty() {
            return Err(ScanFailure::NoKeyColumns);
        }
        Ok(Self { keys, values, present: None, rows: Vec::new() })
    }

    /// Take one batch.
    ///
    /// # Errors
    ///
    /// [`ScanFailure`] when a key column is absent, a value column appears or vanishes between
    /// batches, or a value cannot be encoded.
    pub fn absorb(&mut self, batch: &RecordBatch) -> Result<(), ScanFailure> {
        let schema = batch.schema();

        let mut key_indices = Vec::with_capacity(self.keys.len());
        for column in &self.keys {
            let index = schema
                .index_of(column)
                .map_err(|_| ScanFailure::MissingKeyColumn { column: column.clone() })?;
            key_indices.push(index);
        }

        let found: Vec<Option<usize>> =
            self.values.iter().map(|column| schema.index_of(column).ok()).collect();
        let here: Vec<bool> = found.iter().map(Option::is_some).collect();
        match &self.present {
            None => self.present = Some(here),
            Some(before) if *before != here => {
                let changed = self
                    .values
                    .iter()
                    .zip(before.iter().zip(here.iter()))
                    .find(|(_, (b, h))| b != h)
                    .map(|(column, _)| column.clone())
                    .unwrap_or_default();
                return Err(ScanFailure::SchemaChanged { column: changed });
            }
            Some(_) => {}
        }

        for row in 0..batch.num_rows() {
            let mut key = Vec::new();
            for (column, index) in self.keys.iter().zip(key_indices.iter()) {
                encode::value(&mut key, column, batch.column(*index).as_ref(), row)
                    .map_err(ScanFailure::Value)?;
            }
            let mut values = Vec::with_capacity(self.values.len());
            for (column, index) in self.values.iter().zip(found.iter()) {
                let Some(index) = index else { continue };
                let mut encoded = Vec::new();
                encode::value(&mut encoded, column, batch.column(*index).as_ref(), row)
                    .map_err(ScanFailure::Value)?;
                values.push(encoded);
            }
            self.rows.push((key, values));
        }
        Ok(())
    }

    /// Sort by key and produce the fingerprint.
    #[must_use]
    pub fn finish(mut self) -> Fingerprint {
        self.rows.sort_by(|left, right| left.0.cmp(&right.0));

        let duplicates = self
            .rows
            .windows(2)
            .filter(|pair| matches!(pair, [before, after] if before.0 == after.0))
            .count() as u64;

        let leaves: Vec<Hash> = self
            .rows
            .chunks(BLOCK_ROWS)
            .map(|block| {
                let mut bytes = Vec::new();
                for (key, _) in block {
                    // Length-prefixed inside the block for the same reason keys are
                    // length-prefixed inside a composite key: a block is a function of its
                    // rows, not of their concatenation.
                    bytes.extend_from_slice(&(key.len() as u64).to_be_bytes());
                    bytes.extend_from_slice(key);
                }
                leaf(&bytes)
            })
            .collect();
        let blocks = leaves.len() as u64;

        let present = self.present.unwrap_or_else(|| vec![false; self.values.len()]);
        let names: Vec<&String> = self
            .values
            .iter()
            .zip(present.iter())
            .filter_map(|(column, here)| here.then_some(column))
            .collect();

        let columns = names
            .iter()
            .enumerate()
            .map(|(slot, column)| {
                let mut hasher = Sha256::new();
                hasher.update([domain::COLUMN]);
                for (_, values) in &self.rows {
                    let Some(encoded) = values.get(slot) else { continue };
                    hasher.update((encoded.len() as u64).to_be_bytes());
                    hasher.update(encoded);
                }
                ColumnDigest {
                    column: (*column).clone(),
                    digest: Hash(hasher.finalize().into()),
                }
            })
            .collect();

        Fingerprint {
            rows: self.rows.len() as u64,
            keys: KeyDigest { root: root(leaves), blocks, duplicates },
            columns,
        }
    }
}
