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
//! # Why completeness is stored beside the cells, and as columns
//!
//! A set of cells cannot say how much of the fact table reached it. A row that hydration
//! could not place, or that policy withheld, **leaves no trace**: counting what arrived and
//! dividing by what arrived gives one, always. So [`Completeness`] has to be carried, and a
//! cuboid read back without it could only claim completeness it never measured --- the exact
//! trap [`crate::complete`] documents, and one this crate has already walked into once.
//!
//! It is stored as two columns rather than as Arrow schema metadata, and that was measured
//! rather than assumed: metadata does not survive the read path the server uses. A probe
//! wrote a batch with metadata through `sankhya_table::write_parquet`, read it back through
//! DataFusion, and got `{}`. Storing completeness where a reader cannot see it would have
//! been worse than not storing it, because the absence would have looked like a value.
//!
//! Two columns of a value constant within the file cost almost nothing once encoded, and
//! they are visible to anything that opens the table --- which is the open-storage
//! commitment applying to the fact that a number is partial, not only to the number.
//!
//! # Why a list column rather than a scalar
//!
//! The expansion is a handful of non-overlapping doubles whose sum is exact. Storing their
//! total would round; storing their count in separate columns would fix a width the algorithm
//! does not have. A list is the shape of the thing.

use crate::cells::{Address, Cells, WrongWidth};
use crate::complete::Completeness;
use arrow_array::builder::{Float64Builder, ListBuilder, StringBuilder};
use arrow_array::{Array, ListArray, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_cube_algo::measure::Rule;
use sankhya_math::Exact;
use std::sync::Arc;

/// The column holding the unrounded expansion.
///
/// Prefixed, because a cuboid's other columns are dimension names chosen by whoever modelled
/// the cube and a collision would silently replace a dimension with an aggregate.
pub const EXACT: &str = "__sankhya_exact";

/// The column holding how many rows contributed to these cells.
pub const CONTRIBUTED: &str = "__sankhya_contributed";

/// The column holding how many rows were withheld from them.
pub const WITHHELD: &str = "__sankhya_withheld";

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
    /// The completeness columns are absent, or disagree between rows.
    ///
    /// Not defaulted to complete. A cuboid that cannot say what it saw is a cuboid whose
    /// answer nobody can qualify, and serving it as complete is the failure this whole
    /// column exists to prevent.
    NoCompleteness {
        /// What the batch does hold.
        found: Vec<String>,
    },
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
            Self::NoCompleteness { found } => write!(
                f,
                "the stored cuboid has no `{CONTRIBUTED}`/`{WITHHELD}` columns agreeing on \
                 one value; it holds {found:?}. A cuboid that cannot say how much of the \
                 fact table reached it can only claim completeness it never measured"
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
    // Constant within a cuboid, and stored per row anyway: encoded, a constant column costs
    // almost nothing, and it is the only place a reader is guaranteed to find it.
    fields.push(Field::new(CONTRIBUTED, DataType::UInt64, false));
    fields.push(Field::new(WITHHELD, DataType::UInt64, false));
    Arc::new(Schema::new(fields))
}

/// Cells as a batch, with every aggregate unrounded and their completeness beside them.
///
/// `completeness` is required rather than optional. It cannot be derived from `cells` --- a
/// withheld or unplaceable row leaves no trace --- so a default here would let every cuboid
/// nobody thought about report itself complete.
///
/// # Errors
///
/// Returns an Arrow error only if the columns cannot be assembled, which means the cells
/// disagreed with their own declared dimensions.
pub fn to_batch(
    cells: &Cells,
    rule: Rule,
    completeness: &Completeness,
) -> Result<RecordBatch, arrow_schema::ArrowError> {
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
        // The value **under this measure's rule**, and this is `COR-05`.
        //
        // This stored `contributions.exact_sum()` and used `rule` only in the tainted
        // fallback below. `from_batch` reads back with `add_reduced`, and `Contributions::
        // reduce` early-returns the stored value for *every* rule --- so a measure declared
        // `MAX ALONG region` and maintained answered `70.0` over facts `30.0, 40.0` where the
        // live path answers `40.0`, and `MEAN` answered `70.0` against `35.0`.
        //
        // This is the 2026-09-01 defect that `cube_rules.rs` was written to pin, resurrected
        // one layer down: every `store` test passed `Rule::Sum`, and every `cube_rules` test
        // declared its cube without `MAINTAINED`.
        //
        // Refused rather than defaulted when the rule has no reduction from partials
        // (`Rule::None`, `Rule::Supplied`): a cuboid that cannot be computed here is one this
        // layer must not invent, and writing a zero would be materialisation turning a refusal
        // into a number, which is the shape of the whole finding.
        let Some(reduced) = contributions.reduce(rule) else {
            continue;
        };
        for (index, member) in address.iter().enumerate() {
            if let Some(builder) = members.get_mut(index) {
                builder.append_value(member);
            }
        }

        // Only a sum composes without rounding, so only a sum is stored as an expansion. Every
        // other rule reduces to one number here, and rolling *that* up further is governed by
        // `answerable_from`, which already refuses the rules that do not decompose.
        let stored = if matches!(rule, Rule::Sum) {
            contributions.exact_sum()
        } else {
            Exact::zero()
        };
        for component in stored.components() {
            exact.values().append_value(*component);
        }
        // A tainted expansion — a non-finite value arrived — has no components, and its
        // rounded total is the honest answer. Stored as a single-element list so reading it
        // back gives the same number rather than an empty sum of zero. The same applies to
        // every non-sum rule, whose value is a scalar by construction.
        if stored.components().is_empty() {
            exact.values().append_value(reduced);
        }
        exact.append(true);
    }

    let mut columns: Vec<arrow_array::ArrayRef> = members
        .into_iter()
        .map(|mut builder| Arc::new(builder.finish()) as arrow_array::ArrayRef)
        .collect();
    let exact = exact.finish();
    let rows = exact.len();
    columns.push(Arc::new(exact));
    columns.push(Arc::new(UInt64Array::from(vec![
        completeness.contributed();
        rows
    ])));
    columns.push(Arc::new(UInt64Array::from(vec![
        completeness.withheld();
        rows
    ])));
    RecordBatch::try_new(schema, columns)
}

/// A batch read back as cells and the completeness they were computed under, rounding once.
///
/// Both, together, because they are only meaningful together: cells without their
/// completeness are a number nobody can qualify, and this signature is what stops a caller
/// from getting one without the other.
///
/// # Errors
///
/// [`NotCells`] when a dimension column, the expansion column or the completeness columns are
/// missing, or when a row addresses the wrong number of dimensions.
pub fn from_batch(
    batch: &RecordBatch,
    dimensions: &[String],
    rule: Rule,
) -> Result<(Cells, Completeness), NotCells> {
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

    // Read before the cells, so a cuboid that cannot say what it saw is refused rather than
    // half-read. Every row must agree: the value is constant within a cuboid by construction,
    // so rows that disagree mean the file was assembled by something that did not know that,
    // and picking the first would be choosing which of two claims to believe.
    let completeness = one_completeness(batch, &names)?;

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
    Ok((cells, completeness))
}

/// The one completeness every row of a cuboid must agree on.
///
/// An empty batch has no rows to read it from, and the honest answer is `Completeness::of(0,
/// 0)` --- nothing contributed and nothing withheld, whose `fraction()` is `None`. That is the
/// absent-versus-complete distinction the rest of this crate keeps: an aggregate over no rows
/// is not a complete aggregate, and rounding it up to complete is how an empty result passes a
/// threshold. An error would be wrong too, because reading an empty batch is not a failure.
///
/// It does not arise on disk in any case: `cuboid::materialise` refuses to write an empty
/// cuboid, because one is indistinguishable on the way back from a cube that saw nothing.
fn one_completeness(batch: &RecordBatch, names: &[String]) -> Result<Completeness, NotCells> {
    let column = |name: &str| {
        batch
            .column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
            .ok_or_else(|| NotCells::NoCompleteness {
                found: names.to_vec(),
            })
    };
    let contributed = column(CONTRIBUTED)?;
    let withheld = column(WITHHELD)?;
    if batch.num_rows() == 0 {
        return Ok(Completeness::of(0, 0));
    }
    let (first_contributed, first_withheld) = (contributed.value(0), withheld.value(0));
    let agrees = (0..batch.num_rows()).all(|row| {
        contributed.value(row) == first_contributed && withheld.value(row) == first_withheld
    });
    if !agrees {
        return Err(NotCells::NoCompleteness {
            found: names.to_vec(),
        });
    }
    Ok(Completeness::of(first_contributed, first_withheld))
}
