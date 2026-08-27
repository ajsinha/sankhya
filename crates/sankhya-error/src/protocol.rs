//! Mapping the error catalogue onto the protocols clients speak.
//!
//! # Why this is a table and not a formatting concern
//!
//! A client's *behaviour* is driven by the protocol status, not by the message. A gRPC
//! client retries `UNAVAILABLE` and gives up on `INVALID_ARGUMENT`; a PostgreSQL driver
//! reconnects on `57P01` and surfaces `42501` to the user. Get the mapping wrong and a
//! well-written client does exactly the wrong thing --- retries something that can never
//! succeed, or abandons something that would have worked on the next attempt.
//!
//! So the mapping is derived from [`Class`], which already encodes how a caller should
//! react, rather than being chosen per error site. One decision per class, made once.
//!
//! # SQLSTATE is not optional
//!
//! `FR-API-02` calls the wire-protocol front door the highest-adoption-value surface in the
//! product, and every driver in that ecosystem branches on SQLSTATE. Returning a plausible
//! message with the wrong five characters produces a client that connects, appears to work,
//! and mishandles every failure.

use crate::{Class, Code};
use std::fmt;

/// A gRPC status code.
///
/// Named rather than numbered at the call site, and carrying its number for the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrpcStatus {
    /// The request was malformed or nonsensical.
    InvalidArgument,
    /// The caller may not do this.
    PermissionDenied,
    /// A limit was reached.
    ResourceExhausted,
    /// Concurrent modification; re-plan and retry.
    Aborted,
    /// Temporarily unable; retrying may work.
    Unavailable,
    /// The deadline passed or the caller went away.
    Cancelled,
    /// The deadline passed specifically.
    DeadlineExceeded,
    /// Something is broken.
    Internal,
}

impl GrpcStatus {
    /// The numeric code that goes on the wire.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Self::Cancelled => 1,
            Self::InvalidArgument => 3,
            Self::DeadlineExceeded => 4,
            Self::PermissionDenied => 7,
            Self::ResourceExhausted => 8,
            Self::Aborted => 10,
            Self::Internal => 13,
            Self::Unavailable => 14,
        }
    }

    /// The name the gRPC specification gives it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cancelled => "CANCELLED",
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::DeadlineExceeded => "DEADLINE_EXCEEDED",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::ResourceExhausted => "RESOURCE_EXHAUSTED",
            Self::Aborted => "ABORTED",
            Self::Internal => "INTERNAL",
            Self::Unavailable => "UNAVAILABLE",
        }
    }
}

impl fmt::Display for GrpcStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A five-character SQLSTATE.
///
/// Every driver in the PostgreSQL ecosystem branches on these, so they are chosen from the
/// standard classes rather than invented. An invented SQLSTATE is worse than a wrong one:
/// a driver that does not recognise the class falls back to treating it as a generic
/// failure, and the fallback is usually "do not retry".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SqlState(&'static str);

impl SqlState {
    /// The five characters.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// The two-character class, which is what most drivers actually branch on.
    #[must_use]
    pub fn class(self) -> &'static str {
        self.0.get(..2).unwrap_or("XX")
    }
}

impl fmt::Display for SqlState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Standard SQLSTATEs this server returns.
pub mod sqlstate {
    use super::SqlState;

    /// `22000` — data exception. The request named something impossible.
    pub const DATA_EXCEPTION: SqlState = SqlState("22000");
    /// `28000` — invalid authorization specification.
    pub const INVALID_AUTHORIZATION: SqlState = SqlState("28000");
    /// `40001` — serialization failure. Re-plan and retry.
    pub const SERIALIZATION_FAILURE: SqlState = SqlState("40001");
    /// `42501` — insufficient privilege.
    pub const INSUFFICIENT_PRIVILEGE: SqlState = SqlState("42501");
    /// `42601` — syntax error.
    pub const SYNTAX_ERROR: SqlState = SqlState("42601");
    /// `53000` — insufficient resources.
    pub const INSUFFICIENT_RESOURCES: SqlState = SqlState("53000");
    /// `53400` — configuration limit exceeded.
    pub const CONFIGURATION_LIMIT_EXCEEDED: SqlState = SqlState("53400");
    /// `57014` — query cancelled.
    pub const QUERY_CANCELED: SqlState = SqlState("57014");
    /// `58030` — I/O error.
    pub const IO_ERROR: SqlState = SqlState("58030");
    /// `XX000` — internal error.
    pub const INTERNAL_ERROR: SqlState = SqlState("XX000");
}

/// How an error appears on each wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProtocolStatus {
    /// The gRPC status.
    pub grpc: GrpcStatus,
    /// The SQLSTATE.
    pub sqlstate: SqlState,
    /// The HTTP status.
    pub http: u16,
}

impl ProtocolStatus {
    /// Whether a well-behaved client seeing this should retry unchanged.
    ///
    /// Derived from the same class the statuses are, so the three wires cannot disagree
    /// about it. A gRPC client retrying while a SQL client gives up, for the same
    /// condition, is a bug nobody finds until it matters.
    #[must_use]
    pub const fn client_should_retry(self) -> bool {
        matches!(self.grpc, GrpcStatus::Unavailable)
    }
}

/// The protocol statuses for a class of error.
///
/// One decision per class, made here. Choosing per error site is how a system ends up with
/// two errors that mean the same thing and behave differently.
#[must_use]
pub const fn statuses_for(class: Class) -> ProtocolStatus {
    match class {
        Class::User => ProtocolStatus {
            grpc: GrpcStatus::InvalidArgument,
            sqlstate: sqlstate::SYNTAX_ERROR,
            http: 400,
        },
        Class::Retryable { .. } => ProtocolStatus {
            grpc: GrpcStatus::Unavailable,
            sqlstate: sqlstate::IO_ERROR,
            http: 503,
        },
        Class::Conflict => ProtocolStatus {
            // `Aborted` rather than `Unavailable`: the gRPC specification says Aborted
            // means the caller should retry at a higher level, which is exactly right for
            // a conflict — the same plan will lose again, a new one may not.
            grpc: GrpcStatus::Aborted,
            sqlstate: sqlstate::SERIALIZATION_FAILURE,
            http: 409,
        },
        Class::Resource => ProtocolStatus {
            grpc: GrpcStatus::ResourceExhausted,
            sqlstate: sqlstate::CONFIGURATION_LIMIT_EXCEEDED,
            http: 429,
        },
        Class::Cancelled => ProtocolStatus {
            grpc: GrpcStatus::Cancelled,
            sqlstate: sqlstate::QUERY_CANCELED,
            http: 499,
        },
        Class::Fatal => ProtocolStatus {
            grpc: GrpcStatus::Internal,
            sqlstate: sqlstate::INTERNAL_ERROR,
            http: 500,
        },
    }
}

/// The statuses for a permission refusal.
///
/// Its own function rather than a `Class` variant, because a refusal is a `User` error in
/// every respect except the status it must carry --- and a driver that receives `42601`
/// for a permission problem will report a syntax error to a confused user.
#[must_use]
pub const fn statuses_for_denied() -> ProtocolStatus {
    ProtocolStatus {
        grpc: GrpcStatus::PermissionDenied,
        sqlstate: sqlstate::INSUFFICIENT_PRIVILEGE,
        http: 403,
    }
}

/// The statuses for a failed authentication.
#[must_use]
pub const fn statuses_for_unauthenticated() -> ProtocolStatus {
    ProtocolStatus {
        grpc: GrpcStatus::PermissionDenied,
        sqlstate: sqlstate::INVALID_AUTHORIZATION,
        http: 401,
    }
}

/// An error as it goes on the wire.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WireError {
    /// The stable catalogue code.
    pub code: Code,
    /// How it appears on each protocol.
    pub status: ProtocolStatus,
    /// What happened.
    pub message: String,
    /// What to do about it.
    pub remediation: &'static str,
}

impl WireError {
    /// Build the wire form of a classified error.
    #[must_use]
    pub fn of(item: &dyn crate::Classify, message: impl Into<String>) -> Self {
        Self {
            code: item.code(),
            status: statuses_for(item.class()),
            message: message.into(),
            remediation: item.remediation(),
        }
    }

    /// The same error, carrying a permission-denied status instead of its class's.
    #[must_use]
    pub const fn denied(mut self) -> Self {
        self.status = statuses_for_denied();
        self
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}/{}] {}",
            self.code, self.status.sqlstate, self.message
        )
    }
}
