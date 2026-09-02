//! What a snapshot records.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The version of one table, at the instant a snapshot was taken.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Pinned {
    /// The log version this table stood at.
    pub version: u64,
}

/// A named instant across many tables.
///
/// # Why the tables are a map and not a list
///
/// Because the question asked of it is always *"what version of this table?"*, from the read
/// path and from the sweeper alike, and a list would make both of them scan. It is also what
/// makes [`Snapshot::pins`] a lookup rather than a search.
///
/// # Why there is no "everything" form
///
/// A snapshot naming *the warehouse* rather than a set of tables would silently change meaning
/// as the warehouse grew: the same name would pin a different set on Tuesday than on Monday,
/// and two runs quoting it would not be reading the same thing. The set is fixed when it is
/// taken.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// What it is called.
    pub name: String,
    /// When it was taken, in microseconds from the epoch.
    pub taken_at: i64,
    /// Who took it, for the audit and for `SHOW SNAPSHOTS`.
    ///
    /// Recorded because a snapshot holds storage on somebody's behalf, and a cost with no owner
    /// is the shape `RSK-35` describes.
    pub taken_by: String,
    /// The day it stops being honoured, as days from the epoch.
    ///
    /// Not an `Option`. `ADR-0019` Decision 3 makes the expiry mandatory and spells no
    /// unbounded form, so there is no value here meaning *never* --- a type that could hold one
    /// is a type somebody will eventually put one in.
    pub expires_on: i32,
    /// The version each table stood at, by its qualified `schema.table` name.
    pub tables: BTreeMap<String, Pinned>,
}

impl Snapshot {
    /// A snapshot of these tables at these versions.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        taken_by: impl Into<String>,
        taken_at: i64,
        expires_on: i32,
        tables: BTreeMap<String, Pinned>,
    ) -> Self {
        Self {
            name: name.into(),
            taken_by: taken_by.into(),
            taken_at,
            expires_on,
            tables,
        }
    }

    /// The version this snapshot pins for a table, or `None` if it names no such table.
    ///
    /// # Why `None` is not "the current version"
    ///
    /// `ADR-0019` Decision 2. A table this snapshot does not name is a table that **did not
    /// exist** when it was taken, and reading it as of the snapshot is refused rather than
    /// answered --- with its current rows, which would silently mix two instants, or with no
    /// rows, which is worse: a table that did not exist is not a table that was empty, and a
    /// join against it returns the rows surviving an inner join with nothing. That is a
    /// confident zero, reported as success.
    ///
    /// So this returns `None` and the caller refuses. It must not resolve it to anything.
    #[must_use]
    pub fn pins(&self, qualified: &str) -> Option<Pinned> {
        self.tables.get(qualified).copied()
    }

    /// Every table this snapshot names, in a stable order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tables.keys().map(String::as_str)
    }

    /// How many tables it pins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tables.len()
    }

    /// Whether it pins nothing at all.
    ///
    /// A snapshot of no tables is legal and useless, and it is not refused: a warehouse with no
    /// readable tables produces one, and refusing there would report an entitlement problem as
    /// a syntax problem.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// The document this snapshot is stored as.
    ///
    /// # Errors
    ///
    /// [`Malformed`] when it cannot be rendered, which means this type and `serde` disagree.
    pub fn to_document(&self) -> Result<String, Malformed> {
        serde_json::to_string_pretty(self).map_err(|error| Malformed {
            detail: error.to_string(),
        })
    }

    /// Read a snapshot from its document.
    ///
    /// # Errors
    ///
    /// [`Malformed`] when the document is not one. A snapshot that cannot be read is **not** a
    /// snapshot that pins nothing: treating it as absent would let the sweeper reclaim files it
    /// protects, so every caller must refuse rather than continue.
    pub fn from_document(text: &str) -> Result<Self, Malformed> {
        serde_json::from_str(text).map_err(|error| Malformed {
            detail: error.to_string(),
        })
    }
}

/// A snapshot document that could not be read or written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Malformed {
    /// What went wrong.
    pub detail: String,
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the snapshot document could not be read: {}. It is refused rather than treated \
             as pinning nothing --- a snapshot nobody can read still protects files, and \
             ignoring it would let them be reclaimed under a reader",
            self.detail
        )
    }
}

impl std::error::Error for Malformed {}
