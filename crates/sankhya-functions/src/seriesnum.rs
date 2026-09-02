//! A function of one series and one number.
//!
//! The shape a one-sample test has --- the observations, and the value they are tested
//! against --- and the shape neither of the other wrappers covers.

use arrow_array::{Array, ArrayRef, FixedSizeListArray, Float64Array, Int64Array, ListArray};
use arrow_schema::DataType;
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDFImpl, Signature, Volatility,
};
use std::sync::Arc;

/// A kernel over a series and a number.
pub type PairKernel =
    Arc<dyn Fn(&[f64], f64) -> std::result::Result<f64, String> + Send + Sync>;

/// One series-and-number function, wired to the planner.
pub struct SeriesAndNumber {
    name: &'static str,
    kernel: PairKernel,
    signature: Signature,
}

impl std::fmt::Debug for SeriesAndNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeriesAndNumber").field("name", &self.name).finish_non_exhaustive()
    }
}

impl PartialEq for SeriesAndNumber {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for SeriesAndNumber {}

impl std::hash::Hash for SeriesAndNumber {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl SeriesAndNumber {
    /// A function of one series and one number.
    pub fn new(
        name: &'static str,
        kernel: impl Fn(&[f64], f64) -> std::result::Result<f64, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            kernel: Arc::new(kernel),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for SeriesAndNumber {
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
        if args.args.len() != 2 {
            return exec_err!(
                "{} takes a series and a number, and was given {} argument(s)",
                self.name,
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
        // Both are present: the arity was checked above, and a `get` here would turn a
        // missing argument into a silent default rather than the refusal it already is.
        let (Some(first), Some(second)) = (args.args.first(), args.args.get(1)) else {
            return exec_err!("{} takes a series and a number", self.name);
        };
        let series = first.clone().into_array(rows)?;
        let numbers = second.clone().into_array(rows)?;

        let mut out: Vec<Option<f64>> = Vec::with_capacity(rows);
        for row in 0..rows {
            let (Some(values), Some(number)) =
                (flat_at(&series, row, self.name)?, number_at(&numbers, row, self.name)?)
            else {
                out.push(None);
                continue;
            };
            match (self.kernel)(&values, number) {
                Ok(value) => out.push(Some(value)),
                Err(reason) => return exec_err!("{}: {reason}", self.name),
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
                "{function} needs a series of doubles, and this column is {other}. Refused \
                 rather than coerced: a coercion computes a real answer from the wrong thing"
            )
        }
    };
    let Some(doubles) = values.as_any().downcast_ref::<Float64Array>() else {
        return exec_err!("{function} needs doubles, and this series holds {}", values.data_type());
    };
    Ok(Some((0..doubles.len()).map(|i| doubles.value(i)).collect()))
}

/// One number, whatever numeric type it arrived as.
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
    exec_err!("{function} needs a number, and this argument is {}", array.data_type())
}
