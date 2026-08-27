//! Pinned snapshots, and why they expire.
//!
//! A pinned read needs the files of its snapshot to still exist, so pinning has to stop
//! maintenance retiring them. That is a lease.
//!
//! # Why every lease has a bounded lifetime
//!
//! A lease with no expiry is a way for one forgotten session to stop a warehouse reclaiming
//! space forever. It does not fail loudly --- storage simply grows, compaction accumulates
//! superseded files it may not delete, and the cause is a connection somebody opened last
//! March.
//!
//! So there is no unbounded lease. A long-running job renews, which means something has to
//! still be alive to renew it, and that is exactly the property wanted: the lease survives
//! as long as the work does and no longer.

use sankhya_types::TenantId;
use std::collections::BTreeMap;
use std::fmt;

/// A held snapshot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Lease {
    /// Which session holds it.
    pub session: String,
    /// Whose data.
    pub tenant: TenantId,
    /// The snapshot version held.
    pub snapshot: u64,
    /// When it stops being held, as microseconds from the epoch.
    pub expires_at: i64,
}

/// Every snapshot currently held, and the rules about holding them.
///
/// Pure: `now` is supplied by the caller rather than read from a clock, so expiry can be
/// tested without waiting and a decision can be replayed exactly.
#[derive(Debug)]
pub struct Leases {
    held: BTreeMap<String, Lease>,
    max_lifetime_micros: i64,
}

impl Leases {
    /// A registry whose leases may live at most `max_lifetime_micros`.
    ///
    /// There is deliberately no constructor without a maximum. The unbounded case is the
    /// one that quietly stops a warehouse reclaiming space, so it is not expressible.
    #[must_use]
    pub fn with_max_lifetime(max_lifetime_micros: i64) -> Self {
        Self {
            held: BTreeMap::new(),
            max_lifetime_micros: max_lifetime_micros.max(1),
        }
    }

    /// Take a lease on a snapshot.
    ///
    /// A request for longer than the maximum is **clamped rather than refused**, and the
    /// granted expiry is returned so the caller knows what it actually got. Refusing would
    /// push callers towards asking for exactly the maximum, which is the same thing with
    /// more steps; silently granting what was asked for would defeat the bound entirely.
    pub fn acquire(
        &mut self,
        session: impl Into<String>,
        tenant: TenantId,
        snapshot: u64,
        now: i64,
        wanted_micros: i64,
    ) -> Lease {
        let granted = wanted_micros.clamp(0, self.max_lifetime_micros);
        let lease = Lease {
            session: session.into(),
            tenant,
            snapshot,
            expires_at: now.saturating_add(granted),
        };
        self.held.insert(lease.session.clone(), lease.clone());
        lease
    }

    /// Extend a lease that has not yet expired.
    ///
    /// An expired lease cannot be renewed, only re-acquired --- and re-acquiring may fail if
    /// the snapshot has since been retired. That asymmetry is deliberate: renewing an
    /// expired lease would let a session hold a snapshot that maintenance was already
    /// entitled to delete, and whether the files still exist would be a matter of timing.
    pub fn renew(
        &mut self,
        session: &str,
        now: i64,
        wanted_micros: i64,
    ) -> Result<Lease, LeaseError> {
        let Some(existing) = self.held.get(session) else {
            return Err(LeaseError::NoSuchLease {
                session: session.to_string(),
            });
        };
        if existing.expires_at <= now {
            return Err(LeaseError::Expired {
                session: session.to_string(),
                expired_at: existing.expires_at,
                now,
            });
        }
        let granted = wanted_micros.clamp(0, self.max_lifetime_micros);
        let renewed = Lease {
            expires_at: now.saturating_add(granted),
            ..existing.clone()
        };
        self.held.insert(session.to_string(), renewed.clone());
        Ok(renewed)
    }

    /// Give a lease up.
    pub fn release(&mut self, session: &str) -> Option<Lease> {
        self.held.remove(session)
    }

    /// Remove every expired lease, returning them.
    ///
    /// Called before maintenance decides what it may delete. Expiry is lazy rather than
    /// timed because a timer is another thing to get wrong, and nothing observes a lease
    /// except the decision this feeds.
    pub fn expire(&mut self, now: i64) -> Vec<Lease> {
        let expired: Vec<Lease> = self
            .held
            .values()
            .filter(|l| l.expires_at <= now)
            .cloned()
            .collect();
        for lease in &expired {
            self.held.remove(&lease.session);
        }
        expired
    }

    /// The snapshots a tenant still holds, ascending.
    ///
    /// What maintenance consults. A file belonging to any of these must not be deleted,
    /// and the oldest one bounds how far back retention has to reach.
    #[must_use]
    pub fn held_snapshots(&self, tenant: &TenantId, now: i64) -> Vec<u64> {
        let mut snapshots: Vec<u64> = self
            .held
            .values()
            .filter(|l| l.tenant == *tenant && l.expires_at > now)
            .map(|l| l.snapshot)
            .collect();
        snapshots.sort_unstable();
        snapshots.dedup();
        snapshots
    }

    /// The oldest snapshot any tenant still holds.
    ///
    /// The number retention is bounded by. `None` means nothing is held and retention is
    /// free to apply its ordinary policy.
    #[must_use]
    pub fn oldest_held(&self, now: i64) -> Option<u64> {
        self.held
            .values()
            .filter(|l| l.expires_at > now)
            .map(|l| l.snapshot)
            .min()
    }

    /// Whether this session holds a live lease.
    #[must_use]
    pub fn is_held(&self, session: &str, now: i64) -> bool {
        self.held.get(session).is_some_and(|l| l.expires_at > now)
    }

    /// How many leases are live.
    #[must_use]
    pub fn live_count(&self, now: i64) -> usize {
        self.held.values().filter(|l| l.expires_at > now).count()
    }

    /// The longest a lease may live.
    #[must_use]
    pub const fn max_lifetime_micros(&self) -> i64 {
        self.max_lifetime_micros
    }
}

/// Why a lease operation failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LeaseError {
    /// This session holds no lease.
    NoSuchLease {
        /// Which session.
        session: String,
    },
    /// The lease has already expired and must be re-acquired.
    Expired {
        /// Which session.
        session: String,
        /// When it expired.
        expired_at: i64,
        /// What time it is.
        now: i64,
    },
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchLease { session } => {
                write!(f, "the session {session} holds no snapshot lease")
            }
            Self::Expired {
                session,
                expired_at,
                now,
            } => write!(
                f,
                "the lease held by {session} expired at {expired_at} and it is now {now}; it \
                 must be re-acquired rather than renewed, and re-acquiring may fail if the \
                 snapshot has since been retired. Renewing an expired lease would let a \
                 session hold a snapshot maintenance was already entitled to delete"
            ),
        }
    }
}

impl std::error::Error for LeaseError {}
