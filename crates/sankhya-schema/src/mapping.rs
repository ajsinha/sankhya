//! Source type identifiers to logical types.
//!
//! The mapping is exhaustive over what SANKHYA supports and refuses everything else.
//! Growing this table is a deliberate act: each addition is a promise that the type
//! round-trips exactly.

use crate::model::{LogicalType, Precision};
use std::fmt;

/// Why a source type cannot be carried.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MappingError {
    /// The type is not in the supported set.
    Unsupported { oid: u32, reason: &'static str },
    /// The type is supported in principle but this instance of it is not.
    Unrepresentable { oid: u32, detail: String },
}

impl fmt::Display for MappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { oid, reason } => {
                write!(f, "source type {oid} is not supported: {reason}")
            }
            Self::Unrepresentable { oid, detail } => {
                write!(f, "source type {oid} cannot be represented exactly: {detail}")
            }
        }
    }
}

impl std::error::Error for MappingError {}

/// A successful mapping, and what it cost.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeMapping {
    pub logical: LogicalType,
    /// True when the round trip is bit-exact. **Always true**: an inexact mapping is
    /// rejected rather than recorded. The field exists so the guarantee is visible at
    /// every call site rather than implied.
    pub lossless: bool,
}

// Source type identifiers. Stable across versions and part of the wire protocol.
const BOOL: u32 = 16;
const BYTEA: u32 = 17;
const INT8: u32 = 20;
const INT2: u32 = 21;
const INT4: u32 = 23;
const TEXT: u32 = 25;
const JSON: u32 = 114;
const FLOAT4: u32 = 700;
const FLOAT8: u32 = 701;
const VARCHAR: u32 = 1043;
const BPCHAR: u32 = 1042;
const DATE: u32 = 1082;
const TIME: u32 = 1083;
const TIMESTAMP: u32 = 1114;
const TIMESTAMPTZ: u32 = 1184;
const NUMERIC: u32 = 1700;
const UUID: u32 = 2950;
const JSONB: u32 = 3802;

// Deliberately refused, each for a stated reason.
const MONEY: u32 = 790;
const INTERVAL: u32 = 1186;
const TIMETZ: u32 = 1266;

/// Map a source type to a logical type.
///
/// `type_modifier` carries precision and scale where the type has them; `-1` means
/// unconstrained.
///
/// # Errors
///
/// Returns [`MappingError`] for any type that cannot be carried exactly. The caller
/// must surface this at onboarding rather than substituting an approximation.
pub fn map_source_type(oid: u32, type_modifier: i32) -> Result<TypeMapping, MappingError> {
    let logical = match oid {
        BOOL => LogicalType::Boolean,
        INT2 => LogicalType::Int16,
        INT4 => LogicalType::Int32,
        INT8 => LogicalType::Int64,
        FLOAT4 => LogicalType::Float32,
        FLOAT8 => LogicalType::Float64,
        TEXT | VARCHAR | BPCHAR => LogicalType::Utf8,
        BYTEA => LogicalType::Binary,
        DATE => LogicalType::Date,
        TIME => LogicalType::Time,
        TIMESTAMPTZ => LogicalType::TimestampUtc,
        TIMESTAMP => LogicalType::TimestampLocal,
        UUID => LogicalType::Uuid,
        JSON | JSONB => LogicalType::Json,
        NUMERIC => return map_numeric(type_modifier),

        // --- refused, with the reason recorded ---
        MONEY => {
            return Err(MappingError::Unsupported {
                oid,
                reason: "its textual form depends on the server's locale, so the same \
                         value renders differently on different servers and cannot be \
                         reconciled",
            })
        }
        INTERVAL => {
            return Err(MappingError::Unsupported {
                oid,
                reason: "it is a months/days/microseconds triple whose components are \
                         not mutually convertible; flattening it to any single unit \
                         changes its meaning",
            })
        }
        TIMETZ => {
            return Err(MappingError::Unsupported {
                oid,
                reason: "a time with an offset but no date cannot be resolved to an \
                         instant, so any conversion invents information",
            })
        }
        other => {
            return Err(MappingError::Unsupported {
                oid: other,
                reason: "not in the supported set; add it deliberately once its round \
                         trip is proven, or exclude the column",
            })
        }
    };
    Ok(TypeMapping { logical, lossless: true })
}

/// Decimals need their precision, and refuse without it.
fn map_numeric(type_modifier: i32) -> Result<TypeMapping, MappingError> {
    // An unconstrained numeric has arbitrary precision. There is no fixed-width type
    // that carries it faithfully, so it is refused rather than truncated — a silently
    // truncated value looks correct and reconciles against nothing.
    if type_modifier < 0 {
        return Err(MappingError::Unrepresentable {
            oid: NUMERIC,
            detail: "unconstrained numeric has arbitrary precision and cannot be \
                     carried in a fixed-width type. Constrain the column, for example \
                     numeric(18,4), or exclude it"
                .into(),
        });
    }

    // Precision and scale are packed into the modifier, offset by the varlena header.
    let packed = type_modifier.saturating_sub(4);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let digits = ((packed >> 16) & 0xFFFF) as u8;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let scale = (packed & 0xFFFF) as u8;

    let precision = Precision::new(digits, scale).ok_or_else(|| MappingError::Unrepresentable {
        oid: NUMERIC,
        detail: format!(
            "numeric({digits},{scale}) exceeds the {} significant digits a 128-bit \
             decimal can hold exactly",
            Precision::MAX_DIGITS
        ),
    })?;

    Ok(TypeMapping { logical: LogicalType::Decimal(precision), lossless: true })
}

/// Build a type modifier for a decimal, for tests and for schema synthesis.
#[must_use]
pub const fn numeric_modifier(digits: u8, scale: u8) -> i32 {
    (((digits as i32) << 16) | (scale as i32)) + 4
}
