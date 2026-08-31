//! Moving a whole table to the published tier without the table going away.
//!
//! # The requirement is about what a table's *name* is worth
//!
//! `FR-TIER-21`: after a whole-table migration the table remains visible in the catalog under
//! **the same name**, backed by the published tier, marked cold and read-only --- because *"a
//! table that vanishes breaks every downstream tool and saved query"*.
//!
//! That is the whole of it, and it is easy to underrate. A table nobody has written to for four
//! years is still named in dashboards, in a report somebody runs each quarter, in a view three
//! other views are built on, and in the query a person pastes from a wiki page. Migrating it and
//! dropping the name turns one storage decision into a morning of unrelated failures in places
//! nobody connected to tiering.
//!
//! So a migration here produces a [`Cold`] table rather than removing anything, and `Cold`
//! carries the original name because its constructor is given one name and uses it for both
//! sides. A migration that renamed the table would have to be written on purpose.
//!
//! # Why a migration is refused unless the archive covers everything
//!
//! The table stays visible. That is the requirement, and it is also the trap: a visible table
//! whose archive covers four of its five years answers four years of questions without
//! mentioning the fifth. `FR-TIER-17` calls the general form of that a coverage gap and makes it
//! an error, and the same rule applies here one step earlier --- before the migration rather
//! than at each query.
//!
//! So [`migrate`] asks the registry to cover the table's whole declared key domain and refuses
//! with every hole it finds. The check reuses [`Registry::coverage`] rather than reimplementing
//! it, so the definition of *covered* cannot drift between the two places that care.

use crate::registry::{Range, Registry};
use std::fmt;

/// A table now backed entirely by the published tier.
///
/// Has no constructor other than [`migrate`], so holding one is evidence the coverage check
/// passed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cold {
    name: String,
    domain: Range,
    archives: Vec<String>,
    at: i64,
}

impl Cold {
    /// The name it had before, which is the name it still has.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The key domain it covers.
    #[must_use]
    pub const fn domain(&self) -> Range {
        self.domain
    }

    /// The archives backing it, in key order.
    #[must_use]
    pub fn archives(&self) -> &[String] {
        &self.archives
    }

    /// When it was migrated, in microseconds from the epoch.
    #[must_use]
    pub const fn at(&self) -> i64 {
        self.at
    }

    /// Whether it accepts writes.
    ///
    /// Always `false`, and a method rather than a constant so a caller reads it from the table
    /// rather than remembering it. There is no path that sets it: `FR-TIER-21` says cold and
    /// read-only, and a migrated table with a writable state would be a table whose rows are in
    /// an immutable archive and whose catalog says otherwise.
    #[must_use]
    pub const fn writable(&self) -> bool {
        false
    }
}

impl fmt::Display for Cold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is cold and read-only over {}, backed by {} archive(s) since {}",
            self.name,
            self.domain,
            self.archives.len(),
            self.at
        )
    }
}

/// Why a table could not be migrated whole.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Incomplete {
    /// Part of the declared key domain is not archived.
    NotWhollyArchived {
        /// The table.
        table: String,
        /// Every uncovered sub-range, not the first.
        gaps: Vec<Range>,
    },
    /// The declared key domain covers nothing.
    EmptyDomain {
        /// The table.
        table: String,
    },
}

impl fmt::Display for Incomplete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWhollyArchived { table, gaps } => {
                write!(f, "`{table}` is not wholly archived --- nothing covers ")?;
                for (index, gap) in gaps.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{gap}")?;
                }
                f.write_str(
                    ". Migrating it would leave a table that is still visible, as it must be, \
                     and answers questions about the archived years without mentioning the rest",
                )
            }
            Self::EmptyDomain { table } => write!(
                f,
                "`{table}` declares a key domain covering nothing, so `wholly archived` is a \
                 claim about no rows and would be satisfied by an empty registry"
            ),
        }
    }
}

/// Migrate a table whole, or say what is missing.
///
/// # Errors
///
/// [`Incomplete::NotWhollyArchived`] listing every uncovered sub-range, and
/// [`Incomplete::EmptyDomain`] when the declared domain covers nothing.
pub fn migrate(
    registry: &Registry,
    table: &str,
    domain: Range,
    at: i64,
) -> Result<Cold, Incomplete> {
    if domain.is_empty() {
        return Err(Incomplete::EmptyDomain { table: table.to_string() });
    }

    let coverage = registry.coverage(table, domain);
    if !coverage.is_complete() {
        return Err(Incomplete::NotWhollyArchived {
            table: table.to_string(),
            gaps: coverage.gaps,
        });
    }

    Ok(Cold {
        name: table.to_string(),
        domain,
        archives: coverage.archives.into_iter().map(|entry| entry.archive).collect(),
        at,
    })
}
