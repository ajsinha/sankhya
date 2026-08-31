//! The seven ways a clone operation is refused.
//!
//! # Why these are refusals and not warnings
//!
//! Each is a way the same failure arrives from a different direction: **silent data loss in a
//! table nobody was touching**, discovered when somebody reads a clone months later. A warning
//! defers that decision to whoever reads the log, and by then the clone is already made or the
//! origin already dropped.
//!
//! `ADR-0016` names four and building the list found three more. They are here together because
//! reading them together is the point --- the audit `FR-TIER-03` made possible for purge is the
//! same idea, and a list nobody can enumerate is a list nobody can check.
//!
//! # Why every predicate is pure
//!
//! Each takes the facts it needs as arguments rather than reading them. A refusal that had to
//! open a log to decide could not be tested against the case it exists for, and the cases it
//! exists for --- a purge in flight, a schema mid-evolution, a version already retired --- are
//! exactly the ones that are awkward to arrange for real.

use crate::family::{Cycle, Lineages};
use crate::lineage::Lineage;
use std::fmt;

/// What somebody is asking to clone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// The table to create.
    pub table: String,
    /// The tenant asking.
    pub tenant: String,
    /// The table to clone.
    pub origin: String,
    /// The tenant that owns the origin.
    pub origin_tenant: String,
    /// The origin version to clone at.
    pub version: u64,
}

/// What the origin's log and catalog say right now.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Origin {
    /// The newest version the origin has.
    pub latest_version: u64,
    /// The oldest version still fully resolvable.
    ///
    /// Below this, files have been retired and the version cannot be reconstructed --- which is
    /// the difference between a clone that reads nothing and a clone that cannot be made.
    pub earliest_retained_version: u64,
    /// Whether a purge is planned or running against it.
    pub purge_in_flight: bool,
    /// Whether its schema is mid-evolution.
    pub schema_evolving: bool,
}

/// Why an operation was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Refused {
    /// The origin has a purge planned or running.
    OriginBeingPurged {
        /// The origin.
        origin: String,
    },
    /// The origin belongs to another tenant.
    AcrossTenants {
        /// Who asked.
        tenant: String,
        /// Who owns the origin.
        origin_tenant: String,
    },
    /// The origin's schema is mid-evolution.
    SchemaEvolving {
        /// The origin.
        origin: String,
    },
    /// The origin never had that version.
    NoSuchVersion {
        /// The origin.
        origin: String,
        /// What was asked for.
        wanted: u64,
        /// The newest it has.
        latest: u64,
    },
    /// The origin had that version and can no longer reconstruct it.
    VersionRetired {
        /// The origin.
        origin: String,
        /// What was asked for.
        wanted: u64,
        /// The oldest still resolvable.
        earliest: u64,
    },
    /// Clones still read this table.
    StillRead {
        /// The table.
        table: String,
        /// What would break.
        by: Vec<String>,
    },
    /// A clone was asked for a moment before it existed.
    BeforeTheClone {
        /// The clone.
        table: String,
        /// What was asked for.
        wanted: i64,
        /// When it was made.
        cloned_at: i64,
        /// Where the real history is.
        origin: String,
    },
    /// The lineage records contradict themselves.
    Tangled {
        /// A table on the cycle.
        at: String,
    },
}

impl From<Cycle> for Refused {
    fn from(cycle: Cycle) -> Self {
        Self::Tangled { at: cycle.at }
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OriginBeingPurged { origin } => write!(
                f,
                "`{origin}` has a purge planned or running, and the files a clone would read \
                 are the ones being detached. Clone it after the purge, or from a version the \
                 purge does not touch"
            ),
            Self::AcrossTenants { tenant, origin_tenant } => write!(
                f,
                "`{tenant}` may not clone a table owned by `{origin_tenant}`. A clone is a \
                 reference rather than a copy, so this would be a way to read another tenant's \
                 bytes without a grant --- which is the whole of the isolation, not a detail of it"
            ),
            Self::SchemaEvolving { origin } => write!(
                f,
                "`{origin}` is mid-schema-evolution. A clone taken now would name files written \
                 under two schemas and belong to neither"
            ),
            Self::NoSuchVersion { origin, wanted, latest } => write!(
                f,
                "`{origin}` has no version {wanted}; its newest is {latest}. A version ahead of \
                 the table is a typo or a plan made against a different warehouse, and neither \
                 becomes true by retrying"
            ),
            Self::VersionRetired { origin, wanted, earliest } => write!(
                f,
                "`{origin}` had version {wanted} and can no longer reconstruct it --- its oldest \
                 resolvable version is {earliest}. There is nothing left for a clone to \
                 reference, and a clone of a version whose files are gone is an empty table \
                 wearing the name of a full one"
            ),
            Self::StillRead { table, by } => write!(
                f,
                "`{table}` is still read by {}. Removing it is the deletion cloning is gated on, \
                 arriving through the front door --- materialise them first, or drop them",
                by.join(", ")
            ),
            Self::BeforeTheClone { table, wanted, cloned_at, origin } => write!(
                f,
                "`{table}` did not exist at {wanted}; it was cloned at {cloned_at}. Answering \
                 from `{origin}`'s history would give this table a past it never had, and \
                 `{origin}` is queryable directly by anybody who wants the real one"
            ),
            Self::Tangled { at } => write!(
                f,
                "the clone lineage forms a cycle at `{at}`, so what reads what is not a question \
                 with an answer"
            ),
        }
    }
}

/// Whether a clone may be created.
///
/// Checks are ordered by what an operator can do about them. Tenancy first, because it is the
/// one that is never a timing problem and never becomes true by waiting.
///
/// # Errors
///
/// [`Refused`] naming which of the five creation-time refusals applies.
pub fn may_clone(request: &Request, origin: &Origin) -> Result<(), Refused> {
    if request.tenant != request.origin_tenant {
        return Err(Refused::AcrossTenants {
            tenant: request.tenant.clone(),
            origin_tenant: request.origin_tenant.clone(),
        });
    }
    if origin.purge_in_flight {
        return Err(Refused::OriginBeingPurged { origin: request.origin.clone() });
    }
    if origin.schema_evolving {
        return Err(Refused::SchemaEvolving { origin: request.origin.clone() });
    }
    if request.version > origin.latest_version {
        return Err(Refused::NoSuchVersion {
            origin: request.origin.clone(),
            wanted: request.version,
            latest: origin.latest_version,
        });
    }
    if request.version < origin.earliest_retained_version {
        return Err(Refused::VersionRetired {
            origin: request.origin.clone(),
            wanted: request.version,
            earliest: origin.earliest_retained_version,
        });
    }
    Ok(())
}

/// Whether a table may be dropped.
///
/// # Errors
///
/// [`Refused::StillRead`] naming every clone that would break, and [`Refused::Tangled`] when the
/// lineage records contradict themselves --- refused rather than guessed, because a drop decided
/// from records that disagree is a drop decided on nonsense.
pub fn may_drop(table: &str, clones: &Lineages) -> Result<(), Refused> {
    let dependents = clones.dependents(table)?;
    if dependents.is_empty() {
        return Ok(());
    }
    Err(Refused::StillRead {
        table: table.to_string(),
        by: dependents.into_iter().collect(),
    })
}

/// Whether a table may be purged.
///
/// Separate from [`may_drop`] and identical in shape, because they are different operations that
/// happen to be refused for the same reason: `M9`'s purge detaches and drops a partition, and a
/// clone naming those files would be reading data the registry says was archived and the source
/// says is gone.
///
/// # Errors
///
/// As [`may_drop`].
pub fn may_purge(table: &str, clones: &Lineages) -> Result<(), Refused> {
    may_drop(table, clones)
}

/// Whether a clone may be read as of a moment.
///
/// # Errors
///
/// [`Refused::BeforeTheClone`] when the moment precedes the clone's creation.
pub fn may_read_as_of(table: &str, at: i64, lineage: &Lineage) -> Result<(), Refused> {
    if at < lineage.cloned_at {
        return Err(Refused::BeforeTheClone {
            table: table.to_string(),
            wanted: at,
            cloned_at: lineage.cloned_at,
            origin: lineage.origin.clone(),
        });
    }
    Ok(())
}
