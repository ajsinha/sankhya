//! Deciding whether a query may start.
//!
//! # Why admission is mandatory rather than advisory
//!
//! The query engine's hash joins do not spill. An unbounded build side does not degrade
//! into slowness — it exhausts memory and the operating system terminates the process,
//! taking every other query with it and, where the transactional store is supervised by
//! the same process, the database too.
//!
//! That is the difference between a governor that is nice to have and one that is load
//! bearing. A query the system cannot afford must not start, because there is no point
//! after starting at which it can be made to fit.
//!
//! # The distinction the caller must be able to act on
//!
//! **"Too large for the pool" and "too large right now" are different answers**, and
//! collapsing them into one is how a client retries forever. A query needing more than
//! the entire pool will never fit, however long anyone waits; one needing more than is
//! currently free will fit as soon as something finishes. The first deserves a rewrite,
//! the second a retry, and the reply says which.
//!
//! # Why the queue is bounded
//!
//! Unbounded queueing is not an alternative to rejection — it is rejection with the
//! latency hidden. A query that will wait ten minutes and then fail has consumed a
//! client connection, a slot in someone's timeout budget, and ten minutes of a user's
//! patience to deliver the same answer that was available immediately.
//!
//! So the queue has a depth, and past it the answer is a refusal that arrives at once.

use std::collections::BTreeMap;
use std::fmt;

/// What a query is expected to need.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Demand {
    pub tenant: String,
    /// Peak bytes, estimated from plan cardinality.
    ///
    /// An estimate, necessarily — nothing has run yet. It is expected to be wrong, and
    /// the accounting that catches it being wrong is a separate mechanism; this decides
    /// whether to start at all.
    pub estimated_bytes: u64,
}

/// How much a tenant may hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TenantLimits {
    /// Always available to this tenant, even under global pressure.
    ///
    /// Without a floor, a tenant that submits steadily starves one that submits
    /// occasionally — the busy tenant holds the pool, and the quiet one never finds
    /// room. The floor is what makes "shared" mean something.
    pub floor_bytes: u64,
    /// Never exceeded, even when the pool is otherwise idle.
    ///
    /// A cap is what stops one tenant's bad afternoon from being everyone's. Without
    /// it, an idle pool is an invitation.
    pub cap_bytes: u64,
}

/// The whole machine's budget and how much of it is spoken for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PoolState {
    pub total_bytes: u64,
    pub in_use_bytes: u64,
    /// Per tenant, of `in_use_bytes`.
    pub per_tenant: BTreeMap<String, u64>,
    /// Queries already waiting.
    pub queued: usize,
    pub max_queue_depth: usize,
}

impl PoolState {
    #[must_use]
    pub fn free_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.in_use_bytes)
    }

    #[must_use]
    pub fn held_by(&self, tenant: &str) -> u64 {
        self.per_tenant.get(tenant).copied().unwrap_or(0)
    }
}

/// Why a query will not be run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Rejection {
    /// Larger than the entire pool. No amount of waiting helps.
    ExceedsPool { needed: u64, pool: u64 },
    /// Larger than this tenant may ever hold. No amount of waiting helps.
    ExceedsTenantCap { needed: u64, cap: u64 },
    /// The queue is full. Waiting would help; there is nowhere to wait.
    QueueFull { depth: usize },
    /// The system is shedding load and is not admitting anything.
    NotAdmitting,
}

impl Rejection {
    /// Whether the same query submitted later could succeed.
    ///
    /// The one thing a client most needs to know, and the one thing a bare error string
    /// does not carry.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::ExceedsPool { .. } | Self::ExceedsTenantCap { .. } => false,
            Self::QueueFull { .. } | Self::NotAdmitting => true,
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExceedsPool { needed, pool } => write!(
                f,
                "this query is estimated to need {needed} bytes and the whole pool is \
                 {pool}; it cannot run at any level of load, so retrying will not help \
                 — reduce what it scans or joins"
            ),
            Self::ExceedsTenantCap { needed, cap } => write!(
                f,
                "this query is estimated to need {needed} bytes and this tenant may hold \
                 at most {cap}; retrying will not help"
            ),
            Self::QueueFull { depth } => write!(
                f,
                "{depth} queries are already waiting; this one is refused now rather \
                 than queued behind them, because a refusal that arrives immediately is \
                 worth more than the same refusal after a timeout"
            ),
            Self::NotAdmitting => f.write_str(
                "the system is shedding load and is not admitting new queries; those \
                 already running will finish",
            ),
        }
    }
}

impl std::error::Error for Rejection {}

/// The decision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Decision {
    Admit,
    /// Wait. The position is what a caller reports rather than an opaque delay.
    Queue {
        position: usize,
    },
    Reject(Rejection),
}

impl Decision {
    #[must_use]
    pub const fn admitted(&self) -> bool {
        matches!(self, Self::Admit)
    }
}

/// Whether the system is accepting work at all.
///
/// Separate from the pool because it is not a capacity question: under the higher
/// pressure levels the system stops admitting *regardless* of free memory, so that
/// everything left goes to whatever is defending the source.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Posture {
    #[default]
    Admitting,
    /// Existing queries run to their deadline; nothing new starts.
    Shedding,
}

/// Decide whether a query may start.
///
/// Checked in a deliberate order: the two permanent refusals first, so a query that can
/// never run is told so immediately rather than queued behind work it will outlive.
#[must_use]
pub fn admit(
    demand: &Demand,
    pool: &PoolState,
    limits: &TenantLimits,
    posture: Posture,
) -> Decision {
    // Permanent refusals first. A query that cannot run at any load level should not
    // wait to be told, and must not occupy a queue slot ahead of work that could run.
    if demand.estimated_bytes > pool.total_bytes {
        return Decision::Reject(Rejection::ExceedsPool {
            needed: demand.estimated_bytes,
            pool: pool.total_bytes,
        });
    }
    if demand.estimated_bytes > limits.cap_bytes {
        return Decision::Reject(Rejection::ExceedsTenantCap {
            needed: demand.estimated_bytes,
            cap: limits.cap_bytes,
        });
    }

    if posture == Posture::Shedding {
        return Decision::Reject(Rejection::NotAdmitting);
    }

    let held = pool.held_by(&demand.tenant);
    let after = held.saturating_add(demand.estimated_bytes);

    // The cap binds even when the pool is idle. An idle pool is otherwise an invitation.
    if after > limits.cap_bytes {
        return queue_or_refuse(pool);
    }

    // Within the tenant's floor, admission does not depend on global pressure. This is
    // the whole point of a floor: without it a tenant that submits steadily holds the
    // pool and a tenant that submits occasionally never finds room.
    if after <= limits.floor_bytes {
        return Decision::Admit;
    }

    if demand.estimated_bytes <= pool.free_bytes() {
        return Decision::Admit;
    }

    queue_or_refuse(pool)
}

fn queue_or_refuse(pool: &PoolState) -> Decision {
    if pool.queued >= pool.max_queue_depth {
        return Decision::Reject(Rejection::QueueFull { depth: pool.queued });
    }
    Decision::Queue {
        position: pool.queued,
    }
}
