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

use sankhya_api_pg::session::Caller;
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
#[path = "../src/adopt.rs"]
mod adopt;
#[path = "../src/clones.rs"]
mod clones;
#[path = "../src/cubes.rs"]
mod cubes;
#[path = "../src/aggregations.rs"]
mod aggregations;
#[path = "../src/feeds.rs"]
mod feeds;
#[path = "../src/driver.rs"]
mod driver;
#[path = "../src/snapshots.rs"]
mod snapshots;
#[path = "../src/audit.rs"]
mod audit;
#[path = "../src/flight.rs"]
mod flight;
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
        roles: Default::default(),
        credentials: Default::default(),
        listen: "127.0.0.1:0".to_string(),
        warehouse: warehouse.to_path_buf(),
        read_as_of: Lsn::new(u64::MAX),
        tenant: tenant(),
        cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
        flight_listen: None,
        maintenance: None,
        require_password,
        user_functions: false,
        metrics_detail: false,
        policy: None,
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

/// A server over the fixture warehouse, with settings the caller chose.
///
/// `server_over` builds its own settings, which is right for every test that does not care
/// about them and wrong for the credential ones, whose whole subject is a setting.
fn server_with(settings: Settings, policy: PolicySet) -> (Server, tempfile::TempDir) {
    let dir = warehouse_with_a_table();
    let mut settings = settings;
    settings.warehouse = dir.path().to_path_buf();
    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture table must open: {refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "the fixture table must read: {unreadable:?}");
    let tables = warehouse::describe(&found);
    (Server::with_tables(settings, policy, tables, servable), dir)
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

    // A password demanded and never verified is still a posture this server can be left in ---
    // `require_password` set with no credentials written down --- and it still has to look
    // wrong in a log. An operator reading "password required" opposite a capitalised
    // "NO AUTHENTICATION" concludes the first one authenticates.
    let (closed, _warehouse) = server(PolicySet::new());
    assert!(closed.describe().contains("PASSWORD UNVERIFIED"));
    assert!(!closed.describe().contains("NO AUTHENTICATION"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_with_credentials_says_so_rather_than_saying_unverified() {
    // The third posture, and the one `SEC-01` is closed by. Until credentials existed there
    // were two, and the honest word for both was that nothing was checked.
    let dir = warehouse_with_a_table();
    let mut settings = settings(true, dir.path());
    settings.credentials.insert(
        "ana".to_string(),
        sankhya_credential::Verifier::parse(&verifier_for(b"open sesame")).expect("a verifier"),
    );
    let (server, _) = server_with(settings, PolicySet::new());

    let said = server.describe();
    assert!(said.contains("password verified for 1 user(s)"), "{said}");
    assert!(!said.contains("UNVERIFIED"), "{said}");
    assert!(!said.contains("NO AUTHENTICATION"), "{said}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_password_is_refused_and_a_right_one_is_not() {
    // `SEC-01`. The entire check was that a password had been *presented* and was non-empty:
    // no credential store, no hash and no comparison anywhere in the workspace. The username
    // is self-asserted, so any client connected as any user --- including one this server has
    // never heard of --- by sending any byte string.
    let dir = warehouse_with_a_table();
    let mut settings = settings(true, dir.path());
    settings.credentials.insert(
        "ana".to_string(),
        sankhya_credential::Verifier::parse(&verifier_for(b"open sesame")).expect("a verifier"),
    );
    let (server, _) = server_with(settings, PolicySet::new());

    let as_user = |user: &str| vec![("user".to_string(), user.to_string())];

    assert!(
        server.authenticate(&as_user("ana"), Some(b"open sesame")).is_ok(),
        "the right password was refused"
    );
    assert!(
        server.authenticate(&as_user("ana"), Some(b"open sesam")).is_err(),
        "a wrong password was accepted"
    );
    assert!(
        server.authenticate(&as_user("ana"), Some(b"")).is_err(),
        "an empty password was accepted"
    );

    // A user this server has never heard of is refused, and refused **the same way**. Telling
    // "no such user" apart from "wrong password" turns the login into a directory of who
    // exists here, which is the first thing an attacker wants.
    let stranger = server
        .authenticate(&as_user("mallory"), Some(b"open sesame"))
        .expect_err("a user with no credential was accepted");
    let known = server
        .authenticate(&as_user("ana"), Some(b"wrong"))
        .expect_err("a wrong password was accepted");
    assert_eq!(
        stranger.message, known.message,
        "the refusal says whether the user exists: {} against {}",
        stranger.message, known.message
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_with_no_credentials_keeps_the_behaviour_it_had() {
    // The upgrade path, stated as a test. An operator who has configured nothing has decided
    // nothing, and a server that began refusing every connection on upgrade is a server
    // nobody upgrades --- so the empty map is the old posture, and the startup line above is
    // what says so.
    let (server, _dir) = server(PolicySet::new());
    let as_user = vec![("user".to_string(), "anyone".to_string())];
    assert!(server.authenticate(&as_user, Some(b"anything")).is_ok());
    assert!(
        server.authenticate(&as_user, Some(b"")).is_err(),
        "and an empty password is still refused, because `require_password` is set"
    );
}

/// A verifier for `password`, at a low iteration count.
///
/// Low **only here**: the shipped default is 600,000 and these run on every build, where a
/// second of derivation per assertion buys nothing. The default is asserted from the constant
/// in `sankhya-credential`'s own tests.
fn verifier_for(password: &[u8]) -> String {
    sankhya_credential::make(password, b"a fixed salt, so this is reproducible", 4096)
        .expect("a verifier")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catalogue_lists_only_tables_the_policy_permits() {
    // A schema browser is a back door to the same disclosure the policy component refuses
    // everywhere else: a table name says what a business does.
    let (permitted, _warehouse) = server_over(|tables| permissive_policy(&tenant(), tables));
    assert_eq!(permitted.visible_tables(&Caller::new(&anyone())).len(), 1);

    let (nothing_granted, _w2) = server(PolicySet::new());
    assert!(
        nothing_granted.visible_tables(&Caller::new(&anyone())).is_empty(),
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
    assert!(server(policy).0.visible_tables(&Caller::new(&anyone())).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn everything_that_happens_is_audited_and_the_chain_verifies() {
    let (server, _warehouse) = server_over(|tables| permissive_policy(&tenant(), tables));
    assert_eq!(audit::len(&server), 0);
    let empty_head = audit::head(&server);

    server.query("SELECT id FROM example", &Caller::new(&anyone())).ok();
    server.visible_tables(&Caller::new(&anyone()));
    server.query("SELECT 1", &Caller::new(&anyone())).ok();

    assert_eq!(audit::len(&server), 3, "refusals are audited too");
    assert!(audit::intact(&server), "the chain must verify");
    assert_ne!(
        audit::head(&server),
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
        .query("SELECT id FROM example WHERE national_id = '123-45-6789'", &Caller::new(&anyone()))
        .ok();

    let head = audit::head(&server);
    assert!(!head.is_empty());
    assert!(audit::intact(&server));
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
        .query("SELECT id, label FROM example ORDER BY id", &Caller::new(&anyone()))
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
        .query("SELECT label FROM example ORDER BY id", &Caller::new(&anyone()))
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
    let Err(failure) = server.query("SELECT id FROM example", &Caller::new(&anyone())) else {
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

    let result = server.query("SELECT id FROM example", &Caller::new(&anyone())).expect("valid");
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
        .query("SELECT id FROM example WHERE id > 0 OR 1 = 1", &Caller::new(&anyone()))
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
    assert!(server.query("SELECT FROM WHERE", &Caller::new(&anyone())).is_err());
    // And the next statement still works.
    assert!(server.query("SELECT id FROM example", &Caller::new(&anyone())).is_ok());
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

    let result = server.query("SHOW FEEDS", &Caller::new(&anyone())).expect("a feed command is answered");

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
    // And what it writes into, which is what `RESUME FEED` is authorized against. Declaring
    // the feed's *state* without its target is a feed the server knows is halted and cannot
    // say whose table it fills --- and that is refused rather than resumed. `SEC-04`.
    feeds::declare_targets(
        &server,
        [("orders".to_string(), TableRef::new("public", "example"))]
            .into_iter()
            .collect(),
    );
    server.feeds().halted("orders", "a reason", 1);
    assert!(!server.feeds().should_run("orders"));

    server.query("RESUME FEED orders", &Caller::new(&anyone())).expect("resuming a declared feed");
    assert!(server.feeds().should_run("orders"), "it runs again on the next tick");

    // Named rather than reported as success. An operator who mistypes and is told it resumed
    // will go away believing it did.
    let refused = server
        .query("RESUME FEED odrers", &Caller::new(&anyone()))
        .expect_err("no feed by that name");
    assert!(refused.message.contains("odrers"), "{}", refused.message);
    assert!(refused.message.contains("SHOW FEEDS"), "{}", refused.message);

    // A feed the server knows is halted and whose target it cannot name is refused with the
    // same sentence. Not knowing what a feed writes into is not permission to start it, and
    // the refusal must not distinguish that case from an unknown name --- otherwise it says
    // which feeds exist.
    server.feeds().declare("sessions");
    server.feeds().halted("sessions", "a reason", 1);
    let untargeted = server
        .query("RESUME FEED sessions", &Caller::new(&anyone()))
        .expect_err("a feed whose target is unknown");
    assert!(untargeted.message.contains("sessions"), "{}", untargeted.message);
    assert!(
        untargeted.message.contains("SHOW FEEDS"),
        "the same sentence an unknown name gets: {}",
        untargeted.message
    );
    assert!(
        !server.feeds().should_run("sessions"),
        "it must not have been resumed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_statement_that_merely_begins_like_a_feed_command_reaches_the_engine() {
    // The important half. `SHOW server_version_num` is what a catalogue-browsing client sends
    // on connection, and a feed parser that took it would break every such client while
    // reporting a refusal about feeds.
    let (server, _dir) = server_over(|tables| permissive_policy(&tenant(), tables));

    let version = server.query("SHOW server_version_num", &Caller::new(&anyone()));
    let refused = version.err().map(|failure| failure.message).unwrap_or_default();
    assert!(
        !refused.contains("feed"),
        "whatever answers this, it is not the feed parser: {refused}"
    );
}

/// The caller a test means when it does not care who is asking.
///
/// Its own helper rather than an inline literal at forty call sites: when a test *does* care,
/// it should be visibly different from one that does not.
#[allow(dead_code)]
fn anyone() -> Vec<(String, String)> {
    vec![("user".to_string(), "quickstart".to_string())]
}

#[tokio::test(flavor = "multi_thread")]
async fn the_user_a_connection_authenticated_as_reaches_authorization_and_audit() {
    // `FR-SEC-02` asks that a principal be carried unchanged through planning, execution and
    // audit. It could not be: `Handler::query` and `Handler::visible_tables` had **no
    // parameter for one**, so `authenticate` read the user, refused an empty one on the
    // grounds that an unattributable connection cannot be audited --- and then discarded it.
    //
    // Every statement was authorized as `principal("query")`, a literal, and every audit entry
    // attributed to it. An adversarial review found this on 2026-09-01, and it is the first
    // thing federated identity would have needed.
    let (server, _warehouse) = server_over(|tables| {
        permissive_policy(&sankhya_authz::principal::TenantId::from_uuid(uuid::Uuid::from_u128(1)), tables)
    });

    // The **record**, not the entry count. A chain that grew by one either way proves only
    // that something was recorded; a record naming the subject proves *who* was.
    //
    // This used to compare the audit head across two fresh servers, on the grounds that the
    // chain is deterministic so a difference in the head is a difference in the subject. That
    // reasoning held only while every record's timestamp was `*clock += 1` --- a counter
    // standing in for a clock, restarting at 1 on every boot. `OPS-04` replaced it with wall
    // clock, so two runs of the same statement no longer produce the same digest, and they
    // should not: the time a thing happened is part of what an audit attests to.
    //
    // What was given up is byte-identical replay across processes. What is asserted instead is
    // the property the test was always about.
    let ana = vec![("user".to_string(), "ana".to_string())];
    let bo = vec![("user".to_string(), "bo".to_string())];

    server.query("SELECT 1", &Caller::new(&ana)).ok();
    server.query("SELECT 1", &Caller::new(&bo)).ok();

    let held = server.audit.lock();
    let records = held.0.records();
    let subjects: Vec<&str> = records.iter().map(|record| record.subject.as_str()).collect();
    assert!(
        subjects.contains(&"ana") && subjects.contains(&"bo"),
        "each statement is attributed to the user that ran it: {subjects:?}"
    );

    // And the two records differ in more than their subject field: the subject is hashed into
    // the digest, which is what makes the chain evidence about *who* rather than a list with a
    // name column somebody could edit.
    let digests: std::collections::BTreeSet<String> =
        records.iter().map(|record| record.digest.to_string()).collect();
    assert_eq!(
        digests.len(),
        records.len(),
        "every record has its own digest: {digests:?}"
    );
    assert!(
        held.0.verify().is_ok(),
        "and the chain they form still verifies"
    );
}

/// Make `path` unlistable, and refuse to continue if that did not work.
///
/// It does not work as root, and a test that quietly passed there would prove nothing on a
/// container's default user --- so the precondition is asserted rather than assumed.
fn make_unreadable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path).expect("it exists").permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(path, permissions).expect("setting permissions");
    assert!(
        std::fs::read_dir(path).is_err(),
        "{} is still readable after chmod 000 --- this test cannot run as root",
        path.display()
    );
}

/// Put it back, so the temporary directory can be cleaned up.
fn make_readable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::metadata(path) else { return };
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).ok();
}

#[test]
fn a_name_in_a_warehouse_nobody_can_read_is_unknown_rather_than_absent() {
    // `OPS-12`. `resolve` answered `Absent` for a warehouse it could not list, and `Absent`
    // is a claim: the table is not there. The two are distinguished because one of them
    // decides a deletion --- a snapshot pins files only if its table resolves, so "not
    // there" on an unmounted export drops the pin and the sweeper reclaims what it was
    // protecting, under a reader.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(warehouse.join("sales")).expect("a schema");

    // A warehouse that is not there at all still answers "no such table", because a
    // warehouse is created on first use and a fresh install genuinely holds nothing.
    assert!(
        matches!(
            warehouse::resolve(&dir.path().join("never-created"), "orders"),
            warehouse::Resolved::Absent
        ),
        "a warehouse that does not exist holds no table of that name"
    );

    make_unreadable(&warehouse);
    let answer = warehouse::resolve(&warehouse, "orders");
    make_readable(&warehouse);
    assert!(
        matches!(answer, warehouse::Resolved::Unreadable(_)),
        "a warehouse that exists and cannot be listed must not answer that the table is absent"
    );
}

#[test]
fn a_feed_directory_nobody_can_read_is_not_a_deployment_with_no_feeds() {
    // The same swallow. A server that starts cleanly and ingests nothing looks, from every
    // table it should have been filling, exactly like a source that stopped producing.
    let dir = tempfile::tempdir().expect("a directory");
    let configuration = dir.path().to_path_buf();

    // Not there is still silent: most deployments declare no feeds.
    let (declared, complaints) = feeds::load(&configuration);
    assert!(declared.is_empty() && complaints.is_empty(), "no feeds directory is not a complaint");

    let feeds_dir = configuration.join("feeds");
    std::fs::create_dir_all(&feeds_dir).expect("a feeds directory");
    make_unreadable(&feeds_dir);
    let (declared, complaints) = feeds::load(&configuration);
    make_readable(&feeds_dir);

    assert!(declared.is_empty(), "nothing could be read, so nothing loaded");
    assert_eq!(complaints.len(), 1, "and that must be said: {complaints:?}");
    assert!(
        complaints[0].contains("feeds"),
        "the complaint must name the directory: {complaints:?}"
    );
}

#[test]
fn an_aggregation_directory_nobody_can_read_is_not_a_warehouse_with_none() {
    // The symptom of the old behaviour was a query that worked yesterday failing to plan
    // with "unknown function" --- a message that names the caller's SQL and not the
    // directory, so the person who reads it has no way to reach the cause.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).expect("a warehouse");

    let (found, complaints) = aggregations::stored(&warehouse);
    assert!(found.is_empty() && complaints.is_empty(), "no directory is not a complaint");

    let store = warehouse.join("_aggregations");
    std::fs::create_dir_all(&store).expect("an aggregation store");
    make_unreadable(&store);
    let (found, complaints) = aggregations::stored(&warehouse);
    make_readable(&store);

    assert!(found.is_empty(), "nothing could be read, so nothing loaded");
    assert_eq!(complaints.len(), 1, "and that must be said: {complaints:?}");
    assert!(
        complaints[0].contains("_aggregations"),
        "the complaint must name the directory: {complaints:?}"
    );
}

#[test]
fn every_statement_shares_one_memory_pool() {
    // `SANKHYA_QUERY_MEMORY_BYTES` names what a server's queries may use **between them** ---
    // its documentation and its help text both say so. The first version of `bounded_session`
    // built a fresh `RuntimeEnv`, and therefore a fresh `FairSpillPool`, on every call: each
    // statement got its own gibibyte, ten concurrent statements got ten, and the machine died
    // exactly as it had before while the setting read as solved.
    //
    // A pool that is not shared is not a bound. Fairness is the reason: a fair pool divides
    // what there is between the consumers actually asking, so the expensive query fails
    // itself rather than starving the others --- and with a pool each there is nothing to be
    // fair about.
    let policy = PolicySet::default();
    let principal = sankhya_authz::principal::Principal::authenticated(
        "ana",
        TenantId::from_uuid(uuid::Uuid::from_u128(1)),
        [Role::new("reader")],
        sankhya_authz::principal::Authentication::Password,
    )
    .expect("a principal");

    // No tables: the property is about the runtime the session is built on, and registering
    // tables would only add filesystem work to a question that does not involve any.
    let (first, _) = execute::session_for(&principal, &policy, &[]).expect("a session");
    let (second, _) = execute::session_for(&principal, &policy, &[]).expect("a second session");

    assert!(
        std::sync::Arc::ptr_eq(&first.runtime_env(), &second.runtime_env()),
        "two statements must run on one runtime, or the memory bound is per statement"
    );
    // And the pool inside it is the same object, which is the part that actually bounds
    // anything: a shared runtime holding two pools would satisfy the line above and none of
    // the reasoning behind it.
    assert!(
        std::sync::Arc::ptr_eq(
            &first.runtime_env().memory_pool,
            &second.runtime_env().memory_pool
        ),
        "two statements must reserve from one pool"
    );
}

#[test]
fn the_freshness_probe_goes_through_the_cache_built_for_it() {
    // `OPS-22`. The probe that asks "has this table moved?" ran `live_files` free-standing
    // --- a replay from version zero --- for every table, on every statement, while holding
    // the `LogCache` built to make it incremental. At a thousand commits a table that is a
    // thousand file reads per table per statement to discover the ordinary case: that
    // nothing has changed.
    //
    // A first version of this test asserted that `LogCache` caches, which is a property of
    // `LogCache`, was true before the fix and after it, and passed against the defect. The
    // mutation catalogue caught that: the entry survived. What separates the two behaviours
    // is whether `refresh` *uses* the cache it is handed, and that is what this asks.
    use sankhya_table_delta::{LogCache, Outcome};

    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_table(&warehouse, "sales", "orders");
    let table = warehouse.join("sales").join("orders");

    let target = sankhya_types::Lsn::new(u64::MAX);
    let (found, _) = warehouse::discover(&warehouse);
    assert_eq!(found.len(), 1, "the fixture must hold exactly one table");

    // The cache `refresh` is given, and the only thing that could warm it.
    let cache = LogCache::new();
    let (mut tables, _) = warehouse::servable(&found, target, &cache);
    assert_eq!(tables.len(), 1, "the table must be servable");

    // A second cache, so that whatever `servable` did above cannot account for the result.
    let probe = LogCache::new();
    let _ = warehouse::refresh(&mut tables, target, &probe);

    let (_, after) = probe.live_files(&table).expect("a readable log");
    assert!(
        !matches!(after, Outcome::Cold),
        "the freshness probe replayed the log without the cache it was handed, so the next \
         reader replays it from version zero all over again: {after:?}"
    );
}

/// A verifier for the password `hunter2`, from `sankhya-server hash-password`.
const HUNTER2: &str = "pbkdf2-sha256$600000$vJrYUsPBmwaSHbzUFQCt8laBJO6MfnsJoH31GS6QVLU=$Ge1+mh9xkPgBMqBuT8In/SK2ofRxO1A5FgCwkobGBuk=";

/// Metadata carrying a user, and optionally a password.
fn presenting(user: &str, password: Option<&str>) -> tonic::metadata::MetadataMap {
    let mut metadata = tonic::metadata::MetadataMap::new();
    metadata.insert(flight::USER_KEY, user.parse().expect("a header value"));
    if let Some(password) = password {
        metadata.insert(flight::PASSWORD_KEY, password.parse().expect("a header value"));
    }
    metadata
}

/// A server that requires a password and has one user who has one.
fn guarding() -> (Server, tempfile::TempDir) {
    let mut settings = settings(true, std::path::Path::new("."));
    settings.credentials.insert(
        "ana".to_string(),
        sankhya_credential::Verifier::parse(HUNTER2).expect("a verifier"),
    );
    server_with(settings, permissive_policy(&tenant(), &[]))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_columnar_door_refuses_a_caller_who_presents_no_credential() {
    // The columnar door read a `sankhya-user` header, checked it was non-empty, and served
    // that user's session --- with **no credential of any kind**. The wire door refuses when
    // `require_password` is set and no password arrives, and then verifies what did arrive.
    // This door did neither, and it is enabled by default on `127.0.0.1:5434`.
    //
    // Worse than an open door: `Server::principal` stamps the record with
    // `Authentication::Password` when passwords are required, so the audit would have said
    // *authenticated by password* about a caller who presented none.
    use sankhya_api_flight::Queries;

    let (server, _dir) = guarding();
    let flying = flight::Flying::new(std::sync::Arc::new(server));

    assert!(
        flying.caller_of(&presenting("ana", None)).is_err(),
        "the columnar door served a caller who named a user and presented no password"
    );
    assert!(
        flying.caller_of(&presenting("ana", Some("wrong"))).is_err(),
        "the columnar door served a caller whose password does not verify"
    );
    // And the other half, so the two above are not passing because it refuses everybody.
    assert!(
        flying.caller_of(&presenting("ana", Some("hunter2"))).is_ok(),
        "the columnar door must serve a caller who presents the right password"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_columnar_door_still_refuses_an_unnamed_caller() {
    // The refusal that was already there, kept: an unattributable request cannot be audited.
    use sankhya_api_flight::Queries;

    let (server, _dir) = guarding();
    let flying = flight::Flying::new(std::sync::Arc::new(server));
    assert!(flying.caller_of(&tonic::metadata::MetadataMap::new()).is_err());
}
