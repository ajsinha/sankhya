//! Writing and replaying the transaction log.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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
    /// `numRecords` is required: without it the log describes where a file is but not
    /// what is in it, a compaction plan cannot state what it expects to merge, and the
    /// check that the merge produced what the plan said becomes uncheckable.
    ///
    /// `minValues`, `maxValues` and `nullCount` are written where they are known.
    ///
    /// **This reverses an earlier decision, deliberately.** They were withheld on the
    /// grounds that a wrong bound silently drops rows and bounds go wrong quietly under
    /// type coercion — which is true, and is why every bound written here comes from
    /// code that refuses to produce one it cannot justify: an unrecognised type gets no
    /// bound, an unorderable value gets no bound, and a merge that would narrow a bound
    /// drops it instead.
    ///
    /// Withholding them had a cost that the original reasoning did not weigh: an
    /// external engine reading these tables can prune only on what the log tells it.
    /// Keeping bounds private to SANKHYA means every other reader scans everything,
    /// which undercuts the reason for using an open format at all.
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

    /// The same, carrying everything known about the file.
    #[must_use]
    pub fn with_statistics(
        path: impl Into<String>,
        size: u64,
        modification_time: i64,
        stats: &crate::stats::FileStatistics,
    ) -> Self {
        Self {
            // An unencodable statistics document leaves the file with none rather than
            // failing the commit. The file is still correct and still readable; only
            // pruning is lost, which costs a scan.
            stats: stats.encode().ok(),
            ..Self::new(path, size, modification_time)
        }
    }

    /// Everything the log records about this file's contents, if anything.
    #[must_use]
    pub fn statistics(&self) -> Option<crate::stats::FileStatistics> {
        crate::stats::FileStatistics::decode(self.stats.as_ref()?).ok()
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
    /// The version would leave a gap in the log.
    NonContiguous {
        attempted: Version,
        expected: Version,
    },
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
            Self::NonContiguous {
                attempted,
                expected,
            } => write!(
                f,
                "committing version {attempted} would leave a gap; the next version is \
                 {expected}. A gap makes it impossible to tell whether a log has more \
                 commits without listing all of them, and a reader that probes forward \
                 would stop at the gap and silently serve an incomplete file set"
            ),
        }
    }
}

impl std::error::Error for CommitError {}

pub(crate) fn log_dir(table_root: &Path) -> PathBuf {
    table_root.join("_delta_log")
}

/// The protocol's file naming: twenty digits, zero-padded, so lexical order is version
/// order. Listing the directory and sorting by name is therefore a correct replay
/// order, which is the property the padding exists for.
pub(crate) fn commit_path(table_root: &Path, version: Version) -> PathBuf {
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

    // Versions must be contiguous. The protocol requires it, and this system depends on
    // it for something specific: a reader that knows the state at version *n* can find
    // out whether anything is newer by asking whether *n+1* exists — one probe instead of
    // listing a directory that grows without bound. A gap would make that probe stop
    // early and silently serve a file set missing everything past the gap.
    if version > 0 && !commit_path(table_root, version - 1).exists() {
        return Err(CommitError::NonContiguous {
            attempted: version,
            expected: previous_version(table_root).map_or(0, |v| v + 1),
        });
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

/// The commit files present, in version order.
///
/// Separated from reading them because listing is cheap and reading is not: a caller
/// that only needs to know whether anything has changed can stop here.
///
/// # Errors
///
/// Returns an error if the log directory cannot be listed.
pub fn commits(table_root: &Path) -> Result<Vec<(Version, PathBuf)>, CommitError> {
    let dir = log_dir(table_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut out: Vec<(Version, PathBuf)> = Vec::new();
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
        out.push((version, entry.path()));
    }
    out.sort_by_key(|(v, _)| *v);
    Ok(out)
}

/// The highest version present, found without listing the whole directory.
///
/// Probes forward from `after`, relying on versions being contiguous — which [`commit`]
/// enforces. This is what keeps a cached reader's cost proportional to what has changed
/// rather than to the table's whole history.
///
/// # Errors
///
/// Never returns an error; the signature matches its neighbours so a caller can treat
/// them uniformly.
#[must_use]
pub fn newest_after(table_root: &Path, after: Option<Version>) -> Option<Version> {
    let mut probe = after.map_or(0, |v| v + 1);
    if !commit_path(table_root, probe).exists() {
        return after.filter(|v| commit_path(table_root, *v).exists());
    }
    while commit_path(table_root, probe + 1).exists() {
        probe += 1;
    }
    Some(probe)
}

fn previous_version(table_root: &Path) -> Option<Version> {
    // The probe rather than a listing: it is cheaper, and it does not depend on the
    // order the filesystem hands back directory entries.
    newest_after(table_root, None)
}

/// Every action in the log, in version order.
///
/// # Errors
///
/// Returns an error if a commit cannot be read or a line cannot be parsed.
pub fn read_actions(table_root: &Path) -> Result<Vec<(Version, Action)>, CommitError> {
    read_actions_after(table_root, None)
}

/// Every action in commits strictly after `after`, in version order.
///
/// `None` means from the beginning. This is what makes an incremental replay possible:
/// a caller holding the state as of version *n* need only read what came after it.
///
/// # Errors
///
/// Returns an error if a commit cannot be read or a line cannot be parsed.
pub fn read_actions_after(
    table_root: &Path,
    after: Option<Version>,
) -> Result<Vec<(Version, Action)>, CommitError> {
    // Walked forward rather than listed. Listing costs one directory read proportional
    // to the table's whole history, which for a caller resuming after a single commit
    // dwarfs the work it came to do — 18 ms of listing to read one 200-byte file, at
    // fifty thousand commits.
    //
    // Sound because versions are contiguous, which `commit` enforces. A gap would stop
    // this walk early and silently return an incomplete set of actions, which is exactly
    // why that rule is enforced rather than assumed.
    let mut to_read: Vec<(Version, PathBuf)> = Vec::new();
    let mut version = after.map_or(0, |v| v + 1);
    loop {
        let path = commit_path(table_root, version);
        if !path.exists() {
            break;
        }
        to_read.push((version, path));
        version += 1;
    }

    let mut out = Vec::new();
    for (version, path) in to_read {
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
    // Start from a checkpoint if there is a usable one, and from nothing if there is not.
    //
    // Every failure here falls back to a full replay rather than surfacing, because a
    // checkpoint carries no information the log does not. That is what makes a checkpoint
    // safe to write by hand: the worst a bad one can do is be ignored.
    let base = crate::checkpoint::latest_checkpoint(table_root)
        .and_then(|version| {
            let files = crate::checkpoint::read_checkpoint(table_root, version).ok()?;
            Some(LiveSet {
                files,
                version: Some(version),
            })
        })
        .unwrap_or_default();

    advance(table_root, &base)
}

/// The live set as of the newest commit, starting from a known earlier one.
///
/// Reads only the commits after `base.version`, so a caller that already knows the state
/// at version *n* pays for what has happened since rather than for the whole history.
/// Passing a default `base` is a full replay.
///
/// # Errors
///
/// Returns an error if the log cannot be read or is malformed.
pub fn advance(table_root: &Path, base: &LiveSet) -> Result<LiveSet, CommitError> {
    let mut replay = Replay::from(base.clone());
    replay.advance(table_root)?;
    Ok(replay.into_live_set())
}

/// A replay that can be resumed without rebuilding its index.
///
/// The index is the whole reason this type exists. Rebuilding it from a file list costs
/// one pass over every live file, which for a caller resuming after a single new commit
/// is the same shape of waste the resumption was meant to avoid — it trades "linear in
/// the history" for "linear in the table", which is better and still not right.
#[derive(Clone, Debug, Default)]
pub struct Replay {
    files: Vec<Option<AddFile>>,
    position: HashMap<String, usize>,
    version: Option<Version>,
}

impl From<LiveSet> for Replay {
    fn from(live: LiveSet) -> Self {
        let position = live
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        Self {
            files: live.files.into_iter().map(Some).collect(),
            position,
            version: live.version,
        }
    }
}

impl Replay {
    /// The version this replay reflects.
    #[must_use]
    pub const fn version(&self) -> Option<Version> {
        self.version
    }

    /// Read and apply every commit newer than this replay.
    ///
    /// # Errors
    ///
    /// Returns an error if the log cannot be read or is malformed.
    pub fn advance(&mut self, table_root: &Path) -> Result<usize, CommitError> {
        let actions = read_actions_after(table_root, self.version)?;
        let count = actions.len();
        for (version, action) in actions {
            self.version = Some(version);
            match action {
                // `position` is only ever written alongside a push to `files`, so every
                // index it holds is in range. Resolving through `get_mut` rather than
                // indexing keeps that invariant from being the only thing standing
                // between a malformed log and a panic during replay.
                Action::Add(add) => match self.position.get(&add.path).copied() {
                    Some(index) => {
                        if let Some(slot) = self.files.get_mut(index) {
                            *slot = Some(add);
                        }
                    }
                    None => {
                        self.position.insert(add.path.clone(), self.files.len());
                        self.files.push(Some(add));
                    }
                },
                Action::Remove(remove) => {
                    if let Some(index) = self.position.remove(&remove.path) {
                        if let Some(slot) = self.files.get_mut(index) {
                            *slot = None;
                        }
                    }
                }
                Action::Protocol { .. } | Action::Metadata(_) => {}
            }
        }
        Ok(count)
    }

    /// The live set as it now stands.
    #[must_use]
    pub fn live_set(&self) -> LiveSet {
        LiveSet {
            files: self.files.iter().flatten().cloned().collect(),
            version: self.version,
        }
    }

    #[must_use]
    fn into_live_set(self) -> LiveSet {
        LiveSet {
            files: self.files.into_iter().flatten().collect(),
            version: self.version,
        }
    }
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
