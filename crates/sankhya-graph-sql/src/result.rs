//! The table a traversal becomes.
//!
//! A materialised batch with exact statistics. Exact because the traversal has already run
//! by the time this exists --- there is nothing left to estimate, and telling the planner a
//! guess when the truth is available is how a downstream join gets ordered backwards.

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use async_trait::async_trait;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::stats::Precision;
use datafusion::common::{Result, Statistics};
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::source::DataSourceExec;
use datafusion::logical_expr::{Expr, TableType};
use datafusion::physical_plan::ExecutionPlan;
use std::sync::Arc;

/// The rows a traversal produced, ready to be joined against.
#[derive(Debug)]
pub struct TraversalTable {
    schema: SchemaRef,
    batch: RecordBatch,
}

impl TraversalTable {
    /// Wrap a finished traversal.
    #[must_use]
    pub fn new(batch: RecordBatch) -> Self {
        Self {
            schema: batch.schema(),
            batch,
        }
    }

    /// How many rows the traversal produced.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.batch.num_rows()
    }
}

#[async_trait]
impl TableProvider for TraversalTable {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    /// Exact, because the rows already exist.
    ///
    /// `Precision::Exact` rather than `Inexact` is the whole point. A traversal returning
    /// forty rows joined against a million-row table should drive the join, and a planner
    /// told the count is a guess will hedge. This is the surface M3 proved matters: an
    /// absent or vague figure moved every join in a six-way query.
    fn statistics(&self) -> Option<Statistics> {
        Some(Statistics {
            num_rows: Precision::Exact(self.batch.num_rows()),
            total_byte_size: Precision::Exact(self.batch.get_array_memory_size()),
            column_statistics: Statistics::unknown_column(&self.schema),
        })
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        let source = MemorySourceConfig::try_new(
            &[vec![self.batch.clone()]],
            Arc::clone(&self.schema),
            projection.cloned(),
        )?;
        Ok(DataSourceExec::from_data_source(source))
    }
}
