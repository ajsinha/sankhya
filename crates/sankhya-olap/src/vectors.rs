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

use arrow_array::builder::{Float64Builder, ListBuilder};
use arrow_array::{Array, ArrayRef, FixedSizeListArray, Float64Array, ListArray};
use arrow_schema::{DataType, Field};
use datafusion::common::{exec_err, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, Volatility,
};
use datafusion::prelude::SessionContext;
use sankhya_math::{calculus, stats, vector};
use std::sync::Arc;

/// Register every vector function against a session.
///
/// One call, so a session has the whole set or none of it. A partially registered set means
/// a query works on one node and fails on another.
pub fn register(context: &SessionContext) {
    for function in functions() {
        context.register_udf(function);
    }
    for function in series_functions() {
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
        // Statistics *within* one vector — a per-row series, such as a window of readings
        // or a term structure. Distinct from an aggregate across rows, which SQL already
        // has: `stddev(x)` describes a column, `vec_stddev(v)` describes one row's series.
        ScalarUDF::from(VectorFunction::unary("vec_variance", |a| {
            stats::variance(a, stats::Population::Sample)
        })),
        ScalarUDF::from(VectorFunction::unary("vec_stddev", |a| {
            stats::standard_deviation(a, stats::Population::Sample)
        })),
        ScalarUDF::from(VectorFunction::unary("vec_median", stats::median)),
        ScalarUDF::from(VectorFunction::unary("vec_skewness", stats::skewness)),
        ScalarUDF::from(VectorFunction::unary(
            "vec_kurtosis",
            stats::excess_kurtosis,
        )),
        ScalarUDF::from(VectorFunction::binary("vec_covariance", |a, b| {
            stats::covariance(a, b, stats::Population::Sample)
        })),
        ScalarUDF::from(VectorFunction::binary(
            "vec_correlation",
            stats::correlation,
        )),
        // Calculus over a sampled series, at unit spacing. A caller wanting another
        // spacing scales the result, which is exact — rather than this taking a spacing
        // argument that would have to be a literal for no benefit.
        ScalarUDF::from(VectorFunction::unary("vec_integral", |a| {
            calculus::integrate_trapezoid(a, 1.0)
        })),
        // Simpson's rule, which is exact for a cubic where the trapezoid is exact only for a
        // line. Offered beside the trapezoid rather than replacing it: Simpson needs an even
        // number of intervals and refuses otherwise, and a function that silently changed
        // rule to accommodate its input would return two different approximations under one
        // name.
        ScalarUDF::from(VectorFunction::unary("vec_integral_simpson", |a| {
            calculus::integrate_simpson(a, 1.0)
        })),
        // The spread of a row's series. Reported as one number --- `max - min` --- because
        // the two ends are separately available as `vec_min` and `vec_max`, and a caller who
        // wants them has them.
        ScalarUDF::from(VectorFunction::unary("vec_range", |a| {
            stats::range(a)
                .map(|(low, high)| high - low)
                .ok_or(vector::VectorError::Empty)
        })),
        ScalarUDF::from(VectorFunction::unary("vec_min", |a| {
            stats::range(a).map(|(low, _)| low).ok_or(vector::VectorError::Empty)
        })),
        ScalarUDF::from(VectorFunction::unary("vec_max", |a| {
            stats::range(a).map(|(_, high)| high).ok_or(vector::VectorError::Empty)
        })),
        // Least-squares regression, as three functions rather than one returning a struct.
        //
        // A struct return would make the common case --- wanting the slope --- into a field
        // access on a composite type, which the wire protocol renders as a string a client
        // then has to parse. Three named scalars compose in a `SELECT` list, and a caller
        // wanting all three writes all three.
        ScalarUDF::from(VectorFunction::binary("vec_regression_slope", |a, b| {
            stats::linear_fit(a, b).map(|fit| fit.slope)
        })),
        ScalarUDF::from(VectorFunction::binary("vec_regression_intercept", |a, b| {
            stats::linear_fit(a, b).map(|fit| fit.intercept)
        })),
        // NULL, not a number, when the response does not vary --- see `LinearFit::r_squared`.
        ScalarUDF::from(VectorFunction::binary_defined_sometimes("vec_regression_r2", |a, b| {
            stats::linear_fit(a, b).map(|fit| fit.r_squared)
        })),
        // The population forms, beside the sample ones above. Which divisor a variance uses
        // is a statement about what the data *is*, not a preference --- a sample variance of
        // a complete population overstates the spread, and the difference is invisible in
        // the number. Naming both is what lets somebody choose the one they mean.
        ScalarUDF::from(VectorFunction::unary("vec_variance_pop", |a| {
            stats::variance(a, stats::Population::Whole)
        })),
        ScalarUDF::from(VectorFunction::unary("vec_stddev_pop", |a| {
            stats::standard_deviation(a, stats::Population::Whole)
        })),
        ScalarUDF::from(VectorFunction::binary("vec_covariance_pop", |a, b| {
            stats::covariance(a, b, stats::Population::Whole)
        })),
        // A quantile of one row's series, by linear interpolation --- the convention most
        // libraries and spreadsheets use, and so the one somebody means when they have not
        // said. `vec_median` is the same kernel at `q = 0.5` and keeps its own name because
        // that is what people write.
        ScalarUDF::from(VectorFunction::binary("vec_quantile", |a, q| {
            let Some(&probability) = q.first() else {
                return Err(vector::VectorError::Empty);
            };
            let mut values = a.to_vec();
            // The quantile's own error, not `Empty`. Mapping every failure to "empty vector"
            // reported a probability outside `[0, 1]` as an empty input --- which sent
            // somebody to look at their data instead of their argument. Found by the parity
            // soak, where the message was the only thing that said what had gone wrong.
            sankhya_math::quantile(
                &mut values,
                probability,
                sankhya_math::Convention::LinearInterpolation,
            )
            .map_err(|reason| vector::VectorError::Refused(reason.to_string()))
        })),
    ]
}

/// Every function that takes a series and returns a series.
///
/// Separate from [`functions`] only because the return type differs; they register together
/// and a session has both or neither.
#[must_use]
pub fn series_functions() -> Vec<ScalarUDF> {
    vec![
        // Element-wise arithmetic between two rows' series --- adding two yield curves,
        // netting two exposures, differencing two readings.
        ScalarUDF::from(SeriesFunction::binary("vec_add", vector::add)),
        ScalarUDF::from(SeriesFunction::binary("vec_subtract", vector::subtract)),
        ScalarUDF::from(SeriesFunction::binary("vec_multiply", vector::multiply)),
        ScalarUDF::from(SeriesFunction::binary("vec_divide", vector::divide)),
        ScalarUDF::from(SeriesFunction::scaled("vec_scale", |a, by| {
            Ok(vector::scale(a, by))
        })),
        // Calculus over a sampled series. Unit spacing, as `vec_integral` uses: a caller
        // wanting another spacing scales the result, which is exact --- rather than this
        // taking a spacing argument that would have to be a literal for no benefit.
        ScalarUDF::from(SeriesFunction::unary("vec_differences", calculus::differences)),
        ScalarUDF::from(SeriesFunction::unary("vec_derivative", |a| {
            calculus::derivative(a, 1.0)
        })),
        ScalarUDF::from(SeriesFunction::unary("vec_second_derivative", |a| {
            calculus::second_derivative(a, 1.0)
        })),
        ScalarUDF::from(SeriesFunction::unary("vec_cumulative_sum", |a| {
            Ok(calculus::cumulative_sum(a))
        })),
        ScalarUDF::from(SeriesFunction::unary("vec_cumulative_integral", |a| {
            calculus::cumulative_integral(a, 1.0)
        })),
        // Centre and scale to unit variance, so two series measured in different units can
        // be compared. The sample form, matching `vec_stddev`.
        ScalarUDF::from(SeriesFunction::unary("vec_standardise", |a| {
            stats::standardise(a, stats::Population::Sample)
        })),
    ]
}

/// A kernel of one or two vectors, returning a number.
type Kernel =
    Arc<dyn Fn(&[&[f64]]) -> std::result::Result<Option<f64>, vector::VectorError> + Send + Sync>;

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
                Some(a) => kernel(a).map(Some),
                None => Err(vector::VectorError::Empty),
            }),
            // Immutable in the strong sense: the same arguments give the same *bits*, not
            // merely the same value, because the reduction is order-fixed. That is what
            // lets the planner cache, hoist and reorder these safely.
            signature: Signature::any(1, Volatility::Immutable),
        }
    }

    /// A function of two vectors.
    /// A function of two vectors whose answer may be **undefined** for some input.
    ///
    /// Distinct from an error, and the distinction is the point. An error is a statement about
    /// the call --- mismatched lengths, an empty vector --- and it fails the statement, which
    /// for a per-row function means one bad row takes ten million good ones with it. Undefined
    /// is a statement about the answer, and SQL already has a word for that.
    fn binary_defined_sometimes(
        name: &'static str,
        kernel: impl Fn(&[f64], &[f64]) -> std::result::Result<Option<f64>, vector::VectorError>
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
                (Some(a), Some(b)) => kernel(a, b).map(Some),
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
                // `None` is the kernel saying the answer is undefined, not that it failed.
                // It becomes a NULL in this row and the rest of the column is unaffected.
                Ok(value) => out.push(value),
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

// ---------------------------------------------------------------------------

/// A kernel that turns one row's series into another series.
///
/// The shape [`VectorFunction`] cannot express. A derivative, a running total, a
/// standardisation and an element-wise sum all take vectors and **return a vector**, and the
/// scalar wrapper returns one number --- which is why twelve tested kernels sat in
/// `sankhya-math` with no name on any surface until `check-kernels` began failing the build
/// for it.
type SeriesKernel =
    Arc<dyn Fn(&[&[f64]], f64) -> std::result::Result<Vec<f64>, vector::VectorError> + Send + Sync>;

/// One series function, wired to the planner.
///
/// # Why the result is a `List` and not a `FixedSizeList`
///
/// A `FixedSizeList` carries its width in its type, and these kernels change it: a first
/// difference of `n` values has `n - 1`. Declaring a fixed width would make the return type a
/// function of the argument's width, and a `differences` of a 384-dimensional embedding would
/// have to be a different function from a `differences` of a 3-dimensional one.
///
/// A `List` costs an offsets buffer and accepts every width, and every vector function here
/// reads both --- so a series result composes with the rest of the catalogue.
pub struct SeriesFunction {
    name: &'static str,
    arity: usize,
    /// A trailing scalar argument, such as the factor `vec_scale` multiplies by.
    scalar: bool,
    kernel: SeriesKernel,
    signature: Signature,
}

impl std::fmt::Debug for SeriesFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeriesFunction")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .finish_non_exhaustive()
    }
}

impl SeriesFunction {
    /// One vector in, one vector out.
    fn unary(
        name: &'static str,
        kernel: impl Fn(&[f64]) -> std::result::Result<Vec<f64>, vector::VectorError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            name,
            arity: 1,
            scalar: false,
            kernel: Arc::new(move |operands, _| {
                let a = operands.first().copied().unwrap_or(&[]);
                kernel(a)
            }),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }

    /// Two vectors in, one vector out.
    fn binary(
        name: &'static str,
        kernel: impl Fn(&[f64], &[f64]) -> std::result::Result<Vec<f64>, vector::VectorError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            name,
            arity: 2,
            scalar: false,
            kernel: Arc::new(move |operands, _| {
                let a = operands.first().copied().unwrap_or(&[]);
                let b = operands.get(1).copied().unwrap_or(&[]);
                kernel(a, b)
            }),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }

    /// One vector and one number in, one vector out.
    fn scaled(
        name: &'static str,
        kernel: impl Fn(&[f64], f64) -> std::result::Result<Vec<f64>, vector::VectorError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            name,
            arity: 1,
            scalar: true,
            kernel: Arc::new(move |operands, by| {
                let a = operands.first().copied().unwrap_or(&[]);
                kernel(a, by)
            }),
            signature: Signature::variadic_any(Volatility::Immutable),
        }
    }
}

impl PartialEq for SeriesFunction {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.arity == other.arity && self.scalar == other.scalar
    }
}

impl Eq for SeriesFunction {}

impl std::hash::Hash for SeriesFunction {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.arity.hash(state);
        self.scalar.hash(state);
    }
}

impl ScalarUDFImpl for SeriesFunction {
    fn name(&self) -> &str {
        self.name
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arguments: &[DataType]) -> Result<DataType> {
        Ok(DataType::List(Arc::new(Field::new(
            "item",
            DataType::Float64,
            true,
        ))))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let expected = self.arity + usize::from(self.scalar);
        let arrays = to_arrays(&args.args, expected)?;
        let rows = arrays.iter().map(|a| a.len()).max().unwrap_or(0);

        let mut builder = ListBuilder::new(Float64Builder::new());
        for row in 0..rows {
            let mut operands: Vec<Vec<f64>> = Vec::with_capacity(self.arity);
            let mut any_null = false;
            for array in arrays.iter().take(self.arity) {
                match vector_at(array, row)? {
                    // A null vector gives a null series, never an empty one. An empty series
                    // is a definite statement --- "nothing was measured" --- and a missing
                    // vector is not that.
                    None => {
                        any_null = true;
                        break;
                    }
                    Some(values) => operands.push(values),
                }
            }
            let by = if self.scalar {
                match arrays.get(self.arity).map(|a| number_at(a, row)) {
                    Some(Ok(Some(value))) => value,
                    Some(Ok(None)) => {
                        any_null = true;
                        0.0
                    }
                    Some(Err(error)) => return Err(error),
                    None => 0.0,
                }
            } else {
                0.0
            };
            if any_null {
                builder.append_null();
                continue;
            }
            let borrowed: Vec<&[f64]> = operands.iter().map(Vec::as_slice).collect();
            match (self.kernel)(&borrowed, by) {
                Ok(values) => {
                    builder.values().append_slice(&values);
                    builder.append(true);
                }
                Err(reason) => return exec_err!("{}: {reason}", self.name),
            }
        }
        Ok(ColumnarValue::Array(Arc::new(builder.finish())))
    }
}

/// A plain number at one row, for a scalar argument that is broadcast across a column.
fn number_at(array: &ArrayRef, row: usize) -> Result<Option<f64>> {
    if array.is_null(row) {
        return Ok(None);
    }
    if let Some(doubles) = array.as_any().downcast_ref::<Float64Array>() {
        return Ok(Some(doubles.value(row)));
    }
    if let Some(ints) = array.as_any().downcast_ref::<arrow_array::Int64Array>() {
        #[allow(clippy::cast_precision_loss)]
        return Ok(Some(ints.value(row) as f64));
    }
    exec_err!(
        "this argument must be a number, and it is {}. Refused rather than coerced: a \
         coercion here computes a real number from the wrong thing",
        array.data_type()
    )
}
