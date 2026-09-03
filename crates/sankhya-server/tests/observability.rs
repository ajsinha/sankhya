//! What the server exports, driven through the real server and the real endpoint.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_api_pg::session::Caller;
use sankhya_api_pg::session::Handler;
use sankhya_authz::policy::PolicySet;
use sankhya_authz::principal::TenantId;

#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/scrape.rs"]
mod scrape;
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
#[path = "../src/wiring.rs"]
mod wiring;

use arrow_schema::{DataType, Field, Schema};
use sankhya_metrics::catalogue::{
    ALL, AUDIT_RECORDS_TOTAL, CONNECTIONS_ACTIVE, QUERIES_TOTAL, QUERY_DURATION_SECONDS,
    ROWS_RETURNED_TOTAL, TABLE_LIVE_FILES,
};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiring::{Server, Settings};

fn table_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, true),
    ]))
}

/// A real Delta table: a log, a metadata action, and one Parquet file.
fn write_table(warehouse: &std::path::Path, schema_name: &str, table_name: &str) {
    use arrow_array::{Int64Array, RecordBatch, StringArray};

    let root = warehouse.join(schema_name).join(table_name);
    std::fs::create_dir_all(&root).expect("creating the table directory");
    // Through the writer that owns publishing, so this fixture cannot drift from the
    // layout the product actually produces.
    let publication = Publication::external(&root, table_name);
    publication.create(&table_schema()).expect("creating the table");

    let batch = RecordBatch::try_new(
        table_schema(),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            Arc::new(StringArray::from(vec![Some("north"), None, Some("south")])),
        ],
    )
    .expect("a valid batch");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(3))
        .expect("publishing");
}

fn tenant() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::from_u128(1))
}

/// A server over one readable table, with a policy that permits reading it.
fn server() -> (Arc<Server>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_table(dir.path(), "public", "example");

    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture must open: {refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "the fixture must read: {unreadable:?}");

    let tables = warehouse::describe(&found);
    let policy = wiring::permissive_policy(&tenant(), &tables);
    let settings = Settings {
        roles: Default::default(),
        listen: "127.0.0.1:0".to_string(),
        warehouse: dir.path().to_path_buf(),
        read_as_of: Lsn::new(u64::MAX),
        tenant: tenant(),
        cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
        flight_listen: None,
        maintenance: None,
        require_password: false,
        metrics_listen: None,
        transport_security: None,
    };
    (
        Arc::new(Server::with_tables(settings, policy, tables, servable)),
        dir,
    )
}

// --- what a query records -----------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_successful_query_is_counted_timed_and_its_rows_totalled() {
    let (server, _warehouse) = server();
    server
        .query("SELECT id FROM public.example", &Caller::new(&anyone()))
        .expect("the fixture table is readable");

    let metrics = server.metrics();
    assert_eq!(metrics.value(&QUERIES_TOTAL, &[("outcome", "ok")]), Some(1.0));
    assert_eq!(
        metrics.observation_count(&QUERY_DURATION_SECONDS, &[("outcome", "ok")]),
        1
    );
    assert_eq!(metrics.value(&ROWS_RETURNED_TOTAL, &[]), Some(3.0));
    assert!(!metrics.rejections().any(), "no call site disagrees with the catalogue");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_query_is_counted_and_timed_too() {
    // A duration histogram fed only by the successful path describes a system that never
    // fails, and the tail an operator goes looking for is made of failures.
    let (server, _warehouse) = server();
    server.query("SELECT * FROM public.no_such_table", &Caller::new(&anyone())).unwrap_err();

    let metrics = server.metrics();
    assert_eq!(metrics.value(&QUERIES_TOTAL, &[("outcome", "error")]), Some(1.0));
    assert_eq!(
        metrics.observation_count(&QUERY_DURATION_SECONDS, &[("outcome", "error")]),
        1,
        "the failure was timed as well as counted"
    );
    assert_eq!(
        metrics.value(&ROWS_RETURNED_TOTAL, &[]),
        None,
        "a failure returns no rows and adds none"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_is_not_counted_as_an_error() {
    // A refusal is the system working — a permission enforced. Counting refusals and errors
    // together makes a healthy system under load look like a broken one, which is how an
    // error-rate alert comes to fire on correct behaviour.
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_table(dir.path(), "public", "example");
    let (found, _) = warehouse::discover(dir.path());
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, _) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    let settings = Settings {
        roles: Default::default(),
        listen: "127.0.0.1:0".to_string(),
        warehouse: dir.path().to_path_buf(),
        read_as_of: Lsn::new(u64::MAX),
        tenant: tenant(),
        cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
        flight_listen: None,
        maintenance: None,
        require_password: false,
        metrics_listen: None,
        transport_security: None,
    };
    // An empty policy: the principal may read nothing.
    let server = Server::with_tables(settings, PolicySet::new(), warehouse::describe(&found), servable);

    server.query("SELECT 1", &Caller::new(&anyone())).unwrap_err();
    let metrics = server.metrics();
    assert_eq!(
        metrics.value(&QUERIES_TOTAL, &[("outcome", "refused")]),
        Some(1.0)
    );
    assert_eq!(metrics.value(&QUERIES_TOTAL, &[("outcome", "error")]), None);
}

#[test]
fn the_outcome_label_is_decided_by_the_sqlstate_class() {
    // A table, tested as a table. Driving this only through the query path exercised the
    // one state that path happens to produce — 42501 — so the arms covering quota
    // exhaustion and authentication were unreached, and a mutation folding them into
    // `error` changed nothing any test could see.
    //
    // The distinction matters because an error-rate alert built on this would fire on a
    // healthy system enforcing its own limits.
    let refusal = |state: &str| {
        wiring::outcome_label(&Err(sankhya_api_pg::session::QueryFailure {
            sqlstate: state.to_string(),
            message: String::new(),
            detail: None,
            subjects: Vec::new(),
        }))
    };

    // Class 53 — insufficient resources. A quota held is the system working.
    assert_eq!(refusal("53400"), "refused");
    assert_eq!(refusal("53200"), "refused");
    // Class 28 — invalid authorization. A rejected credential is the system working.
    assert_eq!(refusal("28000"), "refused");
    assert_eq!(refusal("28P01"), "refused");
    // Insufficient privilege.
    assert_eq!(refusal("42501"), "refused");
    // Cancelled by the client or a deadline: not a failure of the server.
    assert_eq!(refusal("57014"), "cancelled");
    // Everything else is an error, including the states that mean we are broken.
    assert_eq!(refusal("42601"), "error");
    assert_eq!(refusal("XX000"), "error");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_audit_count_rises_with_the_audit_and_not_with_the_scrape() {
    // Counted at the append rather than read from the chain's length, so that "the audit
    // stopped recording" and "the scrape stopped running" are distinguishable. Only one of
    // them is an emergency.
    let (server, _warehouse) = server();
    server.query("SELECT id FROM public.example", &Caller::new(&anyone())).expect("readable");
    server.query("SELECT * FROM public.nope", &Caller::new(&anyone())).unwrap_err();

    let metrics = server.metrics();
    assert_eq!(
        metrics.value(&AUDIT_RECORDS_TOTAL, &[]),
        Some(2.0),
        "both the success and the failure were audited"
    );
}

// --- the connection gauge -----------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_connection_gauge_returns_to_zero_however_a_connection_ends() {
    let (server, _warehouse) = server();
    let metrics = server.metrics();

    server.connection_opened();
    server.connection_opened();
    assert_eq!(metrics.value(&CONNECTIONS_ACTIVE, &[]), Some(2.0));

    server.connection_closed();
    server.connection_closed();
    assert_eq!(
        metrics.value(&CONNECTIONS_ACTIVE, &[]),
        Some(0.0),
        "a gauge that does not return to zero reads as a connection leak that is not there"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_connection_increments_and_decrements_the_gauge() {
    // Through the listener rather than by calling the hooks, because the property being
    // tested is that the listener calls them — on every path out, including the one a client
    // takes when it disconnects without saying goodbye.
    let (server, _warehouse) = server();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("an address");

    let handler: Arc<dyn Handler> = Arc::clone(&server) as Arc<_>;
    let accepting = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("a connection");
        sankhya_api_pg::listener::serve(stream, handler).await.ok();
    });

    let mut client = tokio::net::TcpStream::connect(address).await.expect("connect");
    // A startup packet the server will answer, so the connection is genuinely established
    // before it is dropped.
    let mut startup = Vec::new();
    let body: Vec<u8> = {
        let mut b = 196_608i32.to_be_bytes().to_vec();
        b.extend_from_slice(b"user\0tester\0\0");
        b
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    client.write_all(&startup).await.expect("startup");
    let mut scratch = [0u8; 1024];
    let read = client.read(&mut scratch).await.expect("a reply");
    assert!(read > 0, "the server answered the startup packet");

    assert_eq!(
        server.metrics().value(&CONNECTIONS_ACTIVE, &[]),
        Some(1.0),
        "the listener told the handler a connection opened"
    );

    // Dropped without a Terminate message: the commonest way a client goes away, and the
    // path an explicit decrement before each `return` is most likely to miss.
    drop(client);
    accepting.await.ok();
    assert_eq!(
        server.metrics().value(&CONNECTIONS_ACTIVE, &[]),
        Some(0.0),
        "and that it closed, even though the client never said so"
    );
}

// --- the endpoint -------------------------------------------------------

/// One `GET` against the scrape endpoint.
async fn get(address: std::net::SocketAddr, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(address).await.expect("connect");
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .await
        .expect("request");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.expect("response");
    String::from_utf8_lossy(&body).into_owned()
}

/// The endpoint, bound and served until the returned sender fires.
async fn endpoint(
    server: Arc<Server>,
) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("an address");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        scrape::serve_until(listener, server, async {
            stopped.await.ok();
        })
        .await
        .ok();
    });
    (address, stop)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_endpoint_serves_the_catalogue_in_the_exposition_format() {
    let (server, _warehouse) = server();
    server.query("SELECT id FROM public.example", &Caller::new(&anyone())).expect("readable");
    let (address, _stop) = endpoint(Arc::clone(&server)).await;

    let response = get(address, "/metrics").await;
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("Content-Type: text/plain; version=0.0.4"));
    assert!(response.contains("sankhya_queries_total{outcome=\"ok\"} 1"));
    assert!(response.contains("# TYPE sankhya_query_duration_seconds histogram"));
    assert!(response.contains("sankhya_rows_returned_total 3"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_table_gauge_is_refreshed_at_the_moment_of_the_scrape() {
    // Refreshed on scrape rather than on a timer, so the number is never from a previous
    // era. A gauge on a slow timer is wrong for as long as the timer is slow, and nothing
    // about the reading says so.
    let (server, warehouse) = server();
    assert_eq!(
        server.metrics().value(&TABLE_LIVE_FILES, &[("table", "public.example")]),
        None,
        "nothing has scraped yet"
    );

    let (address, _stop) = endpoint(Arc::clone(&server)).await;
    let response = get(address, "/metrics").await;
    assert!(
        response.contains("sankhya_table_live_files{table=\"public.example\"} 1"),
        "{response}"
    );

    // A second file lands between scrapes.
    //
    // Named in the log and never written --- the metric counts what the log says is live,
    // which is the thing under test. Constructed by the crate that owns the log rather than
    // assembled here, so this test says what it means: a log entry pointing at nothing.
    let root = warehouse.path().join("public").join("example");
    sankhya_table_delta::malformed::add_naming_a_missing_file(&root, 2, "part-0001.parquet", 1)
        .expect("a second commit");

    let response = get(address, "/metrics").await;
    assert!(
        response.contains("sankhya_table_live_files{table=\"public.example\"} 2"),
        "the second scrape sees the second file: {response}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn anything_that_is_not_the_metrics_path_is_refused() {
    // This is not an HTTP server and must not grow into one. The REST gateway is a separate
    // piece of work with authentication, size caps and a Flight-ticket fallback.
    let (server, _warehouse) = server();
    let (address, _stop) = endpoint(server).await;

    for path in [
        "/",
        "/query",
        // A prefix match served this one. It is harmless against an endpoint that reads no
        // files, and it is the shape that becomes a traversal the moment one does.
        "/metrics/../etc/passwd",
        "/metricsandmore",
        "/healthz",
    ] {
        let response = get(address, path).await;
        assert!(
            response.starts_with("HTTP/1.1 404 Not Found"),
            "{path} was not refused: {response}"
        );
    }

    // A query string is permitted, because some collectors append one and a 404 for that
    // reason is unguessable from the other end.
    assert!(get(address, "/metrics?x=1").await.starts_with("HTTP/1.1 200 OK"));
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_get_is_served() {
    let (server, _warehouse) = server();
    let (address, _stop) = endpoint(server).await;
    let mut stream = tokio::net::TcpStream::connect(address).await.expect("connect");
    stream
        .write_all(b"POST /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .expect("request");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.expect("response");
    assert!(String::from_utf8_lossy(&body).starts_with("HTTP/1.1 404 Not Found"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registrys_own_refusals_are_exported() {
    // A dashboard that cannot see these cannot tell an incomplete metric from a quiet one.
    let (server, _warehouse) = server();
    let (address, _stop) = endpoint(Arc::clone(&server)).await;

    let response = get(address, "/metrics").await;
    for reason in [
        "value_not_permitted",
        "label_not_declared",
        "label_missing",
        "over_cap",
    ] {
        assert!(
            response.contains(&format!("sankhya_metrics_rejected_total{{reason=\"{reason}\"}} 0")),
            "{reason} is not exported: {response}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_declared_metric_appears_in_a_scrape_even_at_zero() {
    // A dashboard must be able to tell "no events" from "not wired up", and an absent metric
    // looks like the second while usually being the first.
    let (server, _warehouse) = server();
    let (address, _stop) = endpoint(server).await;
    let response = get(address, "/metrics").await;
    for metric in ALL {
        assert!(
            response.contains(&format!("# TYPE {} ", metric.name)),
            "{} never appeared in a scrape",
            metric.name
        );
    }
}

// --- every failure a user meets carries a code and a remediation --------

/// The failure a statement produces, or a panic naming what came back instead.
fn failure_for(server: &Server, sql: &str) -> sankhya_api_pg::session::QueryFailure {
    match server.query(sql, &Caller::new(&anyone())) {
        Ok(_) => panic!("`{sql}` was expected to fail and did not"),
        Err(failure) => failure,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_statement_carries_a_catalogue_code_and_a_remediation() {
    // M6 exit criterion 6: every user-reachable error has documented remediation, generated
    // from the same source as the catalogue. The errors a user actually meets are almost all
    // engine failures, and those used to come back as a raw message with a SQLSTATE guessed
    // from substrings — no code, nothing to do, nothing to look up.
    let (server, _warehouse) = server();
    let failure = failure_for(&server, "SELECT * FROM public.no_such_table");

    assert!(
        failure.message.starts_with("[SNK-C0001]"),
        "the code comes first, because that is what a runbook is indexed by: {}",
        failure.message
    );
    let remediation = failure.detail.expect("a remediation reaches the client");
    assert!(
        remediation.contains("Correct the statement"),
        "the remediation is the catalogue's own words: {remediation}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sqlstate_names_the_kind_of_failure_and_not_only_its_class() {
    // Every driver in the PostgreSQL ecosystem branches on SQLSTATE. A plausible message with
    // the wrong five characters produces a client that connects, appears to work, and
    // mishandles every failure.
    //
    // This asserted `42601` for a missing table until an adversarial review pointed out what
    // that costs: `42601` is *syntax_error*, so every migration tool asking for a table it is
    // about to create was told its generated SQL was malformed. `42P01`, *undefined_table*, is
    // the code all of them branch on to mean "create it".
    //
    // The class remains the **floor** --- a failure whose kind carries no code of its own still
    // gets the class's. What changed is that a kind which does have one now gets it, which is
    // a refinement of the old rule rather than an exception to it.
    let (server, _warehouse) = server();
    let failure = failure_for(&server, "SELECT * FROM public.no_such_table");
    assert_eq!(failure.sqlstate, "42P01", "undefined_table, not a syntax error");

    let failure = failure_for(&server, "SELECT no_such_column FROM example");
    assert_eq!(failure.sqlstate, "42703", "undefined_column");

    // Not a five-character-looking string: exactly five characters, which is what the
    // protocol requires and what a driver's lookup table is keyed by.
    assert_eq!(failure.sqlstate.len(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_statement_is_a_user_error_and_not_an_internal_one() {
    // The difference decides whether a client retries and whether anybody is woken.
    let (server, _warehouse) = server();
    for sql in [
        "SELECT FROM WHERE",
        "SELEKT 1",
        "SELECT no_such_column FROM public.example",
    ] {
        let failure = failure_for(&server, sql);
        assert!(
            failure.message.starts_with("[SNK-C"),
            "`{sql}` produced {} — the C prefix is the client's own fault",
            failure.message
        );
        assert!(failure.detail.is_some(), "`{sql}` came back with no remediation");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_query_naming_a_forbidden_table_looks_exactly_like_one_naming_a_missing_table() {
    // The property the whole registration approach exists to produce. Saying "you may not
    // read that" would confirm it exists, and existence is frequently the secret.
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_table(dir.path(), "public", "example");
    write_table(dir.path(), "public", "secret");
    let (found, _) = warehouse::discover(dir.path());
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, _) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    let tables = warehouse::describe(&found);

    // A policy granting only `example`.
    let policy = wiring::permissive_policy(
        &tenant(),
        &tables
            .iter()
            .filter(|t| t.name == "example")
            .cloned()
            .collect::<Vec<_>>(),
    );
    let settings = Settings {
        roles: Default::default(),
        listen: "127.0.0.1:0".to_string(),
        warehouse: dir.path().to_path_buf(),
        read_as_of: Lsn::new(u64::MAX),
        tenant: tenant(),
        cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
        flight_listen: None,
        maintenance: None,
        require_password: false,
        metrics_listen: None,
        transport_security: None,
    };
    let server = Server::with_tables(settings, policy, tables, servable);

    let forbidden = failure_for(&server, "SELECT * FROM public.secret");
    let missing = failure_for(&server, "SELECT * FROM public.absent");
    assert_eq!(forbidden.sqlstate, missing.sqlstate);
    assert_eq!(
        forbidden.message.replace("secret", "X"),
        missing.message.replace("absent", "X"),
        "the two failures differ only in the name the caller supplied"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_is_refused_rather_than_confirmed_and_discarded() {
    // This one was a real defect and the worst shape a defect takes here: not an error, not
    // a wrong number, but a *success tag for work that did not happen*. DataFusion will
    // happily run DDL against its own in-memory catalogue, so `CREATE TABLE` returned
    // success, the table existed for the rest of that connection, and it was gone on
    // reconnect. A user would reasonably conclude their table had been created.
    let (server, _warehouse) = server();
    for sql in [
        "CREATE TABLE public.other (id BIGINT)",
        // Columns named rather than positional. A published table carries the mandated
        // `sank_data_date` partition column, so a bare VALUES list has the wrong arity and
        // fails in the *planner* --- which would pass this test for the wrong reason, on a
        // planning error rather than the write refusal it exists to check.
        "INSERT INTO public.example (id, label) VALUES (4, 'east')",
        "CREATE VIEW public.v AS SELECT 1",
        // A leading CTE, which is why the check is against the planned logical plan rather
        // than against the statement's first word.
        "WITH src AS (SELECT 9 AS id) INSERT INTO public.example (id, label) SELECT id, 'x' FROM src",
    ] {
        let failure = failure_for(&server, sql);
        assert!(
            failure.message.starts_with("[SNK-C0006]"),
            "`{sql}` gave {}",
            failure.message
        );
        assert!(failure.detail.is_some(), "`{sql}` came back with no remediation");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_write_refusal_names_the_supported_route() {
    // A refusal that only says no sends somebody looking for a flag to turn it on, and
    // there is no flag: writes reach the warehouse through capture or through the publishing
    // tool. Asserted on DDL specifically, because a statement the planner itself refuses
    // first — an INSERT against a provider with no insert path — never reaches this check
    // and carries the catalogue's general wording instead.
    let (server, _warehouse) = server();
    let failure = failure_for(&server, "CREATE TABLE public.other (id BIGINT)");
    let remediation = failure.detail.expect("a remediation");
    assert!(remediation.contains("sankhya-publish"), "{remediation}");
    assert!(remediation.contains("GUIDE.md"), "{remediation}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_that_looks_like_a_write_is_still_served() {
    // The refusal is on the plan's shape, so a query whose *text* contains the word insert
    // is unaffected. A keyword check would have refused this.
    let (server, _warehouse) = server();
    server
        .query("SELECT id AS insert_id FROM public.example WHERE label = 'create table'", &Caller::new(&anyone()))
        .expect("a read is a read whatever it is spelled with");
}

/// The caller a test means when it does not care who is asking.
///
/// Its own helper rather than an inline literal at forty call sites: when a test *does* care,
/// it should be visibly different from one that does not.
#[allow(dead_code)]
fn anyone() -> Vec<(String, String)> {
    vec![("user".to_string(), "quickstart".to_string())]
}
