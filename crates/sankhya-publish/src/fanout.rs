//! Keeping a partitioned write from becoming ten thousand tiny files.
//!
//! # The failure this exists for
//!
//! `FR-CDC-14`: *a single commit batch shall not produce unbounded file fan-out; a batch
//! touching many partitions must not write one tiny file per partition without a guard.*
//!
//! [`Publication::append`](crate::Publication::append) writes one file per partition the
//! batch touches, which is correct and, unguarded, is exactly the forbidden shape. A
//! two-hundred-thousand-row batch spread over ninety days becomes ninety files; a
//! five-thousand-row append becomes ninety files of fifty-five rows.
//!
//! This is not theoretical. It was measured: a soak driving this crate reached **32,279 live
//! files across ten tables in four minutes**, averaging 37 KB each, where the compaction
//! policy targets 256 MB. The defect was introduced by the fix for the *previous*
//! partitioning defect, and found within minutes of a harness finally running the write path
//! instead of its own.
//!
//! # The guards, and which one matters
//!
//! `ARCHITECTURE` §6.4.2 names four. Three are implemented here:
//!
//! - **A minimum file size.** A partition holding less than [`FanOut::min_file_bytes`] is
//!   not written; its rows wait for the next batch.
//! - **A deferral age.** Unless it has waited [`FanOut::max_deferred_batches`], after which
//!   it is written however small --- or waiting becomes losing.
//! - **A cap per commit.** At most [`FanOut::max_partitions_per_commit`] partitions are
//!   written in one version, the rest deferred.
//!
//! The fourth --- routing bulk loads through a path that sorts by partition so each is
//! written once in full --- is what an [`Accumulator`] *is*, when a caller feeds it the whole
//! load before flushing.
//!
//! # The alarm is the point
//!
//! §6.4.2 is blunt about it: *"the alarm is the important one: sustained high fan-out is a
//! symptom that the partition scheme violates the minimum-partition-size guardrail. The
//! guards buy time; the alarm gets the design fixed. Silently absorbing it would be the
//! failure."*
//!
//! So [`Accumulator::strain`] reports it, and a caller that never reads it is a caller that
//! has turned a design problem into a permanent tax. The guards make ninety tiny files into
//! one good one; they cannot make ninety partitions per batch into a sensible partition
//! scheme.

use crate::publish::{Publication, PublishError, Published};
use arrow_array::RecordBatch;
use sankhya_types::Lsn;
use std::collections::BTreeMap;

/// How hard to work at avoiding tiny files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FanOut {
    /// Below this, a partition waits rather than being written.
    pub min_file_bytes: u64,
    /// How many batches a partition may wait before it is written anyway.
    ///
    /// Waiting forever is not deferral, it is loss. A partition that receives one row a day
    /// must still become readable within a bounded time.
    pub max_deferred_batches: u32,
    /// The most partitions one commit may write.
    ///
    /// A cap on the *shape* of a commit rather than on its size: a hundred-partition commit
    /// is a hundred files however many rows each holds.
    pub max_partitions_per_commit: usize,
}

impl Default for FanOut {
    /// Conservative: hold back anything under sixteen megabytes for up to sixty-four
    /// batches, and write at most thirty-two partitions per commit.
    ///
    /// Sixteen megabytes rather than the compaction target of 256 MB, because these are
    /// *arriving* files that compaction will merge again. The number that matters is that it
    /// is far above the 37 KB the unguarded path produced.
    fn default() -> Self {
        Self {
            min_file_bytes: 16 * 1024 * 1024,
            max_deferred_batches: 64,
            max_partitions_per_commit: 32,
        }
    }
}

/// What the guards are having to absorb.
///
/// Reported rather than swallowed. Sustained strain means the partition scheme is wrong, and
/// the guards are converting that into latency instead of into a fix.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Strain {
    /// Batches absorbed.
    pub batches: u64,
    /// Partitions touched across all of them.
    pub partitions_touched: u64,
    /// Times a partition was deferred for being too small.
    pub deferrals: u64,
    /// Times a partition was written only because it had waited long enough.
    pub aged_out: u64,
    /// Times the per-commit cap deferred a partition that was otherwise ready.
    pub capped: u64,
    /// The most partitions any single batch touched.
    pub widest_batch: usize,
}

impl Strain {
    /// Average partitions per batch, or `None` before any batch has arrived.
    ///
    /// `None` rather than zero: no batches is not a fan-out of nothing, it is no evidence.
    #[must_use]
    pub fn average_fan_out(&self) -> Option<f64> {
        (self.batches > 0).then(|| self.partitions_touched as f64 / self.batches as f64)
    }

    /// Whether the fan-out is sustained enough to be a design problem rather than a spike.
    ///
    /// The threshold is deliberately low. `ARCHITECTURE` §6.4.2 wants this reported early,
    /// because the guards hide the symptom and the symptom is the only thing that gets the
    /// partition scheme fixed.
    #[must_use]
    pub fn wants_attention(&self, fan_out: &FanOut) -> bool {
        self.batches >= 8
            && self
                .average_fan_out()
                .is_some_and(|average| average > fan_out.max_partitions_per_commit as f64)
    }

    /// What to tell an operator, or `None` when there is nothing to say.
    #[must_use]
    pub fn explain(&self, fan_out: &FanOut) -> Option<String> {
        let average = self.average_fan_out()?;
        self.wants_attention(fan_out).then(|| format!(
            "sustained partition fan-out: {average:.0} partitions per batch across {} \
             batches, widest {}. The guards are converting this into deferral and will keep \
             doing so; the partition scheme is the thing to change, because a batch touching \
             this many partitions has a partition granularity finer than its arrival pattern",
            self.batches, self.widest_batch
        ))
    }
}

/// Rows waiting for a partition to be worth writing.
#[derive(Debug)]
struct Deferred {
    batches: Vec<RecordBatch>,
    bytes: u64,
    waited: u32,
}

/// Accumulates batches per partition and writes each once it is worth writing.
///
/// The bulk-load path of §6.4.2: feed it everything, then [`Accumulator::flush`], and each
/// partition is written once in full rather than once per input batch.
#[derive(Debug)]
pub struct Accumulator<'a> {
    publication: &'a Publication,
    fan_out: FanOut,
    deferred: BTreeMap<String, Deferred>,
    strain: Strain,
}

impl<'a> Accumulator<'a> {
    /// An accumulator over a publication.
    #[must_use]
    pub fn new(publication: &'a Publication, fan_out: FanOut) -> Self {
        Self {
            publication,
            fan_out,
            deferred: BTreeMap::new(),
            strain: Strain::default(),
        }
    }

    /// What the guards have absorbed so far.
    #[must_use]
    pub const fn strain(&self) -> &Strain {
        &self.strain
    }

    /// How many partitions are holding rows that are not yet readable.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.deferred.len()
    }

    /// Absorb a batch, writing whatever partitions are now worth writing.
    ///
    /// # Errors
    /// As [`Publication::append`].
    pub fn absorb(
        &mut self,
        file_name: &str,
        batch: &RecordBatch,
        covers_through: Lsn,
    ) -> Result<Vec<Published>, PublishError> {
        let split = self.publication.split_by_partition(batch)?;
        self.strain.batches = self.strain.batches.saturating_add(1);
        self.strain.partitions_touched = self
            .strain
            .partitions_touched
            .saturating_add(split.len() as u64);
        self.strain.widest_batch = self.strain.widest_batch.max(split.len());

        for (partition, part) in split {
            // An estimate, and it only has to be good enough to decide "too small". Encoding
            // the batch to find out would write the file this exists to avoid writing.
            let bytes = part.get_array_memory_size() as u64;
            let entry = self.deferred.entry(partition).or_insert_with(|| Deferred {
                batches: Vec::new(),
                bytes: 0,
                waited: 0,
            });
            entry.batches.push(part);
            entry.bytes = entry.bytes.saturating_add(bytes);
        }
        for entry in self.deferred.values_mut() {
            entry.waited = entry.waited.saturating_add(1);
        }

        self.write_ready(file_name, covers_through, false)
    }

    /// Write everything still deferred, however small.
    ///
    /// # Errors
    /// As [`Publication::append`].
    pub fn flush(
        &mut self,
        file_name: &str,
        covers_through: Lsn,
    ) -> Result<Vec<Published>, PublishError> {
        self.write_ready(file_name, covers_through, true)
    }

    /// Write the partitions that qualify.
    fn write_ready(
        &mut self,
        file_name: &str,
        covers_through: Lsn,
        everything: bool,
    ) -> Result<Vec<Published>, PublishError> {
        let mut ready: Vec<String> = Vec::new();
        for (partition, entry) in &self.deferred {
            if everything {
                ready.push(partition.clone());
                continue;
            }
            if entry.bytes >= self.fan_out.min_file_bytes {
                ready.push(partition.clone());
            } else if entry.waited >= self.fan_out.max_deferred_batches {
                self.strain.aged_out = self.strain.aged_out.saturating_add(1);
                ready.push(partition.clone());
            } else {
                self.strain.deferrals = self.strain.deferrals.saturating_add(1);
            }
        }

        if !everything && ready.len() > self.fan_out.max_partitions_per_commit {
            let over = ready.len() - self.fan_out.max_partitions_per_commit;
            self.strain.capped = self.strain.capped.saturating_add(over as u64);
            ready.truncate(self.fan_out.max_partitions_per_commit);
        }

        let mut taken = Vec::new();
        for partition in &ready {
            if let Some(entry) = self.deferred.remove(partition) {
                taken.push(entry.batches);
            }
        }
        let batches: Vec<RecordBatch> = taken.into_iter().flatten().collect();
        if batches.is_empty() {
            return Ok(Vec::new());
        }
        // The version is decided *here*, when a commit actually happens --- never by the
        // caller per input batch.
        //
        // An earlier version took a version parameter, and the first caller advanced it once
        // per batch absorbed. Since most batches are deferred, sixty-three versions passed
        // with no commit and the flush asked for version 64 against a log holding none. The
        // log refused it, correctly: *"committing version 64 would leave a gap; the next
        // version is 1"*. Deferral and caller-assigned versions cannot both be right, and the
        // caller has no business knowing log versions at all.
        self.publication
            .append_all_rebasing(file_name, &batches, covers_through)
    }
}
