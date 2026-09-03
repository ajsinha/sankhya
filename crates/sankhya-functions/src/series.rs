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
use arrow_array::ArrayRef;
use arrow_schema::{DataType, Field};
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDFImpl, Signature, Volatility,
};
use std::sync::Arc;

/// A kernel over one flat array, returning another.
///
/// The result is `Option<f64>` per element, because some series genuinely have **holes**: a
/// rolling mean has no answer for the positions its window does not reach, and every plausible
/// invention there is wrong --- zero is a number somebody acts on, the series mean pretends to
/// information that is not there, and repeating the first value makes a flat start that reads
/// as low volatility.
///
/// A `NaN` would not do. `NaN` means *not a number* and travels silently into every arithmetic
/// it touches; a null means *no value*, which is what a window that did not reach produced.
pub type ArrayKernel =
    Arc<dyn Fn(&[f64]) -> std::result::Result<Vec<Option<f64>>, String> + Send + Sync>;

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
    /// A function of one array, every element of which is a value.
    pub fn new(
        name: &'static str,
        kernel: impl Fn(&[f64]) -> std::result::Result<Vec<f64>, String> + Send + Sync + 'static,
    ) -> Self {
        Self::holed(name, move |values| kernel(values).map(|out| out.into_iter().map(Some).collect()))
    }

    /// A function of one array whose result may have **holes**.
    ///
    /// See [`ArrayKernel`] for why a hole is a null rather than a zero or a `NaN`.
    pub fn holed(
        name: &'static str,
        kernel: impl Fn(&[f64]) -> std::result::Result<Vec<Option<f64>>, String>
            + Send
            + Sync
            + 'static,
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

        // Borrowed rather than copied; see `rows` for the measurement.
        let mut vectors = crate::rows::Vectors::read(&array, self.name)?;
        let mut builder = ListBuilder::new(Float64Builder::new());
        for row in 0..rows {
            match vectors.row(row) {
                // A null matrix gives a null result, never an empty one. An empty result is a
                // definite statement --- "this matrix has no eigenvalues" --- and a missing
                // matrix is not that.
                None => builder.append_null(),
                Some(values) => match (self.kernel)(values) {
                    Ok(out) => {
                        for element in out {
                            builder.values().append_option(element);
                        }
                        builder.append(true);
                    }
                    Err(reason) => return exec_err!("{}: {reason}", self.name),
                },
            }
        }
        Ok(ColumnarValue::Array(Arc::new(builder.finish())))
    }
}
