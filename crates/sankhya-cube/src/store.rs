//! Cells as rows, so a materialised cuboid is an ordinary table.
//!
//! # Why the stored value is not a number
//!
//! A materialised cuboid is a cache, and the one property a cache must have is that it does
//! not change the answer. M7's exit criterion 3a says so in the strongest available terms:
//! *every query returns bit-identical results with materialisation on and off*, compared by
//! bits rather than within a tolerance.
//!
//! Storing a rounded aggregate breaks that, and breaks it invisibly. A cube rolls up in
//! stages and every stage rounds, so `round(round(a+b) + round(c+d))` is not
//! `round(a+b+c+d)`. Fixing the *order* of summation makes one reduction reproducible and
//! does nothing about **associativity** — and a materialised cuboid is precisely a
//! re-association of the same addition. The discrepancy was measured at one ULP: large enough
//! for two reports to disagree by a penny, small enough that nobody can point at a defect.
//!
//! So a cell is stored as the **components of its Shewchuk expansion** — the unrounded exact
//! sum — and rounded once when read. [`Exact::components`] exists for this.
//!
//! # Why a list column rather than a scalar
//!
//! The expansion is a handful of non-overlapping doubles whose sum is exact. Storing their
//! total would round; storing their count in separate columns would fix a width the algorithm
//! does not have. A list is the shape of the thing.

use crate::cells::{Address, Cells, WrongWidth};
use arrow_array::builder::{Float64Builder, ListBuilder, StringBuilder};
use arrow_array::{Array, ListArray, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_cube_algo::measure::Rule;
use sankhya_math::Exact;
use std::sync::Arc;

/// The column holding the unrounded expansion.
///
/// Prefixed, because a cuboid's other columns are dimension names chosen by whoever modelled
/// the cube and a collision would silently replace a dimension with an aggregate.
pub const EXACT: &str = "__sankhya_exact";

/// Why a batch could not be read back as cells.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NotCells {
    /// A dimension the cuboid declares is not a column of the batch.
    MissingDimension {
        /// The dimension.
        dimension: String,
        /// What the batch does hold.
        found: Vec<String>,
    },
    /// The expansion column is absent or not a list of doubles.
    MissingExact {
        /// What the batch does hold.
        found: Vec<String>,
    },
    /// A row addressed the wrong number of dimensions.
    Width(WrongWidth),
}

impl std::fmt::Display for NotCells {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDimension { dimension, found } => write!(
                f,
                "the stored cuboid has no column for dimension `{dimension}`; it holds \
                 {found:?}. Reading it would group every row under one member and report a \
                 total that looks right"
            ),
            Self::MissingExact { found } => write!(
                f,
                "the stored cuboid has no `{EXACT}` column of doubles; it holds {found:?}. \
                 Without the unrounded expansion a materialised answer cannot be \
                 bit-identical to the base one, which is the only reason to trust it"
            ),
            Self::Width(width) => write!(f, "{width}"),
        }
    }
}

impl std::error::Error for NotCells {}

/// The schema a cuboid over these dimensions is stored with.
#[must_use]
pub fn schema_for(dimensions: &[String]) -> SchemaRef {
    let mut fields: Vec<Field> = dimensions
        .iter()
        .map(|name| Field::new(name, DataType::Utf8, false))
        .collect();
    fields.push(Field::new(
        EXACT,
        // The item field is nullable because `ListBuilder`'s default values builder is, and
        // a schema that disagrees with the builder rejects the batch it just built.
        DataType::List(Arc::new(Field::new("item", DataType::Float64, true))),
        false,
    ));
    Arc::new(Schema::new(fields))
}

/// Cells as a batch, with every aggregate unrounded.
///
/// # Errors
///
/// Returns an Arrow error only if the columns cannot be assembled, which means the cells
/// disagreed with their own declared dimensions.
pub fn to_batch(cells: &Cells, rule: Rule) -> Result<RecordBatch, arrow_schema::ArrowError> {
    let schema = schema_for(cells.dimensions());
    let width = cells.dimensions().len();

    let mut members: Vec<StringBuilder> = (0..width).map(|_| StringBuilder::new()).collect();
    let mut exact = ListBuilder::new(Float64Builder::new());

    // Sorted, because `Cells` is a `BTreeMap` and iterating it is already in order. Stated
    // rather than relied upon silently: two runs materialising the same cuboid must produce
    // byte-identical files, or a digest over them is not a comparison of the data.
    for address in cells.addresses() {
        let Some(contributions) = cells.contributions(address) else {
            continue;
        };
        for (index, member) in address.iter().enumerate() {
            if let Some(builder) = members.get_mut(index) {
                builder.append_value(member);
            }
        }
        let sum = contributions.exact_sum();
        for component in sum.components() {
            exact.values().append_value(*component);
        }
        // A tainted expansion — a non-finite value arrived — has no components, and its
        // rounded total is the honest answer. Stored as a single-element list so reading it
        // back gives the same number rather than an empty sum of zero.
        if sum.components().is_empty() {
            exact.values().append_value(contributions.reduce(rule).unwrap_or(0.0));
        }
        exact.append(true);
    }

    let mut columns: Vec<arrow_array::ArrayRef> = members
        .into_iter()
        .map(|mut builder| Arc::new(builder.finish()) as arrow_array::ArrayRef)
        .collect();
    columns.push(Arc::new(exact.finish()));
    RecordBatch::try_new(schema, columns)
}

/// A batch read back as cells, rounding once.
///
/// # Errors
///
/// [`NotCells`] when a dimension column or the expansion column is missing, or when a row
/// addresses the wrong number of dimensions.
pub fn from_batch(
    batch: &RecordBatch,
    dimensions: &[String],
    rule: Rule,
) -> Result<Cells, NotCells> {
    let names: Vec<String> = batch
        .schema()
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect();

    let mut columns: Vec<&StringArray> = Vec::with_capacity(dimensions.len());
    for dimension in dimensions {
        let column = batch
            .column_by_name(dimension)
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| NotCells::MissingDimension {
                dimension: dimension.clone(),
                found: names.clone(),
            })?;
        columns.push(column);
    }
    let exact = batch
        .column_by_name(EXACT)
        .and_then(|column| column.as_any().downcast_ref::<ListArray>())
        .ok_or_else(|| NotCells::MissingExact {
            found: names.clone(),
        })?;

    let mut cells = Cells::over(dimensions.to_vec());
    for row in 0..batch.num_rows() {
        let address: Address = columns
            .iter()
            .map(|column| column.value(row).to_string())
            .collect();
        let components = exact.value(row);
        let values = components
            .as_any()
            .downcast_ref::<arrow_array::Float64Array>()
            .ok_or_else(|| NotCells::MissingExact {
                found: names.clone(),
            })?;
        // Rebuilt from the components rather than from their total. `Exact::of` over an
        // existing expansion reproduces it, so the sum read back is the sum written.
        let restored = Exact::of(&(0..values.len()).map(|i| values.value(i)).collect::<Vec<f64>>());
        // Structurally unreachable: the address above is built from exactly the requested
        // dimensions, so its width cannot disagree with theirs. Propagated rather than
        // unwrapped because a panic in a reader is never the right answer, and left without
        // a catalogue entry because a mutation removing it survives --- correctly, since no
        // input can reach it.
        cells
            .add_reduced(address, rule, restored)
            .map_err(NotCells::Width)?;
    }
    Ok(cells)
}
