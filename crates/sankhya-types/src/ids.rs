//! Identifier newtypes.
//!
//! Every identifier is distinct in the type system. Passing a tenant where a table
//! was expected is a compile error rather than a support ticket.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$m:meta])* $name:ident, $prefix:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{self}")
            }
        }
    };
}

uuid_id!(
    /// A tenant. Present on every internal interface from the first commit, even
    /// where enforcement lands later — retrofitting tenancy is the classic tax.
    TenantId, "tenant:"
);
uuid_id!(
    /// A table's stable identity, independent of its name.
    ///
    /// Ingest is keyed by this rather than by name, which is what allows a table
    /// rename to pause *publication* without pausing *capture*. Nothing backs up and
    /// no log pressure accumulates while an operator decides how to resolve it.
    TableId, "table:"
);
uuid_id!(
    /// A single query, for cancellation, accounting and audit correlation.
    QueryId, "query:"
);

/// A dense internal vertex index.
///
/// 32 bits is a deliberate, documented bound: it keeps adjacency arrays cache-friendly,
/// which is the whole reason the graph uses a compressed sparse layout. The ceiling is
/// published in the capacity model rather than discovered.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(u32);

impl NodeId {
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A dense internal edge index.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EdgeId(u32);

impl EdgeId {
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A schema name as it appears in the source, preserved byte-exactly.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaName(String);

/// A table name as it appears in the source, preserved byte-exactly.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TableName(String);

macro_rules! name_type {
    ($name:ident) => {
        impl $name {
            #[must_use]
            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(SchemaName);
name_type!(TableName);
