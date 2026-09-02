//! `CREATE TABLE ... CLONE`, run by a client against a running server.
//!
//! # Why this file exists at all
//!
//! Four times in one day a crate exposing a whole SQL surface turned out to be unreachable from
//! the thing that serves SQL, and every one was found by accident. `check-surfaces` catches the
//! crate-level case; it cannot catch a statement that parses, is intercepted, and does nothing
//! useful. Only running it can.
//!
//! So these are about *reachability and effect*: the statement is accepted, the clone exists
//! afterwards, it survives a restart, its lineage is readable, and the ordinary statements this
//! interception sits in front of still reach the engine.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use sankhya_api_pg::catalog::CatalogTable;
use sankhya_api_pg::session::Handler;
use sankhya_publish::Publication;
use sankhya_table_delta::Action;
use std::sync::Arc;

#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/warehouse.rs"]
mod warehouse;
#[path = "../src/clones.rs"]
mod clones;
#[path = "../src/wiring.rs"]
mod wiring;

use wiring::{permissive_policy, Server, Settings};

fn tenant() -> sankhya_authz::principal::TenantId {
    sankhya_authz::principal::TenantId::from_uuid(uuid::Uuid::from_u128(1))
}

fn settings(warehouse: &std::path::Path) -> Settings {
    Settings {
        listen: "127.0.0.1:0".to_string(),
        warehouse: warehouse.to_path_buf(),
        read_as_of: sankhya_types::Lsn::new(u64::MAX),
        tenant: tenant(),
        cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
        flight_listen: None,
        // No maintenance, for the reason the cube tests give: a test that lets the server
        // maintain a warehouse it is also inspecting has become a second writer.
        maintenance: None,
        require_password: false,
        metrics_listen: None,
        transport_security: None,
    }
}

/// A catalogue entry naming the schema its table is actually in.
///
/// Empty, once. That made the fixture's own catalogue disagree with the fixture's own
/// warehouse --- discovery would have said `records.entries`, and this said `entries` under no
/// schema at all --- and it is the same class of mistake as putting the tables at the
/// warehouse root: a fixture whose shape is not the product's shape tests the fixture.
fn table(name: &str) -> CatalogTable {
    CatalogTable { schema: SCHEMA.to_string(), name: name.to_string(), columns: Vec::new() }
}

/// The schema every table in this fixture lives under.
///
/// # Why this is not the warehouse root, which is where it used to be
///
/// Discovery reads `<schema>/<table>/` and a session registers each table under its bare name.
/// This fixture put `entries` at the warehouse *root*, which is a layout no deployment has ---
/// and cloning resolved names the same way, so every test here passed against a warehouse the
/// server could not have served. `CREATE TABLE ... CLONE` had never worked against a table
/// anybody could query, and nothing in this file could see it.
///
/// A fixture whose shape is not the product's shape tests the fixture.
const SCHEMA: &str = "records";

/// Where a table of this name lives in this fixture.
fn table_root(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    root.join(SCHEMA).join(name)
}

/// A warehouse holding `entries`, whose version 1 names one file and version 2 another.
///
/// Built through the product's own writer. The first draft assembled the log by hand ---
/// metadata, a parquet, the add actions --- and `check-writers` refused it, which is the rule
/// working rather than getting in the way: a fixture that encodes the storage layout goes on
/// encoding the *old* layout after the layout changes, and a test whose fixture cannot have the
/// write path's bug is testing less than it looks like it is.
fn warehouse_with_entries(root: &std::path::Path) {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let table_root = table_root(root, "entries");
    let publication = Publication::external(&table_root, "entries");
    publication.create(&schema).expect("creating");

    for version in 1..=2u64 {
        let base = i64::try_from(version).unwrap_or(0) * 100;
        let ids: Vec<i64> = (0..8).map(|i| base + i).collect();
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Int64Array::from(ids))],
        )
        .expect("a batch");
        publication
            .append(
                version,
                &format!("part-{version:04}.parquet"),
                &batch,
                sankhya_types::Lsn::new(version),
            )
            .expect("appending");
    }
}

/// A file the table's log named at `version` and no longer names.
fn a_file_of(root: &std::path::Path, version: u64) -> std::path::PathBuf {
    let live = sankhya_table_delta::live_files_at(&table_root(root, "entries"), version)
        .expect("that version resolves");
    table_root(root, "entries").join(&live.files.first().expect("a file").path)
}

fn server_over(root: &std::path::Path) -> Server {
    let tables = vec![table("entries")];
    let policy = permissive_policy(&tenant(), &tables);
    let (server, _) = Server::with_tables(settings(root), policy, tables, Vec::new())
        .adopting_cubes(root);
    server
}

fn lineage_of(root: &std::path::Path, table: &str) -> sankhya_clone::Lineage {
    let actions =
        sankhya_table_delta::read_actions(&table_root(root, table)).expect("the clone's log");
    let actions: Vec<Action> = actions.into_iter().map(|(_, action)| action).collect();
    sankhya_clone::lineage_of(&actions)
        .expect("it is a clone")
        .expect("its lineage is readable")
}

#[tokio::test]
async fn a_clone_created_from_sql_exists_afterwards_and_records_where_it_came_from() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    let server = server_over(dir.path());

    let result = server
        .query("CREATE TABLE staging CLONE entries AT VERSION 1")
        .expect("the statement is accepted");
    assert_eq!(result.tag, "CREATE TABLE");

    assert!(
        table_root(dir.path(), "staging").join("_delta_log").exists(),
        "the clone was committed beside the table it was cloned from"
    );
    let lineage = lineage_of(dir.path(), "staging");
    // Qualified, because a lineage is a *record* and outlives the statement that wrote it. A
    // bare `entries` is unambiguous until a second schema grows one, and on that day every
    // clone in the warehouse would silently point at whichever the walk found first.
    assert_eq!(lineage.origin, format!("{SCHEMA}.entries"));
    assert_eq!(lineage.version, 1);
}

#[tokio::test]
async fn a_clone_adds_no_files_of_its_own() {
    // Decision 1a, from outside. A clone that copied files would be a copy, and the whole
    // lifetime argument rests on it not being one.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    server_over(dir.path())
        .query("CREATE TABLE staging CLONE entries")
        .expect("cloning");

    let live = sankhya_table_delta::live_files(&table_root(dir.path(), "staging")).expect("its log");
    assert!(live.files.is_empty(), "a clone that adds files is a copy");

    let parquet = std::fs::read_dir(table_root(dir.path(), "staging"))
        .expect("the clone's directory")
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "parquet"))
        .count();
    assert_eq!(parquet, 0, "and it wrote none to disk either");
}

#[tokio::test]
async fn a_clone_with_no_version_takes_the_origin_as_it_stands() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    server_over(dir.path())
        .query("CREATE TABLE staging CLONE entries")
        .expect("cloning");

    assert_eq!(
        lineage_of(dir.path(), "staging").version,
        2,
        "the newest version, resolved when the clone was made rather than when it was parsed"
    );
}

#[tokio::test]
async fn cloning_a_table_that_does_not_exist_says_what_the_query_path_says() {
    // Saying "you may not read `payroll`" would confirm that `payroll` exists, so a table the
    // principal cannot see and a table that is not there give the same sentence.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());

    let refused = server_over(dir.path())
        .query("CREATE TABLE staging CLONE payroll")
        .expect_err("no such table");
    assert!(format!("{refused:?}").contains("payroll"), "{refused:?}");
}

#[tokio::test]
async fn cloning_a_table_that_exists_and_may_not_be_read_is_refused_by_the_same_sentence() {
    // The test above says "no such table" and proves only that. `payroll` is not in that
    // warehouse at all, so the statement is refused when the *name* fails to resolve and the
    // authorization check is never reached --- which a mutation showed by surviving its
    // removal.
    //
    // This is the case the check exists for: the table is really there, and this principal has
    // no rule granting it. A clone is a read (`ADR-0016` makes it a reference to the origin's
    // files), so cloning what you may not read *is* reading it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());

    // Present in the warehouse and absent from the catalogue this server was built with, so
    // `permissive_policy` wrote no rule for it.
    let columns = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    Publication::external(table_root(dir.path(), "payroll"), "payroll")
        .create(&columns)
        .expect("creating payroll");

    let refused = server_over(dir.path())
        .query("CREATE TABLE staging CLONE payroll")
        .expect_err("a table this principal may not read");
    assert!(format!("{refused:?}").contains("payroll"), "{refused:?}");
    assert!(
        !table_root(dir.path(), "staging").exists(),
        "and nothing was created from a table the caller may not read"
    );
}

#[tokio::test]
async fn cloning_over_a_table_that_already_exists_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    let server = server_over(dir.path());

    let refused = server
        .query("CREATE TABLE entries CLONE entries")
        .expect_err("the name is taken");
    let said = format!("{refused:?}");
    assert!(said.contains("already exists"), "{said}");
    assert!(said.contains("only reader of"), "{said}");
}

#[tokio::test]
async fn cloning_a_version_the_origin_never_had_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());

    let refused = server_over(dir.path())
        .query("CREATE TABLE staging CLONE entries AT VERSION 99")
        .expect_err("no such version");
    let said = format!("{refused:?}");
    assert!(said.contains("no version 99"), "{said}");
    assert!(said.contains("newest is 2"), "{said}");
}

#[tokio::test]
async fn cloning_a_version_whose_files_are_gone_is_refused() {
    // The case the log bound cannot see: the commit survives and the data does not. Without
    // this check the clone would be an empty table wearing the name of a full one.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    std::fs::remove_file(a_file_of(dir.path(), 1)).expect("retiring it");

    let refused = server_over(dir.path())
        .query("CREATE TABLE staging CLONE entries AT VERSION 1")
        .expect_err("its files are gone");
    assert!(
        format!("{refused:?}").contains("empty table wearing the name"),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_malformed_clone_statement_is_a_syntax_error_rather_than_a_pass() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());

    let refused = server_over(dir.path())
        .query("CREATE TABLE staging CLONE entries AT VERSION yesterday")
        .expect_err("not a version");
    assert!(format!("{refused:?}").contains("yesterday"), "{refused:?}");
}

// Multi-threaded, unlike every test above, and the difference is the point: this is the only
// one whose statement reaches the engine. `query` uses `block_in_place`, which panics on a
// current-thread runtime — so a test that needs this attribute is a test that proves the
// statement got past the pre-filter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ordinary_statement_still_reaches_the_engine() {
    // The interception sits in front of every statement the server receives. If it claimed one
    // it should not, the damage would not be to cloning.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    let server = server_over(dir.path());

    let result = server.query("SELECT 1 AS one").expect("ordinary SQL is untouched");
    assert_eq!(result.rows.len(), 1);

    // And one that is wrong is wrong in the engine's words, not the pre-filter's.
    let refused = server.query("SELECT FROM").expect_err("malformed");
    let said = format!("{refused:?}");
    assert!(!said.contains("CLONE"), "the pre-filter answered for the engine: {said}");
}


#[tokio::test]
async fn a_clone_is_dropped_and_its_directory_goes_with_it() {
    // A clone must be droppable because it is creatable. A thing a statement can make and no
    // statement can remove accumulates, and accumulation with nobody responsible is exactly the
    // shape `RSK-35` describes for rehydrated copies.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    let server = server_over(dir.path());
    server.query("CREATE TABLE staging CLONE entries").expect("cloning");
    assert!(table_root(dir.path(), "staging").join("_delta_log").exists());

    let result = server.query("DROP TABLE staging").expect("dropping it");
    assert_eq!(result.tag, "DROP TABLE");
    assert!(!table_root(dir.path(), "staging").exists(), "and it is gone from disk");
}

// Multi-threaded because one of its statements is handed back to the engine, and `query` uses
// `block_in_place`. Needing the attribute is itself the proof that the hand-back happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_an_origin_a_clone_still_reads_is_refused() {
    // The deletion ADR-0016 exists to prevent, at the one door it can arrive through. Before
    // this the predicate existed and nothing called it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    let server = server_over(dir.path());
    server.query("CREATE TABLE staging CLONE entries").expect("cloning");

    // `entries` is not itself a clone, so the statement is handed back and the server's
    // standing refusal answers it — which is also a refusal, and for a reason that would still
    // hold if cloning did not exist.
    let refused = server.query("DROP TABLE entries").expect_err("refused either way");
    assert!(table_root(dir.path(), "entries").join("_delta_log").exists(), "and it is still there");
    let _ = refused;

    // A clone of a clone is the case where the refusal is this one rather than that one.
    server
        .query("CREATE TABLE scratch CLONE staging")
        .expect("cloning the clone");
    let refused = server
        .query("DROP TABLE staging")
        .expect_err("scratch still reads it");
    let said = format!("{refused:?}");
    assert!(said.contains("scratch"), "the refusal names what would break: {said}");
    assert!(said.contains("materialise"), "{said}");
    assert!(
        table_root(dir.path(), "staging").join("_delta_log").exists(),
        "and nothing was removed"
    );
}

#[tokio::test]
async fn dropping_the_leaf_first_then_its_origin_works() {
    // The way out that the refusal points at. Without this the message would be naming an
    // action that does not work.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());
    let server = server_over(dir.path());
    server.query("CREATE TABLE staging CLONE entries").expect("cloning");
    server.query("CREATE TABLE scratch CLONE staging").expect("cloning again");

    server.query("DROP TABLE scratch").expect("the leaf drops");
    server.query("DROP TABLE staging").expect("and then its origin does");
    assert!(!table_root(dir.path(), "staging").exists());
    assert!(table_root(dir.path(), "entries").join("_delta_log").exists(), "the real table is untouched");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_table_that_is_not_a_clone_is_answered_by_the_server_it_always_was() {
    // The statement is handed back untouched, and the standing refusal answers it. A pre-filter
    // that answered here would have replaced a good refusal with a reimplementation of one.
    let dir = tempfile::tempdir().expect("a temporary directory");
    warehouse_with_entries(dir.path());

    let refused = server_over(dir.path())
        .query("DROP TABLE entries")
        .expect_err("data definition is not served");
    let said = format!("{refused:?}");
    assert!(said.contains("read path over a published warehouse"), "{said}");
    assert!(
        said.contains("sankhya-publish"),
        "and it still names the supported route: {said}"
    );
}

