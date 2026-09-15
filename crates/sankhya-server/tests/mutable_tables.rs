//! A row updated twice is one row.
//!
//! `M26a`. Capture records inserts, updates and deletes as **rows**, so a table whose source
//! mutates in place holds several versions of one row. Unioning its files returns all of them:
//! `COUNT(*)` says two where the answer is one, and `SUM` adds the old value to the new one.
//! Nothing about the result says so, and every correctness property built around it holds ---
//! exactly-once capture, reconciliation against the source, splice coverage. None of them is
//! about *resolving* two versions of a row.
//!
//! `sankhya_readpath::ResolvedTable` resolves exactly that, and **nothing outside its own
//! tests ever constructed one**. The key columns were declared by the publisher and written
//! into the table's own log where they cannot be forgotten; the read path never read them.
//!
//! These tests go through the front door, because that is where the defect was: the library
//! was correct and unreachable.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use sankhya_api_pg::session::{Caller, Handler};
use sankhya_authz::policy::{Action, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Role, TenantId};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::Arc;

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
#[path = "../src/graphs.rs"]
mod graphs;
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
#[path = "../src/wiring.rs"]
mod wiring;

use wiring::{Server, Settings};

fn tenant() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::from_u128(1))
}

/// The shape capture produces: the row's own columns, its position, and what happened to it.
fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
        Field::new("_sankhya_op", DataType::Utf8, false),
    ]))
}

fn batch(rows: Vec<(i64, f64, u64, &str)>) -> RecordBatch {
    RecordBatch::try_new(schema(), vec![
        Arc::new(Int64Array::from(rows.iter().map(|r| r.0).collect::<Vec<i64>>())),
        Arc::new(Float64Array::from(rows.iter().map(|r| r.1).collect::<Vec<f64>>())),
        Arc::new(UInt64Array::from(rows.iter().map(|r| r.2).collect::<Vec<u64>>())),
        Arc::new(StringArray::from(rows.iter().map(|r| r.3).collect::<Vec<&str>>())),
    ])
    .expect("a valid batch")
}

/// A warehouse holding `sales.accounts`, keyed by `id` when `keyed` is set.
///
/// Row 1 is inserted at 1 and updated at 3; row 2 is inserted at 2 and deleted at 4; row 3 is
/// inserted at 5 and never touched. Resolved, that is two live rows totalling 130.
/// Unresolved it is five rows totalling 151 — a number of the right shape and no meaning.
fn warehouse_with_a_mutable_table(keyed: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("sales").join("accounts");
    let publication = Publication::external(&root, "accounts");
    let publication = if keyed { publication.keyed_by(["id"]) } else { publication };
    publication.create(&schema()).expect("creating");
    publication
        .append(
            1,
            "part-0000.parquet",
            &batch(vec![
                (1, 10.0, 1, "I"),
                (2, 11.0, 2, "I"),
                (1, 100.0, 3, "U"),
                (2, 0.0, 4, "D"),
                (3, 30.0, 5, "I"),
            ]),
            Lsn::new(5),
        )
        .expect("publishing");
    dir
}

fn settings(warehouse: &std::path::Path) -> Settings {
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
        require_password: false,
        user_functions: false,
        python: std::path::PathBuf::from("/usr/bin/python3"),
        metrics_detail: false,
        policy: None,
        metrics_listen: None,
        transport_security: None,
    }
}

fn server_over(dir: &tempfile::TempDir) -> (Server, Vec<String>) {
    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture must open: {refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, complaints) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    let policy = PolicySet::new().with(Rule::grant(
        tenant(),
        Role::new("reader"),
        TableRef::new("sales", "accounts"),
        Action::Read,
    ));
    let mut settings = settings(dir.path());
    settings.roles = [("ana".to_string(), vec!["reader".to_string()])]
        .into_iter()
        .collect();
    let server =
        Server::with_tables(settings, policy, warehouse::describe(&found), servable);
    server
        .authenticate(&[("user".to_string(), "ana".to_string())], Some(b"x"))
        .expect("authenticated");
    let said: Vec<String> = complaints.into_iter().map(|(_, why)| why).collect();
    (server, said)
}

fn anyone() -> Vec<(String, String)> {
    vec![("user".to_string(), "ana".to_string())]
}

fn one(server: &Server, sql: &str) -> String {
    let result = server
        .query(sql, &Caller::new(&anyone()))
        .unwrap_or_else(|why| panic!("{sql}: {}", why.message));
    result.rows[0][0].clone().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_row_updated_twice_is_counted_once() {
    // **The whole defect in one number.** Five change rows describe three keys, one of which
    // was deleted. The answer is two rows totalling 130. Unresolved it is five rows totalling
    // 151 --- which is not an error, not a crash, and not obviously wrong to anybody reading
    // it.
    let dir = warehouse_with_a_mutable_table(true);
    let (server, complaints) = server_over(&dir);
    assert!(complaints.is_empty(), "the table must serve: {complaints:?}");

    assert_eq!(one(&server, "SELECT COUNT(*) FROM sales.accounts"), "2");
    assert_eq!(one(&server, "SELECT SUM(amount) FROM sales.accounts"), "130");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_latest_version_of_a_key_is_the_one_served() {
    // Not merely *a* version. Row 1 was inserted at 10.0 and updated to 100.0, and the
    // resolution orders by position descending --- so a fold that kept the first arrival
    // rather than the last would give 10.0 here and the same row count.
    let dir = warehouse_with_a_mutable_table(true);
    let (server, _) = server_over(&dir);
    assert_eq!(
        one(&server, "SELECT amount FROM sales.accounts WHERE id = 1"),
        "100"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deleted_key_is_absent_rather_than_present_at_its_last_value() {
    // The tombstone is filtered **after** the resolution, deliberately: removing it first
    // would let the version before it win, so a deleted row would come back as whatever it
    // was --- which is worse than a row that is merely stale, because it is a row the source
    // says does not exist.
    let dir = warehouse_with_a_mutable_table(true);
    let (server, _) = server_over(&dir);
    assert_eq!(
        one(&server, "SELECT COUNT(*) FROM sales.accounts WHERE id = 2"),
        "0"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_table_that_declares_no_key_is_served_exactly_as_it_was() {
    // **Append-only pays nothing.** Most high-volume tables are append-only, so most queries
    // take this path, and it has to cost nothing at all --- not a cheap check, nothing. The
    // same rows, undeclared, are five rows totalling 151, and that is the correct answer for
    // a table whose source only ever adds.
    //
    // It is also the control for every assertion above: without it they would pass on a
    // fixture that had simply been written with two rows.
    let dir = warehouse_with_a_mutable_table(false);
    let (server, complaints) = server_over(&dir);
    assert!(complaints.is_empty(), "the table must serve: {complaints:?}");

    assert_eq!(one(&server, "SELECT COUNT(*) FROM sales.accounts"), "5");
    assert_eq!(one(&server, "SELECT SUM(amount) FROM sales.accounts"), "151");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_declared_on_a_table_that_never_came_through_capture_is_reported() {
    // Such a table has one version of each row by construction, so serving it raw is
    // **correct** --- and refusing it would take a perfectly readable table out of the
    // warehouse over metadata describing a pipeline it is not on. The declaration is still
    // reported, because a key on a table that cannot use it is a declaration somebody wrote
    // for a reason that has stopped being true.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let plain = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
    ]));
    let root = dir.path().join("sales").join("accounts");
    let publication = Publication::external(&root, "accounts").keyed_by(["id"]);
    publication.create(&plain).expect("creating");
    publication
        .append(
            1,
            "part-0000.parquet",
            &RecordBatch::try_new(Arc::clone(&plain), vec![
                Arc::new(Int64Array::from(vec![1_i64, 2])),
                Arc::new(Float64Array::from(vec![10.0, 20.0])),
            ])
            .expect("a valid batch"),
            Lsn::new(2),
        )
        .expect("publishing");

    let (server, complaints) = server_over(&dir);
    assert!(
        complaints.iter().any(|said| said.contains("did not come through capture")),
        "the declaration must be reported: {complaints:?}"
    );
    // And the table still answers, with its rows.
    assert_eq!(one(&server, "SELECT COUNT(*) FROM sales.accounts"), "2");
}
