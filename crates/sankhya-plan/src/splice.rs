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
/// A third variant, `Overlap`, was declared here and constructed by nothing.
///
/// It described two selected tiers claiming the same positions --- and the planner cannot
/// produce that, deliberately: it trims each chosen tier so it abuts exactly, under a comment
/// reading *"this is what makes the result non-overlapping **by construction** rather than by
/// hope"*. So the variant documented a refusal the algorithm had already made unnecessary,
/// carried a mapping onto `SNK-S0005`, and had a test built on a value only the test could
/// construct. It is deleted rather than left, for the same reason
/// `Unservable::NotReconciled` stopped being unconstructible: a declared case nothing can
/// reach is a promise nothing keeps.
pub enum SpliceError {
    /// Part of the requested span is held by no tier.
    ///
    /// Fatal rather than a warning. The alternative — returning what is available — is
    /// a silently short answer, which is worse than no answer because nothing marks it.
    CoverageGap { from: Lsn, to: Lsn },
    /// No tier reaches the requested position.
    BeyondFrontier { requested: Lsn, available: Lsn },
}

impl fmt::Display for SpliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoverageGap { from, to } => write!(
                f,
                "no tier covers ({from}, {to}]; the query was refused rather than \
                 answered partially"
            ),
            Self::BeyondFrontier {
                requested,
                available,
            } => write!(
                f,
                "requested position {requested} exceeds the frontier {available}; \
                 capture has not yet reached it"
            ),
        }
    }
}

impl std::error::Error for SpliceError {}

/// The catalogue code each refusal is, so a caller can report one.
///
/// # Why this exists
///
/// Because without it `SNK-S0001` was published as a code this build produces and no code
/// path could produce it. The condition it names --- part of the requested span held by no
/// tier --- **is** detected, right here, and was reported as this crate's own type; nothing
/// converted it, so an alert rule written on `SNK-S0001` was permanently silent while the
/// exact condition occurred. A catalogue entry whose construction site belongs to a different
/// type in a different crate is a documented promise nothing keeps.
///
/// The detail carries the positions rather than a summary, because the remediation says to
/// *"investigate capture continuity and retention"* and neither question can be asked without
/// knowing which range went missing.
///
/// This closes the mapping. It does not by itself make the code reachable from a query: no
/// read path in the server composes a tier splice yet, which is the other half and is tracked
/// as such in `xtask/src/catalogues.rs`.
/// Written as `sankhya_error::Error::...` rather than `Self::...` on purpose: the gate that
/// decides whether a catalogue code is producible greps for a construction site, and `Self`
/// inside this `impl` is one it cannot see. A code that is reachable and reads as
/// unreachable is the same documented lie in the other direction.
impl From<SpliceError> for sankhya_error::Error {
    fn from(error: SpliceError) -> Self {
        match error {
            SpliceError::CoverageGap { from, to } => {
                sankhya_error::Error::CoverageGap(format!("no tier covers ({from}, {to}]"))
            }
            // Not a coverage gap: nothing is missing from the middle. The caller asked for
            // a position past everything on offer.
            //
            // This mapped to `SNK-T0003` (retryable, 500 ms) on the reading that capture
            // had not caught up. That reading is one of two, and the splice cannot tell
            // them apart --- the other is a caller naming a position that will **never**
            // exist, and telling that caller the server is unavailable and to retry in half
            // a second is a permanent retry loop, amplified by any middleware that honours
            // a 503. The code's own message did not fit either: it says the requested
            // *freshness* could not be met within a *deadline*, and this function consults
            // neither.
            //
            // So it is a user error, and the detail carries the frontier: a caller who was
            // merely early can see how far the data reaches and decide to wait, which is a
            // decision they are better placed to make than a retry policy is.
            SpliceError::BeyondFrontier {
                requested,
                available,
            } => sankhya_error::Error::StatementFailed(format!(
                "position {requested} is beyond the frontier {available}, which is the \
                 furthest any tier on offer reaches"
            )),
        }
    }
}


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
        return Ok(Splice {
            tiers: Vec::new(),
            target,
        });
    }

    let frontier = tiers
        .iter()
        .map(|t| t.coverage.end_inclusive())
        .max()
        .unwrap_or(Lsn::ZERO);
    if frontier < target {
        return Err(SpliceError::BeyondFrontier {
            requested: target,
            available: frontier,
        });
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
        chosen.push(TierRef {
            name: next.name,
            coverage: trimmed,
        });
    }

    debug_assert!(
        is_exact_cover(&chosen, target),
        "the planner produced an unsound cover"
    );
    Ok(Splice {
        tiers: chosen,
        target,
    })
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
