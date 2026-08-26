//! Writing and replaying the transaction log.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// A log version. Monotone, gapless, and starting at zero.
pub type Version = u64;

/// The reader and writer versions this crate emits.
///
/// Deliberately the lowest that expresses what is written. Claiming a higher version
/// would exclude readers for no benefit; claiming a lower one would let a reader that
/// cannot understand the log try anyway.
const MIN_READER_VERSION: u32 = 1;
const MIN_WRITER_VERSION: u32 = 2;

/// A file added to the table.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AddFile {
    /// Relative to the table root, as the protocol requires.
    pub path: String,
    /// The partition each column places this file in.
    ///
    /// **Required, and empty for an unpartitioned table — not absent.** The protocol
    /// declares this field non-nullable, and a reader that builds a typed structure from
    /// the log fails on the missing field rather than defaulting it.
    ///
    /// This was omitted in the first version of this crate. Nothing noticed: the log
    /// looked reasonable, and this crate's own reader ignored the field it did not
    /// write. The kernel rejected it on the first read, which is the entire reason the
    /// oracle test exists.
    #[serde(rename = "partitionValues")]
    pub partition_values: BTreeMap<String, String>,
    pub size: u64,
    #[serde(rename = "modificationTime")]
    pub modification_time: i64,
    /// File-level statistics, as a JSON string, or absent.
    ///
    /// The protocol carries these as an encoded string rather than a nested object, so
    /// a reader can skip parsing them entirely when it does not need them.
    ///
    /// Only `numRecords` is written. That is deliberate: it is the one statistic
    /// *required* for the log to describe the table rather than merely locate it. A
    /// planner reading the log has to know how many rows a file holds — without it, a
    /// compaction plan cannot state what it expects to merge, and the check that the
    /// merge produced what the plan said becomes uncheckable.
    ///
    /// Column bounds and null counts are not written. They would enable file pruning,
    /// but wrong bounds silently drop rows from results, and bounds are exactly the kind
    /// of thing that goes wrong quietly under type coercion. They belong with the
    /// statistics catalogue, which can be rebuilt when it is wrong.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stats: Option<String>,
    /// Whether the file's rows are part of the table.
    ///
    /// Always true here. The protocol permits false for files staged but not committed,
    /// which this crate does not do — a file is written and then committed, never
    /// half-committed.
    #[serde(rename = "dataChange")]
    pub data_change: bool,
}

impl AddFile {
    #[must_use]
    pub fn new(path: impl Into<String>, size: u64, modification_time: i64) -> Self {
        Self {
            path: path.into(),
            partition_values: BTreeMap::new(),
            size,
            modification_time,
            stats: None,
            data_change: true,
        }
    }

    /// The same, carrying the row count.
    ///
    /// Prefer this everywhere. A file in the log without a row count can be located but
    /// not planned against.
    #[must_use]
    pub fn with_rows(
        path: impl Into<String>,
        size: u64,
        modification_time: i64,
        rows: u64,
    ) -> Self {
        Self {
            stats: Some(format!(r#"{{"numRecords":{rows}}}"#)),
            ..Self::new(path, size, modification_time)
        }
    }

    /// The row count this file declares, if it declared one.
    ///
    /// Returns `None` both when statistics are absent and when they are present but do
    /// not carry `numRecords`. The caller must not treat an unknown count as zero — a
    /// plan built on that would claim to merge nothing and then merge everything.
    #[must_use]
    pub fn rows(&self) -> Option<u64> {
        let stats = self.stats.as_ref()?;
        let value: serde_json::Value = serde_json::from_str(stats).ok()?;
        value.get("numRecords")?.as_u64()
    }
}

/// A file removed from the table.
///
/// Removal from the *table*, not from storage. The file stays on disk until retirement
/// decides it is safe to delete, which is a separate decision under separate
/// preconditions.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RemoveFile {
    pub path: String,
    #[serde(rename = "deletionTimestamp")]
    pub deletion_timestamp: i64,
    #[serde(rename = "dataChange")]
    pub data_change: bool,
}

impl RemoveFile {
    /// A removal that does not change what the table contains.
    ///
    /// Compaction rewrites files without changing rows, so its removals must declare
    /// `dataChange: false`. A reader streaming changes from this table would otherwise
    /// see every compacted row as a deletion followed by a re-insertion — a stream of
    /// spurious changes proportional to how well maintenance is working, which is a
    /// perverse thing to punish.
    #[must_use]
    pub fn rewritten(path: impl Into<String>, deletion_timestamp: i64) -> Self {
        Self {
            path: path.into(),
            deletion_timestamp,
            data_change: false,
        }
    }

    /// A removal that does delete rows.
    #[must_use]
    pub fn deleted(path: impl Into<String>, deletion_timestamp: i64) -> Self {
        Self {
            path: path.into(),
            deletion_timestamp,
            data_change: true,
        }
    }
}

/// One line of the log.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Action {
    #[serde(rename = "protocol")]
    Protocol {
        #[serde(rename = "minReaderVersion")]
        min_reader_version: u32,
        #[serde(rename = "minWriterVersion")]
        min_writer_version: u32,
    },
    #[serde(rename = "metaData")]
    Metadata(Metadata),
    #[serde(rename = "add")]
    Add(AddFile),
    #[serde(rename = "remove")]
    Remove(RemoveFile),
}

/// The table's identity and shape.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub id: String,
    #[serde(rename = "schemaString")]
    pub schema_string: String,
    pub format: Format,
    #[serde(rename = "partitionColumns")]
    pub partition_columns: Vec<String>,
    pub configuration: BTreeMap<String, String>,
    #[serde(rename = "createdTime")]
    pub created_time: i64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Format {
    pub provider: String,
    pub options: BTreeMap<String, String>,
}

impl Metadata {
    /// A table of the given shape.
    ///
    /// `schema_string` is the Delta schema as JSON. It is taken as a string rather than
    /// built here because the schema is owned by the type mapping, and duplicating that
    /// translation is how the two drift apart.
    #[must_use]
    pub fn new(id: impl Into<String>, schema_string: impl Into<String>, created_time: i64) -> Self {
        Self {
            id: id.into(),
            schema_string: schema_string.into(),
            format: Format {
                provider: "parquet".to_string(),
                options: BTreeMap::new(),
            },
            partition_columns: Vec::new(),
            configuration: BTreeMap::new(),
            created_time,
        }
    }
}

/// Why a commit failed.
#[derive(Debug)]
pub enum CommitError {
    /// A commit already exists at this version.
    ///
    /// The protocol's concurrency control in its entirety: a writer picks the next
    /// version and fails if someone else took it. Failing is correct — the loser must
    /// re-read the log and rebase, because its decisions were made against a table
    /// state that no longer exists.
    VersionTaken(Version),
    /// The table root is not usable.
    Io(String),
    /// A log line could not be parsed, which means the log is not what this crate wrote.
    Malformed { version: Version, detail: String },
}

impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionTaken(v) => write!(
                f,
                "version {v} is already committed; re-read the log and rebase, because \
                 this commit was decided against a table state that no longer exists"
            ),
            Self::Io(e) => write!(f, "the table log could not be read or written: {e}"),
            Self::Malformed { version, detail } => {
                write!(f, "log version {version} is malformed: {detail}")
            }
        }
    }
}

impl std::error::Error for CommitError {}

fn log_dir(table_root: &Path) -> PathBuf {
    table_root.join("_delta_log")
}

/// The protocol's file naming: twenty digits, zero-padded, so lexical order is version
/// order. Listing the directory and sorting by name is therefore a correct replay
/// order, which is the property the padding exists for.
fn commit_path(table_root: &Path, version: Version) -> PathBuf {
    log_dir(table_root).join(format!("{version:020}.json"))
}

/// Append a commit.
///
/// Each action is one line of JSON, which is what makes the log appendable and readable
/// without parsing the whole file.
///
/// # Errors
///
/// Returns [`CommitError::VersionTaken`] if the version already exists — the whole of
/// the protocol's concurrency control — or [`CommitError::Io`] if the log cannot be
/// written.
pub fn commit(
    table_root: &Path,
    version: Version,
    actions: &[Action],
) -> Result<Version, CommitError> {
    std::fs::create_dir_all(log_dir(table_root))
        .map_err(|e| CommitError::Io(format!("creating the log directory: {e}")))?;

    let path = commit_path(table_root, version);
    if path.exists() {
        return Err(CommitError::VersionTaken(version));
    }

    let mut body = String::new();
    for action in actions {
        let line = serde_json::to_string(action)
            .map_err(|e| CommitError::Io(format!("encoding an action: {e}")))?;
        body.push_str(&line);
        body.push('\n');
    }

    // Written to a temporary name and renamed, so a reader never observes a partial
    // commit. A half-written commit file would be a log the protocol has no way to
    // describe.
    let staging = path.with_extension("json.tmp");
    std::fs::write(&staging, body)
        .map_err(|e| CommitError::Io(format!("writing {}: {e}", staging.display())))?;

    if path.exists() {
        let _ = std::fs::remove_file(&staging);
        return Err(CommitError::VersionTaken(version));
    }
    std::fs::rename(&staging, &path)
        .map_err(|e| CommitError::Io(format!("publishing {}: {e}", path.display())))?;

    Ok(version)
}

/// Every action in the log, in version order.
///
/// # Errors
///
/// Returns an error if a commit cannot be read or a line cannot be parsed.
pub fn read_actions(table_root: &Path) -> Result<Vec<(Version, Action)>, CommitError> {
    let dir = log_dir(table_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut commits: Vec<(Version, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .map_err(|e| CommitError::Io(format!("listing {}: {e}", dir.display())))?
    {
        let entry = entry.map_err(|e| CommitError::Io(format!("reading an entry: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let Ok(version) = stem.parse::<Version>() else {
            continue;
        };
        commits.push((version, entry.path()));
    }
    commits.sort_by_key(|(v, _)| *v);

    let mut out = Vec::new();
    for (version, path) in commits {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| CommitError::Io(format!("reading {}: {e}", path.display())))?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let action: Action =
                serde_json::from_str(line).map_err(|e| CommitError::Malformed {
                    version,
                    detail: e.to_string(),
                })?;
            out.push((version, action));
        }
    }

    Ok(out)
}

/// The files a table currently consists of.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LiveSet {
    /// In the order they were added.
    pub files: Vec<AddFile>,
    /// The version this reflects, or `None` for a table with no commits.
    pub version: Option<Version>,
}

impl LiveSet {
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    #[must_use]
    pub fn paths(&self) -> Vec<&str> {
        self.files.iter().map(|f| f.path.as_str()).collect()
    }
}

/// Replay the log to find which files are live.
///
/// The replay is order-sensitive and that is the point: a file added, removed and added
/// again is live, and one added twice is live once. Both are reachable through ordinary
/// compaction followed by a rebase, so neither is hypothetical.
///
/// # Errors
///
/// Returns an error if the log cannot be read or is malformed.
pub fn live_files(table_root: &Path) -> Result<LiveSet, CommitError> {
    let actions = read_actions(table_root)?;
    if actions.is_empty() {
        return Ok(LiveSet::default());
    }

    let mut files: Vec<AddFile> = Vec::new();
    let mut version = None;

    for (v, action) in actions {
        version = Some(v);
        match action {
            Action::Add(add) => {
                // An add of a path already present replaces it rather than duplicating
                // it. Duplicating would double-count every row in the file.
                if let Some(existing) = files.iter_mut().find(|f| f.path == add.path) {
                    *existing = add;
                } else {
                    files.push(add);
                }
            }
            Action::Remove(remove) => files.retain(|f| f.path != remove.path),
            Action::Protocol { .. } | Action::Metadata(_) => {}
        }
    }

    Ok(LiveSet { files, version })
}

/// The actions that create a table.
#[must_use]
pub fn create(metadata: Metadata) -> Vec<Action> {
    vec![
        Action::Protocol {
            min_reader_version: MIN_READER_VERSION,
            min_writer_version: MIN_WRITER_VERSION,
        },
        Action::Metadata(metadata),
    ]
}
