//! Keeping the files a backup needs.
//!
//! # A backup is a very long-lived lease
//!
//! `FR-OPS-14`: *"Table snapshots referenced by a backup SHALL be protected from expiry for
//! the backup's lifetime."* The machinery already exists in a different shape --- retirement
//! refuses to remove a file a retained snapshot still reaches, and `FR-STORE-21` requires
//! physical deletion to skip anything covered by a live lease. A backup is the same idea
//! with a lifetime measured in months rather than in query durations.
//!
//! # Expiry and removal are two steps, and the gap is the point
//!
//! Deleting a backup does **not** immediately release its protection.
//!
//! The failure this prevents is specific. A backup is deleted --- by an operator clearing
//! space, by a retention rule, by a script with the wrong argument. If protection lapsed at
//! that instant, the next orphan sweep would remove the files, and the backup would be
//! unrecoverable even if the manifest were restored from somewhere five minutes later. There
//! is no way back from that and no warning before it.
//!
//! So a backup is *expired* first and *removed* later, with a grace period between. It costs
//! storage that could have been reclaimed sooner, and it buys a window in which a mistake is
//! still a mistake rather than a loss. `FR-STORE-21` weighs the same two things the same way
//! for compaction --- only add files; a separate, later job removes them --- and for the same
//! reason.

use crate::manifest::{BackupId, Manifest};
use sankhya_table_delta::Version;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// How long a released backup's files stay protected anyway.
///
/// Seven days. Long enough that an accidental deletion is noticed by somebody returning from
/// a week away, which is the span over which this kind of mistake is actually caught.
pub const GRACE_MICROS: i64 = 7 * 24 * 3_600 * 1_000_000;

/// What a backup is doing to the snapshots it references.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    /// Live: protected because the backup is still wanted.
    Held,
    /// Expired, and still protected through the grace period.
    ///
    /// Reported separately from `Held` so an operator can see storage that is retained for
    /// no ongoing reason and know when it will be released.
    Grace {
        /// When protection actually lapses.
        until: i64,
    },
    /// Released. The files are sweepable.
    Released,
}

/// Every backup whose snapshots are protected.
#[derive(Debug, Default)]
pub struct Protection {
    /// When each backup was expired, if it has been.
    expired_at: BTreeMap<BackupId, i64>,
    /// The snapshots each backup references.
    referenced: BTreeMap<BackupId, Vec<(String, Version)>>,
    /// Until when each backup's own policy protects it.
    protect_until: BTreeMap<BackupId, i64>,
}

impl Protection {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a backup's protection.
    pub fn register(&mut self, manifest: &Manifest) {
        self.referenced.insert(
            manifest.id,
            manifest
                .protected_snapshots()
                .into_iter()
                .map(|(table, version)| (table.to_string(), version))
                .collect(),
        );
        self.protect_until.insert(manifest.id, manifest.protect_until);
    }

    /// Mark a backup no longer wanted. Its files stay protected for the grace period.
    ///
    /// Returns when protection will actually lapse, so a caller can say so rather than
    /// leaving an operator to wonder why the space has not come back.
    pub fn expire(&mut self, backup: BackupId, now: i64) -> i64 {
        let entry = self.expired_at.entry(backup).or_insert(now);
        entry.saturating_add(GRACE_MICROS)
    }

    /// Remove a backup's protection entirely.
    ///
    /// Only takes effect once the grace period has passed; a caller that removes too early
    /// gets `false` and the protection stands. Deliberately not an error: a sweep calling
    /// this over every expired backup should skip the ones that are not ready, not stop.
    pub fn remove(&mut self, backup: BackupId, now: i64) -> bool {
        let Some(expired) = self.expired_at.get(&backup) else {
            return false;
        };
        if now < expired.saturating_add(GRACE_MICROS) {
            return false;
        }
        self.expired_at.remove(&backup);
        self.referenced.remove(&backup);
        self.protect_until.remove(&backup);
        true
    }

    /// What a backup's protection is doing now.
    #[must_use]
    pub fn standing(&self, backup: BackupId, now: i64) -> Standing {
        if !self.referenced.contains_key(&backup) {
            return Standing::Released;
        }
        if let Some(expired) = self.expired_at.get(&backup) {
            return Standing::Grace {
                until: expired.saturating_add(GRACE_MICROS),
            };
        }
        // A backup past its own `protect_until` is in grace from that moment, so a policy
        // horizon and an explicit expiry behave identically. One code path for both, because
        // two would eventually disagree about which files are safe to remove.
        match self.protect_until.get(&backup) {
            Some(until) if now >= *until => Standing::Grace {
                until: until.saturating_add(GRACE_MICROS),
            },
            _ => Standing::Held,
        }
    }

    /// Every snapshot that must survive an orphan sweep at `now`.
    ///
    /// Feeds the reachability set that retirement already computes. A table appearing here
    /// means "keep everything this version reaches", which is a question the log answers.
    #[must_use]
    pub fn retained_snapshots(&self, now: i64) -> BTreeSet<(String, Version)> {
        let mut out = BTreeSet::new();
        for (backup, snapshots) in &self.referenced {
            if matches!(self.standing(*backup, now), Standing::Released) {
                continue;
            }
            for (table, version) in snapshots {
                out.insert((table.clone(), *version));
            }
        }
        out
    }

    /// How many backups are holding storage.
    #[must_use]
    pub fn len(&self) -> usize {
        self.referenced.len()
    }

    /// Whether nothing is protected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.referenced.is_empty()
    }

    /// Backups whose grace has passed and whose protection can now be dropped.
    #[must_use]
    pub fn sweepable(&self, now: i64) -> Vec<BackupId> {
        self.expired_at
            .iter()
            .filter(|(_, expired)| now >= expired.saturating_add(GRACE_MICROS))
            .map(|(backup, _)| *backup)
            .collect()
    }
}

impl fmt::Display for Standing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Held => f.write_str("held"),
            Self::Grace { .. } => f.write_str("expired, in grace"),
            Self::Released => f.write_str("released"),
        }
    }
}
