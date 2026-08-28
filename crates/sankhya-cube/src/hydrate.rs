//! Building a cube from published data.
//!
//! # What this is, and why it was missing
//!
//! A [`Definition`](crate::model::Definition) names a fact table and the columns its
//! dimensions join on. Validating it checks those names are well formed. **It does not read
//! them**, and until this module existed nothing did: every `Cells` in the crate was built
//! by a navigation operation or by a test fixture, so the cube was an algebra over data the
//! caller supplied.
//!
//! That gap survived eight passing exit criteria, because every criterion supplied its own
//! cells. A test that never has to read a published table cannot tell you whether the cube
//! can.
//!
//! # A row that cannot be placed is counted, never dropped
//!
//! This is the property worth building the module around.
//!
//! Hydration turns rows into cells by reading each dimension's key column. Some rows will
//! not have one --- a null region, a key of a type the column does not hold. The convenient
//! handling is to skip them, and the result is a cube whose totals are quietly short by
//! however many rows were skipped. Every figure it produces is then wrong, plausible, and
//! made of real data, which is the same failure as a policy-filtered total presented as
//! complete.
//!
//! So unplaced rows are counted and returned as a [`Completeness`], which is the type that
//! already refuses to let a partial figure be reported as a total. The two problems have
//! different causes --- one is policy, one is data quality --- and exactly the same
//! consequence, so they get the same machinery.
//!
//! # A null member is not a member named ""
//!
//! Placing a null key under the empty string invents a member that is not in the dimension
//! table, and it then appears in results, in drill-downs, and in reconciliations, as a real
//! thing with real money against it. A null key means the row cannot be placed. Callers who
//! genuinely want an "unknown" bucket should have one **in the dimension table**, where it
//! is a member somebody declared.

use crate::cells::{Address, Cells};
use crate::complete::Completeness;
use crate::model::Cube;
use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int64Type};
use arrow_array::{Array, RecordBatch};
use arrow_schema::DataType;
use sankhya_cube_algo::measure::Measure;
use std::fmt;

/// What one batch contributed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Absorbed {
    /// Rows read.
    pub rows: u64,
    /// Rows placed in a cell.
    pub placed: u64,
    /// Rows that could not be placed: a null dimension key, or a null measure.
    pub unplaced: u64,
}

impl Absorbed {
    /// Two batches combined.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        Self {
            rows: self.rows.saturating_add(other.rows),
            placed: self.placed.saturating_add(other.placed),
            unplaced: self.unplaced.saturating_add(other.unplaced),
        }
    }

    /// How much of the intended input reached the cube.
    ///
    /// The same type policy filtering produces, deliberately. A total short by the rows
    /// hydration could not place is wrong in exactly the way a policy-filtered total is
    /// wrong, and it should refuse to be reported as a total for exactly the same reason.
    #[must_use]
    pub const fn completeness(&self) -> Completeness {
        Completeness::of(self.placed, self.unplaced)
    }
}

/// Read a batch of fact rows into `cells`.
///
/// `cells` must already be over the cube's dimensions, in the cube's order --- see
/// [`empty_for`].
///
/// # Errors
/// [`NotHydratable`] when a column the cube names is absent from the batch or holds a type
/// no member key can be read from. Both are refused rather than skipped: a missing dimension
/// column silently groups every row under one member, and a total computed that way is a
/// number nobody can trace.
pub fn absorb(
    cube: &Cube,
    measure: &Measure,
    batch: &RecordBatch,
    cells: &mut Cells,
) -> Result<Absorbed, NotHydratable> {
    let mut keys: Vec<&dyn Array> = Vec::with_capacity(cube.dimensions().len());
    for dimension in cube.dimensions() {
        let column = batch.column_by_name(&dimension.joins_on).ok_or_else(|| {
            NotHydratable::MissingColumn {
                dimension: dimension.name.clone(),
                column: dimension.joins_on.clone(),
                found: batch.schema().fields().iter().map(|f| f.name().clone()).collect(),
            }
        })?;
        if !readable_key(column.data_type()) {
            return Err(NotHydratable::UnreadableKey {
                column: dimension.joins_on.clone(),
                found: column.data_type().to_string(),
            });
        }
        keys.push(column.as_ref());
    }

    let values = batch
        .column_by_name(&measure.name)
        .ok_or_else(|| NotHydratable::MissingMeasure {
            measure: measure.name.to_string(),
            found: batch.schema().fields().iter().map(|f| f.name().clone()).collect(),
        })?;
    let values = match values.data_type() {
        DataType::Float64 => Numbers::Float(values.as_primitive::<Float64Type>()),
        DataType::Int64 => Numbers::Int(values.as_primitive::<Int64Type>()),
        other => {
            return Err(NotHydratable::UnreadableMeasure {
                measure: measure.name.to_string(),
                found: other.to_string(),
            })
        }
    };

    let mut absorbed = Absorbed {
        rows: batch.num_rows() as u64,
        ..Absorbed::default()
    };
    for row in 0..batch.num_rows() {
        let Some(address) = address_of(&keys, row) else {
            absorbed.unplaced = absorbed.unplaced.saturating_add(1);
            continue;
        };
        let Some(value) = values.at(row) else {
            absorbed.unplaced = absorbed.unplaced.saturating_add(1);
            continue;
        };
        // The width is the cube's own, so this cannot fail; if it somehow does, the row is
        // unplaced rather than filed somewhere nobody addressed.
        if cells.add(address, value).is_err() {
            absorbed.unplaced = absorbed.unplaced.saturating_add(1);
            continue;
        }
        absorbed.placed = absorbed.placed.saturating_add(1);
    }
    Ok(absorbed)
}

/// An empty cube of cells over a cube's dimensions, in the cube's order.
#[must_use]
pub fn empty_for(cube: &Cube) -> Cells {
    Cells::over(cube.dimension_names().iter().map(|d| (*d).to_string()).collect())
}

/// Whether member keys can be read from this type.
fn readable_key(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Int64 | DataType::Int32
    )
}

/// The address of one row, or `None` if any key is null.
///
/// All-or-nothing on purpose. A row with three good keys and one null cannot be placed at
/// all: putting it at a partial address would file it in some other cell's total.
fn address_of(keys: &[&dyn Array], row: usize) -> Option<Address> {
    let mut address = Address::with_capacity(keys.len());
    for column in keys {
        if column.is_null(row) {
            return None;
        }
        address.push(member_at(*column, row)?);
    }
    Some(address)
}

/// One member key, rendered.
fn member_at(column: &dyn Array, row: usize) -> Option<String> {
    match column.data_type() {
        DataType::Utf8 => Some(column.as_string::<i32>().value(row).to_string()),
        DataType::LargeUtf8 => Some(column.as_string::<i64>().value(row).to_string()),
        DataType::Int64 => Some(column.as_primitive::<Int64Type>().value(row).to_string()),
        DataType::Int32 => Some(
            column
                .as_primitive::<arrow_array::types::Int32Type>()
                .value(row)
                .to_string(),
        ),
        _ => None,
    }
}

/// A measure column, whichever numeric type it is.
enum Numbers<'a> {
    Float(&'a arrow_array::PrimitiveArray<Float64Type>),
    Int(&'a arrow_array::PrimitiveArray<Int64Type>),
}

impl Numbers<'_> {
    /// The value at a row, or `None` if it is null.
    ///
    /// A null measure is unplaced rather than zero. A fact with no amount is not a fact with
    /// an amount of nothing, and the difference reaches the total.
    fn at(&self, row: usize) -> Option<f64> {
        match self {
            Self::Float(values) => (!values.is_null(row)).then(|| values.value(row)),
            #[allow(clippy::cast_precision_loss)]
            Self::Int(values) => (!values.is_null(row)).then(|| values.value(row) as f64),
        }
    }
}

/// Why a batch could not be read into a cube.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NotHydratable {
    /// A dimension's join column is not in the fact table.
    MissingColumn {
        /// The dimension.
        dimension: String,
        /// The column it joins on.
        column: String,
        /// The columns that are there, so a rename is one glance from being found.
        found: Vec<String>,
    },
    /// A join column holds a type no member key can be read from.
    UnreadableKey {
        /// The column.
        column: String,
        /// What it holds.
        found: String,
    },
    /// The measure column is not in the fact table.
    MissingMeasure {
        /// The measure.
        measure: String,
        /// The columns that are there.
        found: Vec<String>,
    },
    /// The measure column is not numeric.
    UnreadableMeasure {
        /// The measure.
        measure: String,
        /// What its column holds.
        found: String,
    },
}

impl fmt::Display for NotHydratable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColumn { dimension, column, found } => write!(
                f,
                "dimension '{}' joins on column '{}', which the fact table does not have — \
                 it has {:?}. Refused rather than skipped: a missing dimension column groups \
                 every row under one member, and the total is then untraceable",
                dimension, column, found
            ),
            Self::UnreadableKey { column, found } => write!(
                f,
                "column '{column}' holds {found}, which no member key can be read from"
            ),
            Self::MissingMeasure { measure, found } => write!(
                f,
                "measure '{measure}' has no column in the fact table — it has {found:?}"
            ),
            Self::UnreadableMeasure { measure, found } => write!(
                f,
                "measure '{measure}' is held as {found}, which is not a number"
            ),
        }
    }
}

impl std::error::Error for NotHydratable {}
