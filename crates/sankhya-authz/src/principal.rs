//! Who is asking.
//!
//! # One type, resolved once
//!
//! A principal is established at the edge --- where the token was verified, the certificate
//! checked, the password accepted --- and then carried unchanged through planning,
//! execution, graph traversal and audit. There is no second construction path and no way to
//! build one from a string deeper in the system.
//!
//! That matters because the alternative is what usually happens: each layer re-derives who
//! the caller is from whatever it has to hand, the derivations drift, and one of them ends
//! up more permissive than the others. A single type built in a single place cannot drift.
//!
//! # Why the tenant is not optional
//!
//! Every principal belongs to exactly one tenant. Not zero, and not many. A principal
//! without a tenant would have to be handled somewhere, and the handling would be a branch
//! that runs without a tenant scope --- which is precisely the branch an attacker wants.
//!
//! Cross-tenant access, where it is legitimate at all, is a *separate* principal with a
//! separate tenant, obtained by a separate authentication. It is never a flag on this one.

/// Which tenant's data a principal may see.
///
/// Re-exported from `sankhya-types` rather than defined here, deliberately. A second type
/// for the same concept is how two parts of a system come to disagree about who someone is,
/// and such a disagreement is always resolved in favour of whichever one checked less.
///
/// It is a UUID, which matters more than it looks. This identifier becomes an object-store
/// path prefix, and a UUID **cannot** contain a `/` or a `..` --- so path traversal into
/// another tenant's data is impossible by construction rather than prevented by validation.
/// An earlier version of this file defined its own string identifier and validated the
/// characters. That works until somebody adds a second construction path that does not.
pub use sankhya_types::TenantId;
use std::collections::BTreeSet;
use std::fmt;

/// The object-store prefix a tenant's data lives under.
///
/// Derived from the identifier rather than supplied by a caller, who could otherwise name
/// somebody else's. Safe to concatenate without escaping, because the identifier is a UUID.
#[must_use]
pub fn storage_prefix(tenant: &TenantId) -> String {
    format!("{}/", tenant.as_uuid())
}

/// A named capability a principal may hold.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Role(String);

impl Role {
    /// A role by name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The role's name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a principal proved who they are.
///
/// Recorded because the audit record has to reproduce the decision, and the decision may
/// depend on this: a policy can reasonably require a stronger method for a stronger
/// permission, and an audit that does not say which method was used cannot show that it did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Authentication {
    /// A federated token from a trusted issuer.
    FederatedToken,
    /// A client certificate.
    MutualTls,
    /// A password over the wire protocol.
    Password,
    /// The process itself, for maintenance work with no external caller.
    ///
    /// Deliberately its own variant rather than a synthetic principal, so a policy or an
    /// audit reader can tell internal work from a request that arrived over a wire.
    Internal,
}

/// Who is asking, established once at the edge.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Principal {
    subject: String,
    tenant: TenantId,
    roles: BTreeSet<Role>,
    authentication: Authentication,
}

impl Principal {
    /// Establish a principal. Called at the edge, after authentication has succeeded.
    ///
    /// There is deliberately no `Default`, no `Principal::anonymous()` and no way to build
    /// one from a bare string. Every construction records how authentication happened,
    /// because a principal that cannot say how it was established cannot be audited.
    pub fn authenticated(
        subject: impl Into<String>,
        tenant: TenantId,
        roles: impl IntoIterator<Item = Role>,
        authentication: Authentication,
    ) -> Result<Self, InvalidPrincipal> {
        let subject = subject.into();
        if subject.is_empty() {
            return Err(InvalidPrincipal::NoSubject);
        }
        Ok(Self {
            subject,
            tenant,
            roles: roles.into_iter().collect(),
            authentication,
        })
    }

    /// The internal principal for maintenance work belonging to one tenant.
    ///
    /// Still scoped to a tenant. Maintenance that could run without a tenant scope would be
    /// the one code path with no boundary, and every attacker looks for exactly that.
    #[must_use]
    pub fn internal(tenant: TenantId) -> Self {
        Self {
            subject: "system".to_string(),
            tenant,
            roles: BTreeSet::new(),
            authentication: Authentication::Internal,
        }
    }

    /// Who they are.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Whose data they may see.
    #[must_use]
    pub const fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// What they hold.
    #[must_use]
    pub fn roles(&self) -> &BTreeSet<Role> {
        &self.roles
    }

    /// Whether they hold this role.
    #[must_use]
    pub fn has_role(&self, role: &Role) -> bool {
        self.roles.contains(role)
    }

    /// How they were authenticated.
    #[must_use]
    pub const fn authentication(&self) -> Authentication {
        self.authentication
    }

    /// Whether this is internal work rather than a request from outside.
    #[must_use]
    pub const fn is_internal(&self) -> bool {
        matches!(self.authentication, Authentication::Internal)
    }
}

/// Why a principal could not be established.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InvalidPrincipal {
    /// No subject was given.
    NoSubject,
}

impl fmt::Display for InvalidPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSubject => f.write_str(
                "a principal must have a subject: an unattributable request cannot be \
                 audited, and an audit that cannot name who acted is not an audit",
            ),
        }
    }
}

impl std::error::Error for InvalidPrincipal {}
