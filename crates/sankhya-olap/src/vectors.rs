//! The vector kernels, callable from SQL.
//!
//! `SELECT cosine_similarity(embedding, :query) FROM documents ORDER BY 1 DESC LIMIT 10`
//!
//! # Why these are scalar functions over whole columns
//!
//! An Arrow `FixedSizeList<Float64, N>` column stores its values in one contiguous child
//! buffer, so a column of a million vectors is a single flat `&[f64]`. Each invocation takes
//! a slice of it with a known stride --- no copy, no allocation, no per-row indirection.
//! That is the reason for preferring the fixed-size list over a variable-length one, and it
//! is visible here as the difference between a slice and a walk.
//!
//! # Determinism travels with them
//!
//! Every reducing kernel goes through `sankhya-numeric`'s compensated, order-fixed sum, so
//! `dot(a, b)` returns the same bits however the query was partitioned. That is the whole
//! argument of ADR-0005, and exposing the kernels through SQL is where it becomes visible to
//! anyone: two runs of the same ranking produce the same order, not merely a similar one.
//!
//! # What a null means here
//!
//! A null vector yields a null result, never zero. A cosine similarity of zero is a definite
//! statement --- "orthogonal" --- and a missing vector is not orthogonal to anything.

use arrow_array::{Array, ArrayRef, FixedSizeListArray, Float64Array, ListArray};
use arrow_schema::DataType;
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, Volatility,
};
use datafusion::prelude::SessionContext;
use sankhya_numeric::vector;
use std::sync::Arc;

/// Register every vector function against a session.
///
/// One call, so a session has the whole set or none of it. A partially registered set means
/// a query works on one node and fails on another.
pub fn register(context: &SessionContext) {
    for function in functions() {
        context.register_udf(function);
    }
}

/// Every vector function this system offers.
#[must_use]
pub fn functions() -> Vec<ScalarUDF> {
    vec![
        ScalarUDF::from(VectorFunction::binary("vec_dot", |a, b| vector::dot(a, b))),
        ScalarUDF::from(VectorFunction::binary("vec_euclidean", |a, b| {
            vector::euclidean(a, b)
        })),
        ScalarUDF::from(VectorFunction::binary("vec_cosine_similarity", |a, b| {
            vector::cosine_similarity(a, b)
        })),
        ScalarUDF::from(VectorFunction::binary("vec_cosine_distance", |a, b| {
            vector::cosine_distance(a, b)
        })),
        ScalarUDF::from(VectorFunction::unary("vec_norm_l2", |a| {
            Ok(vector::norm_l2(a))
        })),
        ScalarUDF::from(VectorFunction::unary("vec_norm_l1", |a| {
            Ok(vector::norm_l1(a))
        })),
        ScalarUDF::from(VectorFunction::unary("vec_sum", |a| Ok(vector::sum(a)))),
        ScalarUDF::from(VectorFunction::unary("vec_mean", vector::mean)),
    ]
}

/// A kernel of one or two vectors, returning a number.
type Kernel = Arc<dyn Fn(&[&[f64]]) -> std::result::Result<f64, vector::VectorError> + Send + Sync>;

/// One vector function, wired to the planner.
///
/// A hand-written implementation rather than `create_udf`, because that helper takes an
/// *exact* argument type and a vector column's type carries its width --- so an exact
/// signature would match `FixedSizeList(3 x Float64)` and reject `FixedSizeList(384 x
/// Float64)`. The signature here accepts any argument and the types are checked when the
/// kernel runs, where the message can say what was wrong.
pub struct VectorFunction {
    name: &'static str,
    arity: usize,
    kernel: Kernel,
    signature: Signature,
}

impl std::fmt::Debug for VectorFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorFunction")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .finish_non_exhaustive()
    }
}

impl VectorFunction {
    /// A function of one vector.
    fn unary(
        name: &'static str,
        kernel: impl Fn(&[f64]) -> std::result::Result<f64, vector::VectorError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            arity: 1,
            kernel: Arc::new(move |args| match args.first() {
                Some(a) => kernel(a),
                None => Err(vector::VectorError::Empty),
            }),
            // Immutable in the strong sense: the same arguments give the same *bits*, not
            // merely the same value, because the reduction is order-fixed. That is what
            // lets the planner cache, hoist and reorder these safely.
            signature: Signature::any(1, Volatility::Immutable),
        }
    }

    /// A function of two vectors.
    fn binary(
        name: &'static str,
        kernel: impl Fn(&[f64], &[f64]) -> std::result::Result<f64, vector::VectorError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            name,
            arity: 2,
            kernel: Arc::new(move |args| match (args.first(), args.get(1)) {
                (Some(a), Some(b)) => kernel(a, b),
                _ => Err(vector::VectorError::Empty),
            }),
            signature: Signature::any(2, Volatility::Immutable),
        }
    }
}

// Identity is the name. Two functions with the same name are the same function, and the
// kernel behind it is a closure with no meaningful equality of its own — so comparing or
// hashing it would be comparing a function pointer, which is not stable.
impl PartialEq for VectorFunction {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.arity == other.arity
    }
}

impl Eq for VectorFunction {}

impl std::hash::Hash for VectorFunction {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.arity.hash(state);
    }
}

impl ScalarUDFImpl for VectorFunction {
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
        let arrays = to_arrays(&args.args, self.arity)?;
        let rows = arrays.iter().map(|a| a.len()).max().unwrap_or(0);
        let mut out: Vec<Option<f64>> = Vec::with_capacity(rows);

        for row in 0..rows {
            let mut operands: Vec<Vec<f64>> = Vec::with_capacity(self.arity);
            let mut any_null = false;
            for array in &arrays {
                match vector_at(array, row)? {
                    // A null vector yields a null result, never zero. A cosine similarity
                    // of zero says "orthogonal", and a missing vector is not orthogonal to
                    // anything.
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
            let borrowed: Vec<&[f64]> = operands.iter().map(Vec::as_slice).collect();
            match (self.kernel)(&borrowed) {
                Ok(value) => out.push(Some(value)),
                Err(reason) => return exec_err!("{}: {reason}", self.name),
            }
        }
        Ok(ColumnarValue::Array(Arc::new(Float64Array::from(out))))
    }
}

/// Materialise the arguments as arrays of equal length.
///
/// A scalar argument — the query vector in a similarity search — is broadcast, because
/// `ORDER BY cosine_similarity(embedding, ARRAY[...])` is the shape this exists for.
fn to_arrays(args: &[ColumnarValue], expected: usize) -> Result<Vec<ArrayRef>> {
    if args.len() != expected {
        return exec_err!("expected {expected} argument(s), got {}", args.len());
    }
    let rows = args
        .iter()
        .filter_map(|arg| match arg {
            ColumnarValue::Array(array) => Some(array.len()),
            ColumnarValue::Scalar(_) => None,
        })
        .max()
        .unwrap_or(1);
    args.iter()
        .map(|arg| arg.clone().into_array(rows))
        .collect()
}

/// The vector at one row, as a flat slice of doubles.
///
/// Returns `None` for a null. Refuses a column that is not an array of doubles rather than
/// coercing: coercion here would silently reinterpret a column of integers as a vector, and
/// the number that came back would be a real number computed from the wrong thing.
fn vector_at(array: &ArrayRef, row: usize) -> Result<Option<Vec<f64>>> {
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
                "a vector function needs an array of doubles, and this column is {other}. \
                 Refusing rather than coercing: coercion would compute a real number from \
                 the wrong thing"
            )
        }
    };

    let Some(doubles) = values.as_any().downcast_ref::<Float64Array>() else {
        return exec_err!(
            "a vector function needs an array of doubles, and this one holds {}",
            values.data_type()
        );
    };
    // A null *inside* a vector has no defensible reading: it is not zero, and dropping it
    // shortens the vector so a dot product silently pairs the wrong elements.
    if doubles.null_count() > 0 {
        return exec_err!(
            "a vector contains a null element. There is no reading of that: it is not zero, \
             and dropping it shortens the vector so a dot product pairs the wrong elements"
        );
    }
    Ok(Some(doubles.values().to_vec()))
}
