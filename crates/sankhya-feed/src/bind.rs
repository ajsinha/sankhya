//! Reading one arriving document against a feed's declared shape.
//!
//! # Everything here refuses rather than converts
//!
//! Each rule below exists because its permissive version produces published data that looks
//! fine. That is the whole difficulty: a coercion is never noticed at the time, because the
//! number it produces is well-formed, the query succeeds, and the report renders.
//!
//! - **A string is not a number.** `"42"` into an `int64` is the conversion every ingest tool
//!   offers and the one that hides a source emitting `"42 "`, `"4 2"`, or `"forty-two"` until
//!   the day it emits one of them into a total.
//! - **A number is not a decimal.** JSON numbers are binary floating point in every producer
//!   this will meet, and `0.1` is not `0.1` there. A `decimal` column reads a **string**,
//!   which is the only JSON representation that survives the trip exactly.
//! - **A float is not an integer.** `3.0` into an `int32` is convertible and `3.5` is a
//!   silent truncation, and a rule that permits the first has to decide about the second.
//! - **A missing key is not a null**, unless the column says so by name, and says it while
//!   being nullable.
//! - **A key nobody claimed is news**, unless the feed has been told to ignore it.

use crate::declare::{Missing, Unknown};
use crate::validate::{Feed, Shaped};
use sankhya_schema::LogicalType;
use serde_json::{Map, Value};
use std::fmt;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

/// The Julian day number of 1970-01-01, for turning a date into days since the epoch.
const UNIX_EPOCH_JULIAN_DAY: i32 = 2_440_588;

/// A timestamp with no offset, which is the only spelling accepted for a local one.
const LOCAL_TIMESTAMP: &[time::format_description::FormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");

/// Microseconds from nanoseconds, saturating rather than wrapping.
fn micros_of(nanos: i128) -> i64 {
    i64::try_from(nanos / 1_000).unwrap_or(i64::MAX)
}

/// One cell of a bound row.
///
/// Deliberately small and typed. Carrying the `serde_json::Value` forward would mean every
/// later stage re-deciding what a value is, and one of them deciding differently.
#[derive(Clone, PartialEq, Debug)]
pub enum Cell {
    /// Absent, in a column that permits it.
    Null,
    /// A boolean.
    Boolean(bool),
    /// A whole number, held at its widest.
    Integer(i64),
    /// An inexact number.
    Real(f64),
    /// Exact, unscaled: the digits with the decimal point removed.
    Decimal(i128),
    /// Text, and the canonical text of a JSON document.
    Text(String),
    /// Bytes.
    Bytes(Vec<u8>),
    /// Microseconds since the epoch, or since midnight.
    Micros(i64),
    /// Days since the epoch.
    Days(i32),
    /// Sixteen bytes.
    Uuid([u8; 16]),
}

/// A document read against a feed.
#[derive(Clone, PartialEq, Debug)]
pub struct Row {
    /// One cell per declared column, in the feed's order.
    pub cells: Vec<Cell>,
}

/// Why a document does not fit.
///
/// Carried into the quarantine whole, alongside the document itself, because a record reduced
/// to its error message cannot be replayed --- and replay is the only actual remedy.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unfit {
    /// What arrived is not a dictionary.
    NotADocument,
    /// A column's key is absent and the column does not say what that means.
    MissingKey {
        /// The column.
        column: String,
        /// The key it reads.
        key: String,
    },
    /// The document carries a key no column claims.
    UnknownKey {
        /// The key.
        key: String,
    },
    /// The value is the wrong kind of thing for the column.
    WrongKind {
        /// The column.
        column: String,
        /// What the column reads.
        wanted: &'static str,
        /// What arrived.
        found: &'static str,
        /// Why this is refused rather than converted.
        because: &'static str,
    },
    /// The value is the right kind and will not fit.
    OutOfRange {
        /// The column.
        column: String,
        /// What the column reads.
        wanted: &'static str,
        /// The value, as it was written.
        value: String,
    },
    /// A null in a column that does not accept one.
    NullIntoNotNull {
        /// The column.
        column: String,
    },
}

impl fmt::Display for Unfit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADocument => write!(
                f,
                "what arrived is not a dictionary. A feed reads named keys, and an array or a \
                 bare value has none"
            ),
            Self::MissingKey { column, key } => write!(
                f,
                "`{column}` reads the key `{key}` and the document has no such key. Refused \
                 rather than filled in: a value invented here is indistinguishable from one \
                 that was measured"
            ),
            Self::UnknownKey { key } => write!(
                f,
                "the document carries `{key}` and no column claims it. A source that grew a \
                 field is news; set `unknown: ignore` to say this one is not"
            ),
            Self::WrongKind { column, wanted, found, because } => write!(
                f,
                "`{column}` reads {wanted} and this document has {found}. {because}"
            ),
            Self::OutOfRange { column, wanted, value } => {
                write!(f, "`{value}` does not fit in `{column}`, which reads {wanted}")
            }
            Self::NullIntoNotNull { column } => {
                write!(f, "`{column}` is null in this document and does not accept nulls")
            }
        }
    }
}

impl std::error::Error for Unfit {}

/// Read a document against a feed.
///
/// Reports the **first** reason it does not fit rather than all of them, which is the opposite
/// of how a configuration is validated --- and deliberately. A configuration is read once by a
/// person who will fix every fault; a document is one of millions, and what matters is that it
/// was refused and why, not an exhaustive account of a record nobody will edit.
///
/// # Errors
///
/// [`Unfit`], naming the column and what was wrong with it.
pub fn bind(feed: &Feed, document: &Value) -> Result<Row, Unfit> {
    let Some(fields) = document.as_object() else {
        return Err(Unfit::NotADocument);
    };

    if feed.unknown() == Unknown::Refuse {
        let claimed: std::collections::BTreeSet<&str> =
            feed.columns().iter().map(|column| column.key.as_str()).collect();
        for key in fields.keys() {
            if !claimed.contains(key.as_str()) {
                return Err(Unfit::UnknownKey { key: key.clone() });
            }
        }
    }

    let mut cells = Vec::with_capacity(feed.columns().len());
    for column in feed.columns() {
        cells.push(cell(column, fields)?);
    }
    Ok(Row { cells })
}

/// One column's value.
fn cell(column: &Shaped, fields: &Map<String, Value>) -> Result<Cell, Unfit> {
    let Some(value) = fields.get(&column.key) else {
        return match column.missing {
            Missing::Null => Ok(Cell::Null),
            Missing::Refuse => Err(Unfit::MissingKey {
                column: column.name.clone(),
                key: column.key.clone(),
            }),
        };
    };
    if value.is_null() {
        return if column.nullable {
            Ok(Cell::Null)
        } else {
            Err(Unfit::NullIntoNotNull { column: column.name.clone() })
        };
    }
    read(column, value)
}

/// What kind of thing a JSON value is, for a refusal to name.
const fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "a dictionary",
    }
}

/// Refuse, naming what was wanted and what arrived.
fn wrong(column: &Shaped, wanted: &'static str, value: &Value, because: &'static str) -> Unfit {
    Unfit::WrongKind {
        column: column.name.clone(),
        wanted,
        found: kind(value),
        because,
    }
}

/// A value that arrives as text in a stated spelling.
///
/// Temporal columns read strings and not numbers. A number would have to be seconds, or
/// milliseconds, or microseconds, or days --- and every producer picks a different one, so a
/// feed that accepted a number would be choosing on the producer's behalf and would be right
/// about three quarters of them.
fn temporal(
    column: &Shaped,
    wanted: &'static str,
    value: &Value,
    read: impl Fn(&str) -> Option<Cell>,
) -> Result<Cell, Unfit> {
    let Some(text) = value.as_str() else {
        return Err(wrong(
            column,
            wanted,
            value,
            "a number here would have to be seconds, or milliseconds, or days, and every \
             producer picks a different one",
        ));
    };
    read(text).ok_or_else(|| Unfit::OutOfRange {
        column: column.name.clone(),
        wanted,
        value: text.to_owned(),
    })
}

/// Read a present, non-null value.
fn read(column: &Shaped, value: &Value) -> Result<Cell, Unfit> {
    match column.logical {
        LogicalType::Boolean => value.as_bool().map(Cell::Boolean).ok_or_else(|| {
            wrong(column, "a boolean", value, "0 and 1 are numbers, and \"true\" is text")
        }),
        LogicalType::Int16 | LogicalType::Int32 | LogicalType::Int64 => integer(column, value),
        LogicalType::Float32 | LogicalType::Float64 => value
            .as_f64()
            .map(Cell::Real)
            .ok_or_else(|| wrong(column, "a number", value, "a string is not a number here")),
        LogicalType::Decimal(_) => decimal(column, value),
        LogicalType::Utf8 => value
            .as_str()
            .map(|text| Cell::Text(text.to_owned()))
            .ok_or_else(|| wrong(column, "text", value, "a number rendered as text is a \
                                                        decision this feed will not make")),
        LogicalType::Json => Ok(Cell::Text(value.to_string())),
        LogicalType::Date => temporal(column, "a date, written as YYYY-MM-DD", value, |text| {
            Date::parse(text, &format_description!("[year]-[month]-[day]"))
                .ok()
                .map(|date| Cell::Days(date.to_julian_day() - UNIX_EPOCH_JULIAN_DAY))
        }),
        LogicalType::TimestampUtc => temporal(
            column,
            "a timestamp with an offset, written as RFC 3339",
            value,
            |text| {
                OffsetDateTime::parse(text, &Rfc3339)
                    .ok()
                    .map(|moment| Cell::Micros(micros_of(moment.unix_timestamp_nanos())))
            },
        ),
        LogicalType::TimestampLocal => temporal(
            column,
            "a timestamp with no offset, written as YYYY-MM-DDTHH:MM:SS",
            value,
            |text| {
                PrimitiveDateTime::parse(text, &LOCAL_TIMESTAMP).ok().map(|moment| {
                    Cell::Micros(micros_of(moment.assume_utc().unix_timestamp_nanos()))
                })
            },
        ),
        LogicalType::Time => temporal(column, "a time of day, written as HH:MM:SS", value, |text| {
            Time::parse(text, &format_description!("[hour]:[minute]:[second]"))
                .ok()
                .map(|time| {
                    let (hour, minute, second, micro) = time.as_hms_micro();
                    Cell::Micros(
                        i64::from(hour) * 3_600_000_000
                            + i64::from(minute) * 60_000_000
                            + i64::from(second) * 1_000_000
                            + i64::from(micro),
                    )
                })
        }),
        LogicalType::Uuid => temporal(column, "a uuid", value, |text| {
            uuid::Uuid::parse_str(text).ok().map(|id| Cell::Uuid(*id.as_bytes()))
        }),
        // Binary is refused, deliberately and for now. Bytes in a JSON document are text in
        // some encoding, and choosing one here --- base64, hex, escaped --- would be this
        // feed guessing at a producer's convention. A declaration that says which encoding
        // is the answer, and it waits until there is a source with an opinion.
        LogicalType::Binary => Err(wrong(
            column,
            "bytes, which a feed cannot yet read",
            value,
            "bytes in a JSON document are text in some encoding, and which one is the \
             producer's decision rather than this feed's to guess",
        )),
    }
}

/// A whole number, refusing a float and a string alike.
fn integer(column: &Shaped, value: &Value) -> Result<Cell, Unfit> {
    let Some(number) = value.as_number() else {
        return Err(wrong(
            column,
            "a whole number",
            value,
            "a string is not a number: accepting \"42\" is what hides the day a source emits \
             \"forty-two\"",
        ));
    };
    let Some(whole) = number.as_i64() else {
        return Err(Unfit::OutOfRange {
            column: column.name.clone(),
            wanted: "a whole number",
            value: number.to_string(),
        });
    };
    let fits = match column.logical {
        LogicalType::Int16 => i16::try_from(whole).is_ok(),
        LogicalType::Int32 => i32::try_from(whole).is_ok(),
        _ => true,
    };
    if fits {
        Ok(Cell::Integer(whole))
    } else {
        Err(Unfit::OutOfRange {
            column: column.name.clone(),
            wanted: "a narrower whole number",
            value: whole.to_string(),
        })
    }
}

/// An exact decimal, which arrives as a string or not at all.
fn decimal(column: &Shaped, value: &Value) -> Result<Cell, Unfit> {
    let LogicalType::Decimal(precision) = column.logical else {
        return Err(wrong(column, "a decimal", value, "this column is not a decimal"));
    };
    let Some(text) = value.as_str() else {
        return Err(wrong(
            column,
            "an exact decimal, written as a string",
            value,
            "JSON numbers are binary floating point in every producer this will meet, where \
             0.1 is not 0.1 --- a string is the only representation that survives the trip",
        ));
    };
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i128, rest),
        None => (1i128, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() {
        return Err(out_of_range(column, text));
    }
    if !whole.bytes().chain(fraction.bytes()).all(|byte| byte.is_ascii_digit()) {
        return Err(out_of_range(column, text));
    }
    // Refused rather than rounded. A value with more places than the column declares is a
    // source disagreeing with a configuration, and quietly rounding it is how a reconciliation
    // fails by a penny that nobody can trace.
    if fraction.len() > usize::from(precision.scale) {
        return Err(out_of_range(column, text));
    }
    let padding = usize::from(precision.scale).saturating_sub(fraction.len());
    let mut unscaled = String::with_capacity(whole.len() + fraction.len() + padding);
    unscaled.push_str(whole);
    unscaled.push_str(fraction);
    for _ in 0..padding {
        unscaled.push('0');
    }
    let Ok(magnitude) = unscaled.parse::<i128>() else {
        return Err(out_of_range(column, text));
    };
    if unscaled.trim_start_matches('0').len() > usize::from(precision.digits) {
        return Err(out_of_range(column, text));
    }
    Ok(Cell::Decimal(sign * magnitude))
}

/// A decimal that will not fit its column.
fn out_of_range(column: &Shaped, text: &str) -> Unfit {
    Unfit::OutOfRange {
        column: column.name.clone(),
        wanted: "an exact decimal of the declared digits and scale",
        value: text.to_owned(),
    }
}
