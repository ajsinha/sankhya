//! Batching, and the transaction-boundary invariant.

use crate::mutation::{apply_unchanged, Mutation, MutationPlan, Op, Row};
use sankhya_cdc_model::Message;
use sankhya_types::Lsn;
use std::collections::BTreeMap;

/// When to flush.
///
/// The time trigger is **size-gated**. Without that gate a table receiving a trickle
/// emits hundreds of tiny commits a day, spending more on metadata than on data — and
/// metadata cost lands on query *planning*, which is a fixed price paid before any
/// data is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BatchPolicy {
    /// Flush once this many rows have accumulated.
    pub max_rows: usize,
    /// Flush once this many sealed transactions have accumulated.
    pub max_transactions: usize,
    /// Flush on age, but only if at least `min_rows_for_age_flush` have accumulated.
    pub max_age_ticks: u64,
    /// The size gate on the age trigger.
    pub min_rows_for_age_flush: usize,
    /// Flush regardless of size once this age is reached, bounding staleness.
    pub hard_age_ticks: u64,
}

impl Default for BatchPolicy {
    fn default() -> Self {
        Self {
            max_rows: 50_000,
            max_transactions: 5_000,
            max_age_ticks: 120,
            min_rows_for_age_flush: 1_000,
            hard_age_ticks: 900,
        }
    }
}

/// Why a flush happened. Reported so the adaptive control loop has a signal, and so an
/// operator can tell a healthy cadence from a pathological one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlushReason {
    RowCount,
    TransactionCount,
    Age,
    HardAge,
    Requested,
}

/// Accumulates decoded events into transaction-aligned batches.
///
/// # The invariant
///
/// A batch is a set of *whole* transactions. A transaction is sealed only by its
/// commit, and only sealed transactions are eligible to flush; an in-flight
/// transaction is carried forward. Splitting one would publish half a transaction,
/// which for any multi-table write is a torn read that no downstream consumer could
/// detect.
#[derive(Debug)]
pub struct Batcher {
    policy: BatchPolicy,
    /// Sealed and ready to publish.
    sealed: Vec<Mutation>,
    /// Keyed by transaction, still in flight.
    open: BTreeMap<u32, Vec<Mutation>>,
    /// The transaction currently being read, for the non-streamed path where row
    /// messages carry no transaction identifier of their own.
    current: Option<u32>,
    sealed_transactions: usize,
    covers_through: Lsn,
    age_ticks: u64,
    /// Rows whose withheld values could not be resolved.
    unresolvable: usize,
}

impl Batcher {
    #[must_use]
    pub fn new(policy: BatchPolicy) -> Self {
        Self {
            policy,
            sealed: Vec::new(),
            open: BTreeMap::new(),
            current: None,
            sealed_transactions: 0,
            covers_through: Lsn::ZERO,
            age_ticks: 0,
            unresolvable: 0,
        }
    }

    /// Rows sealed and ready to publish.
    #[must_use]
    pub fn sealed_rows(&self) -> usize {
        self.sealed.len()
    }

    /// Rows belonging to transactions still in flight. These are never published.
    #[must_use]
    pub fn open_rows(&self) -> usize {
        self.open.values().map(Vec::len).sum()
    }

    /// Rows dropped because a withheld value could not be resolved.
    ///
    /// Non-zero is a defect condition, not a tolerable loss: it means an update
    /// arrived for a row the applier does not have. Surfaced rather than absorbed.
    #[must_use]
    pub const fn unresolvable(&self) -> usize {
        self.unresolvable
    }

    /// Advance the logical clock. Injected rather than read, so batching behaviour is
    /// reproducible in tests.
    pub fn tick(&mut self) {
        self.age_ticks = self.age_ticks.saturating_add(1);
    }

    /// Feed one decoded message.
    ///
    /// `current_rows` supplies the present value of a row being updated, used only to
    /// resolve withheld values.
    pub fn accept(&mut self, message: &Message, current_rows: Option<&Row>) {
        match message {
            Message::Begin { xid, .. } => {
                self.current = Some(*xid);
                self.open.entry(*xid).or_default();
            }
            Message::StreamStart { xid, .. } => {
                self.current = Some(*xid);
                self.open.entry(*xid).or_default();
            }
            Message::Commit { end_lsn, .. } => {
                if let Some(xid) = self.current.take() {
                    self.seal(xid, *end_lsn);
                }
            }
            Message::StreamCommit { xid, end_lsn, .. } => {
                self.seal(*xid, *end_lsn);
                if self.current == Some(*xid) {
                    self.current = None;
                }
            }
            // An aborted transaction leaves nothing behind. Discarding here rather
            // than filtering later is what keeps the invariant simple.
            Message::StreamAbort { xid, .. } => {
                self.open.remove(xid);
                if self.current == Some(*xid) {
                    self.current = None;
                }
            }
            Message::Insert { relation_id, new } => {
                self.push_row(*relation_id, Op::Insert, new, current_rows);
            }
            Message::Update {
                relation_id, new, ..
            } => {
                self.push_row(*relation_id, Op::Update, new, current_rows);
            }
            Message::Delete {
                relation_id, old, ..
            } => {
                self.push_row(*relation_id, Op::Delete, old, current_rows);
            }
            _ => {}
        }
    }

    fn push_row(
        &mut self,
        relation_id: u32,
        op: Op,
        tuple: &sankhya_cdc_model::TupleData,
        current: Option<&Row>,
    ) {
        let Some(xid) = self.current else {
            // A row outside a transaction is a protocol violation. Counting rather
            // than silently discarding, so it surfaces.
            self.unresolvable = self.unresolvable.saturating_add(1);
            return;
        };
        match apply_unchanged(tuple, current) {
            Some(row) => {
                self.open.entry(xid).or_default().push(Mutation {
                    relation_id,
                    op,
                    row,
                    commit_lsn: Lsn::ZERO, // stamped at seal, when the position is known
                });
            }
            // A withheld value with nothing to resolve against. Refusing is the only
            // safe option: writing a null here would silently destroy the column.
            None => self.unresolvable = self.unresolvable.saturating_add(1),
        }
    }

    fn seal(&mut self, xid: u32, end_lsn: Lsn) {
        let Some(mut rows) = self.open.remove(&xid) else {
            return;
        };
        for row in &mut rows {
            row.commit_lsn = end_lsn;
        }
        self.sealed.append(&mut rows);
        self.sealed_transactions = self.sealed_transactions.saturating_add(1);
        if end_lsn > self.covers_through {
            self.covers_through = end_lsn;
        }
    }

    /// Whether a flush is due, and why.
    #[must_use]
    pub fn due(&self) -> Option<FlushReason> {
        if self.sealed.is_empty() {
            return None;
        }
        if self.sealed.len() >= self.policy.max_rows {
            return Some(FlushReason::RowCount);
        }
        if self.sealed_transactions >= self.policy.max_transactions {
            return Some(FlushReason::TransactionCount);
        }
        if self.age_ticks >= self.policy.hard_age_ticks {
            return Some(FlushReason::HardAge);
        }
        // The size gate: age alone does not flush a trickle.
        if self.age_ticks >= self.policy.max_age_ticks
            && self.sealed.len() >= self.policy.min_rows_for_age_flush
        {
            return Some(FlushReason::Age);
        }
        None
    }

    /// Take everything sealed, leaving in-flight transactions untouched.
    ///
    /// The returned plan contains only whole transactions, and its coverage extends
    /// exactly to the last one sealed — which is what lets the tier declare an
    /// interval the read path can splice against.
    pub fn flush(&mut self) -> MutationPlan {
        let plan = MutationPlan {
            mutations: std::mem::take(&mut self.sealed),
            covers_through: self.covers_through,
            transaction_count: self.sealed_transactions,
        };
        self.sealed_transactions = 0;
        self.age_ticks = 0;
        plan
    }
}
