//! SANKHYA's own table provider.
//!
//! # What "metadata-only coupling" means in practice
//!
//! The table format library tells us **which files exist and what is in them**. It does
//! not read them, does not decode them, and does not appear in the execution plan. Scan
//! execution is the query engine's own Parquet source, unmodified.
//!
//! The reason is a version-skew one and it is worth being concrete about. A table format
//! library and a query engine move on independent schedules, and both expose Arrow types
//! in their signatures. Coupling to *both* execution surfaces means every upgrade of
//! either is a coordinated upgrade of the pair, several times a year. Coupling to one
//! for metadata and the other for execution means a format upgrade touches a file list
//! and an engine upgrade touches a plan, and neither is a negotiation.
//!
//! # Why planning does no file I/O
//!
//! Statistics come from the table log, which already records how many rows each file
//! holds. The alternative — opening every Parquet footer at planning time — costs one
//! round trip per file before a single row is read, which is exactly the small-file
//! penalty compaction exists to reduce, moved to a place compaction cannot help.
//!
//! # The splice is resolved at construction, not at scan
//!
//! A query pinned at a position must see one consistent set of tiers, and resolving them
//! inside `scan` would let two scans of the same table in one query disagree — a
//! self-join whose halves saw different data. So the tiers are resolved once, the proof
//! is kept, and `scan` only builds a plan over what was already decided.

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::{Result as DfResult, Statistics};
use datafusion::datasource::listing::PartitionedFile;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::physical_plan::{FileScanConfigBuilder, ParquetSource};
use datafusion::datasource::source::DataSourceExec;
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_expr::expressions::{col, lit, BinaryExpr};
use datafusion::physical_plan::filter::FilterExec;
use datafusion::physical_plan::projection::ProjectionExec;
use datafusion::physical_plan::union::UnionExec;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::scalar::ScalarValue;
use sankhya_plan::{plan_splice, Splice, TierRef};
use sankhya_stats::{can_skip, ColumnStats, Predicate};
use sankhya_table_delta::live_files;
use sankhya_table_memory::ArrivalBuffer;
use sankhya_types::{Lsn, LsnRange};
use std::collections::BTreeMap;

use crate::ReadError;

use crate::COMMIT_LSN;

/// One published file, as the log describes it.
#[derive(Clone, PartialEq, Debug)]
pub struct LoggedFile {
    /// Absolute path.
    pub path: String,
    pub size: u64,
    /// Rows the log says this file holds.
    ///
    /// Exact, and taken from the log rather than from the file's own footer. This is
    /// what lets planning cost nothing per file.
    pub rows: u64,
    /// Per-column statistics, where the catalogue has them.
    ///
    /// Absent means "nothing is known", which means the file is read. It never means
    /// "no values" — see the statistics crate on why unknown and unbounded must not be
    /// conflated.
    pub stats: BTreeMap<String, ColumnStats>,
}

impl LoggedFile {
    #[must_use]
    pub fn new(path: String, size: u64, rows: u64) -> Self {
        Self {
            path,
            size,
            rows,
            stats: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_stats(mut self, stats: BTreeMap<String, ColumnStats>) -> Self {
        self.stats = stats;
        self
    }

    /// Whether the catalogue proves this file cannot satisfy `predicates`.
    ///
    /// Every predicate must hold for a row to match, so proving *any one* of them
    /// impossible is enough to skip the file. A predicate on a column the catalogue
    /// knows nothing about proves nothing and is ignored.
    #[must_use]
    pub fn provably_irrelevant(&self, predicates: &[(String, Predicate)]) -> bool {
        predicates.iter().any(|(column, predicate)| {
            self.stats
                .get(column)
                .is_some_and(|stats| can_skip(stats, predicate))
        })
    }
}

/// A table SANKHYA answers for, over a proven set of tiers.
#[derive(Debug)]
pub struct SankhyaTable {
    schema: SchemaRef,
    published: Vec<LoggedFile>,
    arrival: Vec<RecordBatch>,
    target: Lsn,
    splice: Splice,
    /// True when the tiers hold nothing past the target, so no row can be filtered out
    /// and the row count is exact rather than an upper bound.
    exact_counts: bool,
}

impl SankhyaTable {
    /// A provider over an already-resolved splice.
    ///
    /// Takes the [`Splice`] rather than computing it, because the proof belongs to the
    /// caller that decided which position to read at — and because a provider that
    /// planned its own splice would resolve a different one for every scan.
    #[must_use]
    pub fn new(
        schema: SchemaRef,
        published: Vec<LoggedFile>,
        arrival: Vec<RecordBatch>,
        target: Lsn,
        splice: Splice,
        exact_counts: bool,
    ) -> Self {
        Self {
            schema,
            published,
            arrival,
            target,
            splice,
            exact_counts,
        }
    }

    /// Which tiers answered, over which intervals.
    #[must_use]
    pub const fn splice(&self) -> &Splice {
        &self.splice
    }

    /// How many published files the catalogue can prove irrelevant to `filters`.
    ///
    /// Exposed so a caller can see pruning happening. A pruning mechanism nobody can
    /// observe is one nobody notices has stopped working.
    #[must_use]
    pub fn prunable(&self, filters: &[Expr]) -> usize {
        let predicates = crate::predicate::extract(filters);
        if predicates.is_empty() {
            return 0;
        }
        self.published
            .iter()
            .filter(|f| f.provably_irrelevant(&predicates))
            .count()
    }

    /// Rows across every tier, before the target filter.
    #[must_use]
    pub fn declared_rows(&self) -> u64 {
        let published: u64 = self.published.iter().map(|f| f.rows).sum();
        let arrival: u64 = self
            .arrival
            .iter()
            .map(|b| u64::try_from(b.num_rows()).unwrap_or(0))
            .sum();
        published.saturating_add(arrival)
    }

    /// `_sankhya_commit_lsn <= target`, as a physical expression.
    ///
    /// Applied above the union rather than per tier, so there is exactly one place the
    /// target is enforced. A per-tier filter would need a correctness argument per tier;
    /// this needs one argument, once.
    fn target_filter(
        &self,
        input_schema: &SchemaRef,
    ) -> DfResult<Arc<dyn datafusion::physical_expr::PhysicalExpr>> {
        Ok(Arc::new(BinaryExpr::new(
            col(COMMIT_LSN, input_schema)?,
            datafusion::logical_expr::Operator::LtEq,
            lit(ScalarValue::UInt64(Some(self.target.get()))),
        )))
    }

    /// The index of the commit-position column in the table schema.
    fn lsn_index(&self) -> DfResult<usize> {
        self.schema.index_of(COMMIT_LSN).map_err(Into::into)
    }

    /// Columns the scan must read: everything the query asked for, plus the commit
    /// position.
    ///
    /// The position column is read even when the query does not select it, because the
    /// target filter is evaluated on it. It is projected away afterwards, so a caller
    /// asking for one column still gets one column — but the scan cost includes a column
    /// they did not ask for, which is the honest price of reading at a pinned position.
    fn scan_indices(&self, projection: Option<&Vec<usize>>) -> DfResult<(Vec<usize>, bool)> {
        let lsn = self.lsn_index()?;
        let Some(requested) = projection else {
            return Ok(((0..self.schema.fields().len()).collect(), false));
        };
        let mut indices = requested.clone();
        let added = !indices.contains(&lsn);
        if added {
            indices.push(lsn);
        }
        Ok((indices, added))
    }

    fn published_plan(
        &self,
        indices: &[usize],
        predicates: &[(String, Predicate)],
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        // The engine's own Parquet source, unmodified. Nothing SANKHYA-specific reaches
        // execution — see the module documentation on why that separation is the point.
        let source = Arc::new(ParquetSource::new(Arc::clone(&self.schema)));
        let mut builder = FileScanConfigBuilder::new(ObjectStoreUrl::local_filesystem(), source);

        let mut files = Vec::with_capacity(self.published.len());
        for file in &self.published {
            // Skipping happens here rather than in the engine, because the catalogue is
            // SANKHYA's and the engine has never seen it. A file the catalogue proves
            // irrelevant is never named in the plan at all, so its footer is never read.
            if file.provably_irrelevant(predicates) {
                continue;
            }
            let trimmed = file.path.trim_start_matches('/');
            let mut partitioned = PartitionedFile::new(trimmed.to_string(), file.size);
            // The row count the log recorded, handed to the engine rather than read
            // back off disk.
            partitioned.statistics = Some(Arc::new(Statistics {
                num_rows: exact(file.rows),
                total_byte_size: exact(file.size),
                column_statistics: Statistics::unknown_column(&self.schema),
            }));
            files.push(partitioned);
        }
        builder = builder
            .with_file_group(files.into())
            .with_projection_indices(Some(indices.to_vec()))?;

        Ok(DataSourceExec::from_data_source(builder.build()))
    }

    fn arrival_plan(&self, indices: &[usize]) -> DfResult<Option<Arc<dyn ExecutionPlan>>> {
        if self.arrival.is_empty() {
            return Ok(None);
        }
        let source = MemorySourceConfig::try_new(
            &[self.arrival.clone()],
            Arc::clone(&self.schema),
            Some(indices.to_vec()),
        )?;
        Ok(Some(DataSourceExec::from_data_source(source)))
    }
}

fn exact(value: u64) -> datafusion::common::stats::Precision<usize> {
    datafusion::common::stats::Precision::Exact(usize::try_from(value).unwrap_or(usize::MAX))
}

#[async_trait::async_trait]
impl TableProvider for SankhyaTable {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    /// Row counts from the log, with no file opened.
    ///
    /// Marked exact only when the tiers hold nothing past the target. Otherwise the
    /// count is an upper bound, and reporting it as exact would let the optimizer make
    /// join-ordering decisions on a number that is simply wrong.
    fn statistics(&self) -> Option<Statistics> {
        let rows = usize::try_from(self.declared_rows()).unwrap_or(usize::MAX);
        let num_rows = if self.exact_counts {
            datafusion::common::stats::Precision::Exact(rows)
        } else {
            datafusion::common::stats::Precision::Inexact(rows)
        };
        Some(Statistics {
            num_rows,
            total_byte_size: datafusion::common::stats::Precision::Absent,
            column_statistics: Statistics::unknown_column(&self.schema),
        })
    }

    /// Filters are evaluated by the engine, not by us.
    ///
    /// Claiming `Exact` would tell the engine it may drop the filter entirely, and this
    /// provider does not evaluate predicates — it selects files. Claiming support it
    /// does not have is how a filter silently stops being applied.
    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DfResult<Vec<TableProviderFilterPushDown>> {
        Ok(vec![TableProviderFilterPushDown::Inexact; filters.len()])
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        _limit: Option<usize>,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        let (indices, lsn_added) = self.scan_indices(projection)?;
        let predicates = crate::predicate::extract(filters);
        let mut parts: Vec<Arc<dyn ExecutionPlan>> = Vec::new();

        if !self.published.is_empty() {
            parts.push(self.published_plan(&indices, &predicates)?);
        }
        if let Some(plan) = self.arrival_plan(&indices)? {
            parts.push(plan);
        }

        let combined: Arc<dyn ExecutionPlan> = match parts.len() {
            0 => {
                return Ok(Arc::new(datafusion::physical_plan::empty::EmptyExec::new(
                    Arc::clone(&self.schema),
                )))
            }
            1 => parts.remove(0),
            _ => UnionExec::try_new(parts)?,
        };

        let scanned_schema = combined.schema();
        let filtered: Arc<dyn ExecutionPlan> = Arc::new(FilterExec::try_new(
            self.target_filter(&scanned_schema)?,
            combined,
        )?);

        if !lsn_added {
            return Ok(filtered);
        }

        // Drop the position column the query did not ask for. It was read only so the
        // target could be enforced.
        let keep: Vec<(Arc<dyn datafusion::physical_expr::PhysicalExpr>, String)> = scanned_schema
            .fields()
            .iter()
            .enumerate()
            .filter(|(_, field)| field.name() != COMMIT_LSN)
            .map(|(index, field)| {
                Ok((
                    Arc::new(datafusion::physical_expr::expressions::Column::new(
                        field.name(),
                        index,
                    )) as Arc<dyn datafusion::physical_expr::PhysicalExpr>,
                    field.name().clone(),
                ))
            })
            .collect::<DfResult<Vec<_>>>()?;

        Ok(Arc::new(ProjectionExec::try_new(keep, filtered)?))
    }
}

/// Build a provider for a table, resolving its tiers as of `target`.
///
/// This is the normal entry point. It reads the table log for the published file set
/// and their row counts, takes the arrival tier's contribution, proves the two cover the
/// query's span exactly once, and refuses if they do not.
///
/// # Errors
///
/// Refuses when the tiers do not cover `(0, target]`, when the log cannot be read, or
/// when the arrival tier cannot be scanned. Refusing is the point: an incomplete answer
/// that looks complete is the worst thing this system can produce, because nothing
/// downstream can detect it.
pub fn resolve(
    schema: SchemaRef,
    table_root: &std::path::Path,
    published_coverage: Option<LsnRange>,
    arrival: Option<&ArrivalBuffer>,
    target: Lsn,
) -> Result<SankhyaTable, ReadError> {
    let mut offered: Vec<TierRef> = Vec::new();
    if let Some(coverage) = published_coverage {
        offered.push(TierRef::new("published", coverage));
    }
    if let Some(coverage) = arrival.and_then(ArrivalBuffer::coverage) {
        offered.push(TierRef::new("arrival", coverage));
    }
    if offered.is_empty() {
        return Err(ReadError::NoTiers);
    }

    // Whether any tier physically holds rows past the target, decided from what the
    // tiers *offered* rather than from what the splice selected.
    //
    // The planner trims each chosen tier so it abuts exactly, so a spliced coverage can
    // never end past the target — asking the splice would always answer no. The arrival
    // tier is excluded because its scan already filters to the target, so it contributes
    // nothing that the filter can remove.
    let published_overshoots =
        published_coverage.is_some_and(|coverage| coverage.end_inclusive() > target);

    let splice = plan_splice(&offered, target)?;

    let mut files = Vec::new();
    let mut batches = Vec::new();

    for tier in &splice.tiers {
        match tier.name {
            "published" => {
                let live = live_files(table_root)?;
                for file in &live.files {
                    // A file whose row count the log does not carry cannot be planned
                    // against. Treating the absence as zero would tell the optimizer the
                    // table is empty, which is a wrong plan rather than a slow one.
                    let rows = file.rows().ok_or_else(|| {
                        ReadError::Engine(format!(
                            "{} is in the log without a row count, so it can be located \
                             but not planned against",
                            file.path
                        ))
                    })?;
                    // Bounds and null counts come from the log, so a restart does not
                    // lose them and a query planned by a fresh process prunes exactly as
                    // one planned by a warm one.
                    let catalogue = file
                        .statistics()
                        .map(|s| sankhya_table_delta::to_column_stats(&s))
                        .unwrap_or_default();

                    files.push(
                        LoggedFile::new(
                            table_root.join(&file.path).to_string_lossy().into_owned(),
                            file.size,
                            rows,
                        )
                        .with_stats(catalogue),
                    );
                }
            }
            "arrival" => {
                let tier = arrival.expect("selected, therefore offered");
                batches = tier.scan(target)?;
            }
            other => {
                return Err(ReadError::Engine(format!(
                    "the planner selected a tier named {other}, which this provider does \
                     not know how to read"
                )))
            }
        }
    }

    Ok(SankhyaTable::new(
        schema,
        files,
        batches,
        target,
        splice,
        !published_overshoots,
    ))
}
