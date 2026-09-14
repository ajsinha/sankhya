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

use arrow_schema::DataType;
use sankhya_stats::{Bound, ColumnStats, TimeUnit};
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
///
/// # Dates and timestamps are strings, because the protocol says so
///
/// Delta writes a statistic as the column's value serialized as JSON, and for a date that is
/// `YYYY-MM-DD` and for a timestamp an ISO-8601 instant in UTC. Writing the underlying day or
/// microsecond count instead would round-trip perfectly through this system's own reader and
/// be meaningless to every other one — which is the failure mode this module's header calls
/// out as the worse kind, because it cannot be fixed from here.
#[must_use]
pub fn encode_bound(bound: &Bound) -> Option<serde_json::Value> {
    match bound {
        Bound::Int(v) => Some(serde_json::Value::from(*v)),
        Bound::Float(v) if v.is_finite() => serde_json::Number::from_f64(*v).map(Into::into),
        Bound::Float(_) => None,
        Bound::Bytes(v) => std::str::from_utf8(v)
            .ok()
            .map(|s| serde_json::Value::from(s)),
        Bound::Date(days) => Some(serde_json::Value::from(render_date(*days))),
        Bound::Timestamp { value, unit } => {
            render_timestamp(*value, *unit).map(serde_json::Value::from)
        }
        // **Exact or nothing.** The protocol carries a decimal as a JSON number, and this
        // crate is built without `serde_json`'s arbitrary-precision mode — so a number goes
        // through `f64` on the way in and on the way out. Most money survives that: two
        // decimal places and an ordinary magnitude round-trip exactly. A value that does not
        // is written as no bound at all, proved by rendering the number back and comparing
        // it to the decimal this started from, rather than by a rule about how many digits
        // are safe.
        Bound::Decimal { unscaled, scale } => {
            let text = render_decimal(*unscaled, *scale);
            let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
            (parsed.to_string() == text).then_some(parsed)
        }
    }
}

/// The inverse, read **against the column's type**.
///
/// # Why the type is needed, and what happened without it
///
/// The protocol's document is schemaless: `"2026-09-14"` is a JSON string, and so is a city
/// name. Decoding by JSON shape alone turned every date bound into a byte string, which is
/// incomparable with the [`Bound::Date`] a query's literal produces — so the bound was
/// written, read back, and never matched anything. That is a scan rather than a wrong answer,
/// and it is also exactly the state `M24` set out to leave: statistics recorded and pruning
/// that never happens.
///
/// `None` where the value does not fit the type, which includes a log written by something
/// that disagrees with its own schema. A bound nobody can interpret is not a bound.
#[must_use]
pub fn decode_bound(value: &serde_json::Value, data_type: Option<&DataType>) -> Option<Bound> {
    match (value, data_type) {
        (serde_json::Value::String(text), Some(DataType::Date32)) => {
            parse_date(text).map(Bound::Date)
        }
        (serde_json::Value::String(text), Some(DataType::Timestamp(unit, _))) => {
            let unit = time_unit(unit);
            parse_timestamp(text, unit).map(|value| Bound::Timestamp { value, unit })
        }
        (serde_json::Value::Number(n), Some(&DataType::Decimal128(_, scale))) => {
            parse_decimal(&n.to_string(), scale).map(|unscaled| Bound::Decimal { unscaled, scale })
        }
        // Unchanged for everything else, including a bound whose column the caller could not
        // name. A log this system wrote before `M24` holds integers, floats and strings under
        // exactly these rules, so it reads back as it always did.
        (serde_json::Value::Number(n), _) => {
            if let Some(i) = n.as_i64() {
                Some(Bound::Int(i))
            } else {
                n.as_f64().map(Bound::Float)
            }
        }
        (serde_json::Value::String(s), _) => Some(Bound::Bytes(s.as_bytes().to_vec())),
        _ => None,
    }
}

/// `TimeUnit` as this system spells it.
const fn time_unit(unit: &arrow_schema::TimeUnit) -> TimeUnit {
    match unit {
        arrow_schema::TimeUnit::Second => TimeUnit::Second,
        arrow_schema::TimeUnit::Millisecond => TimeUnit::Millisecond,
        arrow_schema::TimeUnit::Microsecond => TimeUnit::Microsecond,
        arrow_schema::TimeUnit::Nanosecond => TimeUnit::Nanosecond,
    }
}

/// `YYYY-MM-DD`, zero-padded, with years before the epoch rendered as they are read.
fn render_date(days: i32) -> String {
    let (year, month, day) = sankhya_schema::civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the epoch, from `YYYY-MM-DD`.
fn parse_date(text: &str) -> Option<i32> {
    let mut parts = text.trim().split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    sankhya_schema::days_from_civil(year, month, day)
}

/// `YYYY-MM-DDTHH:MM:SS[.fff…]Z`, with only as many fractional digits as the unit has.
///
/// `None` for an instant outside the calendar this can render, which is a bound nobody can
/// read rather than one rendered wrongly.
fn render_timestamp(value: i64, unit: TimeUnit) -> Option<String> {
    let per_second = unit.per_second();
    let seconds = value.div_euclid(per_second);
    let fraction = value.rem_euclid(per_second);
    let days = i32::try_from(seconds.div_euclid(86_400)).ok()?;
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = sankhya_schema::civil_from_days(days);
    let (hour, minute, second) = (
        second_of_day / 3_600,
        (second_of_day / 60) % 60,
        second_of_day % 60,
    );
    let stamp = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    Some(match unit {
        TimeUnit::Second => format!("{stamp}Z"),
        TimeUnit::Millisecond => format!("{stamp}.{fraction:03}Z"),
        TimeUnit::Microsecond => format!("{stamp}.{fraction:06}Z"),
        TimeUnit::Nanosecond => format!("{stamp}.{fraction:09}Z"),
    })
}

/// The count of `unit` since the epoch, from an ISO-8601 instant.
///
/// Accepts the shape this system writes and the shapes other writers use: a `T` or a space
/// between the date and the time, an optional fractional part, and an optional trailing `Z`.
///
/// # An offset is refused, and by the parser rather than by a guard
///
/// A stats document is specified as UTC, and a value carrying `+05:30` is one whose writer
/// meant something this cannot check --- applying the offset on a guess moves every bound in
/// the column, consistently, so nothing looks wrong until somebody reconciles against the
/// source.
///
/// There *was* a guard here that rejected `+` and a late `-` before anything else. The
/// mutation catalogue proved it could not fail: every offset form reaches the field checks
/// below and is refused there, because the offset's digits land either in the fractional part
/// --- which must be digits and no longer than the unit holds --- or as a fourth colon-
/// separated field. A check that cannot fail is worse than an absent one, so it is gone and
/// the property is stated where it is actually enforced.
fn parse_timestamp(text: &str, unit: TimeUnit) -> Option<i64> {
    let text = text.trim().trim_end_matches('Z');
    let (date, time) = text.split_once('T').or_else(|| text.split_once(' '))?;
    let days = i64::from(parse_date(date)?);

    let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut fields = clock.split(':');
    let hour: i64 = fields.next()?.parse().ok()?;
    let minute: i64 = fields.next()?.parse().ok()?;
    let second: i64 = fields.next().unwrap_or("0").parse().ok()?;
    if fields.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    // Padded or truncated to the unit's own width. Truncating loses precision the bound had,
    // which moves a maximum **down** --- so a fraction finer than the unit refuses instead.
    let digits = (unit.per_second() as f64).log10().round() as usize;
    if fraction.len() > digits || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut padded = fraction.to_string();
    while padded.len() < digits {
        padded.push('0');
    }
    let sub: i64 = if digits == 0 { 0 } else { padded.parse().ok()? };

    days.checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?
        .checked_mul(unit.per_second())?
        .checked_add(sub)
}

/// The decimal `unscaled × 10⁻ˢᶜᵃˡᵉ`, written out in full.
fn render_decimal(unscaled: i128, scale: i8) -> String {
    if scale <= 0 {
        // A negative scale multiplies. Rendered as the integer it is, with the zeroes on it.
        let zeroes = usize::try_from(-i32::from(scale)).unwrap_or(0);
        return format!("{unscaled}{}", "0".repeat(zeroes));
    }
    let places = usize::from(scale.unsigned_abs());
    let sign = if unscaled < 0 { "-" } else { "" };
    let digits = unscaled.unsigned_abs().to_string();
    let padded = format!("{:0>width$}", digits, width = places + 1);
    let split = padded.len() - places;
    format!("{sign}{}.{}", &padded[..split], &padded[split..])
}

/// The unscaled value at `scale`, from a decimal written out in full.
///
/// `None` when the text carries more fractional digits than the scale holds: dropping them
/// rounds, and a rounded maximum is a maximum smaller than the truth.
fn parse_decimal(text: &str, scale: i8) -> Option<i128> {
    let text = text.trim();
    let (sign, text) = match text.strip_prefix('-') {
        Some(rest) => (-1_i128, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let places = usize::try_from(i32::from(scale)).ok()?;
    if fraction.len() > places {
        return None;
    }
    let mut digits = format!("{whole}{fraction}");
    for _ in fraction.len()..places {
        digits.push('0');
    }
    digits.parse::<i128>().ok()?.checked_mul(sign)
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
pub fn to_column_stats(
    stats: &FileStatistics,
    schema: Option<&arrow_schema::Schema>,
) -> BTreeMap<String, ColumnStats> {
    let mut out: BTreeMap<String, ColumnStats> = BTreeMap::new();

    let names: std::collections::BTreeSet<&String> = stats
        .null_count
        .keys()
        .chain(stats.min_values.keys())
        .chain(stats.max_values.keys())
        .collect();

    for name in names {
        // The column's declared type, where the caller has a schema to offer. A date bound is
        // `"2026-09-14"` in the document and a city name is `"paris"`, and nothing but the
        // schema tells them apart --- see [`decode_bound`].
        let data_type = schema
            .and_then(|schema| schema.column_with_name(name))
            .map(|(_, field)| field.data_type());
        out.insert(
            name.clone(),
            ColumnStats {
                rows: stats.num_records,
                nulls: stats.null_count.get(name).copied().unwrap_or(0),
                min: stats
                    .min_values
                    .get(name)
                    .and_then(|v| decode_bound(v, data_type)),
                max: stats
                    .max_values
                    .get(name)
                    .and_then(|v| decode_bound(v, data_type)),
                ..ColumnStats::default()
            },
        );
    }

    out
}
