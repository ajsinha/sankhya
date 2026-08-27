//! Translating an Arrow schema into the log's schema string.
//!
//! # Why this refuses rather than approximates
//!
//! The log's schema is what an external engine believes the table contains. A type
//! translated to something merely similar produces a table other engines read
//! confidently and wrongly — a decimal read as a float loses exactness silently, and a
//! microsecond timestamp read as milliseconds is off by a factor of a thousand with no
//! indication.
//!
//! So every type is either translated exactly or refused by name. Refusing means the
//! table is not published, which is visible; approximating means it is published and
//! wrong, which is not.

use arrow_schema::{DataType, Field, Schema, TimeUnit};
use std::fmt;

/// A type with no exact representation in the log.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnsupportedType {
    pub field: String,
    pub arrow_type: String,
}

impl fmt::Display for UnsupportedType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "column {} has type {}, which has no exact representation in the table log; \
             publishing it as something merely similar would produce a table other \
             engines read confidently and wrongly",
            self.field, self.arrow_type
        )
    }
}

impl std::error::Error for UnsupportedType {}

/// The log's name for an Arrow type.
fn type_name(data_type: &DataType) -> Option<String> {
    let name = match data_type {
        DataType::Boolean => "boolean".to_string(),
        DataType::Int8 => "byte".to_string(),
        DataType::Int16 => "short".to_string(),
        DataType::Int32 => "integer".to_string(),
        DataType::Int64 | DataType::UInt64 => "long".to_string(),
        DataType::Float32 => "float".to_string(),
        DataType::Float64 => "double".to_string(),
        DataType::Decimal128(precision, scale) => {
            // Negative scale has no representation here, and silently clamping it would
            // move the decimal point.
            let scale = u8::try_from(*scale).ok()?;
            format!("decimal({precision},{scale})")
        }
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "string".to_string(),
        DataType::Binary
        | DataType::LargeBinary
        | DataType::BinaryView
        | DataType::FixedSizeBinary(_) => "binary".to_string(),
        DataType::Date32 => "date".to_string(),
        // Only microseconds. The protocol's timestamp is microsecond-precision, so a
        // nanosecond column would lose three digits and a millisecond column would gain
        // three zeroes of false precision. Both are refused.
        DataType::Timestamp(TimeUnit::Microsecond, _) => "timestamp".to_string(),
        _ => return None,
    };
    Some(name)
}

fn field_json(field: &Field) -> Result<String, UnsupportedType> {
    let name = type_name(field.data_type()).ok_or_else(|| UnsupportedType {
        field: field.name().clone(),
        arrow_type: format!("{}", field.data_type()),
    })?;

    Ok(format!(
        r#"{{"name":{},"type":{},"nullable":{},"metadata":{{}}}}"#,
        serde_json::to_string(field.name()).unwrap_or_else(|_| "\"\"".to_string()),
        serde_json::to_string(&name).unwrap_or_else(|_| "\"\"".to_string()),
        field.is_nullable()
    ))
}

/// The schema string for a table's log.
///
/// # Errors
///
/// Returns [`UnsupportedType`] naming the first column that cannot be represented
/// exactly. The column is named because "the schema is unsupported" is not something an
/// operator can act on.
pub fn schema_string(schema: &Schema) -> Result<String, UnsupportedType> {
    let fields: Result<Vec<String>, UnsupportedType> =
        schema.fields().iter().map(|f| field_json(f)).collect();
    Ok(format!(
        r#"{{"type":"struct","fields":[{}]}}"#,
        fields?.join(",")
    ))
}

/// The Arrow type a log's type name denotes.
///
/// The inverse of [`type_name`], and deliberately not its mirror image. `long` becomes
/// `Int64` and never `UInt64`, because the log cannot tell them apart: both are written as
/// `long`, so a reader has to pick one and picking the signed one is the choice that
/// preserves every value the log can hold.
///
/// A column written as `UInt64` therefore reads back as `Int64`. That is lossless for every
/// value below 2^63 and would silently wrap above it, which is why the writer's own bound
/// on those columns matters --- and why this is written down here rather than discovered.
fn arrow_type(name: &str) -> Option<DataType> {
    let data_type = match name {
        "boolean" => DataType::Boolean,
        "byte" => DataType::Int8,
        "short" => DataType::Int16,
        "integer" => DataType::Int32,
        "long" => DataType::Int64,
        "float" => DataType::Float32,
        "double" => DataType::Float64,
        "string" => DataType::Utf8,
        "binary" => DataType::Binary,
        "date" => DataType::Date32,
        "timestamp" => DataType::Timestamp(TimeUnit::Microsecond, None),
        other => {
            let inner = other.strip_prefix("decimal(")?.strip_suffix(')')?;
            let (precision, scale) = inner.split_once(',')?;
            DataType::Decimal128(
                precision.trim().parse::<u8>().ok()?,
                scale.trim().parse::<i8>().ok()?,
            )
        }
    };
    Some(data_type)
}

/// Read a table's schema back out of its log.
///
/// A server reads tables it did not write --- on restart, or written by another node --- so
/// the schema has to come from the log rather than from whoever happened to create it.
///
/// # Errors
///
/// Returns [`UnsupportedType`] naming the first field whose type this reader does not
/// recognise. Named, because "the schema is unsupported" is not something an operator can
/// act on, and skipping the field would produce a table that is quietly missing a column.
pub fn schema_from_string(json: &str) -> Result<Schema, UnsupportedType> {
    #[derive(serde::Deserialize)]
    struct DeltaField {
        name: String,
        #[serde(rename = "type")]
        type_name: serde_json::Value,
        #[serde(default = "default_true")]
        nullable: bool,
    }
    #[derive(serde::Deserialize)]
    struct DeltaSchema {
        fields: Vec<DeltaField>,
    }
    const fn default_true() -> bool {
        true
    }

    let parsed: DeltaSchema = serde_json::from_str(json).map_err(|error| UnsupportedType {
        field: "<the schema itself>".to_string(),
        arrow_type: error.to_string(),
    })?;

    let fields: Result<Vec<Field>, UnsupportedType> = parsed
        .fields
        .into_iter()
        .map(|field| {
            // A nested type arrives as an object rather than a string. Refusing it by name
            // is better than the alternatives: skipping it hides a column, and guessing a
            // flat type for it produces a table other engines read confidently and wrongly.
            let name = field.type_name.as_str().ok_or_else(|| UnsupportedType {
                field: field.name.clone(),
                arrow_type: field.type_name.to_string(),
            })?;
            let data_type = arrow_type(name).ok_or_else(|| UnsupportedType {
                field: field.name.clone(),
                arrow_type: name.to_string(),
            })?;
            Ok(Field::new(field.name, data_type, field.nullable))
        })
        .collect();

    Ok(Schema::new(fields?))
}
