//! The table a feed lands in, and turning bound rows into a batch for it.
//!
//! # The schema comes from the declaration, never from the data
//!
//! A schema read off the first document is a schema that changes when the first document does
//! --- and the change is invisible, because the new one is just as well-formed as the old.
//! Every column here comes from what somebody wrote down, which is also what makes a
//! document that does not match it *refusable*: there is something to refuse it against.

use crate::bind::{Cell, Row};
use crate::validate::{Feed, Shaped};
use arrow_array::builder::{
    BooleanBuilder, Date32Builder, Decimal128Builder, FixedSizeBinaryBuilder, Float32Builder,
    Float64Builder, Int16Builder, Int32Builder, Int64Builder, StringBuilder,
    Time64MicrosecondBuilder, TimestampMicrosecondBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{Field, Schema};
use sankhya_schema::LogicalType;
use std::sync::Arc;

/// The Arrow schema of the table a feed lands in.
///
/// The declared columns and nothing else. `sank_data_date` is not here: `sankhya-publish`
/// owns the date axis, stamps the column onto each partition file, and would find a
/// hand-written one either redundant or --- worse --- disagreeing with the axis it was told
/// about.
#[must_use]
pub fn table_schema(feed: &Feed) -> Schema {
    Schema::new(
        feed.columns()
            .iter()
            .map(|column| {
                Field::new(&column.name, column.logical.arrow_type(), column.nullable)
            })
            .collect::<Vec<_>>(),
    )
}

/// Why bound rows could not be assembled into a batch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Unassembled {
    /// What went wrong.
    pub detail: String,
}

impl std::fmt::Display for Unassembled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the batch could not be assembled: {}", self.detail)
    }
}

impl std::error::Error for Unassembled {}

/// Assemble bound rows into a batch for this feed's table.
///
/// # Errors
///
/// [`Unassembled`] when a cell is not the kind its column reads --- which means the binder
/// and this module disagree, rather than anything being wrong with the data.
pub fn batch(feed: &Feed, rows: &[Row]) -> Result<RecordBatch, Unassembled> {
    let mut columns: Vec<ArrayRef> = Vec::with_capacity(feed.columns().len());
    for (index, column) in feed.columns().iter().enumerate() {
        columns.push(column_of(column, rows, index)?);
    }
    RecordBatch::try_new(Arc::new(table_schema(feed)), columns)
        .map_err(|error| Unassembled { detail: error.to_string() })
}

/// One column's array.
#[allow(clippy::too_many_lines)]
fn column_of(column: &Shaped, rows: &[Row], index: usize) -> Result<ArrayRef, Unassembled> {
    let cells = || rows.iter().map(move |row| row.cells.get(index));
    let wrong = |cell: Option<&Cell>| Unassembled {
        detail: format!(
            "`{}` reads {:?} and the bound row holds {cell:?}",
            column.name, column.logical
        ),
    };

    Ok(match column.logical {
        LogicalType::Boolean => {
            let mut builder = BooleanBuilder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    Some(Cell::Boolean(value)) => builder.append_value(*value),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Int16 => {
            let mut builder = Int16Builder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    Some(Cell::Integer(value)) => builder.append_value(
                        i16::try_from(*value).map_err(|_| wrong(cell))?,
                    ),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Int32 => {
            let mut builder = Int32Builder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    Some(Cell::Integer(value)) => builder.append_value(
                        i32::try_from(*value).map_err(|_| wrong(cell))?,
                    ),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Int64 => {
            let mut builder = Int64Builder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    Some(Cell::Integer(value)) => builder.append_value(*value),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Float32 => {
            let mut builder = Float32Builder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    // Narrowed here rather than at binding, so the value the quarantine
                    // would have shown is the one the source sent.
                    Some(Cell::Real(value)) => builder.append_value(*value as f32),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Float64 => {
            let mut builder = Float64Builder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    Some(Cell::Real(value)) => builder.append_value(*value),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Decimal(precision) => {
            let mut builder = Decimal128Builder::with_capacity(rows.len())
                .with_precision_and_scale(
                    precision.digits,
                    i8::try_from(precision.scale).unwrap_or(0),
                )
                .map_err(|error| Unassembled { detail: error.to_string() })?;
            for cell in cells() {
                match cell {
                    Some(Cell::Decimal(value)) => builder.append_value(*value),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Utf8 | LogicalType::Json => {
            let mut builder = StringBuilder::with_capacity(rows.len(), rows.len() * 16);
            for cell in cells() {
                match cell {
                    Some(Cell::Text(value)) => builder.append_value(value),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Date => {
            let mut builder = Date32Builder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    Some(Cell::Days(value)) => builder.append_value(*value),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::TimestampUtc | LogicalType::TimestampLocal => {
            let mut builder = TimestampMicrosecondBuilder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    Some(Cell::Micros(value)) => builder.append_value(*value),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            // The zone is part of the type, and it is what keeps "the moment this happened"
            // separate from "the wall clock somebody read".
            let finished = if matches!(column.logical, LogicalType::TimestampUtc) {
                builder.finish().with_timezone("UTC")
            } else {
                builder.finish()
            };
            Arc::new(finished)
        }
        LogicalType::Time => {
            let mut builder = Time64MicrosecondBuilder::with_capacity(rows.len());
            for cell in cells() {
                match cell {
                    Some(Cell::Micros(value)) => builder.append_value(*value),
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        LogicalType::Uuid => {
            let mut builder = FixedSizeBinaryBuilder::with_capacity(rows.len(), 16);
            for cell in cells() {
                match cell {
                    Some(Cell::Uuid(value)) => builder
                        .append_value(value)
                        .map_err(|error| Unassembled { detail: error.to_string() })?,
                    Some(Cell::Null) => builder.append_null(),
                    other => return Err(wrong(other)),
                }
            }
            Arc::new(builder.finish())
        }
        // Unreachable: the binder refuses a binary column, so no bound row can hold one.
        // Written as a refusal because an unreachable arm that panics is a claim, and this
        // one would be made in a builder nobody is watching.
        LogicalType::Binary => {
            return Err(Unassembled {
                detail: format!(
                    "`{}` is a binary column, which a feed cannot read from a document",
                    column.name
                ),
            })
        }
    })
}
