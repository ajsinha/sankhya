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

/// A kernel over several arrays, giving a number or a series.
///
/// One type rather than two wrappers. A scalar result is a vector of one, which costs a `Vec`
/// per row and buys the catalogue a single many-argument shape instead of a fifth nearly
/// identical file --- see `ADR-0020` Decision 7 on what stops scaling.
pub type MultiKernel =
    Arc<dyn Fn(&[Vec<f64>]) -> std::result::Result<Vec<Option<f64>>, String> + Send + Sync>;

/// One many-argument function, wired to the planner.
pub struct Multi {
    name: &'static str,
    arity: usize,
    /// Whether the answer is a series rather than one number.
    gives_series: bool,
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
            && self.gives_series == other.gives_series
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
            gives_series: false,
            kernel: Arc::new(move |operands| kernel(operands).map(|value| vec![Some(value)])),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }

    /// A function of `arity` arguments that answers with a **series**.
    ///
    /// A rolling statistic is the shape this exists for: a series and a window in, a series
    /// with holes out.
    pub fn series(
        name: &'static str,
        arity: usize,
        kernel: impl Fn(&[Vec<f64>]) -> std::result::Result<Vec<f64>, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            arity,
            gives_series: true,
            kernel: Arc::new(move |operands| {
                kernel(operands).map(|out| {
                    // A `NaN` from the kernel is the carrier for a hole --- a window that did
                    // not reach --- and becomes a null here. A real `NaN` from arithmetic is
                    // indistinguishable and becomes one too, which is the honest reading:
                    // neither is a value.
                    out.into_iter()
                        .map(|value| if value.is_nan() { None } else { Some(value) })
                        .collect()
                })
            }),
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
        if self.gives_series {
            return Ok(DataType::List(Arc::new(arrow_schema::Field::new(
                "item",
                DataType::Float64,
                true,
            ))));
        }
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

        // The buffers are allocated **once** and refilled, not allocated per row.
        //
        // This wrapper holds several arrays at a time, so it cannot borrow each row the way
        // the single-argument wrappers do --- two simultaneous borrows of two readers is a
        // fight with the borrow checker for a shape that is genuinely more complex. Reusing
        // the buffers removes the allocation, which is where nearly all the cost was: see
        // `rows` for the measurement that made the difference visible.
        let mut operands: Vec<Vec<f64>> = vec![Vec::new(); self.arity];
        let mut numbers: Vec<Option<f64>> = Vec::with_capacity(rows);
        let mut series = arrow_array::builder::ListBuilder::new(
            arrow_array::builder::Float64Builder::new(),
        );
        for row in 0..rows {
            let mut any_null = false;
            for (at, array) in arrays.iter().enumerate() {
                let Some(slot) = operands.get_mut(at) else {
                    break;
                };
                slot.clear();
                if !fill(array, row, self.name, slot)? {
                    any_null = true;
                    break;
                }
            }
            if any_null {
                if self.gives_series {
                    series.append_null();
                } else {
                    numbers.push(None);
                }
                continue;
            }
            match (self.kernel)(&operands) {
                Ok(values) => {
                    if self.gives_series {
                        for element in values {
                            series.values().append_option(element);
                        }
                        series.append(true);
                    } else {
                        numbers.push(values.first().copied().flatten());
                    }
                }
                Err(reason) => return exec_err!("{}: {reason}", self.name),
            }
        }
        if self.gives_series {
            return Ok(ColumnarValue::Array(Arc::new(series.finish())));
        }
        Ok(ColumnarValue::Array(Arc::new(Float64Array::from(numbers))))
    }
}

/// Fill `into` with one argument at one row. `false` for a null.
///
/// Takes the buffer rather than returning one, so the caller can reuse it across rows. The
/// allocation was the cost, not the copy: a `Vec` per argument per row is millions of
/// allocations over a scan, and none of them holds anything for longer than one row.
fn fill(array: &ArrayRef, row: usize, function: &str, into: &mut Vec<f64>) -> Result<bool> {
    if array.is_null(row) {
        return Ok(false);
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
            into.push(doubles.value(row));
            return Ok(true);
        }
        DataType::Int64 => {
            let Some(ints) = array.as_any().downcast_ref::<Int64Array>() else {
                return exec_err!("{function}: expected integers");
            };
            #[allow(clippy::cast_precision_loss)]
            into.push(ints.value(row) as f64);
            return Ok(true);
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
    into.extend_from_slice(doubles.values());
    Ok(true)
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
