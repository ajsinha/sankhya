//! Backups that are bound to a point, protected while they are wanted, and proven.
//!
//! Three requirements, and each exists because of a specific way backups fail.
//!
//! **`FR-OPS-13`** — *three backups that do not agree with each other are worse than one*.
//! Three that agree restore a system; three that do not restore a puzzle, with nothing to
//! say which is the one to trust. So [`manifest::Manifest`] binds all three artefacts and
//! **refuses to exist** when they cannot be bound, rather than recording the disagreement
//! for somebody to find later.
//!
//! **`FR-OPS-14`** — the snapshots a backup names have to survive. [`protect`] holds them,
//! and separates expiry from removal so that deleting a backup by mistake is a mistake
//! rather than a loss.
//!
//! **`FR-OPS-15`** — *an untested backup is a rumour*. [`drill`] reads the data back and
//! recomputes its digest, because a file-presence check passes on every failure that
//! actually happens, and it keeps an append-only record that includes the failures.

#![doc(html_root_url = "https://docs.rs/sankhya-backup")]

pub mod drill;
pub mod manifest;
pub mod protect;
pub mod warehouse;

pub use drill::{Evidence, TableOutcome};
pub use manifest::{BackupId, InconsistentBackup, KeyGeneration, Manifest, SourceBackup, TableSnapshot};
pub use protect::{Protection, Standing};
pub use warehouse::Warehouse;
