//! Pipeline state and the publish cycle.

use sankhya_cdc_apply::{BatchPolicy, Batcher, MutationPlan};
use sankhya_cdc_model::{Decoder, Message, RelationDescriptor};
use sankhya_error::{Error, Result};
use sankhya_publish::Publication;
use sankhya_schema::{
    classify_change, onboard_relation, Compatibility, Onboarded, OnboardingWarning,
};
use sankhya_table::{encode_batch, WriterConfig};
use sankhya_table_delta::{read_actions as delta_read_actions, Action as DeltaAction};
use sankhya_types::Lsn;

/// How many times a publish will rebase before giving up.
///
/// Bounded so a runaway committer produces a diagnosable failure rather than a capture
/// pipeline that appears to hang. Sixteen is far beyond any plausible contention: the
/// only other committer is maintenance, which commits on a duty cycle.
const REBASE_ATTEMPTS: usize = 16;

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
    /// Tables stopped because a schema change could not be applied safely.
    pub tables_quarantined: usize,
    /// Compatible schema changes applied without operator involvement.
    pub schema_changes_applied: usize,
    /// Events discarded because their table is quarantined.
    ///
    /// Counted rather than silent: this is real data not reaching the analytical tier,
    /// and an operator needs to know how much before deciding how to resolve it.
    pub dead_lettered: usize,
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
    /// Set when an incompatible schema change arrived.
    ///
    /// A quarantined table stops publishing but its last consistent version remains
    /// queryable, and — critically — its events keep being consumed so the replication
    /// cursor advances. With a single slot there is one cursor, and a quarantined table
    /// holding it back would grow retained log without bound until the source's volume
    /// filled. Quarantine must never become a source-safety problem.
    quarantine: Option<String>,
    /// The shape the source moved to, kept so an operator can adopt it.
    ///
    /// Without this, clearing a quarantine would leave the table expecting the old
    /// shape while the source sends the new one — every subsequent row would fail to
    /// encode. Resolving a quarantine has to carry a decision, not merely silence a
    /// complaint.
    pending_schema: Option<Onboarded>,
    /// Events discarded while quarantined, so the loss is visible rather than silent.
    dead_lettered: usize,
    /// Times a publish lost a version race and had to rebase.
    ///
    /// Expected and healthy: maintenance commits to the same log, so a compaction
    /// between two publishes takes the version capture was about to use. A count that
    /// climbs steadily means something is committing far more often than it should.
    rebases: usize,
    /// The next version to commit to this table's log.
    ///
    /// Zero means the table has no log yet, so the first publish also creates it. Held
    /// per table because each table has its own log — which is what makes a table
    /// independently readable by an external engine, and what stops one table's
    /// quarantine from blocking another's publications.
    next_version: u64,
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
            next_version: 0,
            rebases: 0,
            quarantine: None,
            pending_schema: None,
            dead_lettered: 0,
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
        self.tables
            .values()
            .map(|t| t.onboarded.location.source_table.as_str())
            .collect()
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

        // A truncation names its own relations and reaches them directly, not through the
        // transaction fan-out below: it is not a boundary, and sending it to every table would
        // quarantine tables the source did not truncate.
        //
        // `ING-05`: this used to fall into the batcher's `_ => {}`. The source table is
        // emptied and the analytical copy keeps every row — permanent, silent divergence,
        // with no counter. A test asserted the decoder *parses* truncate; nothing asserted
        // that anything acted on it.
        //
        // Quarantined rather than applied. The analytical copy is an unfolded change log with
        // no key-based fold, so there is no "delete every row" this pipeline can emit — and
        // inventing one would be worse than saying so. Quarantine stops publication, leaves
        // the last consistent version queryable, and keeps consuming events so the
        // replication cursor still advances, which is the rule a quarantine has to obey.
        if let Message::Truncate { relation_ids, .. } = message {
            for relation_id in relation_ids {
                if let Some(state) = self.tables.get_mut(relation_id) {
                    if state.quarantine.is_none() {
                        state.quarantine = Some(
                            "the source truncated this table. Every row it held is gone there \
                             and still present here, and this pipeline appends changes rather \
                             than folding them, so it has no way to apply a truncation. \
                             Reload the table, or exclude it"
                                .to_owned(),
                        );
                        self.stats.tables_quarantined =
                            self.stats.tables_quarantined.saturating_add(1);
                    }
                }
            }
            return Ok(());
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
                if state.quarantine.is_some() {
                    // Consumed and discarded, not buffered. The cursor must keep
                    // advancing or retained log grows without bound — a quarantined
                    // table must never become a source-safety problem.
                    state.dead_lettered = state.dead_lettered.saturating_add(1);
                    self.stats.dead_lettered = self.stats.dead_lettered.saturating_add(1);
                    return Ok(());
                }
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
            return self.evolve(relation);
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

    /// Handle a repeated relation description: the shape may have changed.
    ///
    /// Additive and widening changes apply automatically. Anything whose intent cannot
    /// be inferred from the stream quarantines the table rather than being guessed at.
    fn evolve(&mut self, relation: &RelationDescriptor) -> Result<()> {
        let incoming = match onboard_relation(relation) {
            Ok(onboarded) => onboarded,
            Err(e) => {
                // The new shape cannot be represented at all. Quarantine rather than
                // continue writing the old shape, which would silently diverge from
                // the source.
                if let Some(state) = self.tables.get_mut(&relation.relation_id) {
                    state.quarantine = Some(format!("the new shape cannot be carried: {e}"));
                    // Deliberately no pending shape: there is nothing to adopt, so the
                    // only resolutions are to change the source or exclude the table.
                    state.pending_schema = None;
                }
                self.stats.tables_quarantined = self.stats.tables_quarantined.saturating_add(1);
                return Ok(());
            }
        };

        let Some(state) = self.tables.get(&relation.relation_id) else {
            return Ok(());
        };
        let classified = classify_change(&state.onboarded.schema, &incoming.schema);

        // Everything captured under the old shape is published **before** the new one is
        // adopted, so no batch spans two schemas and the log's metadata always describes the
        // files that precede it. Forced, because a partial batch under the old shape must not
        // wait for a size threshold that the new shape will never contribute to.
        if matches!(classified, Compatibility::Compatible { .. }) {
            self.publish(true)?;
        }

        let Some(state) = self.tables.get_mut(&relation.relation_id) else {
            return Ok(());
        };

        match classified {
            Compatibility::Unchanged => {}
            Compatibility::Compatible { changes } => {
                // The change reaches the **log**, not only this process's memory.
                //
                // `FMT-01`: this used to update `state.onboarded` and increment the counter,
                // and `Publication::create` --- the only writer of `schemaString` --- is gated
                // on the table being new. So the added column's data was encoded, written into
                // Parquet, and unreachable by every query for ever, while the metric said the
                // change had been applied. Compaction then refused inputs that do not share a
                // schema, so that partition's compaction failed on every tick, silently.
                //
                // A table with no log yet is not evolved and does not need to be: the `create`
                // that follows will carry the new shape, and there are no files under the old
                // one to be orphaned by it.
                let directory = self.warehouse.join(state.onboarded.location.relative_path());
                if sankhya_publish::is_table(&directory) {
                    let publication = Publication::external(
                        &directory,
                        state.onboarded.location.source_table.clone(),
                    )
                    .writing_with(self.writer);
                    let at = if state.next_version == 0 {
                        publication.next_version()
                    } else {
                        state.next_version
                    };
                    publication
                        .evolve(at, &incoming.schema.arrow_schema())
                        .map_err(|e| {
                            Error::InvariantViolated(format!(
                                "adopting the new shape of {}: {e}",
                                state.onboarded.location.source_table
                            ))
                        })?;
                    state.next_version = at.saturating_add(1);
                }
                state.onboarded = incoming;
                self.stats.schema_changes_applied = self
                    .stats
                    .schema_changes_applied
                    .saturating_add(changes.len());
            }
            Compatibility::Incompatible { reason, .. } => {
                state.quarantine = Some(reason);
                state.pending_schema = Some(incoming);
                self.stats.tables_quarantined = self.stats.tables_quarantined.saturating_add(1);
            }
        }
        Ok(())
    }

    /// Whether a table is quarantined, and why.
    #[must_use]
    pub fn quarantine_reason(&self, relation_id: u32) -> Option<&str> {
        self.tables
            .get(&relation_id)
            .and_then(|s| s.quarantine.as_deref())
    }

    /// Events discarded while a table was quarantined.
    #[must_use]
    pub fn dead_lettered(&self, relation_id: u32) -> usize {
        self.tables.get(&relation_id).map_or(0, |s| s.dead_lettered)
    }

    /// Resolve a quarantine by adopting the shape the source moved to.
    ///
    /// This is the explicit operator action, and it carries a decision rather than
    /// merely silencing a complaint. Clearing the quarantine without adopting the new
    /// shape would leave the table expecting the old one while the source sends the
    /// new — every subsequent row would fail to encode, turning a schema problem into
    /// an ingest outage.
    ///
    /// Returns `false` when there is no shape to adopt, which happens when the new
    /// shape could not be represented at all. The only resolutions then are to change
    /// the source or exclude the table, and neither is something the pipeline can do.
    ///
    /// Data written under the previous shape is untouched; the two shapes coexist as
    /// separate files, which is what makes adopting a new shape cheap.
    pub fn adopt_pending_schema(&mut self, relation_id: u32) -> bool {
        let Some(state) = self.tables.get_mut(&relation_id) else {
            return false;
        };
        let Some(pending) = state.pending_schema.take() else {
            return false;
        };
        state.onboarded = pending;
        state.quarantine = None;
        true
    }

    /// The shape the source moved to, if one is waiting to be adopted.
    #[must_use]
    pub fn pending_schema_columns(&self, relation_id: u32) -> Option<usize> {
        self.tables
            .get(&relation_id)
            .and_then(|s| s.pending_schema.as_ref())
            .map(|o| o.schema.fields.len())
    }

    /// Publish every table with a batch ready, or all of them when `force`.
    ///
    /// # Errors
    ///
    /// Returns an error if a batch cannot be encoded or written.
    pub fn publish(&mut self, force: bool) -> Result<Vec<PublishedFile>> {
        let mut published = Vec::new();

        for state in self.tables.values_mut() {
            // A quarantined table stops publishing; its last consistent version
            // remains queryable.
            if state.quarantine.is_some() {
                continue;
            }
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

            let directory = self
                .warehouse
                .join(state.onboarded.location.relative_path());

            // Recover position from the table's own log before naming anything.
            //
            // A restart resets in-memory state, and both counters below are derived
            // rather than remembered — so they must come from the log, which is the only
            // thing that survives. Without this the pipeline would restart at version zero
            // and be told the table already exists.
            //
            // It would also restart the file sequence at zero. That used to mean writing
            // `00000000.parquet` over a file that is still live; since the publisher began
            // appending the version and a per-write token to the name it does not, and the
            // sequence's remaining job is **order**: two files whose numbers repeat and whose
            // only difference is a token nobody can sort are a directory listing that no
            // longer answers "what arrived when".
            //
            // The sequence is the highest ever committed, not the highest still live: a
            // compacted-away file's name must not be reused while readers holding an
            // older snapshot can still resolve it.
            if state.next_version == 0 {
                let history = delta_read_actions(&directory)
                    .map_err(|e| Error::StorageUnavailable(e.to_string()))?;
                if let Some((last_version, _)) = history.last() {
                    state.next_version = last_version.saturating_add(1);
                    let highest = history
                        .iter()
                        .filter_map(|(_, action)| match action {
                            DeltaAction::Add(add) => add
                                .path
                                // The path is relative to the table root and now carries the
                                // partition directory: `sank_data_date=2024-03-01/0000.parquet`.
                                // Parsing the whole thing yields nothing, the sequence
                                // restarts at zero, and the next write overwrites a live
                                // file. That is the failure this recovery exists to prevent,
                                // and partitioning reintroduced it.
                                .rsplit('/')
                                .next()
                                .and_then(|name| name.strip_suffix(".parquet"))
                                // The publisher decides the on-disk name, and it appends the
                                // version it was attempting and a per-write token: what capture
                                // asked to call `00000000.parquet` is committed as
                                // `00000000-v0000002-1a2f3.parquet`. Parsing the whole stem
                                // yields nothing, so the sequence restarts at zero --- which is
                                // the failure the comment above records happening once already,
                                // when partitioning put a directory in front of the name.
                                //
                                // The sequence is the part before the version marker, and it is
                                // taken by splitting on it rather than by a fixed width, so a
                                // caller that names files differently is skipped rather than
                                // misread.
                                .and_then(|stem| stem.split_once("-v").map_or(stem, |(head, _)| head).parse::<u64>().ok()),
                            _ => None,
                        })
                        .max();
                    if let Some(highest) = highest {
                        state.sequence = state.sequence.max(highest.saturating_add(1));
                    }
                }
            }

            // Sequence-numbered rather than time-named, so a replay produces the same
            // file names and the output is reproducible.
            let file_name = format!("{:08}.parquet", state.sequence);
            state.sequence = state.sequence.saturating_add(1);

            // Through `sankhya-publish`, which is the one library that writes to a
            // warehouse.
            //
            // This path used to write its own files and commit its own log, and the
            // consequence was a whole class of table the product could not otherwise
            // produce: no partition columns, flat files, in violation of `FR-STORE-20`,
            // which every table published through the other path satisfies. A second writer
            // is not a stylistic complaint --- it is a path that does not get the guarantees
            // the first one enforces.
            //
            // The date axis is the ingest date, and it says so: `FR-STORE-21` requires a
            // table naming no source column to use the ingest date **and record that it
            // does**. Nothing here can do better --- `Mutation` carries a commit position and
            // no timestamp --- and a per-row guess would make the column mean "when this
            // happened" in some rows and "when we received it" in others.
            let publication = Publication::external(
                &directory,
                state.onboarded.location.source_table.clone(),
            )
            .writing_with(self.writer);
            if state.next_version == 0 && !sankhya_publish::is_table(&directory) {
                publication
                    .create(&state.onboarded.schema.arrow_schema())
                    .map_err(|e| Error::InvariantViolated(e.to_string()))?;
                state.next_version = 1;
            }

            // Rebasing, because capture is not the only committer: a compaction between two
            // publishes takes the version capture was about to use. Failing there would mean
            // maintenance can stop capture, which inverts the ordering rule --- the source
            // outranks maintenance, always.
            let rebased = publication
                .append_rebasing(
                    state.next_version,
                    REBASE_ATTEMPTS,
                    &file_name,
                    &batch,
                    plan.covers_through,
                )
                .map_err(|e| Error::StorageUnavailable(e.to_string()))?;
            state.next_version = rebased.version.saturating_add(1);
            state.rebases = state.rebases.saturating_add(rebased.retries);

            let rows: usize = rebased.written.iter().map(|p| p.rows).sum();
            let bytes: u64 = rebased.written.iter().map(|p| p.bytes).sum();
            let files = rebased.written.len();

            state.published_through = plan.covers_through;

            self.stats.rows_captured = self.stats.rows_captured.saturating_add(rows);
            self.stats.files_published = self
                .stats
                .files_published
                .saturating_add(files);
            self.stats.bytes_published = self.stats.bytes_published.saturating_add(bytes);
            if plan.covers_through > self.stats.applied_through {
                self.stats.applied_through = plan.covers_through;
            }

            // One entry per file, because a batch spanning several dates becomes several
            // files --- one per partition. Reporting only the first would understate what was
            // published, and the caller uses these to reconcile.
            for one in rebased.written {
                published.push(PublishedFile {
                    table: state.onboarded.location.source_table.clone(),
                    // Relative to the table root, which is what the log records and what a
                    // reader resolves against.
                    path: PathBuf::from(one.file),
                    rows: one.rows,
                    bytes: one.bytes,
                    covers_through: plan.covers_through,
                });
            }
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
