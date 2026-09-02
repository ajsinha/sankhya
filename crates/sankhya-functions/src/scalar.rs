//! A function of numbers that returns a number.
//!
//! # Why this is written out rather than using `create_udf`
//!
//! `create_udf` takes an **exact** argument type, so a function declared over `Float64` is not
//! reached by a statement that passed an integer literal --- and `norm_cdf(1)` is a thing
//! everybody writes. Accepting any argument and checking when the kernel runs puts the
//! message where the value is, so a refusal can say which argument was wrong and what it was.

use arrow_array::{Array, ArrayRef, Float64Array, Int64Array};
use arrow_schema::DataType;
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDFImpl, Signature, Volatility,
};
use std::sync::Arc;

/// A kernel over a fixed number of numbers.
pub type NumericKernel = Arc<dyn Fn(&[f64]) -> std::result::Result<f64, String> + Send + Sync>;

/// One numeric function, wired to the planner.
pub struct Numeric {
    name: &'static str,
    arity: usize,
    kernel: NumericKernel,
    signature: Signature,
}

impl std::fmt::Debug for Numeric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Numeric")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Numeric {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.arity == other.arity
    }
}

impl Eq for Numeric {}

impl std::hash::Hash for Numeric {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.arity.hash(state);
    }
}

impl Numeric {
    /// A function of `arity` numbers.
    pub fn new(
        name: &'static str,
        arity: usize,
        kernel: impl Fn(&[f64]) -> std::result::Result<f64, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            arity,
            kernel: Arc::new(kernel),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for Numeric {
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
        if args.args.len() != self.arity {
            return exec_err!(
                "{} takes {} argument(s) and was given {}",
                self.name,
                self.arity,
                args.args.len()
            );
        }
        let rows = args
            .args
            .iter()
            .filter_map(|arg| match arg {
                ColumnarValue::Array(array) => Some(array.len()),
                ColumnarValue::Scalar(_) => None,
            })
            .max()
            .unwrap_or(1);
        let arrays: Vec<ArrayRef> = args
            .args
            .iter()
            .map(|arg| arg.clone().into_array(rows))
            .collect::<Result<_>>()?;

        let mut out: Vec<Option<f64>> = Vec::with_capacity(rows);
        for row in 0..rows {
            let mut operands = Vec::with_capacity(self.arity);
            let mut any_null = false;
            for array in &arrays {
                match number_at(array, row, self.name)? {
                    // A null argument gives a null result. Substituting zero would compute a
                    // real number from a value nobody supplied, which is the wrong answer that
                    // looks most like a right one.
                    None => {
                        any_null = true;
                        break;
                    }
                    Some(value) => operands.push(value),
                }
            }
            if any_null {
                out.push(None);
                continue;
            }
            match (self.kernel)(&operands) {
                Ok(value) => out.push(Some(value)),
                Err(reason) => return exec_err!("{}: {reason}", self.name),
            }
        }
        Ok(ColumnarValue::Array(Arc::new(Float64Array::from(out))))
    }
}

/// One number out of an array, whatever numeric type it arrived as.
///
/// Integers are accepted because `norm_cdf(1)` is what a person writes, and refusing it would
/// be pedantry with a syntax error attached. A *non-numeric* column is refused rather than
/// parsed: reading a string as a number here would compute a real answer from text somebody
/// never meant as a number.
fn number_at(array: &ArrayRef, row: usize, function: &str) -> Result<Option<f64>> {
    if array.is_null(row) {
        return Ok(None);
    }
    if let Some(doubles) = array.as_any().downcast_ref::<Float64Array>() {
        return Ok(Some(doubles.value(row)));
    }
    if let Some(ints) = array.as_any().downcast_ref::<Int64Array>() {
        #[allow(clippy::cast_precision_loss)]
        return Ok(Some(ints.value(row) as f64));
    }
    if let Some(floats) = array.as_any().downcast_ref::<arrow_array::Float32Array>() {
        return Ok(Some(f64::from(floats.value(row))));
    }
    if let Some(ints) = array.as_any().downcast_ref::<arrow_array::Int32Array>() {
        return Ok(Some(f64::from(ints.value(row))));
    }
    exec_err!(
        "{function} needs numbers, and this argument is {}. Refused rather than coerced: a \
         coercion here computes a real answer from something nobody meant as a number",
        array.data_type()
    )
}
