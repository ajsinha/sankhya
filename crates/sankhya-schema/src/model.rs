//! The logical type model.
//!
//! Deliberately independent of any storage system's type names, so that neither the
//! source's vocabulary nor the file format's leaks into the rest of the engine.

use arrow_schema::{DataType, Field as ArrowField, Schema as ArrowSchema, TimeUnit};
use std::sync::Arc;

/// Decimal precision and scale.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Precision {
    pub digits: u8,
    pub scale: u8,
}

impl Precision {
    /// The widest decimal representable without a wider physical type.
    pub const MAX_DIGITS: u8 = 38;

    /// Returns `None` when the precision cannot be represented exactly.
    #[must_use]
    pub const fn new(digits: u8, scale: u8) -> Option<Self> {
        if digits == 0 || digits > Self::MAX_DIGITS || scale > digits {
            None
        } else {
            Some(Self { digits, scale })
        }
    }
}

/// A type SANKHYA can carry faithfully.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LogicalType {
    Boolean,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    /// Exact decimal. The only representation permitted for values that must
    /// reconcile — floating point cannot represent decimal fractions exactly.
    Decimal(Precision),
    Utf8,
    Binary,
    /// Microseconds since the Unix epoch, UTC.
    ///
    /// One representation, always UTC. A source type that carries no zone is mapped
    /// separately so the distinction is never silently lost.
    TimestampUtc,
    /// A wall-clock timestamp with no zone. Kept distinct from [`Self::TimestampUtc`]
    /// because conflating them is how an entire column shifts by hours.
    TimestampLocal,
    Date,
    /// Microseconds since midnight.
    Time,
    Uuid,
    /// A structured document, carried as its canonical text form.
    Json,
}

impl LogicalType {
    /// The physical representation.
    #[must_use]
    pub fn arrow_type(&self) -> DataType {
        match self {
            Self::Boolean => DataType::Boolean,
            Self::Int16 => DataType::Int16,
            Self::Int32 => DataType::Int32,
            Self::Int64 => DataType::Int64,
            Self::Float32 => DataType::Float32,
            Self::Float64 => DataType::Float64,
            Self::Decimal(p) => DataType::Decimal128(p.digits, i8::try_from(p.scale).unwrap_or(0)),
            Self::Utf8 | Self::Json => DataType::Utf8,
            Self::Binary => DataType::Binary,
            // Microseconds, matching the source's own resolution: finer would invent
            // precision, coarser would lose it.
            Self::TimestampUtc => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            Self::TimestampLocal => DataType::Timestamp(TimeUnit::Microsecond, None),
            Self::Date => DataType::Date32,
            Self::Time => DataType::Time64(TimeUnit::Microsecond),
            Self::Uuid => DataType::FixedSizeBinary(16),
        }
    }

    /// Whether values of this type are exact.
    ///
    /// Used to refuse an exact aggregate over an inexact column rather than producing
    /// a number that cannot be reproduced.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        !matches!(self, Self::Float32 | Self::Float64)
    }
}

/// One column.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Field {
    pub name: String,
    pub logical: LogicalType,
    pub nullable: bool,
    /// Whether this column participates in row identity.
    pub is_key: bool,
}

/// A table's logical shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LogicalSchema {
    pub fields: Vec<Field>,
}

impl LogicalSchema {
    #[must_use]
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    /// Columns forming row identity, in declaration order.
    #[must_use]
    pub fn key_fields(&self) -> Vec<&Field> {
        self.fields.iter().filter(|f| f.is_key).collect()
    }

    /// Whether rows can be identified at all.
    ///
    /// Without identity a table can be appended to but not updated or deleted from,
    /// which determines the storage strategy rather than being a mere inconvenience.
    #[must_use]
    pub fn has_identity(&self) -> bool {
        self.fields.iter().any(|f| f.is_key)
    }

    /// The physical schema, with the system columns SANKHYA adds.
    ///
    /// The system columns carry provenance with the data, so the applied position is
    /// recoverable from the table's own history rather than from external state that
    /// could drift out of agreement with it.
    #[must_use]
    pub fn arrow_schema(&self) -> ArrowSchema {
        let mut fields: Vec<Arc<ArrowField>> = self
            .fields
            .iter()
            .map(|f| Arc::new(ArrowField::new(&f.name, f.logical.arrow_type(), f.nullable)))
            .collect();

        fields.push(Arc::new(ArrowField::new(
            "_sankhya_commit_lsn",
            DataType::UInt64,
            false,
        )));
        fields.push(Arc::new(ArrowField::new(
            "_sankhya_commit_ts",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        )));
        fields.push(Arc::new(ArrowField::new("_sankhya_op", DataType::Utf8, false)));

        ArrowSchema::new(fields)
    }

    /// The names SANKHYA reserves.
    ///
    /// A source column colliding with one of these is refused at onboarding rather
    /// than silently shadowed, which would make the provenance column unreadable.
    #[must_use]
    pub const fn system_column_names() -> [&'static str; 3] {
        ["_sankhya_commit_lsn", "_sankhya_commit_ts", "_sankhya_op"]
    }

    /// Whether any column would collide with a reserved name.
    #[must_use]
    pub fn collides_with_system_column(&self) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| Self::system_column_names().contains(&f.name.as_str()))
            .map(|f| f.name.as_str())
    }
}
