//! `sank_data_date`: the one date axis every table carries.
//!
//! See ADR-0004. Three capabilities — partitioning, time-based retention, and the hot/cold
//! tiering axis — are each blocked on the same missing thing: a column every table is
//! guaranteed to have and every one of them can agree on.
//!
//! # The rule that shapes everything here
//!
//! The value is **declared per table, never defaulted per row**.
//!
//! The obvious design is "use the supplied value, else today". It is rejected, and the
//! reason is not that a default is imprecise. It is that a default taken from write time
//! makes the column mean different things in different rows *of the same table*, with
//! nothing recording which. A backfill of last year's data lands in today's partition. And
//! then `WHERE sank_data_date = '2024-03-01'` returns a mixture of rows meaning "this
//! happened that day" and rows meaning "we received this that day" — inseparable
//! afterwards, because the distinction was never written down.
//!
//! So a table declares where its date comes from, once. If a source column is named, every
//! row uses it and a null there is an **error**. If none is named, every row uses the ingest
//! date and *the table records that it did*, so a reader can always find out what the column
//! means.

use std::collections::BTreeMap;
use std::fmt;

/// The column every table carries.
pub const DATA_DATE_COLUMN: &str = "sank_data_date";

/// The prefix this system reserves for its own columns.
///
/// A source column already so named is a collision refused at onboarding, not silently
/// shadowed — a shadowed column means the user's data disappears behind a system value
/// with no error anywhere.
pub const RESERVED_PREFIX: &str = "sank_";

/// The configuration key naming the column a table's date comes from.
pub const SOURCE_KEY: &str = "sank.dataDate.source";

/// The configuration key naming how coarsely a table is partitioned.
pub const GRANULARITY_KEY: &str = "sank.dataDate.granularity";

/// How coarsely a table is partitioned in time.
///
/// Declarable because a fixed daily granularity on a low-volume table produces 365 small
/// files a year — the small-file problem compaction exists to fix, created deliberately.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Granularity {
    /// One partition per day. The default, and right for anything high-volume.
    #[default]
    Day,
    /// One per month.
    Month,
    /// One per year, for the long tail.
    Year,
}

impl Granularity {
    /// The name written into the log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    /// The granularity a name denotes, or `None` if it is not one.
    ///
    /// Returns `None` rather than defaulting, so a misspelling is caught at load. Silently
    /// falling back to daily would repartition a monthly table on the next write, which
    /// rewrites the whole thing for a typo.
    #[must_use]
    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "day" => Some(Self::Day),
            "month" => Some(Self::Month),
            "year" => Some(Self::Year),
            _ => None,
        }
    }
}

impl fmt::Display for Granularity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a table's date comes from.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum DateSource {
    /// From a named column of the source data.
    ///
    /// Every row uses it. A null there is an error rather than a fallback, because a
    /// fallback reintroduces the mixture one row at a time.
    Column {
        /// Which column.
        name: String,
    },
    /// From the moment this system ingested the row.
    ///
    /// Recorded explicitly rather than being the absence of a declaration, so a reader can
    /// tell "this column means arrival" from "nobody thought about it".
    #[default]
    IngestDate,
}

impl DateSource {
    /// Whether the date describes what the data is *about*.
    ///
    /// False for ingest date, which describes when this system heard about it. The
    /// distinction matters to anyone doing time-bounded analysis, and this is how they ask.
    #[must_use]
    pub const fn is_business_date(&self) -> bool {
        matches!(self, Self::Column { .. })
    }

    /// A sentence for whoever reads a table's description.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Column { name } => {
                format!("the date each row is about, from the source column '{name}'")
            }
            Self::IngestDate => "the date this system received each row, not the date the \
                                 row is about"
                .to_string(),
        }
    }
}

/// A table's date axis: where the date comes from and how coarsely it partitions.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DateAxis {
    /// Where the value comes from.
    pub source: DateSource,
    /// How coarsely it partitions.
    pub granularity: Granularity,
}

impl DateAxis {
    /// An axis taking its date from a source column.
    #[must_use]
    pub fn from_column(name: impl Into<String>) -> Self {
        Self {
            source: DateSource::Column { name: name.into() },
            granularity: Granularity::Day,
        }
    }

    /// An axis using the ingest date, said out loud.
    #[must_use]
    pub fn ingest_date() -> Self {
        Self {
            source: DateSource::IngestDate,
            granularity: Granularity::Day,
        }
    }

    /// The same axis at a different granularity.
    #[must_use]
    pub const fn at(mut self, granularity: Granularity) -> Self {
        self.granularity = granularity;
        self
    }

    /// The configuration a table declares this axis with.
    #[must_use]
    pub fn to_configuration(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        if let DateSource::Column { name } = &self.source {
            out.insert(SOURCE_KEY.to_string(), name.clone());
        }
        out.insert(
            GRANULARITY_KEY.to_string(),
            self.granularity.as_str().to_string(),
        );
        out
    }

    /// The axis a configuration declares.
    ///
    /// # Errors
    ///
    /// Refuses an unrecognised granularity rather than defaulting to daily. Silently
    /// falling back would repartition a monthly table on its next write — a full rewrite,
    /// for a typo.
    pub fn from_configuration(configuration: &BTreeMap<String, String>) -> Result<Self, AxisError> {
        let granularity = match configuration.get(GRANULARITY_KEY) {
            None => Granularity::Day,
            Some(name) => {
                Granularity::from_str(name).ok_or_else(|| AxisError::UnknownGranularity {
                    offered: name.clone(),
                })?
            }
        };
        let source = match configuration.get(SOURCE_KEY) {
            Some(name) if !name.is_empty() => DateSource::Column { name: name.clone() },
            _ => DateSource::IngestDate,
        };
        Ok(Self {
            source,
            granularity,
        })
    }

    /// The partition value for a date, at this granularity.
    ///
    /// Truncation, not rounding: a row dated the 20th of March belongs to March, never to
    /// April. Rounding would put the second half of every period in the next one, which is
    /// wrong in a way that only shows up at period boundaries.
    #[must_use]
    pub fn partition_of(&self, days_since_epoch: i32) -> String {
        let (year, month, day) = civil_from_days(days_since_epoch);
        match self.granularity {
            Granularity::Day => format!("{year:04}-{month:02}-{day:02}"),
            Granularity::Month => format!("{year:04}-{month:02}"),
            Granularity::Year => format!("{year:04}"),
        }
    }

    /// The partition path component, as external engines expect it.
    ///
    /// `sank_data_date=2024-03-01`, which is the Hive convention Spark and Trino parse as a
    /// date without being told the encoding. `CON-08` requires those engines to read these
    /// tables, and an encoding they have to be told about is one every pruning query can
    /// forget.
    #[must_use]
    pub fn partition_path(&self, days_since_epoch: i32) -> String {
        format!("{DATA_DATE_COLUMN}={}", self.partition_of(days_since_epoch))
    }
}

/// Convert days since the Unix epoch into a civil date.
///
/// Howard Hinnant's algorithm, which is exact for the whole representable range and does
/// not go through a floating-point step. Written out rather than pulled from a dependency
/// because a partition key computed slightly differently by two components is a table that
/// splits in half.
#[must_use]
pub const fn civil_from_days(days: i32) -> (i32, u32, u32) {
    // Shift the era so that the leap-day irregularity falls at the end of a cycle.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month, day)
}

/// Whether a column name is reserved by this system.
///
/// A source column so named is a collision, refused at onboarding rather than shadowed. A
/// shadowed column means the user's data disappears behind a system value, with no error
/// anywhere and no way to notice except by missing it.
#[must_use]
pub fn is_reserved(column: &str) -> bool {
    column.starts_with(RESERVED_PREFIX)
}

/// Why a date axis could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AxisError {
    /// The declared granularity is not one this system knows.
    UnknownGranularity {
        /// What was declared.
        offered: String,
    },
    /// The declared source column is not in the table's schema.
    NoSuchSourceColumn {
        /// What was declared.
        column: String,
        /// What the schema has.
        available: Vec<String>,
    },
    /// A source column has a null where a date is required.
    NullDate {
        /// Which column.
        column: String,
    },
    /// A source column already uses the reserved prefix.
    ReservedName {
        /// The offending name.
        column: String,
    },
}

impl fmt::Display for AxisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownGranularity { offered } => write!(
                f,
                "'{offered}' is not a partition granularity; the choices are day, month and \
                 year. Refusing rather than defaulting to daily: a monthly table that \
                 silently became daily would be repartitioned on its next write, which \
                 rewrites the whole table for a typo"
            ),
            Self::NoSuchSourceColumn { column, available } => write!(
                f,
                "the date column '{column}' is not in the schema, which has {available:?}"
            ),
            Self::NullDate { column } => write!(
                f,
                "a row has no value in '{column}', which this table declares as its date. \
                 Refusing rather than substituting today: a per-row fallback makes the \
                 column mean 'when it happened' in some rows and 'when we received it' in \
                 others, inseparably"
            ),
            Self::ReservedName { column } => write!(
                f,
                "'{column}' uses the reserved '{RESERVED_PREFIX}' prefix. Refusing rather \
                 than shadowing it: a shadowed column means the source's data disappears \
                 behind a system value, with no error anywhere"
            ),
        }
    }
}

impl std::error::Error for AxisError {}
