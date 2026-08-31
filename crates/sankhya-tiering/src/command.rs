//! Planning a purge, and what a non-interactive invocation has to bring with it.
//!
//! # Planning is always a dry run, structurally
//!
//! `FR-TIER-26`: the planning command *"SHALL always be a dry run"*, report what moves, what
//! remains, **every** failing precondition rather than the first, and emit an expiring plan
//! digest.
//!
//! "Always" is the word doing the work, and a `--dry-run` flag defaulting to true is not it: a
//! flag that defaults safely is one argument away from not. So [`propose`] returns a [`Proposal`]
//! and a `Proposal` has no method that does anything. The path that acts starts from
//! [`clear`], which cannot be reached without a digest, and the digest cannot be produced
//! except by planning.
//!
//! # What the digest is binding, and why it is not a nonce
//!
//! `FR-TIER-27` requires a non-interactive invocation to carry a cluster assertion matching the
//! server's declared identity, a **valid unexpired plan digest bound to the exact ranges**, and
//! a change-management reference.
//!
//! A digest that were merely a random token would say *"somebody planned something recently"*.
//! This one is taken over the cluster, the policy, the table and every range, so it says
//! *"somebody planned **this**"* --- and a plan approved for one range cannot authorise another
//! by editing the command line, because the digest would no longer match. That is the failure
//! being designed against: the approval is read by a person and the arguments are typed by a
//! machine, and between those two the ranges are where a mistake hides.
//!
//! # Why it expires
//!
//! A plan is a statement about a table's contents at a moment. Rows arrive; a boundary that was
//! outside the retention basis this morning is inside it tonight. An approval with no expiry is
//! an approval of whatever the table happens to hold when somebody gets round to running it,
//! which is not what the approver read.

use crate::policy::Ineligible;
use crate::registry::Range;
use crate::verify::Hash;
use sha2::{Digest as _, Sha256};
use std::fmt;

/// Microseconds in an hour.
const HOUR: i64 = 3_600 * 1_000_000;

/// How long a plan digest stays valid.
///
/// Long enough for a person to read a plan, ask a colleague and run it; short enough that it
/// cannot be run tomorrow against a table that has moved on.
pub const VALID_FOR_HOURS: i64 = 4;

/// What a purge would do, and what it would leave.
///
/// Produced by [`propose`], which is the only way to obtain one. It carries no method that acts,
/// which is `FR-TIER-26`'s *"always a dry run"* as a property of the type rather than of a
/// default.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Proposal {
    /// The cluster this was planned against.
    pub cluster: String,
    /// The policy.
    pub policy: String,
    /// The table.
    pub table: String,
    /// What would leave the system of record, in key order.
    pub moves: Vec<Range>,
    /// What would remain hot.
    pub remains: Vec<Range>,
    /// Every failing precondition, never the first.
    pub refusals: Vec<Ineligible>,
    /// When it was planned, in microseconds from the epoch.
    pub at: i64,
}

impl Proposal {
    /// Whether anything stands in the way.
    #[must_use]
    pub fn is_runnable(&self) -> bool {
        self.refusals.is_empty() && !self.moves.is_empty()
    }

    /// When the digest stops being valid.
    #[must_use]
    pub fn expires_at(&self) -> i64 {
        self.at.saturating_add(VALID_FOR_HOURS.saturating_mul(HOUR))
    }

    /// The digest an approver signs off and an invocation must carry.
    ///
    /// Taken over the cluster, the policy, the table, every range **in order**, and the expiry.
    /// Nothing else: two plans that would move the same rows on the same cluster are the same
    /// plan, and making the digest depend on the wall clock would mean an approval could never
    /// be quoted back.
    #[must_use]
    pub fn digest(&self) -> Hash {
        let mut hasher = Sha256::new();
        for part in [self.cluster.as_str(), self.policy.as_str(), self.table.as_str()] {
            hasher.update((part.len() as u64).to_be_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.update((self.moves.len() as u64).to_be_bytes());
        for range in &self.moves {
            hasher.update(range.from.to_be_bytes());
            hasher.update(range.until.to_be_bytes());
        }
        hasher.update(self.expires_at().to_be_bytes());
        Hash::from_bytes(hasher.finalize().into())
    }
}

impl fmt::Display for Proposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "plan {} for `{}` on {}: {} range(s) move, {} remain",
            self.digest(),
            self.table,
            self.cluster,
            self.moves.len(),
            self.remains.len()
        )?;
        for refusal in &self.refusals {
            write!(f, "\n  refused: {refusal}")?;
        }
        Ok(())
    }
}

/// Plan a purge. Does nothing else, and can do nothing else.
///
/// `refusals` comes from [`crate::policy::Policy::eligible`], which already reports every reason
/// rather than the first; it is carried through rather than re-derived so the two cannot
/// disagree about what makes a table ineligible.
#[must_use]
pub fn propose(
    cluster: impl Into<String>,
    policy: impl Into<String>,
    table: impl Into<String>,
    moves: Vec<Range>,
    remains: Vec<Range>,
    refusals: Vec<Ineligible>,
    at: i64,
) -> Proposal {
    Proposal {
        cluster: cluster.into(),
        policy: policy.into(),
        table: table.into(),
        moves,
        remains,
        refusals,
        at,
    }
}

/// What a non-interactive caller presents.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Invocation {
    /// The cluster the caller believes it is talking to.
    pub cluster_asserted: String,
    /// The digest the caller was given, and an approver saw.
    pub digest: Hash,
    /// The exact ranges the caller intends to move.
    pub ranges: Vec<Range>,
    /// The change-management reference.
    pub change_reference: String,
}

/// Why an invocation was rejected.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Rejected {
    /// The caller believes it is talking to a different cluster.
    ///
    /// The failure this catches is a runbook copied from staging, which is how the right
    /// command gets run against the wrong database.
    WrongCluster {
        /// What the caller asserted.
        asserted: String,
        /// What this server is.
        actual: String,
    },
    /// The digest does not match the plan for these ranges.
    ///
    /// Either the plan is not the one that was approved, or the ranges have been edited since.
    /// The two are indistinguishable from here and the answer to both is the same.
    DigestMismatch,
    /// The digest was valid and no longer is.
    Expired {
        /// When it stopped being valid.
        expired_at: i64,
        /// When the invocation arrived.
        now: i64,
    },
    /// No change-management reference was given.
    NoChangeReference,
    /// The plan itself reported failing preconditions.
    ///
    /// Carried whole rather than counted: an operator who has to fix them needs them all, which
    /// is the same argument `FR-TIER-26` makes about the planning command.
    PreconditionsFailed {
        /// Every one.
        refusals: Vec<Ineligible>,
    },
    /// The plan moves nothing.
    NothingToDo,
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongCluster { asserted, actual } => write!(
                f,
                "this invocation asserts the cluster `{asserted}` and this server is \
                 `{actual}`. A runbook copied from another environment is how the right \
                 command is run against the wrong database"
            ),
            Self::DigestMismatch => f.write_str(
                "the plan digest does not match a plan for these exact ranges. Either this is \
                 not the plan that was approved, or the ranges have been edited since --- and \
                 the answer to both is to plan again and have it read",
            ),
            Self::Expired { expired_at, now } => write!(
                f,
                "the plan digest expired at {expired_at} and it is now {now}. A plan is a \
                 statement about a table's contents at a moment, and an approval with no expiry \
                 approves whatever the table holds when somebody gets round to it"
            ),
            Self::NoChangeReference => f.write_str(
                "no change-management reference was given. A purge that cannot be traced to a \
                 change record is a purge nobody can account for afterwards",
            ),
            Self::PreconditionsFailed { refusals } => {
                write!(f, "{} failing precondition(s):", refusals.len())?;
                for refusal in refusals {
                    write!(f, "\n  {refusal}")?;
                }
                Ok(())
            }
            Self::NothingToDo => f.write_str("the plan moves no ranges"),
        }
    }
}

/// Evidence that an invocation may proceed.
///
/// Not `Clone` or `Copy`: it is evidence about one invocation of one plan, and a cleared
/// invocation kept and presented again is the replay the expiry exists to bound.
#[derive(Debug)]
pub struct Cleared {
    proposal: Proposal,
    change_reference: String,
}

impl Cleared {
    /// The plan that was cleared.
    #[must_use]
    pub const fn proposal(&self) -> &Proposal {
        &self.proposal
    }

    /// The change record it is accountable to.
    #[must_use]
    pub fn change_reference(&self) -> &str {
        &self.change_reference
    }
}

/// Check a non-interactive invocation against a plan.
///
/// This returns the first failure rather than all of them, which is the opposite of what the
/// planning command does and is deliberate. A plan's preconditions are things an operator fixes
/// together, so they are reported together. These are answers to *"should this run at all"*,
/// and the first `no` settles it --- there is nothing to fix in a runbook that names the wrong
/// cluster. The exception is [`Rejected::PreconditionsFailed`], which carries every refusal,
/// because those *are* the ones somebody has to go and fix.
///
/// # Errors
///
/// [`Rejected`] naming which requirement of `FR-TIER-27` was not met.
pub fn clear(
    proposal: &Proposal,
    this_cluster: &str,
    invocation: &Invocation,
    now: i64,
) -> Result<Cleared, Rejected> {
    if invocation.cluster_asserted != this_cluster {
        return Err(Rejected::WrongCluster {
            asserted: invocation.cluster_asserted.clone(),
            actual: this_cluster.to_string(),
        });
    }
    if invocation.change_reference.trim().is_empty() {
        return Err(Rejected::NoChangeReference);
    }
    if !proposal.refusals.is_empty() {
        return Err(Rejected::PreconditionsFailed { refusals: proposal.refusals.clone() });
    }
    if proposal.moves.is_empty() {
        return Err(Rejected::NothingToDo);
    }
    if now >= proposal.expires_at() {
        return Err(Rejected::Expired { expired_at: proposal.expires_at(), now });
    }
    // The ranges are compared through the digest rather than beside it, so there is one
    // definition of "the same plan" instead of two that can drift.
    if invocation.ranges != proposal.moves || invocation.digest != proposal.digest() {
        return Err(Rejected::DigestMismatch);
    }

    Ok(Cleared {
        proposal: proposal.clone(),
        change_reference: invocation.change_reference.clone(),
    })
}
