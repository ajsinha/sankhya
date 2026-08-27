//! Estimating how many distinct values a column holds.
//!
//! # Why an estimate is the right answer here
//!
//! The optimizer needs distinct-value counts to order joins, and neither the file format
//! nor the table log carries them. An exact count would need every value held or a full
//! pass per column, which is precisely the cost the statistics exist to avoid.
//!
//! An estimate is acceptable because of what it is *for*: choosing which side of a join
//! to build. A cardinality wrong by two percent picks the same side; one wrong by a
//! factor of a thousand does not, and that is the failure worth preventing. This is not
//! a number anyone reports.
//!
//! # Why the hash is defined here rather than borrowed
//!
//! The estimate must be reproducible: the same column must give the same figure on every
//! machine, in every process, forever, or two nodes will disagree about a plan and the
//! disagreement will look like a bug in the optimizer. A hash whose seed varies per
//! process — which most default hashers do, deliberately — makes that impossible.

/// Registers per sketch, as a power of two.
///
/// 4,096 registers gives roughly 1.6% relative error for 4 KiB per column. Larger is
/// more accurate and the accuracy is not what limits this estimate's usefulness.
const PRECISION: u32 = 12;
const REGISTERS: usize = 1 << PRECISION;

/// A fixed, process-independent hash.
///
/// FNV-1a for the bytes, then a finalising mix. Neither is cryptographic and neither
/// needs to be; what is required is determinism and a reasonable avalanche, since a hash
/// with poor high-bit distribution biases the register index.
fn hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // splitmix64 finaliser.
    let mut z = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A mergeable estimate of distinct values.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DistinctSketch {
    registers: Vec<u8>,
}

impl Default for DistinctSketch {
    fn default() -> Self {
        Self::new()
    }
}

impl DistinctSketch {
    #[must_use]
    pub fn new() -> Self {
        Self {
            registers: vec![0; REGISTERS],
        }
    }

    pub fn add(&mut self, value: &[u8]) {
        let h = hash(value);
        let index = (h >> (64 - PRECISION)) as usize;
        // Leading zeros of the remaining bits, plus one. Shifting the index bits out
        // first, then compensating, keeps the count in the documented range.
        let remaining = (h << PRECISION) | ((1 << PRECISION) - 1);
        let rank = u8::try_from(remaining.leading_zeros() + 1).unwrap_or(u8::MAX);
        // `index` is `PRECISION` bits wide and `registers` is `1 << PRECISION` long, so
        // this cannot miss — but the workspace denies indexing, and a sketch that
        // silently wrote past its registers would corrupt a cardinality estimate rather
        // than fail, which is the failure mode hardest to notice.
        if let Some(register) = self.registers.get_mut(index) {
            *register = (*register).max(rank);
        }
    }

    /// Combine with another sketch.
    ///
    /// Register-wise maximum, which is exact: merging sketches gives the same registers
    /// as sketching the union directly. That is what makes statistics maintainable at
    /// compaction — the merged file's sketch is the merge of its inputs', with no need
    /// to re-read a single value.
    ///
    /// # Panics
    ///
    /// Never. Sketches always have the same register count, fixed at compile time.
    pub fn merge(&mut self, other: &Self) {
        for (mine, theirs) in self.registers.iter_mut().zip(&other.registers) {
            if *theirs > *mine {
                *mine = *theirs;
            }
        }
    }

    /// The estimated number of distinct values.
    #[must_use]
    pub fn estimate(&self) -> u64 {
        let m = REGISTERS as f64;
        let sum: f64 = self
            .registers
            .iter()
            .map(|r| 2f64.powi(-i32::from(*r)))
            .sum();
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let raw = alpha * m * m / sum;

        // Small-cardinality correction. Without it a nearly-empty sketch reports a few
        // hundred distinct values for a column that holds three, which is exactly the
        // magnitude of error that changes a join decision.
        let zeros = self.registers.iter().filter(|r| **r == 0).count();
        if raw <= 2.5 * m && zeros > 0 {
            return (m * (m / zeros as f64).ln()).round() as u64;
        }
        raw.round() as u64
    }

    /// Whether nothing has been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registers.iter().all(|r| *r == 0)
    }
}
