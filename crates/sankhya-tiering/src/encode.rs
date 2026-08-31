//! The canonical byte encoding verification compares over.
//!
//! [`policy::canonical_encoding`](crate::policy::canonical_encoding) decides *whether* a type
//! may be archived. This is the encoding it was promising exists, and the property it has to
//! carry is the one stated there:
//!
//! > **Two values are equal if and only if they encode to the same bytes.**
//!
//! # Three ways an encoding loses that property by accident
//!
//! **Untagged values.** `Int32(1)` and `Int64(1)` are different values. If both encode to
//! `01 00 00 00 …` under a scheme that writes the integer and nothing else, an archive whose
//! schema drifted one width verifies as faithful. Every value here carries its type tag first,
//! so a width change is a mismatch rather than a coincidence.
//!
//! **Unprefixed variable-length values.** `["ab", "c"]` and `["a", "bc"]` are different keys
//! that concatenate to the same bytes. Every value is length-prefixed, so a composite key is a
//! function of its parts rather than of their concatenation.
//!
//! **Null as an empty value.** A null and an empty string are different values, and a scheme
//! that writes zero bytes for both cannot tell an archive that dropped a value from one that
//! preserved an empty one. Null has its own presence byte, distinct from a zero length.
//!
//! # Why this is not `serde`
//!
//! Nothing here is deserialised. The encoding exists to be hashed, and a format that can be
//! read back invites somebody to read it back --- at which point its stability becomes a
//! compatibility obligation across versions rather than a property of one run. Both sides of a
//! verification are encoded by the same binary in the same run, which is the only
//! compatibility this needs.

use arrow_array::{
    Array, BinaryArray, BooleanArray, Date32Array, Decimal128Array, FixedSizeBinaryArray,
    Int16Array, Int32Array, Int64Array, StringArray, Time64MicrosecondArray,
    TimestampMicrosecondArray,
};
use arrow_schema::DataType;
use std::fmt;

/// A value could not be encoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unencodable {
    /// The column's Arrow type is not one this encoding covers.
    ///
    /// Reached when an archive's physical schema does not match what the policy declared ---
    /// which is itself the finding, and a mismatch rather than a crash.
    UnsupportedType {
        /// The column.
        column: String,
        /// What its array actually is.
        found: String,
    },
    /// The column's array is not the concrete type its Arrow type promised.
    ///
    /// Only reachable through a downcast failing on an array whose `data_type` said otherwise,
    /// which means something built the batch inconsistently.
    Malformed {
        /// The column.
        column: String,
    },
}

impl fmt::Display for Unencodable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedType { column, found } => write!(
                f,
                "column `{column}` has type {found}, which has no canonical byte encoding --- \
                 the policy that admitted this table should have refused it"
            ),
            Self::Malformed { column } => write!(
                f,
                "column `{column}` is not the array type its schema declares"
            ),
        }
    }
}

/// The tag byte that opens every encoded value.
///
/// Distinct per type so that equal bit patterns under different types do not collide. The
/// values are fixed rather than derived from enum order: a reordering of [`DataType`] or of
/// this list must not silently change what an archive's checksum is compared against.
mod tag {
    pub(super) const BOOLEAN: u8 = 0x01;
    pub(super) const INT16: u8 = 0x02;
    pub(super) const INT32: u8 = 0x03;
    pub(super) const INT64: u8 = 0x04;
    pub(super) const DECIMAL: u8 = 0x05;
    pub(super) const UTF8: u8 = 0x06;
    pub(super) const BINARY: u8 = 0x07;
    pub(super) const TIMESTAMP_UTC: u8 = 0x08;
    pub(super) const TIMESTAMP_LOCAL: u8 = 0x09;
    pub(super) const DATE: u8 = 0x0a;
    pub(super) const TIME: u8 = 0x0b;
    pub(super) const UUID: u8 = 0x0c;
}

/// Written after the tag when the value is null, in place of the value.
const NULL: u8 = 0x00;
/// Written after the tag when the value is present.
const PRESENT: u8 = 0x01;

/// Append the canonical encoding of `row` in `array` to `out`.
///
/// # Errors
///
/// [`Unencodable`] when the array's type is outside the canonical set, or when its concrete
/// type disagrees with the type it declares.
pub fn value(
    out: &mut Vec<u8>,
    column: &str,
    array: &dyn Array,
    row: usize,
) -> Result<(), Unencodable> {
    let tag = tag_for(column, array.data_type())?;
    out.push(tag);

    if array.is_null(row) {
        out.push(NULL);
        return Ok(());
    }
    out.push(PRESENT);

    let malformed = || Unencodable::Malformed { column: column.to_string() };
    match array.data_type() {
        DataType::Boolean => {
            let a = array.as_any().downcast_ref::<BooleanArray>().ok_or_else(malformed)?;
            out.push(u8::from(a.value(row)));
        }
        DataType::Int16 => {
            let a = array.as_any().downcast_ref::<Int16Array>().ok_or_else(malformed)?;
            out.extend_from_slice(&a.value(row).to_be_bytes());
        }
        DataType::Int32 => {
            let a = array.as_any().downcast_ref::<Int32Array>().ok_or_else(malformed)?;
            out.extend_from_slice(&a.value(row).to_be_bytes());
        }
        DataType::Int64 => {
            let a = array.as_any().downcast_ref::<Int64Array>().ok_or_else(malformed)?;
            out.extend_from_slice(&a.value(row).to_be_bytes());
        }
        DataType::Decimal128(digits, scale) => {
            let a = array.as_any().downcast_ref::<Decimal128Array>().ok_or_else(malformed)?;
            // Precision and scale are part of the value. `1.0` at scale 1 and `1.00` at scale
            // 2 have the same numeric value and different unscaled integers, so encoding the
            // integer alone would make a rescaled archive verify as faithful.
            out.push(*digits);
            out.extend_from_slice(&scale.to_be_bytes());
            out.extend_from_slice(&a.value(row).to_be_bytes());
        }
        DataType::Utf8 => {
            let a = array.as_any().downcast_ref::<StringArray>().ok_or_else(malformed)?;
            prefixed(out, a.value(row).as_bytes());
        }
        DataType::Binary => {
            let a = array.as_any().downcast_ref::<BinaryArray>().ok_or_else(malformed)?;
            prefixed(out, a.value(row));
        }
        DataType::Timestamp(_, _) => {
            let a =
                array.as_any().downcast_ref::<TimestampMicrosecondArray>().ok_or_else(malformed)?;
            out.extend_from_slice(&a.value(row).to_be_bytes());
        }
        DataType::Date32 => {
            let a = array.as_any().downcast_ref::<Date32Array>().ok_or_else(malformed)?;
            out.extend_from_slice(&a.value(row).to_be_bytes());
        }
        DataType::Time64(_) => {
            let a =
                array.as_any().downcast_ref::<Time64MicrosecondArray>().ok_or_else(malformed)?;
            out.extend_from_slice(&a.value(row).to_be_bytes());
        }
        DataType::FixedSizeBinary(_) => {
            let a = array.as_any().downcast_ref::<FixedSizeBinaryArray>().ok_or_else(malformed)?;
            prefixed(out, a.value(row));
        }
        // Unreachable: `tag_for` has already refused everything else.
        other => {
            return Err(Unencodable::UnsupportedType {
                column: column.to_string(),
                found: other.to_string(),
            });
        }
    }
    Ok(())
}

/// The tag for a column's type, or a refusal.
///
/// The zone on a timestamp distinguishes the tag rather than being encoded, because
/// `TimestampUtc` and `TimestampLocal` are different logical types whose microsecond values
/// are indistinguishable. Conflating them is how a column shifts by hours, and an archive that
/// lost the zone would otherwise checksum identically to the source that had it.
fn tag_for(column: &str, data_type: &DataType) -> Result<u8, Unencodable> {
    Ok(match data_type {
        DataType::Boolean => tag::BOOLEAN,
        DataType::Int16 => tag::INT16,
        DataType::Int32 => tag::INT32,
        DataType::Int64 => tag::INT64,
        DataType::Decimal128(_, _) => tag::DECIMAL,
        DataType::Utf8 => tag::UTF8,
        DataType::Binary => tag::BINARY,
        DataType::Timestamp(_, Some(_)) => tag::TIMESTAMP_UTC,
        DataType::Timestamp(_, None) => tag::TIMESTAMP_LOCAL,
        DataType::Date32 => tag::DATE,
        DataType::Time64(_) => tag::TIME,
        DataType::FixedSizeBinary(16) => tag::UUID,
        other => {
            return Err(Unencodable::UnsupportedType {
                column: column.to_string(),
                found: other.to_string(),
            });
        }
    })
}

/// Length-prefixed, so a concatenation is a function of its parts.
///
/// Eight bytes rather than a varint: the length is hashed, never parsed, and a fixed width has
/// no encoding of its own to get wrong.
fn prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
}
