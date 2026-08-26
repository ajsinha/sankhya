//! Text values to typed Arrow arrays.

use arrow_array::builder::{
    BooleanBuilder, Date32Builder, Decimal128Builder, Float32Builder, Float64Builder,
    Int16Builder, Int32Builder, Int64Builder, StringBuilder, TimestampMicrosecondBuilder,
    UInt64Builder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::ArrowError;
use sankhya_cdc_apply::Mutation;
use sankhya_schema::{LogicalSchema, LogicalType, Precision};
use std::fmt;
use std::sync::Arc;

/// Why a value could not be encoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EncodeError {
    /// A value does not parse as its declared type.
    ///
    /// Fatal by design. Substituting a null would turn a parsing defect into missing
    /// data, which is far harder to detect — the row is present, the query succeeds,
    /// and one column is silently empty.
    Unparseable { column: String, logical: String, value: String },
    /// A null arrived for a column declared not-null.
    UnexpectedNull { column: String },
    /// A row has the wrong number of values for the schema.
    Arity { expected: usize, found: usize },
    /// The Arrow layer rejected the assembled batch.
    Arrow { detail: String },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparseable { column, logical, value } => write!(
                f,
                "{column}: {value:?} is not a valid {logical}. The value is refused \
                 rather than nulled, because a silently empty column is harder to \
                 detect than a failed write"
            ),
            Self::UnexpectedNull { column } => {
                write!(f, "{column} is declared not-null but a null arrived")
            }
            Self::Arity { expected, found } => {
                write!(f, "row has {found} values, schema declares {expected}")
            }
            Self::Arrow { detail } => write!(f, "arrow rejected the batch: {detail}"),
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<ArrowError> for EncodeError {
    fn from(e: ArrowError) -> Self {
        Self::Arrow { detail: e.to_string() }
    }
}

/// Encode mutations into one typed batch, appending the provenance columns.
///
/// # Errors
///
/// Returns [`EncodeError`] for any value that does not parse, any unexpected null, or
/// any row whose arity disagrees with the schema.
pub fn encode_batch(
    schema: &LogicalSchema,
    mutations: &[Mutation],
) -> Result<RecordBatch, EncodeError> {
    let arrow_schema = Arc::new(schema.arrow_schema());
    let mut columns: Vec<ArrayRef> = Vec::with_capacity(schema.fields.len() + 3);

    for (index, field) in schema.fields.iter().enumerate() {
        columns.push(encode_column(field.name.as_str(), &field.logical, field.nullable, index, mutations)?);
    }

    // Provenance travels with the data, so the applied position is recoverable from
    // the table's own history rather than from external state that could drift.
    let mut lsn = UInt64Builder::with_capacity(mutations.len());
    let mut ts = TimestampMicrosecondBuilder::with_capacity(mutations.len());
    let mut op = StringBuilder::new();
    for m in mutations {
        lsn.append_value(m.commit_lsn.get());
        // The commit position is the ordering coordinate; the timestamp is recorded
        // for human reading only and is never used for ordering.
        ts.append_value(0);
        op.append_value(match m.op {
            sankhya_cdc_apply::Op::Insert => "I",
            sankhya_cdc_apply::Op::Update => "U",
            sankhya_cdc_apply::Op::Delete => "D",
        });
    }
    columns.push(Arc::new(lsn.finish()));
    columns.push(Arc::new(ts.finish().with_timezone("UTC")));
    columns.push(Arc::new(op.finish()));

    Ok(RecordBatch::try_new(arrow_schema, columns)?)
}

fn value_at<'a>(m: &'a Mutation, index: usize, expected: usize) -> Result<&'a Option<String>, EncodeError> {
    m.row.values.get(index).ok_or(EncodeError::Arity { expected, found: m.row.values.len() })
}

macro_rules! numeric_column {
    ($builder:expr, $name:expr, $nullable:expr, $index:expr, $mutations:expr, $ty:literal, $parse:expr) => {{
        let mut b = $builder;
        for m in $mutations {
            match value_at(m, $index, usize::MAX)? {
                None if $nullable => b.append_null(),
                None => return Err(EncodeError::UnexpectedNull { column: $name.to_string() }),
                Some(raw) => {
                    let parsed = $parse(raw.as_str()).ok_or_else(|| EncodeError::Unparseable {
                        column: $name.to_string(),
                        logical: $ty.to_string(),
                        value: raw.clone(),
                    })?;
                    b.append_value(parsed);
                }
            }
        }
        Arc::new(b.finish()) as ArrayRef
    }};
}

fn encode_column(
    name: &str,
    logical: &LogicalType,
    nullable: bool,
    index: usize,
    mutations: &[Mutation],
) -> Result<ArrayRef, EncodeError> {
    let n = mutations.len();
    Ok(match logical {
        LogicalType::Boolean => numeric_column!(
            BooleanBuilder::with_capacity(n), name, nullable, index, mutations, "boolean",
            |s: &str| match s {
                "t" | "true" | "TRUE" | "1" => Some(true),
                "f" | "false" | "FALSE" | "0" => Some(false),
                _ => None,
            }
        ),
        LogicalType::Int16 => numeric_column!(
            Int16Builder::with_capacity(n), name, nullable, index, mutations, "int16",
            |s: &str| s.parse::<i16>().ok()
        ),
        LogicalType::Int32 => numeric_column!(
            Int32Builder::with_capacity(n), name, nullable, index, mutations, "int32",
            |s: &str| s.parse::<i32>().ok()
        ),
        LogicalType::Int64 => numeric_column!(
            Int64Builder::with_capacity(n), name, nullable, index, mutations, "int64",
            |s: &str| s.parse::<i64>().ok()
        ),
        LogicalType::Float32 => numeric_column!(
            Float32Builder::with_capacity(n), name, nullable, index, mutations, "float32",
            |s: &str| s.parse::<f32>().ok()
        ),
        LogicalType::Float64 => numeric_column!(
            Float64Builder::with_capacity(n), name, nullable, index, mutations, "float64",
            |s: &str| s.parse::<f64>().ok()
        ),
        LogicalType::Date => numeric_column!(
            Date32Builder::with_capacity(n), name, nullable, index, mutations, "date",
            parse_date
        ),
        LogicalType::TimestampUtc | LogicalType::TimestampLocal => {
            let array = numeric_column!(
                TimestampMicrosecondBuilder::with_capacity(n), name, nullable, index,
                mutations, "timestamp", parse_timestamp_micros
            );
            if matches!(logical, LogicalType::TimestampUtc) {
                // The zone is part of the type. Dropping it here is how a column
                // silently shifts by hours later.
                let typed = array
                    .as_any()
                    .downcast_ref::<arrow_array::TimestampMicrosecondArray>()
                    .ok_or_else(|| EncodeError::Arrow {
                        detail: "timestamp array had an unexpected type".into(),
                    })?
                    .clone();
                Arc::new(typed.with_timezone("UTC")) as ArrayRef
            } else {
                array
            }
        }
        LogicalType::Decimal(p) => encode_decimal(name, *p, nullable, index, mutations)?,
        LogicalType::Utf8 | LogicalType::Json | LogicalType::Uuid | LogicalType::Binary => {
            let mut b = StringBuilder::new();
            for m in mutations {
                match value_at(m, index, usize::MAX)? {
                    None if nullable => b.append_null(),
                    None => return Err(EncodeError::UnexpectedNull { column: name.to_string() }),
                    Some(raw) => b.append_value(raw),
                }
            }
            Arc::new(b.finish()) as ArrayRef
        }
        LogicalType::Time => numeric_column!(
            Int64Builder::with_capacity(n), name, nullable, index, mutations, "time",
            |s: &str| parse_time_micros(s)
        ),
    })
}

/// Decimals are parsed to integer minor units, never through floating point.
///
/// Going via `f64` would introduce representation error in the one type whose entire
/// purpose is not to have any.
fn encode_decimal(
    name: &str,
    precision: Precision,
    nullable: bool,
    index: usize,
    mutations: &[Mutation],
) -> Result<ArrayRef, EncodeError> {
    let mut b = Decimal128Builder::with_capacity(mutations.len())
        .with_precision_and_scale(precision.digits, i8::try_from(precision.scale).unwrap_or(0))?;
    for m in mutations {
        match value_at(m, index, usize::MAX)? {
            None if nullable => b.append_null(),
            None => return Err(EncodeError::UnexpectedNull { column: name.to_string() }),
            Some(raw) => {
                let units = parse_decimal_units(raw, precision.scale).ok_or_else(|| {
                    EncodeError::Unparseable {
                        column: name.to_string(),
                        logical: format!("decimal({},{})", precision.digits, precision.scale),
                        value: raw.clone(),
                    }
                })?;
                b.append_value(units);
            }
        }
    }
    Ok(Arc::new(b.finish()) as ArrayRef)
}

/// Parse a decimal into minor units at the given scale, exactly.
///
/// Refuses rather than rounds when the input carries more fractional digits than the
/// column declares: silently dropping a digit is precisely the corruption this type
/// exists to prevent.
fn parse_decimal_units(text: &str, scale: u8) -> Option<i128> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, frac) = match digits.split_once('.') {
        Some((w, f)) => (w, f),
        None => (digits, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let scale = usize::from(scale);
    if frac.len() > scale {
        // More precision than the column can hold. Refuse.
        if frac[scale..].chars().any(|c| c != '0') {
            return None;
        }
    }
    let mut padded = String::with_capacity(whole.len() + scale);
    padded.push_str(whole);
    for i in 0..scale {
        padded.push(frac.as_bytes().get(i).map_or('0', |b| char::from(*b)));
    }
    let magnitude: i128 = padded.parse().ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

/// Days since the Unix epoch, from `YYYY-MM-DD`.
fn parse_date(text: &str) -> Option<i32> {
    let (y, m, d) = split_ymd(text.trim())?;
    Some(days_from_civil(y, m, d))
}

/// Microseconds since the Unix epoch.
///
/// Accepts `YYYY-MM-DD HH:MM:SS[.ffffff][±HH[:MM]|Z]`, in either space- or
/// `T`-separated form.
///
/// # The offset must be applied, never discarded
///
/// The source renders a zoned timestamp in the server's own offset, so a value may
/// arrive as `2026-08-26 00:27:11.367744-04`. Stripping the `-04` and treating the
/// wall time as UTC shifts the entire column by four hours — and every value stays
/// internally consistent, so nothing looks wrong until someone compares against the
/// source.
///
/// An earlier version of this function did exactly that. It was caught by
/// reconciliation against the source rather than by any test of this function alone,
/// which is the argument for reconciling against an independent model in the first
/// place.
fn parse_timestamp_micros(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date_part, rest) = text.split_once(' ').or_else(|| text.split_once('T'))?;
    let (y, m, d) = split_ymd(date_part)?;

    let (time_part, offset_micros) = split_offset(rest)?;
    let micros_of_day = parse_time_micros(time_part)?;
    let days = i64::from(days_from_civil(y, m, d));

    days.checked_mul(86_400_000_000)?
        .checked_add(micros_of_day)?
        // The offset says how far local time is ahead of UTC, so UTC is the local
        // reading minus the offset.
        .checked_sub(offset_micros)
}

/// Separate the time portion from its trailing zone offset.
///
/// Returns the offset in microseconds, positive for zones ahead of UTC.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(stripped) = rest.strip_suffix('Z') {
        return Some((stripped, 0));
    }

    // Scan from the end for a sign that begins a zone offset. It cannot be confused
    // with anything in the time itself, which contains only digits, colons and a dot.
    let bytes = rest.as_bytes();
    for (i, byte) in bytes.iter().enumerate().rev() {
        if !matches!(byte, b'+' | b'-') {
            continue;
        }
        let (time_part, offset_text) = rest.split_at(i);
        let sign = if *byte == b'-' { -1i64 } else { 1i64 };
        let digits = offset_text.get(1..)?;

        let (hours, minutes) = match digits.split_once(':') {
            Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
            // A bare two- or four-digit form: "04" or "0430".
            None if digits.len() <= 2 => (digits.parse::<i64>().ok()?, 0),
            None => (
                digits.get(..2)?.parse::<i64>().ok()?,
                digits.get(2..)?.parse::<i64>().ok()?,
            ),
        };
        if !(0..=14).contains(&hours) || !(0..60).contains(&minutes) {
            return None;
        }
        return Some((time_part, sign * (hours * 3_600 + minutes * 60) * 1_000_000));
    }

    // No offset at all: an unzoned timestamp, already the value it says it is.
    Some((rest, 0))
}

fn parse_time_micros(text: &str) -> Option<i64> {
    let mut parts = text.trim().split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let mi: i64 = parts.next()?.parse().ok()?;
    let sec_part = parts.next().unwrap_or("0");
    let (s, frac) = match sec_part.split_once('.') {
        Some((s, f)) => (s, f),
        None => (sec_part, ""),
    };
    let s: i64 = s.parse().ok()?;
    let mut micros = 0i64;
    for i in 0..6 {
        micros = micros * 10 + i64::from(frac.as_bytes().get(i).map_or(0, |b| b - b'0'));
    }
    Some(((h * 60 + mi) * 60 + s) * 1_000_000 + micros)
}

fn split_ymd(text: &str) -> Option<(i64, u32, u32)> {
    let mut parts = text.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    (1..=12).contains(&m).then_some(())?;
    (1..=31).contains(&d).then_some(())?;
    Some((y, m, d))
}

/// Days from the civil epoch, per Howard Hinnant's algorithm.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn days_from_civil(y: i64, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) as i32
}
