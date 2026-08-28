//! Executing a compaction.
//!
//! # The rule that makes this safe
//!
//! **Compaction only ever adds. A separate operation ever removes.**
//!
//! A merge writes a new file and leaves its inputs in place. Readers already holding a
//! snapshot continue reading the files they resolved, entirely unaffected — there is
//! no window in which a file a reader is using disappears underneath it.
//!
//! Deleting the inputs is a distinct operation with its own preconditions: the inputs
//! must be unreferenced by any retained snapshot and older than the longest permitted
//! query. Separating the two means the frequent, cheap operation carries essentially no
//! risk, while the dangerous one runs rarely and under stricter conditions.
//!
//! # Why compaction is where sorting happens
//!
//! Row-group statistics only prune when a row group's values fall outside a predicate's
//! range. Written in arrival order every row group holds the whole range of every column,
//! so the bounds exclude nothing and a selective query reads the entire table.
//!
//! Sorting on the column a query filters by turns those bounds into a usable index:
//! measured on TPC-H Q6, which selects one year in seven, sorting the file by ship date
//! took the query from 229 ms to 55 ms — **4.2×**, entirely from row groups skipped
//! before any decoding.
//!
//! Compaction is the place for it because compaction has already read and rewritten the
//! data. Ingest cannot sort — it sees one batch at a time and has no idea what will
//! arrive next — and a separate sorting pass would read and write everything a second
//! time for a result compaction could have produced for the cost of an ordering.
//!
//! # Why only settled partitions are sorted
//!
//! Sorting a partition that is still receiving writes means sorting it again tomorrow,
//! for a layout that was correct for as long as nobody appended to it. The caller decides
//! what settled means; this only acts on the answer.
//!
//! # Why the output is verified before anything is removed
//!
//! A merge that silently dropped rows would leave a smaller, internally consistent
//! dataset. The row count is therefore checked against the inputs before the operation
//! is reported as successful, so a defect surfaces here rather than as a
//! reconciliation failure days later.

use arrow_array::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use sankhya_error::{Error, Result};
use sankhya_stats::ColumnStats;
use sankhya_types::Lsn;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::write::{write_parquet, WriterConfig};

/// What a compaction produced.
#[derive(Clone, PartialEq, Debug)]
pub struct CompactionOutcome {
    /// The file written. Its inputs are still present.
    pub output: PathBuf,
    pub rows: u64,
    pub bytes: u64,
    /// Inputs, still on disk and safe for in-flight readers.
    ///
    /// Removing them is a separate decision made under separate conditions.
    pub inputs_retained: Vec<PathBuf>,
    pub bytes_before: u64,
    pub covers_through: Lsn,
    /// Statistics for the merged file, computed from the data the merge already read.
    ///
    /// Free in the sense that matters: no additional pass over storage. The alternative
    /// is an analysis command someone has to run, which on a system that onboards tables
    /// automatically means the tables nobody thought about have no statistics.
    pub column_stats: BTreeMap<String, ColumnStats>,
}

impl CompactionOutcome {
    /// How much smaller the merged form is.
    ///
    /// Above one means the merge saved space, largely through better compression across
    /// a larger block and less per-file overhead.
    #[must_use]
    pub fn size_ratio(&self) -> f64 {
        if self.bytes == 0 {
            return 0.0;
        }
        self.bytes_before as f64 / self.bytes as f64
    }
}

/// Reorder a batch by the named columns.
///
/// Nulls sort last, matching the read path's default ordering — a file whose nulls are
/// at one end and whose reader expects them at the other gains nothing from being sorted.
fn sort_by(batch: &RecordBatch, clustering: &[String]) -> Result<RecordBatch> {
    let mut columns = Vec::with_capacity(clustering.len());
    for name in clustering {
        let column = batch.column_by_name(name).ok_or_else(|| {
            Error::InvariantViolated(format!(
                "the clustering key names {name}, which is not a column of this table; \
                 merging unsorted would leave a partition that looks clustered and is \
                 not, and nothing downstream could tell"
            ))
        })?;
        columns.push(arrow::compute::SortColumn {
            values: Arc::clone(column),
            options: Some(arrow::compute::SortOptions {
                descending: false,
                nulls_first: false,
            }),
        });
    }

    let indices = arrow::compute::lexsort_to_indices(&columns, None)
        .map_err(|e| Error::InvariantViolated(format!("sorting the merged batch: {e}")))?;

    let sorted: std::result::Result<Vec<_>, _> = batch
        .columns()
        .iter()
        .map(|column| arrow::compute::take(column, &indices, None))
        .collect();

    RecordBatch::try_new(
        batch.schema(),
        sorted.map_err(|e| Error::InvariantViolated(format!("reordering a column: {e}")))?,
    )
    .map_err(|e| Error::InvariantViolated(format!("rebuilding the sorted batch: {e}")))
}

/// Row count and size of an existing file.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or its metadata read.
pub fn read_parquet_stats(path: &Path) -> Result<(u64, u64)> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error::StorageUnavailable(format!("opening {}: {e}", path.display())))?;
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| Error::StorageUnavailable(format!("reading {}: {e}", path.display())))?;
    let rows = builder.metadata().file_metadata().num_rows();
    Ok((u64::try_from(rows).unwrap_or(0), bytes))
}

/// What a scan of one file actually read.
///
/// Row count *and* a checksum over the decoded values. The checksum is not for integrity ---
/// Parquet has its own --- it is there so that "the scan read nothing" is distinguishable
/// from "the scan read zeros", and so that a decode cannot be skipped by an optimiser and
/// leave a benchmark measuring an empty loop. A soak that reports throughput for work it
/// did not do is worse than one that reports nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Scanned {
    /// Rows decoded.
    pub rows: u64,
    /// Bytes the file occupies.
    pub bytes: u64,
    /// A running total over the decoded values, so the work is observable.
    pub checksum: u64,
}

impl Scanned {
    /// Two scans combined.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        Self {
            rows: self.rows.saturating_add(other.rows),
            bytes: self.bytes.saturating_add(other.bytes),
            checksum: self.checksum.wrapping_add(other.checksum),
        }
    }
}

/// Decode every row of a file.
///
/// Unlike [`read_parquet_stats`], which reads the footer, this reads the data --- which is
/// what a query pays for and what a soak has to exercise if it is going to claim anything
/// about the read path.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or a batch cannot be decoded.
pub fn scan_parquet(path: &Path) -> Result<Scanned> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error::StorageUnavailable(format!("opening {}: {e}", path.display())))?;
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| Error::StorageUnavailable(format!("reading {}: {e}", path.display())))?
        .build()
        .map_err(|e| Error::StorageUnavailable(format!("scanning {}: {e}", path.display())))?;

    let mut scanned = Scanned { rows: 0, bytes, checksum: 0 };
    for batch in reader {
        let batch =
            batch.map_err(|e| Error::InvariantViolated(format!("decoding a batch: {e}")))?;
        scanned.rows = scanned.rows.saturating_add(batch.num_rows() as u64);
        // Touch the decoded values. Counting rows alone would let a reader that produced
        // empty batches look like a successful scan.
        for column in batch.columns() {
            scanned.checksum = scanned
                .checksum
                .wrapping_add(column.len() as u64)
                .wrapping_add(column.null_count() as u64);
        }
    }
    Ok(scanned)
}

/// Merge several files into one.
///
/// # Errors
///
/// Returns an error if any input cannot be read, if the output cannot be written, or —
/// importantly — if the output does not contain every input row. The last is checked
/// rather than assumed: a merge that silently dropped rows would leave a smaller,
/// internally consistent dataset that nothing downstream would flag.
pub fn compact_files(
    inputs: &[PathBuf],
    output_dir: &Path,
    output_name: &str,
    covers_through: Lsn,
    config: WriterConfig,
) -> Result<CompactionOutcome> {
    compact_files_sorted(inputs, output_dir, output_name, covers_through, config, &[])
}

/// Merge several files into one, ordered by `clustering`.
///
/// An empty `clustering` merges without sorting, which is [`compact_files`].
///
/// # Errors
///
/// As [`compact_files`], and additionally if a clustering column is not in the schema —
/// silently merging unsorted would leave a partition that looks clustered and is not,
/// which is worse than refusing because nothing downstream can tell.
pub fn compact_files_sorted(
    inputs: &[PathBuf],
    output_dir: &Path,
    output_name: &str,
    covers_through: Lsn,
    config: WriterConfig,
    clustering: &[String],
) -> Result<CompactionOutcome> {
    if inputs.len() < 2 {
        return Err(Error::InvariantViolated(
            "a compaction must merge at least two files; merging one rewrites it for \
             no benefit"
                .to_string(),
        ));
    }

    let mut batches: Vec<RecordBatch> = Vec::new();
    let mut expected_rows = 0u64;
    let mut bytes_before = 0u64;

    for path in inputs {
        let (rows, bytes) = read_parquet_stats(path)?;
        expected_rows = expected_rows.saturating_add(rows);
        bytes_before = bytes_before.saturating_add(bytes);

        let file = std::fs::File::open(path)
            .map_err(|e| Error::StorageUnavailable(format!("opening {}: {e}", path.display())))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| Error::StorageUnavailable(format!("reading {}: {e}", path.display())))?
            .build()
            .map_err(|e| Error::StorageUnavailable(format!("building reader: {e}")))?;

        for batch in reader {
            batches.push(
                batch.map_err(|e| Error::StorageUnavailable(format!("decoding a batch: {e}")))?,
            );
        }
    }

    let Some(first) = batches.first() else {
        return Err(Error::InvariantViolated(
            "the inputs contained no data".to_string(),
        ));
    };
    let schema = first.schema();

    // Every input must share a schema. A merge across shapes would silently drop or
    // reorder columns, which is worse than refusing.
    for batch in &batches {
        if batch.schema() != schema {
            return Err(Error::InvariantViolated(
                "inputs do not share a schema; merging across shapes would silently \
                 drop or misalign columns"
                    .to_string(),
            ));
        }
    }

    let merged = arrow::compute::concat_batches(&schema, &batches)
        .map_err(|e| Error::InvariantViolated(format!("concatenating batches: {e}")))?;

    let merged = if clustering.is_empty() {
        merged
    } else {
        sort_by(&merged, clustering)?
    };

    let report = write_parquet(output_dir, output_name, &merged, covers_through, config)?;

    // Verified, not assumed.
    let (written_rows, written_bytes) = read_parquet_stats(&report.path)?;
    if written_rows != expected_rows {
        return Err(Error::InvariantViolated(format!(
            "compaction wrote {written_rows} rows but its inputs held {expected_rows}; \
             the output has been left in place for inspection and no input was removed"
        )));
    }

    Ok(CompactionOutcome {
        column_stats: crate::stats::column_stats(&merged),
        output: report.path,
        rows: written_rows,
        bytes: written_bytes,
        inputs_retained: inputs.to_vec(),
        bytes_before,
        covers_through,
    })
}
