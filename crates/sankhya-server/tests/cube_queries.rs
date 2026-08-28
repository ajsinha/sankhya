//! A cube, answered by the server, under the policy the caller is subject to.
//!
//! Everything before this made cubes *possible*: an algebra, a SQL surface, a persisted
//! definition, a server that loads one. None of it made a cube **answerable**, and a cube
//! nobody can query is a library rather than a feature.
//!
//! The property that matters is not that a number comes back. It is that **two principals
//! with different entitlements get different numbers**, computed over the rows each may read,
//! with no cube-specific authorization code anywhere — hydration reads the fact table through
//! the session's own `SecuredTable`, so a cube is filtered by exactly the code that filters a
//! plain `SELECT`.
//!
//! That is the whole argument of [ADR-0008](../../../docs/adr/0008-serving-cubes-under-policy.md):
//! a second implementation of the authorization rule would disagree with the first eventually,
//! and the cube is the one that leaks, because it answers with a number rather than with rows
//! somebody might notice were missing.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    // The totals here are small sums of exact decimal literals --- 10 + 20, and 10 + 20 + 30
    // + 40 --- so they are representable and comparison is exact. The lint is right in
    // general and this is the case it is not about.
    clippy::float_cmp
)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_api_pg::session::Handler;
use sankhya_authz::policy::{Action, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Role, TenantId};
use sankhya_cube::catalogue;
use sankhya_cube::model::{Definition, Dimension, Level};
use sankhya_cube_algo::measure::{Along, Measure, Rule as CubeRule};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::Arc;

#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/warehouse.rs"]
mod warehouse;
#[path = "../src/wiring.rs"]
mod wiring;

use wiring::{Server, Settings};

fn tenant() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::from_u128(1))
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("amount", DataType::Float64, false),
    ]))
}

/// Four rows, two regions, so a region filter has something to exclude.
///
/// North totals 30, south totals 70, everything totals 100 — three numbers far enough apart
/// that a test asserting one cannot pass by accident on another.
fn warehouse_with_a_fact_table() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("sales").join("orders");

    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(vec![1_i64, 2, 3, 4])),
            Arc::new(StringArray::from(vec!["north", "north", "south", "south"])),
            Arc::new(Float64Array::from(vec![10.0, 20.0, 30.0, 40.0])),
        ],
    )
    .expect("a valid batch");

    let publication = Publication::external(&root, "orders");
    publication.create(&schema()).expect("creating");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(4))
        .expect("publishing");

    catalogue::save(dir.path(), &sales()).expect("declaring the cube");
    dir
}

/// A cube over the fact table: one dimension, one additive measure.
fn sales() -> Definition {
    Definition::new(
        "sales",
        "orders",
        vec![Dimension {
            name: "region".to_string(),
            table: "orders".to_string(),
            joins_on: "region".to_string(),
            // Two levels, so their *order* is observable. With one level every ordering is
            // the same ordering, and a test over it cannot tell a hierarchy from a list.
            levels: vec![Level::new("country", "region"), Level::new("area", "region")],
            rollups: None,
            parent_child: None,
        }],
        vec![
            Measure::new("amount", vec![Along::new("region", CubeRule::Sum)]),
            // A measure that does *not* compose, so composability is observable. A ratio
            // cannot be derived from its parts: there is no operation over the pieces that
            // yields the whole, which is exactly what a client must be told before it offers
            // to roll one up.
            Measure::new("ratio", vec![Along::new("region", CubeRule::None)]),
        ],
    )
}

fn settings(warehouse: &std::path::Path) -> Settings {
    Settings {
        listen: "127.0.0.1:0".to_string(),
        warehouse: warehouse.to_path_buf(),
        read_as_of: Lsn::new(u64::MAX),
        tenant: tenant(),
        maintenance: None,
        require_password: false,
        metrics_listen: None,
    }
}

/// A server over the fixture, with the given policy, having adopted its cubes.
fn server_with(policy: PolicySet) -> (Server, tempfile::TempDir) {
    let dir = warehouse_with_a_fact_table();
    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture must open: {refused:?}");

    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "the fixture must read: {unreadable:?}");

    let tables = warehouse::describe(&found);
    let (server, complaints) =
        Server::with_tables(settings(dir.path()), policy, tables, servable)
            .adopting_cubes(dir.path());
    assert!(complaints.is_empty(), "the cube must adopt: {complaints:?}");
    assert_eq!(server.cubes().len(), 1);
    (server, dir)
}

/// A policy letting `role` read orders, optionally only some rows.
fn policy(role: &str, rows: Option<&str>) -> PolicySet {
    let mut rule = Rule::grant(
        tenant(),
        Role::new(role),
        TableRef::new("sales", "orders"),
        Action::Read,
    );
    if let Some(rows) = rows {
        rule = rule.where_rows(rows);
    }
    PolicySet::new().with(rule)
}

fn connect(server: &Server, user: &str) {
    server
        .authenticate(&[("user".to_string(), user.to_string())], Some(b"x"))
        .expect("authenticated");
}

/// The `amount` column of a rolled-up cube, as a total.
fn total_from(result: &sankhya_api_pg::session::QueryResult) -> f64 {
    let column = result
        .fields
        .iter()
        .position(|field| field.name == "amount")
        .expect("the measure is a column");
    result
        .rows
        .iter()
        .filter_map(|row| row[column].as_deref())
        .filter_map(|cell| cell.parse::<f64>().ok())
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cube_is_answerable_over_the_wire() {
    // The thing none of the machinery before this could do.
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let result = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')")
        .expect("the cube answers");

    assert_eq!(
        total_from(&result),
        100.0,
        "an unrestricted principal sees every row's contribution: {:?}",
        result.rows
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restricted_principal_gets_a_total_over_the_rows_they_may_read() {
    // The property the whole design exists for. This total is *legitimately* different from
    // the one above — and there is no cube-specific authorization code producing it, because
    // hydration reads the fact table through the same `SecuredTable` a plain SELECT does.
    let (server, _dir) = server_with(policy("reader", Some("region = 'north'")));
    connect(&server, "ana");

    let result = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')")
        .expect("the cube answers");

    assert_eq!(
        total_from(&result),
        30.0,
        "north is 10 + 20; seeing 100 would mean the cube answered over rows this principal \
         may not read, which is a disclosure with nothing in the result to notice: {:?}",
        result.rows
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_answer_carries_how_much_of_the_table_it_saw() {
    // What SSAS makes an operator choose between — a true total or a visible one — this
    // carries in the result. A policy-filtered total is distinguishable by looking at it
    // rather than by knowing which role you were in.
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let result = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')")
        .expect("the cube answers");

    assert!(
        result.fields.iter().any(|field| field.name == "completeness"),
        "every cube answer states how much of its input it saw: {:?}",
        result.fields
    );
    assert!(
        result.fields.iter().any(|field| field.name == "snapshot"),
        "and which snapshot it was computed at, so it can be reconciled with a relational \
         figure taken at another moment: {:?}",
        result.fields
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_principal_who_may_not_read_the_fact_table_cannot_reach_the_cube() {
    // Not registered rather than refused, which is the same answer a table gets and for the
    // same reason: saying "you may not read that" confirms it exists.
    //
    // Every connection is given the `reader` role, so granting to another role grants to
    // nobody who can connect.
    let (server, _dir) = server_with(policy("someone-else", None));
    connect(&server, "ana");

    let refused = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')")
        .expect_err("a principal with no grant reaches nothing");
    assert!(
        !format!("{}", refused.message).is_empty(),
        "the refusal says something"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_statement_that_names_no_cube_does_not_hydrate_one() {
    // Hydration reads the fact table, so doing it for every statement would make every query
    // pay the cost the cube exists to avoid. A plain SELECT must not touch the cube path.
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let result = server
        .query("SELECT COUNT(*) FROM orders")
        .expect("a plain query still works");
    assert_eq!(result.rows.len(), 1);
}

// --- description: what a client needs before it can ask anything -------------

/// The first column of every row, as text.
fn first_column(result: &sankhya_api_pg::session::QueryResult, name: &str) -> Vec<String> {
    let at = result
        .fields
        .iter()
        .position(|field| field.name == name)
        .unwrap_or_else(|| panic!("no column named {name}: {:?}", result.fields));
    result
        .rows
        .iter()
        .filter_map(|row| row[at].clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_can_discover_what_cubes_exist() {
    // Without this the only way to offer a picker is to hardcode the model, and a hardcoded
    // model drifts from the cube it describes with nothing to notice.
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let result = server.query("SELECT * FROM cubes()").expect("cubes are listable");
    assert_eq!(first_column(&result, "cube"), vec!["sales".to_string()]);
    assert_eq!(first_column(&result, "fact_table"), vec!["orders".to_string()]);
    assert_eq!(first_column(&result, "measures"), vec!["2".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_can_discover_a_cube_s_dimensions_and_their_order() {
    // Levels are ordered coarse to fine and that order is a fact about the model rather than
    // about how rows arrived, so it is a column. A client that sorted the result and drew the
    // hierarchy alphabetically would draw the wrong hierarchy.
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let result = server
        .query("SELECT * FROM cube_dimensions('sales')")
        .expect("dimensions are listable");
    assert_eq!(
        first_column(&result, "dimension"),
        vec!["region".to_string(), "region".to_string()]
    );
    assert_eq!(
        first_column(&result, "level"),
        vec!["country".to_string(), "area".to_string()],
        "coarse to fine, as declared"
    );
    assert_eq!(
        first_column(&result, "depth"),
        vec!["0".to_string(), "1".to_string()],
        "the depth is what tells a client which level is coarser, and it must not be \
         constant --- a client drawing a hierarchy from a constant draws a list"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_can_discover_which_roll_ups_are_even_legal() {
    // A UI offering "roll up by time" on a non-composing measure offers a button that cannot
    // work. Finding that out at query time is worse than not offering it, so composability is
    // part of the description rather than something to discover by failing.
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let result = server
        .query("SELECT * FROM cube_measures('sales')")
        .expect("measures are listable");
    assert_eq!(
        first_column(&result, "measure"),
        vec!["amount".to_string(), "ratio".to_string()]
    );
    assert_eq!(
        first_column(&result, "rule"),
        vec!["sum".to_string(), "none".to_string()]
    );
    // `t`, because the wire renders booleans the way PostgreSQL does and a client parsing
    // this is a PostgreSQL client.
    assert_eq!(
        first_column(&result, "composes"),
        vec!["t".to_string(), "f".to_string()],
        "a sum composes and a ratio does not, and a client offering to roll up the second \
         offers a button that cannot work"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn describing_a_cube_that_does_not_exist_names_the_ones_that_do() {
    // A typo and an unserved cube are the same experience otherwise, and the first is one
    // glance from being fixed.
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let refused = server
        .query("SELECT * FROM cube_dimensions('sails')")
        .expect_err("a misspelt cube is refused");
    assert!(
        refused.message.contains("sales"),
        "the refusal names what does exist: {}",
        refused.message
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn describing_a_cube_does_not_read_its_fact_table() {
    // A picker that costs a hydration per keystroke is a picker nobody leaves switched on.
    // Asserted through the description functions being available without the statement
    // naming a navigation function at all --- hydration is gated on that, description is not.
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let result = server
        .query("SELECT cube FROM cubes()")
        .expect("listing needs no cells");
    assert_eq!(result.rows.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_snapshot_a_cube_reports_is_the_table_s_version() {
    // The test that catches a stale cache key, and whose absence let a real defect ship.
    //
    // Hydration is cached across statements — it has to be, or every query pays the fact
    // table read the cube exists to avoid. The cache treats a new snapshot as a miss and has
    // a unit test saying so. But the *caller* passed `settings.read_as_of`, which defaults to
    // `u64::MAX` and is read once at startup: the snapshot in the key never moved, so the
    // first hydration would have been served for the life of the process.
    //
    // A guard that is correct and never reached is the shape of defect this warehouse keeps
    // finding, and every unit involved was behaving perfectly. What catches it is asserting
    // the *value*: the snapshot a cube reports must be the version its table actually stands
    // at, not a sentinel meaning "everything".
    let (server, _dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let result = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')")
        .expect("the cube answers");

    let snapshots = first_column(&result, "snapshot");
    assert!(!snapshots.is_empty(), "the answer has rows");
    for snapshot in &snapshots {
        assert_ne!(
            snapshot,
            &u64::MAX.to_string(),
            "`u64::MAX` is `read_as_of`'s default meaning \"everything published\". Reported \
             as the snapshot it makes the cache key a constant, so the first hydration is \
             served forever and a cube silently stops tracking its table"
        );
        let version: u64 = snapshot.parse().expect("a numeric snapshot");
        assert!(
            version <= 8,
            "the fixture commits twice, so the version is small and real: {version}"
        );
    }
}
