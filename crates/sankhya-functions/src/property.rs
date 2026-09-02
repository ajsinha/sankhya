//! A function taking an array and returning one number.
//!
//! The third shape, after "numbers to a number" and "array to an array". A matrix property ---
//! is it symmetric, is it positive definite, what is its rank --- reads a whole matrix and
//! answers with a single value, and neither of the other two wrappers fits.

use arrow_array::{Array, ArrayRef, FixedSizeListArray, Float64Array, ListArray};
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

        let mut out: Vec<Option<f64>> = Vec::with_capacity(rows);
        for row in 0..rows {
            match flat_at(&array, row, self.name)? {
                None => out.push(None),
                Some(values) => match (self.kernel)(&values) {
                    Ok(value) => out.push(Some(value)),
                    Err(reason) => return exec_err!("{}: {reason}", self.name),
                },
            }
        }
        Ok(ColumnarValue::Array(Arc::new(Float64Array::from(out))))
    }
}

/// One row's flat array of doubles.
fn flat_at(array: &ArrayRef, row: usize, function: &str) -> Result<Option<Vec<f64>>> {
    if array.is_null(row) {
        return Ok(None);
    }
    let values: ArrayRef = match array.data_type() {
        DataType::FixedSizeList(_, _) => {
            let Some(list) = array.as_any().downcast_ref::<FixedSizeListArray>() else {
                return exec_err!("{function}: expected a fixed-size list");
            };
            list.value(row)
        }
        DataType::List(_) => {
            let Some(list) = array.as_any().downcast_ref::<ListArray>() else {
                return exec_err!("{function}: expected a list");
            };
            list.value(row)
        }
        other => {
            return exec_err!(
                "{function} needs an array of doubles, and this column is {other}. Refused \
                 rather than coerced: a coercion computes a real answer from the wrong thing"
            )
        }
    };
    let Some(doubles) = values.as_any().downcast_ref::<Float64Array>() else {
        return exec_err!(
            "{function} needs an array of doubles, and this one holds {}",
            values.data_type()
        );
    };
    Ok(Some((0..doubles.len()).map(|i| doubles.value(i)).collect()))
}
