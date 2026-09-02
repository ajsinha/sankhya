//! Writing a table, with the invariants made unavoidable rather than documented.
//!
//! # What this exists to prevent
//!
//! Every requirement below has a failure mode that is silent. That is the whole reason the
//! library exists rather than a page of instructions:
//!
//! - **A missing required field.** The `add` action's `partitionValues` is non-nullable.
//!   Omitting it produces a log that this system's own reader accepts happily, because a
//!   reader ignores a field it never writes --- and that an independent implementation
//!   rejects on the first read. This system made exactly that mistake, writing its own
//!   format with the specification open.
//! - **Missing statistics.** A file with no recorded statistics cannot be pruned, so every
//!   query reads it. The answers stay correct and the table gets slower and slower, and
//!   nothing anywhere says why.
//! - **A type that does not round-trip.** Publishing something merely similar produces a
//!   table other engines read confidently and wrongly.
//! - **An undeclared class.** A table that does not say what it is defaults to external,
//!   which is safe --- but a *managed* table that forgets to say so silently loses every
//!   guarantee it was supposed to have.
//!
//! # What it deliberately does not do
//!
//! It does not hide the format. The log it writes is ordinary, open, and documented, and
//! anything can read it. What the library provides is not secrecy but *correctness by
//! construction*: there is no way to call it that produces an invalid table.

use crate::class::{configuration, TableClass};
use arrow_array::RecordBatch;
use arrow_schema::{Schema, SchemaRef};
use sankhya_schema::{is_reserved, DateAxis, DateSource, DATA_DATE_COLUMN};
use sankhya_table::{write_parquet, WriterConfig};
use std::collections::BTreeMap;
use std::sync::Arc;
use arrow_array::cast::AsArray;
use arrow_array::{ArrayRef, Date32Array};
use arrow_schema::{DataType, Field};
use sankhya_schema::Granularity;
use arrow_array::types::Date32Type;
use arrow_array::{Array, UInt32Array};
use sankhya_table_delta::{commit, create, schema_string, Action, AddFile, Metadata};
use sankhya_types::Lsn;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How many rebases an append may cost before it is refused as hopeless.
///
/// # Why it is this large, and why a count is a poor bound
///
/// A rebase is not a failure, it is what a contended table is *supposed* to cost: another
/// writer took the version and this one moves on. So the budget has to cover what a healthy
/// race costs, and a healthy race is heavy-tailed.
///
/// Measured on this write path with eight writers appending continuously to one table ---
/// sixteen hundred commits, every one of them real: **a mean of five rebases, a median of
/// three, a p95 of fourteen, a p99 of twenty-five, and a longest of fifty-three.** The tail is
/// far longer than the middle, because which writer wins is decided inside a window a few
/// microseconds wide and a descheduled writer can lose a long run of them.
///
/// The previous budget here was **sixteen**, which sits at the p95: about one append in twenty
/// would have been refused as contention while nothing was wrong. Two hundred and fifty-six is
/// four times the longest measured run, and still bounds a writer that is genuinely being
/// outpaced --- one that has fallen this far behind is not going to catch up by trying again.
///
/// **A backoff was tried and is not here.** Spinning and yielding before each retry, perturbed
/// per writer so losers would not resume in step, moved the mean from 5.0 to 4.5 and moved the
/// p99 from 25 to 35 --- it made the tail *worse*. The tail is not writers colliding in step;
/// it is the scheduler, and no amount of politeness in this loop changes that.
const REBASE_BUDGET: usize = 256;

/// How to publish a table.
#[derive(Clone, Debug)]
pub struct Publication {
    /// Where the table lives.
    pub root: PathBuf,
    /// What it is called.
    pub name: String,
    /// What kind of table it is.
    pub class: TableClass,
    /// The columns identifying a row, for a mutable table. Empty means append-only.
    pub key_columns: Vec<String>,
    /// Where this table's date comes from, and how coarsely it partitions.
    ///
    /// Not optional. Every table has a date axis (ADR-0004), and the only question is
    /// whether it means "when this happened" or "when we received it" --- which is exactly
    /// the question a default would let a publisher avoid answering.
    pub date_axis: DateAxis,
    /// How files are encoded.
    ///
    /// Carried here because the caller that knows the workload knows the row-group size and
    /// the compression level that suit it, and because a publisher that cannot say so would
    /// have to write its own files to choose --- which is the second writer this crate exists
    /// to make unnecessary.
    pub writer: WriterConfig,
    /// The newest version this publication has seen, plus one. Zero means it has not looked.
    ///
    /// Not a counter of the table's state --- the log is that, and a counter held beside it
    /// is a counter that can be wrong about somebody else's commit. This is a **floor for
    /// the probe**: `newest_after` walks forward from a version it is given, and is checked
    /// against the filesystem at every step, so a floor that is stale merely costs a longer
    /// walk and a floor that is wrong is discarded rather than believed.
    ///
    /// Shared across clones, because two clones of one publication are two views of one
    /// table and there is nothing for them to disagree about.
    seen: Arc<AtomicU64>,
    /// The schema this table declared, read once.
    ///
    /// # Why once, and not on every append
    ///
    /// Reading it costs a replay of the whole log, and a check that did that per append would
    /// make the write path **quadratic in the table's own history** --- the defect this
    /// repository has already been bitten by once, and which showed up here as a concurrency
    /// test that stopped meeting its floor within an hour of the check being written.
    ///
    /// Once is also the right semantics rather than merely the cheap one. The check exists to
    /// catch a **writer** sending the wrong shape, which is a property of the writer and not
    /// of the log's latest state; a schema that evolves under a live writer is a different
    /// event, and one this writer's next commit conflict is the honest place to discover.
    declared: Arc<std::sync::OnceLock<Option<Schema>>>,
}

impl Publication {
    /// An external, append-only table at `root`.
    ///
    /// The common case, and the defaults are the conservative ones: external rather than
    /// managed, append-only rather than keyed.
    #[must_use]
    pub fn external(root: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            name: name.into(),
            class: TableClass::External,
            key_columns: Vec::new(),
            // Ingest date until told otherwise, and recorded as such rather than left
            // undeclared, so a reader can tell "this means arrival" from "nobody said".
            date_axis: DateAxis::ingest_date(),
            writer: WriterConfig::default(),
            seen: Arc::new(AtomicU64::new(0)),
            declared: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// The same, encoding files as the caller asks.
    #[must_use]
    pub const fn writing_with(mut self, writer: WriterConfig) -> Self {
        self.writer = writer;
        self
    }

    /// The same, taking each row's date from a source column.
    ///
    /// The column must exist and must be a date. Every row uses it, and a null there is an
    /// error rather than a fallback to today --- a per-row fallback makes the column mean
    /// "when it happened" in some rows and "when we received it" in others, inseparably.
    #[must_use]
    pub fn dated_by(mut self, column: impl Into<String>) -> Self {
        self.date_axis = DateAxis::from_column(column);
        self
    }

    /// The same, at a coarser partition granularity.
    #[must_use]
    pub const fn partitioned_by(mut self, granularity: sankhya_schema::Granularity) -> Self {
        self.date_axis.granularity = granularity;
        self
    }

    /// The same, declaring the columns that identify a row.
    ///
    /// Makes the table mutable: the latest version of each key wins. Nothing infers this,
    /// because a guessed key resolves distinct rows into one and loses the rest --- silently,
    /// and in a way that looks like deduplication working.
    #[must_use]
    pub fn keyed_by(mut self, columns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.key_columns = columns.into_iter().map(Into::into).collect();
        self
    }

    /// Create the table, writing version zero of its log.
    ///
    /// # Errors
    ///
    /// Refuses a schema this system cannot round-trip exactly, naming the column, and
    /// refuses a declared key column that is not in the schema.
    pub fn create(&self, schema: &Schema) -> Result<(), PublishError> {
        // The partition column is part of the table's schema, not merely of its layout.
        //
        // `FR-STORE-20` requires every analytical table to carry `sank_data_date` as a
        // `DATE`, and the Delta protocol requires every name in `partitionColumns` to be a
        // field of the schema. A table declaring a partition column absent from its schema
        // is malformed twice over, and the reader that notices is somebody else's engine.
        //
        // Appended rather than prepended, so a source column's position is unchanged and a
        // reader written against the source schema still finds its columns where they were.
        let stored = with_date_column(schema);
        let json =
            schema_string(&stored).map_err(|error| PublishError::UnrepresentableSchema {
                detail: error.to_string(),
            })?;

        // A key column that is not in the schema would make every merge silently return
        // nothing for that key. Caught here, where the person who typed it is still present.
        // A source column using the reserved prefix would be shadowed by a system value,
        // and the source's data would disappear with no error anywhere.
        for field in schema.fields() {
            if is_reserved(field.name()) && field.name() != DATA_DATE_COLUMN {
                return Err(PublishError::DateColumn {
                    detail: sankhya_schema::AxisError::ReservedName {
                        column: field.name().clone(),
                    }
                    .to_string(),
                });
            }
        }

        for column in &self.key_columns {
            // The comma check comes first, deliberately. A name containing one is a
            // *structural* problem — the key list is comma-separated in the log, so it
            // would split into two on the way back — and that is true whether or not the
            // column exists. Checking existence first would report the wrong thing for
            // "a,b", which is unlikely to be in any schema.
            if column.contains(',') {
                return Err(PublishError::CommaInKeyColumn {
                    column: column.clone(),
                });
            }
            if schema.field_with_name(column).is_err() {
                return Err(PublishError::NoSuchKeyColumn {
                    column: column.clone(),
                    available: schema.fields().iter().map(|f| f.name().clone()).collect(),
                });
            }
        }

        // The date axis, checked against the schema this table will actually have.
        self.check_date_axis(schema)?;

        std::fs::create_dir_all(&self.root).map_err(|error| PublishError::Io {
            detail: error.to_string(),
        })?;

        let mut metadata = Metadata::new(self.name.clone(), json, 0);
        metadata.configuration = configuration(self.class, &self.key_columns);
        metadata
            .configuration
            .extend(self.date_axis.to_configuration());
        // Partitioned on the date column, which is what makes retention a metadata
        // operation rather than a bulk delete.
        metadata.partition_columns = vec![DATA_DATE_COLUMN.to_string()];

        // Protocol *and* metadata. A Delta table without a protocol action does not declare
        // the reader and writer versions it needs, and a reader is entitled to refuse it or
        // to assume defaults it does not meet.
        //
        // This crate wrote metadata alone until the CDC pipeline was moved onto it, at which
        // point a test that pipeline already had --- asserting the creating commit carries a
        // protocol action --- failed. Its old, unsanctioned write path emitted one; the
        // official one did not. Two paths converging is what exposed it.
        commit(&self.root, 0, &create(metadata)).map_err(|error| {
            PublishError::Commit {
                version: 0,
                detail: error.to_string(),
            }
        })?;
        Ok(())
    }

    /// Create a clone: version zero of a log carrying the origin's schema **verbatim** and the
    /// properties that record where it came from.
    ///
    /// # Why the schema is a string rather than a `Schema`
    ///
    /// [`Self::create`] derives the stored schema --- it appends the date column, checks the
    /// axis, refuses reserved names. A clone must not re-derive anything: it has to reproduce
    /// its origin's schema *exactly*, and a schema that came out differently by a column
    /// ordering or an added field would be a clone that is not one. So the string is taken as
    /// it was read from the origin's log and written back unchanged.
    ///
    /// # Why this is here rather than in the server
    ///
    /// `check-writers` refuses a second writer to a warehouse, and it caught the first draft of
    /// the clone statement committing from `sankhya-server`. Widening that list would have been
    /// the easy answer and the wrong one: the point of the rule is that table state has **one**
    /// write path, and a clone's creating commit is table state. So the official writer gains
    /// the entry point instead.
    ///
    /// No `Add` actions, ever. `ADR-0016`'s Decision 1a is that a clone's log names none of the
    /// origin's files, which is what makes it constant-space and what makes the origin's
    /// reclamation question answerable.
    ///
    /// # Errors
    ///
    /// [`PublishError::Io`] if the directory cannot be made, and [`PublishError::Commit`] if
    /// version zero cannot be written --- including because the table already exists, which is
    /// the commit refusing to overwrite a log rather than this deciding it should not.
    pub fn create_clone(
        &self,
        schema_string: &str,
        properties: &BTreeMap<String, String>,
    ) -> Result<(), PublishError> {
        std::fs::create_dir_all(&self.root).map_err(|error| PublishError::Io {
            detail: error.to_string(),
        })?;

        let mut metadata = Metadata::new(self.name.clone(), schema_string.to_string(), 0);
        metadata.configuration = properties.clone();
        // Partitioned as the origin is, because the clone reads the origin's files and a
        // disagreement about partitioning is a disagreement about where those files are.
        metadata.partition_columns = vec![DATA_DATE_COLUMN.to_string()];

        commit(&self.root, 0, &create(metadata))
            .map(|_| ())
            .map_err(|error| PublishError::Commit {
                version: 0,
                detail: error.to_string(),
            })
    }

    /// Check the date axis against the schema.
    ///
    /// At creation, where the person who declared it is still present. Discovering at query
    /// time that the date column does not exist means discovering it from a table that has
    /// been partitioned wrongly for a month.
    fn check_date_axis(&self, schema: &Schema) -> Result<(), PublishError> {
        // No source column named: the table uses ingest date, which needs nothing from the
        // schema. The column is added at publication.
        let DateSource::Column { name } = &self.date_axis.source else {
            return Ok(());
        };

        let field = schema
            .field_with_name(name)
            .map_err(|_| PublishError::DateColumn {
                detail: sankhya_schema::AxisError::NoSuchSourceColumn {
                    column: name.clone(),
                    available: schema.fields().iter().map(|f| f.name().clone()).collect(),
                }
                .to_string(),
            })?;

        // A date, not a timestamp. A timestamp carries a time of day the partition cannot
        // represent, so two rows an hour apart would land in different partitions or the
        // same one depending on a truncation nobody asked for.
        if !matches!(field.data_type(), arrow_schema::DataType::Date32) {
            return Err(PublishError::DateColumn {
                detail: format!(
                    "the date column '{name}' is {}, and must be a date. A timestamp \
                     carries a time of day this partition cannot represent, so the \
                     truncation would happen somewhere nobody chose",
                    field.data_type()
                ),
            });
        }
        Ok(())
    }

    /// Publish one batch, and commit it.
    ///
    /// **One batch can become several files.** The table is partitioned by its date axis, so
    /// rows of different dates belong in different partitions; writing them to one file
    /// would put a date in a partition it does not belong to, and every pruning query would
    /// then read the wrong set or the whole table. All the files land in a **single commit**,
    /// so a reader never sees half a batch.
    ///
    /// Statistics are recorded for every column, always. There is no option to skip them:
    /// a file without them cannot be pruned, so every query reads it, and the table gets
    /// slower with nothing to say why.
    ///
    /// # Errors
    ///
    /// Refuses a batch whose schema differs from the table's, a null in the date column, or
    /// a failed write or commit --- rather than leaving a file with no action referring to it.
    pub fn append(
        &self,
        version: u64,
        file_name: &str,
        batch: &RecordBatch,
        covers_through: Lsn,
    ) -> Result<Vec<Published>, PublishError> {
        let (written, actions) = self.write_files(version, file_name, batch, covers_through)?;
        commit(&self.root, version, &actions).map_err(|error| PublishError::Commit {
            version,
            detail: error.to_string(),
        })?;
        Ok(written)
    }

    /// Publish a batch, taking a later version if another committer took ours.
    ///
    /// # Why this lives here rather than in the caller
    ///
    /// Capture is not the only committer: maintenance writes to the same log, and a
    /// compaction between two publishes takes the version capture was about to use. Failing
    /// there would mean a compaction can stop capture, which inverts the ordering rule ---
    /// the source outranks maintenance, always.
    ///
    /// **The files are written once.** Only the commit is retried, and that is safe because
    /// nothing about a file depends on the version: its name comes from the caller and its
    /// directory from its rows' dates. Rewriting them per attempt would multiply the work by
    /// the contention.
    ///
    /// # Errors
    /// As [`Publication::append`], and [`PublishError::Commit`] when `attempts` versions were
    /// all taken --- which means something is committing faster than this caller can follow,
    /// and is worth surfacing rather than retrying for ever.
    pub fn append_rebasing(
        &self,
        start: u64,
        attempts: usize,
        file_name: &str,
        batch: &RecordBatch,
        covers_through: Lsn,
    ) -> Result<Rebased, PublishError> {
        let (written, actions) = self.write_files(start, file_name, batch, covers_through)?;
        let mut version = start;
        for retries in 0..attempts.max(1) {
            match commit(&self.root, version, &actions) {
                Ok(_) => {
                    self.remember(version);
                    return Ok(Rebased { written, version, retries });
                }
                Err(sankhya_table_delta::CommitError::VersionTaken(_)) => {
                    // The version this writer wanted is taken, so it exists --- which makes
                    // it a floor for the walk that finds the next free one, and the walk is
                    // then over the commits made since rather than over the whole history.
                    self.remember(version);
                    version = self
                        .newest()
                        .map_or(version.saturating_add(1), |v| v.saturating_add(1));
                }
                Err(error) => {
                    return Err(PublishError::Commit {
                        version,
                        detail: error.to_string(),
                    })
                }
            }
        }
        Err(PublishError::Commit {
            version,
            detail: format!(
                "could not commit after {attempts} rebases; something else is committing to \
                 this table faster than this writer can follow"
            ),
        })
    }

    /// Publish a batch and record properties on the table **in the same commit**.
    ///
    /// # Why this exists rather than a second write
    ///
    /// A feed that publishes rows and then records how far it got has two commits and a gap
    /// between them. A crash in the gap leaves either rows nobody knows arrived --- which a
    /// restart duplicates --- or a position ahead of the data, which a restart skips. Which
    /// of the two you get depends on the order somebody chose, and both are silent.
    ///
    /// Committing them together makes a restart a question with an answer: either the rows
    /// and the position are both visible or neither is. This is `FR-TIER-08`'s argument for
    /// the purge journal --- commit the transition before the action, and make every phase
    /// resumable --- applied to ingest.
    ///
    /// The properties are merged into the table's existing metadata, re-read on every rebase
    /// attempt: another committer may have changed it between attempts, and writing back a
    /// copy read before theirs would silently undo it.
    ///
    /// # Errors
    ///
    /// As [`Publication::append_rebasing`], and [`PublishError::Commit`] when the table's
    /// current metadata cannot be read --- a table being appended to has some.
    pub fn append_recording(
        &self,
        start: u64,
        attempts: usize,
        file_name: &str,
        batch: &RecordBatch,
        covers_through: Lsn,
        properties: &BTreeMap<String, String>,
    ) -> Result<Rebased, PublishError> {
        let (written, actions) = self.write_files(start, file_name, batch, covers_through)?;
        let mut version = start;
        for retries in 0..attempts.max(1) {
            let mut metadata = self.current_metadata()?;
            metadata.configuration.extend(
                properties.iter().map(|(key, value)| (key.clone(), value.clone())),
            );
            let mut recorded = Vec::with_capacity(actions.len() + 1);
            recorded.push(Action::Metadata(metadata));
            recorded.extend(actions.iter().cloned());

            match commit(&self.root, version, &recorded) {
                Ok(_) => {
                    self.remember(version);
                    return Ok(Rebased { written, version, retries });
                }
                Err(sankhya_table_delta::CommitError::VersionTaken(_)) => {
                    self.remember(version);
                    version = self
                        .newest()
                        .map_or(version.saturating_add(1), |v| v.saturating_add(1));
                }
                Err(error) => {
                    return Err(PublishError::Commit {
                        version,
                        detail: error.to_string(),
                    })
                }
            }
        }
        Err(PublishError::Commit {
            version,
            detail: format!(
                "could not commit after {attempts} rebases; something else is committing to \
                 this table faster than this writer can follow"
            ),
        })
    }

    /// Refuse a batch that contradicts what the table has declared.
    ///
    /// # Why this was missing, and what it let through
    ///
    /// [`publish_table`] compares a batch against the schema **it was handed**, which is the
    /// easy case: the caller already had the schema. `append` is the path everything actually
    /// uses --- feeds, compaction, every test --- and it read the batch's schema from the
    /// batch and never looked at the table's. So a table declaring `id, amount` accepted a
    /// batch of two entirely different columns, and a vector column of width three accepted a
    /// vector of width four.
    ///
    /// The second is the one that does real damage quietly. A cosine similarity between a
    /// 384-dimensional embedding and a 512-dimensional one is not a near miss --- it is a
    /// different question --- and a column that silently held both would answer it.
    ///
    /// # The rule, from `ARCHITECTURE` §6.6
    ///
    /// *Additive and compatible changes apply automatically; incompatible ones are refused.*
    /// So:
    ///
    /// - A column the table already declares must keep its **type**. A width change on a
    ///   fixed-size list is a type change, which is the point of the width being in the type.
    /// - A **new** column is additive and passes. That is evolution working, not a hole.
    /// - Every declared column that cannot be null must be **present**, because a batch that
    ///   omits one is not adding to the table --- it is producing rows the table's own schema
    ///   says cannot exist.
    ///
    /// A table with no metadata yet is not checked: it is being created, and there is nothing
    /// to contradict.
    fn batch_agrees_with_the_table(&self, batch: &RecordBatch) -> Result<(), PublishError> {
        let declared = self.declared.get_or_init(|| {
            let metadata = self.current_metadata().ok()?;
            sankhya_table_delta::schema_from_string(&metadata.schema_string).ok()
        });
        // A table with no readable metadata is being created, and there is nothing to
        // contradict yet.
        let Some(declared) = declared else {
            return Ok(());
        };

        let offered = batch.schema();
        for field in declared.fields() {
            // The date column is stamped on after this check, from the partition value, so a
            // batch legitimately arrives without it.
            if field.name() == DATA_DATE_COLUMN {
                continue;
            }
            match offered.column_with_name(field.name()) {
                Some((_, supplied)) if !same_to_the_format(field, supplied) => {
                    return Err(PublishError::ContradictsSchema {
                        column: field.name().clone(),
                        declared: field.data_type().to_string(),
                        offered: supplied.data_type().to_string(),
                    })
                }
                None if !field.is_nullable() => {
                    return Err(PublishError::MissingColumn {
                        column: field.name().clone(),
                        offered: offered
                            .fields()
                            .iter()
                            .map(|f| f.name().clone())
                            .collect(),
                    })
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// One of the table's properties, as it stands.
    ///
    /// `None` for a table with no such property **and** for a table whose log cannot be read
    /// --- the two are the same answer to a writer that is about to create the table anyway.
    /// A caller that needs to tell them apart is asking a different question and should read
    /// the metadata.
    #[must_use]
    pub fn property(&self, key: &str) -> Option<String> {
        self.current_metadata().ok()?.configuration.get(key).cloned()
    }

    /// The table's metadata as it stands.
    ///
    /// The newest `metaData` action wins, which is what a reader of the log concludes too.
    fn current_metadata(&self) -> Result<Metadata, PublishError> {
        let actions = sankhya_table_delta::read_actions(&self.root).map_err(|error| {
            PublishError::Commit {
                version: 0,
                detail: format!("reading this table's metadata: {error}"),
            }
        })?;
        actions
            .into_iter()
            .filter_map(|(_, action)| match action {
                Action::Metadata(metadata) => Some(metadata),
                _ => None,
            })
            .next_back()
            .ok_or_else(|| PublishError::Commit {
                version: 0,
                detail: "this table's log has no metadata, so it is not a table this writer \
                         may append to"
                    .to_owned(),
            })
    }

    /// Write a batch's files and build the actions that would publish them.
    ///
    /// Separated from the commit so a caller can retry the commit without rewriting the
    /// files. Nothing here touches the log, so a crash between this and the commit leaves
    /// files nobody references --- which is invisible to queries and reclaimed by the orphan
    /// cleaner. The log always lags the filesystem, never leads it.
    fn write_files(
        &self,
        version: u64,
        file_name: &str,
        batch: &RecordBatch,
        covers_through: Lsn,
    ) -> Result<(Vec<Published>, Vec<Action>), PublishError> {
        if file_name.contains('/') || file_name.contains("..") {
            // The name becomes a path relative to the table root. A separator in it writes
            // outside the table, and `..` writes outside the warehouse.
            return Err(PublishError::UnsafeFileName {
                name: file_name.to_string(),
            });
        }

        self.batch_agrees_with_the_table(batch)?;

        let mut written = Vec::new();
        let mut actions = Vec::new();
        for (partition, rows) in self.partitions_of(batch)? {
            let part = take_rows(batch, &rows)?;
            // The file carries the column as well as the path. Delta permits a partition
            // column to be absent from the data and reconstructed from `partitionValues`,
            // and `FR-STORE-20` asks for it to be carried natively --- so a reader that
            // ignores partition values, and any tool that opens the Parquet directly, still
            // sees the date rather than a column that exists only in metadata.
            let part = stamped(&part, &partition, self.date_axis.granularity)?;
            let directory = format!("{DATA_DATE_COLUMN}={partition}");
            let into = self.root.join(&directory);
            std::fs::create_dir_all(&into).map_err(|error| PublishError::Write {
                file: directory.clone(),
                detail: error.to_string(),
            })?;

            let report = write_parquet(
                &into,
                file_name,
                &part,
                covers_through,
                self.writer,
            )
            .map_err(|error| PublishError::Write {
                file: format!("{directory}/{file_name}"),
                detail: error.to_string(),
            })?;

            // Statistics from the batch that was written, not from re-reading the file.
            // Both would work; taking them from the batch means the file is never opened
            // twice and means a discrepancy between them is impossible rather than merely
            // unlikely.
            let statistics = sankhya_table::column_stats(&part);
            let mut add = AddFile::with_statistics(
                // Relative to the table root, which is what the Delta protocol means by a
                // file path, and what an external reader resolves against.
                format!("{directory}/{file_name}"),
                report.bytes,
                0,
                &sankhya_table_delta::from_column_stats(
                    u64::try_from(part.num_rows()).unwrap_or(0),
                    &statistics,
                ),
            );
            // Declared in the metadata *and* supplied here. A table declaring a partition
            // column whose files carry no value for it is malformed: an external engine
            // reads the column as null for every row, and prunes nothing.
            add.partition_values
                .insert(DATA_DATE_COLUMN.to_string(), partition.clone());
            actions.push(Action::Add(add));

            written.push(Published {
                file: format!("{directory}/{file_name}"),
                bytes: report.bytes,
                rows: part.num_rows(),
                version,
            });
        }

        Ok((written, actions))
    }

    /// One batch per partition the batch touches, in partition order.
    ///
    /// Exposed because the fan-out guards in [`crate::fanout`] need to know how wide a batch
    /// is *before* writing it --- that is the whole point of a guard --- and because
    /// discovering it by writing the files is what they exist to prevent.
    ///
    /// # Errors
    /// As [`Publication::append`]: a missing or unreadable date column, or a null in it.
    pub fn split_by_partition(
        &self,
        batch: &RecordBatch,
    ) -> Result<Vec<(String, RecordBatch)>, PublishError> {
        let mut out = Vec::new();
        for (partition, rows) in self.partitions_of(batch)? {
            out.push((partition, take_rows(batch, &rows)?));
        }
        Ok(out)
    }

    /// Publish several batches as one commit.
    ///
    /// Rows for the same partition land in **one file** however many batches they arrived
    /// in, which is the property that turns per-batch fan-out into per-partition batching.
    ///
    /// # Errors
    /// As [`Publication::append`].
    pub fn append_all(
        &self,
        version: u64,
        file_name: &str,
        batches: &[RecordBatch],
        covers_through: Lsn,
    ) -> Result<Vec<Published>, PublishError> {
        if batches.is_empty() {
            return Ok(Vec::new());
        }
        // Concatenated first, so a partition present in five batches becomes one file rather
        // than five. Writing them separately would defeat the accumulation entirely.
        let schema = batches.first().map_or_else(
            || Arc::new(arrow_schema::Schema::empty()),
            RecordBatch::schema,
        );
        let combined = arrow_select::concat::concat_batches(&schema, batches).map_err(|e| {
            PublishError::Write {
                file: file_name.to_string(),
                detail: format!("combining {} batch(es): {e}", batches.len()),
            }
        })?;
        self.append(version, file_name, &combined, covers_through)
    }

    /// Publish several batches as one commit, at the next free version.
    ///
    /// The version comes from the log rather than from the caller. A caller that tracks
    /// versions itself has to be right about every commit somebody else makes, and it is not
    /// in a position to be.
    ///
    /// # Errors
    /// As [`Publication::append_rebasing`].
    pub fn append_all_rebasing(
        &self,
        file_name: &str,
        batches: &[RecordBatch],
        covers_through: Lsn,
    ) -> Result<Vec<Published>, PublishError> {
        if batches.is_empty() {
            return Ok(Vec::new());
        }
        let schema = batches.first().map_or_else(
            || Arc::new(arrow_schema::Schema::empty()),
            RecordBatch::schema,
        );
        let combined = arrow_select::concat::concat_batches(&schema, batches).map_err(|e| {
            PublishError::Write {
                file: file_name.to_string(),
                detail: format!("combining {} batch(es): {e}", batches.len()),
            }
        })?;
        let start = self.next_version();
        self.append_rebasing(start, REBASE_BUDGET, file_name, &combined, covers_through)
            .map(|rebased| rebased.written)
    }

    /// The version this table's next commit must take.
    ///
    /// Read from the log, because commit versions are contiguous by protocol and a counter
    /// held anywhere else is a counter that can be wrong about somebody else's commit.
    #[must_use]
    pub fn next_version(&self) -> u64 {
        // `newest_after`, not `live_files`.
        //
        // A live set reports the version of the newest commit *that contributed a file*. A
        // table that has been created and holds no data yet has commits and no files, so the
        // live set reports no version at all --- and a caller reading it as "no commits"
        // starts at zero, finds zero taken, walks forward one at a time, and lands in a gap.
        // That is not hypothetical: it stopped a soak twice, and the second time the log
        // said so plainly --- "committing version 14 would leave a gap; the next version is 0".
        self.newest().map_or(0, |version| version.saturating_add(1))
    }

    /// The newest version the log holds, probed forward from what this publication last saw.
    ///
    /// # Why the floor exists
    ///
    /// `newest_after(root, None)` walks from version zero, one `exists()` probe per version,
    /// so asking a table at version *v* costs *v* system calls --- and a publisher asks once
    /// per append and once per rebase. That is quadratic in a table's history and it is not
    /// theoretical: measured against the write path, sixteen hundred commits to one table ran
    /// at **11%** of the rate the same writers reached across sixteen hundred commits spread
    /// over eight tables, and almost all of the difference was this walk. It made exit
    /// criterion 6 --- contention degrades gracefully --- fail for a reason that had nothing
    /// to do with contention.
    ///
    /// # Why it is safe
    ///
    /// The floor is not an answer, it is a starting point, and every step of the walk is a
    /// filesystem probe. A floor behind the truth costs a longer walk. A floor *ahead* of it
    /// --- a table restored from a backup, or replaced underneath a long-lived publication ---
    /// makes `newest_after` return `None`, and returning `None` here would start the next
    /// commit at zero and walk into a gap, so a floor that fails is discarded and the full
    /// walk is done instead.
    fn newest(&self) -> Option<u64> {
        let floor = self.seen.load(Ordering::Relaxed).checked_sub(1);
        let found = match floor {
            Some(from) => sankhya_table_delta::newest_after(&self.root, Some(from))
                .or_else(|| sankhya_table_delta::newest_after(&self.root, None)),
            None => sankhya_table_delta::newest_after(&self.root, None),
        };
        if let Some(version) = found {
            self.remember(version);
        }
        found
    }

    /// Raise the probe's floor to `version`, never lower it.
    ///
    /// Lowering it would be harmless and pointless; racing two threads down to the older of
    /// two true answers is how a floor becomes a source of work rather than a saving.
    fn remember(&self, version: u64) {
        self.seen
            .fetch_max(version.saturating_add(1), Ordering::Relaxed);
    }

    /// Which rows of a batch belong to which partition.
    ///
    /// Ordered by partition value, so a batch published twice produces the same files in the
    /// same order.
    fn partitions_of(
        &self,
        batch: &RecordBatch,
    ) -> Result<Vec<(String, Vec<u32>)>, PublishError> {
        let mut groups: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        match &self.date_axis.source {
            DateSource::Column { name } => {
                let column = batch.column_by_name(name).ok_or_else(|| {
                    PublishError::DateColumn {
                        detail: format!("the date column '{name}' is not in the batch"),
                    }
                })?;
                let days = column.as_primitive_opt::<Date32Type>().ok_or_else(|| {
                    PublishError::DateColumn {
                        detail: format!(
                            "the date column '{name}' is {}, and must be a date",
                            column.data_type()
                        ),
                    }
                })?;
                for row in 0..batch.num_rows() {
                    if days.is_null(row) {
                        // A per-row fallback to today makes the column mean "when it
                        // happened" in some rows and "when we received it" in others,
                        // inseparably and for ever.
                        return Err(PublishError::DateColumn {
                            detail: format!(
                                "row {row} has no value in the date column '{name}'; a \
                                 fallback would make the column mean two different things \
                                 in one table"
                            ),
                        });
                    }
                    let partition = self.date_axis.partition_of(days.value(row));
                    groups
                        .entry(partition)
                        .or_default()
                        .push(u32::try_from(row).unwrap_or(0));
                }
            }
            DateSource::IngestDate => {
                // Every row of this batch arrived now, so they share one partition.
                let partition = self.date_axis.partition_of(today());
                groups.insert(
                    partition,
                    (0..batch.num_rows())
                        .map(|row| u32::try_from(row).unwrap_or(0))
                        .collect(),
                );
            }
        }
        Ok(groups.into_iter().collect())
    }
}

/// The rows of `batch` at `indices`.
fn take_rows(batch: &RecordBatch, indices: &[u32]) -> Result<RecordBatch, PublishError> {
    if indices.len() == batch.num_rows() {
        return Ok(batch.clone());
    }
    let picks = UInt32Array::from(indices.to_vec());
    let columns = batch
        .columns()
        .iter()
        .map(|column| arrow_select::take::take(column.as_ref(), &picks, None))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| PublishError::Write {
            file: "partitioning a batch".to_string(),
            detail: error.to_string(),
        })?;
    RecordBatch::try_new(batch.schema(), columns).map_err(|error| PublishError::Write {
        file: "partitioning a batch".to_string(),
        detail: error.to_string(),
    })
}

/// Today, as days since the Unix epoch.
///
/// Only reached for a table whose date axis is the ingest date, which is the axis that says
/// out loud that it means arrival.
fn today() -> i32 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i32::try_from(seconds / 86_400).unwrap_or(0)
}



/// What one publication wrote.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Published {
    /// The file's name, relative to the table root.
    pub file: String,
    /// How large it is.
    pub bytes: u64,
    /// How many rows it holds.
    pub rows: usize,
    /// The log version that added it.
    pub version: u64,
}

/// Whether two fields are the same type **as the table format records it**.
///
/// # Why this is not `==` on the Arrow types
///
/// The declared schema is not the schema somebody wrote --- it is that schema after a round
/// trip through the format's own string, which is lossy on purpose. A `Timestamp(µs, "UTC")`
/// and a `Timestamp(µs)` both render as `timestamp`, because the protocol's timestamps are
/// UTC-normalised and there is nowhere to put the zone.
///
/// So comparing Arrow types directly reports a contradiction for every type richer than the
/// format can express, and the first thing it caught was the quarantine table writing
/// UTC-aware timestamps into a column its own metadata calls naive --- a difference no reader
/// can observe, because the round trip erases it before anybody sees it.
///
/// Comparing the *rendered* forms asks the question that matters: **would a reader see two
/// different columns?** A fixed-size list carries its width into the string, so a width change
/// still differs, which is the case this check exists for.
fn same_to_the_format(declared: &Field, offered: &Field) -> bool {
    let render = |field: &Field| {
        sankhya_table_delta::schema_string(&Schema::new(vec![field.clone()])).ok()
    };
    match (render(declared), render(offered)) {
        (Some(left), Some(right)) => left == right,
        // A type the format cannot represent at all is refused elsewhere, by `create`. Here it
        // is not this check's question, and guessing would turn an unrelated failure into a
        // schema contradiction.
        _ => true,
    }
}

/// Publish a whole table in one call: create it, write every batch, commit.
///
/// The convenience form, and the one most publishers want. Each batch becomes one file,
/// which is what the read path prunes and parallelises over --- one enormous file cannot be
/// pruned at all, and ten thousand tiny ones cost more in metadata than they save.
///
/// # Errors
///
/// Any error creating the table or publishing any batch. The table is left as far as it
/// got: a partially published table is a valid table with fewer rows, which is recoverable,
/// and unwinding would mean deleting files that a concurrent reader may already hold.
pub fn publish_table(
    publication: &Publication,
    schema: &SchemaRef,
    batches: &[RecordBatch],
) -> Result<Vec<Published>, PublishError> {
    publication.create(schema)?;

    let mut written = Vec::new();
    for (index, batch) in batches.iter().enumerate() {
        if batch.schema().fields() != schema.fields() {
            return Err(PublishError::SchemaMismatch { batch: index });
        }
        let version = u64::try_from(index).unwrap_or(0).saturating_add(1);
        // Extended rather than pushed: a batch spanning several dates becomes several
        // files, one per partition, and the caller wants all of them.
        written.extend(publication.append(
            version,
            &format!("part-{index:05}.parquet"),
            batch,
            Lsn::new(version),
        )?);
    }
    Ok(written)
}

/// A publish that may have taken a later version than it asked for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rebased {
    /// The files published.
    pub written: Vec<Published>,
    /// The version it landed at.
    pub version: u64,
    /// How many versions were taken before this one.
    ///
    /// Reported rather than discarded: sustained rebasing means capture and maintenance are
    /// contending, which is a scheduling problem visible nowhere else.
    pub retries: usize,
}

/// Why a publication could not proceed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PublishError {
    /// A column's type cannot be represented exactly in the table format.
    UnrepresentableSchema {
        /// What the mapping said.
        detail: String,
    },
    /// A declared key column is not in the schema.
    NoSuchKeyColumn {
        /// What was declared.
        column: String,
        /// What the schema has.
        available: Vec<String>,
    },
    /// A key column's name contains a comma.
    CommaInKeyColumn {
        /// The offending name.
        column: String,
    },
    /// A file name that would write outside the table.
    UnsafeFileName {
        /// What was offered.
        name: String,
    },
    /// A batch's schema differs from the table's.
    SchemaMismatch {
        /// Which batch.
        batch: usize,
    },
    /// A batch gives a declared column a different type.
    ContradictsSchema {
        /// The column.
        column: String,
        /// What the table says it is.
        declared: String,
        /// What the batch offered.
        offered: String,
    },
    /// A batch leaves out a column the table says cannot be null.
    MissingColumn {
        /// The column.
        column: String,
        /// What the batch did carry.
        offered: Vec<String>,
    },
    /// Writing a file failed.
    Write {
        /// Which file.
        file: String,
        /// What went wrong.
        detail: String,
    },
    /// Committing failed.
    Commit {
        /// Which version.
        version: u64,
        /// What went wrong.
        detail: String,
    },
    /// The date axis does not fit the schema.
    DateColumn {
        /// What is wrong.
        detail: String,
    },
    /// The filesystem refused.
    Io {
        /// What went wrong.
        detail: String,
    },
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnrepresentableSchema { detail } => write!(
                f,
                "{detail}. Refusing rather than publishing something merely similar, which \
                 produces a table other engines read confidently and wrongly"
            ),
            Self::NoSuchKeyColumn { column, available } => write!(
                f,
                "the key column '{column}' is not in the schema, which has {available:?}. A \
                 key naming a column that does not exist makes every merge return nothing \
                 for that key"
            ),
            Self::CommaInKeyColumn { column } => write!(
                f,
                "the key column '{column}' contains a comma. The key list is comma-separated \
                 in the log, so this would split one column into two on the way back"
            ),
            Self::UnsafeFileName { name } => write!(
                f,
                "'{name}' is not a safe file name: it becomes a path relative to the table \
                 root, and a separator writes outside the table while '..' writes outside \
                 the warehouse"
            ),
            Self::ContradictsSchema { column, declared, offered } => write!(
                f,
                "the column `{column}` is `{declared}` in this table and the batch offers \
                 `{offered}`. Refused rather than written: a reader combining the two files \
                 would find one column with two types, and for a vector a change of width is \
                 a change of question --- a similarity between a 384-dimensional embedding \
                 and a 512-dimensional one is not a near miss. An additive change is applied \
                 automatically; this one is not additive"
            ),
            Self::MissingColumn { column, offered } => write!(
                f,
                "this table declares `{column}` and says it cannot be null, and the batch \
                 carries {offered:?}. A batch that leaves out a required column is not adding \
                 to the table --- it is producing rows the table's own schema says cannot \
                 exist"
            ),
            Self::SchemaMismatch { batch } => write!(
                f,
                "batch {batch} has a different schema from the table. Publishing it would \
                 produce files a reader cannot combine, and the failure would appear at \
                 query time rather than here"
            ),
            Self::Write { file, detail } => write!(f, "could not write {file}: {detail}"),
            Self::Commit { version, detail } => {
                write!(f, "could not commit version {version}: {detail}")
            }
            Self::DateColumn { detail } => write!(f, "{detail}"),
            Self::Io { detail } => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for PublishError {}

/// Whether a path looks like a table this system can read.
#[must_use]
pub fn is_table(root: &Path) -> bool {
    root.join("_delta_log").is_dir()
}

/// The table's schema, with the partition column appended.
///
/// Appended rather than prepended so a source column's position is unchanged: a reader
/// written against the source schema still finds its columns where they were.
fn with_date_column(schema: &Schema) -> Schema {
    if schema.field_with_name(DATA_DATE_COLUMN).is_ok() {
        return schema.clone();
    }
    let mut fields: Vec<Arc<Field>> = schema.fields().iter().map(Arc::clone).collect();
    // Not nullable. A null partition value has no path to live at, so a nullable column
    // here would promise something the layout cannot represent.
    fields.push(Arc::new(Field::new(
        DATA_DATE_COLUMN,
        DataType::Date32,
        false,
    )));
    Schema::new(fields)
}

/// The batch with its partition's date attached to every row.
///
/// The value comes from the partition, not from the source column, so a coarser granularity
/// stamps the first day of the period --- which is what the partition path says, and a row
/// whose stamp disagreed with the directory it sits in would be a table that reconciles
/// differently depending on which of the two a reader trusts.
fn stamped(
    batch: &RecordBatch,
    partition: &str,
    granularity: Granularity,
) -> Result<RecordBatch, PublishError> {
    let days = days_of_partition(partition, granularity)?;
    let mut columns: Vec<ArrayRef> = batch.columns().to_vec();
    columns.push(Arc::new(Date32Array::from(vec![days; batch.num_rows()])));
    let schema = Arc::new(with_date_column(batch.schema().as_ref()));
    RecordBatch::try_new(schema, columns).map_err(|error| PublishError::Write {
        file: "stamping the date column".to_string(),
        detail: error.to_string(),
    })
}

/// The first day of a partition, as days since the Unix epoch.
fn days_of_partition(partition: &str, granularity: Granularity) -> Result<i32, PublishError> {
    let parts: Vec<&str> = partition.split('-').collect();
    let read = |at: usize, default: i32| -> i32 {
        parts.get(at).and_then(|p| p.parse().ok()).unwrap_or(default)
    };
    let (year, month, day) = match granularity {
        Granularity::Day => (read(0, 1970), read(1, 1), read(2, 1)),
        Granularity::Month => (read(0, 1970), read(1, 1), 1),
        Granularity::Year => (read(0, 1970), 1, 1),
    };
    days_from_civil(year, month, day).ok_or_else(|| PublishError::DateColumn {
        detail: format!("'{partition}' is not a date this granularity can represent"),
    })
}

/// Howard Hinnant's civil-to-days, the inverse of the one in `sankhya-schema`.
///
/// Written out rather than pulled from a dependency for the same reason as its inverse: a
/// partition key computed slightly differently by two components is a table whose rows do
/// not agree with the directories they sit in.
fn days_from_civil(year: i32, month: i32, day: i32) -> Option<i32> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}
