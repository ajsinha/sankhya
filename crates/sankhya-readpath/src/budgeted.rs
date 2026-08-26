//! Carrying a deadline into execution.
//!
//! # Why this is a plan node rather than a check in the driver
//!
//! A deadline enforced only where results are consumed stops nothing: the work is
//! already done by the time a batch arrives, and an operator that buffers — a sort, a
//! hash join's build side — produces no batches at all until it has consumed everything.
//! Checking at the top of the plan therefore cancels a query precisely when it was about
//! to finish anyway.
//!
//! Putting the check *inside* the plan means each batch that crosses this node is a
//! chance to stop, and placing the node beneath a buffering operator gives the deadline
//! somewhere to bite while that operator is still filling.
//!
//! # The bound
//!
//! **One batch per partition.** Each partition of the scan runs its own stream and checks
//! the budget independently, so a plan with twenty-four partitions can have twenty-four
//! batches in flight when the deadline passes.
//!
//! Saying "one batch" would be the tidier claim and it would be wrong. The node cannot
//! interrupt an operator that is mid-batch, and it cannot make one partition stop
//! another — the shared cancellation token propagates a *cancellation*, but a deadline is
//! a fact each partition observes for itself.
//!
//! The bound is therefore proportional to parallelism, which is worth stating because
//! parallelism is chosen for throughput and this is what it costs on the other side.
//!
//! # Why the clock is injected
//!
//! A node that reads the wall clock cannot be tested without sleeping, and a test that
//! sleeps is a test nobody runs in a loop. Production passes elapsed milliseconds; tests
//! pass a counter, which makes "stops after three batches" an assertion rather than an
//! observation.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use arrow_schema::SchemaRef;
use datafusion::common::{DataFusionError, Result as DfResult, Statistics};
use datafusion::execution::TaskContext;
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, ExecutionPlanProperties, PlanProperties,
    RecordBatchStream, SendableRecordBatchStream,
};
use futures::{Stream, StreamExt};
use sankhya_governor::{Budget, Stopped};

/// Reads the current tick.
///
/// Shared rather than owned, because every partition's stream needs it and they run on
/// different threads.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// A plan node that stops its input when the budget says to.
#[derive(Clone)]
pub struct BudgetedExec {
    input: Arc<dyn ExecutionPlan>,
    budget: Budget,
    clock: Clock,
    properties: Arc<PlanProperties>,
}

impl BudgetedExec {
    #[must_use]
    pub fn new(input: Arc<dyn ExecutionPlan>, budget: Budget, clock: Clock) -> Self {
        let properties = Arc::new(PlanProperties::new(
            EquivalenceProperties::new(input.schema()),
            input.output_partitioning().clone(),
            input.pipeline_behavior(),
            input.boundedness(),
        ));
        Self {
            input,
            budget,
            clock,
            properties,
        }
    }
}

impl fmt::Debug for BudgetedExec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BudgetedExec")
    }
}

impl DisplayAs for BudgetedExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BudgetedExec: deadline checked every {} batches",
            self.budget.check_every()
        )
    }
}

impl ExecutionPlan for BudgetedExec {
    fn name(&self) -> &str {
        "BudgetedExec"
    }

    fn properties(&self) -> &Arc<PlanProperties> {
        &self.properties
    }

    /// This node holds no expressions of its own.
    fn apply_expressions(
        &self,
        _f: &mut dyn FnMut(
            &Arc<dyn datafusion::physical_expr::PhysicalExpr>,
        ) -> DfResult<datafusion::common::tree_node::TreeNodeRecursion>,
    ) -> DfResult<datafusion::common::tree_node::TreeNodeRecursion> {
        Ok(datafusion::common::tree_node::TreeNodeRecursion::Continue)
    }

    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![&self.input]
    }

    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        let Some(input) = children.into_iter().next() else {
            return Err(DataFusionError::Internal(
                "BudgetedExec requires exactly one child".to_string(),
            ));
        };
        Ok(Arc::new(Self::new(
            input,
            self.budget.clone(),
            Arc::clone(&self.clock),
        )))
    }

    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> DfResult<SendableRecordBatchStream> {
        Ok(Box::pin(BudgetedStream {
            schema: self.input.schema(),
            inner: self.input.execute(partition, context)?,
            budget: self.budget.clone(),
            clock: Arc::clone(&self.clock),
            batches: 0,
        }))
    }

    // Deprecated upstream in favour of a newer statistics context, and overridden
    // anyway: without it this node reports unknown statistics, and an unknown row count
    // in the middle of a plan can change join ordering. Passing them through is a
    // deliberate choice to keep the node invisible to the optimizer, and moving to the
    // replacement API is a follow-up rather than a reason to lose them now.
    #[allow(deprecated)]
    fn partition_statistics(&self, partition: Option<usize>) -> DfResult<Arc<Statistics>> {
        // Unchanged: this node drops nothing when the budget holds, and when it does not
        // the query fails rather than returning fewer rows. A node that reported fewer
        // rows would be describing a truncated answer as a smaller one.
        self.input.partition_statistics(partition)
    }
}

struct BudgetedStream {
    schema: SchemaRef,
    inner: SendableRecordBatchStream,
    budget: Budget,
    clock: Clock,
    batches: u64,
}

impl Stream for BudgetedStream {
    type Item = DfResult<arrow_array::RecordBatch>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        this.batches += 1;

        if let Err(stopped) = this.budget.check_periodically(this.batches, (this.clock)()) {
            // An error, never an early end of stream. Ending quietly would hand the
            // caller a partial result that looks complete, which is the one outcome
            // worse than failing.
            return Poll::Ready(Some(Err(to_error(stopped))));
        }

        this.inner.poll_next_unpin(cx)
    }
}

impl RecordBatchStream for BudgetedStream {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }
}

fn to_error(stopped: Stopped) -> DataFusionError {
    // Wrapped rather than flattened to a string, so the distinction between a deadline
    // and a cancellation survives to whoever has to decide about retrying.
    DataFusionError::External(Box::new(stopped))
}
