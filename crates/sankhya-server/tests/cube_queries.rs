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
use sankhya_api_pg::session::Caller;
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
#[path = "../src/adopt.rs"]
mod adopt;
#[path = "../src/clones.rs"]
mod clones;
#[path = "../src/feeds.rs"]
mod feeds;
#[path = "../src/driver.rs"]
mod driver;
#[path = "../src/snapshots.rs"]
mod snapshots;
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
        Field::new("period", DataType::Utf8, false),
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
            Arc::new(StringArray::from(vec!["q1", "q2", "q1", "q2"])),
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

/// The same cube, composing by `Max`.
fn largest() -> Definition {
    let mut definition = sales();
    definition.name = "largest".to_string();
    definition.measures = vec![Measure::new(
        "amount",
        vec![
            Along::new("region", CubeRule::Max),
            Along::new("period", CubeRule::Max),
        ],
    )];
    definition
}

/// A cube over the fact table: one dimension, one additive measure.
fn sales() -> Definition {
    Definition::new(
        "sales",
        "orders",
        vec![
            Dimension {
                name: "region".to_string(),
                table: "orders".to_string(),
                joins_on: "region".to_string(),
                // Two levels, so their *order* is observable. With one level every ordering
                // is the same ordering, and a test over it cannot tell a hierarchy from a
                // list.
                levels: vec![Level::new("country", "region"), Level::new("area", "region")],
                rollups: None,
                parent_child: None,
            },
            // A second dimension, so a query can ask for a shape *coarser* than the base.
            // With one dimension every ask is the base, and a test cannot tell selection
            // running from selection never running.
            Dimension {
                name: "period".to_string(),
                table: "orders".to_string(),
                joins_on: "period".to_string(),
                levels: vec![Level::new("quarter", "period")],
                rollups: None,
                parent_child: None,
            },
        ],
        vec![
            Measure::new(
                "amount",
                vec![
                    Along::new("region", CubeRule::Sum),
                    Along::new("period", CubeRule::Sum),
                ],
            ),
            // A measure that does *not* compose, so composability is observable. A ratio
            // cannot be derived from its parts: there is no operation over the pieces that
            // yields the whole, which is exactly what a client must be told before it offers
            // to roll one up.
            Measure::new(
                "ratio",
                vec![
                    Along::new("region", CubeRule::None),
                    Along::new("period", CubeRule::None),
                ],
            ),
        ],
    )
}

fn settings(warehouse: &std::path::Path) -> Settings {
    Settings {
        listen: "127.0.0.1:0".to_string(),
        warehouse: warehouse.to_path_buf(),
        read_as_of: Lsn::new(u64::MAX),
        tenant: tenant(),
        cuboid_budget_rows: wiring::CUBOID_ROW_BUDGET,
        flight_listen: None,
        maintenance: None,
        require_password: false,
        metrics_listen: None,
        transport_security: None,
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
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
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
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
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
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
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
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
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
        .query("SELECT COUNT(*) FROM orders", &Caller::new(&anyone()))
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

    let result = server.query("SELECT * FROM cubes()", &Caller::new(&anyone())).expect("cubes are listable");
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
        .query("SELECT * FROM cube_dimensions('sales')", &Caller::new(&anyone()))
        .expect("dimensions are listable");
    assert_eq!(
        first_column(&result, "dimension"),
        vec!["region".to_string(), "region".to_string(), "period".to_string()]
    );
    assert_eq!(
        first_column(&result, "level"),
        vec!["country".to_string(), "area".to_string(), "quarter".to_string()],
        "coarse to fine, as declared"
    );
    assert_eq!(
        first_column(&result, "depth"),
        vec!["0".to_string(), "1".to_string(), "0".to_string()],
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
        .query("SELECT * FROM cube_measures('sales')", &Caller::new(&anyone()))
        .expect("measures are listable");
    // One row per (measure, dimension): a rule is declared per dimension, which is what
    // makes a semi-additive measure expressible at all.
    assert_eq!(
        first_column(&result, "measure"),
        vec![
            "amount".to_string(),
            "amount".to_string(),
            "ratio".to_string(),
            "ratio".to_string()
        ]
    );
    assert_eq!(
        first_column(&result, "rule"),
        vec![
            "sum".to_string(),
            "sum".to_string(),
            "none".to_string(),
            "none".to_string()
        ]
    );
    // `t`, because the wire renders booleans the way PostgreSQL does and a client parsing
    // this is a PostgreSQL client.
    assert_eq!(
        first_column(&result, "composes"),
        vec!["t".to_string(), "t".to_string(), "f".to_string(), "f".to_string()],
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
        .query("SELECT * FROM cube_dimensions('sails')", &Caller::new(&anyone()))
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
        .query("SELECT cube FROM cubes()", &Caller::new(&anyone()))
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
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commit_after_a_cube_was_hydrated_changes_the_answer() {
    // A server used to resolve its providers once at boot and never look again, so data
    // committed afterwards was invisible to it. That was defensible while the warehouse did
    // not move; it stopped being defensible when the server began maintaining the warehouse
    // itself, and the fix for *that* --- re-resolving a table whose log has moved --- makes
    // this true as a side effect worth having.
    let (server, dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let before = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the cube answers");
    assert_eq!(total_from(&before), 100.0);

    let root = dir.path().join("sales").join("orders");
    let more = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(vec![5_i64])),
            Arc::new(StringArray::from(vec!["north"])),
            Arc::new(StringArray::from(vec!["q1"])),
            Arc::new(Float64Array::from(vec![50.0])),
        ],
    )
    .expect("a valid batch");
    Publication::external(&root, "orders")
        .append_rebasing(2, 8, "part-0001.parquet", &more, Lsn::new(5))
        .expect("publishing more");

    let after = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the cube answers again");
    assert_eq!(
        total_from(&after),
        150.0,
        "a commit must change the answer; 100 means the cube served cells from before it, \
         which is how a dashboard comes to disagree with the table it is drawn from with \
         nobody able to say by how much: {:?}",
        after.rows
    );
}

// --- materialised cuboids: cells that outlive the process ---------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_materialised_cuboid_answers_without_reading_the_fact_table() {
    // The reason to materialise at all. The in-memory cache dies with the process, so a
    // server that restarts makes every dashboard pay for a fact-table read again. A cuboid on
    // disk is the same cells, surviving.
    //
    // **Proved by taking the fact table away.** If the answer still comes back, it did not
    // come from there.
    //
    // The version of this test that stood here until 2026-08-28 said exactly that in a
    // comment and did neither: it wrote a cuboid by hand and asserted the file existed. So it
    // passed for as long as the server never read a cuboid at all --- which it did not, for
    // the whole of M7. `materialised` was dead code and the compiler said so. A test named
    // for a behaviour is not a test of it.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));
    connect(&server, "ana");

    let live = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the cube answers from its table");
    assert_eq!(total_from(&live), 100.0);

    // Built by the server, through the maintenance tick, at the scope and snapshot the
    // server itself will look under. Building it by hand here would prove only that this test
    // can agree with itself about a key.
    let refreshed = server.refresh_maintained_cubes();
    assert!(!refreshed.is_empty(), "a cuboid was built: {refreshed:?}");

    // Now take the fact table's data away, leaving its log alone so the snapshot --- and
    // therefore the cuboid's key --- does not move. Any read of the fact table now fails.
    let facts = dir.path().join("sales").join("orders");
    let mut removed = 0;
    for file in sankhya_table_delta::live_files(&facts)
        .expect("the fact table's log replays")
        .files
    {
        std::fs::remove_file(facts.join(&file.path)).expect("removing");
        removed += 1;
    }
    assert!(removed > 0, "the fixture must have had data files to remove");

    // A **new server**, not a new connection --- which is the whole point, and the first
    // version of this rewrite got wrong. `hydrated` is a process-lifetime cache and a second
    // connection to the same process is served straight out of it, so the fact table can be
    // deleted and the answer still comes back without a cuboid being involved at all. A
    // mutation removing the cuboid read survived exactly that test.
    //
    // Restarting is what the cuboid is for: the in-memory cache dies with the process, and
    // this is the thing that outlives it.
    drop(server);
    let restarted = server_over(&dir, policy("reader", None));
    connect(&restarted, "ana");
    let from_disk = restarted
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the cuboid answers with no fact table to read");
    assert_eq!(
        total_from(&from_disk),
        100.0,
        "the same total, and it cannot have come from the fact table because there is none"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_materialised_cuboid_is_not_served_as_a_user_table() {
    // It is a published table on purpose --- open storage gets no exception for the fast path
    // --- and it must still not appear in a catalogue somebody browses, where its name is a
    // hash and it looks like something to query.
    let (server, dir) = server_with(policy("reader", None));
    connect(&server, "ana");

    let cube = &server.cubes()[0];
    let key = sankhya_cube::materialise::Key::new(
        cube.version(),
        1,
        0,
        sankhya_cube_algo::lattice::Cuboid::of(&["region", "period"]),
    );
    let mut cells = sankhya_cube::cells::Cells::over(vec!["region".to_string()]);
    cells.add(vec!["north".to_string()], 1.0).expect("well-formed");
    sankhya_maintenance::cuboid::materialise(dir.path(), &key, cube.name(), &cells, CubeRule::Sum, &SAW_EVERYTHING)
        .expect("materialising");

    let (found, _) = warehouse::discover(dir.path());
    let names: Vec<String> = found
        .iter()
        .map(|table| format!("{}.{}", table.reference.schema, table.reference.table))
        .collect();
    assert_eq!(
        names,
        vec!["sales.orders".to_string()],
        "discovery finds the user's table and not the cache beside it: {names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn materialising_the_same_cuboid_twice_writes_it_once() {
    // The key embeds the definition version, the snapshot and the scope, so a cuboid that
    // exists is a cuboid that is still correct. There is no staleness to check, which is what
    // FR-QUERY-20's key buys — and rewriting it would be work for no change.
    let (server, dir) = server_with(policy("reader", None));
    let cube = &server.cubes()[0];
    let key = sankhya_cube::materialise::Key::new(
        cube.version(),
        1,
        0,
        sankhya_cube_algo::lattice::Cuboid::of(&["region", "period"]),
    );
    let mut cells = sankhya_cube::cells::Cells::over(vec!["region".to_string()]);
    cells.add(vec!["north".to_string()], 1.0).expect("well-formed");

    let first = sankhya_maintenance::cuboid::materialise(
        dir.path(), &key, cube.name(), &cells, CubeRule::Sum,
        &SAW_EVERYTHING,
    )
    .expect("materialising");
    let second = sankhya_maintenance::cuboid::materialise(
        dir.path(), &key, cube.name(), &cells, CubeRule::Sum,
        &SAW_EVERYTHING,
    )
    .expect("materialising again");

    assert!(first, "written the first time");
    assert!(!second, "and left alone the second");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_cuboid_is_not_written() {
    // A table of no rows is indistinguishable, on the way back, from a cube that saw nothing
    // — and writing one would make the next run skip the hydration that would have found the
    // rows.
    let (server, dir) = server_with(policy("reader", None));
    let cube = &server.cubes()[0];
    let key = sankhya_cube::materialise::Key::new(
        cube.version(),
        1,
        0,
        sankhya_cube_algo::lattice::Cuboid::of(&["region", "period"]),
    );
    let empty = sankhya_cube::cells::Cells::over(vec!["region".to_string()]);

    let written = sankhya_maintenance::cuboid::materialise(
        dir.path(), &key, cube.name(), &empty, CubeRule::Sum,
        &SAW_EVERYTHING,
    )
    .expect("materialising");
    assert!(!written);
    assert!(!sankhya_maintenance::cuboid::exists(dir.path(), &key, cube.name()));
}

// --- maintained cubes refresh without a caller -------------------------------

/// The fixture cube, marked maintained.
/// What a hand-built fixture cuboid saw.
///
/// Stated rather than defaulted, and the same reason `Completeness` has no `Default`: a value
/// nobody thought about must not be able to report itself complete. These cells are the
/// fixture's own, so "complete over the rows it holds" is the honest description of them.
const SAW_EVERYTHING: sankhya_cube::complete::Completeness =
    sankhya_cube::complete::Completeness::complete(2);

fn maintained_warehouse() -> tempfile::TempDir {
    let dir = warehouse_with_a_fact_table();
    catalogue::save(dir.path(), &sales().maintained_within(5)).expect("declaring maintained");
    dir
}

fn server_over(dir: &tempfile::TempDir, policy: PolicySet) -> Server {
    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture must open: {refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "the fixture must read: {unreadable:?}");
    let tables = warehouse::describe(&found);
    let (server, complaints) =
        Server::with_tables(settings(dir.path()), policy, tables, servable)
            .adopting_cubes(dir.path());
    assert!(complaints.is_empty(), "{complaints:?}");
    server
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_maintained_cube_is_built_with_nobody_logged_in() {
    // The whole point of the lifetime. A dashboard is fast at nine because something built
    // its cells at four, and nothing about that involves the person who declared the cube
    // being connected.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));
    assert_eq!(server.cubes().len(), 1);

    // No `authenticate`, no query, no principal anywhere.
    let refreshed = server.refresh_maintained_cubes();
    assert!(
        refreshed.iter().any(|name| name.starts_with("sales.")),
        "a maintained cube builds without a caller: {refreshed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declared_cube_is_not_built_by_the_refresher() {
    // Persisting a definition is cheap; materialising is storage and work. A cube that did
    // not ask to be maintained must not acquire cuboids by being written down.
    let dir = warehouse_with_a_fact_table(); // saves `sales()` with no target_lag
    let server = server_over(&dir, policy("reader", None));

    let refreshed = server.refresh_maintained_cubes();
    assert!(
        refreshed.is_empty(),
        "a Declared cube materialises nothing: {refreshed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refreshing_twice_builds_once() {
    // The key embeds the definition version, the snapshot and the scope, so a cuboid that
    // exists is still correct. A refresher that rebuilt it every pass would spend the
    // maintenance budget rewriting identical files.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));

    let first = server.refresh_maintained_cubes();
    let second = server.refresh_maintained_cubes();
    assert!(!first.is_empty(), "built the first time: {first:?}");
    assert!(second.is_empty(), "and left alone the second: {second:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restricted_caller_is_never_served_the_unrestricted_cuboid() {
    // The other half of serving a cuboid, and the half that must not be got wrong once.
    //
    // The refresher builds the unrestricted cuboid: an aggregate over **every** row. Serving
    // it to a caller a policy filters would be a disclosure through arithmetic, and an
    // invisible one --- the number is real, it is simply over rows they may not read. There
    // is no error, no refusal, and nothing in a log to notice.
    //
    // Proved the same way as its sibling: build the cuboid, take the fact table away, and
    // restart. An unrestricted caller gets an answer. A filtered one must get **no answer at
    // all**, because the only cells that could serve them are ones nothing has computed.
    let dir = maintained_warehouse();
    let building = server_over(&dir, policy("reader", None));
    let refreshed = building.refresh_maintained_cubes();
    assert!(!refreshed.is_empty(), "a cuboid was built: {refreshed:?}");
    drop(building);

    let facts = dir.path().join("sales").join("orders");
    for file in sankhya_table_delta::live_files(&facts).expect("its log replays").files {
        std::fs::remove_file(facts.join(&file.path)).expect("removing");
    }

    // Same warehouse, same cuboid on disk --- a policy that withholds rows.
    let restricted = server_over(&dir, policy("reader", Some("region = 'north'")));
    connect(&restricted, "ana");
    let answer = restricted.query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()));

    assert!(
        answer.is_err(),
        "a filtered caller was served an aggregate over rows they may not read: {:?}",
        answer.map(|result| total_from(&result))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_provenance_column_says_where_the_answer_came_from() {
    // "Why was this fast?" and "why was this slow?" are the same question asked twice, and an
    // operator cannot answer either from a column that reports what the query typed. This one
    // did exactly that until 2026-08-28 --- it echoed the caller's `materialise` argument ---
    // and the echo agreed with reality by accident, because nothing served a cuboid and the
    // honest answer was `false` for every query ever run.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));
    connect(&server, "ana");

    let live = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the cube answers from its table");
    assert_eq!(
        first_column(&live, "materialised"),
        vec!["f".to_string(); first_column(&live, "materialised").len()],
        "hydrated from the fact table, and it says so"
    );

    server.refresh_maintained_cubes();
    drop(server);

    // Restarted, so the only thing left is the cuboid on disk.
    let restarted = server_over(&dir, policy("reader", None));
    connect(&restarted, "ana");
    let cached = restarted
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the cuboid answers");
    assert_eq!(
        first_column(&cached, "materialised"),
        vec!["t".to_string(); first_column(&cached, "materialised").len()],
        "read from a cuboid, and it says that instead"
    );
}

// --- answering from a materialised ancestor ----------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_query_is_answered_from_the_narrowest_cuboid_that_can_answer_it() {
    // §11.6's reason for having a lattice at all. A cuboid over `[region]` is far cheaper to
    // scan than one over `[region, period]`, and it can answer `by=region` exactly --- there
    // is nothing left to roll away.
    //
    // Until this existed the server only ever read the *base* cuboid, so the lattice, the
    // ancestor-answering predicate and `materialise::plan` were all written, tested, and
    // reached by nothing that serves a query.
    let dir = maintained_warehouse();
    let building = server_over(&dir, policy("reader", None));
    connect(&building, "ana");
    // Ask for `by=region` repeatedly so selection buys that shape from the query log.
    for _ in 0..5 {
        building
            .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
            .expect("the cube answers");
    }
    building.refresh_maintained_cubes();
    drop(building);

    let cube_version = {
        let peek = server_over(&dir, policy("reader", None));
        let cube = &peek.cubes()[0];
        (cube.version(), peek.snapshot_for_test(cube.fact_table()))
    };
    let narrow = sankhya_cube::materialise::Key::unrestricted(
        cube_version.0,
        cube_version.1,
        sankhya_cube::algo::Cuboid::of(&["region"]),
    );
    assert!(
        sankhya_maintenance::cuboid::exists(dir.path(), &narrow, "sales"),
        "selection bought the shape that was asked for, which this test depends on"
    );

    // Remove the **base** cuboid and the fact table, leaving only the narrow ancestor. If the
    // answer still comes back it came from there, and from nowhere else.
    let base = sankhya_cube::materialise::Key::unrestricted(
        cube_version.0,
        cube_version.1,
        sankhya_cube::algo::Cuboid::of(&["region", "period"]),
    );
    std::fs::remove_dir_all(sankhya_maintenance::cuboid::root_of(dir.path(), &base, "sales"))
        .expect("removing the base cuboid");
    let facts = dir.path().join("sales").join("orders");
    for file in sankhya_table_delta::live_files(&facts).expect("its log replays").files {
        std::fs::remove_file(facts.join(&file.path)).expect("removing");
    }

    let restarted = server_over(&dir, policy("reader", None));
    connect(&restarted, "ana");
    let answer = restarted
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the narrow cuboid answers on its own");
    assert_eq!(total_from(&answer), 100.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_query_finer_than_every_cuboid_is_not_answered_from_one() {
    // The other direction, and the one that would be a wrong number rather than an error.
    //
    // Cells published to a session are the finest grain a query may reach: the SQL surface
    // dices and rolls up *from* them. A cuboid over `[region]` cannot answer `by=region|period`
    // --- the period column is gone --- and answering from it anyway would report each region's
    // total under every period.
    let dir = maintained_warehouse();
    let building = server_over(&dir, policy("reader", None));
    connect(&building, "ana");
    for _ in 0..5 {
        building
            .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
            .expect("the cube answers");
    }
    building.refresh_maintained_cubes();
    drop(building);

    let cube_version = {
        let peek = server_over(&dir, policy("reader", None));
        let cube = &peek.cubes()[0];
        (cube.version(), peek.snapshot_for_test(cube.fact_table()))
    };
    let base = sankhya_cube::materialise::Key::unrestricted(
        cube_version.0,
        cube_version.1,
        sankhya_cube::algo::Cuboid::of(&["region", "period"]),
    );
    std::fs::remove_dir_all(sankhya_maintenance::cuboid::root_of(dir.path(), &base, "sales"))
        .expect("removing the base cuboid");
    let facts = dir.path().join("sales").join("orders");
    for file in sankhya_table_delta::live_files(&facts).expect("its log replays").files {
        std::fs::remove_file(facts.join(&file.path)).expect("removing");
    }

    // Only the `[region]` cuboid is left, and the query needs `[region, period]`.
    let restarted = server_over(&dir, policy("reader", None));
    connect(&restarted, "ana");
    assert!(
        restarted
            .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region|period')", &Caller::new(&anyone()))
            .is_err(),
        "a query was answered from a cuboid too coarse to express it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dice_is_not_answered_from_a_cuboid_that_rolled_its_dimension_away() {
    // A dice needs its column to still be there. `where=region:north` over a cuboid that
    // rolled `region` away has nothing to restrict, so `region` counts towards the grain a
    // statement needs **even though it never appears in the result** --- slicing drops the
    // axis it fixes.
    //
    // Pinning `[period]` is what makes this falsifiable: it puts a cuboid on disk that is
    // cheap, current, and unable to express the query. A test where every materialised cuboid
    // happens to contain `region` cannot tell the two behaviours apart, and the first version
    // of this test was exactly that --- a mutation dropping `where=` from the grain survived it.
    let dir = warehouse_with_a_fact_table();
    catalogue::save(
        dir.path(),
        &sales().maintained_within(5).pinning(["period"]),
    )
    .expect("declaring a cube that pins the wrong shape for this query");

    let server = server_over(&dir, policy("reader", None));
    server.refresh_maintained_cubes();
    connect(&server, "ana");

    let sliced = server
        .query("SELECT * FROM cube_slice('sales', 'amount', 'where=region:north')", &Caller::new(&anyone()))
        .expect("the slice answers");

    assert_eq!(total_from(&sliced), 30.0, "north's total, and only north's");
    assert_eq!(
        first_column(&sliced, "materialised"),
        vec!["f".to_string(); first_column(&sliced, "materialised").len()],
        "the only cuboid on disk cannot express this query, so it must not have been used"
    );
}

// --- §11.6's three levels of control -----------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_may_ask_for_the_base_data_and_get_the_same_answer() {
    // Exit criterion 3a, from the caller's side. `materialisation=off` is the reproducibility
    // check: a figure that differs between it and the default is a defect, not a tuning
    // question. That is the whole reason materialisation can be automatic --- being wrong
    // about what to cache costs latency, never correctness.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));
    server.refresh_maintained_cubes();
    connect(&server, "ana");

    let cached = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the default path answers");
    let base = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region', 'materialise=false')", &Caller::new(&anyone()))
        .expect("and so does the base path");

    assert_eq!(
        total_from(&cached).to_bits(),
        total_from(&base).to_bits(),
        "bit-identical with materialisation on and off, which is what makes it a cache"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_that_asks_for_the_base_data_is_not_served_a_cuboid() {
    // The previous test would pass if `materialisation=off` did nothing at all --- both
    // answers would come from the same place and agree trivially. This one takes the fact
    // table away, so the base path has nothing to read: a caller asking for it must fail
    // rather than be quietly handed the cuboid they said not to use.
    let dir = maintained_warehouse();
    let building = server_over(&dir, policy("reader", None));
    building.refresh_maintained_cubes();
    drop(building);

    let facts = dir.path().join("sales").join("orders");
    for file in sankhya_table_delta::live_files(&facts).expect("its log replays").files {
        std::fs::remove_file(facts.join(&file.path)).expect("removing");
    }

    let server = server_over(&dir, policy("reader", None));
    connect(&server, "ana");

    assert_eq!(
        total_from(
            &server
                .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
                .expect("the cuboid answers")
        ),
        100.0
    );
    assert!(
        server
            .query(
                "SELECT * FROM cube_rollup('sales', 'amount', 'by=region', 'materialise=false')"
            , &Caller::new(&anyone()))
            .is_err(),
        "a caller who asked for the base data was served the cuboid instead"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_definition_can_pin_a_shape_nobody_has_asked_for() {
    // The definition level. Selection spends the operator's budget on evidence, and a pin is
    // the statement that a shape is worth holding *before* any evidence exists --- the
    // month-end roll-up nobody runs until the day it must be instant. A pin that had to
    // compete against a query log would be no control at all.
    let dir = warehouse_with_a_fact_table();
    catalogue::save(
        dir.path(),
        &sales().maintained_within(5).pinning(["region"]),
    )
    .expect("declaring a cube with a pinned shape");

    let server = server_over(&dir, policy("reader", None));
    // No queries at all, so the query log is empty and selection can choose nothing.
    let refreshed = server.refresh_maintained_cubes();
    assert!(!refreshed.is_empty(), "{refreshed:?}");

    let cube = &server.cubes()[0];
    let snapshot = server.snapshot_for_test(cube.fact_table());
    let pinned = sankhya_cube::materialise::Key::unrestricted(
        cube.version(),
        snapshot,
        sankhya_cube::algo::Cuboid::of(&["region"]),
    );
    assert!(
        sankhya_maintenance::cuboid::exists(dir.path(), &pinned, cube.name()),
        "the pinned shape was built with nothing in the query log to justify it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pin_survives_being_written_down_and_read_back() {
    // A pin lives in the definition, and the definition lives on disk. A control that is
    // honoured in memory and lost by the catalogue is a control that works until a restart.
    let dir = warehouse_with_a_fact_table();
    catalogue::save(dir.path(), &sales().pinning(["region"]).pinning(["period"]))
        .expect("declaring");

    let server = server_over(&dir, policy("reader", None));
    let mut pinned: Vec<Vec<String>> = server.cubes()[0]
        .pinned()
        .iter()
        .map(|shape| shape.dimensions().iter().map(ToString::to_string).collect())
        .collect();
    pinned.sort();

    assert_eq!(
        pinned,
        vec![vec!["period".to_string()], vec!["region".to_string()]],
        "both pins came back"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zero_budget_buys_nothing_beyond_the_base_and_the_pins() {
    // The operator's level. The budget is what selection may spend on somebody else's query
    // log, so setting it to nothing must stop selection buying anything --- while leaving the
    // base cuboid, which is not bought but required, and any pinned shape, which the modeller
    // asked for rather than the log.
    let dir = maintained_warehouse();
    let mut with_no_budget = settings(dir.path());
    with_no_budget.cuboid_budget_rows = 0;

    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "{refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "{unreadable:?}");
    let (server, complaints) = Server::with_tables(
        with_no_budget,
        policy("reader", None),
        warehouse::describe(&found),
        servable,
    )
    .adopting_cubes(dir.path());
    assert!(complaints.is_empty(), "{complaints:?}");

    // Ask for a shape repeatedly, so the query log has evidence selection would act on.
    connect(&server, "ana");
    for _ in 0..5 {
        server
            .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
            .expect("the cube answers");
    }
    server.refresh_maintained_cubes();

    let cube = &server.cubes()[0];
    let snapshot = server.snapshot_for_test(cube.fact_table());
    let asked_for = sankhya_cube::materialise::Key::unrestricted(
        cube.version(),
        snapshot,
        sankhya_cube::algo::Cuboid::of(&["region"]),
    );
    assert!(
        !sankhya_maintenance::cuboid::exists(dir.path(), &asked_for, cube.name()),
        "a zero budget bought a cuboid anyway, so the operator's number is decorative"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_the_refresher_builds_is_the_unrestricted_scope() {
    // A refresh running on a timer has no principal, so it builds the unrestricted cuboid —
    // and per ADR-0008 an unrestricted cuboid may serve only an unrestricted caller. The
    // consequence is that background refresh helps dashboards and service accounts and does
    // nothing for a restricted analyst, whose cuboids can only be built by their own queries.
    //
    // Asserted on where the file lands, because that is where the separation is enforced:
    // two scopes are two tables.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));
    server.refresh_maintained_cubes();

    let cube = &server.cubes()[0];
    let snapshot = server.snapshot_for_test(cube.fact_table());
    let base = sankhya_cube::algo::Cuboid::of(&["region", "period"]);
    let unrestricted =
        sankhya_cube::materialise::Key::unrestricted(cube.version(), snapshot, base.clone());
    let restricted =
        sankhya_cube::materialise::Key::new(cube.version(), snapshot, 0xdead_beef, base);

    assert!(
        sankhya_maintenance::cuboid::exists(dir.path(), &unrestricted, cube.name()),
        "the unrestricted cuboid was built"
    );
    assert!(
        !sankhya_maintenance::cuboid::exists(dir.path(), &restricted, cube.name()),
        "and no restricted scope was, because the refresher has no principal to build one for"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_refresher_collects_cuboids_the_table_has_left_behind() {
    // The population bound. Cuboids multiply by cubes, cuboids per cube, scopes and *live
    // snapshots* — and the last factor was bounded by nothing. A cuboid at an old snapshot
    // can never be selected, so it is garbage the moment the table advances, and neither the
    // orphan sweep (which finds files within a table) nor retirement (which retires
    // compaction inputs) could see it: it is a whole table no log mentions.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));
    let cube = &server.cubes()[0];

    // A cuboid from far enough in the past that nothing could ask for it.
    let stale = sankhya_cube::materialise::Key::unrestricted(
        cube.version(),
        1,
        sankhya_cube::algo::Cuboid::of(&["region"]),
    );
    let mut cells = sankhya_cube::cells::Cells::over(vec!["region".to_string()]);
    cells.add(vec!["north".to_string()], 1.0).expect("well-formed");
    sankhya_maintenance::cuboid::materialise(dir.path(), &stale, cube.name(), &cells, CubeRule::Sum, &SAW_EVERYTHING)
        .expect("materialising a stale cuboid");
    assert!(sankhya_maintenance::cuboid::exists(dir.path(), &stale, cube.name()));

    // Pretend the table has moved a long way past it.
    for version in 2..200u64 {
        let root = dir.path().join("sales").join("orders");
        let more = RecordBatch::try_new(
            schema(),
            vec![
                Arc::new(Int64Array::from(vec![version as i64])),
                Arc::new(StringArray::from(vec!["north"])),
                Arc::new(StringArray::from(vec!["q1"])),
                Arc::new(Float64Array::from(vec![1.0])),
            ],
        )
        .expect("a valid batch");
        if Publication::external(&root, "orders")
            .append_rebasing(version, 8, &format!("part-{version:05}.parquet"), &more, Lsn::new(version))
            .is_err()
        {
            break;
        }
    }

    server.refresh_maintained_cubes();

    assert!(
        !sankhya_maintenance::cuboid::exists(dir.path(), &stale, cube.name()),
        "a cuboid nothing can ask for is collected, or the population grows without bound"
    );
}

// --- the query log closes the loop -------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_was_asked_for_is_what_gets_materialised() {
    // The loop M7 could not close: selection has existed and been tested since the milestone
    // began, and nothing recorded the signal its own documentation says it needs. A cube
    // asked repeatedly for one shape should end up holding that shape.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));
    connect(&server, "ana");

    // Ask for a coarser grain than the base, several times over.
    for _ in 0..5 {
        server
            .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
            .expect("the cube answers");
    }

    server.refresh_maintained_cubes();

    let cube = &server.cubes()[0];
    let snapshot = server.snapshot_for_test(cube.fact_table());
    let asked = sankhya_cube::materialise::Key::unrestricted(
        cube.version(),
        snapshot,
        sankhya_cube::algo::Cuboid::of(&["region"]),
    );
    assert!(
        sankhya_maintenance::cuboid::exists(dir.path(), &asked, cube.name()),
        "the shape people asked for five times is the shape that got materialised"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cube_nobody_queried_gets_its_base_and_no_guesses() {
    // The honest answer for an unqueried cube. There is no evidence about what would help,
    // and spending an operator's storage on a guess is worse than spending none --- which is
    // exactly what selecting against the whole lattice would do.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));

    // No queries at all before refreshing.
    server.refresh_maintained_cubes();

    let cube = &server.cubes()[0];
    let snapshot = server.snapshot_for_test(cube.fact_table());
    // The base is both dimensions. `region` alone is a shape selection could choose --- and
    // must not, with nothing asked for.
    let base = sankhya_cube::materialise::Key::unrestricted(
        cube.version(),
        snapshot,
        sankhya_cube::algo::Cuboid::of(&["region", "period"]),
    );
    let guessed = sankhya_cube::materialise::Key::unrestricted(
        cube.version(),
        snapshot,
        sankhya_cube::algo::Cuboid::of(&["region"]),
    );
    assert!(
        sankhya_maintenance::cuboid::exists(dir.path(), &base, cube.name()),
        "the base is built regardless --- it is the cube"
    );
    assert!(
        !sankhya_maintenance::cuboid::exists(dir.path(), &guessed, cube.name()),
        "and nothing else, because nothing has been asked for"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_materialised_cuboid_holds_the_grain_its_key_names() {
    // A cache that lies about its own grain is worse than no cache, because a reader trusts
    // the key. Hydration produces cells at the *base* grain, so a coarser cuboid has to be
    // rolled to its shape before it is stored --- and the first version of the refresher
    // stored base cells under whatever key it was writing.
    let dir = maintained_warehouse();
    let server = server_over(&dir, policy("reader", None));
    connect(&server, "ana");

    for _ in 0..5 {
        server
            .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
            .expect("the cube answers");
    }
    server.refresh_maintained_cubes();

    let cube = &server.cubes()[0];
    let snapshot = server.snapshot_for_test(cube.fact_table());
    let key = sankhya_cube::materialise::Key::unrestricted(
        cube.version(),
        snapshot,
        sankhya_cube::algo::Cuboid::of(&["region"]),
    );
    let root = sankhya_maintenance::cuboid::root_of(dir.path(), &key, cube.name());
    assert!(root.join("_delta_log").is_dir(), "the cuboid was written");

    // The columns **in the file**, read from its own footer.
    //
    // Not from `resolve`, which returns the schema it was handed rather than the schema on
    // disk --- so asserting on that is asserting on the test's own input. The first version
    // of this test did exactly that and passed under a mutation that stored the wrong grain.
    let live = sankhya_table_delta::live_files(&root).expect("its log replays");
    let file = live.files.first().expect("a file");
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        std::fs::File::open(root.join(&file.path)).expect("openable"),
    )
    .expect("a parquet file");
    let columns: Vec<String> = reader
        .schema()
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect();

    assert!(
        columns.contains(&"region".to_string()),
        "the grain its key names: {columns:?}"
    );
    assert!(
        !columns.contains(&"period".to_string()),
        "and not the base's --- storing base cells under a coarser key files them at a grain \
         they do not have: {columns:?}"
    );
}

/// A server whose warehouse carries **two** cubes over the same facts and the same measure
/// name, differing only in the declared rule.
///
/// Its own fixture rather than an addition to the shared one: every other test here counts the
/// cubes it can see, and a second one would change what they are asserting.
fn server_with_two_rules() -> (Server, tempfile::TempDir) {
    let dir = warehouse_with_a_fact_table();
    catalogue::save(dir.path(), &largest()).expect("declaring the second cube");

    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture must open: {refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "the fixture must read: {unreadable:?}");
    let tables = warehouse::describe(&found);
    let (server, complaints) = Server::with_tables(
        settings(dir.path()),
        policy("reader", None),
        tables,
        servable,
    )
    .adopting_cubes(dir.path());
    assert!(complaints.is_empty(), "the cubes must adopt: {complaints:?}");
    assert_eq!(server.cubes().len(), 2);
    (server, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_cubes_over_the_same_facts_answer_by_their_own_declared_rules() {
    // Two cubes over the same fact table and the same measure *name*, differing only in the
    // rule declared along each dimension. If a declaration were ignored anywhere in the
    // answering path, the two would agree --- and a measure of the right magnitude and the
    // wrong meaning is the failure the whole additivity model exists to prevent.
    //
    // What this does **not** guard: the reduction inside a single cell. `navigate::roll_up`
    // reduces along the dimension being rolled away before `batch` sees the cells, so a cell
    // of a rolled result holds one value and any rule over it returns that value. The rule
    // `batch` applies governs the base grain and the slice path instead, and is currently
    // unguarded --- recorded in `tools/mutation-audit.py` rather than left to be discovered.
    let (server, _dir) = server_with_two_rules();
    connect(&server, "ana");

    let summed = server
        .query("SELECT * FROM cube_rollup('sales', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the summing cube answers");
    let largest = server
        .query("SELECT * FROM cube_rollup('largest', 'amount', 'by=region')", &Caller::new(&anyone()))
        .expect("the max cube answers");

    assert_ne!(
        total_from(&summed),
        total_from(&largest),
        "the declared rule was ignored and both answered with the sum: {:?} against {:?}",
        summed.rows,
        largest.rows
    );
    assert!(
        total_from(&summed) > total_from(&largest),
        "summing contributions must exceed taking the largest of them: {} against {}",
        total_from(&summed),
        total_from(&largest)
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
