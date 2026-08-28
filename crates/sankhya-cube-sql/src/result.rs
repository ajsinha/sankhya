//! The table a cube result becomes.
//!
//! A materialised batch with exact statistics. Exact because the cube has already been
//! navigated by the time this exists --- there is nothing left to estimate, and telling the
//! planner a guess when the truth is available is how a downstream join gets ordered
//! backwards. `M3` proved that surface matters: a vague row count moved every join in a
//! six-way query.

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

/// The rows a cube query produced, ready to be joined against.
#[derive(Debug)]
pub(crate) struct CubeTable {
    schema: SchemaRef,
    batch: RecordBatch,
}

impl CubeTable {
    /// Wrap a finished cube result.
    ///
    /// The schema is passed rather than taken from the batch: a cube's dimension columns are
    /// named by the cube, and rebuilding the schema from the batch would lose the
    /// nullability that says an absent cell is absent rather than zero.
    #[must_use]
    pub(crate) fn new(schema: SchemaRef, batch: RecordBatch) -> Self {
        Self { schema, batch }
    }

}

#[async_trait]
impl TableProvider for CubeTable {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    /// Exact, because the rows already exist.
    ///
    /// `Precision::Exact` rather than `Inexact` is the whole point. A cube breakdown of
    /// forty rows joined against a million-row table should drive the join, and a planner
    /// told the count is a guess will hedge.
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
