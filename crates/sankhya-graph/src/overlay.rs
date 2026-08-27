//! Applying new rows to a live graph without rebuilding it.
//!
//! A full rehydration is a linear pass over the whole table. On a graph that changes
//! constantly and is queried constantly, doing that per change is not affordable --- and
//! doing it rarely means the graph is always stale.
//!
//! So changes accumulate in a **copy-on-write overlay**: a small, separately built epoch
//! layered over the base one. Queries read both. When the overlay grows past a threshold
//! fraction of the base, it stops being cheaper than a rebuild and a rebuild is triggered.
//!
//! # The property that decides whether any of this is safe
//!
//! `FR-GRAPH-09` names it directly: **incremental application and full rehydration must
//! produce identical graphs**. Not equivalent, not close --- identical, so the two are
//! interchangeable and nobody has to reason about which one answered.
//!
//! It is the single most valuable test in this tier because incremental hydration is where
//! the subtle defects live, and because they are invisible: an overlay that drops an edge
//! produces a graph that is smaller than it should be and wrong in no way anyone can see.
//! The test is in `tests/incremental.rs` and it compares the two structures edge by edge
//! rather than comparing counts.

use crate::epoch::{Epoch, EpochId};
use crate::hydrate::{Hydration, HydrationError, MemoryBudget};
use crate::spec::GraphSpec;
use arrow_array::RecordBatch;

/// When an overlay stops being worth keeping.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RebuildThreshold {
    /// The fraction of the base epoch's edge count at which to rebuild.
    ///
    /// Above roughly a fifth, reading two structures and merging their results costs more
    /// than the rebuild saves. The exact figure depends on the query mix, so it is a
    /// setting rather than a constant --- but it must be well below one, or the overlay
    /// grows until it *is* the graph and every query pays for both copies.
    pub fraction_of_base: f64,
    /// An absolute edge count at which to rebuild regardless of the fraction.
    ///
    /// Bounds the overlay on a graph whose base is enormous, where a fifth is still far too
    /// many edges to keep in a second structure.
    pub max_edges: usize,
}

impl Default for RebuildThreshold {
    fn default() -> Self {
        Self {
            fraction_of_base: 0.2,
            max_edges: 1_000_000,
        }
    }
}

/// A base epoch with pending changes layered over it.
#[derive(Debug)]
pub struct Overlay {
    spec: GraphSpec,
    budget: MemoryBudget,
    threshold: RebuildThreshold,
    /// Batches absorbed since the base was built, retained so a rebuild can replay them.
    ///
    /// Retained rather than discarded because a rebuild has to produce the same graph the
    /// overlay was presenting, and the only way to guarantee that is to build from the same
    /// rows. Discarding them and re-scanning the table would introduce a window in which
    /// the two disagree.
    pending: Vec<RecordBatch>,
    pending_edges: usize,
}

impl Overlay {
    /// An empty overlay over a base epoch.
    #[must_use]
    pub fn new(spec: GraphSpec, budget: MemoryBudget, threshold: RebuildThreshold) -> Self {
        Self {
            spec,
            budget,
            threshold,
            pending: Vec::new(),
            pending_edges: 0,
        }
    }

    /// Absorb a batch of changes.
    ///
    /// The batch is validated by building it immediately, so a malformed one is refused at
    /// the point it arrives rather than at the next rebuild --- by which time the caller
    /// who could have fixed it is long gone.
    pub fn apply(&mut self, batch: &RecordBatch) -> Result<(), HydrationError> {
        let mut probe = Hydration::new(self.spec.clone(), self.budget);
        probe.absorb(batch)?;
        let epoch = probe.finish(EpochId(0), 0, 0)?;
        self.pending_edges = self.pending_edges.saturating_add(epoch.edge_count());
        self.pending.push(batch.clone());
        Ok(())
    }

    /// How many edges are pending.
    #[must_use]
    pub const fn pending_edges(&self) -> usize {
        self.pending_edges
    }

    /// How many batches are pending.
    #[must_use]
    pub fn pending_batches(&self) -> usize {
        self.pending.len()
    }

    /// Whether the overlay has grown past the point where a rebuild is cheaper.
    #[must_use]
    pub fn needs_rebuild(&self, base: &Epoch) -> bool {
        if self.pending_edges >= self.threshold.max_edges {
            return true;
        }
        #[allow(clippy::cast_precision_loss)]
        let base_edges = base.edge_count() as f64;
        if base_edges <= 0.0 {
            return !self.pending.is_empty();
        }
        #[allow(clippy::cast_precision_loss)]
        let pending = self.pending_edges as f64;
        pending / base_edges >= self.threshold.fraction_of_base
    }

    /// Build a fresh epoch from the base's rows plus everything pending.
    ///
    /// `base_batches` are the rows the base epoch was built from. Replaying them alongside
    /// the pending ones is what makes the rebuilt epoch identical to what a full
    /// rehydration of the current table would produce --- and identical is the requirement,
    /// because anything less means the answer depends on which path built the graph.
    pub fn rebuild(
        &mut self,
        base_batches: &[RecordBatch],
        id: EpochId,
        snapshot: u64,
        built_at: i64,
    ) -> Result<Epoch, HydrationError> {
        let mut hydration = Hydration::new(self.spec.clone(), self.budget);
        for batch in base_batches.iter().chain(self.pending.iter()) {
            hydration.absorb(batch)?;
        }
        let epoch = hydration.finish(id, snapshot, built_at)?;
        self.pending.clear();
        self.pending_edges = 0;
        Ok(epoch)
    }

    /// The pending batches, for a caller that wants to drive the rebuild itself.
    #[must_use]
    pub fn pending(&self) -> &[RecordBatch] {
        &self.pending
    }
}
