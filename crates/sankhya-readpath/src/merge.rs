//! Resolving several versions of a row into the one a query should see.
//!
//! # The defect this exists to fix
//!
//! Capture records inserts, updates and deletes as rows. Unioning the tiers and stopping
//! there returns a row that has been updated **twice** — once as it was, once as it is.
//! `COUNT(*)` says two; `SUM` adds the old value to the new one.
//!
//! Nothing about the result indicates it, and every correctness property built around it
//! holds: exactly-once capture, reconciliation against the source, splice coverage. None
//! of them is about *resolving* two versions of a row, so none of them fails.
//!
//! # Why the strategy is declared rather than inferred
//!
//! A table's capability is a statement about what the source does to it, and the source
//! is the only thing that knows. Inferring "this table looks append-only because no
//! update has arrived yet" is correct until the first update, at which point every query
//! silently starts double-counting — the exact failure this module exists to prevent,
//! reintroduced by the mechanism meant to avoid configuring it.
//!
//! # Why append-only pays nothing
//!
//! Most high-volume tables are append-only, so most queries take that path, and it has to
//! cost nothing at all — not "a cheap check", nothing. A table declared append-only is
//! scanned exactly as it was before this module existed.

use std::fmt;

/// What the source does to a table, and therefore how its rows must be resolved.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Capability {
    /// Rows are only ever added.
    ///
    /// Union, no deduplication, no sort, no key comparison.
    AppendOnly,
    /// Rows are updated and deleted in place.
    ///
    /// The latest version of each key wins, and a key whose latest version is a deletion
    /// is absent.
    Mutable {
        /// The columns that identify a row.
        ///
        /// Must match what the source treats as the row's identity. A key that is not
        /// unique in the source resolves several distinct rows into one and loses the
        /// rest — silently, and in a way that looks like the deduplication working.
        key: Vec<String>,
    },
}

impl Capability {
    /// A mutable table keyed by the named columns.
    ///
    /// # Errors
    ///
    /// Refuses an empty key. A mutable table with no identity has no "latest version of
    /// each key" to resolve to, and defaulting to whole-row identity would silently turn
    /// every update into a new row — which is the unresolved behaviour, arrived at by a
    /// different route.
    pub fn mutable<I, S>(key: I) -> Result<Self, CapabilityError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let key: Vec<String> = key.into_iter().map(Into::into).collect();
        if key.is_empty() {
            return Err(CapabilityError::NoKey);
        }
        Ok(Self::Mutable { key })
    }

    /// The capability the source itself declared.
    ///
    /// # Why reading this is not a heuristic
    ///
    /// The architecture requires the strategy to be *declared*, never guessed. Guessing
    /// would mean something like "no update has arrived yet, so treat it as append-only" —
    /// correct until the first update, after which every query silently double-counts.
    ///
    /// This is not that. The source states which columns identify a row — its replica
    /// identity — and that statement is what arrives with the relation. Reading it is
    /// taking the declaration, not inferring one.
    ///
    /// A relation that declares no identifying columns is append-only **as far as reading
    /// is concerned**, and that is a statement about what can be done rather than about
    /// what will happen: without a row identity there is no key to resolve versions
    /// against, so an update could not be applied even if one arrived. The right response
    /// to updates on such a table is to fix the source's replica identity, and the
    /// onboarding path already warns about it.
    #[must_use]
    pub fn from_source<I, S>(key_columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let key: Vec<String> = key_columns.into_iter().map(Into::into).collect();
        if key.is_empty() {
            return Self::AppendOnly;
        }
        Self::Mutable { key }
    }

    /// Whether reading this table requires resolving versions.
    #[must_use]
    pub const fn needs_resolution(&self) -> bool {
        matches!(self, Self::Mutable { .. })
    }

    /// The identifying columns, or nothing for an append-only table.
    #[must_use]
    pub fn key(&self) -> &[String] {
        match self {
            Self::AppendOnly => &[],
            Self::Mutable { key } => key,
        }
    }
}

/// Why a capability could not be declared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CapabilityError {
    /// A mutable table was declared with no identifying columns.
    NoKey,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "a mutable table needs the columns that identify a row; without them there is \
             no latest-version-per-key to resolve to, and treating the whole row as the \
             key turns every update into a new row",
        )
    }
}

impl std::error::Error for CapabilityError {}

use arrow_schema::SchemaRef;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::{Column, DataFusionError, Result as DfResult};
use datafusion::datasource::provider_as_source;
use datafusion::logical_expr::{
    col, lit, Expr, LogicalPlan, LogicalPlanBuilder, TableProviderFilterPushDown, TableType,
};
use datafusion::physical_plan::ExecutionPlan;
use std::borrow::Cow;
use std::sync::Arc;

use crate::provider::SankhyaTable;
use crate::COMMIT_LSN;

/// The column recording what the source did to a row.
const COMMIT_OP: &str = "_sankhya_op";

/// The marker a deletion carries.
const DELETED: &str = "D";

/// A mutable table, with each key resolved to its latest version.
///
/// # Why this is a logical plan rather than a physical one
///
/// The resolution is `DISTINCT ON (key) … ORDER BY key, position DESC`, then a filter that
/// drops keys whose latest version is a deletion. Expressed logically, the optimizer sees
/// it: it can push the query's own predicates through, choose how to sort, and combine the
/// distinct with whatever follows. Built physically it would be opaque, and every
/// optimisation would have to be re-implemented inside it.
///
/// # Why the deletion filter comes after the distinct, not before
///
/// A deletion must suppress the row entirely. Filtering deletions first removes the
/// tombstone and leaves the *previous* version to win the distinct — so a deleted row
/// comes back, holding the values it had before it was deleted. That is a worse failure
/// than the one this module fixes, because it looks like data rather than like
/// duplication.
#[derive(Debug)]
pub struct ResolvedTable {
    raw: Arc<SankhyaTable>,
    plan: LogicalPlan,
    schema: SchemaRef,
}

impl ResolvedTable {
    /// Wrap a raw scan so each key resolves to its latest version.
    ///
    /// # Errors
    ///
    /// Returns an error if a key column is not in the table's schema, or if the table
    /// lacks the commit-position and operation columns the resolution orders by — a table
    /// without those did not come through capture, and there is nothing to resolve
    /// against.
    pub fn new(raw: Arc<SankhyaTable>, key: &[String]) -> DfResult<Self> {
        let schema = raw.schema();

        for name in key {
            if schema.index_of(name).is_err() {
                return Err(DataFusionError::Plan(format!(
                    "the declared key names {name}, which is not a column of this table; \
                     resolving on a column that does not exist would silently keep every \
                     version of every row"
                )));
            }
        }
        for required in [COMMIT_LSN, COMMIT_OP] {
            if schema.index_of(required).is_err() {
                return Err(DataFusionError::Plan(format!(
                    "this table has no {required} column, so it did not come through \
                     capture and there is nothing to resolve versions against"
                )));
            }
        }

        let source = provider_as_source(Arc::clone(&raw) as Arc<dyn TableProvider>);
        let scan = LogicalPlanBuilder::scan("sankhya_raw", source, None)?;

        // Latest wins: order by the key, then by position descending, and keep the first
        // of each key.
        let on: Vec<Expr> = key.iter().map(|k| col(k)).collect();
        let mut sort: Vec<datafusion::logical_expr::SortExpr> =
            key.iter().map(|k| col(k).sort(true, false)).collect();
        sort.push(col(COMMIT_LSN).sort(false, false));

        // Every column by name rather than a wildcard: a wildcard here would be resolved
        // against whatever the plan happens to expose, and the resolution must select
        // exactly the table's own columns.
        let select: Vec<Expr> = schema.fields().iter().map(|f| col(f.name())).collect();

        let plan = scan
            .distinct_on(on, select, Some(sort))?
            // After the distinct, so a tombstone suppresses the row rather than being
            // removed and letting the previous version win.
            .filter(col(COMMIT_OP).not_eq(lit(DELETED)))?
            .build()?;

        Ok(Self { raw, schema, plan })
    }

    /// The unresolved rows, for a caller that wants the history rather than the state.
    #[must_use]
    pub fn raw(&self) -> &Arc<SankhyaTable> {
        &self.raw
    }
}

#[async_trait::async_trait]
impl TableProvider for ResolvedTable {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn table_type(&self) -> TableType {
        TableType::View
    }

    /// The resolution, which the planner inlines like a view.
    fn get_logical_plan(&self) -> Option<Cow<'_, LogicalPlan>> {
        Some(Cow::Borrowed(&self.plan))
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DfResult<Vec<TableProviderFilterPushDown>> {
        // Inexact, as the raw scan reports. A filter pushed below the distinct could
        // remove the very version that was going to win, leaving an older one in its
        // place — so the engine must apply it again above.
        Ok(vec![TableProviderFilterPushDown::Inexact; filters.len()])
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        _projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        // Unreachable in practice: the planner inlines `get_logical_plan` instead of
        // calling this. Returning an error rather than a raw scan matters — a raw scan
        // here would silently serve unresolved rows, which is the defect this type
        // exists to fix.
        Err(DataFusionError::Internal(
            "a resolved table is planned through its logical plan; reaching scan() means \
             the planner did not inline it, and serving a raw scan here would return \
             every version of every row"
                .to_string(),
        ))
    }
}

/// Silence the unused import when the column constant is only used above.
const _: Option<Column> = None;
