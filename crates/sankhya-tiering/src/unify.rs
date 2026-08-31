//! Serving a query whose predicate spans both tiers, and the tie-break rule that makes it total.
//!
//! # Why this is automatic rather than opt-in
//!
//! `DEC-25`: a user who queries six years of history and silently receives two has been handed a
//! wrong answer by a system that knew better. Anything other than unioning the tiers is a
//! footgun, so the union is what a predicate spanning them produces.
//!
//! # The symmetry with the read path, which is why this is not a new mechanism
//!
//! The planner already splices along the **freshness axis** --- contiguous, non-overlapping
//! intervals covering `[0, target]`. Tiering adds a second, orthogonal **key-range axis** with
//! the same shape and a different authority:
//!
//! | Axis | Coverage rule | Authority |
//! |---|---|---|
//! | Freshness | intervals covering `[0, target]` | commit metadata and buffer epochs |
//! | Key range | hot and cold extents disjoint, together covering the predicate | the source catalog (hot), the archival registry (cold) |
//!
//! # The tie-break rule, and what "total" is doing in that sentence
//!
//! Every point of the predicate falls into exactly one of four cases, and each has an answer
//! decided here rather than at the point of surprise:
//!
//! | Catalog | Registry | Answer |
//! |---|---|---|
//! | attached | silent | read hot |
//! | detached | covers it | read cold |
//! | attached | covers it | **read hot, exactly once**, and flag the inconsistency separately |
//! | detached | silent | **fail** with a coverage gap |
//!
//! The third case is a restored backup resurrecting purged rows. Hot wins and the range is read
//! *once*, so the failure case does not become double-counting on top of an inconsistency ---
//! and the inconsistency is reported rather than absorbed, because a query that quietly papers
//! over it is a query that stops anybody finding out.
//!
//! The fourth is the one worth being loud about: **never return a silently short answer.** A
//! coverage gap means the data is in neither tier, which is either a defect or a purge that
//! lost its registry entry, and both are things somebody must be told.
//!
//! # One snapshot, and why it costs nothing
//!
//! `FR-TIER-16` requires the hot extent and the cold extent to be read **within one source
//! snapshot**. The registry lives in the source, so this is one snapshot rather than an
//! agreement protocol --- and without it a range can move from hot to cold between the two
//! reads, which produces a plan that reads it twice or not at all depending on which way it
//! moved.
//!
//! # What is not being traded away
//!
//! Serving the cold portion from the published tier does not weaken a strongly-consistent read.
//! Archived data is immutable by policy, so nothing can change it, so a snapshot read of it is
//! equivalent to a linearizable one. The change is of storage, not of consistency --- recorded
//! here because a reviewer will otherwise assume the opposite.

use crate::registry::{Conflict, Range, Registry, Servable};
use std::fmt;

/// Where a segment of the predicate is read from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Read {
    /// The system of record.
    Source,
    /// An archive, named so a plan can be explained.
    Archive {
        /// Where it is.
        archive: String,
        /// The range the archive covers, which may be wider than the segment.
        extent: Range,
    },
}

impl fmt::Display for Read {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source => f.write_str("source"),
            Self::Archive { archive, .. } => write!(f, "archive {archive}"),
        }
    }
}

/// One contiguous piece of the predicate and where it comes from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Segment {
    /// The piece.
    pub range: Range,
    /// Where it is read.
    pub read: Read,
}

/// How a predicate is served.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    /// The table.
    pub table: String,
    /// The source snapshot the hot extent and the registry were both read in.
    ///
    /// Carried so a plan can say which snapshot it is a plan *for*. Two reads at two snapshots
    /// produce a plan that reads a moving range twice or not at all.
    pub snapshot: String,
    /// The pieces, in key order, disjoint, together covering the predicate exactly.
    pub segments: Vec<Segment>,
    /// Ranges the catalog and the registry disagree about, served hot and reported.
    pub inconsistencies: Vec<Conflict>,
    /// Whether the predicate covers the whole declared key domain.
    ///
    /// `DEC-25`'s operator-visible consequence: a query with no predicate on the tiering key
    /// scans both tiers in full. That is the tiering equivalent of a missing partition filter,
    /// and it should surface as a warning long before it surfaces as a forty-minute query.
    pub whole_domain: bool,
}

impl Plan {
    /// The segments read from an archive.
    #[must_use]
    pub fn cold(&self) -> Vec<&Segment> {
        self.segments
            .iter()
            .filter(|segment| matches!(segment.read, Read::Archive { .. }))
            .collect()
    }

    /// Whether any part of the predicate is served from an archive.
    #[must_use]
    pub fn spans_tiers(&self) -> bool {
        self.segments.iter().any(|segment| matches!(segment.read, Read::Archive { .. }))
            && self.segments.iter().any(|segment| segment.read == Read::Source)
    }
}

/// Why a predicate cannot be served.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unservable {
    /// Part of the predicate is in neither tier.
    CoverageGap {
        /// The table.
        table: String,
        /// Every uncovered sub-range, not the first.
        gaps: Vec<Range>,
    },
    /// Reconciliation found the tiers disagreeing about this table.
    ///
    /// `FR-TIER-23`. Not the same as [`Self::CoverageGap`]: a gap is a question about the data,
    /// and this is a statement that the two authorities cannot both be believed, which makes
    /// every answer about the table suspect rather than one range of it.
    NotReconciled {
        /// The table.
        table: String,
    },
}

impl fmt::Display for Unservable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoverageGap { table, gaps } => {
                write!(f, "`{table}` has no tier covering ")?;
                for (index, gap) in gaps.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{gap}")?;
                }
                f.write_str(
                    ". The rows are in neither the source nor an archive, which is a defect or \
                     a purge that lost its registry entry --- and answering without them would \
                     be a silently short answer",
                )
            }
            Self::NotReconciled { table } => write!(
                f,
                "`{table}` has not been reconciled, or its reconciliation found the catalog and \
                 the registry disagreeing. Serving a unified query would be serving a plausible \
                 wrong answer"
            ),
        }
    }
}

/// Plan a predicate across both tiers.
///
/// `attached` is what the catalog says is still hot, `registry` is authority for the cold side,
/// and both must have been read in `snapshot`. `servable` is the witness `FR-TIER-23` requires:
/// a planner cannot reach this function without a reconciliation that had nothing to say about
/// the table, which is why an unreconciled table cannot be served by forgetting to check.
///
/// # Errors
///
/// [`Unservable::NotReconciled`] when the witness is for another table, and
/// [`Unservable::CoverageGap`] listing **every** uncovered sub-range.
pub fn plan(
    predicate: Range,
    attached: &[Range],
    registry: &Registry,
    servable: &Servable,
    snapshot: &str,
    domain: Range,
) -> Result<Plan, Unservable> {
    let table = servable.table().to_string();

    // Every boundary either side of the predicate, so each elementary interval between two
    // consecutive boundaries is wholly inside or wholly outside every extent. Sweeping
    // boundaries rather than points is what makes the rule total without being quadratic.
    let mut edges = vec![predicate.from, predicate.until];
    for hot in attached {
        edges.push(hot.from);
        edges.push(hot.until);
    }
    for entry in registry.entries().iter().filter(|entry| entry.table == table) {
        edges.push(entry.range.from);
        edges.push(entry.range.until);
    }
    edges.retain(|edge| *edge >= predicate.from && *edge <= predicate.until);
    edges.sort_unstable();
    edges.dedup();

    let mut segments: Vec<Segment> = Vec::new();
    let mut inconsistencies = Vec::new();
    let mut gaps: Vec<Range> = Vec::new();

    for pair in edges.windows(2) {
        let [from, until] = pair else { continue };
        let piece = Range::new(*from, *until);
        if piece.is_empty() {
            continue;
        }
        let midpoint = *from;
        let hot = attached.iter().any(|range| range.contains(midpoint));
        let cold = registry
            .entries()
            .iter()
            .find(|entry| entry.table == table && entry.range.contains(midpoint));

        let read = match (hot, cold) {
            (true, Some(entry)) => {
                // Hot wins, and the range is read exactly once, so a restored backup
                // resurrecting purged rows does not become double-counting on top of an
                // inconsistency. The inconsistency is reported rather than absorbed.
                inconsistencies.push(Conflict::InBothTiers {
                    table: table.clone(),
                    cold: entry.range,
                    hot: piece,
                });
                Read::Source
            }
            (true, None) => Read::Source,
            (false, Some(entry)) => {
                Read::Archive { archive: entry.archive.clone(), extent: entry.range }
            }
            (false, None) => {
                gaps.push(piece);
                continue;
            }
        };

        match segments.last_mut() {
            Some(last) if last.read == read && last.range.until == piece.from => {
                last.range.until = piece.until;
            }
            _ => segments.push(Segment { range: piece, read }),
        }
    }

    if !gaps.is_empty() {
        // Every gap, not the first: a predicate with three holes should be reported once.
        return Err(Unservable::CoverageGap { table, gaps });
    }

    Ok(Plan {
        table,
        snapshot: snapshot.to_string(),
        segments,
        inconsistencies,
        whole_domain: predicate.from <= domain.from && predicate.until >= domain.until,
    })
}

/// Why a mutation was refused.
///
/// `FR-TIER-18` is emphatic about the alternative: **reporting zero rows affected is a silent
/// wrong answer.** The rows exist, they are simply somewhere this statement cannot reach, and a
/// count of zero says the opposite --- so this is a typed refusal naming the archive and what to
/// do instead.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Immutable {
    /// The table.
    pub table: String,
    /// Where the rows are now.
    pub archive: String,
    /// The archived range the statement reached into.
    pub extent: Range,
}

impl fmt::Display for Immutable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rows of `{}` in {} were archived to {} and are immutable there. Record a \
             compensating entry in the hot tier referencing the original, or take the \
             controlled-rewrite path, which retains the prior version and records an amendment \
             link. Reporting no rows affected would say these rows do not exist",
            self.table, self.extent, self.archive
        )
    }
}

/// Whether a statement may modify rows at `at`.
///
/// # Errors
///
/// [`Immutable`] when the key falls inside an archived range.
pub fn mutable(registry: &Registry, table: &str, at: i64) -> Result<(), Immutable> {
    match registry
        .entries()
        .iter()
        .find(|entry| entry.table == table && entry.range.contains(at))
    {
        Some(entry) => Err(Immutable {
            table: table.to_string(),
            archive: entry.archive.clone(),
            extent: entry.range,
        }),
        None => Ok(()),
    }
}
