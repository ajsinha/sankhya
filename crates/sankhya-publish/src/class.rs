//! What kind of table this is, written where it cannot be forgotten.
//!
//! A table's class is a fact about the table, so it lives in the table's own log rather than
//! in any server's configuration. Two nodes reading one warehouse cannot then disagree, a
//! restart cannot forget, and a publisher can declare itself without asking anybody.
//!
//! # Why absence means external
//!
//! A directory somebody dropped files into is, by construction, not managed by this system.
//! Defaulting the other way would have such a table claim a transactional tier it does not
//! have --- and the first strongly-consistent read against it would return published-only
//! data while asserting currency, with nothing in the result to say so.
//!
//! The conservative default is the one that under-claims. This one does.

use std::collections::BTreeMap;
use std::fmt;

/// The configuration key a table's class is written under.
pub const CLASS_KEY: &str = "sankhya.tableClass";

/// The configuration key a mutable table's identifying columns are written under.
pub const KEY_COLUMNS_KEY: &str = "sankhya.keyColumns";

/// Which kind of table this is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TableClass {
    /// Published directly by a writer outside this system, and read-only to it.
    ///
    /// The default, and deliberately so.
    #[default]
    External,
    /// This system's transactional store is the writer of record.
    Managed,
}

impl TableClass {
    /// The string written into the log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Managed => "managed",
        }
    }

    /// The class a configuration map declares.
    ///
    /// An **unrecognised** value is external, like an absent one. A future version of this
    /// system might add a class this one does not know, and treating an unknown class as
    /// managed would grant it guarantees nobody here can honour.
    #[must_use]
    pub fn from_configuration(configuration: &BTreeMap<String, String>) -> Self {
        match configuration.get(CLASS_KEY).map(String::as_str) {
            Some("managed") => Self::Managed,
            _ => Self::External,
        }
    }

    /// Whether this system may write to a table of this class.
    #[must_use]
    pub const fn is_writable_here(self) -> bool {
        matches!(self, Self::Managed)
    }

    /// Whether a strongly-consistent read of this table can mean anything.
    ///
    /// False for an external table, which has no transactional tier to read. Such a request
    /// is refused rather than served from published data, because serving it would assert a
    /// currency the table cannot offer.
    #[must_use]
    pub const fn supports_strong_reads(self) -> bool {
        matches!(self, Self::Managed)
    }
}

impl fmt::Display for TableClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The configuration a table of this class and key should carry.
#[must_use]
pub fn configuration(class: TableClass, key_columns: &[String]) -> BTreeMap<String, String> {
    let mut configuration = BTreeMap::new();
    configuration.insert(CLASS_KEY.to_string(), class.as_str().to_string());
    if !key_columns.is_empty() {
        // Comma-separated, and the names are validated at publication time so a comma
        // inside one cannot be produced here.
        configuration.insert(KEY_COLUMNS_KEY.to_string(), key_columns.join(","));
    }
    configuration
}

/// The identifying columns a configuration declares, if any.
///
/// A table with key columns is mutable: the latest version of each key wins. A table
/// without them is append-only. Nothing infers this --- a table whose key is guessed
/// resolves distinct rows into one and loses the rest, silently, in a way that looks like
/// deduplication working.
#[must_use]
pub fn key_columns(configuration: &BTreeMap<String, String>) -> Vec<String> {
    configuration
        .get(KEY_COLUMNS_KEY)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
