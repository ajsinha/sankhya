//! Linear algebra, callable from SQL.
//!
//! ```sql
//! SELECT mat_determinant(covariance) FROM portfolios;
//! SELECT mat_solve(coefficients, observations) FROM systems;
//! ```
//!
//! # Where the shape comes from
//!
//! A matrix is stored flat --- `rows * columns` values in a `FixedSizeList<Float64>` --- so
//! the shape has to come from somewhere. It comes from the **field metadata**, which
//! DataFusion hands a scalar function alongside its arguments.
//!
//! The key is Arrow's canonical `ARROW:extension:metadata` for `arrow.fixed_shape_tensor`,
//! whose value is `{"shape":[rows,columns]}`. Using the canonical form rather than inventing
//! one means an external engine that understands tensors understands these columns, and one
//! that does not sees a plain fixed-size array of the right length.
//!
//! A column with no shape metadata is **refused**, not guessed at. The obvious guess ---
//! square, since `n * n` values often are --- is wrong for every rectangular matrix and
//! produces numbers from values that were never in the same row. Every one of those numbers
//! looks ordinary.

use arrow_array::{Array, ArrayRef, FixedSizeListArray, Float64Array, ListArray};
use arrow_schema::{DataType, Field, FieldRef};
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, Volatility,
};
use datafusion::prelude::SessionContext;
use sankhya_numeric::matrix;
use std::sync::Arc;

/// The Arrow metadata key carrying an extension type's parameters.
const EXTENSION_METADATA: &str = "ARROW:extension:metadata";

/// The Arrow metadata key naming an extension type.
const EXTENSION_NAME: &str = "ARROW:extension:name";

/// The canonical name of Arrow's fixed-shape tensor extension.
pub const TENSOR_EXTENSION: &str = "arrow.fixed_shape_tensor";

/// The field metadata a matrix column of this shape should carry.
///
/// Provided so a publisher can produce columns these functions read, without having to know
/// the spelling. Using Arrow's canonical extension rather than a private key means an
/// engine that understands tensors understands these columns too.
#[must_use]
pub fn tensor_metadata(rows: usize, columns: usize) -> std::collections::HashMap<String, String> {
    let mut metadata = std::collections::HashMap::new();
    metadata.insert(EXTENSION_NAME.to_string(), TENSOR_EXTENSION.to_string());
    metadata.insert(
        EXTENSION_METADATA.to_string(),
        format!(r#"{{"shape":[{rows},{columns}]}}"#),
    );
    metadata
}

/// The shape a field declares, if it declares one.
fn shape_of(field: &FieldRef) -> Option<(usize, usize)> {
    let raw = field.metadata().get(EXTENSION_METADATA)?;
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let shape = parsed.get("shape")?.as_array()?;
    // Two dimensions only. A higher-rank tensor is a coherent thing and is not a matrix,
    // and quietly flattening one would multiply the wrong axes together.
    if shape.len() != 2 {
        return None;
    }
    let rows = usize::try_from(shape.first()?.as_u64()?).ok()?;
    let columns = usize::try_from(shape.get(1)?.as_u64()?).ok()?;
    Some((rows, columns))
}

/// Register every matrix function against a session.
pub fn register(context: &SessionContext) {
    for function in functions() {
        context.register_udf(function);
    }
}

/// Every matrix function this system offers.
#[must_use]
pub fn functions() -> Vec<ScalarUDF> {
    vec![
        ScalarUDF::from(MatrixFunction::new("mat_multiply", Operation::Multiply)),
        ScalarUDF::from(MatrixFunction::new("mat_transpose", Operation::Transpose)),
        ScalarUDF::from(MatrixFunction::new("mat_inverse", Operation::Inverse)),
        ScalarUDF::from(MatrixFunction::new("mat_solve", Operation::Solve)),
        ScalarUDF::from(MatrixFunction::new("mat_vec", Operation::MatVec)),
        ScalarUDF::from(MatrixFunction::new(
            "mat_determinant",
            Operation::Determinant,
        )),
        ScalarUDF::from(MatrixFunction::new("mat_trace", Operation::Trace)),
    ]
}

/// What a matrix function does.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Operation {
    Multiply,
    Transpose,
    Inverse,
    Solve,
    MatVec,
    Determinant,
    Trace,
}

impl Operation {
    /// The shape of the result, given the shapes of the arguments.
    ///
    /// This is what makes `mat_determinant(mat_multiply(a, b))` work. A matrix-returning
    /// function that emitted no shape would produce a run of values the next function
    /// refuses --- and composing these is the first thing anybody does.
    const fn result_shape(
        self,
        first: (usize, usize),
        second: Option<(usize, usize)>,
    ) -> Option<(usize, usize)> {
        let (rows, columns) = first;
        Some(match self {
            Self::Transpose => (columns, rows),
            Self::Inverse => (rows, columns),
            Self::Multiply => match second {
                // The outer dimensions. The inner ones must agree, which the kernel checks.
                Some((_, right_columns)) => (rows, right_columns),
                None => return None,
            },
            // Solving and applying both produce a column vector.
            Self::Solve | Self::MatVec => (rows, 1),
            Self::Determinant | Self::Trace => return None,
        })
    }

    /// How many arguments it takes.
    const fn arity(self) -> usize {
        match self {
            Self::Transpose | Self::Inverse | Self::Determinant | Self::Trace => 1,
            Self::Multiply | Self::Solve | Self::MatVec => 2,
        }
    }

    /// Whether it returns a matrix or vector rather than a single number.
    const fn returns_array(self) -> bool {
        !matches!(self, Self::Determinant | Self::Trace)
    }
}

/// One matrix function, wired to the planner.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct MatrixFunction {
    name: &'static str,
    operation: Operation,
    signature: Signature,
}

impl MatrixFunction {
    fn new(name: &'static str, operation: Operation) -> Self {
        Self {
            name,
            operation,
            // Immutable in the strong sense: the same arguments give the same bits. A
            // product is a grid of order-fixed dot products, and elimination's only freedom
            // — which pivot — is resolved by a tie-break on row index.
            signature: Signature::any(operation.arity(), Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for MatrixFunction {
    fn name(&self) -> &str {
        self.name
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arguments: &[DataType]) -> Result<DataType> {
        // Never used for a matrix-returning function: `return_field_from_args` takes
        // precedence and must, because a shape cannot be expressed as a bare `DataType`.
        Ok(if self.operation.returns_array() {
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), 0)
        } else {
            DataType::Float64
        })
    }

    fn return_field_from_args(
        &self,
        args: datafusion::logical_expr::ReturnFieldArgs,
    ) -> Result<FieldRef> {
        if !self.operation.returns_array() {
            return Ok(Arc::new(Field::new(self.name, DataType::Float64, true)));
        }

        let first = args.arg_fields.first().and_then(shape_of);
        let second = args.arg_fields.get(1).and_then(shape_of);
        let Some(first) = first else {
            return datafusion::common::plan_err!(
                "{}: its argument declares no matrix shape",
                self.name
            );
        };
        let Some((rows, columns)) = self.operation.result_shape(first, second) else {
            return datafusion::common::plan_err!(
                "{}: the shape of the result cannot be determined from its arguments",
                self.name
            );
        };

        let width = i32::try_from(rows.saturating_mul(columns)).unwrap_or(0);
        Ok(Arc::new(
            Field::new(
                self.name,
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float64, true)),
                    width,
                ),
                true,
            )
            // The result carries its own shape, so it can be fed straight into the next
            // matrix function.
            .with_metadata(tensor_metadata(rows, columns)),
        ))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let arity = self.operation.arity();
        if args.args.len() != arity {
            return exec_err!("{} takes {arity} argument(s)", self.name);
        }

        // The shape of the first argument, from its field metadata. Refused rather than
        // guessed: the obvious guess — square — is wrong for every rectangular matrix and
        // produces numbers from values that were never in the same row.
        let Some(first_field) = args.arg_fields.first() else {
            return exec_err!("{}: no field information for its argument", self.name);
        };
        let Some((rows, columns)) = shape_of(first_field) else {
            return exec_err!(
                "{}: the column '{}' declares no matrix shape. A matrix is stored flat, so \
                 the shape must come from field metadata ('{TENSOR_EXTENSION}'). Refusing \
                 rather than assuming it is square: that is wrong for every rectangular \
                 matrix and produces numbers from values that were never in the same row",
                self.name,
                first_field.name()
            );
        };
        let second_shape = args.arg_fields.get(1).and_then(shape_of);

        let arrays: Vec<ArrayRef> = args
            .args
            .iter()
            .map(|arg| arg.clone().into_array(args.number_rows))
            .collect::<Result<_>>()?;

        let count = arrays.iter().map(|a| a.len()).max().unwrap_or(0);
        let mut numbers: Vec<Option<f64>> = Vec::new();
        let mut vectors: Vec<Option<Vec<f64>>> = Vec::new();

        for row in 0..count {
            let mut operands = Vec::with_capacity(arity);
            let mut missing = false;
            for array in &arrays {
                match values_at(array, row)? {
                    // A null matrix yields a null result. A zero matrix is a definite
                    // thing, and a missing one is not it.
                    None => {
                        missing = true;
                        break;
                    }
                    Some(values) => operands.push(values),
                }
            }
            if missing {
                if self.operation.returns_array() {
                    vectors.push(None);
                } else {
                    numbers.push(None);
                }
                continue;
            }

            match self.apply(&operands, rows, columns, second_shape) {
                Err(reason) => return exec_err!("{}: {reason}", self.name),
                Ok(Outcome::Number(value)) => numbers.push(Some(value)),
                Ok(Outcome::Vector(values)) => vectors.push(Some(values)),
            }
        }

        if self.operation.returns_array() {
            let width = vectors
                .iter()
                .flatten()
                .map(Vec::len)
                .next()
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or(0);
            Ok(ColumnarValue::Array(Arc::new(fixed_list_of(
                &vectors, width,
            ))))
        } else {
            Ok(ColumnarValue::Array(Arc::new(Float64Array::from(numbers))))
        }
    }
}

/// What one invocation produced.
enum Outcome {
    Number(f64),
    Vector(Vec<f64>),
}

impl MatrixFunction {
    /// Apply the operation to one row's operands.
    fn apply(
        &self,
        operands: &[Vec<f64>],
        rows: usize,
        columns: usize,
        second: Option<(usize, usize)>,
    ) -> std::result::Result<Outcome, matrix::MatrixError> {
        let a = operands.first().map_or(&[][..], Vec::as_slice);
        let b = operands.get(1).map_or(&[][..], Vec::as_slice);

        Ok(match self.operation {
            Operation::Transpose => Outcome::Vector(matrix::transpose(a, rows, columns)?),
            Operation::Inverse => Outcome::Vector(matrix::inverse(a, rows, columns)?),
            Operation::Determinant => Outcome::Number(matrix::determinant(a, rows, columns)?),
            Operation::Trace => Outcome::Number(matrix::trace(a, rows, columns)?),
            Operation::Solve => Outcome::Vector(matrix::solve(a, rows, b)?),
            Operation::MatVec => Outcome::Vector(
                sankhya_numeric::vector::matvec(a, rows, columns, b)
                    .map_err(matrix::MatrixError::from)?,
            ),
            Operation::Multiply => {
                // The second operand's shape, from its own metadata. Without it there is no
                // way to know whether a flat run of values is 2×3 or 3×2, and the two give
                // different answers that both look ordinary.
                let (right_rows, right_columns) = second.ok_or(matrix::MatrixError::Degenerate)?;
                Outcome::Vector(matrix::multiply(
                    a,
                    rows,
                    columns,
                    b,
                    right_rows,
                    right_columns,
                )?)
            }
        })
    }
}

/// Build a fixed-size list array from per-row vectors.
///
/// Fixed rather than variable, so the result carries the same shape guarantee its input had
/// and can be fed straight into the next matrix function.
fn fixed_list_of(rows: &[Option<Vec<f64>>], width: i32) -> FixedSizeListArray {
    use arrow_array::builder::{FixedSizeListBuilder, Float64Builder};
    let mut builder = FixedSizeListBuilder::new(Float64Builder::new(), width);
    for row in rows {
        match row {
            Some(values) => {
                for value in values {
                    builder.values().append_value(*value);
                }
                builder.append(true);
            }
            None => {
                for _ in 0..width {
                    builder.values().append_null();
                }
                builder.append(false);
            }
        }
    }
    builder.finish()
}

/// The flat values at one row, or `None` for a null.
fn values_at(array: &ArrayRef, row: usize) -> Result<Option<Vec<f64>>> {
    if array.is_null(row) {
        return Ok(None);
    }
    let values: ArrayRef = match array.data_type() {
        DataType::FixedSizeList(_, _) => {
            let Some(list) = array.as_any().downcast_ref::<FixedSizeListArray>() else {
                return exec_err!("expected a fixed-size list");
            };
            list.value(row)
        }
        DataType::List(_) => {
            let Some(list) = array.as_any().downcast_ref::<ListArray>() else {
                return exec_err!("expected a list");
            };
            list.value(row)
        }
        other => {
            return exec_err!(
                "a matrix function needs an array of doubles, and this column is {other}"
            )
        }
    };
    let Some(doubles) = values.as_any().downcast_ref::<Float64Array>() else {
        return exec_err!(
            "a matrix function needs an array of doubles, and this one holds {}",
            values.data_type()
        );
    };
    if doubles.null_count() > 0 {
        return exec_err!(
            "a matrix contains a null element. There is no reading of that: it is not zero, \
             and dropping it changes the shape so every subsequent element moves row"
        );
    }
    Ok(Some(doubles.values().to_vec()))
}
