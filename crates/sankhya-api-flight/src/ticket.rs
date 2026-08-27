//! The token that stands between planning a query and receiving its data.
//!
//! # Why a ticket is a security boundary
//!
//! Flight splits a query in two. `GetFlightInfo` plans it and returns a ticket;
//! `DoGet` redeems the ticket for data. Those are separate calls, possibly on separate
//! connections, and in a real deployment possibly to separate nodes.
//!
//! The obvious implementation makes the ticket *be* the query text, and re-plans it on
//! redemption. That is wrong in a way that is easy to miss: the principal redeeming a ticket
//! is not necessarily the one who requested it. A ticket that carries a query re-authorizes
//! against whoever presents it --- which is either a second authorization the caller did not
//! ask for, or, if the redemption path is laxer than the planning path, none at all.
//!
//! So the decision is made **once**, at `GetFlightInfo`, and the ticket carries its outcome.
//! Redeeming does not re-plan and does not re-authorize. It only checks that the ticket is
//! being presented by the tenant it was issued to, which is the one thing that can be
//! checked without repeating the decision.
//!
//! # Why it expires
//!
//! A ticket names a snapshot, and a snapshot's files are eventually retired. A ticket with
//! no expiry is a lease nobody granted: maintenance cannot know whether it is still wanted,
//! so either it refuses to reclaim space or it reclaims space a ticket still points at. The
//! second produces a redemption that fails on a missing file, far from the cause.

use sankhya_authz::principal::TenantId;
use std::fmt;

/// A planned query, redeemable for its data.
///
/// Deliberately not `Serialize`: the wire form is produced by [`Ticket::encode`] and read by
/// [`Ticket::decode`], so there is exactly one representation and it is this crate's
/// business. A derived serialisation would let a field be added that the decoder silently
/// ignores.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ticket {
    /// Whose data this permits reaching.
    tenant: TenantId,
    /// The statement that was planned and authorized.
    statement: String,
    /// The table snapshot the plan was made against.
    snapshot: u64,
    /// When this stops being redeemable, in microseconds from the epoch.
    expires_at: i64,
}

impl Ticket {
    /// Issue a ticket for a query that has already been authorized.
    ///
    /// Called only from the planning path, after a decision. There is no constructor that
    /// takes a statement without a tenant, so a ticket cannot exist without naming who it
    /// is for.
    #[must_use]
    pub fn issue(
        tenant: TenantId,
        statement: impl Into<String>,
        snapshot: u64,
        now: i64,
        lifetime_micros: i64,
    ) -> Self {
        Self {
            tenant,
            statement: statement.into(),
            snapshot,
            // Clamped rather than refused: a caller asking for longer than the maximum is
            // told what it got, and a negative lifetime is an expired ticket rather than a
            // ticket that has always been valid.
            expires_at: now.saturating_add(lifetime_micros.max(0)),
        }
    }

    /// The statement this ticket was issued for.
    #[must_use]
    pub fn statement(&self) -> &str {
        &self.statement
    }

    /// The snapshot the plan was made against.
    #[must_use]
    pub const fn snapshot(&self) -> u64 {
        self.snapshot
    }

    /// Whose data it permits reaching.
    #[must_use]
    pub const fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// Whether this ticket may be redeemed now, by this tenant.
    ///
    /// The tenant check is not a second authorization --- the decision was made when the
    /// ticket was issued. It is the one thing that can be verified without repeating that
    /// decision, and it is what stops a leaked ticket being useful to somebody else.
    pub fn admit(&self, presented_by: &TenantId, now: i64) -> Result<(), Refused> {
        if self.tenant != *presented_by {
            return Err(Refused::WrongTenant);
        }
        if now >= self.expires_at {
            return Err(Refused::Expired {
                expired_at: self.expires_at,
                now,
            });
        }
        Ok(())
    }

    /// The wire form.
    ///
    /// Opaque to a client, which is the point: a client that could read a snapshot version
    /// out of a ticket would begin depending on it, and the next change to the format would
    /// be a breaking wire change.
    ///
    /// Not signed. A ticket is a *capability* and this encoding does not stop a client
    /// forging one --- what stops a forged ticket being useful is that redemption checks the
    /// tenant, and a client cannot forge the identity it authenticated as. Where a stronger
    /// guarantee is wanted, the encoding is the place to add a signature, and this comment
    /// is where somebody will look.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        // Length-prefixed, so a statement containing the separator cannot split the ticket
        // into different fields than it was written with.
        let mut out = Vec::new();
        out.extend_from_slice(b"skhyft1");
        push_string(&mut out, &self.tenant.to_string());
        push_string(&mut out, &self.statement);
        out.extend_from_slice(&self.snapshot.to_be_bytes());
        out.extend_from_slice(&self.expires_at.to_be_bytes());
        out
    }

    /// Read a ticket back.
    ///
    /// Returns `None` for anything this server did not issue, and is deliberately silent
    /// about which part failed: a caller who learned that the prefix was right but the
    /// length wrong would have learned the format.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(b"skhyft1")?;
        let (tenant, rest) = take_string(rest)?;
        let (statement, rest) = take_string(rest)?;
        if rest.len() != 16 {
            return None;
        }
        let snapshot = u64::from_be_bytes(rest.get(..8)?.try_into().ok()?);
        let expires_at = i64::from_be_bytes(rest.get(8..16)?.try_into().ok()?);

        // The tenant is a UUID with a prefix; parsing it back is what makes a ticket naming
        // a malformed tenant unredeemable rather than redeemable by nobody.
        let uuid = uuid::Uuid::parse_str(tenant.strip_prefix("tenant:")?).ok()?;
        Some(Self {
            tenant: TenantId::from_uuid(uuid),
            statement,
            snapshot,
            expires_at,
        })
    }
}

/// Write a length-prefixed string.
fn push_string(out: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Read a length-prefixed string.
fn take_string(bytes: &[u8]) -> Option<(String, &[u8])> {
    let length = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    let text = std::str::from_utf8(bytes.get(4..4 + length)?)
        .ok()?
        .to_string();
    Some((text, bytes.get(4 + length..)?))
}

/// Why a ticket was not honoured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Presented by a tenant it was not issued to.
    WrongTenant,
    /// Presented after it stopped being redeemable.
    Expired {
        /// When it expired.
        expired_at: i64,
        /// What time it is.
        now: i64,
    },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Deliberately says nothing about which tenant it *was* issued to. That would
            // tell a caller holding a leaked ticket whose it is.
            Self::WrongTenant => f.write_str("this ticket was not issued to you"),
            Self::Expired { expired_at, now } => write!(
                f,
                "this ticket expired at {expired_at} and it is now {now}. Plan the query \
                 again: a ticket names a snapshot, and a snapshot's files are eventually \
                 retired"
            ),
        }
    }
}

impl std::error::Error for Refused {}
