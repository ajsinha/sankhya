//! A function taking a vector or a matrix and returning a vector.
//!
//! # Why the result is a `List` and not a `FixedSizeList`
//!
//! These change the width: the eigenvalues of an `n × n` matrix are `n` numbers from `n²`, and
//! a Cholesky factor is `n²` from `n²`. Declaring a fixed width would make the return type a
//! function of the argument's width, so `mat_eigenvalues` of a 3×3 would be a different
//! function from `mat_eigenvalues` of a 4×4.
//!
//! A `List` costs an offsets buffer and accepts every width, and every vector function reads
//! both — so a factor feeds straight back into another matrix function.

use arrow_array::builder::{Float64Builder, ListBuilder};
use arrow_array::{Array, ArrayRef, FixedSizeListArray, Float64Array, ListArray};
use arrow_schema::{DataType, Field};
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDFImpl, Signature, Volatility,
};
use std::sync::Arc;

/// A kernel over one flat array, returning another.
pub type ArrayKernel =
    Arc<dyn Fn(&[f64]) -> std::result::Result<Vec<f64>, String> + Send + Sync>;

/// One array-to-array function, wired to the planner.
pub struct Series {
    name: &'static str,
    kernel: ArrayKernel,
    signature: Signature,
}

impl std::fmt::Debug for Series {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Series").field("name", &self.name).finish_non_exhaustive()
    }
}

impl PartialEq for Series {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Series {}

impl std::hash::Hash for Series {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl Series {
    /// A function of one array.
    pub fn new(
        name: &'static str,
        kernel: impl Fn(&[f64]) -> std::result::Result<Vec<f64>, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            kernel: Arc::new(kernel),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for Series {
    fn name(&self) -> &str {
        self.name
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arguments: &[DataType]) -> Result<DataType> {
        Ok(DataType::List(Arc::new(Field::new("item", DataType::Float64, true))))
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

        let mut builder = ListBuilder::new(Float64Builder::new());
        for row in 0..rows {
            match flat_at(&array, row, self.name)? {
                // A null matrix gives a null result, never an empty one. An empty result is a
                // definite statement --- "this matrix has no eigenvalues" --- and a missing
                // matrix is not that.
                None => builder.append_null(),
                Some(values) => match (self.kernel)(&values) {
                    Ok(out) => {
                        builder.values().append_slice(&out);
                        builder.append(true);
                    }
                    Err(reason) => return exec_err!("{}: {reason}", self.name),
                },
            }
        }
        Ok(ColumnarValue::Array(Arc::new(builder.finish())))
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
