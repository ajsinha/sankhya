//! Per-tenant limits other than memory, and refusals that say which one was hit.
//!
//! Memory has its own model --- floors, caps and a pool --- because it is the resource that
//! kills the process when it runs out. These are the others: how many queries a tenant may
//! run at once, how much data they may scan, how many rows they may take back, how much
//! they may store.
//!
//! # Why every refusal names the limit and the numbers
//!
//! "Quota exceeded" is not an error message, it is a shrug. The person receiving it needs
//! three things to act: *which* limit, *what* they asked for, and *what* they are allowed.
//! Without those they cannot tell whether to make the query smaller, ask for more quota, or
//! look for the runaway job that used it all --- and they will open a ticket instead.
//!
//! # Why a quota check is not an authorization check
//!
//! Being over quota is not the same as being forbidden, and conflating them produces the
//! wrong client behaviour. A forbidden request must never be retried; an over-quota one
//! frequently should be, later or smaller. [`Refusal::retryable`] says which this is.

use sankhya_types::TenantId;
use std::collections::BTreeMap;
use std::fmt;

/// What a tenant may consume.
///
/// Every field is a hard ceiling. There is deliberately no way to express "unlimited": a
/// tenant with no ceiling is one bad query away from being everyone's problem, and the
/// unlimited case is the one nobody notices until it happens. Where a limit genuinely
/// should not bind, set it high and write down why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Quota {
    /// How many queries this tenant may have running at once.
    pub max_concurrent_queries: u32,
    /// How many bytes one query may read.
    pub max_scan_bytes: u64,
    /// How many rows one query may return.
    ///
    /// A separate limit from the scan bound, because they fail differently: a large scan
    /// costs the server, and a large result costs the client, which is frequently the
    /// thing that actually falls over.
    pub max_result_rows: u64,
    /// How many bytes this tenant may store.
    pub max_storage_bytes: u64,
    /// How many graph epochs may be resident for this tenant at once.
    ///
    /// An epoch is large and lives until its last reader releases it. Without a bound, a
    /// tenant issuing traversals against successive snapshots holds every epoch between
    /// them.
    pub max_graph_epochs: u32,
}

impl Quota {
    /// A quota that will not bind in a test.
    ///
    /// Still finite. Named for what it is, because there is no unlimited variant to reach
    /// for by accident.
    #[must_use]
    pub const fn generous() -> Self {
        Self {
            max_concurrent_queries: 1_000,
            max_scan_bytes: u64::MAX,
            max_result_rows: u64::MAX,
            max_storage_bytes: u64::MAX,
            max_graph_epochs: 64,
        }
    }
}

/// What a tenant is currently consuming.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Consumption {
    /// Queries running now.
    pub running_queries: u32,
    /// Bytes stored.
    pub storage_bytes: u64,
    /// Graph epochs resident.
    pub graph_epochs: u32,
}

/// What a query is about to ask for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Request {
    /// Bytes the plan expects to read.
    pub scan_bytes: u64,
    /// Rows the plan expects to return.
    pub result_rows: u64,
    /// Bytes about to be written.
    pub write_bytes: u64,
    /// Whether this will hydrate a new graph epoch.
    pub hydrates_epoch: bool,
}

/// Every tenant's quota and consumption.
#[derive(Debug, Default)]
pub struct Quotas {
    quotas: BTreeMap<TenantId, Quota>,
    consumption: BTreeMap<TenantId, Consumption>,
    default_quota: Option<Quota>,
}

impl Quotas {
    /// An empty set. With no default configured, an unknown tenant is refused.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply this quota to any tenant without one of their own.
    ///
    /// Optional, and its absence means an unknown tenant is refused outright. That is the
    /// conservative direction: a tenant nobody configured should not inherit whatever the
    /// most permissive tenant happens to have.
    #[must_use]
    pub const fn with_default(mut self, quota: Quota) -> Self {
        self.default_quota = Some(quota);
        self
    }

    /// Set one tenant's quota.
    pub fn set(&mut self, tenant: TenantId, quota: Quota) {
        self.quotas.insert(tenant, quota);
    }

    /// Record what a tenant is consuming.
    pub fn observe(&mut self, tenant: TenantId, consumption: Consumption) {
        self.consumption.insert(tenant, consumption);
    }

    /// The quota in force for a tenant.
    #[must_use]
    pub fn quota_for(&self, tenant: &TenantId) -> Option<Quota> {
        self.quotas.get(tenant).copied().or(self.default_quota)
    }

    /// What a tenant is consuming.
    #[must_use]
    pub fn consumption_of(&self, tenant: &TenantId) -> Consumption {
        self.consumption.get(tenant).copied().unwrap_or_default()
    }

    /// Whether this request fits.
    ///
    /// Checks every limit and reports the **first** one exceeded, in a fixed order, so the
    /// same request always produces the same refusal. Reporting an arbitrary one would make
    /// a client's retry behaviour depend on map iteration order.
    pub fn admit(&self, tenant: &TenantId, request: &Request) -> Result<(), Refusal> {
        let Some(quota) = self.quota_for(tenant) else {
            return Err(Refusal::NoQuota {
                tenant: tenant.to_string(),
            });
        };
        let used = self.consumption_of(tenant);

        if used.running_queries >= quota.max_concurrent_queries {
            return Err(Refusal::Exceeded {
                tenant: tenant.to_string(),
                limit: Limit::ConcurrentQueries,
                requested: u64::from(used.running_queries).saturating_add(1),
                allowed: u64::from(quota.max_concurrent_queries),
            });
        }
        if request.scan_bytes > quota.max_scan_bytes {
            return Err(Refusal::Exceeded {
                tenant: tenant.to_string(),
                limit: Limit::ScanBytes,
                requested: request.scan_bytes,
                allowed: quota.max_scan_bytes,
            });
        }
        if request.result_rows > quota.max_result_rows {
            return Err(Refusal::Exceeded {
                tenant: tenant.to_string(),
                limit: Limit::ResultRows,
                requested: request.result_rows,
                allowed: quota.max_result_rows,
            });
        }
        let after_write = used.storage_bytes.saturating_add(request.write_bytes);
        if after_write > quota.max_storage_bytes {
            return Err(Refusal::Exceeded {
                tenant: tenant.to_string(),
                limit: Limit::StorageBytes,
                requested: after_write,
                allowed: quota.max_storage_bytes,
            });
        }
        if request.hydrates_epoch && used.graph_epochs >= quota.max_graph_epochs {
            return Err(Refusal::Exceeded {
                tenant: tenant.to_string(),
                limit: Limit::GraphEpochs,
                requested: u64::from(used.graph_epochs).saturating_add(1),
                allowed: u64::from(quota.max_graph_epochs),
            });
        }
        Ok(())
    }
}

/// Which ceiling was reached.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Limit {
    /// Queries running at once.
    ConcurrentQueries,
    /// Bytes one query may read.
    ScanBytes,
    /// Rows one query may return.
    ResultRows,
    /// Bytes a tenant may store.
    StorageBytes,
    /// Graph epochs resident at once.
    GraphEpochs,
}

impl Limit {
    /// The name an operator will look for in configuration.
    #[must_use]
    pub const fn setting(self) -> &'static str {
        match self {
            Self::ConcurrentQueries => "max_concurrent_queries",
            Self::ScanBytes => "max_scan_bytes",
            Self::ResultRows => "max_result_rows",
            Self::StorageBytes => "max_storage_bytes",
            Self::GraphEpochs => "max_graph_epochs",
        }
    }

    /// Whether waiting could help.
    ///
    /// Concurrency and epoch residency free up on their own; a scan or result too large
    /// will be too large next time too, and a storage ceiling needs data deleted or the
    /// quota raised. Telling a client to retry something that can never succeed turns one
    /// refusal into a loop.
    #[must_use]
    pub const fn frees_on_its_own(self) -> bool {
        matches!(self, Self::ConcurrentQueries | Self::GraphEpochs)
    }
}

/// Why a request was not admitted.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// A ceiling was reached.
    Exceeded {
        /// Whose.
        tenant: String,
        /// Which ceiling.
        limit: Limit,
        /// What the request would bring the total to.
        requested: u64,
        /// What is permitted.
        allowed: u64,
    },
    /// No quota is configured for this tenant, and there is no default.
    NoQuota {
        /// Which tenant.
        tenant: String,
    },
}

impl Refusal {
    /// Whether the same request could succeed later, unchanged.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Exceeded { limit, .. } => limit.frees_on_its_own(),
            // An unconfigured tenant stays unconfigured until somebody configures it.
            Self::NoQuota { .. } => false,
        }
    }

    /// Which limit, for a caller that wants to branch on it.
    #[must_use]
    pub const fn limit(&self) -> Option<Limit> {
        match self {
            Self::Exceeded { limit, .. } => Some(*limit),
            Self::NoQuota { .. } => None,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exceeded {
                tenant,
                limit,
                requested,
                allowed,
            } => {
                let advice = if limit.frees_on_its_own() {
                    "this frees up as other work finishes, so retrying shortly may succeed"
                } else {
                    "retrying unchanged will fail the same way; make the request smaller or \
                     have the limit raised"
                };
                write!(
                    f,
                    "tenant {tenant} exceeded {}: asked for {requested}, allowed {allowed}. \
                     {advice}",
                    limit.setting()
                )
            }
            Self::NoQuota { tenant } => write!(
                f,
                "no quota is configured for tenant {tenant} and there is no default. \
                 Refusing rather than inheriting: a tenant nobody configured should not \
                 receive whatever the most permissive tenant happens to have"
            ),
        }
    }
}

impl std::error::Error for Refusal {}
