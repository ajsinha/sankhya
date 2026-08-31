//! What a clone records about where it came from.
//!
//! # Why this is a table property and not a log action
//!
//! The obvious home is a new action in the table log beside `add` and `remove`. It is the wrong
//! one. This project's open-storage claim is not a slogan --- there is a test asserting the
//! Delta kernel reads these logs --- and it is kept by writing only what the format defines.
//! An action nobody else knows is a bet that every reader ignores what it does not recognise,
//! and losing that bet turns an open table into one this software can read.
//!
//! `Metadata.configuration` is the place the format sets aside for exactly this: a string map of
//! table properties, carried through by every reader, meaningful to the ones that care. So a
//! clone's lineage is three entries under a `sankhya.clone.` prefix, and a foreign reader sees a
//! table with some properties it does not use.
//!
//! # Why the version is recorded and not inferred
//!
//! A clone reads what its origin read **at a version**. That version is the whole content of the
//! statement: without it a lineage says only that two tables are related, which is not enough to
//! answer *"what should this clone contain?"* years later, and not enough for the audit chain to
//! record which data a decision saw.

use std::collections::BTreeMap;
use std::fmt;

/// The property naming a clone's origin.
pub const ORIGIN: &str = "sankhya.clone.origin";
/// The property naming the origin version a clone was taken at.
pub const VERSION: &str = "sankhya.clone.version";
/// The property recording when the clone was made.
pub const CLONED_AT: &str = "sankhya.clone.at";

/// Where a clone came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Lineage {
    /// The table it was cloned from.
    pub origin: String,
    /// The origin version it reads.
    pub version: u64,
    /// When, in microseconds from the epoch.
    pub cloned_at: i64,
}

impl Lineage {
    /// A lineage.
    pub fn new(origin: impl Into<String>, version: u64, cloned_at: i64) -> Self {
        Self { origin: origin.into(), version, cloned_at }
    }

    /// The table properties that record it.
    #[must_use]
    pub fn to_properties(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (ORIGIN.to_string(), self.origin.clone()),
            (VERSION.to_string(), self.version.to_string()),
            (CLONED_AT.to_string(), self.cloned_at.to_string()),
        ])
    }

    /// Read a lineage from a table's properties.
    ///
    /// `None` when the table is not a clone, which is the ordinary case and not a failure ---
    /// every table that exists today answers this way.
    ///
    /// # Errors
    ///
    /// [`Malformed`] when the properties claim a clone and do not describe one. **Not treated as
    /// "not a clone"**, which is the distinction that matters: a table whose lineage cannot be
    /// read is a table whose origin's sweeper cannot know it exists, and answering `None` there
    /// would be answering *"nothing else reads these files"* on no evidence.
    pub fn from_properties(
        properties: &BTreeMap<String, String>,
    ) -> Option<Result<Self, Malformed>> {
        let claims = properties.keys().any(|key| key.starts_with("sankhya.clone."));
        if !claims {
            return None;
        }

        let Some(origin) = properties.get(ORIGIN).filter(|origin| !origin.trim().is_empty())
        else {
            return Some(Err(Malformed::NoOrigin));
        };
        let Some(version) = properties.get(VERSION) else {
            return Some(Err(Malformed::NoVersion));
        };
        let Ok(version) = version.parse::<u64>() else {
            return Some(Err(Malformed::UnreadableVersion { found: version.clone() }));
        };
        let cloned_at = properties
            .get(CLONED_AT)
            .and_then(|at| at.parse::<i64>().ok())
            .unwrap_or_default();

        Some(Ok(Self { origin: origin.clone(), version, cloned_at }))
    }
}

impl fmt::Display for Lineage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cloned from `{}` at version {}", self.origin, self.version)
    }
}

/// Why a table's lineage could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Malformed {
    /// It claims to be a clone and names no origin.
    NoOrigin,
    /// It names an origin and no version.
    NoVersion,
    /// Its version is not a number.
    UnreadableVersion {
        /// What was there.
        found: String,
    },
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = match self {
            Self::NoOrigin => "it carries clone properties and names no origin".to_string(),
            Self::NoVersion => "it names an origin and no version, so what it should contain \
                                is not a question with an answer"
                .to_string(),
            Self::UnreadableVersion { found } => {
                format!("its origin version is `{found}`, which is not a version")
            }
        };
        write!(
            f,
            "this table's lineage cannot be read: {detail}. It is not therefore treated as an \
             ordinary table --- a clone whose lineage is unreadable is one its origin's sweeper \
             cannot know about, and reclaiming on that basis is the loss cloning is gated on"
        )
    }
}
