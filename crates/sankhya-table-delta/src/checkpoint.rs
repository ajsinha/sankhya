//! Collapsing a log's history into one file.
//!
//! # What a checkpoint is for, and who it is for
//!
//! Replaying a log costs one file open per commit. A warm process avoids that by
//! remembering what it read, but a **cold** one cannot, and neither can any reader that
//! is not SANKHYA — which is the whole point of writing an open format. A Spark job
//! reading a table with fifty thousand commits opens fifty thousand files before it
//! reads a row.
//!
//! A checkpoint is the reconciled state at a version, written once, so a reader starts
//! from it and replays only what came after.
//!
//! # Why it is written rather than derived
//!
//! Nothing about a checkpoint is new information: it is exactly what replay produces.
//! That makes it safe in a specific and useful way — **a checkpoint can always be
//! deleted**. A reader that cannot find one, or refuses to trust one, falls back to the
//! log and gets the same answer more slowly. Nothing depends on a checkpoint being
//! present, correct, or even parseable.
//!
//! That is why this is worth writing by hand and validating against another
//! implementation, rather than avoided as risky. The failure mode of a wrong checkpoint
//! is bounded by our willingness to throw it away.
//!
//! # What is not implemented
//!
//! Multi-part checkpoints, V2 checkpoints, sidecars, and every action type except
//! `protocol`, `metaData` and `add`. Removals are not written at all — a checkpoint is
//! the *reconciled* state, so a file that was added and later removed simply is not
//! there. That is the protocol's own design and not a shortcut.

use crate::log::{commit_path, log_dir, AddFile, CommitError, LiveSet, Metadata, Version};
use arrow_array::builder::{MapBuilder, MapFieldNames, StringBuilder};
use arrow_array::{
    ArrayRef, BooleanArray, Int32Array, Int64Array, ListArray, RecordBatch, StringArray,
    StructArray,
};
use arrow_buffer::{NullBuffer, OffsetBuffer};
use arrow_schema::{DataType, Field, Fields, Schema};
use parquet::arrow::ArrowWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The name of the pointer readers look for first.
const LAST_CHECKPOINT: &str = "_last_checkpoint";

fn checkpoint_path(table_root: &Path, version: Version) -> PathBuf {
    log_dir(table_root).join(format!("{version:020}.checkpoint.parquet"))
}

fn map_field_names() -> MapFieldNames {
    MapFieldNames {
        entry: "key_value".to_string(),
        key: "key".to_string(),
        value: "value".to_string(),
    }
}

/// A `map<string, string>` of `rows` empty maps.
///
/// Every map this crate writes is empty — there are no partition columns and no
/// configuration — but the *column* must exist and be correctly typed, because a reader
/// projecting the action schema fails on a missing field rather than defaulting it.
fn empty_maps(rows: usize) -> ArrayRef {
    let mut builder = MapBuilder::new(
        Some(map_field_names()),
        StringBuilder::new(),
        StringBuilder::new(),
    );
    for _ in 0..rows {
        // Appending an empty map cannot fail — the builder has no children to be out of
        // step with. Ignoring the result rather than unwrapping it keeps the crate free
        // of panic paths; a builder that did fail would produce a short array, and the
        // schema check on read would catch that.
        let _ = builder.append(true);
    }
    Arc::new(builder.finish())
}

fn map_type() -> DataType {
    DataType::Map(
        Arc::new(Field::new(
            "key_value",
            DataType::Struct(Fields::from(vec![
                Field::new("key", DataType::Utf8, false),
                Field::new("value", DataType::Utf8, true),
            ])),
            false,
        )),
        false,
    )
}

/// A `list<string>` of `rows` empty lists.
fn empty_lists(rows: usize) -> ArrayRef {
    let field = Arc::new(Field::new("element", DataType::Utf8, true));
    Arc::new(ListArray::new(
        field,
        OffsetBuffer::new(vec![0i32; rows + 1].into()),
        Arc::new(StringArray::from(Vec::<Option<&str>>::new())),
        None,
    ))
}

fn list_type() -> DataType {
    DataType::List(Arc::new(Field::new("element", DataType::Utf8, true)))
}

fn protocol_fields() -> Fields {
    Fields::from(vec![
        Field::new("minReaderVersion", DataType::Int32, false),
        Field::new("minWriterVersion", DataType::Int32, false),
    ])
}

fn format_fields() -> Fields {
    Fields::from(vec![
        Field::new("provider", DataType::Utf8, false),
        Field::new("options", map_type(), false),
    ])
}

fn metadata_fields() -> Fields {
    Fields::from(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("format", DataType::Struct(format_fields()), false),
        Field::new("schemaString", DataType::Utf8, false),
        Field::new("partitionColumns", list_type(), false),
        Field::new("configuration", map_type(), false),
        Field::new("createdTime", DataType::Int64, true),
    ])
}

fn add_fields() -> Fields {
    Fields::from(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("partitionValues", map_type(), false),
        Field::new("size", DataType::Int64, false),
        Field::new("modificationTime", DataType::Int64, false),
        Field::new("dataChange", DataType::Boolean, false),
        Field::new("stats", DataType::Utf8, true),
    ])
}

/// One row per action, with exactly one non-null column each.
fn checkpoint_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("protocol", DataType::Struct(protocol_fields()), true),
        Field::new("metaData", DataType::Struct(metadata_fields()), true),
        Field::new("add", DataType::Struct(add_fields()), true),
    ]))
}

/// Build the batch: the protocol row, the metadata row, then one row per live file.
fn checkpoint_batch(
    reader_version: u32,
    writer_version: u32,
    metadata: &Metadata,
    files: &[AddFile],
) -> Result<RecordBatch, CommitError> {
    let rows = 2 + files.len();

    // Each action column is present on exactly one row, so every column is built at full
    // height with a validity mask selecting its row. Arrow requires child arrays to be
    // full length even where the parent is null, so the unused slots carry placeholder
    // values that no reader will ever look at.
    // Row 0 carries the protocol action, row 1 the metadata. Built by position rather
    // than by index assignment so the length and the set bit cannot disagree.
    let protocol_valid: Vec<bool> = (0..rows).map(|row| row == 0).collect();
    let protocol = StructArray::new(
        protocol_fields(),
        vec![
            Arc::new(Int32Array::from(vec![
                i32::try_from(reader_version)
                    .unwrap_or(1);
                rows
            ])) as ArrayRef,
            Arc::new(Int32Array::from(vec![
                i32::try_from(writer_version)
                    .unwrap_or(2);
                rows
            ])),
        ],
        Some(NullBuffer::from(protocol_valid)),
    );

    let metadata_valid: Vec<bool> = (0..rows).map(|row| row == 1).collect();
    let format = StructArray::new(
        format_fields(),
        vec![
            Arc::new(StringArray::from(vec![
                metadata.format.provider.as_str();
                rows
            ])) as ArrayRef,
            empty_maps(rows),
        ],
        None,
    );
    let metadata_array = StructArray::new(
        metadata_fields(),
        vec![
            Arc::new(StringArray::from(vec![metadata.id.as_str(); rows])) as ArrayRef,
            Arc::new(format),
            Arc::new(StringArray::from(vec![
                metadata.schema_string.as_str();
                rows
            ])),
            empty_lists(rows),
            empty_maps(rows),
            Arc::new(Int64Array::from(vec![metadata.created_time; rows])),
        ],
        Some(NullBuffer::from(metadata_valid)),
    );

    let mut add_valid = vec![false; rows];
    for slot in add_valid.iter_mut().skip(2) {
        *slot = true;
    }
    let placeholder = AddFile::new(String::new(), 0, 0);
    let all: Vec<&AddFile> = std::iter::repeat_n(&placeholder, 2)
        .chain(files.iter())
        .collect();

    let add = StructArray::new(
        add_fields(),
        vec![
            Arc::new(StringArray::from(
                all.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            )) as ArrayRef,
            empty_maps(rows),
            Arc::new(Int64Array::from(
                all.iter()
                    .map(|f| i64::try_from(f.size).unwrap_or(i64::MAX))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                all.iter().map(|f| f.modification_time).collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                all.iter().map(|f| f.data_change).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                all.iter().map(|f| f.stats.as_deref()).collect::<Vec<_>>(),
            )),
        ],
        Some(NullBuffer::from(add_valid)),
    );

    RecordBatch::try_new(
        checkpoint_schema(),
        vec![Arc::new(protocol), Arc::new(metadata_array), Arc::new(add)],
    )
    .map_err(|e| CommitError::Io(format!("building the checkpoint batch: {e}")))
}

/// What a checkpoint recorded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CheckpointReport {
    pub version: Version,
    /// Actions written: the protocol, the metadata, and one per live file.
    pub actions: usize,
    pub bytes: u64,
    pub path: PathBuf,
}

/// Write a checkpoint for the state at `version`.
///
/// The `_last_checkpoint` pointer is written **after** the checkpoint file, and only if
/// the file was written. A pointer to a checkpoint that is not there sends every reader
/// down a path that fails, where no pointer at all simply costs them a replay.
///
/// # Errors
///
/// Returns an error if the checkpoint cannot be written. The log is untouched either
/// way — a failed checkpoint costs nothing but the attempt.
pub fn write_checkpoint(
    table_root: &Path,
    live: &LiveSet,
    metadata: &Metadata,
    reader_version: u32,
    writer_version: u32,
) -> Result<CheckpointReport, CommitError> {
    let Some(version) = live.version else {
        return Err(CommitError::Io(
            "a table with no commits has no state to checkpoint".to_string(),
        ));
    };

    let batch = checkpoint_batch(reader_version, writer_version, metadata, &live.files)?;
    let path = checkpoint_path(table_root, version);

    // Staged and renamed, like a commit: a reader must never see a partial checkpoint,
    // and here it would not even be detectably partial.
    let staging = path.with_extension("parquet.tmp");
    {
        let file = std::fs::File::create(&staging)
            .map_err(|e| CommitError::Io(format!("creating {}: {e}", staging.display())))?;
        let mut writer = ArrowWriter::try_new(file, batch.schema(), None)
            .map_err(|e| CommitError::Io(format!("opening the checkpoint writer: {e}")))?;
        writer
            .write(&batch)
            .map_err(|e| CommitError::Io(format!("writing the checkpoint: {e}")))?;
        // Synced before the rename, for the same reason `atomicfs::publish` does it: the
        // rename makes the name visible, and a name over unsynced bytes is a checkpoint that
        // exists and parses into nothing. A checkpoint is the file readers use *instead of*
        // replaying the log, so a corrupt one is not a slow read --- it is a wrong table.
        let written = writer
            .into_inner()
            .map_err(|e| CommitError::Io(format!("closing the checkpoint: {e}")))?;
        written
            .sync_all()
            .map_err(|e| CommitError::Io(format!("syncing {}: {e}", staging.display())))?;
    }
    std::fs::rename(&staging, &path)
        .map_err(|e| CommitError::Io(format!("publishing {}: {e}", path.display())))?;
    // And the directory entry the rename created.
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            directory
                .sync_all()
                .map_err(|e| CommitError::Io(format!("syncing {}: {e}", parent.display())))?;
        }
    }

    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let actions = batch.num_rows();

    // Only now, and only because the file is there.
    let pointer = serde_json::json!({
        "version": version,
        "size": actions,
        "sizeInBytes": bytes,
        "numOfAddFiles": live.files.len(),
    });
    // The checkpoint parquet above is staged and renamed; this pointer beside it was not,
    // which meant the expensive half was safe and the half a reader consults first was not.
    let encoded = serde_json::to_string(&pointer)
        .map_err(|e| CommitError::Io(format!("encoding the checkpoint pointer: {e}")))?;
    sankhya_atomicfs::publish(
        &log_dir(table_root).join(LAST_CHECKPOINT),
        encoded.as_bytes(),
    )
    .map_err(|e| CommitError::Io(format!("writing {LAST_CHECKPOINT}: {e}")))?;

    Ok(CheckpointReport {
        version,
        actions,
        bytes,
        path,
    })
}

/// The version the newest checkpoint covers, if there is one.
///
/// Returns `None` for a missing, unreadable or unparseable pointer, and for a pointer to
/// a checkpoint file that is not present. **Every failure here is a fallback rather than
/// an error**, because a checkpoint carries no information the log does not: a reader
/// that ignores it gets the same answer by replaying.
#[must_use]
pub fn latest_checkpoint(table_root: &Path) -> Option<Version> {
    let text = std::fs::read_to_string(log_dir(table_root).join(LAST_CHECKPOINT)).ok()?;
    let pointer: serde_json::Value = serde_json::from_str(&text).ok()?;
    let version = pointer.get("version")?.as_u64()?;

    // The pointer is a hint, and a hint to a file that is not there is worse than none:
    // it sends every reader down a path that fails.
    if !checkpoint_path(table_root, version).exists() {
        return None;
    }
    // A checkpoint must not claim a version the log does not have. That would mean the
    // log was rebuilt underneath it, and trusting the checkpoint would serve files from
    // a table that no longer exists.
    if !commit_path(table_root, version).exists() {
        return None;
    }
    Some(version)
}

/// The live files a checkpoint records.
///
/// # Errors
///
/// Returns an error if the file cannot be read or does not have the shape this crate
/// writes. Callers are expected to treat that as a reason to replay the log rather than
/// as a failure — see [`latest_checkpoint`] on why every problem here is a fallback.
pub fn read_checkpoint(table_root: &Path, version: Version) -> Result<Vec<AddFile>, CommitError> {
    use arrow_array::cast::AsArray;
    use arrow_array::types::Int64Type;
    use arrow_array::Array;

    let path = checkpoint_path(table_root, version);
    let file = std::fs::File::open(&path)
        .map_err(|e| CommitError::Io(format!("opening {}: {e}", path.display())))?;
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| CommitError::Io(format!("reading {}: {e}", path.display())))?
        .build()
        .map_err(|e| CommitError::Io(format!("building a checkpoint reader: {e}")))?;

    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| CommitError::Io(format!("decoding the checkpoint: {e}")))?;
        let Some(column) = batch.column_by_name("add") else {
            // No `add` column at all means this is not a checkpoint we can use. Not an
            // error in the sense that matters — the log is still there.
            continue;
        };
        let adds = column.as_struct();

        let paths = adds.column_by_name("path").ok_or_else(missing("path"))?;
        let paths = paths.as_string::<i32>();
        let sizes = adds.column_by_name("size").ok_or_else(missing("size"))?;
        let sizes = sizes.as_primitive::<Int64Type>();
        let times = adds
            .column_by_name("modificationTime")
            .ok_or_else(missing("modificationTime"))?;
        let times = times.as_primitive::<Int64Type>();
        let stats = adds.column_by_name("stats").map(|c| c.as_string::<i32>());

        for row in 0..adds.len() {
            // A row whose `add` is null is some other action. The protocol puts every
            // action in the same table with one non-null column per row.
            if !adds.is_valid(row) {
                continue;
            }
            let mut file = AddFile::new(
                paths.value(row).to_string(),
                u64::try_from(sizes.value(row)).unwrap_or(0),
                times.value(row),
            );
            if let Some(stats) = stats {
                if stats.is_valid(row) {
                    file.stats = Some(stats.value(row).to_string());
                }
            }
            out.push(file);
        }
    }

    Ok(out)
}

fn missing(field: &'static str) -> impl Fn() -> CommitError {
    move || CommitError::Io(format!("the checkpoint has no {field} column"))
}
