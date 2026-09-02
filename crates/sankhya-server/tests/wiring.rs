//! The composition root, driven the way a client drives it.
//!
//! The wiring is a library rather than a `main` precisely so this test can exist. A binary
//! is hard to test and easy to let drift; the thing that decides which policy is in force,
//! which principal a connection becomes and what the audit records is the thing most worth
//! testing, so it lives where a test can reach it.

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

use sankhya_api_pg::session::Handler;
use sankhya_authz::policy::{Action, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Role, TenantId};

// The wiring module is part of the binary, so the test builds it directly. That is the
// cost of a composition root living in a binary crate, and it is cheaper than the
// alternative of never testing it.
#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/warehouse.rs"]
mod warehouse;
#[path = "../src/clones.rs"]
mod clones;
#[path = "../src/wiring.rs"]
mod wiring;

use wiring::{permissive_policy, Server, Settings};

use arrow_schema::{DataType, Field, Schema};
use sankhya_table_delta::{commit, create, Action as DeltaAction, AddFile, Metadata};
use sankhya_types::Lsn;
use std::sync::Arc;

/// The Arrow schema of the table the fixtures write.
fn table_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, true),
    ]))
}

/// Write a real Delta table to disk: a log, a metadata action, and one Parquet file.
///
/// Real rather than in-memory, because the whole point of the read path is that it plans
/// from a table log and prunes by recorded statistics — and none of that happens against a
/// `MemTable`. A test that proves the query path over an in-memory table proves the query
/// path over an in-memory table.
fn write_table(warehouse: &std::path::Path, schema_name: &str, table_name: &str) {
    use arrow_array::{Int64Array, RecordBatch, StringArray};
    use sankhya_table::{write_parquet, WriterConfig};

    let root = warehouse.join(schema_name).join(table_name);
    std::fs::create_dir_all(&root).expect("creating the table directory");

    let delta_schema = sankhya_table_delta::schema_string(&table_schema()).expect("representable");
    commit(
        &root,
        0,
        &create(Metadata::new(table_name, delta_schema, 0)),
    )
    .expect("creating the table");

    let batch = RecordBatch::try_new(
        table_schema(),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            Arc::new(StringArray::from(vec![Some("north"), None, Some("south")])),
        ],
    )
    .expect("a valid batch");

    let report = write_parquet(
        &root,
        "part-0000.parquet",
        &batch,
        Lsn::new(3),
        WriterConfig::default(),
    )
    .expect("writing the file");

    commit(
        &root,
        1,
        &[DeltaAction::Add(AddFile::with_rows(
            "part-0000.parquet",
            report.bytes,
            0,
            3,
        ))],
    )
    .expect("publishing the file");
}

/// A warehouse holding one real table.
fn warehouse_with_a_table() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_table(dir.path(), "public", "example");
    dir
}

fn tenant() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::from_u128(1))
}

fn settings(require_password: bool, warehouse: &std::path::Path) -> Settings {
    Settings {
        listen: "127.0.0.1:0".to_string(),
        warehouse: warehouse.to_path_buf(),
        read_as_of: Lsn::new(u64::MAX),
        tenant: tenant(),
        cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
        flight_listen: None,
        maintenance: None,
        require_password,
        metrics_listen: None,
        transport_security: None,
    }
}

/// A server serving one real Delta table from a temporary warehouse.
///
/// The `TempDir` is returned alongside because dropping it deletes the warehouse, and a
/// server whose files vanish mid-test fails in a way that looks like a bug in the server.
fn server_over(
    policy_for: impl Fn(&[sankhya_api_pg::catalog::CatalogTable]) -> PolicySet,
) -> (Server, tempfile::TempDir) {
    let dir = warehouse_with_a_table();
    let (found, refused) = warehouse::discover(dir.path());
    assert!(
        refused.is_empty(),
        "the fixture table must open: {refused:?}"
    );
    assert_eq!(found.len(), 1, "one table was written");

    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(
        unreadable.is_empty(),
        "the fixture table must read: {unreadable:?}"
    );

    let tables = warehouse::describe(&found);
    let policy = policy_for(&tables);
    let server = Server::with_tables(settings(true, dir.path()), policy, tables, servable);
    (server, dir)
}

fn server(policy: PolicySet) -> (Server, tempfile::TempDir) {
    server_over(|_| policy.clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_with_no_user_is_refused() {
    // An unattributable connection cannot be audited, and an audit that cannot name who
    // acted is not an audit. A default user would make that hole silent.
    let (server, _warehouse) = server(PolicySet::new());
    let Err(failure) = server.authenticate(&[], Some(b"anything")) else {
        panic!("a connection with no user must be refused");
    };
    assert!(failure.message.contains("cannot be audited"));
    assert_eq!(failure.sqlstate, "28000");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_is_required_when_configured_and_not_otherwise() {
    let (with, _warehouse) = server(PolicySet::new());
    assert!(with.requires_password(&[]));
    let user = vec![("user".to_string(), "ana".to_string())];
    assert!(with.authenticate(&user, None).is_err());
    assert!(with.authenticate(&user, Some(b"anything")).is_ok());

    let empty = tempfile::tempdir().expect("a temporary directory");
    let without = Server::with_tables(
        settings(false, empty.path()),
        PolicySet::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(!without.requires_password(&[]));
    assert!(without.authenticate(&user, None).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_insecure_configuration_looks_wrong_in_the_startup_line() {
    // An operator who has accidentally started an open server should see it, not have to
    // go and check.
    let empty = tempfile::tempdir().expect("a temporary directory");
    let open = Server::with_tables(
        settings(false, empty.path()),
        PolicySet::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(open.describe().contains("NO AUTHENTICATION"));

    let (closed, _warehouse) = server(PolicySet::new());
    assert!(closed.describe().contains("password required"));
    assert!(!closed.describe().contains("NO AUTHENTICATION"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catalogue_lists_only_tables_the_policy_permits() {
    // A schema browser is a back door to the same disclosure the policy component refuses
    // everywhere else: a table name says what a business does.
    let (permitted, _warehouse) = server_over(|tables| permissive_policy(&tenant(), tables));
    assert_eq!(permitted.visible_tables().len(), 1);

    let (nothing_granted, _w2) = server(PolicySet::new());
    assert!(
        nothing_granted.visible_tables().is_empty(),
        "a table nobody was granted must not appear in a schema listing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_granted_to_another_tenant_is_not_listed() {
    let other = TenantId::from_uuid(uuid::Uuid::from_u128(2));
    let policy = PolicySet::new().with(Rule::grant(
        other,
        Role::new("reader"),
        TableRef::new("public", "example"),
        Action::Read,
    ));
    assert!(server(policy).0.visible_tables().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn everything_that_happens_is_audited_and_the_chain_verifies() {
    let (server, _warehouse) = server_over(|tables| permissive_policy(&tenant(), tables));
    assert_eq!(server.audit_len(), 0);
    let empty_head = server.audit_head();

    server.query("SELECT id FROM example").ok();
    server.visible_tables();
    server.query("SELECT 1").ok();

    assert_eq!(server.audit_len(), 3, "refusals are audited too");
    assert!(server.audit_intact(), "the chain must verify");
    assert_ne!(
        server.audit_head(),
        empty_head,
        "the head moves, which is what gets mirrored somewhere append-only"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_audit_records_the_shape_of_a_statement_and_not_its_values() {
    // A statement can contain the values a query was filtering on. Copying those verbatim
    // into a durable log turns the audit into a second place the data lives, with different
    // retention and different access control from the table it came from.
    let (server, _warehouse) = server_over(|tables| permissive_policy(&tenant(), tables));
    server
        .query("SELECT id FROM example WHERE national_id = '123-45-6789'")
        .ok();

    let head = server.audit_head();
    assert!(!head.is_empty());
    assert!(server.audit_intact());
    // The sensitive literal must not be reachable through the audit surface at all.
    assert!(
        !format!("{server:?}").contains("123-45-6789"),
        "a filtered value reached the audit record"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tenant_has_a_quota_in_force() {
    // Not a placeholder: a tenant with no quota is refused by the governor, so a server
    // that forgot to set one would refuse every query with a confusing message.
    let (server, _warehouse) = server(PolicySet::new());
    assert!(
        server.quota().is_some(),
        "a tenant with no quota is refused every query"
    );
}

// --- statements actually execute now --------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_statement_executes_and_returns_rows() {
    // The whole point of the integration: a statement arrives, is authorised, planned
    // against a policy-wrapped provider, executed, and rendered back for the wire.
    let (server, _warehouse) = server_over(|tables| permissive_policy(&tenant(), tables));
    let result = server
        .query("SELECT id, label FROM example ORDER BY id")
        .expect("the table is granted and the query is valid");

    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.tag, "SELECT 3");
    assert_eq!(
        result.fields.first().map(|f| f.type_oid),
        Some(20),
        "int8 must be reported as int8, or the client renders it as opaque text that will \
         not sort or aggregate"
    );
    assert_eq!(
        result.rows.first().and_then(|r| r.first().cloned()),
        Some(Some("1".to_string()))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_null_stays_null_all_the_way_out() {
    // Rendering it as an empty string here would be the same wrong answer one layer
    // earlier, and one nobody could see.
    let (server, _warehouse) = server_over(|tables| permissive_policy(&tenant(), tables));
    let result = server
        .query("SELECT label FROM example ORDER BY id")
        .expect("valid");
    assert_eq!(
        result.rows.get(1).and_then(|r| r.first().cloned()),
        Some(None),
        "the middle row's label is null"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_the_principal_may_not_read_does_not_resolve() {
    // Not registered at all, so the query fails to resolve — indistinguishable from naming
    // a table that does not exist, which is the right answer rather than an accident.
    // Saying "you may not read that" would confirm it exists.
    let (server, _warehouse) = server(PolicySet::new());
    let Err(failure) = server.query("SELECT id FROM example") else {
        panic!("a table nobody granted must not be queryable");
    };
    assert!(
        !failure.message.contains("permission") && !failure.message.contains("not permitted"),
        "the refusal must not confirm the table exists: {}",
        failure.message
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_policy_row_filter_is_enforced_through_the_front_door() {
    // The end-to-end version of what the catalog crate tests in isolation. Until this path
    // existed the enforcement was provable in a unit test and unreachable through the
    // server — and an unreachable enforcement point is one nobody has confirmed is
    // actually on the path.
    let policy = PolicySet::new().with(
        Rule::grant(
            tenant(),
            Role::new("reader"),
            TableRef::new("public", "example"),
            Action::Read,
        )
        .where_rows("id = 1"),
    );
    let (server, _warehouse) = server_over(move |_| policy.clone());

    let result = server.query("SELECT id FROM example").expect("valid");
    assert_eq!(
        result.rows.len(),
        1,
        "the policy predicate must reach the scan, not merely exist"
    );
    assert_eq!(
        result.rows.first().and_then(|r| r.first().cloned()),
        Some(Some("1".to_string()))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_query_cannot_widen_its_own_policy_filter() {
    // The obvious attack, now reachable over the protocol rather than only in a unit test.
    let policy = PolicySet::new().with(
        Rule::grant(
            tenant(),
            Role::new("reader"),
            TableRef::new("public", "example"),
            Action::Read,
        )
        .where_rows("id = 1"),
    );
    let (server, _warehouse) = server_over(move |_| policy.clone());

    let result = server
        .query("SELECT id FROM example WHERE id > 0 OR 1 = 1")
        .expect("valid");
    assert_eq!(
        result.rows.len(),
        1,
        "a tautology must not widen the policy"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_syntactically_invalid_statement_is_refused_without_taking_the_server_down() {
    let (server, _warehouse) = server_over(|tables| permissive_policy(&tenant(), tables));
    assert!(server.query("SELECT FROM WHERE").is_err());
    // And the next statement still works.
    assert!(server.query("SELECT id FROM example").is_ok());
}

/// A feed's standing, over the wire the operator uses.
///
/// Written against the server rather than the registry because the registry's own tests
/// already prove the state machine. What these check is that the two statements reach it at
/// all --- which is the half that a client depends on and a unit test cannot see.
#[tokio::test(flavor = "multi_thread")]
async fn showing_feeds_reports_every_declared_feed_and_its_state() {
    let (server, _dir) = server_over(|tables| permissive_policy(&tenant(), tables));
    let feeds = server.feeds();
    feeds.declare("orders");
    feeds.declare("sessions");
    feeds.halted("sessions", "not one of this source's 40 records fitted", 1_756_000_000_000_000);

    let result = server.query("SHOW FEEDS").expect("a feed command is answered");

    assert_eq!(result.rows.len(), 2);
    let names: Vec<&str> = result
        .rows
        .iter()
        .filter_map(|row| row.first().and_then(Option::as_deref))
        .collect();
    assert_eq!(names, vec!["orders", "sessions"]);

    // The reason is a column rather than something to grep a log for, which is the whole
    // point: an operator arriving an hour later has the same question as one arriving
    // immediately.
    let sessions = result.rows.iter().find(|row| row[0].as_deref() == Some("sessions"));
    let sessions = sessions.expect("the halted feed");
    assert_eq!(sessions[1].as_deref(), Some("halted"));
    assert!(
        sessions[3].as_deref().is_some_and(|why| why.contains("40 records")),
        "the reason travels with the state: {sessions:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resuming_a_feed_sets_it_running_and_resuming_a_typo_is_refused() {
    let (server, _dir) = server_over(|tables| permissive_policy(&tenant(), tables));
    server.feeds().declare("orders");
    server.feeds().halted("orders", "a reason", 1);
    assert!(!server.feeds().should_run("orders"));

    server.query("RESUME FEED orders").expect("resuming a declared feed");
    assert!(server.feeds().should_run("orders"), "it runs again on the next tick");

    // Named rather than reported as success. An operator who mistypes and is told it resumed
    // will go away believing it did.
    let refused = server
        .query("RESUME FEED odrers")
        .expect_err("no feed by that name");
    assert!(refused.message.contains("odrers"), "{}", refused.message);
    assert!(refused.message.contains("SHOW FEEDS"), "{}", refused.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_statement_that_merely_begins_like_a_feed_command_reaches_the_engine() {
    // The important half. `SHOW server_version_num` is what a catalogue-browsing client sends
    // on connection, and a feed parser that took it would break every such client while
    // reporting a refusal about feeds.
    let (server, _dir) = server_over(|tables| permissive_policy(&tenant(), tables));

    let version = server.query("SHOW server_version_num");
    let refused = version.err().map(|failure| failure.message).unwrap_or_default();
    assert!(
        !refused.contains("feed"),
        "whatever answers this, it is not the feed parser: {refused}"
    );
}
