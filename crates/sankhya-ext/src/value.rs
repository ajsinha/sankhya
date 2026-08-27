//! The values a pack function sees, and the types it declares.
//!
//! # Why these are not Arrow's types
//!
//! This crate carries the only stability commitment in the repository, and re-exporting
//! another project's types would hand that commitment to that project. Arrow's `DataType`
//! has changed shape across releases; each time it does, every pack ever written would need
//! recompiling, and the version SANKHYA pins would become a fact packs have to track.
//!
//! There is a real cost --- a conversion at the boundary --- and it buys the thing the
//! extension mechanism exists for: a pack compiled against version 1 of this crate keeps
//! working when the engine underneath it is replaced.
//!
//! The set is deliberately small. Every type here is one a pack can reason about without
//! knowing how it is stored, and nothing here exposes a buffer, an offset or a null bitmap.

use std::fmt;

/// A type a pack function declares in its signature.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum LogicalType {
    /// True or false.
    Boolean,
    /// A 64-bit signed integer.
    Integer,
    /// A 64-bit float.
    ///
    /// Present for measurements. Anything whose sum must be reproducible should use
    /// [`LogicalType::Decimal`] instead --- floating-point addition is not associative, so
    /// two runs that partition the data differently disagree.
    Real,
    /// An exact number with a fixed number of fractional digits.
    Decimal {
        /// Total digits.
        precision: u8,
        /// Digits after the point.
        scale: u8,
    },
    /// Text.
    Text,
    /// Opaque bytes.
    Bytes,
    /// An instant, as microseconds from the epoch.
    ///
    /// One unit, always. Offering a choice of unit means every pack has to ask which one it
    /// was given, and the ones that forget are wrong by a factor of a thousand.
    Instant,
    /// A value of any type, decided at call time.
    ///
    /// Used only where a function genuinely accepts anything, which is rare. A signature
    /// reaching for this to avoid enumerating its types has given up the checking that
    /// makes a signature worth declaring.
    Any,
}

impl LogicalType {
    /// Whether a value of this type can be supplied where `wanted` is declared.
    #[must_use]
    pub fn satisfies(&self, wanted: &Self) -> bool {
        if matches!(wanted, Self::Any) || self == wanted {
            return true;
        }
        // Integers widen into reals and decimals, and nothing narrows. A narrowing
        // conversion loses information silently, which is the one thing a type system in
        // this position exists to prevent.
        matches!(
            (self, wanted),
            (Self::Integer, Self::Real | Self::Decimal { .. })
        )
    }
}

impl fmt::Display for LogicalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean => f.write_str("boolean"),
            Self::Integer => f.write_str("integer"),
            Self::Real => f.write_str("real"),
            Self::Decimal { precision, scale } => write!(f, "decimal({precision},{scale})"),
            Self::Text => f.write_str("text"),
            Self::Bytes => f.write_str("bytes"),
            Self::Instant => f.write_str("instant"),
            Self::Any => f.write_str("any"),
        }
    }
}

/// One value passed to or returned from a pack function.
///
/// Null is a variant rather than an `Option` wrapper, so a pack cannot forget to handle it.
/// An `Option<Value>` invites `unwrap_or_default`, and a null silently becoming zero is a
/// wrong answer rather than a missing one.
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    /// Absent.
    Null,
    /// True or false.
    Boolean(bool),
    /// A 64-bit signed integer.
    Integer(i64),
    /// A 64-bit float.
    Real(f64),
    /// An exact number, held as unscaled units and a scale.
    Decimal {
        /// The value, times ten to the scale.
        units: i128,
        /// Digits after the point.
        scale: u8,
    },
    /// Text.
    Text(String),
    /// Opaque bytes.
    Bytes(Vec<u8>),
    /// An instant, as microseconds from the epoch.
    Instant(i64),
}

impl Value {
    /// The type of this value, or `None` for null.
    ///
    /// Null has no type of its own: it is a valid value of every type, and giving it one
    /// would make a null integer and a null text distinguishable when they are not.
    #[must_use]
    pub const fn logical_type(&self) -> Option<LogicalType> {
        Some(match self {
            Self::Null => return None,
            Self::Boolean(_) => LogicalType::Boolean,
            Self::Integer(_) => LogicalType::Integer,
            Self::Real(_) => LogicalType::Real,
            Self::Decimal { units: _, scale } => LogicalType::Decimal {
                precision: 38,
                scale: *scale,
            },
            Self::Text(_) => LogicalType::Text,
            Self::Bytes(_) => LogicalType::Bytes,
            Self::Instant(_) => LogicalType::Instant,
        })
    }

    /// Whether this is null.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// As an integer, if it is one.
    #[must_use]
    pub const fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// As text, if it is text.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// As a real, widening an integer if needed.
    #[must_use]
    pub fn as_real(&self) -> Option<f64> {
        match self {
            Self::Real(x) => Some(*x),
            #[allow(clippy::cast_precision_loss)]
            Self::Integer(n) => Some(*n as f64),
            _ => None,
        }
    }

    /// As an instant, if it is one.
    #[must_use]
    pub const fn as_instant(&self) -> Option<i64> {
        match self {
            Self::Instant(t) => Some(*t),
            _ => None,
        }
    }
}
