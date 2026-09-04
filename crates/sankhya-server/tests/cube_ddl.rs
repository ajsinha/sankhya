//! Cubes created and dropped from SQL, by a client, against a running server.
//!
//! # What was missing, and why it blocked something specific
//!
//! Until this, a cube reached a warehouse exactly one way: somebody wrote a JSON file into
//! `_cubes/` and restarted the server. `tests/cubes.rs` proves that path and says in its own
//! header that it is deliberately the *startup* half.
//!
//! `M12` --- the twelve-hour, two-machine acceptance run --- builds, queries and drops cuboids
//! underneath a live workload, and names this gap as blocking: *"this run needs cubes created
//! and dropped from SQL by a client rather than declared into a warehouse directory."* A
//! directory and a restart cannot do that.
//!
//! These tests are the other half: the statement, its refusals, and what a drop is obliged to
//! clean up.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_api_pg::catalog::CatalogTable;
use sankhya_api_pg::session::Caller;
use sankhya_api_pg::session::Handler;

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
#[path = "../src/wiring.rs"]
mod wiring;

use wiring::{permissive_policy, Server, Settings};

fn tenant() -> sankhya_authz::principal::TenantId {
    sankhya_authz::principal::TenantId::from_uuid(uuid::Uuid::from_u128(1))
}

fn settings(warehouse: &std::path::Path) -> Settings {
    Settings {
        roles: Default::default(),
        listen: "127.0.0.1:0".to_string(),
        warehouse: warehouse.to_path_buf(),
        read_as_of: sankhya_types::Lsn::new(u64::MAX),
        tenant: tenant(),
        cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
        flight_listen: None,
        // No maintenance. A test that lets the server maintain its own warehouse becomes a
        // second writer against a directory the test is also inspecting, which is how a soak
        // once filled a disk.
        maintenance: None,
        require_password: false,
        user_functions: false,
        metrics_listen: None,
        transport_security: None,
    }
}

fn table(name: &str) -> CatalogTable {
    CatalogTable { schema: String::new(), name: name.to_string(), columns: Vec::new() }
}

/// A server whose principal may read `orders` and `regions` and nothing else.
fn server_over(warehouse: &std::path::Path) -> Server {
    let tables = vec![table("orders"), table("regions")];
    let policy = permissive_policy(&tenant(), &tables);
    let (server, complaints) =
        Server::with_tables(settings(warehouse), policy, tables, Vec::new())
            .adopting_cubes(warehouse);
    assert!(complaints.is_empty(), "nothing to complain about: {complaints:?}");
    server
}

const SALES: &str = "CREATE CUBE sales FROM orders \
     DIMENSION region FROM regions ON region (LEVEL area = region) \
     MEASURE amount (SUM ALONG region)";

#[tokio::test]
async fn a_cube_created_from_sql_is_served_and_survives_a_restart() {
    // Both halves, because either alone is a cube that only looks like it works. Served but
    // not persisted is a cube that vanishes at the next restart; persisted but not served is
    // one that answers nothing until then.
    let dir = tempfile::tempdir().expect("a temporary directory");

    let server = server_over(dir.path());
    assert!(server.cubes().is_empty(), "nothing declared this warehouse a cube yet");

    let result = server.query(SALES, &Caller::new(&anyone())).expect("the statement is accepted");
    assert_eq!(result.tag, "CREATE CUBE");
    assert_eq!(server.cubes().len(), 1, "and it is being served immediately");
    assert_eq!(server.cubes()[0].name(), "sales");

    let restarted = server_over(dir.path());
    assert_eq!(
        restarted.cubes().len(),
        1,
        "a cube created by a statement must be there after a restart, or the statement wrote \
         nothing that matters"
    );
    assert_eq!(restarted.cubes()[0].name(), "sales");
}

#[tokio::test]
async fn a_cube_is_dropped_from_sql_and_stays_dropped() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());
    server.query(SALES, &Caller::new(&anyone())).expect("creating it");

    let result = server.query("DROP CUBE sales", &Caller::new(&anyone())).expect("dropping it");
    assert_eq!(result.tag, "DROP CUBE");
    assert!(server.cubes().is_empty(), "it stops being served at once");

    let restarted = server_over(dir.path());
    assert!(
        restarted.cubes().is_empty(),
        "and its definition is gone, or the drop only lasted until the next restart"
    );
}

#[tokio::test]
async fn dropping_a_cube_reclaims_the_cuboids_nothing_else_ever_would() {
    // The failure this guards is silent and permanent. `retire_superseded` *deliberately*
    // retains a cuboid whose cube has no known current version --- "deleting on a guess is
    // how a cache becomes a data loss" --- and a dropped cube is exactly that. So without a
    // drop that reclaims, every cuboid a dropped cube materialised is kept for good, on
    // purpose, by the one mechanism that could have removed it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());
    server.query(SALES, &Caller::new(&anyone())).expect("creating it");

    // A cuboid directory named the way this cube's cuboids are named, and one belonging to a
    // cube with a similar name that must survive.
    let store = dir.path().join("_cubes");
    let key = sankhya_cube::materialise::Key {
        definition: 7,
        snapshot: 11,
        scope: 13,
        cuboid: sankhya_cube::algo::Cuboid::of(&["region"]),
    };
    let ours = store.join(key.table("sales"));
    let theirs = store.join(key.table("sales_archive"));
    for path in [&ours, &theirs] {
        std::fs::create_dir_all(path.join("_delta_log")).expect("a cuboid directory");
        std::fs::write(path.join("part-0.parquet"), b"cells").expect("something to reclaim");
    }

    server.query("DROP CUBE sales", &Caller::new(&anyone())).expect("dropping it");

    assert!(!ours.exists(), "the dropped cube's cuboid is reclaimed");
    assert!(
        theirs.exists(),
        "and a cube whose name merely starts the same keeps its own. Dropping by prefix \
         would have taken this, which is why the name is length-prefixed and parsed rather \
         than matched"
    );
}

#[tokio::test]
async fn a_name_already_taken_is_refused_rather_than_replaced() {
    // There is no `OR REPLACE`, and that is a decision rather than an omission: replacing a
    // cube retires every cuboid it materialised, and a re-run of a script should not cause
    // that silently.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());
    server.query(SALES, &Caller::new(&anyone())).expect("creating it");

    let failure = server.query(SALES, &Caller::new(&anyone())).expect_err("the second one is refused");
    assert!(
        failure.message.contains("already exists"),
        "the message should say what is wrong: {}",
        failure.message
    );
    assert_eq!(server.cubes().len(), 1, "and the first is untouched");
}

#[tokio::test]
async fn dropping_a_cube_that_is_not_there_is_an_error_unless_if_exists_was_written() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());

    let failure = server.query("DROP CUBE nothing", &Caller::new(&anyone())).expect_err("there is no such cube");
    assert!(failure.message.contains("nothing"), "{}", failure.message);

    let result = server
        .query("DROP CUBE IF EXISTS nothing", &Caller::new(&anyone()))
        .expect("IF EXISTS makes it a no-op rather than a failure");
    assert_eq!(result.tag, "DROP CUBE");
}

#[tokio::test]
async fn a_cube_on_a_table_the_caller_cannot_read_is_refused_without_confirming_it_exists() {
    // The query path already refuses to confirm a table's existence to somebody who may not
    // read it. Cube DDL naming a fact table would be the same question through another door,
    // so it gets the same answer — and, importantly, the *same* answer for both cases.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());

    let denied = server
        .query(
            "CREATE CUBE payroll_cube FROM payroll \
             DIMENSION region FROM regions ON region (LEVEL area = region) \
             MEASURE amount (SUM ALONG region)",
            &Caller::new(&anyone()),
        )
        .expect_err("payroll is not readable by this principal");
    let absent = server
        .query(
            "CREATE CUBE missing_cube FROM no_such_table \
             DIMENSION region FROM regions ON region (LEVEL area = region) \
             MEASURE amount (SUM ALONG region)",
            &Caller::new(&anyone()),
        )
        .expect_err("and this table does not exist at all");

    assert_eq!(
        denied.message.replace("payroll", "T"),
        absent.message.replace("no_such_table", "T"),
        "a refusal that reads differently for `may not` and `does not exist` tells the \
         caller which one it was, which is the disclosure the whole policy layer refuses"
    );
    assert!(server.cubes().is_empty());
}

#[tokio::test]
async fn a_dimension_table_the_caller_cannot_read_is_refused_too() {
    // The fact table is the obvious one and the dimension tables are the ones a check
    // written in a hurry forgets. A cube reads all of them.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());

    server
        .query(
            "CREATE CUBE sales FROM orders \
             DIMENSION region FROM secret_regions ON region (LEVEL area = region) \
             MEASURE amount (SUM ALONG region)",
            &Caller::new(&anyone()),
        )
        .expect_err("the dimension table is not readable");
    assert!(server.cubes().is_empty());
}

#[tokio::test]
async fn a_definition_that_does_not_describe_a_usable_cube_is_refused_with_every_reason() {
    // Validation is the validator's, not the parser's, and it reports every rejection rather
    // than the first — a definition fixable in one sitting should be reported in one message.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());

    let failure = server
        .query(
            "CREATE CUBE sales FROM orders \
             DIMENSION region FROM regions ON region (LEVEL area = region) \
             DIMENSION period FROM regions ON region (LEVEL month = month) \
             MEASURE amount (SUM ALONG region)",
            &Caller::new(&anyone()),
        )
        .expect_err("the measure declares no rule along `period`");
    assert!(
        failure.message.contains("period"),
        "the rejection should name what is missing: {}",
        failure.message
    );
    assert!(server.cubes().is_empty(), "and nothing was persisted on the way to refusing");
}

#[tokio::test]
async fn a_syntax_error_is_reported_as_one_rather_than_handed_to_the_engine() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());

    let failure = server
        .query("CREATE CUBE sales FROM orders DIMENSION", &Caller::new(&anyone()))
        .expect_err("that is not a statement");
    assert_eq!(failure.sqlstate, "42601", "a syntax error is a syntax error");
}

// A multi-threaded runtime, unlike every test above it: this statement reaches the
// engine, and the engine blocks a worker to run it. That the DDL tests need no such
// runtime is the useful signal --- cube DDL answers without the query path at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ordinary_statement_still_reaches_the_engine() {
    // The load-bearing one. Cube DDL is recognised before the engine is asked anything, so a
    // regression here does not break cubes — it breaks SQL.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());

    let result = server.query("SELECT 1", &Caller::new(&anyone())).expect("ordinary SQL is untouched by the DDL path");
    assert_eq!(result.rows.len(), 1);
    assert_ne!(result.tag, "CREATE CUBE");
}

// A multi-threaded runtime, unlike every test above it: this statement reaches the
// engine, and the engine blocks a worker to run it. That the DDL tests need no such
// runtime is the useful signal --- cube DDL answers without the query path at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cube_created_by_one_statement_is_visible_to_the_next() {
    // A `SessionContext` is built per statement and the cube list is not, so this is the
    // property that makes the DDL useful at all rather than a write nobody sees.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let server = server_over(dir.path());
    server.query(SALES, &Caller::new(&anyone())).expect("creating it");

    let result = server
        .query("SELECT cube FROM cubes()", &Caller::new(&anyone()))
        .expect("the catalogue function answers");
    let named: Vec<String> =
        result.rows.iter().filter_map(|row| row[0].clone()).collect();
    assert!(
        named.contains(&"sales".to_string()),
        "a cube created a statement ago should be listed by the next: {named:?}"
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
