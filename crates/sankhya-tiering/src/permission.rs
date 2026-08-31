//! Seven permissions, and the pairs one principal may not hold at once.
//!
//! # What separation of duty is actually preventing
//!
//! `FR-TIER-33`: defining, approving, executing, purging, dropping, rehydrating and retiring are
//! **distinct permissions**, and *"the principal who defines a policy SHALL NOT be a principal
//! who approves it or who executes a purge under it"*.
//!
//! The failure is not somebody malicious. It is somebody competent, working alone, at the end of
//! a long day, who writes a policy with a boundary a day out and then approves their own work
//! because they are the person who understands it. Every step is reasonable and the review that
//! was supposed to happen did not, because it was the same person twice.
//!
//! So the conflict is between **a policy's definer and its approver or executor**, and it is
//! checked against the policy rather than in the abstract: holding both permissions is normal in
//! a small team, and using both on the *same policy* is what is refused.
//!
//! # Why this is not the authorization type
//!
//! [`crate::authorize::Authorization`] answers *"did a person or a schedule start this?"*, which
//! is a question about the path. This answers *"may this person do this to this policy?"*, which
//! is a question about the people. They are separate because a schedule with a valid definer and
//! approver still has a service principal executing it, and folding them together would make the
//! service principal look like an approver.

use std::collections::BTreeSet;
use std::fmt;

/// What a principal may do.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Permission {
    /// Write a tiering policy.
    Define,
    /// Approve one, or a schedule's plan digest.
    Approve,
    /// Run a plan or an archive.
    Execute,
    /// Detach from the system of record.
    Purge,
    /// Drop a quarantined partition.
    Drop,
    /// Load an archived range back for reading.
    Rehydrate,
    /// Migrate a table whole and mark it cold.
    Retire,
}

impl Permission {
    /// Every permission, so a test can walk them rather than list them.
    pub const ALL: [Self; 7] = [
        Self::Define,
        Self::Approve,
        Self::Execute,
        Self::Purge,
        Self::Drop,
        Self::Rehydrate,
        Self::Retire,
    ];

    /// Its name, as a grant names it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Define => "define",
            Self::Approve => "approve",
            Self::Execute => "execute",
            Self::Purge => "purge",
            Self::Drop => "drop",
            Self::Rehydrate => "rehydrate",
            Self::Retire => "retire",
        }
    }

    /// Whether holding this on a policy conflicts with having defined it.
    ///
    /// `FR-TIER-33` names approval and purge execution. `Execute` is included because the
    /// distinction between *executing a plan* and *executing a purge* is one of arguments rather
    /// than of permissions, and a rule that depended on reading the arguments would be a rule
    /// somebody has to apply correctly at each call site.
    #[must_use]
    pub const fn conflicts_with_defining(&self) -> bool {
        matches!(self, Self::Approve | Self::Execute | Self::Purge)
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Who wrote a policy, so a later act on it can be checked against them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Policy {
    /// The policy's name.
    pub name: String,
    /// The principal who defined it.
    pub definer: String,
}

/// What a principal holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Principal {
    /// Who they are.
    pub name: String,
    /// What they may do.
    pub permissions: BTreeSet<Permission>,
}

impl Principal {
    /// A principal holding these permissions.
    pub fn holding(name: impl Into<String>, permissions: &[Permission]) -> Self {
        Self { name: name.into(), permissions: permissions.iter().copied().collect() }
    }
}

/// Why an act was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Denied {
    /// The principal does not hold the permission at all.
    NotHeld {
        /// Who.
        principal: String,
        /// What they tried to do.
        permission: Permission,
    },
    /// The principal defined this policy and may not also do this to it.
    OwnWork {
        /// Who.
        principal: String,
        /// What they tried to do.
        permission: Permission,
        /// Which policy.
        policy: String,
    },
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHeld { principal, permission } => {
                write!(f, "`{principal}` does not hold the `{permission}` permission")
            }
            Self::OwnWork { principal, permission, policy } => write!(
                f,
                "`{principal}` defined the policy `{policy}` and may not also `{permission}` \
                 under it. The review this exists for is not a review when it is the same \
                 person twice --- and the person it catches is not a malicious one, it is a \
                 competent one working alone at the end of a long day"
            ),
        }
    }
}

/// Whether `principal` may do `permission` to `policy`.
///
/// # Errors
///
/// [`Denied::NotHeld`] when the permission is not granted, and [`Denied::OwnWork`] when the
/// principal defined the policy and the permission conflicts with having defined it.
pub fn may(
    principal: &Principal,
    permission: Permission,
    policy: &Policy,
) -> Result<(), Denied> {
    if !principal.permissions.contains(&permission) {
        return Err(Denied::NotHeld { principal: principal.name.clone(), permission });
    }
    if principal.name == policy.definer && permission.conflicts_with_defining() {
        return Err(Denied::OwnWork {
            principal: principal.name.clone(),
            permission,
            policy: policy.name.clone(),
        });
    }
    Ok(())
}
