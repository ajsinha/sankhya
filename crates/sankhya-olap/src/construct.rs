//! Building arrays and matrices from SQL.
//!
//! ```sql
//! SELECT vec_of(1.0, 2.0, 3.0);
//! SELECT mat_of(2, 2, 1.0, 2.0, 3.0, 4.0);
//! SELECT mat_identity(3);
//! ```
//!
//! # Why a matrix constructor is more than a list constructor
//!
//! A matrix is stored flat, and its shape lives in field metadata. So a constructor cannot
//! merely produce values --- it has to produce a *field* carrying the shape, or the result
//! is a run of numbers that the matrix functions will refuse.
//!
//! DataFusion allows exactly that: a function may compute its own return field, metadata
//! and all, from its arguments. That is why `mat_of` takes the shape as its first two
//! arguments and why they must be literals --- the shape is part of the result's *type*,
//! decided at planning time, and a type cannot depend on a value that varies per row.
//!
//! # Why the shape is checked at planning time
//!
//! `mat_of(2, 3, ...)` with five values is refused when the query is planned, not when it
//! runs. The alternative is a query that plans, starts, and fails partway through a scan ---
//! after work has been done and, in a longer pipeline, after rows have been written.

use arrow_array::builder::{FixedSizeListBuilder, Float64Builder};
use arrow_array::{Array, ArrayRef, Float64Array};
use arrow_schema::{DataType, Field, FieldRef};
use datafusion::common::{exec_err, plan_err, Result, ScalarValue};
use datafusion::logical_expr::{
    ColumnarValue, ReturnFieldArgs, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature,
    Volatility,
};
use datafusion::prelude::SessionContext;
use std::sync::Arc;

/// Register the constructors against a session.
pub fn register(context: &SessionContext) {
    for function in functions() {
        context.register_udf(function);
    }
}

/// Every constructor this system offers.
#[must_use]
pub fn functions() -> Vec<ScalarUDF> {
    vec![
        ScalarUDF::from(VectorOf),
        ScalarUDF::from(MatrixOf),
        ScalarUDF::from(MatrixIdentity),
    ]
}

/// `vec_of(a, b, c, …)` — a fixed-length vector from its elements.
///
/// Fixed-length rather than variable, because the width is knowable at planning time: it is
/// the argument count. That makes it a schema-level guarantee rather than a per-row fact,
/// and it is what lets a kernel take a flat slice with a known stride.
#[derive(Debug, PartialEq, Eq, Hash)]
struct VectorOf;

impl ScalarUDFImpl for VectorOf {
    fn name(&self) -> &str {
        "vec_of"
    }

    fn signature(&self) -> &Signature {
        static SIGNATURE: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
        SIGNATURE.get_or_init(|| Signature::variadic_any(Volatility::Immutable))
    }

    fn return_type(&self, arguments: &[DataType]) -> Result<DataType> {
        Ok(DataType::FixedSizeList(
            Arc::new(Field::new("item", DataType::Float64, true)),
            i32::try_from(arguments.len()).unwrap_or(0),
        ))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        if args.args.is_empty() {
            return exec_err!("vec_of needs at least one element");
        }
        let width = i32::try_from(args.args.len()).unwrap_or(0);
        let columns: Vec<ArrayRef> = args
            .args
            .iter()
            .map(|arg| arg.clone().into_array(args.number_rows))
            .collect::<Result<_>>()?;

        let mut builder = FixedSizeListBuilder::new(Float64Builder::new(), width);
        for row in 0..args.number_rows {
            // The row is gathered before anything is appended, so a null found halfway
            // through does not leave a half-written vector in the builder.
            let mut cells = Vec::with_capacity(columns.len());
            let mut missing = false;
            for column in &columns {
                match as_double(column, row)? {
                    // A null element makes the whole vector null. There is no reading of a
                    // vector with a hole in it: the element is not zero, and shortening it
                    // moves every element after it to the wrong position.
                    None => {
                        missing = true;
                        break;
                    }
                    Some(value) => cells.push(value),
                }
            }
            if missing {
                for _ in 0..width {
                    builder.values().append_null();
                }
                builder.append(false);
                continue;
            }
            for cell in cells {
                builder.values().append_value(cell);
            }
            builder.append(true);
        }
        Ok(ColumnarValue::Array(Arc::new(builder.finish())))
    }
}

/// `mat_of(rows, columns, a, b, c, …)` — a matrix carrying its own shape.
///
/// The shape arguments must be literals, because the shape is part of the result's *type*
/// and a type is decided when the query is planned. A shape that varied per row would mean
/// a column whose type changed row to row, which is not a thing a column can be.
#[derive(Debug, PartialEq, Eq, Hash)]
struct MatrixOf;

/// Read a positive dimension from a literal argument.
fn dimension(value: Option<&ScalarValue>, which: &str) -> Result<usize> {
    let Some(scalar) = value else {
        return plan_err!(
            "mat_of's {which} must be a literal: the shape is part of the result's type, \
             and a type cannot depend on a value that varies row to row"
        );
    };
    let count = match scalar {
        ScalarValue::Int64(Some(n)) => *n,
        ScalarValue::Int32(Some(n)) => i64::from(*n),
        ScalarValue::UInt64(Some(n)) => i64::try_from(*n).unwrap_or(-1),
        other => return plan_err!("mat_of's {which} must be a whole number, not {other}"),
    };
    if count <= 0 {
        return plan_err!("mat_of's {which} must be above zero, and is {count}");
    }
    usize::try_from(count).map_err(|_| {
        datafusion::common::DataFusionError::Plan(format!("mat_of's {which} is too large"))
    })
}

impl ScalarUDFImpl for MatrixOf {
    fn name(&self) -> &str {
        "mat_of"
    }

    fn signature(&self) -> &Signature {
        static SIGNATURE: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
        SIGNATURE.get_or_init(|| Signature::variadic_any(Volatility::Immutable))
    }

    fn return_type(&self, _arguments: &[DataType]) -> Result<DataType> {
        // Never called: `return_field_from_args` takes precedence, and it must, because the
        // shape metadata cannot be expressed as a bare `DataType`.
        Ok(DataType::FixedSizeList(
            Arc::new(Field::new("item", DataType::Float64, true)),
            0,
        ))
    }

    fn return_field_from_args(&self, args: ReturnFieldArgs) -> Result<FieldRef> {
        let rows = dimension(
            args.scalar_arguments.first().copied().flatten(),
            "row count",
        )?;
        let columns = dimension(
            args.scalar_arguments.get(1).copied().flatten(),
            "column count",
        )?;

        let supplied = args.arg_fields.len().saturating_sub(2);
        let expected = rows.saturating_mul(columns);
        // Checked here rather than at execution: the alternative is a query that plans,
        // starts, and fails partway through a scan after work has been done.
        if supplied != expected {
            return plan_err!(
                "mat_of({rows}, {columns}, …) needs {expected} values and was given \
                 {supplied}. Refusing at planning time rather than partway through the scan"
            );
        }

        let width = i32::try_from(expected).unwrap_or(0);
        Ok(Arc::new(
            Field::new(
                "matrix",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float64, true)),
                    width,
                ),
                true,
            )
            // The shape, in Arrow's canonical tensor metadata, so the matrix functions can
            // read it and an external engine that understands tensors can too.
            .with_metadata(crate::matrices::tensor_metadata(rows, columns)),
        ))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let values = args.args.get(2..).unwrap_or(&[]);
        let width = i32::try_from(values.len()).unwrap_or(0);
        let columns: Vec<ArrayRef> = values
            .iter()
            .map(|arg| arg.clone().into_array(args.number_rows))
            .collect::<Result<_>>()?;

        let mut builder = FixedSizeListBuilder::new(Float64Builder::new(), width);
        for row in 0..args.number_rows {
            let mut cells = Vec::with_capacity(columns.len());
            let mut missing = false;
            for column in &columns {
                match as_double(column, row)? {
                    None => {
                        missing = true;
                        break;
                    }
                    Some(value) => cells.push(value),
                }
            }
            if missing {
                for _ in 0..width {
                    builder.values().append_null();
                }
                builder.append(false);
                continue;
            }
            for cell in cells {
                builder.values().append_value(cell);
            }
            builder.append(true);
        }
        Ok(ColumnarValue::Array(Arc::new(builder.finish())))
    }
}

/// `mat_identity(n)` — the identity matrix, carrying its shape.
#[derive(Debug, PartialEq, Eq, Hash)]
struct MatrixIdentity;

impl ScalarUDFImpl for MatrixIdentity {
    fn name(&self) -> &str {
        "mat_identity"
    }

    fn signature(&self) -> &Signature {
        static SIGNATURE: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
        SIGNATURE.get_or_init(|| Signature::any(1, Volatility::Immutable))
    }

    fn return_type(&self, _arguments: &[DataType]) -> Result<DataType> {
        Ok(DataType::FixedSizeList(
            Arc::new(Field::new("item", DataType::Float64, true)),
            0,
        ))
    }

    fn return_field_from_args(&self, args: ReturnFieldArgs) -> Result<FieldRef> {
        let size = dimension(args.scalar_arguments.first().copied().flatten(), "size")?;
        let width = i32::try_from(size.saturating_mul(size)).unwrap_or(0);
        Ok(Arc::new(
            Field::new(
                "matrix",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float64, true)),
                    width,
                ),
                false,
            )
            .with_metadata(crate::matrices::tensor_metadata(size, size)),
        ))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let Some(ColumnarValue::Scalar(scalar)) = args.args.first() else {
            return exec_err!("mat_identity's size must be a literal");
        };
        let size = dimension(Some(scalar), "size")?;
        let identity = sankhya_math::matrix::identity(size);
        let width = i32::try_from(identity.len()).unwrap_or(0);

        let mut builder = FixedSizeListBuilder::new(Float64Builder::new(), width);
        for _ in 0..args.number_rows.max(1) {
            for value in &identity {
                builder.values().append_value(*value);
            }
            builder.append(true);
        }
        Ok(ColumnarValue::Array(Arc::new(builder.finish())))
    }
}

/// One element as a double, or `None` for a null.
///
/// Integers widen; anything else is refused rather than coerced. A text column silently
/// parsed into numbers would produce a vector from values nobody meant as numbers.
fn as_double(array: &ArrayRef, row: usize) -> Result<Option<f64>> {
    use arrow_array::cast::AsArray;
    use arrow_array::types;

    if array.is_null(row) {
        return Ok(None);
    }
    Ok(Some(match array.data_type() {
        DataType::Float64 => array.as_primitive::<types::Float64Type>().value(row),
        DataType::Float32 => f64::from(array.as_primitive::<types::Float32Type>().value(row)),
        #[allow(clippy::cast_precision_loss)]
        DataType::Int64 => array.as_primitive::<types::Int64Type>().value(row) as f64,
        DataType::Int32 => f64::from(array.as_primitive::<types::Int32Type>().value(row)),
        other => {
            return exec_err!(
                "a vector element must be a number, and this one is {other}. Refusing \
                 rather than coercing: a text column parsed into numbers produces a vector \
                 from values nobody meant as numbers"
            )
        }
    }))
}

/// Kept so the unused-import warning does not hide a real one.
#[allow(dead_code)]
fn _uses_float_array(_: &Float64Array) {}
