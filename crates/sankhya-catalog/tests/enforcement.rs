//! Two tenants, one deployment, and every attempt to cross the boundary.
//!
//! These tests are written from the attacker's side. Each one names something that must
//! **not** happen, and most of them would pass trivially if the feature were absent
//! entirely — which is why several also assert the positive case in the same test. A
//! security test that passes because nothing is wired up is the worst kind.
//!
//! The one that matters most is `the_security_predicate_reaches_the_final_physical_plan`.
//! Pushing a filter down is a request, not a guarantee, and every way it can be declined
//! turns a secured table into an unsecured one silently: the query still runs and still
//! returns rows, just more of them.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::catalog::TableProvider;
use datafusion::datasource::MemTable;
use datafusion::prelude::*;
use sankhya_authz::policy::{Action, Mask, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Authentication, Principal, Role, TenantId};
use sankhya_catalog::guard::Guard;
use sankhya_catalog::secured::{assert_filter_present, SecuredTable};
use std::sync::Arc;

/// A stable tenant identifier for a readable name.
fn tenant(name: &str) -> TenantId {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(name.bytes()) {
        *slot = byte;
    }
    TenantId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn person(name: &str, in_tenant: &str, roles: &[&str]) -> Principal {
    Principal::authenticated(
        name,
        tenant(in_tenant),
        roles.iter().map(|r| Role::new(*r)),
        Authentication::MutualTls,
    )
    .expect("a valid principal")
}

fn orders() -> TableRef {
    TableRef::new("sales", "orders")
}

/// Rows in two regions, so a region predicate has something to exclude.
fn orders_table() -> Arc<dyn TableProvider> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("email", DataType::Utf8, false),
        Field::new("total", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![1, 2, 3, 4])),
            // The forbidden rows come FIRST, deliberately. With the permitted rows first, a
            // limit pushed below the security filter still happens to produce the right
            // answer, and the test that checks for it passes for the wrong reason. The
            // mutation audit caught exactly that.
            Arc::new(StringArray::from(vec!["south", "south", "north", "north"])),
            Arc::new(StringArray::from(vec![
                "a@example.com",
                "b@example.com",
                "c@example.com",
                "d@example.com",
            ])),
            Arc::new(Int64Array::from(vec![10, 20, 30, 40])),
        ],
    )
    .expect("a valid batch");
    Arc::new(MemTable::try_new(schema, vec![vec![batch]]).expect("a valid table"))
}

/// A provider that honours a pushed-down limit, the way a real one does.
///
/// `MemTable` ignores both filters and limits, which makes it a poor stand-in for exactly
/// the behaviour under test: a limit pushed below a security predicate is only dangerous if
/// something acts on it. The mutation audit caught this — the limit test passed against
/// `MemTable` whether or not the limit was pushed down, because nothing honoured it either
/// way.
#[derive(Debug)]
struct LimitHonouringTable {
    inner: Arc<dyn TableProvider>,
}

#[async_trait::async_trait]
impl TableProvider for LimitHonouringTable {
    fn schema(&self) -> arrow_schema::SchemaRef {
        self.inner.schema()
    }

    fn table_type(&self) -> datafusion::logical_expr::TableType {
        self.inner.table_type()
    }

    async fn scan(
        &self,
        state: &dyn datafusion::catalog::Session,
        projection: Option<&Vec<usize>>,
        filters: &[datafusion::logical_expr::Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn datafusion::physical_plan::ExecutionPlan>> {
        let plan = self.inner.scan(state, projection, filters, None).await?;
        match limit {
            None => Ok(plan),
            Some(n) => Ok(Arc::new(
                datafusion::physical_plan::limit::GlobalLimitExec::new(plan, 0, Some(n)),
            )),
        }
    }
}

fn north_only() -> PolicySet {
    PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("north"), orders(), Action::Read)
            .where_rows("region = 'north'"),
    )
}

async fn secured(
    policy: &PolicySet,
    principal: &Principal,
) -> Option<(SessionContext, Arc<SecuredTable>)> {
    let context = SessionContext::new();
    let guard = Guard::authorize(policy, principal, &orders(), Action::Read)?;
    let table = SecuredTable::new(orders_table(), guard, &context.state())
        .expect("the fixture's predicate parses");
    Some((context, Arc::new(table)))
}

// --- the type-level guarantee ---------------------------------------------

#[test]
fn a_guard_cannot_be_obtained_for_a_denied_decision() {
    // The whole point of the type. There is no constructor that yields a guard on refusal,
    // so a caller cannot proceed by ignoring an error the way a Result invites.
    let policy = north_only();

    assert!(Guard::authorize(
        &policy,
        &person("ana", "acme", &["north"]),
        &orders(),
        Action::Read
    )
    .is_some());
    assert!(
        Guard::authorize(
            &policy,
            &person("mal", "acme", &["nobody"]),
            &orders(),
            Action::Read
        )
        .is_none(),
        "a principal with no grant must not be able to obtain a guard"
    );
    assert!(
        Guard::authorize(
            &policy,
            &person("mal", "other", &["north"]),
            &orders(),
            Action::Read
        )
        .is_none(),
        "the same role in another tenant must not obtain a guard"
    );
    assert!(
        Guard::authorize(
            &policy,
            &person("ana", "acme", &["north"]),
            &orders(),
            Action::Delete
        )
        .is_none(),
        "a read grant must not yield a guard for a delete"
    );
}

#[tokio::test]
async fn a_secured_table_cannot_be_built_without_a_guard() {
    // Not a test of behaviour but of the API's shape: there is exactly one constructor and
    // it takes a Guard by value. The failure this prevents is not a wrong policy, it is a
    // new code path that never consulted one.
    let denied = Guard::authorize(
        &north_only(),
        &person("mal", "acme", &["nobody"]),
        &orders(),
        Action::Read,
    );
    assert!(denied.is_none());
    // With `denied` being None there is no value to pass, and no other way to build a
    // SecuredTable. That is the guarantee.
}

#[test]
fn a_guards_storage_prefix_comes_from_the_tenant_not_the_caller() {
    // A caller supplying its own prefix could supply somebody else's.
    let guard = Guard::authorize(
        &north_only(),
        &person("ana", "acme", &["north"]),
        &orders(),
        Action::Read,
    )
    .expect("permitted");
    assert_eq!(
        guard.storage_prefix(),
        sankhya_authz::principal::storage_prefix(&tenant("acme"))
    );
}

// --- enforcement in the plan ----------------------------------------------

#[tokio::test]
async fn the_security_predicate_reaches_the_final_physical_plan() {
    // Pushing a filter down is a request, not a guarantee. A provider may decline it, an
    // optimizer may rewrite it, a rule added next year may drop it — and each turns a
    // secured table into an unsecured one silently.
    let (context, table) = secured(&north_only(), &person("ana", "acme", &["north"]))
        .await
        .expect("permitted");
    context
        .register_table("orders", table)
        .expect("registering");

    let plan = context
        .sql("SELECT id FROM orders")
        .await
        .expect("planning")
        .create_physical_plan()
        .await
        .expect("a physical plan");

    assert!(
        assert_filter_present(&plan, "north"),
        "the security predicate is not in the final physical plan:\n{}",
        datafusion::physical_plan::displayable(plan.as_ref()).indent(true)
    );
}

#[tokio::test]
async fn a_restricted_principal_sees_only_their_rows() {
    let (context, table) = secured(&north_only(), &person("ana", "acme", &["north"]))
        .await
        .expect("permitted");
    context
        .register_table("orders", table)
        .expect("registering");

    let rows = context
        .sql("SELECT id, region FROM orders ORDER BY id")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let text = pretty(&rows);
    assert!(text.contains("north"), "{text}");
    assert!(
        !text.contains("south"),
        "rows outside the policy predicate reached the result:\n{text}"
    );
    let total: usize = rows.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 2);
}

#[tokio::test]
async fn a_principal_cannot_widen_their_own_filter_with_a_predicate_of_their_own() {
    // The obvious attack: ask for the rows you are not allowed to see. The security
    // predicate is conjoined, so the query's own filter can only ever narrow further.
    let (context, table) = secured(&north_only(), &person("ana", "acme", &["north"]))
        .await
        .expect("permitted");
    context
        .register_table("orders", table)
        .expect("registering");

    let rows = context
        .sql("SELECT id FROM orders WHERE region = 'south' OR 1 = 1")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let total: usize = rows.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(
        total, 2,
        "a tautology in the query must not widen what the policy permits"
    );
}

#[tokio::test]
async fn a_limit_does_not_short_circuit_the_security_filter() {
    // A limit pushed past a security predicate lets the provider stop before the
    // restriction is applied, returning rows the principal may not see — or too few.
    // Against a provider that *honours* the limit, because MemTable does not, and a test
    // whose subject ignores the thing under test proves nothing. The mutation audit caught
    // exactly that: pushing the limit down changed no result, because nothing acted on it.
    let context = SessionContext::new();
    let guard = Guard::authorize(
        &north_only(),
        &person("ana", "acme", &["north"]),
        &orders(),
        Action::Read,
    )
    .expect("permitted");
    let honouring: Arc<dyn TableProvider> = Arc::new(LimitHonouringTable {
        inner: orders_table(),
    });
    let table = Arc::new(
        SecuredTable::new(honouring, guard, &context.state()).expect("the predicate parses"),
    );
    context
        .register_table("orders", table)
        .expect("registering");

    let rows = context
        .sql("SELECT id, region FROM orders LIMIT 2")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let text = pretty(&rows);
    assert!(
        !text.contains("south"),
        "a limit must not let a forbidden row through:\n{text}"
    );
    let total: usize = rows.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(
        total, 2,
        "the limit must apply to the rows the principal may see, not to the rows the scan \
         happened to read first. Pushed below the filter, this returns nothing:\n{text}"
    );
}

#[tokio::test]
async fn an_unrestricted_grant_sees_everything_so_the_test_above_means_something() {
    // The positive case, in the same file, because a security test that passes because
    // nothing is wired up is the worst kind.
    let policy = PolicySet::new().with(Rule::grant(
        tenant("acme"),
        Role::new("auditor"),
        orders(),
        Action::Read,
    ));
    let (context, table) = secured(&policy, &person("ana", "acme", &["auditor"]))
        .await
        .expect("permitted");
    assert!(
        table.security_filter().is_none(),
        "an unrestricted grant imposes no predicate"
    );
    context
        .register_table("orders", table)
        .expect("registering");

    let rows = context
        .sql("SELECT id FROM orders")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");
    let total: usize = rows.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 4, "all four rows");
}

// --- policy predicates that should not load -------------------------------

#[tokio::test]
async fn a_policy_predicate_naming_a_column_that_does_not_exist_is_refused_at_open() {
    // Such a predicate matches nothing, so the principal sees no rows — which looks exactly
    // like a working restriction and is in fact a broken one.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("north"), orders(), Action::Read)
            .where_rows("regionn = 'north'"),
    );
    let context = SessionContext::new();
    let guard = Guard::authorize(
        &policy,
        &person("ana", "acme", &["north"]),
        &orders(),
        Action::Read,
    )
    .expect("the rule grants");

    let outcome = SecuredTable::new(orders_table(), guard, &context.state());
    let Err(error) = outcome else {
        panic!("a predicate naming a column that does not exist must be refused");
    };
    assert!(
        error
            .to_string()
            .contains("look like a working restriction"),
        "{error}"
    );
}

#[tokio::test]
async fn a_policy_predicate_may_not_reference_another_table() {
    // A predicate that could run a subquery would be a policy reading data the policy has
    // not itself authorised.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("north"), orders(), Action::Read)
            .where_rows("id IN (SELECT id FROM secrets)"),
    );
    let context = SessionContext::new();
    let guard = Guard::authorize(
        &policy,
        &person("ana", "acme", &["north"]),
        &orders(),
        Action::Read,
    )
    .expect("the rule grants");

    assert!(
        SecuredTable::new(orders_table(), guard, &context.state()).is_err(),
        "a policy predicate must not be able to reach another table"
    );
}

#[tokio::test]
async fn a_policy_predicate_that_is_not_sql_is_refused() {
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("north"), orders(), Action::Read)
            .where_rows("region ==== "),
    );
    let context = SessionContext::new();
    let guard = Guard::authorize(
        &policy,
        &person("ana", "acme", &["north"]),
        &orders(),
        Action::Read,
    )
    .expect("the rule grants");
    assert!(SecuredTable::new(orders_table(), guard, &context.state()).is_err());
}

// --- masks ----------------------------------------------------------------

#[tokio::test]
async fn the_columns_a_decision_masks_travel_with_the_table() {
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("email", Mask::Null)
            .masking("region", Mask::Partial { keep: 2 }),
    );
    let (_context, table) = secured(&policy, &person("ana", "acme", &["clerk"]))
        .await
        .expect("permitted");

    let mut masked = table.masked_columns();
    masked.sort_unstable();
    assert_eq!(masked, vec!["email", "region"]);
    assert_eq!(table.mask_for("email"), Some(&Mask::Null));
    assert_eq!(table.mask_for("id"), None);
}

fn pretty(batches: &[RecordBatch]) -> String {
    datafusion::arrow::util::pretty::pretty_format_batches(batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn the_limit_actually_reaches_the_provider_so_the_test_above_is_not_vacuous() {
    // The mutation audit reported that pushing the limit below the security filter changed
    // nothing. Before believing that is safe, check the premise: does a LIMIT reach `scan`
    // at all? If it never does, the test above proves nothing about limits.
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Recording {
        inner: Arc<dyn TableProvider>,
        seen: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl TableProvider for Recording {
        fn schema(&self) -> arrow_schema::SchemaRef {
            self.inner.schema()
        }
        fn table_type(&self) -> datafusion::logical_expr::TableType {
            self.inner.table_type()
        }
        async fn scan(
            &self,
            state: &dyn datafusion::catalog::Session,
            projection: Option<&Vec<usize>>,
            filters: &[datafusion::logical_expr::Expr],
            limit: Option<usize>,
        ) -> datafusion::common::Result<Arc<dyn datafusion::physical_plan::ExecutionPlan>> {
            self.seen.store(limit.unwrap_or(0), Ordering::SeqCst);
            self.inner.scan(state, projection, filters, limit).await
        }
    }

    let seen = Arc::new(AtomicUsize::new(usize::MAX));
    let context = SessionContext::new();
    let recording: Arc<dyn TableProvider> = Arc::new(Recording {
        inner: orders_table(),
        seen: Arc::clone(&seen),
    });
    context
        .register_table("plain", recording)
        .expect("registering");

    let _ = context
        .sql("SELECT id FROM plain LIMIT 2")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "a LIMIT must reach the provider's scan; if it does not, no test about pushing \
         limits past a security filter means anything"
    );
}

// --- the surface people forget: the error message -------------------------

#[tokio::test]
async fn an_error_about_a_forbidden_table_does_not_confirm_it_exists() {
    // The demonstration M5 is written against names five surfaces, and this is the one
    // that gets forgotten: "through an error message". A permission error naming a table
    // confirms the table exists, and a table name discloses what a business does. The
    // difference between "no such table" and "you may not read that table" is a working
    // enumeration oracle.
    let policy = north_only();
    let context = SessionContext::new();

    let unknown = Guard::authorize(
        &policy,
        &person("mal", "acme", &["north"]),
        &TableRef::new("hr", "does_not_exist"),
        Action::Read,
    );
    let forbidden = Guard::authorize(
        &policy,
        &person("mal", "acme", &["north"]),
        &TableRef::new("hr", "salaries"),
        Action::Read,
    );

    assert!(unknown.is_none());
    assert!(forbidden.is_none());
    // Both produce the same outcome — a refusal with nothing to distinguish them — so the
    // caller cannot use the difference to enumerate what exists.

    // And nothing is registered under either name, so SQL cannot tell them apart either.
    for name in ["does_not_exist", "salaries"] {
        let outcome = context.sql(&format!("SELECT * FROM {name}")).await;
        assert!(outcome.is_err(), "{name} must not resolve");
    }
    let first = context.sql("SELECT * FROM does_not_exist").await;
    let second = context.sql("SELECT * FROM salaries").await;
    let shape = |r: &datafusion::common::Result<DataFrame>| {
        r.as_ref().err().map(|e| {
            e.to_string()
                .replace("does_not_exist", "X")
                .replace("salaries", "X")
        })
    };
    assert_eq!(
        shape(&first),
        shape(&second),
        "the two errors must differ only in the name the caller already supplied"
    );
}

#[tokio::test]
async fn a_denial_reason_is_the_same_whichever_way_it_was_denied() {
    // Distinguishing "no rule mentions you" from "a rule forbids you" tells a caller
    // something about the policy set they were not granted.
    let with_deny = PolicySet::new()
        .with(Rule::grant(
            tenant("acme"),
            Role::new("north"),
            orders(),
            Action::Read,
        ))
        .with(Rule::deny(
            tenant("acme"),
            Role::new("blocked"),
            orders(),
            Action::Read,
        ));

    let by_absence = with_deny.decide(&person("x", "acme", &["nobody"]), &orders(), Action::Read);
    let by_rule = with_deny.decide(
        &person("y", "acme", &["north", "blocked"]),
        &orders(),
        Action::Read,
    );

    assert!(!by_absence.is_allowed() && !by_rule.is_allowed());
    let text = |d: &sankhya_authz::policy::Decision| match d {
        sankhya_authz::policy::Decision::Denied { reason } => reason.to_string(),
        sankhya_authz::policy::Decision::Allowed { .. } => String::new(),
    };
    assert_eq!(text(&by_absence), text(&by_rule));
}

// --- masks, applied -------------------------------------------------------
//
// `SEC-02`. Everything above this line about masks was true and inert: `masked_columns` and
// `mask_for` reported what the policy said, `scan` never read either, and every query
// returned the real value. A control that is declared, stored, hashed into the cache key,
// documented in three places and never applied is worse than an absent one, because the
// documentation is what an operator decides on.

/// Every row of one column, as text, in the order the query returned them.
async fn column(context: &SessionContext, sql: &str, name: &str) -> Vec<Option<String>> {
    let batches = context
        .sql(sql)
        .await
        .expect("the query plans")
        .collect()
        .await
        .expect("the query runs");
    let mut out = Vec::new();
    for batch in &batches {
        let index = batch.schema().index_of(name).expect("the column is there");
        let array = batch.column(index);
        for row in 0..batch.num_rows() {
            out.push(if array.is_null(row) {
                None
            } else {
                Some(
                    datafusion::common::ScalarValue::try_from_array(array, row)
                        .expect("a value")
                        .to_string(),
                )
            });
        }
    }
    out
}

/// A context with `orders` secured under `policy` for `principal`, registered by name.
async fn registered(policy: &PolicySet, principal: &Principal) -> SessionContext {
    let (context, table) = secured(policy, principal).await.expect("permitted");
    context
        .register_table("orders", table)
        .expect("the table registers");
    context
}

#[tokio::test]
async fn a_null_mask_returns_no_value_at_all() {
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("email", Mask::Null),
    );
    let context = registered(&policy, &person("ana", "acme", &["clerk"])).await;

    let emails = column(&context, "SELECT email FROM orders", "email").await;
    assert_eq!(
        emails,
        vec![None, None, None, None],
        "a masked column must not return its values"
    );
    // The reach assertion. The fixture has four non-null addresses, so an empty result or a
    // column that was never selected would satisfy the line above while proving nothing.
    let ids = column(&context, "SELECT id FROM orders", "id").await;
    assert_eq!(ids.len(), 4, "the rows are still there: {ids:?}");
    let unmasked = column(&context, "SELECT region FROM orders", "region").await;
    assert!(
        unmasked.iter().all(Option::is_some),
        "only the masked column is masked: {unmasked:?}"
    );
}

#[tokio::test]
async fn a_partial_mask_keeps_the_tail_and_hides_the_rest() {
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("email", Mask::Partial { keep: 4 }),
    );
    let context = registered(&policy, &person("ana", "acme", &["clerk"])).await;

    let emails = column(&context, "SELECT email FROM orders ORDER BY id", "email").await;
    assert_eq!(
        emails,
        vec![
            Some("*********.com".to_string()),
            Some("*********.com".to_string()),
            Some("*********.com".to_string()),
            Some("*********.com".to_string()),
        ],
        "the last four characters stay and the rest go, character by character"
    );
}

#[tokio::test]
async fn a_constant_mask_says_the_same_thing_about_every_row() {
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read).masking(
            "email",
            Mask::Constant {
                value: "redacted".to_string(),
            },
        ),
    );
    let context = registered(&policy, &person("ana", "acme", &["clerk"])).await;

    let emails = column(&context, "SELECT email FROM orders", "email").await;
    assert_eq!(emails, vec![Some("redacted".to_string()); 4]);
}

#[tokio::test]
async fn a_mask_survives_an_expression_over_the_column() {
    // The mask has to be part of what the scan *produces*, not something applied to the
    // printed output. A masked column read by an expression, an aggregate or a join must be
    // the masked value everywhere, or the mask is a display convention.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("email", Mask::Partial { keep: 4 }),
    );
    let context = registered(&policy, &person("ana", "acme", &["clerk"])).await;

    let upper = column(&context, "SELECT upper(email) AS e FROM orders", "e").await;
    assert!(
        upper
            .iter()
            .all(|value| value.as_deref() == Some("*********.COM")),
        "an expression reads the masked value, not the real one: {upper:?}"
    );
}

#[tokio::test]
async fn a_predicate_on_a_masked_column_cannot_probe_the_real_value() {
    // The channel a mask closes only if pushdown is refused. Nothing here ever prints an
    // address; the row count answers the question instead. `WHERE email = 'a@example.com'`
    // matching one row says the address is in the table just as loudly as printing it.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("email", Mask::Partial { keep: 4 }),
    );
    let context = registered(&policy, &person("ana", "acme", &["clerk"])).await;

    let hits = column(
        &context,
        "SELECT id FROM orders WHERE email = 'a@example.com'",
        "id",
    )
    .await;
    assert!(
        hits.is_empty(),
        "the predicate must be answered against the masked value, and no masked value is \
         'a@example.com': {hits:?}"
    );
    // Not vacuous: the same predicate against what the principal can actually see matches.
    let visible = column(
        &context,
        "SELECT id FROM orders WHERE email = '*********.com'",
        "id",
    )
    .await;
    assert_eq!(
        visible.len(),
        4,
        "a predicate over the masked value still works: {visible:?}"
    );
}

#[tokio::test]
async fn a_mask_the_table_cannot_carry_is_refused_when_the_table_is_opened() {
    // The same rule as a policy predicate that does not parse. A partial mask on an integer
    // cannot be applied; discovering that on the first query that selects the column is a
    // policy that is wrong for months and looks right.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("total", Mask::Partial { keep: 2 }),
    );
    let context = SessionContext::new();
    let guard = Guard::authorize(
        &policy,
        &person("ana", "acme", &["clerk"]),
        &orders(),
        Action::Read,
    )
    .expect("the rule grants");
    assert!(SecuredTable::new(orders_table(), guard, &context.state()).is_err());

    // And a mask naming a column that does not exist, for the same reason: it obscures
    // nothing and looks like a control.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("ssn", Mask::Null),
    );
    let guard = Guard::authorize(
        &policy,
        &person("ana", "acme", &["clerk"]),
        &orders(),
        Action::Read,
    )
    .expect("the rule grants");
    assert!(SecuredTable::new(orders_table(), guard, &context.state()).is_err());

    // Not vacuous: a null mask on that same integer column is fine, because null is
    // meaningful at every type.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("total", Mask::Null),
    );
    let guard = Guard::authorize(
        &policy,
        &person("ana", "acme", &["clerk"]),
        &orders(),
        Action::Read,
    )
    .expect("the rule grants");
    assert!(SecuredTable::new(orders_table(), guard, &context.state()).is_ok());
}

#[tokio::test]
async fn a_mask_and_a_row_filter_are_both_applied_and_the_filter_sees_the_real_value() {
    // The ordering that matters. A policy predicate is written about the data, so it must be
    // evaluated before the mask: masking first would compare 'north' against a row of stars
    // and match nothing, which shows no rows and looks like a working restriction.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .where_rows("region = 'north'")
            .masking("region", Mask::Constant {
                value: "elsewhere".to_string(),
            }),
    );
    let context = registered(&policy, &person("ana", "acme", &["clerk"])).await;

    let regions = column(&context, "SELECT region FROM orders", "region").await;
    assert_eq!(
        regions,
        vec![Some("elsewhere".to_string()); 2],
        "two northern rows, and neither of them says so: {regions:?}"
    );
}

/// A provider that takes filters and means it, the way a real one does.
///
/// `MemTable` declines every filter, which makes it useless for the one property that only
/// matters against a provider that accepts them: a predicate on a masked column must not be
/// evaluated below the mask. Against `MemTable` the predicate stays above the scan whatever
/// `SecuredTable` declares, so the refusal is unreachable and a test using it proves nothing
/// --- the same trap `LimitHonouringTable` above exists to avoid.
#[derive(Debug)]
struct FilterHonouringTable {
    inner: Arc<dyn TableProvider>,
}

#[async_trait::async_trait]
impl TableProvider for FilterHonouringTable {
    fn schema(&self) -> arrow_schema::SchemaRef {
        self.inner.schema()
    }

    fn table_type(&self) -> datafusion::logical_expr::TableType {
        self.inner.table_type()
    }

    /// `Exact`: whatever it is handed, it applies.
    fn supports_filters_pushdown(
        &self,
        filters: &[&datafusion::logical_expr::Expr],
    ) -> datafusion::common::Result<Vec<datafusion::logical_expr::TableProviderFilterPushDown>> {
        Ok(vec![
            datafusion::logical_expr::TableProviderFilterPushDown::Exact;
            filters.len()
        ])
    }

    async fn scan(
        &self,
        state: &dyn datafusion::catalog::Session,
        projection: Option<&Vec<usize>>,
        filters: &[datafusion::logical_expr::Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn datafusion::physical_plan::ExecutionPlan>> {
        let plan = self.inner.scan(state, projection, &[], limit).await?;
        let Some((first, rest)) = filters.split_first() else {
            return Ok(plan);
        };
        let combined = rest
            .iter()
            .fold(first.clone(), |all, one| all.and(one.clone()));
        let df_schema =
            datafusion::common::DFSchema::try_from(plan.schema().as_ref().clone())?;
        let physical = state.create_physical_expr(combined, &df_schema)?;
        Ok(Arc::new(
            datafusion::physical_plan::filter::FilterExec::try_new(physical, plan)?,
        ))
    }
}

#[tokio::test]
async fn a_provider_that_takes_filters_is_not_given_one_over_a_masked_column() {
    // The same probe as `a_predicate_on_a_masked_column_cannot_probe_the_real_value`, against
    // a provider that would actually act on the filter. This is the one that fails if the
    // refusal is removed.
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), orders(), Action::Read)
            .masking("email", Mask::Partial { keep: 4 }),
    );
    let context = SessionContext::new();
    let guard = Guard::authorize(
        &policy,
        &person("ana", "acme", &["clerk"]),
        &orders(),
        Action::Read,
    )
    .expect("the rule grants");
    let honouring: Arc<dyn TableProvider> = Arc::new(FilterHonouringTable {
        inner: orders_table(),
    });
    let table = SecuredTable::new(honouring, guard, &context.state()).expect("the mask applies");
    context
        .register_table("orders", Arc::new(table))
        .expect("the table registers");

    let probe = column(
        &context,
        "SELECT email FROM orders WHERE email = 'a@example.com'",
        "email",
    )
    .await;
    assert!(
        probe.is_empty(),
        "the provider must never be handed a predicate over a masked column: it would answer \
         it against the real address and report the answer as a row count, which is the fact \
         the mask exists to withhold: {probe:?}"
    );

    // Not vacuous, twice over. The provider does act on the filters it is given ...
    let visible = column(
        &context,
        "SELECT email FROM orders WHERE email = '*********.com'",
        "email",
    )
    .await;
    assert_eq!(visible.len(), 4, "the masked value still matches: {visible:?}");
    // ... and it prunes on a column that is *not* masked, which is what makes the refusal a
    // refusal of one filter rather than of pushdown altogether. Both columns are selected
    // because this stand-in applies its filters after the projection rather than widening it
    // the way a real provider does --- a shortcut in the fixture, not in what it is testing.
    let pruned = column(
        &context,
        "SELECT email, region FROM orders WHERE region = 'north'",
        "email",
    )
    .await;
    assert_eq!(pruned.len(), 2, "an unmasked column still prunes: {pruned:?}");
}
