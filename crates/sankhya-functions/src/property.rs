//! A function taking an array and returning one number.
//!
//! The third shape, after "numbers to a number" and "array to an array". A matrix property ---
//! is it symmetric, is it positive definite, what is its rank --- reads a whole matrix and
//! answers with a single value, and neither of the other two wrappers fits.

use arrow_array::{ArrayRef, Float64Array};
use arrow_schema::DataType;
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDFImpl, Signature, Volatility,
};
use std::sync::Arc;

/// A kernel reading a flat array and answering with a number.
pub type PropertyKernel = Arc<dyn Fn(&[f64]) -> std::result::Result<f64, String> + Send + Sync>;

/// One array-to-number function, wired to the planner.
pub struct Property {
    name: &'static str,
    kernel: PropertyKernel,
    signature: Signature,
}

impl std::fmt::Debug for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Property").field("name", &self.name).finish_non_exhaustive()
    }
}

impl PartialEq for Property {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Property {}

impl std::hash::Hash for Property {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl Property {
    /// A property of one array.
    pub fn new(
        name: &'static str,
        kernel: impl Fn(&[f64]) -> std::result::Result<f64, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            kernel: Arc::new(kernel),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for Property {
    fn name(&self) -> &str {
        self.name
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arguments: &[DataType]) -> Result<DataType> {
        Ok(DataType::Float64)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let Some(first) = args.args.first() else {
            return exec_err!("{} takes one array", self.name);
        };
        let rows = match first {
            ColumnarValue::Array(array) => array.len(),
            ColumnarValue::Scalar(_) => 1,
        };
        let array = first.clone().into_array(rows)?;

        // Borrowed rather than copied. A `FixedSizeList` stores its rows end to end, so a row
        // is a slice with a known stride --- and copying one per row was measured at 25x the
        // cost for a narrow column. See `rows`.
        let mut vectors = crate::rows::Vectors::read(&array, self.name)?;
        let mut out: Vec<Option<f64>> = Vec::with_capacity(rows);
        for row in 0..rows {
            match vectors.row(row) {
                None => out.push(None),
                Some(values) => match (self.kernel)(values) {
                    Ok(value) => out.push(Some(value)),
                    Err(reason) => return exec_err!("{}: {reason}", self.name),
                },
            }
        }
        Ok(ColumnarValue::Array(Arc::new(Float64Array::from(out))))
    }
}
