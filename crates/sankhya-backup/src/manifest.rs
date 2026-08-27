//! What a backup *is*, and the two positions it has to keep straight.
//!
//! # Three artefacts, and the thing that binds them
//!
//! `FR-OPS-13` requires a manifest binding the transactional backup, the table snapshots and
//! the key generation to a consistent point, and states the reason bluntly: *"three backups
//! that do not agree with each other are worse than one"*. Worse, because three that agree
//! restore a system, and three that do not restore a puzzle --- with no indication which of
//! the three is the one to trust.
//!
//! # There are two positions, and conflating them is the defect this file exists to prevent
//!
//! [`Manifest::source_restores_to`] is where the transactional store lands. [`queryable_at`]
//! is the highest position at which **every** table is complete, which is the minimum over
//! the tables' coverage --- because a query joining two tables can only be answered at a
//! position both of them reach.
//!
//! They are not the same number and they are rarely equal. Tables publish at their own
//! cadence, so at any instant some are further behind than others, and the transactional
//! store is ahead of all of them. Recording one number and calling it "the consistent point"
//! means recording whichever of the two the author happened to think of.
//!
//! [`queryable_at`]: Manifest::queryable_at
//!
//! # The rule that must hold, and why the manifest refuses to exist without it
//!
//! **No table may cover a position past where the source restores to.**
//!
//! If it does, then after a restore the analytical tier holds rows the transactional store
//! no longer has. Capture resumes from the source's position and republishes that range, so
//! those rows arrive a second time at different positions --- or they sit there permanently
//! as data with no origin. It is the shape of `SNK-S0002`, one layer up, and it is not
//! detectable afterwards from either side alone.
//!
//! So it is checked when the manifest is **built**, not when it is restored. A manifest that
//! records an inconsistency has recorded a broken backup as a backup, and the moment to find
//! that out is not the moment you need it.

use sankhya_ingest::TableDigest;
use sankhya_table_delta::Version;
use sankhya_types::Lsn;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A backup's identity.
///
/// A UUID rather than a name or a timestamp. Two backups taken in the same second by two
/// operators must not collide, and a name is something a person can reuse.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct BackupId(uuid::Uuid);

impl BackupId {
    /// A fresh identity.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    /// An identity from a UUID, for a manifest being read back.
    #[must_use]
    pub const fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl fmt::Display for BackupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "backup:{}", self.0)
    }
}

/// The transactional half.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SourceBackup {
    /// Where the artefact lives. Opaque here: this system does not take the backup, it
    /// records which one it is bound to.
    pub location: String,
    /// The position restoring it lands at.
    pub restores_to: Lsn,
    /// A digest of the artefact, so a truncated or swapped file is detectable.
    ///
    /// Of the *artefact*, not of the data --- this half is opaque, and the honest thing to
    /// record is the thing we can actually check.
    pub artefact_digest: String,
}

/// One table's contribution.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TableSnapshot {
    /// Which table.
    pub table: String,
    /// The version this backup pins.
    pub version: Version,
    /// The position this version's data covers up to.
    pub covers_to: Lsn,
    /// How many rows, and their order-independent checksum.
    ///
    /// A digest of the **data**, not of the files. A file-presence check passes on a
    /// truncated Parquet, and "verified" has to mean the data is right rather than that
    /// something is present where a file should be.
    pub rows: u64,
    /// The checksum, as a decimal string --- `u128` has no portable JSON representation and
    /// silently losing the high bits to a double is exactly the failure a digest exists to
    /// catch.
    pub checksum: String,
}

impl TableSnapshot {
    /// Record a computed digest.
    #[must_use]
    pub fn new(table: impl Into<String>, version: Version, covers_to: Lsn, digest: TableDigest) -> Self {
        Self {
            table: table.into(),
            version,
            covers_to,
            rows: digest.rows(),
            checksum: digest.checksum().to_string(),
        }
    }

    /// The recorded digest, or `None` if the checksum did not survive the round trip.
    ///
    /// `None` rather than a zero digest. A digest that fails to parse and reads as zero
    /// would compare unequal to everything, which looks like corruption of the data rather
    /// than corruption of the manifest --- and sends an operator to investigate the wrong
    /// artefact.
    #[must_use]
    pub fn digest(&self) -> Option<TableDigest> {
        let checksum: u128 = self.checksum.parse().ok()?;
        Some(TableDigest::from_parts(self.rows, checksum))
    }
}

/// Which key generation the data was written under.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct KeyGeneration {
    /// The key's name.
    pub name: String,
    /// Its version.
    pub version: u32,
}

/// A backup, as recorded.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// Which backup.
    pub id: BackupId,
    /// When it was taken, in microseconds from the epoch.
    pub taken_at: i64,
    /// Where the transactional store lands.
    pub source_restores_to: Lsn,
    /// The highest position at which every table is complete.
    ///
    /// Derived, not supplied: it is the minimum of the tables' coverage, and a caller who
    /// could set it could set it wrong.
    pub queryable_at: Lsn,
    /// The transactional half.
    pub source: SourceBackup,
    /// The tables, in name order so two manifests of the same state are byte-identical.
    pub tables: Vec<TableSnapshot>,
    /// The key generation in force.
    pub keys: KeyGeneration,
    /// Until when the snapshots this references are protected from expiry.
    pub protect_until: i64,
}

impl Manifest {
    /// Bind a backup to a consistent point, or refuse.
    ///
    /// # Errors
    ///
    /// Refuses a backup with no tables, and refuses one where any table covers a position
    /// past where the source restores to --- see the module documentation for why that is
    /// checked here rather than at restore.
    pub fn bind(
        taken_at: i64,
        source: SourceBackup,
        mut tables: Vec<TableSnapshot>,
        keys: KeyGeneration,
        protect_until: i64,
    ) -> Result<Self, InconsistentBackup> {
        if tables.is_empty() {
            return Err(InconsistentBackup::NoTables);
        }

        // Ahead of the source is the failure this refuses. Reported for every table rather
        // than for the first, because an operator fixing them one at a time learns about the
        // next one only after another full backup.
        let ahead: Vec<(String, Lsn, Lsn)> = tables
            .iter()
            .filter(|table| table.covers_to > source.restores_to)
            .map(|table| (table.table.clone(), table.covers_to, source.restores_to))
            .collect();
        if !ahead.is_empty() {
            return Err(InconsistentBackup::TableAheadOfSource { tables: ahead });
        }

        // Sorted, so that two manifests describing the same state serialise identically and
        // a diff between them shows what changed rather than what was iterated first.
        tables.sort_by(|a, b| a.table.cmp(&b.table));

        let queryable_at = tables
            .iter()
            .map(|table| table.covers_to)
            .min()
            .unwrap_or(Lsn::new(0));

        Ok(Self {
            id: BackupId::new(),
            taken_at,
            source_restores_to: source.restores_to,
            queryable_at,
            source,
            tables,
            keys,
            protect_until,
        })
    }

    /// How far the transactional store is ahead of the analytical tier.
    ///
    /// Not a fault. It is the amount of re-capture a restore implies before a cross-table
    /// query can be answered at the source's position, and an operator planning a restore
    /// window needs the number.
    #[must_use]
    pub fn recapture_span(&self) -> u64 {
        self.source_restores_to
            .get()
            .saturating_sub(self.queryable_at.get())
    }

    /// The snapshots this backup protects, as `(table, version)`.
    ///
    /// By version rather than by file list. The reachable files of a retained snapshot are
    /// already computable, a table can hold thousands of them, and a manifest that embeds a
    /// file list is a manifest that goes stale the moment compaction rewrites one.
    #[must_use]
    pub fn protected_snapshots(&self) -> Vec<(&str, Version)> {
        self.tables
            .iter()
            .map(|table| (table.table.as_str(), table.version))
            .collect()
    }

    /// Whether protection still applies at `now`.
    #[must_use]
    pub const fn protects_at(&self, now: i64) -> bool {
        now < self.protect_until
    }

    /// Render as JSON.
    ///
    /// # Errors
    ///
    /// When the manifest cannot be serialised.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Read one back.
    ///
    /// # Errors
    ///
    /// When the text is not a manifest.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

/// Why a backup could not be bound to a consistent point.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InconsistentBackup {
    /// A backup of no tables.
    NoTables,
    /// One or more tables cover a position past where the source restores to.
    TableAheadOfSource {
        /// Which tables, what they cover, and what the source restores to.
        tables: Vec<(String, Lsn, Lsn)>,
    },
}

impl fmt::Display for InconsistentBackup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTables => f.write_str(
                "a backup of no tables is not a backup: restoring it would produce a source \
                 with no analytical tier, and nothing would say so",
            ),
            Self::TableAheadOfSource { tables } => {
                write!(
                    f,
                    "refusing to record a backup whose analytical tier is ahead of its \
                     source. After restoring it, {} table(s) would hold rows the \
                     transactional store no longer has; capture would resume behind them \
                     and republish that range at different positions. Not detectable \
                     afterwards from either side alone: ",
                    tables.len()
                )?;
                for (index, (table, covers, restores)) in tables.iter().enumerate() {
                    if index > 0 {
                        f.write_str("; ")?;
                    }
                    write!(
                        f,
                        "{table} covers to {} and the source restores to {}",
                        covers.get(),
                        restores.get()
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for InconsistentBackup {}
