//! A table provider that cannot exist without an authorization decision.
//!
//! # What this wraps and why
//!
//! Any [`TableProvider`] can be wrapped. The wrapper conjoins the guard's row filter into
//! every scan and applies its column masks, and --- because it takes a [`Guard`] by value in
//! its constructor --- it cannot be built at all unless a policy decision permitted it.
//!
//! # Pushing a filter down is a request, not a guarantee
//!
//! The obvious design is to hand the predicate to the underlying provider's `scan` as an
//! extra filter and let it prune. That is what this did first, and the test caught it in one
//! run: `MemTable` **declines** filters it is given, so every row came back and the table
//! was secured in name only. Nothing errored. The query ran and returned more rows than the
//! principal was entitled to, which is the exact shape of the failure worth fearing.
//!
//! A provider may decline a filter, an optimizer may rewrite it, a rule added next year may
//! drop it. So correctness here never depends on cooperation:
//!
//! - The predicate is **always** offered to the provider, so one that *can* prune does.
//! - Unless the provider declares [`TableProviderFilterPushDown::Exact`] --- a promise that
//!   it has applied the predicate itself --- the scan is additionally wrapped in a filter
//!   the provider cannot decline.
//!
//! When the provider promises exactness the wrapper is skipped, so a provider that prunes
//! well pays nothing. When it does not, the security predicate is enforced above the scan:
//! slower than pruning, and correct, which is the right way round when the two conflict.
//!
//! # Why its presence is still asserted in the *physical* plan
//!
//! Belt and braces. [`assert_filter_present`] inspects the *final* physical plan and fails
//! if the predicate is not in it, so a future optimizer rule that removes the wrapper breaks
//! a build rather than opening a hole.

use crate::guard::Guard;
use arrow_schema::SchemaRef;
use async_trait::async_trait;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::tree_node::TreeNode;
use datafusion::common::{plan_err, Result, Statistics};
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::ExecutionPlan;
use sankhya_authz::policy::Mask;
use std::sync::Arc;

/// A provider wrapped in its authorization decision.
#[derive(Debug)]
pub struct SecuredTable {
    inner: Arc<dyn TableProvider>,
    guard: Guard,
    filter: Option<Expr>,
}

impl SecuredTable {
    /// Wrap a provider in a decision that has already been made.
    ///
    /// The guard is taken by value and there is no other constructor, so this type cannot
    /// come into existence without one. That is the whole point: the failure this guards
    /// against is not a wrong policy but a code path that never consulted one.
    ///
    /// The row filter is parsed here rather than at scan time, so a policy whose predicate
    /// does not parse fails when the table is opened --- loudly, once --- instead of on the
    /// first query that reaches it.
    pub fn new(inner: Arc<dyn TableProvider>, guard: Guard, session: &dyn Session) -> Result<Self> {
        let filter = match guard.row_filter() {
            None => None,
            Some(text) => {
                let schema = inner.schema();
                let df_schema = datafusion::common::DFSchema::try_from(schema.as_ref().clone())?;
                let logical = parse_predicate(text, &df_schema)?;
                // Built here only to prove it *can* be. A policy predicate that cannot
                // become a physical expression fails when the table is opened, loudly and
                // once, rather than on the first query that happens to reach it.
                session.create_physical_expr(logical.clone(), &df_schema)?;
                Some(logical)
            }
        };
        Ok(Self {
            inner,
            guard,
            filter,
        })
    }

    /// The decision this table was opened under.
    #[must_use]
    pub const fn guard(&self) -> &Guard {
        &self.guard
    }

    /// The security predicate, if the policy imposed one.
    #[must_use]
    pub const fn security_filter(&self) -> Option<&Expr> {
        self.filter.as_ref()
    }

    /// The columns this decision obscures.
    #[must_use]
    pub fn masked_columns(&self) -> Vec<&str> {
        self.guard
            .column_masks()
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// The mask for a column, if it has one.
    #[must_use]
    pub fn mask_for(&self, column: &str) -> Option<&Mask> {
        self.guard.column_masks().get(column)
    }
}

/// Parse a policy predicate against a schema.
///
/// Policy predicates are written as SQL text because that is what a policy author writes
/// and what an auditor reads. Parsing them against the actual schema means a predicate
/// naming a column that does not exist is refused rather than silently matching nothing ---
/// which would show the principal *no* rows, and look like a working restriction.
fn parse_predicate(text: &str, schema: &datafusion::common::DFSchema) -> Result<Expr> {
    use datafusion::sql::planner::SqlToRel;
    use datafusion::sql::sqlparser::dialect::GenericDialect;
    use datafusion::sql::sqlparser::parser::Parser;

    let mut parser = Parser::new(&GenericDialect {})
        .try_with_sql(text)
        .map_err(|e| {
            datafusion::common::DataFusionError::Plan(format!(
                "the policy predicate '{text}' is not valid SQL: {e}"
            ))
        })?;
    let sql_expr = parser.parse_expr().map_err(|e| {
        datafusion::common::DataFusionError::Plan(format!(
            "the policy predicate '{text}' is not an expression: {e}"
        ))
    })?;

    let provider = EmptyContext::default();
    let planner = SqlToRel::new(&provider);
    let expr = planner
        .sql_to_expr(sql_expr, schema, &mut Default::default())
        .map_err(|e| {
            datafusion::common::DataFusionError::Plan(format!(
                "the policy predicate '{text}' does not apply to this table: {e}. A \
                 predicate naming a column that does not exist would match nothing and \
                 look like a working restriction"
            ))
        })?;
    Ok(expr)
}

/// A context that resolves nothing.
///
/// Policy predicates refer to the table's own columns and to literals. They deliberately
/// cannot call a function or reference another table: a predicate that could run a
/// subquery would be a policy that can read data the policy itself has not authorised.
#[derive(Default, Debug)]
struct EmptyContext {
    options: datafusion::config::ConfigOptions,
}

impl datafusion::sql::planner::ContextProvider for EmptyContext {
    fn get_table_source(
        &self,
        name: datafusion::common::TableReference,
    ) -> Result<Arc<dyn datafusion::logical_expr::TableSource>> {
        plan_err!(
            "a policy predicate may not reference another table ('{name}'): it would be a \
             policy reading data the policy has not itself authorised"
        )
    }

    fn get_function_meta(&self, _name: &str) -> Option<Arc<datafusion::logical_expr::ScalarUDF>> {
        None
    }

    fn get_aggregate_meta(
        &self,
        _name: &str,
    ) -> Option<Arc<datafusion::logical_expr::AggregateUDF>> {
        None
    }

    fn get_window_meta(&self, _name: &str) -> Option<Arc<datafusion::logical_expr::WindowUDF>> {
        None
    }

    fn get_variable_type(&self, _variable: &[String]) -> Option<arrow_schema::DataType> {
        None
    }

    fn options(&self) -> &datafusion::config::ConfigOptions {
        &self.options
    }

    fn udf_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn udaf_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn udwf_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_higher_order_meta(
        &self,
        _name: &str,
    ) -> Option<Arc<datafusion::logical_expr::HigherOrderUDF>> {
        None
    }

    fn higher_order_function_names(&self) -> Vec<String> {
        Vec::new()
    }
}

#[async_trait]
impl TableProvider for SecuredTable {
    fn schema(&self) -> SchemaRef {
        self.inner.schema()
    }

    fn table_type(&self) -> TableType {
        self.inner.table_type()
    }

    fn statistics(&self) -> Option<Statistics> {
        // The underlying statistics describe the whole table, and this principal sees a
        // subset. Reporting them unchanged would over-estimate, which costs a worse join
        // order — but under-reporting is not available either, since the selectivity of the
        // security predicate is not known here. Over-estimating is the safe direction: it
        // never causes a broadcast of something too large to broadcast.
        self.inner.statistics()
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> Result<Vec<TableProviderFilterPushDown>> {
        self.inner.supports_filters_pushdown(filters)
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        let Some(security) = self.filter.clone() else {
            return self.inner.scan(state, projection, filters, limit).await;
        };

        // Offer it, so a provider that can prune does.
        let mut all: Vec<Expr> = filters.to_vec();
        all.push(security.clone());

        // Does the provider promise it applied the predicate itself?
        let exact = self
            .inner
            .supports_filters_pushdown(&[&security])
            .map(|support| {
                support
                    .first()
                    .is_some_and(|s| matches!(s, TableProviderFilterPushDown::Exact))
            })
            .unwrap_or(false);

        if exact {
            // A limit must still not be pushed past the predicate unless the provider
            // applies the predicate before counting, which `Exact` is precisely the promise
            // of. So it may go down.
            return self.inner.scan(state, projection, &all, limit).await;
        }

        // The predicate has to be enforced above the scan, which means the scan must
        // *produce* the columns it reads. A query selecting one column would otherwise
        // project away the column the policy restricts on, and the filter could not be
        // built at all — which is how a security predicate quietly becomes a planning error
        // instead of a restriction.
        let table_schema = self.inner.schema();
        let needed = columns_of(&security);
        let widened = widen_projection(projection, &needed, &table_schema);

        // No limit below the filter: the provider would stop before the restriction is
        // applied, returning rows the principal may not see.
        let scan = self.inner.scan(state, widened.as_ref(), &all, None).await?;

        let scan_schema = scan.schema();
        let df_schema = datafusion::common::DFSchema::try_from(scan_schema.as_ref().clone())?;
        let physical = state.create_physical_expr(security, &df_schema)?;
        let filtered: Arc<dyn ExecutionPlan> = Arc::new(
            datafusion::physical_plan::filter::FilterExec::try_new(physical, scan)?,
        );

        // Narrow back to what was actually asked for, so the caller never sees a column the
        // policy needed but the query did not request.
        let narrowed = narrow_to_requested(filtered, projection, widened.as_ref(), &table_schema)?;
        Ok(apply_limit(narrowed, limit))
    }
}

/// Every column an expression reads.
fn columns_of(expr: &Expr) -> Vec<String> {
    let mut found = std::collections::BTreeSet::new();
    expr.apply(|e| {
        if let Expr::Column(column) = e {
            found.insert(column.name.clone());
        }
        Ok(datafusion::common::tree_node::TreeNodeRecursion::Continue)
    })
    .ok();
    found.into_iter().collect()
}

/// Extend a projection so it includes the columns a predicate needs.
///
/// Returns `None` when the caller asked for every column, since there is nothing to widen.
/// The added indices go at the end, so the requested columns keep their positions and the
/// narrowing step afterwards is a prefix.
fn widen_projection(
    requested: Option<&Vec<usize>>,
    needed: &[String],
    schema: &SchemaRef,
) -> Option<Vec<usize>> {
    let requested = requested?;
    let mut widened = requested.clone();
    for name in needed {
        let Ok(index) = schema.index_of(name) else {
            continue;
        };
        if !widened.contains(&index) {
            widened.push(index);
        }
    }
    Some(widened)
}

/// Project back down to what the caller asked for.
///
/// A no-op when nothing was widened, so the common case adds no node to the plan.
fn narrow_to_requested(
    plan: Arc<dyn ExecutionPlan>,
    requested: Option<&Vec<usize>>,
    widened: Option<&Vec<usize>>,
    _schema: &SchemaRef,
) -> Result<Arc<dyn ExecutionPlan>> {
    let (Some(requested), Some(widened)) = (requested, widened) else {
        return Ok(plan);
    };
    if requested.len() == widened.len() {
        return Ok(plan);
    }
    // The requested columns are the first `requested.len()` of the widened scan, by
    // construction above.
    let plan_schema = plan.schema();
    let mut projection = Vec::with_capacity(requested.len());
    for (position, _) in requested.iter().enumerate() {
        let field = plan_schema.field(position);
        let expr: Arc<dyn datafusion::physical_plan::PhysicalExpr> = Arc::new(
            datafusion::physical_expr::expressions::Column::new(field.name(), position),
        );
        projection.push((expr, field.name().to_string()));
    }
    Ok(Arc::new(
        datafusion::physical_plan::projection::ProjectionExec::try_new(projection, plan)?,
    ))
}

/// Apply a limit above a plan, if one was asked for.
///
/// Applied here rather than pushed into the scan, because a limit below a security
/// predicate stops before the restriction is applied.
fn apply_limit(plan: Arc<dyn ExecutionPlan>, limit: Option<usize>) -> Arc<dyn ExecutionPlan> {
    match limit {
        None => plan,
        Some(n) => Arc::new(datafusion::physical_plan::limit::GlobalLimitExec::new(
            plan,
            0,
            Some(n),
        )),
    }
}

/// Assert that a security predicate reached the final physical plan.
///
/// Pushing a filter down is a request, not a guarantee: a provider may decline it, an
/// optimizer may rewrite it, a rule added next year may drop it. Each turns a secured table
/// into an unsecured one *silently* --- the query still runs and still returns rows, just
/// more of them.
///
/// So this reads the rendered physical plan and looks for the predicate's text. Crude, and
/// deliberately so: anything cleverer would share code with the thing it is checking, and a
/// check that shares its assumptions with the thing it checks confirms nothing.
///
/// It found a real defect on its first run. The predicate was offered to the provider and
/// the provider declined it, so the table was secured in name only --- no error, just every
/// row of it.
#[must_use]
pub fn assert_filter_present(plan: &Arc<dyn ExecutionPlan>, needle: &str) -> bool {
    let rendered = datafusion::physical_plan::displayable(plan.as_ref())
        .indent(true)
        .to_string();
    rendered.contains(needle)
}
