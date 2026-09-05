//! A column mask, as an expression the plan cannot decline.
//!
//! # Why this is not built out of the string functions
//!
//! The obvious implementation of [`Mask::Partial`] is
//! `concat(repeat('*', length(x) - keep), right(x, keep))`, assembled from the functions the
//! session already registers. It was rejected for two reasons, and the first is a hole rather
//! than a preference.
//!
//! **`concat` treats a null as the empty string.** A null `email` would therefore come back as
//! `***` --- a masked value where there was no value, which is not obscuring data but inventing
//! it. A reader cannot tell the two apart, and the one place that must never blur them is the
//! one that decides what a principal is allowed to know.
//!
//! **And the semantics would be DataFusion's rather than ours.** What `repeat` does with a
//! negative count, what `length` counts on a multi-byte string, whether an optimizer folds any
//! of it --- each is a decision made elsewhere that this crate would be quietly depending on.
//! A mask is a security control; the arithmetic of one belongs where it can be read.
//!
//! # What each mask means
//!
//! - [`Mask::Null`] --- every value becomes null, at the column's own type. Applies to a column
//!   of any type.
//! - [`Mask::Constant`] --- every value becomes the constant, **including the nulls**. Leaving a
//!   null as null would publish which rows have no value, and "this customer has no email
//!   address" is a fact about that customer.
//! - [`Mask::Partial`] --- all but the last `keep` characters become `*`, and **a null stays
//!   null**, for the reason `concat` was rejected: there is no tail to show, and showing `***`
//!   would be inventing one. Characters, not bytes, so a name in a non-Latin script is masked
//!   to its own length rather than to the length of its encoding.
//!
//! `Constant` and `Partial` produce text and are refused at open time on a column that does not
//! hold text --- see [`Masking::over`]. `Null` is refused nowhere, because it is meaningful
//! everywhere.

use arrow_array::{Array, ArrayRef, LargeStringArray, StringArray};
use arrow_schema::{DataType, Schema};
use datafusion::common::{plan_err, Result};
use datafusion::logical_expr::ColumnarValue;
use datafusion::physical_expr::PhysicalExpr;
use sankhya_authz::policy::Mask;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::sync::Arc;

/// One column, obscured.
#[derive(Debug, Clone)]
pub struct Masked {
    /// The value as the scan produced it.
    input: Arc<dyn PhysicalExpr>,
    /// What to show instead.
    mask: Mask,
    /// The type the column had, so a null keeps it.
    ///
    /// Held rather than recomputed because [`PhysicalExpr::data_type`] is asked on a schema
    /// that a later rule may have narrowed, and a mask that changed a column's type depending
    /// on where in the plan it was asked would be a mask that breaks the plan rather than the
    /// disclosure.
    kind: DataType,
}

impl Masked {
    /// Obscure `input`, which the caller has already checked is of a type the mask can take.
    pub fn new(input: Arc<dyn PhysicalExpr>, mask: Mask, kind: DataType) -> Self {
        Self { input, mask, kind }
    }
}

/// Written out rather than derived, because `Arc<dyn PhysicalExpr>` compares through the
/// trait object and the derive tries to move it. Equality is what the plan cache keys on:
/// two masks agree when they obscure the same input the same way, at the same type.
impl PartialEq for Masked {
    fn eq(&self, other: &Self) -> bool {
        *self.input == *other.input && self.mask == other.mask && self.kind == other.kind
    }
}

impl Eq for Masked {}

impl Hash for Masked {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.input.hash(state);
        self.mask.hash(state);
        self.kind.hash(state);
    }
}

impl Display for Masked {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.mask {
            Mask::Null => write!(f, "mask_null({})", self.input),
            Mask::Partial { keep } => write!(f, "mask_partial({}, keep={keep})", self.input),
            Mask::Constant { value } => write!(f, "mask_constant({}, '{value}')", self.input),
        }
    }
}

impl PhysicalExpr for Masked {
    fn data_type(&self, _input_schema: &Schema) -> Result<DataType> {
        Ok(self.kind.clone())
    }

    fn nullable(&self, input_schema: &Schema) -> Result<bool> {
        match self.mask {
            // A column every value of which is null is null whatever the column was.
            Mask::Null => Ok(true),
            // A constant is never null, whatever the column was --- that is the point of it.
            Mask::Constant { .. } => Ok(false),
            // A partial mask preserves nulls, so it preserves nullability too. Claiming
            // otherwise would let a later rule prune a null check that is still needed.
            Mask::Partial { .. } => self.input.nullable(input_schema),
        }
    }

    fn evaluate(&self, batch: &arrow_array::RecordBatch) -> Result<ColumnarValue> {
        let rows = batch.num_rows();
        let values = self.input.evaluate(batch)?.into_array(rows)?;
        let masked: ArrayRef = match &self.mask {
            Mask::Null => arrow_array::new_null_array(&self.kind, values.len()),
            Mask::Constant { value } => constant(value, &self.kind, values.len()),
            Mask::Partial { keep } => partial(&values, *keep)?,
        };
        Ok(ColumnarValue::Array(masked))
    }

    fn children(&self) -> Vec<&Arc<dyn PhysicalExpr>> {
        vec![&self.input]
    }

    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn PhysicalExpr>>,
    ) -> Result<Arc<dyn PhysicalExpr>> {
        let Some(input) = children.into_iter().next() else {
            // Refused rather than kept, and this is the important direction. A rewrite that
            // handed this expression no child and got the old one back would be a mask whose
            // input the plan and this expression disagree about, which is how a mask comes to
            // obscure a column nobody is reading any more.
            return plan_err!("a column mask has exactly one input and was given none");
        };
        Ok(Arc::new(Self {
            input,
            mask: self.mask.clone(),
            kind: self.kind.clone(),
        }))
    }

    fn fmt_sql(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

/// Every row, the same text.
fn constant(value: &str, kind: &DataType, rows: usize) -> ArrayRef {
    match kind {
        DataType::LargeUtf8 => Arc::new(LargeStringArray::from(vec![value; rows])),
        // Utf8 by construction: `Masking::over` refuses any other type for this mask, and a
        // type that reached here regardless is text-shaped or the scan would not have
        // produced it.
        _ => Arc::new(StringArray::from(vec![value; rows])),
    }
}

/// All but the last `keep` characters, replaced.
fn partial(values: &ArrayRef, keep: usize) -> Result<ArrayRef> {
    if let Some(strings) = values.as_any().downcast_ref::<StringArray>() {
        return Ok(Arc::new(
            strings
                .iter()
                .map(|value| value.map(|text| hide(text, keep)))
                .collect::<StringArray>(),
        ));
    }
    if let Some(strings) = values.as_any().downcast_ref::<LargeStringArray>() {
        return Ok(Arc::new(
            strings
                .iter()
                .map(|value| value.map(|text| hide(text, keep)))
                .collect::<LargeStringArray>(),
        ));
    }
    // Refused rather than passed through. A mask that gave up and returned the column would
    // be a policy that discloses on a type nobody thought about, silently.
    plan_err!(
        "a partial mask needs text and this column is {}",
        values.data_type()
    )
}

/// One value, with all but its last `keep` characters replaced.
///
/// A value of `keep` characters or fewer is short enough that its tail is the whole of it, and
/// is returned unchanged: there is nothing this mask can hide about it, and returning `***`
/// instead would hide its length, which the mask does not promise to do either way.
fn hide(text: &str, keep: usize) -> String {
    let length = text.chars().count();
    let Some(hidden) = length.checked_sub(keep) else {
        return text.to_owned();
    };
    let mut out = "*".repeat(hidden);
    out.extend(text.chars().skip(hidden));
    out
}

/// The masks a decision imposes, checked against the schema they will be applied to.
///
/// # Why the check is here and not at scan time
///
/// The same reason the row predicate is parsed when the table is opened: a policy that names a
/// column the table does not have, or asks for a partial mask on an integer, is a mistake in
/// the policy. It should fail loudly and once, when the table is opened, rather than on the
/// first query that happens to select that column --- which is a policy that is wrong for
/// months and looks right.
#[derive(Debug, Clone)]
pub struct Masking {
    /// The mask for each column that has one, and the type that column holds.
    columns: Vec<(String, Mask, DataType)>,
}

impl Masking {
    /// Check a decision's masks against the schema of the table it was made about.
    ///
    /// # Errors
    ///
    /// When a mask names a column the table does not have, or asks for text of a column that
    /// does not hold text.
    pub fn over(
        masks: &std::collections::BTreeMap<String, Mask>,
        schema: &arrow_schema::SchemaRef,
    ) -> Result<Self> {
        let mut columns = Vec::new();
        for (name, mask) in masks {
            let Ok(index) = schema.index_of(name) else {
                return plan_err!(
                    "the policy masks '{name}', which this table does not have: a mask on a \
                     column that does not exist obscures nothing and looks like a control"
                );
            };
            let kind = schema.field(index).data_type().clone();
            let textual = matches!(kind, DataType::Utf8 | DataType::LargeUtf8);
            if !textual && !matches!(mask, Mask::Null) {
                return plan_err!(
                    "the policy masks '{name}' with text and the column holds {kind}: a mask \
                     that cannot be applied is not a mask, and `Mask::Null` obscures a column \
                     of any type"
                );
            }
            columns.push((name.clone(), mask.clone(), kind));
        }
        Ok(Self { columns })
    }

    /// Whether any of these masks touches a column of `schema`.
    #[must_use]
    pub fn touches(&self, schema: &Schema) -> bool {
        self.columns
            .iter()
            .any(|(name, _, _)| schema.index_of(name).is_ok())
    }

    /// The mask for a column of the plan's output, if it has one.
    #[must_use]
    pub fn wrap(&self, column: &str, input: Arc<dyn PhysicalExpr>) -> Arc<dyn PhysicalExpr> {
        match self
            .columns
            .iter()
            .find(|(name, _, _)| name.as_str() == column)
        {
            None => input,
            Some((_, mask, kind)) => Arc::new(Masked::new(input, mask.clone(), kind.clone())),
        }
    }

    /// Whether this expression reads a masked column.
    ///
    /// Used to refuse pushdown: a query predicate on a masked column, evaluated below the
    /// mask, is a probe for the value the mask exists to withhold. `WHERE email = 'a@b.c'`
    /// returning a row says what the mask says it will not.
    #[must_use]
    pub fn reads_a_masked_column(&self, columns: &[String]) -> bool {
        columns
            .iter()
            .any(|column| self.columns.iter().any(|(name, _, _)| name == column))
    }
}
