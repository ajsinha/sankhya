//! The proof that an authorization decision was made, and what it decided.
//!
//! # The guarantee this type provides
//!
//! A [`Guard`] cannot be constructed except from a [`Decision::Allowed`]. It has no public
//! constructor, no public fields, no `Default`, and no `From<&str>`. The only way to obtain
//! one is [`Guard::from_decision`], which returns `None` for a denial.
//!
//! Anything requiring a `Guard` in its signature therefore **cannot be called** unless a
//! policy decision has already permitted the call. This is `9.2`'s type-level guarantee:
//! not a check that a reviewer must remember to look for, but a thing the compiler will not
//! let you skip.
//!
//! The distinction matters because the usual failure is not a wrong policy, it is a code
//! path that never consulted one. A new provider, a new cache, a new maintenance job, a
//! debugging endpoint --- each is a place where someone reasonably forgets, and no amount of
//! care in the policy component helps if the policy component was never called.
//!
//! # Why it carries the decision rather than just proving one happened
//!
//! A bare proof token would let the row filter and column masks be passed separately, and
//! separately means they can be dropped separately. Carrying them inside the proof means
//! the thing that establishes permission is the same thing that carries the restrictions,
//! so there is no way to hold the first without the second.

use sankhya_authz::policy::{Action, Decision, Mask, PolicySet, TableRef};
use sankhya_authz::principal::{Principal, TenantId};
use std::collections::BTreeMap;

/// Proof that a policy decision permitted this access, and what it required.
///
/// Deliberately not `Clone`-restricted --- copying a guard copies a decision that was
/// genuinely made, which is fine. What is impossible is *fabricating* one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Guard {
    tenant: TenantId,
    subject: String,
    table: TableRef,
    action: Action,
    row_filter: Option<String>,
    column_masks: BTreeMap<String, Mask>,
}

impl Guard {
    /// The only way to obtain a guard.
    ///
    /// Returns `None` for a denial, so a caller cannot proceed by ignoring an error the way
    /// a `Result` invites. There is no variant of this that yields a guard on refusal.
    #[must_use]
    pub fn from_decision(
        principal: &Principal,
        table: &TableRef,
        action: Action,
        decision: &Decision,
    ) -> Option<Self> {
        let Decision::Allowed {
            row_filter,
            column_masks,
        } = decision
        else {
            return None;
        };
        Some(Self {
            tenant: principal.tenant().clone(),
            subject: principal.subject().to_string(),
            table: table.clone(),
            action,
            row_filter: row_filter.clone(),
            column_masks: column_masks.clone(),
        })
    }

    /// Decide and, if permitted, produce the guard --- in one step.
    ///
    /// The form every caller should use. Splitting the decision from the guard leaves a
    /// moment where a decision exists and has not been acted on, and someone eventually
    /// writes the branch that acts on it wrongly.
    #[must_use]
    pub fn authorize(
        policy: &PolicySet,
        principal: &Principal,
        table: &TableRef,
        action: Action,
    ) -> Option<Self> {
        let decision = policy.decide(principal, table, action);
        Self::from_decision(principal, table, action, &decision)
    }

    /// What this guard *permits*, as one value.
    ///
    /// # Why this exists, and why the subject is not in it
    ///
    /// An aggregate computed over the rows one principal may read is not an answer for
    /// another principal, so anything caching aggregates must key them by the scope they were
    /// computed under --- see [ADR-0008](../../../docs/adr/0008-serving-cubes-under-policy.md).
    /// The obvious key is the principal, and it is the wrong one: it makes a cache with one
    /// entry per user, which for a deployment of a thousand analysts across six roles is a
    /// thousand copies of six answers.
    ///
    /// So the digest covers the tenant, the row filter and the column masks --- everything
    /// that decides *what is visible* --- and deliberately excludes the subject, which decides
    /// only *who is looking*. Two principals with identical entitlements digest the same and
    /// share the work.
    ///
    /// The safety property runs the other way and matters more: **any difference in what is
    /// visible must change this value.** A field that affects visibility and is left out here
    /// would let one principal be served another's aggregate, which is a disclosure with no
    /// trace in the result. A field added to `Guard` must be added here or deliberately
    /// excluded with a reason.
    #[must_use]
    pub fn scope_digest(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Not `subject`: who is asking does not change what may be seen.
        self.tenant.hash(&mut hasher);
        self.table.hash(&mut hasher);
        self.action.hash(&mut hasher);
        self.row_filter.hash(&mut hasher);
        // A `BTreeMap` hashes in key order, so two guards with the same masks declared in a
        // different order agree --- which they must, or the cache misses on a difference that
        // is not one.
        self.column_masks.hash(&mut hasher);
        hasher.finish()
    }

    /// Whose data this permits reaching.
    #[must_use]
    pub const fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// Who was permitted.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// What it permits reaching.
    #[must_use]
    pub const fn table(&self) -> &TableRef {
        &self.table
    }

    /// Which action it permits.
    #[must_use]
    pub const fn action(&self) -> Action {
        self.action
    }

    /// The predicate that must be conjoined into the scan, if any.
    ///
    /// `None` means every row, which is a *decision* rather than an absence: the policy
    /// found an unrestricted grant. A caller that cannot distinguish "no filter needed"
    /// from "I forgot to ask" would be one bug away from reading everything, which is why
    /// the guard exists at all.
    #[must_use]
    pub fn row_filter(&self) -> Option<&str> {
        self.row_filter.as_deref()
    }

    /// The columns that must be obscured.
    #[must_use]
    pub const fn column_masks(&self) -> &BTreeMap<String, Mask> {
        &self.column_masks
    }

    /// Whether this guard restricts which rows are visible.
    #[must_use]
    pub const fn restricts_rows(&self) -> bool {
        self.row_filter.is_some()
    }

    /// The object-store prefix this guard's tenant owns.
    ///
    /// Derived from the tenant rather than passed in, so a caller cannot supply a prefix
    /// belonging to somebody else. Safe to concatenate without escaping because a tenant
    /// identifier is a UUID: it cannot contain a path separator.
    #[must_use]
    pub fn storage_prefix(&self) -> String {
        sankhya_authz::principal::storage_prefix(&self.tenant)
    }
}
