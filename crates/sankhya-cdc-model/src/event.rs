//! The decoded event model.

use sankhya_types::{Lsn, Timestamp};
use std::sync::Arc;

/// How much of the previous row version the source will send for updates and deletes.
///
/// This determines whether SANKHYA can identify the row being changed at all. A table
/// with no key and the default setting causes the *database itself* to reject updates
/// and deletes, so onboarding detects it rather than discovering it at the first write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReplicaIdentity {
    /// Key columns only. The common, cheap case.
    Default,
    /// Nothing. Updates and deletes are rejected by the source.
    Nothing,
    /// Every column. Complete before-images, at the cost of substantially more log volume.
    Full,
    /// A named unique index.
    Index,
}

impl ReplicaIdentity {
    /// Decode the wire byte. Unknown values are rejected rather than guessed.
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'd' => Some(Self::Default),
            b'n' => Some(Self::Nothing),
            b'f' => Some(Self::Full),
            b'i' => Some(Self::Index),
            _ => None,
        }
    }

    /// Whether a before-image is available for updates and deletes.
    #[must_use]
    pub const fn identifies_rows(self) -> bool {
        !matches!(self, Self::Nothing)
    }
}

/// One column in a relation's shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ColumnDescriptor {
    pub name: String,
    /// Source type identifier. Mapping is the schema crate's concern, not this one's.
    pub type_oid: u32,
    /// Type modifier, carrying precision and scale where the type has them.
    pub type_modifier: i32,
    /// Whether this column participates in the row's identity.
    pub is_key: bool,
}

/// A relation's shape as the source most recently described it.
///
/// Shared behind a reference count because a single relation description applies to
/// every subsequent row message for that relation, and copying it per row would
/// dominate decode cost on narrow tables.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RelationDescriptor {
    pub relation_id: u32,
    pub namespace: String,
    pub name: String,
    pub replica_identity: ReplicaIdentity,
    pub columns: Vec<ColumnDescriptor>,
}

/// One column value within a row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TupleValue {
    /// SQL null.
    Null,
    /// Present, in the source's textual representation.
    Text(String),
    /// Present, in the source's binary representation.
    Binary(Vec<u8>),
    /// **Not transmitted because it did not change.**
    ///
    /// This is the single most dangerous value in the protocol. It appears when a
    /// large out-of-line value was left alone by an update. Writing a null in its
    /// place destroys real data and produces a row that looks entirely plausible.
    ///
    /// It is a distinct variant precisely so that no consumer can handle it by
    /// accident: matching on this enum forces a decision.
    Unchanged,
}

impl TupleValue {
    /// Whether this value carries data that may be written downstream.
    ///
    /// Returns `false` for [`TupleValue::Unchanged`], which must be resolved against
    /// the current row rather than written.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        matches!(self, Self::Null | Self::Text(_) | Self::Binary(_))
    }
}

/// A row image.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TupleData {
    pub values: Vec<TupleValue>,
}

impl TupleData {
    /// Whether any value was withheld as unchanged.
    #[must_use]
    pub fn has_unchanged(&self) -> bool {
        self.values
            .iter()
            .any(|v| matches!(v, TupleValue::Unchanged))
    }
}

/// A decoded protocol message.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Message {
    /// Transaction start. Everything until the matching commit belongs to it.
    Begin {
        final_lsn: Lsn,
        commit_time: Timestamp,
        xid: u32,
    },
    /// Transaction end. **A transaction is sealed only here**, and only sealed
    /// transactions are eligible to be flushed — a batch never splits one.
    Commit {
        commit_lsn: Lsn,
        end_lsn: Lsn,
        commit_time: Timestamp,
    },
    /// A relation's shape. Sent before the first row message for that relation, and
    /// again whenever the shape changes.
    Relation(Arc<RelationDescriptor>),
    /// A type description for a non-built-in type.
    Type {
        type_oid: u32,
        namespace: String,
        name: String,
    },
    Insert {
        relation_id: u32,
        new: TupleData,
    },
    /// An update. `old` is present only when the source sends a before-image.
    Update {
        relation_id: u32,
        old: Option<TupleData>,
        key_only: bool,
        new: TupleData,
    },
    /// A delete. `old` identifies the row; `key_only` says whether it is the full row.
    Delete {
        relation_id: u32,
        old: TupleData,
        key_only: bool,
    },
    /// Truncate, which may name several relations at once.
    Truncate {
        relation_ids: Vec<u32>,
        cascade: bool,
        restart_identity: bool,
    },
    /// Origin of a replicated change, used to avoid loops in bidirectional setups.
    Origin {
        commit_lsn: Lsn,
        name: String,
    },
    /// A logical message, transactional or not. SANKHYA uses these as archival
    /// attestations — for provenance, never as a safety mechanism.
    Logical {
        transactional: bool,
        lsn: Lsn,
        prefix: String,
        content: Vec<u8>,
    },
    /// Start of an in-progress transaction streamed before commit.
    StreamStart {
        xid: u32,
        first_segment: bool,
    },
    /// End of a streamed segment.
    StreamStop,
    /// A streamed transaction committed.
    StreamCommit {
        xid: u32,
        commit_lsn: Lsn,
        end_lsn: Lsn,
        commit_time: Timestamp,
    },
    /// A streamed transaction aborted. All buffered state for it is discarded.
    StreamAbort {
        xid: u32,
        subtransaction_xid: u32,
    },
}

impl Message {
    /// Whether this message ends a transaction and therefore seals it for flushing.
    #[must_use]
    pub const fn seals_transaction(&self) -> bool {
        matches!(self, Self::Commit { .. } | Self::StreamCommit { .. })
    }

    /// The relation this message concerns, where it concerns exactly one.
    #[must_use]
    pub const fn relation_id(&self) -> Option<u32> {
        match self {
            Self::Insert { relation_id, .. }
            | Self::Update { relation_id, .. }
            | Self::Delete { relation_id, .. } => Some(*relation_id),
            _ => None,
        }
    }
}
