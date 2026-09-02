//! A function of several arguments, each an array or a number.
//!
//! # Why one wrapper rather than one per shape
//!
//! A two-sample test takes two series. A regression takes a design matrix, a target series and
//! a predictor count. A ridge takes those and a penalty. Writing a wrapper per shape produces
//! four nearly identical files that drift, and the fifth shape arrives next week.
//!
//! So every argument arrives as a `Vec<f64>` --- a number becoming a vector of one --- and the
//! kernel indexes what it needs. Uniform, and the arity check catches the caller who supplied
//! the wrong count before any of it is read.

use arrow_array::{Array, ArrayRef, FixedSizeListArray, Float64Array, Int64Array, ListArray};
use arrow_schema::DataType;
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDFImpl, Signature, Volatility,
};
use std::sync::Arc;

/// A kernel over several arrays.
pub type MultiKernel =
    Arc<dyn Fn(&[Vec<f64>]) -> std::result::Result<f64, String> + Send + Sync>;

/// One many-argument function, wired to the planner.
pub struct Multi {
    name: &'static str,
    arity: usize,
    kernel: MultiKernel,
    signature: Signature,
}

impl std::fmt::Debug for Multi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Multi")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Multi {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.arity == other.arity
    }
}

impl Eq for Multi {}

impl std::hash::Hash for Multi {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.arity.hash(state);
    }
}

impl Multi {
    /// A function of `arity` arguments, each an array or a number.
    pub fn new(
        name: &'static str,
        arity: usize,
        kernel: impl Fn(&[Vec<f64>]) -> std::result::Result<f64, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            arity,
            kernel: Arc::new(kernel),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for Multi {
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
                match values_at(array, row, self.name)? {
                    None => {
                        any_null = true;
                        break;
                    }
                    Some(values) => operands.push(values),
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

/// One argument at one row, as a vector --- a number becoming a vector of one.
fn values_at(array: &ArrayRef, row: usize, function: &str) -> Result<Option<Vec<f64>>> {
    if array.is_null(row) {
        return Ok(None);
    }
    let inner: ArrayRef = match array.data_type() {
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
        DataType::Float64 => {
            let Some(doubles) = array.as_any().downcast_ref::<Float64Array>() else {
                return exec_err!("{function}: expected doubles");
            };
            return Ok(Some(vec![doubles.value(row)]));
        }
        DataType::Int64 => {
            let Some(ints) = array.as_any().downcast_ref::<Int64Array>() else {
                return exec_err!("{function}: expected integers");
            };
            #[allow(clippy::cast_precision_loss)]
            return Ok(Some(vec![ints.value(row) as f64]));
        }
        other => {
            return exec_err!(
                "{function} needs arrays of doubles or numbers, and this argument is {other}. \
                 Refused rather than coerced: a coercion computes a real answer from the \
                 wrong thing"
            )
        }
    };
    let Some(doubles) = inner.as_any().downcast_ref::<Float64Array>() else {
        return exec_err!("{function} needs doubles, and this array holds {}", inner.data_type());
    };
    Ok(Some((0..doubles.len()).map(|i| doubles.value(i)).collect()))
}

/// One number out of an argument that should be a single value.
///
/// Named rather than inlined because a kernel reading `a[2][0]` to get a penalty is a kernel
/// that will read `a[2][1]` by accident, and the failure would be silent.
pub fn one(operands: &[Vec<f64>], at: usize, what: &str) -> std::result::Result<f64, String> {
    match operands.get(at).and_then(|values| values.first()) {
        Some(value) => Ok(*value),
        None => Err(format!("argument {} must be a number: {what}", at + 1)),
    }
}
