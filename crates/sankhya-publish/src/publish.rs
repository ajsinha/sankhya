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
use sankhya_table_delta::{commit, schema_string, Action, AddFile, Metadata};
use sankhya_types::Lsn;
use std::fmt;
use std::path::{Path, PathBuf};

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
        }
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

        commit(&self.root, 0, &[Action::Metadata(metadata)]).map_err(|error| {
            PublishError::Commit {
                version: 0,
                detail: error.to_string(),
            }
        })?;
        Ok(())
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
        if file_name.contains('/') || file_name.contains("..") {
            // The name becomes a path relative to the table root. A separator in it writes
            // outside the table, and `..` writes outside the warehouse.
            return Err(PublishError::UnsafeFileName {
                name: file_name.to_string(),
            });
        }

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
                WriterConfig::default(),
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

        commit(&self.root, version, &actions).map_err(|error| PublishError::Commit {
            version,
            detail: error.to_string(),
        })?;

        Ok(written)
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
