//! Pipeline state and the publish cycle.

use sankhya_cdc_apply::{BatchPolicy, Batcher, MutationPlan};
use sankhya_cdc_model::{Decoder, Message, RelationDescriptor};
use sankhya_error::{Error, Result};
use sankhya_schema::{Onboarded, OnboardingWarning, onboard_relation};
use sankhya_table::{WriterConfig, encode_batch, write_parquet};
use sankhya_types::Lsn;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A file the pipeline published.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PublishedFile {
    pub table: String,
    pub path: PathBuf,
    pub rows: usize,
    pub bytes: u64,
    /// The position this file is known to contain, which is what lets the read path
    /// splice it against other tiers.
    pub covers_through: Lsn,
}

/// What the pipeline has done.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PipelineStats {
    pub messages_decoded: usize,
    pub tables_onboarded: usize,
    pub rows_captured: usize,
    pub files_published: usize,
    pub bytes_published: u64,
    /// Mutations refused because a withheld value could not be resolved.
    ///
    /// Non-zero is a defect condition, not a tolerable loss. Surfaced rather than
    /// absorbed so it cannot pass unnoticed.
    pub unresolvable: usize,
    /// Batches skipped entirely because every row in them was already published.
    ///
    /// Expected and healthy after a restart — this is what converts at-least-once
    /// delivery into exactly-once effect. A count that keeps rising during steady
    /// operation, however, means something is replaying that should not be.
    pub batches_skipped_as_duplicate: usize,
    /// Individual rows dropped from a batch that spanned the published boundary.
    ///
    /// Counted separately from whole skipped batches because the two mean different
    /// things: a skipped batch is a clean replay, while a partially filtered batch is
    /// the ordinary case after a restart, where the resent stream rebatches across the
    /// boundary.
    pub rows_skipped_as_duplicate: usize,
    /// The highest position wholly published across every table.
    pub applied_through: Lsn,
}

/// One table's ingest state.
#[derive(Debug)]
pub struct TableState {
    pub onboarded: Onboarded,
    batcher: Batcher,
    sequence: u64,
    /// The furthest position already published for this table.
    ///
    /// Recovered from the table's own commit history rather than from external state,
    /// so there is nothing that can fall out of agreement with the data itself.
    published_through: Lsn,
}

impl TableState {
    fn new(onboarded: Onboarded, policy: BatchPolicy) -> Self {
        Self {
            onboarded,
            batcher: Batcher::new(policy),
            sequence: 0,
            published_through: Lsn::ZERO,
        }
    }
}

/// The capture pipeline.
#[derive(Debug)]
pub struct Pipeline {
    warehouse: PathBuf,
    policy: BatchPolicy,
    writer: WriterConfig,
    decoder: Decoder,
    /// Relation identifier to table state. Keyed by identifier rather than by name
    /// because the identifier is stable across a rename, which is what allows capture
    /// to continue while publication is paused for an operator decision.
    tables: BTreeMap<u32, TableState>,
    /// The transaction currently open, if any.
    ///
    /// Tracked because tables are onboarded lazily, on first sight of their relation
    /// description — which frequently happens *inside* a transaction that has already
    /// begun. A batcher created at that moment would otherwise have no transaction
    /// context, and every row of that transaction would be refused.
    open_transaction: Option<Message>,
    warnings: Vec<OnboardingWarning>,
    stats: PipelineStats,
}

impl Pipeline {
    #[must_use]
    pub fn new(warehouse: impl Into<PathBuf>, policy: BatchPolicy, writer: WriterConfig) -> Self {
        Self {
            warehouse: warehouse.into(),
            policy,
            writer,
            decoder: Decoder::new(),
            tables: BTreeMap::new(),
            open_transaction: None,
            warnings: Vec::new(),
            stats: PipelineStats::default(),
        }
    }

    #[must_use]
    pub const fn stats(&self) -> &PipelineStats {
        &self.stats
    }

    /// Onboarding observations worth an operator's attention.
    #[must_use]
    pub fn warnings(&self) -> &[OnboardingWarning] {
        &self.warnings
    }

    #[must_use]
    pub fn table_names(&self) -> Vec<&str> {
        self.tables.values().map(|t| t.onboarded.location.source_table.as_str()).collect()
    }

    /// Feed one raw message.
    ///
    /// # Errors
    ///
    /// Returns an error if the message cannot be decoded, or if a relation it describes
    /// cannot be onboarded. A relation that cannot be onboarded stops the pipeline
    /// rather than being skipped: silently ignoring a table would mean its data is
    /// absent from the analytical tier with nothing recording why.
    pub fn accept_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let (message, consumed) = self
            .decoder
            .decode_prefix(bytes)
            .map_err(|e| Error::InvariantViolated(format!("decoding a captured message: {e}")))?;

        if consumed != bytes.len() {
            // Under-consumption desynchronises a live stream; over-consumption swallows
            // the next message. Either is a decoder defect, not a data problem.
            return Err(Error::InvariantViolated(format!(
                "decoder consumed {consumed} of {} bytes",
                bytes.len()
            )));
        }

        self.stats.messages_decoded = self.stats.messages_decoded.saturating_add(1);
        self.accept(&message)
    }

    /// Feed one decoded message.
    ///
    /// # Errors
    ///
    /// As [`Pipeline::accept_bytes`].
    pub fn accept(&mut self, message: &Message) -> Result<()> {
        if let Message::Relation(relation) = message {
            self.onboard(relation)?;
        }

        // Transaction boundaries must reach every table's batcher, because a
        // transaction may span several tables and each must seal at the same position.
        if matches!(
            message,
            Message::Begin { .. }
                | Message::Commit { .. }
                | Message::StreamStart { .. }
                | Message::StreamCommit { .. }
                | Message::StreamAbort { .. }
        ) {
            // Remember an opening boundary so a table onboarded later in this same
            // transaction can be given the context it missed.
            self.open_transaction = match message {
                Message::Begin { .. } | Message::StreamStart { .. } => Some(message.clone()),
                _ => None,
            };
            for state in self.tables.values_mut() {
                state.batcher.accept(message, None);
            }
            return Ok(());
        }

        // A row message goes only to the table it belongs to.
        if let Some(relation_id) = message.relation_id() {
            if let Some(state) = self.tables.get_mut(&relation_id) {
                let before = state.batcher.unresolvable();
                state.batcher.accept(message, None);
                let added = state.batcher.unresolvable().saturating_sub(before);
                self.stats.unresolvable = self.stats.unresolvable.saturating_add(added);
            }
        }
        Ok(())
    }

    fn onboard(&mut self, relation: &RelationDescriptor) -> Result<()> {
        if self.tables.contains_key(&relation.relation_id) {
            // A repeated description means the shape may have changed. Handling that
            // is schema evolution, which is deliberately not silently absorbed here.
            return Ok(());
        }
        let onboarded = onboard_relation(relation).map_err(|e| {
            Error::UnsupportedType(format!(
                "{}.{} cannot be onboarded: {e}",
                relation.namespace, relation.name
            ))
        })?;
        self.warnings.extend(onboarded.warnings.iter().cloned());

        let mut state = TableState::new(onboarded, self.policy);
        // Replay the opening boundary into the new batcher.
        //
        // Without this, every row of the transaction that introduced the table is
        // refused for having no transaction context — which is exactly one table's
        // worth of silent loss, per table, on first sight. The interleaved multi-table
        // test exists to catch this; a single-table test cannot, because the first
        // transaction there contains nothing but that table.
        if let Some(open) = &self.open_transaction {
            state.batcher.accept(open, None);
        }
        self.tables.insert(relation.relation_id, state);
        self.stats.tables_onboarded = self.stats.tables_onboarded.saturating_add(1);
        Ok(())
    }

    /// Publish every table with a batch ready, or all of them when `force`.
    ///
    /// # Errors
    ///
    /// Returns an error if a batch cannot be encoded or written.
    pub fn publish(&mut self, force: bool) -> Result<Vec<PublishedFile>> {
        let mut published = Vec::new();

        for state in self.tables.values_mut() {
            if !force && state.batcher.due().is_none() {
                continue;
            }
            let plan = state.batcher.flush();
            if plan.is_empty() {
                continue;
            }

            // Idempotence, at ROW granularity rather than batch granularity.
            //
            // Delivery is at-least-once: after a crash the source resends everything
            // since the last confirmed position, so already-published work arrives
            // again. Publishing it twice duplicates rows, and row counts alone still
            // look plausible against a source that has itself grown.
            //
            // Filtering must be per row, not per batch. A resent stream does not
            // rebatch identically — the restarted pipeline sees a different message
            // boundary — so a batch routinely spans both already-published and new
            // positions. Skipping only wholly-old batches would republish every row
            // in such a batch, which is exactly what the crash tests found.
            //
            // The comparison is of positions rather than of content, because positions
            // are monotonic and content is not.
            let floor = state.published_through;
            let before = plan.mutations.len();
            let mutations: Vec<_> = plan
                .mutations
                .into_iter()
                .filter(|m| m.commit_lsn > floor)
                .collect();

            if mutations.is_empty() {
                self.stats.batches_skipped_as_duplicate =
                    self.stats.batches_skipped_as_duplicate.saturating_add(1);
                continue;
            }
            if mutations.len() < before {
                // A partial replay: some of this batch was already durable.
                self.stats.rows_skipped_as_duplicate = self
                    .stats
                    .rows_skipped_as_duplicate
                    .saturating_add(before - mutations.len());
            }
            let plan = MutationPlan {
                mutations,
                covers_through: plan.covers_through,
                transaction_count: plan.transaction_count,
            };

            let batch = encode_batch(&state.onboarded.schema, &plan.mutations)
                .map_err(|e| Error::InvariantViolated(format!("encoding a batch: {e}")))?;

            let directory = self.warehouse.join(state.onboarded.location.relative_path());
            // Sequence-numbered rather than time-named, so a replay produces the same
            // file names and the output is reproducible.
            let file_name = format!("{:08}.parquet", state.sequence);
            state.sequence = state.sequence.saturating_add(1);

            let report =
                write_parquet(&directory, &file_name, &batch, plan.covers_through, self.writer)?;

            state.published_through = plan.covers_through;

            self.stats.rows_captured = self.stats.rows_captured.saturating_add(report.rows);
            self.stats.files_published = self.stats.files_published.saturating_add(1);
            self.stats.bytes_published = self.stats.bytes_published.saturating_add(report.bytes);
            if plan.covers_through > self.stats.applied_through {
                self.stats.applied_through = plan.covers_through;
            }

            published.push(PublishedFile {
                table: state.onboarded.location.source_table.clone(),
                path: report.path,
                rows: report.rows,
                bytes: report.bytes,
                covers_through: plan.covers_through,
            });
        }
        Ok(published)
    }

    /// Rows sealed but not yet published, across every table.
    #[must_use]
    pub fn pending_rows(&self) -> usize {
        self.tables.values().map(|t| t.batcher.sealed_rows()).sum()
    }

    /// Rows belonging to transactions still in flight. These are never published.
    #[must_use]
    pub fn open_rows(&self) -> usize {
        self.tables.values().map(|t| t.batcher.open_rows()).sum()
    }

    /// The warehouse root.
    #[must_use]
    pub fn warehouse(&self) -> &Path {
        &self.warehouse
    }

    /// Restore the published position for a table, as recovery would.
    ///
    /// In a running system this comes from the table's own commit metadata. It is
    /// exposed so a restart can be simulated exactly, rather than approximated.
    pub fn restore_published_position(&mut self, relation_id: u32, through: Lsn) {
        if let Some(state) = self.tables.get_mut(&relation_id) {
            state.published_through = through;
        }
    }

    /// The furthest position published for a table.
    #[must_use]
    pub fn published_through(&self, relation_id: u32) -> Option<Lsn> {
        self.tables.get(&relation_id).map(|s| s.published_through)
    }
}
