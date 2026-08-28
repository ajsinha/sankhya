//! Error taxonomy.
//!
//! # Why a classification exists at all
//!
//! One enum drives six behaviours: retry policy, protocol status, SQL state, log
//! level, metric labelling and alerting. Without it each call site decides
//! independently, and the decisions drift until an operator cannot tell from a log
//! line whether to page someone.
//!
//! Every variant **must** be classifiable, and an exhaustive test asserts it. Without
//! that test a newly added variant silently inherits whatever the fallback happens to
//! be, which is how a fatal condition ends up being retried forever.

#![doc(html_root_url = "https://docs.rs/sankhya-error")]

pub mod protocol;

use std::fmt;
use std::time::Duration;

/// How the caller should react. This is the load-bearing type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// The request was wrong. Do not retry, do not page; log at informational level.
    User,
    /// Transient. Retry after the hint, if one is given.
    Retryable { after: Option<Duration> },
    /// A concurrent writer won. Re-plan and retry — a blind retry will lose again.
    Conflict,
    /// A limit was reached. Shed load and apply backpressure; do not retry immediately.
    Resource,
    /// The work was abandoned deliberately: deadline, client disconnect, or shutdown.
    Cancelled,
    /// An invariant was violated or state is corrupt. Fail fast and page.
    Fatal,
}

impl Class {
    /// Whether a caller may retry the identical request unchanged.
    ///
    /// `Conflict` is deliberately excluded: it needs a new plan against the new state,
    /// not the same plan again.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Retryable { .. })
    }

    /// Whether reaching this should wake a human.
    #[must_use]
    pub const fn is_pageable(self) -> bool {
        matches!(self, Self::Fatal)
    }
}

/// A stable, documented error code.
///
/// Codes are permanent. Removing or renumbering one breaks every runbook, alert rule
/// and support script that references it, so the catalogue is snapshot-tested and a
/// change to it is a visible diff rather than a customer's discovery.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Code(&'static str);

impl Code {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Anything that can be classified and reported.
pub trait Classify {
    fn class(&self) -> Class;
    fn code(&self) -> Code;
    /// What an operator should actually do. Rendered into the published catalogue, so
    /// documentation and code cannot drift apart.
    fn remediation(&self) -> &'static str;
}

macro_rules! catalogue {
    ($( $variant:ident => ($code:literal, $class:expr, $message:literal, $remedy:literal) ),+ $(,)?) => {
        /// The error catalogue.
        #[derive(Clone, Debug, thiserror::Error)]
        #[non_exhaustive]
        pub enum Error {
            $(
                #[doc = $message]
                #[error("[{code}] {message}{}", detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default(), code = $code, message = $message)]
                $variant { detail: Option<String> },
            )+
        }

        impl Error {
            $(
                #[allow(non_snake_case)]
                #[must_use]
                pub fn $variant(detail: impl Into<String>) -> Self {
                    Self::$variant { detail: Some(detail.into()) }
                }
            )+

            /// Every variant, for the exhaustiveness test and the generated catalogue.
            #[must_use]
            pub fn all() -> Vec<Error> {
                vec![ $( Error::$variant { detail: None } ),+ ]
            }
        }

        impl Classify for Error {
            fn class(&self) -> Class {
                match self { $( Self::$variant { .. } => $class ),+ }
            }
            fn code(&self) -> Code {
                match self { $( Self::$variant { .. } => Code($code) ),+ }
            }
            fn remediation(&self) -> &'static str {
                match self { $( Self::$variant { .. } => $remedy ),+ }
            }
        }
    };
}

catalogue! {
    // --- client ---------------------------------------------------------------
    InvalidQuery => ("SNK-C0001", Class::User,
        "the query is malformed or references something that does not exist",
        "Correct the statement. The detail names the offending element."),
    UnsupportedType => ("SNK-C0002", Class::User,
        "a source column type has no faithful representation",
        "Exclude the column, or convert it in the source. SANKHYA refuses an approximate \
         mapping because a silently lossy column cannot be reconciled afterwards."),
    ApproximateNotPermitted => ("SNK-C0003", Class::User,
        "an approximate aggregate was used where exactness is required",
        "Use the exact equivalent named in the detail, or clear the exactness \
         requirement for this session if approximation is genuinely acceptable."),
    ArchivedRangeImmutable => ("SNK-C0004", Class::User,
        "the statement targets a range that has been archived",
        "Archived data is immutable. Record a compensating entry in the live tier, or \
         rehydrate the range read-only for inspection."),
    NotSupported => ("SNK-C0006", Class::User,
        "the statement uses a feature this build does not implement",
        "The detail names the construct. It is refused rather than approximated: a \
         statement that silently means something slightly different from what it says is \
         worse than one that is rejected."),
    StatementFailed => ("SNK-C0007", Class::User,
        "the statement failed during execution",
        "The detail names what failed --- usually a cast, a division, or a value outside \
         the range of its type. If the statement should have worked, this is worth \
         reporting with the detail attached."),
    NamingCollision => ("SNK-C0005", Class::User,
        "two distinct source identifiers map to the same storage path",
        "Rename one in the source, declare an explicit mapping, or exclude one. \
         SANKHYA refuses to disambiguate automatically because a generated suffix \
         destroys the naming relationship it exists to preserve."),

    // --- resource -------------------------------------------------------------
    AdmissionRejected => ("SNK-R0001", Class::Resource,
        "the query was refused because its estimated cost exceeds available capacity",
        "Retry when load falls, narrow the predicate, or raise the tenant's limit. \
         Refusal is deliberate: admitting it would risk terminating the process."),
    QuotaExceeded => ("SNK-R0002", Class::Resource,
        "a tenant quota was reached",
        "The detail names the quota. Raise it or reduce consumption."),
    ArrivalBufferFull => ("SNK-R0003", Class::Resource,
        "the arrival buffer has no room",
        "Capture is ahead of publication. The system is already lengthening its commit \
         interval; if this persists, publication throughput is the bottleneck."),

    // --- conflict -------------------------------------------------------------
    CommitConflict => ("SNK-F0001", Class::Conflict,
        "a concurrent writer committed first",
        "Re-plan against the new snapshot and retry. Maintenance yields to the applier; \
         the applier never yields."),

    // --- retryable ------------------------------------------------------------
    StorageUnavailable => ("SNK-T0001", Class::Retryable { after: Some(Duration::from_millis(250)) },
        "object storage is unreachable or returned a transient failure",
        "Retried automatically within the request deadline. Persistent failure \
         indicates a storage or credential problem."),
    SourceUnavailable => ("SNK-T0002", Class::Retryable { after: Some(Duration::from_secs(1)) },
        "the transactional store is unreachable",
        "Check the database is running and reachable. In managed mode the supervisor \
         restarts it with backoff."),
    StaleData => ("SNK-T0003", Class::Retryable { after: Some(Duration::from_millis(500)) },
        "the requested freshness could not be met within the deadline",
        "Retry, relax the freshness requirement, or investigate capture lag. Returning \
         stale data silently would be worse than failing."),

    // --- cancelled ------------------------------------------------------------
    Cancelled => ("SNK-X0001", Class::Cancelled,
        "the work was cancelled",
        "No action. The deadline expired, the client disconnected, or the server is \
         draining."),

    // --- fatal ----------------------------------------------------------------
    CoverageGap => ("SNK-S0001", Class::Fatal,
        "no tier covers part of the requested range",
        "A correctness event, not a performance one. The query was refused rather than \
         answered partially. Investigate capture continuity and retention immediately."),
    ArchiveConflict => ("SNK-S0002", Class::Fatal,
        "the archival registry disagrees with the live catalogue",
        "Usually a restore that resurrected purged rows. Queries on the affected table \
         are refused until an operator re-purges or re-adopts the range."),
    VerificationFailed => ("SNK-S0003", Class::Fatal,
        "archive verification did not match",
        "Terminal until a human acts. There is no automatic retry: a mismatch means a \
         defect exists, and retrying would be the wrong response."),
    SourceEndangered => ("SNK-S0004", Class::Fatal,
        "continuing would threaten the availability of the transactional store",
        "Retained log has reached its limit. The analytical tier is being sacrificed to \
         protect the source. A gap marker is recorded and re-snapshot begins \
         automatically."),
    InvariantViolated => ("SNK-S0005", Class::Fatal,
        "an internal invariant does not hold",
        "A defect. Capture a diagnostic bundle and report it. The detail names the \
         invariant."),
    ConfigInvalid => ("SNK-S0006", Class::Fatal,
        "configuration is not valid",
        "The detail names the key, the value and its origin. An unknown key is an error \
         rather than a warning, because silently ignored typos are a leading cause of \
         production incidents."),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
