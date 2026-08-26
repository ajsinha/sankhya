//! The arrival tier: recently applied changes, held in memory until they are durable.
//!
//! # Why this tier exists
//!
//! Publication is batched, because writing a Parquet file per transaction would produce
//! exactly the small-file pathology that compaction exists to clean up. But a query
//! issued between two publications must still see the writes that happened in between,
//! or the system does not have read-your-own-writes — it has read-your-own-writes-
//! eventually, which is a different and much weaker promise.
//!
//! The arrival tier closes that window. It holds applied changes in Arrow form, declares
//! coverage the read path can splice against, and releases nothing until the published
//! tier has taken over.
//!
//! # The rule that governs eviction
//!
//! **A segment may be released only once a durable tier covers it.** Not when it is old,
//! not when memory is tight, not when it has been read. Any other rule can open a gap in
//! the middle of the coverage interval, and a gap means the read path must refuse the
//! query — the splice is a proof of exact cover, and it cannot be talked into
//! approximating one.
//!
//! This inverts the usual cache relationship. A cache evicts under pressure and takes a
//! miss; this tier is not a cache, and there is no backing store to miss *to* until
//! publication has happened. So when memory runs short and nothing is releasable, the
//! only correct response is to push back on ingest. That is a real operational
//! condition, reported as such rather than absorbed.
//!
//! # Why coverage is trimmed but data is not
//!
//! The buffer physically retains segments the published tier already covers, because
//! releasing them is a separate decision made under the rule above. But it *declares*
//! coverage starting at the durable frontier, so the two tiers abut exactly and the
//! splice succeeds.
//!
//! The consequence is that a scan must filter per row — `durable_through < lsn <=
//! target` — rather than per segment. A segment straddling the frontier is half durable
//! and half not, and returning the whole thing would double-count the durable half
//! against the published tier. That is the same defect, at a different layer, as
//! suppressing duplicates per batch rather than per row.

#![doc(html_root_url = "https://docs.rs/sankhya-table-memory")]

use arrow_array::{cast::AsArray, RecordBatch, UInt64Array};
use arrow_schema::SchemaRef;
use sankhya_types::{Lsn, LsnRange};
use std::collections::VecDeque;

/// The column every applied batch carries, and the one this tier orders by.
const COMMIT_LSN: &str = "_sankhya_commit_lsn";

/// Why an append was refused or accepted grudgingly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Admission {
    /// Accepted, with headroom remaining.
    Accepted,
    /// Accepted, but the tier is above its soft limit.
    ///
    /// Ingest should lengthen its commit interval, which reduces publication *and*
    /// compaction load at the same time. It is the highest-leverage response because it
    /// attacks the cause rather than the symptom.
    AcceptedUnderPressure { bytes: usize, soft_limit: usize },
    /// Refused. The tier is full and nothing may be released.
    ///
    /// This is not a memory problem; it is a publication problem wearing a memory
    /// problem's clothes. The remedy is to find out why publication has stalled.
    Refused {
        bytes: usize,
        hard_limit: usize,
        durable_through: Lsn,
        held_through: Lsn,
    },
}

impl Admission {
    #[must_use]
    pub const fn accepted(&self) -> bool {
        !matches!(self, Self::Refused { .. })
    }
}

/// How much memory the tier may use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryBudget {
    /// Above this, appends succeed but report pressure.
    pub soft_limit: usize,
    /// Above this, appends are refused.
    ///
    /// The gap between the two is deliberate: it gives ingest a chance to react before
    /// it is stopped, rather than running normally until it hits a wall.
    pub hard_limit: usize,
}

impl Default for MemoryBudget {
    fn default() -> Self {
        Self {
            soft_limit: 64 * 1024 * 1024,
            hard_limit: 256 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
struct Segment {
    batch: RecordBatch,
    coverage: LsnRange,
    bytes: usize,
}

/// What a release actually freed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Released {
    pub segments: usize,
    pub rows: usize,
    pub bytes: usize,
    /// Retained despite being durable, because part of the segment is not.
    ///
    /// A straddling segment is kept whole; splitting it would cost a copy to save
    /// memory the next publication frees anyway.
    pub straddling_retained: usize,
}

/// Recently applied changes for one table, held until published.
#[derive(Clone, Debug)]
pub struct ArrivalBuffer {
    name: &'static str,
    schema: SchemaRef,
    segments: VecDeque<Segment>,
    durable_through: Lsn,
    budget: MemoryBudget,
    bytes: usize,
}

impl ArrivalBuffer {
    #[must_use]
    pub fn new(name: &'static str, schema: SchemaRef, budget: MemoryBudget) -> Self {
        Self {
            name,
            schema,
            segments: VecDeque::new(),
            durable_through: Lsn::new(0),
            budget,
            bytes: 0,
        }
    }

    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub fn segments(&self) -> usize {
        self.segments.len()
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        self.segments.iter().map(|s| s.batch.num_rows()).sum()
    }

    #[must_use]
    pub const fn durable_through(&self) -> Lsn {
        self.durable_through
    }

    /// The furthest position this tier holds.
    #[must_use]
    pub fn held_through(&self) -> Lsn {
        self.segments
            .back()
            .map_or(self.durable_through, |s| s.coverage.end_inclusive())
    }

    /// The earliest position this tier actually holds.
    ///
    /// Distinct from the durable frontier. A tier that started mid-stream — which is
    /// the normal case for a table onboarded from a running stream — holds nothing
    /// below its first segment, however far behind publication is.
    #[must_use]
    pub fn held_from(&self) -> Lsn {
        self.segments
            .front()
            .map_or(self.durable_through, |s| s.coverage.start_exclusive())
    }

    /// Coverage to offer the splice planner, or `None` when the tier adds nothing.
    ///
    /// Two separate trims apply, and conflating them is a bug this code has already
    /// had once:
    ///
    /// - **Trimmed up to the durable frontier**, so the tier abuts the published tier
    ///   exactly rather than overlapping it. The planner rejects overlapping tiers
    ///   rather than guessing which one to believe.
    /// - **Trimmed down to what is actually held.** A tier whose oldest segment starts
    ///   above the frontier does *not* cover the space in between, and must not say it
    ///   does. Declaring from the frontier regardless would make the splice a proof of
    ///   nothing: the planner would find an exact cover, the query would be answered,
    ///   and the positions in the gap would simply be missing from the result.
    ///
    /// The second case is not hypothetical. A table onboarded from a running stream
    /// starts mid-stream by construction, and its arrival tier holds nothing before its
    /// first captured transaction. Reporting that honestly is what makes the read path
    /// refuse the query instead of answering it short.
    #[must_use]
    pub fn coverage(&self) -> Option<LsnRange> {
        let held = self.held_through();
        let from = self.held_from().max(self.durable_through);
        if held <= from {
            return None;
        }
        LsnRange::new(from, held)
    }

    /// Append an applied batch.
    ///
    /// # Panics
    ///
    /// Never. A batch whose schema disagrees is refused through the return value.
    pub fn append(&mut self, batch: RecordBatch, coverage: LsnRange) -> Admission {
        let bytes = batch.get_array_memory_size();

        if self.bytes.saturating_add(bytes) > self.budget.hard_limit {
            return Admission::Refused {
                bytes: self.bytes,
                hard_limit: self.budget.hard_limit,
                durable_through: self.durable_through,
                held_through: self.held_through(),
            };
        }

        self.bytes = self.bytes.saturating_add(bytes);
        self.segments.push_back(Segment {
            batch,
            coverage,
            bytes,
        });

        if self.bytes > self.budget.soft_limit {
            Admission::AcceptedUnderPressure {
                bytes: self.bytes,
                soft_limit: self.budget.soft_limit,
            }
        } else {
            Admission::Accepted
        }
    }

    /// Record that a durable tier now covers through `through`, and release what it
    /// makes redundant.
    ///
    /// A frontier that moves backwards is ignored rather than treated as an error:
    /// publication reports can arrive out of order, and the frontier is a high-water
    /// mark. Acting on a stale report would release nothing and re-hold nothing, but
    /// *lowering* the frontier would make already-released data appear uncovered.
    pub fn note_durable(&mut self, through: Lsn) -> Released {
        if through > self.durable_through {
            self.durable_through = through;
        }

        let mut released = Released {
            segments: 0,
            rows: 0,
            bytes: 0,
            straddling_retained: 0,
        };

        while let Some(front) = self.segments.front() {
            if front.coverage.end_inclusive() <= self.durable_through {
                let segment = self.segments.pop_front().expect("just inspected");
                self.bytes = self.bytes.saturating_sub(segment.bytes);
                released.segments += 1;
                released.rows += segment.batch.num_rows();
                released.bytes += segment.bytes;
            } else {
                if front.coverage.start_exclusive() < self.durable_through {
                    released.straddling_retained = 1;
                }
                break;
            }
        }

        released
    }

    /// Batches answering a query as of `target`.
    ///
    /// Filtered per row to `durable_through < lsn <= target`. Per-segment filtering
    /// would double-count a segment straddling the durable frontier against the
    /// published tier, and would over-report one straddling the target.
    ///
    /// # Errors
    ///
    /// Returns an error if a batch is missing the commit-position column or it has an
    /// unexpected type, which would mean an unapplied batch reached this tier.
    pub fn scan(&self, target: Lsn) -> Result<Vec<RecordBatch>, ScanError> {
        let floor = self.durable_through;
        let mut out = Vec::new();

        for segment in &self.segments {
            // Whole segments that need no filtering are the common case and are passed
            // through untouched — filtering costs a copy of every column.
            if segment.coverage.start_exclusive() >= floor
                && segment.coverage.end_inclusive() <= target
            {
                out.push(segment.batch.clone());
                continue;
            }
            if segment.coverage.end_inclusive() <= floor
                || segment.coverage.start_exclusive() >= target
            {
                continue;
            }

            let column = segment
                .batch
                .column_by_name(COMMIT_LSN)
                .ok_or(ScanError::MissingCommitColumn)?;
            let lsns: &UInt64Array = column
                .as_primitive_opt()
                .ok_or(ScanError::WrongColumnType)?;

            let mask: Vec<bool> = (0..lsns.len())
                .map(|i| {
                    let lsn = lsns.value(i);
                    lsn > floor.get() && lsn <= target.get()
                })
                .collect();

            if mask.iter().all(|keep| !keep) {
                continue;
            }

            let filtered = arrow::compute::filter_record_batch(
                &segment.batch,
                &arrow_array::BooleanArray::from(mask),
            )
            .map_err(|e| ScanError::Filter(e.to_string()))?;

            if filtered.num_rows() > 0 {
                out.push(filtered);
            }
        }

        Ok(out)
    }

    #[must_use]
    pub fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// Why a scan could not be served.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ScanError {
    /// A batch reached this tier without the commit-position column, which means it was
    /// never applied.
    MissingCommitColumn,
    WrongColumnType,
    Filter(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCommitColumn => write!(
                f,
                "a batch in the arrival tier has no {COMMIT_LSN} column, so it cannot be \
                 positioned; it did not come through the apply path"
            ),
            Self::WrongColumnType => write!(f, "{COMMIT_LSN} is not an unsigned 64-bit column"),
            Self::Filter(e) => write!(f, "filtering the arrival tier failed: {e}"),
        }
    }
}

impl std::error::Error for ScanError {}
