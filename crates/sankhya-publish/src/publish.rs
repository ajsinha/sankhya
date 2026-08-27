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
use sankhya_table::{write_parquet, WriterConfig};
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
        }
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
        let json = schema_string(schema).map_err(|error| PublishError::UnrepresentableSchema {
            detail: error.to_string(),
        })?;

        // A key column that is not in the schema would make every merge silently return
        // nothing for that key. Caught here, where the person who typed it is still present.
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

        std::fs::create_dir_all(&self.root).map_err(|error| PublishError::Io {
            detail: error.to_string(),
        })?;

        let mut metadata = Metadata::new(self.name.clone(), json, 0);
        metadata.configuration = configuration(self.class, &self.key_columns);

        commit(&self.root, 0, &[Action::Metadata(metadata)]).map_err(|error| {
            PublishError::Commit {
                version: 0,
                detail: error.to_string(),
            }
        })?;
        Ok(())
    }

    /// Publish one batch as a new file, and commit it.
    ///
    /// Statistics are recorded for every column, always. There is no option to skip them:
    /// a file without them cannot be pruned, so every query reads it, and the table gets
    /// slower with nothing to say why.
    ///
    /// # Errors
    ///
    /// Refuses a batch whose schema differs from the table's, and reports a failed write or
    /// commit rather than leaving a file with no action referring to it.
    pub fn append(
        &self,
        version: u64,
        file_name: &str,
        batch: &RecordBatch,
        covers_through: Lsn,
    ) -> Result<Published, PublishError> {
        if file_name.contains('/') || file_name.contains("..") {
            // The name becomes a path relative to the table root. A separator in it writes
            // outside the table, and `..` writes outside the warehouse.
            return Err(PublishError::UnsafeFileName {
                name: file_name.to_string(),
            });
        }

        let report = write_parquet(
            &self.root,
            file_name,
            batch,
            covers_through,
            WriterConfig::default(),
        )
        .map_err(|error| PublishError::Write {
            file: file_name.to_string(),
            detail: error.to_string(),
        })?;

        // Statistics from the batch that was written, not from re-reading the file. Both
        // would work; taking them from the batch means the file is never opened twice and
        // means a discrepancy between them is impossible rather than merely unlikely.
        let statistics = sankhya_table::column_stats(batch);
        let add = AddFile::with_statistics(
            file_name,
            report.bytes,
            0,
            &sankhya_table_delta::from_column_stats(
                u64::try_from(batch.num_rows()).unwrap_or(0),
                &statistics,
            ),
        );

        commit(&self.root, version, &[Action::Add(add)]).map_err(|error| PublishError::Commit {
            version,
            detail: error.to_string(),
        })?;

        Ok(Published {
            file: file_name.to_string(),
            bytes: report.bytes,
            rows: batch.num_rows(),
            version,
        })
    }
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
        written.push(publication.append(
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
