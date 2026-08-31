//! The four layers standing between an archival purge and a propagated delete.
//!
//! # The trap, stated first
//!
//! `DEC-15`: the capture path replicates deletes. An ordinary `DELETE` used to purge tiered
//! data would faithfully propagate and **erase from the published tier exactly the data the
//! purge was meant to preserve**. The purge would work, the replication would work, and the
//! archive would be gone.
//!
//! So an archival purge has to be distinguishable from a business delete **structurally rather
//! than by convention**, which is what partition detach is for: it removes rows without
//! emitting row-level events, because it is a catalog operation and logical decoding reads row
//! changes.
//!
//! # Four layers, only the first load-bearing
//!
//! | | Layer | Where it lives | What it is for |
//! |---|---|---|---|
//! | 1 | Purge is detach then drop, never row deletion | [`crate::machine::Phase`] --- the chain has no delete in it, and enumerating the phases is the audit | The property itself |
//! | 2 | Delete and truncate excluded from the publication | [`crate::policy::Ineligible::PublicationPropagatesDeletes`], refused at policy creation | A defective code path cannot propagate what is not published |
//! | 3 | The applier refuses a delete or truncate in an archived range | [`Extents::consider`] | A defect upstream of the publication is still caught |
//! | 4 | A marker committed with the registry change | [`Marker`] | Provenance, and **never** safety |
//!
//! **Layer 4 is documentation, not defence, and the distinction is deliberate.** A scheme in
//! which deletes are emitted and the applier is expected to suppress them between two markers
//! fails if a marker is lost, reordered, or the applier restarts mid-bracket. Never make a
//! safety property depend on a message arriving. It is here so that a person reading the
//! applier's history years later can see that a purge happened and when.
//!
//! # Two traps recorded so nobody re-proposes them
//!
//! `session_replication_role = 'replica'` disables triggers and rules and has **no effect on
//! logical decoding**, which reads the write-ahead log directly. The deletes are still decoded.
//!
//! There is no row-level path the replication slot does not observe. Every row change on a
//! logged, published table is written to the log and decoded, and `TRUNCATE` is decoded too.
//! Only catalog operations are invisible, which is the whole reason detach is the answer.

use crate::registry::{Range, Registry};
use std::collections::BTreeMap;
use std::fmt;

/// What a change does to a row, as far as this decision cares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    /// A new row.
    Insert,
    /// A row modified in place.
    Update,
    /// A row removed.
    Delete,
    /// Every row of a relation removed at once.
    Truncate,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Truncate => "truncate",
        })
    }
}

/// Where a change sits relative to the tiering key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum At {
    /// The tiering key's ordinal for the row.
    Ordinal(i64),
    /// The change carries no usable key.
    ///
    /// A delete decoded from a relation with no replica identity is the ordinary cause. It is
    /// **not** a reason to apply: see [`Reason::UnlocatableAgainstArchive`].
    Unknown,
    /// The change names a whole relation and no row at all.
    WholeRelation,
}

/// Why a change may not be applied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// A delete whose key falls inside an archived range.
    ///
    /// The row it names is not in the source any more --- it was purged --- so the only thing
    /// this can remove is the archived copy. `FR-TIER-06` makes it a **fatal alarm** rather
    /// than a warning or a skipped record, and the severity is the point: a skipped record is
    /// a decision made silently, and a warning is a decision made by whoever reads the log.
    DeleteInArchivedRange,
    /// A truncate against a table with anything archived.
    ///
    /// Fatal whether or not a range can be worked out, because a truncate names no rows: there
    /// is no key to compare, and "every row" necessarily includes every archived one.
    TruncateWithArchive,
    /// A delete against a table with something archived, whose key cannot be read.
    ///
    /// The fail-closed case, and the one worth stating plainly: *"we could not tell"* must not
    /// become *"apply"*. A delete that cannot be located might be in an archived range, and the
    /// cost of halting on one that was not is an operator's afternoon --- against a permanent,
    /// undetectable loss of a retained record.
    UnlocatableAgainstArchive,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeleteInArchivedRange => f.write_str(
                "the row it names was purged from the source, so the only copy this can remove \
                 is the archived one",
            ),
            Self::TruncateWithArchive => f.write_str(
                "a truncate names no rows, so there is no key to compare and `every row` \
                 includes every archived one",
            ),
            Self::UnlocatableAgainstArchive => f.write_str(
                "the change carries no usable key, so it cannot be shown to fall outside an \
                 archived range --- and `could not tell` is not `apply`",
            ),
        }
    }
}

/// A change the applier must not apply, and must not continue past.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Alarm {
    /// The table.
    pub table: String,
    /// What the change was.
    pub change: Change,
    /// Why it was refused.
    pub reason: Reason,
    /// The archived range it landed in, where one could be identified.
    pub extent: Option<Range>,
}

impl fmt::Display for Alarm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fatal: a {} on `{}` was refused --- {}", self.change, self.table, self.reason)?;
        if let Some(extent) = self.extent {
            write!(f, " (archived range {extent})")?;
        }
        f.write_str(
            ". The applier stops here: this is a defect somewhere upstream, and applying the \
             next change would be applying it on top of one that should never have arrived",
        )
    }
}

/// What the applier should do with a change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Nothing archived is at risk.
    Apply,
    /// Stop.
    ///
    /// Not `Skip`. `FR-TIER-06` is explicit that this is neither a warning nor a skipped
    /// record, and a skip would leave the source and the published tier permanently disagreeing
    /// about a row nobody was told about.
    Halt(Alarm),
}

impl Verdict {
    /// Whether the applier may proceed.
    #[must_use]
    pub const fn is_apply(&self) -> bool {
        matches!(self, Self::Apply)
    }
}

/// Which ranges of which tables are archived.
///
/// Built from a [`Registry`], which is the authority for the cold extent. Held by the applier
/// so the check costs a lookup rather than a query --- `FR-TIER-06` says *the applier SHALL
/// hold the archival extent map*, and an applier that had to ask something else would be an
/// applier that stops checking when the something else is slow.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Extents {
    by_table: BTreeMap<String, Vec<Range>>,
}

impl Extents {
    /// Nothing is archived.
    ///
    /// The state of every deployment today, and the honest default: an applier with no extent
    /// map refuses nothing, rather than refusing everything or silently doing neither.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The archived ranges a registry knows about.
    #[must_use]
    pub fn of(registry: &Registry) -> Self {
        let mut by_table: BTreeMap<String, Vec<Range>> = BTreeMap::new();
        for entry in registry.entries() {
            by_table.entry(entry.table.clone()).or_default().push(entry.range);
        }
        Self { by_table }
    }

    /// Whether anything of this table is archived.
    #[must_use]
    pub fn has_archive(&self, table: &str) -> bool {
        self.by_table.get(table).is_some_and(|ranges| !ranges.is_empty())
    }

    /// The archived range containing `at`, if any.
    #[must_use]
    pub fn covering(&self, table: &str, at: i64) -> Option<Range> {
        self.by_table
            .get(table)?
            .iter()
            .find(|range| range.contains(at))
            .copied()
    }

    /// What the applier should do with one change.
    #[must_use]
    pub fn consider(&self, table: &str, change: Change, at: At) -> Verdict {
        // Insert and update are outside this layer's claim. `FR-TIER-06` names delete and
        // truncate, and an update reaching an archived range is a different failure with a
        // different answer --- `FR-TIER-19`'s compensating entry --- rather than a quieter
        // version of this one.
        if !matches!(change, Change::Delete | Change::Truncate) {
            return Verdict::Apply;
        }
        if !self.has_archive(table) {
            return Verdict::Apply;
        }

        let alarm = |reason, extent| {
            Verdict::Halt(Alarm { table: table.to_string(), change, reason, extent })
        };
        match (change, at) {
            (Change::Truncate, _) | (_, At::WholeRelation) => {
                alarm(Reason::TruncateWithArchive, None)
            }
            (_, At::Unknown) => alarm(Reason::UnlocatableAgainstArchive, None),
            (_, At::Ordinal(at)) => match self.covering(table, at) {
                Some(extent) => alarm(Reason::DeleteInArchivedRange, Some(extent)),
                None => Verdict::Apply,
            },
        }
    }
}

/// The record committed alongside a registry change, for provenance.
///
/// # Why this is not a safety mechanism, said where somebody would be tempted
///
/// It is the fourth layer and the only one that could be lost without anything noticing. A
/// design in which deletes are emitted and the applier suppresses them between two markers
/// fails if a marker is lost, reordered, or the applier restarts mid-bracket --- and it fails
/// *open*, by applying the deletes. **Never make a safety property depend on a message
/// arriving.** Layers one to three hold without this; this exists so a person reading the
/// history later can see that a purge happened, to what, and when.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Marker {
    /// The purge that wrote it.
    pub purge: String,
    /// The table.
    pub table: String,
    /// The range that left the source.
    pub range: Range,
    /// When, in microseconds from the epoch.
    pub at: i64,
}

impl fmt::Display for Marker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "archival purge {} removed {} of `{}` from the system of record at {} --- \
             provenance only, and no delete was emitted for it",
            self.purge, self.range, self.table, self.at
        )
    }
}
