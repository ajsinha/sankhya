//! What a feed document says, before anybody has checked whether it makes sense.

use serde::{Deserialize, Serialize};

/// A feed, as written down.
///
/// Deserialized from a document and **not usable**: everything downstream takes a
/// [`Feed`](crate::validate::Feed), which only [`validate`](crate::validate::validate)
/// produces. The split is deliberate --- a type that can hold an invalid declaration is
/// exactly what makes "did anybody check this?" unanswerable at a call site.
#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
pub struct Declaration {
    /// What this feed is called. Appears in metrics, in quarantined records, and in the
    /// refusal when it stops.
    pub name: String,
    /// The directory documents arrive in.
    pub from: String,
    /// The schema the table lives in.
    pub schema: String,
    /// The table rows land in.
    pub table: String,
    /// The columns, and where each one's value comes from.
    #[serde(default)]
    pub columns: Vec<Column>,
    /// Where each row's `sank_data_date` comes from.
    ///
    /// An `Option` so that saying nothing is refused *here*, with a sentence, rather than by
    /// a deserializer reporting a missing field. `DEC-34` requires the date to be declared
    /// per table and never defaulted, and a feed is where a table's rows come from.
    #[serde(default)]
    pub date: Option<DateFrom>,
    /// What to do with a key no column claims.
    #[serde(default)]
    pub unknown: Unknown,
    /// When a batch is closed.
    #[serde(default)]
    pub microbatch: Microbatch,
    /// What happens to records that do not fit.
    #[serde(default)]
    pub quarantine: Quarantine,
}

impl Declaration {
    /// Read a declaration from the document somebody wrote.
    ///
    /// Here rather than in whoever loads the file, so that one crate decides what a feed
    /// document *is*. A second parser elsewhere would be a second answer to what an unknown
    /// field means, and they would drift.
    ///
    /// # Errors
    ///
    /// The parse error, naming the line. A declaration that does not parse is not validated
    /// --- there is nothing to validate --- so this is the only refusal a malformed document
    /// gets.
    pub fn from_document(text: &str) -> Result<Self, serde_yaml_ng::Error> {
        serde_yaml_ng::from_str(text)
    }
}

/// One column, and the key it is read from.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct Column {
    /// The column's name in the table.
    pub name: String,
    /// The key in the arriving document.
    ///
    /// Defaults to the column's name, because they are usually the same and a mapping that
    /// must be written twice is one that will one day be written twice differently.
    #[serde(default)]
    pub from: Option<String>,
    /// The type the value is read as, written out.
    ///
    /// A string rather than a deserialized enum, so that `int` --- which is not a type here
    /// --- is refused by [`validate`](crate::validate::validate) with a sentence naming what
    /// was written and what exists, instead of by a deserializer reporting that it expected
    /// one of fifteen variants.
    ///
    /// Never inferred from the data. A type read off the first document is a type that
    /// changes when the first document does.
    #[serde(rename = "type")]
    pub written_type: String,
    /// Whether the column accepts nulls.
    #[serde(default)]
    pub nullable: bool,
    /// What a document that lacks this key means.
    #[serde(default)]
    pub missing: Missing,
}

impl Column {
    /// The key this column reads.
    #[must_use]
    pub fn key(&self) -> &str {
        self.from.as_deref().unwrap_or(&self.name)
    }
}

/// Where a row's date comes from.
///
/// # Why this cannot be left out
///
/// Every table carries one date axis, and the two possible meanings --- *the date this row is
/// about* and *the date we heard about it* --- are not interchangeable. A table holding a
/// mixture answers `WHERE sank_data_date = '2024-03-01'` with rows of both kinds, and nothing
/// in the answer says which is which.
///
/// So a feed says which it is. `IngestDate` is a legitimate answer and it has to be written
/// down, because "nobody thought about it" and "this is arrival time, deliberately" produce
/// the same column and mean different things.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DateFrom {
    /// The moment this system read the record.
    Ingest,
    /// A declared column of the record, which must be a date and must not be null.
    Column {
        /// Which column.
        name: String,
    },
}

/// What a document that lacks a column's key means.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Missing {
    /// The document does not fit. **The default.**
    ///
    /// A default value is indistinguishable from a measurement, and a measurement nobody
    /// made is the kind of wrong number that survives every review.
    #[default]
    Refuse,
    /// The absence is itself the value, and the column says so by being nullable.
    Null,
}

/// What a key no column claims means.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unknown {
    /// The document does not fit. **The default.**
    ///
    /// A source that grew a field is news. Discarding it silently is how a schema change
    /// becomes visible six months later, when somebody asks where the data went.
    #[default]
    Refuse,
    /// Written down, deliberately, by an operator who knows the source emits more than this
    /// table wants.
    Ignore,
}

/// When a batch is closed and published.
///
/// # Both bounds, because either alone stalls
///
/// Size alone leaves the last few records of a quiet hour unpublished until enough arrive.
/// Time alone publishes a file per tick under load, which is how a warehouse acquires a
/// million tiny files. The batch closes on whichever comes first.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct Microbatch {
    /// Close after this many rows.
    pub rows: u64,
    /// Close after this many seconds, however few rows there are.
    pub seconds: u64,
}

impl Default for Microbatch {
    fn default() -> Self {
        Self { rows: 10_000, seconds: 30 }
    }
}

/// What happens to records that do not fit, and when a run of them stops the feed.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, Serialize)]
pub struct Quarantine {
    /// How many days a quarantined record is kept.
    ///
    /// Mandatory in effect: zero is refused. A quarantine that only grows is an accumulation
    /// nobody is responsible for, holding precisely the records nobody looked at.
    pub retain_days: u32,
    /// The window, in records, the stop rate is measured over.
    pub window: u32,
    /// The fraction of that window which, once quarantined, stops the feed.
    ///
    /// A rate rather than a total: a total accumulates over the life of a feed and eventually
    /// trips for reasons that are historical, where a rate says what is happening now.
    pub stop_above: f64,
}

impl Default for Quarantine {
    fn default() -> Self {
        // Twenty percent of a hundred. A source that has genuinely changed shape produces far
        // more than this; a source with occasional bad records produces far less. The default
        // exists because a control an operator must invent a number for ships switched off.
        Self { retain_days: 30, window: 100, stop_above: 0.2 }
    }
}
