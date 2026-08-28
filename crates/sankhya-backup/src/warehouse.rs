//! Reading a table back as it stood, and digesting what is there.
//!
//! # The same code both times, or the comparison means nothing
//!
//! A digest is recorded when the backup is taken and recomputed when it is drilled. If those
//! two are computed by different code — a different value rendering, a different null
//! convention, a different column order — then every drill fails on data that is perfectly
//! fine, and after the third false alarm nobody runs drills any more.
//!
//! So both paths go through [`digest_at`], and there is no second implementation to drift
//! from it. That is the entire reason this lives beside the manifest rather than in whatever
//! binary happens to be taking the backup.
//!
//! # Rendering is where a digest silently disagrees with itself
//!
//! Arrow's own `ArrayFormatter` does the rendering rather than a hand-written match on
//! `DataType`. It is not a shortcut: a hand-written renderer has to decide how a timestamp,
//! a decimal, a negative zero and a nested list are spelled, and any of those decisions
//! changing between two releases changes every digest ever recorded. Deferring to the Arrow
//! crate the workspace pins exactly means the rendering is pinned exactly too.

use arrow_array::RecordBatch;
use sankhya_ingest::{RowDigest, TableDigest};
use sankhya_table_delta::{live_files_at, Version};
use std::path::Path;

/// Digest a table's data as it stood at `version`.
///
/// # Errors
///
/// Returns text an operator can act on: which file, and what went wrong with it.
pub fn digest_at(table_root: &Path, version: Version) -> Result<TableDigest, String> {
    let live = live_files_at(table_root, version)
        .map_err(|error| format!("the log would not replay to version {version}: {error}"))?;

    // Sorted by path, so a digest does not depend on the order the log happened to list
    // files in. The digest itself is order-independent, which makes this belt and braces —
    // and it also makes a failure reproducible, which matters more.
    let mut paths: Vec<&str> = live.files.iter().map(|file| file.path.as_str()).collect();
    paths.sort_unstable();

    let mut digest = TableDigest::empty();
    for path in paths {
        digest = digest.merge(digest_file(&table_root.join(path))?);
    }
    Ok(digest)
}

/// Digest one Parquet file.
fn digest_file(path: &Path) -> Result<TableDigest, String> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let file = std::fs::File::open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .build()
        .map_err(|error| format!("{}: {error}", path.display()))?;

    let mut digest = TableDigest::empty();
    for batch in reader {
        let batch = batch.map_err(|error| format!("{}: {error}", path.display()))?;
        digest = digest.merge(digest_batch(&batch)?);
    }
    Ok(digest)
}

/// Digest one batch, row by row.
fn digest_batch(batch: &RecordBatch) -> Result<TableDigest, String> {
    use arrow::util::display::{ArrayFormatter, FormatOptions};

    // The null convention is stated once, here. A digest whose two sides spell null
    // differently disagrees on every row containing one, which reads as total corruption.
    let options = FormatOptions::default().with_null("");
    let formatters: Vec<ArrayFormatter<'_>> = batch
        .columns()
        .iter()
        .map(|column| ArrayFormatter::try_new(column.as_ref(), &options))
        .collect::<Result<_, _>>()
        .map_err(|error| format!("a column could not be rendered: {error}"))?;

    let mut digest = TableDigest::empty();
    let mut rendered: Vec<String> = Vec::with_capacity(formatters.len());
    for row in 0..batch.num_rows() {
        rendered.clear();
        for (index, formatter) in formatters.iter().enumerate() {
            // A null stays a null rather than becoming the empty string, so a genuinely
            // empty value and an absent one digest differently. They are different facts and
            // a backup that cannot tell them apart cannot prove it restored either.
            let column = batch.column(index);
            rendered.push(if column.is_null(row) {
                String::new()
            } else {
                formatter.value(row).to_string()
            });
        }
        let values: Vec<Option<&str>> = rendered
            .iter()
            .enumerate()
            .map(|(index, text)| {
                if batch.column(index).is_null(row) {
                    None
                } else {
                    Some(text.as_str())
                }
            })
            .collect();
        digest.add(RowDigest::of(&values));
    }
    Ok(digest)
}

/// Reads a warehouse laid out as `<root>/<schema>/<table>/`.
#[derive(Clone, Debug)]
pub struct Warehouse {
    root: std::path::PathBuf,
}

impl Warehouse {
    /// Read from this directory.
    #[must_use]
    pub fn at(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where a table named `schema.table` lives.
    #[must_use]
    pub fn table_root(&self, table: &str) -> std::path::PathBuf {
        match table.split_once('.') {
            Some((schema, name)) => self.root.join(schema).join(name),
            None => self.root.join(table),
        }
    }

    /// Digest a table as it stands now, for recording in a manifest.
    ///
    /// # Errors
    ///
    /// Returns text naming the file that would not read.
    pub fn digest_now(&self, table: &str) -> Result<(Version, TableDigest), String> {
        let root = self.table_root(table);
        let live = sankhya_table_delta::live_files(&root)
            .map_err(|error| format!("{table}: {error}"))?;
        let version = live
            .version
            .ok_or_else(|| format!("{table} has no commits, so there is nothing to back up"))?;
        Ok((version, digest_at(&root, version)?))
    }
}

impl crate::drill::ReadsBack for Warehouse {
    fn digest_of(&self, table: &str, version: Version) -> Result<TableDigest, String> {
        digest_at(&self.table_root(table), version)
    }
}
