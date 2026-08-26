//! Encoding and decoding the protocol's file statistics.
//!
//! # Why this is a separate module from the log
//!
//! The protocol carries statistics as a JSON string *inside* a JSON object, and the
//! inner document is schemaless: its keys are column names. That is a different shape
//! from the rest of the log, which is a fixed set of typed actions, and mixing the two
//! makes both harder to read.
//!
//! # What is written, and what is refused
//!
//! A bound is written only where it can be justified. An unrecognised type produces no
//! bound; an unorderable value produces no bound; a merge that would narrow a bound
//! drops it. So an absent entry here means "nothing is known", never "no values" — and a
//! reader that treats absence as an unbounded range is correct, while one that treats it
//! as an empty range is not.

use sankhya_stats::{Bound, ColumnStats};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The protocol's statistics document.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct FileStatistics {
    #[serde(rename = "numRecords")]
    pub num_records: u64,
    /// Per-column minimum, for the columns where one is known.
    #[serde(
        rename = "minValues",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub min_values: BTreeMap<String, serde_json::Value>,
    #[serde(
        rename = "maxValues",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub max_values: BTreeMap<String, serde_json::Value>,
    #[serde(
        rename = "nullCount",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub null_count: BTreeMap<String, u64>,
}

impl FileStatistics {
    #[must_use]
    pub fn new(num_records: u64) -> Self {
        Self {
            num_records,
            ..Self::default()
        }
    }

    /// Record what is known about one column.
    ///
    /// `min` and `max` are optional independently: a column may have a known null count
    /// and no bounds, which is exactly what happens for a type this system does not
    /// recognise.
    pub fn with_column(
        &mut self,
        name: &str,
        min: Option<serde_json::Value>,
        max: Option<serde_json::Value>,
        nulls: u64,
    ) {
        if let Some(min) = min {
            self.min_values.insert(name.to_string(), min);
        }
        if let Some(max) = max {
            self.max_values.insert(name.to_string(), max);
        }
        self.null_count.insert(name.to_string(), nulls);
    }

    /// The encoded form the protocol expects.
    ///
    /// # Errors
    ///
    /// Returns an error only if the document cannot be encoded, which would mean a
    /// column name or value that is not representable in JSON.
    pub fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Parse an encoded statistics document.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not the document this protocol defines.
    /// Refusing is correct: statistics that cannot be parsed are statistics that are not
    /// known, and guessing at a partially-read document is how a bound ends up bounding
    /// nothing.
    pub fn decode(encoded: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(encoded)
    }
}

/// The protocol's representation of a bound, or `None` where there is none it can carry.
///
/// A value the protocol cannot represent — a non-finite float, bytes that are not text —
/// yields no bound rather than an approximation. An absent bound costs a scan; a
/// misrepresented one costs an answer, and it costs it in *other engines* too, which is
/// worse because they cannot be fixed from here.
#[must_use]
pub fn encode_bound(bound: &Bound) -> Option<serde_json::Value> {
    match bound {
        Bound::Int(v) => Some(serde_json::Value::from(*v)),
        Bound::Float(v) if v.is_finite() => serde_json::Number::from_f64(*v).map(Into::into),
        Bound::Float(_) => None,
        Bound::Bytes(v) => std::str::from_utf8(v)
            .ok()
            .map(|s| serde_json::Value::from(s)),
    }
}

/// The inverse, or `None` where the value is not a bound this system understands.
#[must_use]
pub fn decode_bound(value: &serde_json::Value) -> Option<Bound> {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Bound::Int(i))
            } else {
                n.as_f64().map(Bound::Float)
            }
        }
        serde_json::Value::String(s) => Some(Bound::Bytes(s.as_bytes().to_vec())),
        _ => None,
    }
}

/// Build a statistics document from SANKHYA's own per-column statistics.
#[must_use]
pub fn from_column_stats(
    num_records: u64,
    columns: &BTreeMap<String, ColumnStats>,
) -> FileStatistics {
    let mut out = FileStatistics::new(num_records);
    for (name, stats) in columns {
        out.with_column(
            name,
            stats.min.as_ref().and_then(encode_bound),
            stats.max.as_ref().and_then(encode_bound),
            stats.nulls,
        );
    }
    out
}

/// Recover per-column statistics from a statistics document.
///
/// The cardinality sketch is **not** recoverable — it is not in the protocol and there
/// is nowhere to put it. A column read back from the log therefore reports zero distinct
/// values, which is why [`ColumnStats::distinct_estimate`] must never be treated as
/// authoritative without checking whether the sketch is populated.
#[must_use]
pub fn to_column_stats(stats: &FileStatistics) -> BTreeMap<String, ColumnStats> {
    let mut out: BTreeMap<String, ColumnStats> = BTreeMap::new();

    let names: std::collections::BTreeSet<&String> = stats
        .null_count
        .keys()
        .chain(stats.min_values.keys())
        .chain(stats.max_values.keys())
        .collect();

    for name in names {
        out.insert(
            name.clone(),
            ColumnStats {
                rows: stats.num_records,
                nulls: stats.null_count.get(name).copied().unwrap_or(0),
                min: stats.min_values.get(name).and_then(decode_bound),
                max: stats.max_values.get(name).and_then(decode_bound),
                ..ColumnStats::default()
            },
        );
    }

    out
}
