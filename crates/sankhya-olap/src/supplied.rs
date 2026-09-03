//! A user's own aggregation, as an aggregate a query can call.
//!
//! # Why this shape and not a special path through the cube
//!
//! DataFusion's [`Accumulator`] is `ADR-0010`'s contract, method for method:
//!
//! | `ADR-0010` | `Accumulator` |
//! |---|---|
//! | `accumulate(state, batch)` | `update_batch` |
//! | `merge(a, b)` | `merge_batch` |
//! | `finish(state)` | `evaluate` |
//! | `state()` | `state` |
//!
//! That is not a coincidence --- both are the shape every distributed aggregation has --- and
//! it decides the design: a supplied aggregation becomes an ordinary `AggregateUDF`, so it is
//! callable in any `GROUP BY` the moment it is declared, it partitions and combines exactly as
//! a built-in does, and a cube measure that names one is computed by the same query machinery
//! as every other measure rather than by a second path.
//!
//! # How the arguments arrive
//!
//! **Interleaved, one tuple per row.** `weighted_mean(amount, weight)` over three rows sends
//! six doubles: amount, weight, amount, weight, amount, weight. A row where any argument is
//! null is skipped whole, which is what SQL aggregates do with a null and what an author would
//! otherwise have to remember to do.
//!
//! # What this costs, said rather than hidden
//!
//! A process boundary and a serialised state per batch. `ADR-0022` Decision 5 puts it at two
//! to three orders of magnitude against a compiled built-in, and that is the reason the built-in
//! catalogue is worth its size. It is also enormously faster than fetching the rows to a client
//! to do the same arithmetic, which is the comparison that decides whether the feature earns
//! its place.

use arrow_array::{Array, ArrayRef, Float64Array, StringArray};
use arrow_schema::{DataType, Field, FieldRef};
use datafusion::common::{exec_err, Result, ScalarValue};
use datafusion::logical_expr::function::{AccumulatorArgs, StateFieldsArgs};
use datafusion::logical_expr::{
    Accumulator, AggregateUDF, AggregateUDFImpl, Signature, Volatility,
};
use sankhya_udf::{Aggregation, Worker};
use std::sync::Arc;

/// One declared aggregation, registered as an aggregate function.
#[derive(Debug)]
pub struct Supplied {
    aggregation: Arc<Aggregation>,
    worker: Arc<Worker>,
    signature: Signature,
}

impl Supplied {
    /// Register a declared aggregation under its own name.
    #[must_use]
    pub fn new(aggregation: Arc<Aggregation>, worker: Arc<Worker>) -> AggregateUDF {
        AggregateUDF::from(Self {
            aggregation,
            worker,
            // Any number of arguments, coerced to numbers by `coerce_types` below. The arity
            // is the author's --- declared by what their `accumulate` reads out of the batch ---
            // and a signature that fixed it here would be a second definition of a signature
            // this server does not own.
            signature: Signature::user_defined(Volatility::Immutable),
        })
    }
}

/// Two registrations are the same function when they are the same *declaration*.
///
/// By name and source, not by worker: the worker is a process pool, and a plan that compared
/// two calls to the same aggregation and found them different would refuse to reuse a partial
/// aggregate that is exactly what the other computed.
impl PartialEq for Supplied {
    fn eq(&self, other: &Self) -> bool {
        self.aggregation == other.aggregation
    }
}

impl Eq for Supplied {}

impl std::hash::Hash for Supplied {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.aggregation.name.hash(state);
        self.aggregation.source.hash(state);
    }
}

impl AggregateUDFImpl for Supplied {
    fn name(&self) -> &str {
        &self.aggregation.name
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arguments: &[DataType]) -> Result<DataType> {
        Ok(DataType::Float64)
    }

    /// Every argument becomes a `Float64`.
    ///
    /// Written here rather than left to a fixed signature because the arity belongs to the
    /// author. An integer column is widened, which is exactly what the built-in aggregates do
    /// and what somebody writing `weighted_mean(quantity, weight)` over an `INT` column
    /// expects; anything with no numeric reading is refused by name rather than sent to a
    /// worker that would fail on it row by row.
    fn coerce_types(&self, arguments: &[DataType]) -> Result<Vec<DataType>> {
        let mut out = Vec::with_capacity(arguments.len());
        for argument in arguments {
            if !argument.is_numeric() && !matches!(argument, DataType::Null) {
                return exec_err!(
                    "{}: a user-supplied aggregation takes numbers, and it was given a {argument}",
                    self.aggregation.name
                );
            }
            out.push(DataType::Float64);
        }
        Ok(out)
    }

    fn accumulator(&self, _arguments: AccumulatorArgs) -> Result<Box<dyn Accumulator>> {
        Ok(Box::new(Partial {
            aggregation: Arc::clone(&self.aggregation),
            worker: Arc::clone(&self.worker),
            state: Vec::new(),
        }))
    }

    /// The partial state, as text.
    ///
    /// `ADR-0010`: *an external measure materialises its **state**, exactly as Snowflake's
    /// `aggregate_state` does, and `finish` runs once when the answer is read.* A partial that
    /// crossed as a finished number could not be rolled up further without the rounding that
    /// the cube's exit criteria forbid.
    fn state_fields(&self, _arguments: StateFieldsArgs) -> Result<Vec<FieldRef>> {
        Ok(vec![Arc::new(Field::new("state", DataType::Utf8, true))])
    }
}

/// One partition's running state.
#[derive(Debug)]
struct Partial {
    aggregation: Arc<Aggregation>,
    worker: Arc<Worker>,
    state: Vec<u8>,
}

impl Accumulator for Partial {
    fn update_batch(&mut self, values: &[ArrayRef]) -> Result<()> {
        let Some(first) = values.first() else {
            return Ok(());
        };
        let rows = first.len();
        let columns: Vec<&Float64Array> = values
            .iter()
            .filter_map(|column| column.as_any().downcast_ref::<Float64Array>())
            .collect();
        if columns.len() != values.len() {
            return exec_err!(
                "{}: a user-supplied aggregation takes numbers, and one of its arguments is \
                 not one",
                self.aggregation.name
            );
        }

        let mut flat: Vec<f64> = Vec::with_capacity(rows * columns.len());
        'row: for row in 0..rows {
            for column in &columns {
                if column.is_null(row) {
                    // The whole row, not the one argument. A weighted mean handed a null
                    // weight and a present value would otherwise be told the weight was zero,
                    // which is a number and is not what a missing weight means.
                    continue 'row;
                }
            }
            for column in &columns {
                flat.push(column.value(row));
            }
        }
        if flat.is_empty() {
            return Ok(());
        }

        self.state = self
            .worker
            .accumulate(&self.aggregation, &self.state, &flat)
            .map_err(|refused| refused_as(&self.aggregation.name, &refused))?;
        Ok(())
    }

    fn merge_batch(&mut self, states: &[ArrayRef]) -> Result<()> {
        if !self.aggregation.composes {
            return exec_err!(
                "{}: this aggregation declares no `merge`, so its partial results cannot be \
                 combined. It is computed from base data rather than from partials, and a \
                 plan that split it across partitions asked for something it cannot give",
                self.aggregation.name
            );
        }
        let Some(column) = states.first().and_then(|c| c.as_any().downcast_ref::<StringArray>())
        else {
            return exec_err!("{}: its partial state is not text", self.aggregation.name);
        };
        for row in 0..column.len() {
            if column.is_null(row) {
                continue;
            }
            let other = column.value(row).as_bytes();
            self.state = if self.state.is_empty() {
                other.to_vec()
            } else {
                self.worker
                    .merge(&self.aggregation, &self.state, other)
                    .map_err(|refused| refused_as(&self.aggregation.name, &refused))?
            };
        }
        Ok(())
    }

    fn evaluate(&mut self) -> Result<ScalarValue> {
        if self.state.is_empty() {
            // Nothing was accumulated. `NULL` rather than zero: a group with no rows and a
            // group whose rows summed to zero are different facts, and every aggregate in this
            // system keeps them apart.
            return Ok(ScalarValue::Float64(None));
        }
        let answer = self
            .worker
            .finish(&self.aggregation, &self.state)
            .map_err(|refused| refused_as(&self.aggregation.name, &refused))?;
        Ok(ScalarValue::Float64(Some(answer)))
    }

    fn state(&mut self) -> Result<Vec<ScalarValue>> {
        Ok(vec![ScalarValue::Utf8(if self.state.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&self.state).into_owned())
        })])
    }

    fn size(&self) -> usize {
        std::mem::size_of_val(self) + self.state.len()
    }
}

/// A refusal from the worker, as an execution error naming the aggregation.
fn refused_as(name: &str, refused: &sankhya_udf::Refused) -> datafusion::error::DataFusionError {
    datafusion::error::DataFusionError::Execution(format!("{name}: {refused}"))
}
