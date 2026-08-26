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
//! # Why the output is verified before anything is removed
//!
//! A merge that silently dropped rows would leave a smaller, internally consistent
//! dataset. The row count is therefore checked against the inputs before the operation
//! is reported as successful, so a defect surfaces here rather than as a
//! reconciliation failure days later.

use arrow_array::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use sankhya_error::{Error, Result};
use sankhya_types::Lsn;
use std::path::{Path, PathBuf};

use crate::write::{WriterConfig, write_parquet};

/// What a compaction produced.
#[derive(Clone, PartialEq, Eq, Debug)]
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
        return Err(Error::InvariantViolated("the inputs contained no data".to_string()));
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
        output: report.path,
        rows: written_rows,
        bytes: written_bytes,
        inputs_retained: inputs.to_vec(),
        bytes_before,
        covers_through,
    })
}
