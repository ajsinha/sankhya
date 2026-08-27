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
use sankhya_stats::{can_skip, Bound, ColumnStats, Predicate};
use sankhya_table_delta::{live_files, LogCache};
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
        let Some(requested) = projection else {
            return Ok(((0..self.schema.fields().len()).collect(), false));
        };
        if !self.needs_target_filter() {
            return Ok((requested.clone(), false));
        }
        let lsn = self.lsn_index()?;
        let mut indices = requested.clone();
        let added = !indices.contains(&lsn);
        if added {
            indices.push(lsn);
        }
        Ok((indices, added))
    }

    /// Whether the target can actually remove a row.
    ///
    /// When no tier holds anything past the target, the filter is provably a no-op — and
    /// so is reading the column it filters on. Both are then skipped.
    ///
    /// This is not a micro-optimisation. Enforcing the position costs a column read and a
    /// predicate **per table**, so it compounds with join arity: measured on a six-way
    /// TPC-H join, the always-on form cost 39%. Paying that on a query reading the latest
    /// data — which is most queries — to support reading an older position is the wrong
    /// way round.
    ///
    /// The condition is exactly the one already computed for statistics: if the tiers hold
    /// nothing past the target, nothing can be filtered out. Reusing it rather than
    /// deriving a second, similar condition matters, because two conditions that are meant
    /// to agree eventually do not.
    const fn needs_target_filter(&self) -> bool {
        !self.exact_counts
    }

    fn published_plan(
        &self,
        indices: &[usize],
        predicates: &[(String, Predicate)],
        partitions: usize,
        split_threshold: usize,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        // The engine's own Parquet source, unmodified. Nothing SANKHYA-specific reaches
        // execution — see the module documentation on why that separation is the point.
        let source = Arc::new(ParquetSource::new(Arc::clone(&self.schema)));
        let mut builder = FileScanConfigBuilder::new(ObjectStoreUrl::local_filesystem(), source);

        let mut files = Vec::with_capacity(self.published.len());
        let mut retained = Vec::with_capacity(self.published.len());
        let (mut rows, mut bytes) = (0u64, 0u64);
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
            retained.push(file.clone());
            rows += file.rows;
            bytes += file.size;
        }
        // Whether to group the files here, or hand them over as one group and let the
        // engine do it.
        //
        // The engine splits file groups by byte range, which balances on size and is
        // strictly better than anything this code can do by counting files — but it
        // only does so once the scan is large enough to be worth splitting. Below that
        // threshold it leaves a single group alone, and a single group is a single
        // partition: the scan reads serially while every core above it waits.
        //
        // So the division of labour follows the engine's own threshold. Above it, hand
        // over one group and let the engine balance by bytes. Below it, deal the files
        // out here, because otherwise nobody will.
        //
        // Doing both — dealing first and letting the engine split afterwards — is worse
        // than either, and measurably so: the engine then rebalances an arrangement that
        // was already unbalanced by file count, and the six-way join paid about seven
        // percent for it.
        let dealt = if bytes >= split_threshold as u64 {
            vec![files]
        } else {
            // Round-robin rather than packed by size. Sizes are known and packing would
            // balance better, but it also groups files that were written together —
            // which after compaction means files covering adjacent ranges land in the
            // same partition, so a pruned scan leaves some partitions with nothing and
            // others with everything.
            let groups = partitions.max(1).min(files.len().max(1));
            let mut dealt: Vec<Vec<PartitionedFile>> = vec![Vec::new(); groups];
            for (index, file) in files.into_iter().enumerate() {
                // `groups` is at least one and the modulus is below it, so this resolves.
                if let Some(group) = dealt.get_mut(index % groups) {
                    group.push(file);
                }
            }
            dealt
        };
        // The scan's own statistics, which are not the table's.
        //
        // `TableProvider::statistics` describes the whole table and is read during
        // logical planning. Join selection runs later, on the physical plan, and reads
        // the statistics of the `DataSourceExec` — which come from here and default to
        // unknown. Leaving them unset meant every small table looked unmeasurable at the
        // moment the engine decided how to join it, so it repartitioned tables it could
        // have broadcast: five rows shuffled across every core.
        //
        // Counted over the files that survived pruning rather than the whole table, so a
        // predicate that removes most of the data is reflected in the number the join
        // decision actually uses.
        //
        // Exact in both fields: these are recorded counts and recorded sizes, for a set
        // of files now fixed in the plan.
        let scanned = Statistics {
            num_rows: exact(rows),
            total_byte_size: exact(bytes),
            column_statistics: column_statistics(&self.schema, &retained),
        };

        builder = builder
            .with_statistics(scanned)
            .with_file_groups(dealt.into_iter().map(Into::into).collect())
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

/// A bound as the engine's optimizer wants it.
///
/// Returns `Absent` for anything that cannot be represented exactly. An approximate bound
/// handed to an optimizer is worse than none: it will be trusted, and the resulting plan
/// is chosen confidently on a wrong number.
fn scalar_of(bound: Option<&Bound>) -> datafusion::common::stats::Precision<ScalarValue> {
    use datafusion::common::stats::Precision;
    match bound {
        Some(Bound::Int(v)) => Precision::Exact(ScalarValue::Int64(Some(*v))),
        Some(Bound::Float(v)) if v.is_finite() => Precision::Exact(ScalarValue::Float64(Some(*v))),
        Some(Bound::Bytes(v)) => match std::str::from_utf8(v) {
            Ok(text) => Precision::Exact(ScalarValue::Utf8(Some(text.to_string()))),
            Err(_) => Precision::Absent,
        },
        _ => Precision::Absent,
    }
}

/// Per-column statistics for the whole table, merged across its live files.
///
/// # Why this is worth the merge
///
/// Distinct-value counts are what an optimizer needs to order a join, and neither the
/// file format nor the table log carries them — so without this the engine plans a join
/// on a guess. The guess is usually "the same as the row count", which makes every column
/// look like a key and every join order look equally good.
///
/// # Why every field is exact or absent
///
/// The counts merge exactly, the bounds merge exactly, and the distinct sketch merges
/// exactly in the sense that matters: merging gives the same registers as sketching the
/// union. The *estimate* it produces is approximate, which is why it is reported as
/// inexact — an optimizer told a cardinality is exact may use it to decide a join is a
/// key lookup, and being wrong about that is a different plan rather than a slower one.
fn column_statistics(
    schema: &SchemaRef,
    files: &[LoggedFile],
) -> Vec<datafusion::common::ColumnStatistics> {
    use datafusion::common::stats::Precision;
    use datafusion::common::ColumnStatistics;

    schema
        .fields()
        .iter()
        .map(|field| {
            let mut merged: Option<ColumnStats> = None;
            for file in files {
                let Some(stats) = file.stats.get(field.name()) else {
                    // A file with nothing recorded makes the whole column unknown. Merging
                    // only the files that happen to have statistics would produce bounds
                    // that describe part of the table and claim to describe all of it.
                    return ColumnStatistics::new_unknown();
                };
                match merged.as_mut() {
                    None => merged = Some(stats.clone()),
                    Some(into) => {
                        if into.merge(stats).is_err() {
                            return ColumnStatistics::new_unknown();
                        }
                    }
                }
            }

            let Some(stats) = merged else {
                return ColumnStatistics::new_unknown();
            };

            let distinct = stats.distinct_estimate();
            // The average width the catalogue recorded, times the rows it covers. Absent
            // where nothing was recorded rather than estimated from the type, since a
            // fixed-width guess for a string column is wrong by whatever the data is.
            let byte_size = if stats.total_width == 0 {
                Precision::Absent
            } else {
                Precision::Inexact(usize::try_from(stats.total_width).unwrap_or(usize::MAX))
            };

            ColumnStatistics {
                byte_size,
                null_count: exact(stats.nulls),
                max_value: scalar_of(stats.max.as_ref()),
                min_value: scalar_of(stats.min.as_ref()),
                sum_value: Precision::Absent,
                // Inexact on purpose. The sketch is accurate to a few percent, which is
                // right for choosing a join order and wrong for concluding a column is
                // unique.
                distinct_count: if distinct == 0 {
                    Precision::Absent
                } else {
                    Precision::Inexact(usize::try_from(distinct).unwrap_or(usize::MAX))
                },
            }
        })
        .collect()
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
            // The optimizer orders joins by size, and a table reporting no size is
            // sorted against tables that do — so one absent figure moves every join in
            // the query, not just this table's.
            //
            // Inexact, because it is the compressed size on disk rather than what the
            // rows occupy once decoded, and because the arrival tier's contribution is
            // not counted. Both make it an understatement, which is the safer direction
            // for a build-side decision.
            total_byte_size: if self.published.is_empty() {
                datafusion::common::stats::Precision::Absent
            } else {
                datafusion::common::stats::Precision::Inexact(
                    usize::try_from(self.published.iter().map(|f| f.size).sum::<u64>())
                        .unwrap_or(usize::MAX),
                )
            },
            column_statistics: column_statistics(&self.schema, &self.published),
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
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        _limit: Option<usize>,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        let (indices, lsn_added) = self.scan_indices(projection)?;
        let predicates = crate::predicate::extract(filters);
        let mut parts: Vec<Arc<dyn ExecutionPlan>> = Vec::new();

        if !self.published.is_empty() {
            parts.push(self.published_plan(
                &indices,
                &predicates,
                state.config().target_partitions(),
                state.config_options().optimizer.repartition_file_min_size,
            )?);
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

        if !self.needs_target_filter() {
            // Nothing in any tier is past the target, so the filter would remove nothing
            // and the column it reads was never added to the scan.
            return Ok(combined);
        }

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
    resolve_with(
        schema,
        table_root,
        published_coverage,
        arrival,
        target,
        None,
    )
}

/// The same, reusing a cache of table file sets across query plans.
///
/// A long-running process replans the same table constantly, and without a cache each
/// plan replays the table's whole history to learn about the handful of commits since
/// the last one. The cache cannot go stale — see [`LogCache`] — so passing one is a
/// performance decision and never a correctness one.
///
/// # Errors
///
/// The same conditions as [`resolve`].
pub fn resolve_cached(
    schema: SchemaRef,
    table_root: &std::path::Path,
    published_coverage: Option<LsnRange>,
    arrival: Option<&ArrivalBuffer>,
    target: Lsn,
    cache: &LogCache,
) -> Result<SankhyaTable, ReadError> {
    resolve_with(
        schema,
        table_root,
        published_coverage,
        arrival,
        target,
        Some(cache),
    )
}

fn resolve_with(
    schema: SchemaRef,
    table_root: &std::path::Path,
    published_coverage: Option<LsnRange>,
    arrival: Option<&ArrivalBuffer>,
    target: Lsn,
    cache: Option<&LogCache>,
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
                let live = match cache {
                    Some(cache) => cache.live_files(table_root)?.0,
                    None => live_files(table_root)?,
                };
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
                // The splice selected this tier, which it can only do from what was
                // offered — so it is present. Reported rather than unwrapped because
                // the arm below already reports a tier name the provider cannot read,
                // and a tier it can name but not find is the same class of fault.
                let Some(tier) = arrival else {
                    return Err(ReadError::Engine(
                        "the planner selected the arrival tier, which was not offered".to_string(),
                    ));
                };
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
