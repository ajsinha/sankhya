//! Tier selection and its proof.

use sankhya_types::{Lsn, LsnRange};
use std::fmt;

/// A tier offered to the planner.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TierRef {
    /// Identifies the tier in provenance and in diagnostics.
    pub name: &'static str,
    /// The interval this tier is known to contain.
    pub coverage: LsnRange,
}

impl TierRef {
    #[must_use]
    pub const fn new(name: &'static str, coverage: LsnRange) -> Self {
        Self { name, coverage }
    }
}

/// A proven-safe selection of tiers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Splice {
    /// In ascending coverage order.
    pub tiers: Vec<TierRef>,
    /// The position this answer is consistent as of.
    pub target: Lsn,
}

impl Splice {
    /// Provenance for the response: which tiers answered, over which intervals.
    ///
    /// Returned on every result rather than only on request. "Which tier answered me"
    /// is a question an auditor eventually asks, and the system should not have to
    /// guess after the fact.
    #[must_use]
    pub fn provenance(&self) -> Vec<(&'static str, LsnRange)> {
        self.tiers.iter().map(|t| (t.name, t.coverage)).collect()
    }

    #[must_use]
    pub fn tier_names(&self) -> Vec<&'static str> {
        self.tiers.iter().map(|t| t.name).collect()
    }
}

/// Why a query cannot be answered safely.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SpliceError {
    /// Part of the requested span is held by no tier.
    ///
    /// Fatal rather than a warning. The alternative — returning what is available — is
    /// a silently short answer, which is worse than no answer because nothing marks it.
    CoverageGap { from: Lsn, to: Lsn },
    /// No tier reaches the requested position.
    BeyondFrontier { requested: Lsn, available: Lsn },
    /// Two selected tiers claim the same positions.
    ///
    /// Indicates a defect in whatever produced the coverage metadata; splicing anyway
    /// would double-count.
    Overlap { left: &'static str, right: &'static str },
}

impl fmt::Display for SpliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoverageGap { from, to } => write!(
                f,
                "no tier covers ({from}, {to}]; the query was refused rather than \
                 answered partially"
            ),
            Self::BeyondFrontier { requested, available } => write!(
                f,
                "requested position {requested} exceeds the frontier {available}; \
                 capture has not yet reached it"
            ),
            Self::Overlap { left, right } => write!(
                f,
                "tiers {left} and {right} both claim the same positions, which would \
                 double-count; this is a defect in their coverage metadata"
            ),
        }
    }
}

impl std::error::Error for SpliceError {}

/// Select a set of tiers that provably covers `[0, target]` exactly once.
///
/// Tiers may be supplied in any order and may overlap; the planner chooses a subset
/// that abuts exactly. It is greedy from the origin: at each step it takes the tier
/// starting where the last one ended and reaching furthest, which for interval cover
/// is optimal in the number of tiers used.
///
/// # Errors
///
/// - [`SpliceError::CoverageGap`] when part of the span is held by nothing.
/// - [`SpliceError::BeyondFrontier`] when no tier reaches `target`.
///
/// Both refuse the query rather than answering partially.
pub fn plan_splice(tiers: &[TierRef], target: Lsn) -> Result<Splice, SpliceError> {
    if target == Lsn::ZERO {
        return Ok(Splice { tiers: Vec::new(), target });
    }

    let frontier = tiers
        .iter()
        .map(|t| t.coverage.end_inclusive())
        .max()
        .unwrap_or(Lsn::ZERO);
    if frontier < target {
        return Err(SpliceError::BeyondFrontier { requested: target, available: frontier });
    }

    let mut chosen: Vec<TierRef> = Vec::new();
    let mut covered_through = Lsn::ZERO;

    while covered_through < target {
        // Any tier that starts at or before the current frontier extends it; take the
        // one reaching furthest.
        let next = tiers
            .iter()
            .filter(|t| {
                t.coverage.start_exclusive() <= covered_through
                    && t.coverage.end_inclusive() > covered_through
            })
            .max_by_key(|t| t.coverage.end_inclusive());

        let Some(next) = next else {
            // Report the gap precisely: an operator needs to know which span is
            // missing, not merely that something is.
            let resumes_at = tiers
                .iter()
                .map(|t| t.coverage.start_exclusive())
                .filter(|start| *start > covered_through)
                .min()
                .unwrap_or(target);
            return Err(SpliceError::CoverageGap {
                from: covered_through,
                to: resumes_at.min(target),
            });
        };

        // Trim the chosen tier so it abuts exactly. This is what makes the result
        // non-overlapping by construction rather than by hope.
        let trimmed = LsnRange::new(covered_through, next.coverage.end_inclusive().min(target))
            .unwrap_or_else(|| LsnRange::up_to(covered_through));

        covered_through = trimmed.end_inclusive();
        chosen.push(TierRef { name: next.name, coverage: trimmed });
    }

    debug_assert!(is_exact_cover(&chosen, target), "the planner produced an unsound cover");
    Ok(Splice { tiers: chosen, target })
}

/// Whether a selection covers `[0, target]` contiguously and without overlap.
///
/// Exposed so callers and tests can assert the property directly rather than trusting
/// that the planner maintained it.
#[must_use]
pub fn is_exact_cover(tiers: &[TierRef], target: Lsn) -> bool {
    if target == Lsn::ZERO {
        return tiers.is_empty();
    }
    let mut position = Lsn::ZERO;
    for tier in tiers {
        if tier.coverage.start_exclusive() != position {
            return false;
        }
        position = tier.coverage.end_inclusive();
    }
    position == target
}
