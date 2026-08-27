//! What a principal may see, decided by a function of its inputs and nothing else.
//!
//! # Why this component is pure
//!
//! Every decision here is a function of the policy set and the principal. No clock, no
//! network, no database, no ambient state. That is what makes it exhaustively testable, and
//! exhaustive testing is what this component needs more than any other in the system: a
//! defect here is not a wrong answer, it is one tenant reading another's data.
//!
//! It is also why the mutation audit covers this file heavily. A surviving mutant anywhere
//! else is a test that proves less than it claims. A surviving mutant *here* is a breach
//! with a green build.
//!
//! # Deny wins, and absence is denial
//!
//! Two rules, and both are the conservative direction:
//!
//! - A principal with no rule granting access is denied. Not "allowed until a rule forbids
//!   it" --- a policy set that fails to load, or a table nobody wrote a rule for, then
//!   grants everything.
//! - An explicit denial beats any number of grants. Otherwise adding a role to a principal
//!   could only ever widen what they see, and there would be no way to express an exclusion.

use crate::principal::{Principal, Role, TenantId};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A table a policy applies to.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TableRef {
    /// The schema it lives in.
    pub schema: String,
    /// Its name.
    pub table: String,
}

impl TableRef {
    /// A reference to `schema.table`.
    #[must_use]
    pub fn new(schema: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            table: table.into(),
        }
    }
}

impl fmt::Display for TableRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.schema, self.table)
    }
}

/// What a principal wants to do.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Action {
    /// Read rows.
    Read,
    /// Add rows.
    Insert,
    /// Change rows.
    Update,
    /// Remove rows.
    Delete,
}

/// How a column is obscured when a principal may see the row but not the value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Mask {
    /// The value becomes null.
    Null,
    /// All but the last `keep` characters become a fixed character.
    ///
    /// Reveals the shape of a value without revealing it. Note that a partially masked
    /// value still leaks: with enough rows, the visible tail plus the length is often
    /// identifying. This is a usability concession and should not be read as anonymisation.
    Partial {
        /// How many trailing characters stay visible.
        keep: usize,
    },
    /// The value becomes a fixed string.
    Constant {
        /// What to show instead.
        value: String,
    },
}

/// One rule.
///
/// A rule grants or denies one action on one table to whoever holds one role, optionally
/// narrowed by a row predicate and a set of column masks.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rule {
    /// Which tenant's policy set this belongs to.
    pub tenant: TenantId,
    /// Who it applies to.
    pub role: Role,
    /// What it applies to.
    pub table: TableRef,
    /// Which action.
    pub action: Action,
    /// Whether this grants or forbids.
    pub effect: Effect,
    /// A predicate conjoined into the scan, restricting which rows are visible.
    ///
    /// Held as text because it is rewritten into the query plan by the catalog, which owns
    /// the parser. Keeping this component free of a query-engine dependency is what lets it
    /// be tested exhaustively without one.
    pub row_filter: Option<String>,
    /// Columns this rule obscures.
    pub column_masks: BTreeMap<String, Mask>,
}

impl Rule {
    /// A rule granting `action` on `table` to holders of `role`.
    #[must_use]
    pub fn grant(tenant: TenantId, role: Role, table: TableRef, action: Action) -> Self {
        Self {
            tenant,
            role,
            table,
            action,
            effect: Effect::Allow,
            row_filter: None,
            column_masks: BTreeMap::new(),
        }
    }

    /// A rule forbidding `action` on `table` to holders of `role`.
    #[must_use]
    pub fn deny(tenant: TenantId, role: Role, table: TableRef, action: Action) -> Self {
        Self {
            effect: Effect::Deny,
            ..Self::grant(tenant, role, table, action)
        }
    }

    /// The same rule, restricted to rows satisfying a predicate.
    #[must_use]
    pub fn where_rows(mut self, predicate: impl Into<String>) -> Self {
        self.row_filter = Some(predicate.into());
        self
    }

    /// The same rule, obscuring a column.
    #[must_use]
    pub fn masking(mut self, column: impl Into<String>, mask: Mask) -> Self {
        self.column_masks.insert(column.into(), mask);
        self
    }
}

/// Whether a rule permits or forbids.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Permits.
    Allow,
    /// Forbids, and beats any grant.
    Deny,
}

/// Every rule in force.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PolicySet {
    rules: Vec<Rule>,
}

impl PolicySet {
    /// An empty policy set, which permits nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a rule.
    #[must_use]
    pub fn with(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// How many rules.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether there are none, in which case nothing is permitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// What this principal may do to this table.
    ///
    /// The whole decision, computed here and nowhere else. Every engine consults this same
    /// function through the catalog, which is what stops the three of them drifting apart.
    #[must_use]
    pub fn decide(&self, principal: &Principal, table: &TableRef, action: Action) -> Decision {
        // The tenant check comes first and is not expressible as a rule. A rule is data,
        // and data can be wrong; the tenant boundary is the one thing that must not depend
        // on anybody having written the right row.
        let applicable: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| {
                r.tenant == *principal.tenant()
                    && r.table == *table
                    && r.action == action
                    && principal.has_role(&r.role)
            })
            .collect();

        // Deny wins, and it wins before anything else is computed. A denial that could be
        // outvoted by grants would make adding a role only ever widen access, and there
        // would be no way to express an exclusion at all.
        if let Some(denial) = applicable.iter().find(|r| r.effect == Effect::Deny) {
            return Decision::Denied {
                reason: DenialReason::ExplicitDeny {
                    role: denial.role.clone(),
                },
            };
        }

        let grants: Vec<&&Rule> = applicable
            .iter()
            .filter(|r| r.effect == Effect::Allow)
            .collect();
        if grants.is_empty() {
            return Decision::Denied {
                reason: DenialReason::NoGrant,
            };
        }

        // Several grants combine by *union* on rows and by *intersection* on visibility.
        //
        // Rows: a principal holding two roles sees the union of what each permits, so the
        // filters are joined with OR. Joining them with AND would mean a second role could
        // only ever *reduce* what someone sees, which is not what granting a role means.
        //
        // A grant with no filter sees every row, so it absorbs the others: OR-ing anything
        // with "all rows" is "all rows", and emitting a filter there would wrongly narrow.
        let row_filter = if grants.iter().any(|r| r.row_filter.is_none()) {
            None
        } else {
            let mut clauses: Vec<String> =
                grants.iter().filter_map(|r| r.row_filter.clone()).collect();
            clauses.sort();
            clauses.dedup();
            match clauses.len() {
                0 => None,
                1 => clauses.first().cloned(),
                _ => Some(
                    clauses
                        .iter()
                        .map(|c| format!("({c})"))
                        .collect::<Vec<_>>()
                        .join(" OR "),
                ),
            }
        };

        // Columns: a column is masked only if *every* grant masks it. If one role may see
        // it in the clear, the principal may. Masking on the union instead would let an
        // added role take visibility away, which is the same asymmetry as above.
        let mut column_masks = BTreeMap::new();
        if let Some(first) = grants.first() {
            for (column, mask) in &first.column_masks {
                let masked_by_all = grants.iter().all(|r| r.column_masks.contains_key(column));
                if masked_by_all {
                    column_masks.insert(column.clone(), mask.clone());
                }
            }
        }

        Decision::Allowed {
            row_filter,
            column_masks,
        }
    }

    /// Every table this principal may read, for catalog listing.
    ///
    /// A principal must not see the *existence* of a table they cannot read. A table list
    /// is an information leak in its own right: names disclose what a business does, and a
    /// permission error on a name they were never meant to know confirms it exists.
    #[must_use]
    pub fn visible_tables(&self, principal: &Principal) -> BTreeSet<TableRef> {
        self.rules
            .iter()
            .filter(|r| r.tenant == *principal.tenant() && principal.has_role(&r.role))
            .filter(|r| r.action == Action::Read)
            .filter(|r| {
                r.effect == Effect::Allow
                    && matches!(
                        self.decide(principal, &r.table, Action::Read),
                        Decision::Allowed { .. }
                    )
            })
            .map(|r| r.table.clone())
            .collect()
    }
}

/// What a principal may do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Permitted, subject to these restrictions.
    Allowed {
        /// A predicate to conjoin into the scan, or `None` for every row.
        row_filter: Option<String>,
        /// Columns to obscure.
        column_masks: BTreeMap<String, Mask>,
    },
    /// Not permitted.
    Denied {
        /// Why.
        reason: DenialReason,
    },
}

impl Decision {
    /// Whether this permits the action.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed { .. })
    }

    /// The row filter, if permitted and restricted.
    #[must_use]
    pub fn row_filter(&self) -> Option<&str> {
        match self {
            Self::Allowed { row_filter, .. } => row_filter.as_deref(),
            Self::Denied { .. } => None,
        }
    }

    /// The column masks, if permitted.
    #[must_use]
    pub fn column_masks(&self) -> BTreeMap<String, Mask> {
        match self {
            Self::Allowed { column_masks, .. } => column_masks.clone(),
            Self::Denied { .. } => BTreeMap::new(),
        }
    }
}

/// Why access was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DenialReason {
    /// No rule granted it.
    NoGrant,
    /// A rule explicitly forbade it.
    ExplicitDeny {
        /// Which role's rule.
        role: Role,
    },
}

impl fmt::Display for DenialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Deliberately identical wording for both. Distinguishing them tells a caller
            // whether a rule exists that names them, which is information about the policy
            // set they were not granted.
            Self::NoGrant | Self::ExplicitDeny { .. } => {
                f.write_str("this principal is not permitted to perform this action")
            }
        }
    }
}
