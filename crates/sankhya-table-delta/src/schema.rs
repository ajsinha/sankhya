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
