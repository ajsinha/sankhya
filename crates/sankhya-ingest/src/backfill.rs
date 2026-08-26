//! Initial backfill, and the handoff to streaming.
//!
//! # The problem
//!
//! A replication slot only carries changes that occur *after* it exists. A table
//! already holding a hundred million rows would otherwise appear empty in the
//! analytical tier until every one of those rows happened to be touched.
//!
//! # The hard part is not the copy
//!
//! Reading existing rows is straightforward. The difficulty is the **handoff**: the
//! boundary between "rows as they were" and "changes since then" must be exact, in
//! both directions.
//!
//! - Overlap duplicates rows, and the duplicates look entirely plausible.
//! - A gap loses them, and the loss is invisible because what remains is internally
//!   consistent.
//!
//! # How the boundary is made exact
//!
//! The slot is created **first**, and it reports the position from which it will begin
//! streaming. The snapshot is then read *at that same position*. Every row in the
//! snapshot is therefore as it stood at exactly the point the stream begins, and every
//! change the stream carries happened strictly afterwards.
//!
//! The two are adjacent with no overlap and no gap — the same contiguity property the
//! read path relies on, applied to a different seam.

use sankhya_types::{Lsn, LsnRange};
use std::fmt;

/// Where a backfill will read from, and what the stream will pick up.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BackfillPlan {
    pub table: String,
    /// The position the snapshot represents.
    ///
    /// Rows are read as they stood here, and the stream carries everything after.
    pub snapshot_position: Lsn,
}

impl BackfillPlan {
    #[must_use]
    pub fn new(table: impl Into<String>, snapshot_position: Lsn) -> Self {
        Self { table: table.into(), snapshot_position }
    }

    /// What the snapshot covers: everything up to and including its position.
    #[must_use]
    pub fn coverage(&self) -> LsnRange {
        LsnRange::up_to(self.snapshot_position)
    }
}

/// A completed backfill.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Backfill {
    pub plan: BackfillPlan,
    pub rows: u64,
}

/// The two halves, proven adjacent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Handoff {
    /// What the snapshot supplied.
    pub snapshot: LsnRange,
    /// What the stream supplies from here on.
    pub stream: LsnRange,
}

impl Handoff {
    /// Whether the two halves abut exactly.
    ///
    /// Checked rather than assumed, because both failure modes are silent: an overlap
    /// duplicates rows that look plausible, and a gap loses rows leaving a dataset that
    /// is internally consistent.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.snapshot.abuts(self.stream) && !self.snapshot.overlaps(self.stream)
    }
}

/// Why a handoff cannot be trusted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandoffError {
    /// The stream begins before the snapshot ends: rows would be applied twice.
    Overlap { snapshot_through: Lsn, stream_from: Lsn },
    /// The stream begins after the snapshot ends: changes in between are lost.
    ///
    /// This is the outcome of creating the slot *after* reading the snapshot, which is
    /// the natural order to write the code in and the wrong one.
    Gap { snapshot_through: Lsn, stream_from: Lsn },
}

impl fmt::Display for HandoffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overlap { snapshot_through, stream_from } => write!(
                f,
                "the stream begins at {stream_from} but the snapshot already covers \
                 through {snapshot_through}; rows in the overlap would be applied twice"
            ),
            Self::Gap { snapshot_through, stream_from } => write!(
                f,
                "the snapshot covers through {snapshot_through} but the stream begins \
                 at {stream_from}; changes in between reach neither half. Create the \
                 slot before reading the snapshot, not after"
            ),
        }
    }
}

impl std::error::Error for HandoffError {}

/// Verify that a snapshot and a stream meet exactly.
///
/// `stream_from` is the position the slot reports it will begin from — exclusive, since
/// the stream carries changes strictly after it.
///
/// # Errors
///
/// Returns [`HandoffError`] if the two halves overlap or leave a gap. The caller must
/// treat either as fatal: proceeding would silently duplicate or lose rows, and neither
/// is detectable afterwards from the data alone.
pub fn plan_handoff(snapshot: &BackfillPlan, stream_from: Lsn) -> Result<Handoff, HandoffError> {
    let snapshot_through = snapshot.snapshot_position;

    if stream_from < snapshot_through {
        return Err(HandoffError::Overlap { snapshot_through, stream_from });
    }
    if stream_from > snapshot_through {
        return Err(HandoffError::Gap { snapshot_through, stream_from });
    }

    Ok(Handoff {
        snapshot: LsnRange::up_to(snapshot_through),
        // Open-ended in practice; represented here as beginning where the snapshot ends.
        stream: LsnRange::new(snapshot_through, snapshot_through).unwrap_or_else(|| {
            LsnRange::up_to(snapshot_through)
        }),
    })
}

/// Extend a handoff's stream half to a position now reached.
///
/// Kept separate so the adjacency check happens once, at the seam, rather than being
/// re-derived every time the stream advances.
#[must_use]
pub fn advance_stream(handoff: &Handoff, to: Lsn) -> Option<Handoff> {
    let stream = LsnRange::new(handoff.snapshot.end_inclusive(), to)?;
    Some(Handoff { snapshot: handoff.snapshot, stream })
}
