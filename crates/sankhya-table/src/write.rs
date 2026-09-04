//! The Parquet write path.
//!
//! # Two settings whose defaults silently disable the mechanism they belong to
//!
//! The writer's page row-count limit is effectively unlimited by default. The page
//! index stores bounds *per page*, so with no row cap a narrow column packs enormous
//! row counts into a single page and the index degenerates to one entry covering
//! everything. **Page pruning then does nothing**, with no symptom other than being
//! slow.
//!
//! Statistics must also be enabled at page granularity, since that is what emits the
//! index in the first place.
//!
//! Both are set explicitly here rather than inherited, and both are asserted by test.

use arrow_array::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, Encoding, ZstdLevel};
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use sankhya_error::{Error, Result};
use sankhya_types::Lsn;
use std::path::{Path, PathBuf};

/// Physical layout choices, made at write time and expensive to undo.
#[derive(Clone, Copy, Debug)]
pub struct WriterConfig {
    /// Rows per page. Bounds the page index's granularity.
    pub page_row_limit: usize,
    /// Rows per row group.
    pub row_group_rows: usize,
    /// Compression level. Heavier wins when I/O-bound, which is the normal case for
    /// remote storage and for any machine with many cores.
    pub zstd_level: i32,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            // Without this cap a narrow column yields one page per row group and page
            // pruning silently stops working.
            page_row_limit: 20_000,
            row_group_rows: 1_000_000,
            zstd_level: 3,
        }
    }
}

impl WriterConfig {
    fn properties(self) -> WriterProperties {
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(
                ZstdLevel::try_new(self.zstd_level).unwrap_or_default(),
            ))
            // Page-level statistics are what emit the page index.
            .set_statistics_enabled(EnabledStatistics::Page)
            .set_data_page_row_count_limit(self.page_row_limit)
            .set_max_row_group_row_count(Some(self.row_group_rows))
            // Keeps footers small on wide tables; truncation stays sound because a
            // truncated lower bound rounds down and an upper bound rounds up.
            .set_statistics_truncate_length(Some(64))
            // The commit position is monotonic, so delta encoding is near-free.
            .set_column_encoding("_sankhya_commit_lsn".into(), Encoding::DELTA_BINARY_PACKED)
            .set_column_dictionary_enabled("_sankhya_commit_lsn".into(), false)
            .build()
    }
}

/// What a write produced.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WriteReport {
    pub path: PathBuf,
    pub rows: usize,
    pub bytes: u64,
    /// The position this file is known to contain, which is what lets the tier declare
    /// coverage the read path can splice against.
    pub covers_through: Lsn,
}

/// Write one batch as a Parquet file.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be written.
pub fn write_parquet(
    directory: &Path,
    file_name: &str,
    batch: &RecordBatch,
    covers_through: Lsn,
    config: WriterConfig,
) -> Result<WriteReport> {
    std::fs::create_dir_all(directory)
        .map_err(|e| Error::StorageUnavailable(format!("creating {}: {e}", directory.display())))?;

    let path = directory.join(file_name);
    // `create_new`, not `create`. `File::create` **truncates** an existing file, and a writer
    // that silently empties a file somebody else's log still points at is the quietest way to
    // lose data this system has.
    //
    // It was reachable. Compaction's output sequence was a per-process counter starting at
    // zero, so after a restart tick 1 recomputed a name tick 1 had already used --- and the
    // planner selects any live file under the target size, so the previous output could be
    // selected as its own input. It was truncated in place, and the commit that followed
    // added and removed the same path, taking the merged partition out of the live set
    // altogether.
    //
    // The sequence is recovered from the log now, which is the root fix. This is the one that
    // holds whatever the caller does: a name that already exists is refused, loudly, rather
    // than overwritten.
    let file = std::fs::File::create_new(&path).map_err(|e| {
        Error::StorageUnavailable(format!(
            "creating {}: {e}. A data file is never overwritten --- a name that already \
             exists belongs to rows some log still refers to",
            path.display()
        ))
    })?;

    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(config.properties()))
        .map_err(|e| Error::StorageUnavailable(format!("opening writer: {e}")))?;
    writer
        .write(batch)
        .map_err(|e| Error::StorageUnavailable(format!("writing batch: {e}")))?;
    // `close` writes the footer and drops the handle. It does not put anything on the
    // medium, so the file that a commit is about to reference exists only in the page cache.
    //
    // The failure that matters is not a lost file --- a commit referencing a file that is not
    // there is loud and refuses to read. It is a file that *is* there, is the right length,
    // and holds whatever those blocks held before: a scan that returns rows nobody wrote, or
    // a footer that parses into the wrong offsets. Synced here, before the caller is in a
    // position to commit a log entry pointing at it.
    let written = writer
        .into_inner()
        .map_err(|e| Error::StorageUnavailable(format!("closing writer: {e}")))?;
    written
        .sync_all()
        .map_err(|e| Error::StorageUnavailable(format!("syncing {}: {e}", path.display())))?;
    // And the directory entry, which is a separate write and survives separately. A synced
    // file with no name is a file the commit refers to and the filesystem cannot find.
    if let Ok(handle) = std::fs::File::open(directory) {
        handle
            .sync_all()
            .map_err(|e| Error::StorageUnavailable(format!("syncing {}: {e}", directory.display())))?;
    }

    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(WriteReport {
        path,
        rows: batch.num_rows(),
        bytes,
        covers_through,
    })
}
