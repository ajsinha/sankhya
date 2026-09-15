//! The graph engine answers.
//!
//! `M25`. Until 2026-09-14 every `graph_*` call on every startable server returned *"no graph
//! named '…'; known graphs are []"*, under every configuration. `GraphCatalog` was constructed
//! as a **temporary** inside the session builder --- the `Arc` bound to nothing, the function
//! running once per session --- so each session got a fresh empty map with no handle by which
//! anything could populate it. `register` and `publish` had zero call sites anywhere, tests
//! included, and `sankhya-graph` was not a runtime dependency of the server at all.
//!
//! One of the three engines in the product's name did not run, and no test said so, because
//! every test of the traversal engine built its epoch by hand.
//!
//! These tests go through the front door: a declaration in the warehouse, a server started
//! over it, and a `SELECT` that has to find the graph the server built.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_api_pg::session::{Caller, Handler};
use sankhya_authz::policy::{Action, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Role, TenantId};
use sankhya_graph::catalogue::{self, DeclaredEdge, Declaration};
use sankhya_graph::spec::EdgeSpec;
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

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("payer", DataType::Utf8, false),
        Field::new("payee", DataType::Utf8, false),
        Field::new("amount", DataType::Int64, false),
    ]))
}

/// `a -> b -> c -> d`, and a branch `b -> e`.
fn transfers() -> RecordBatch {
    RecordBatch::try_new(schema(), vec![
        Arc::new(StringArray::from(vec!["a", "b", "c", "b"])),
        Arc::new(StringArray::from(vec!["b", "c", "d", "e"])),
        Arc::new(Int64Array::from(vec![10_i64, 20, 30, 40])),
    ])
    .expect("a valid batch")
}

/// A warehouse holding `sales.transfers`, and a `payments` graph declared over it.
fn warehouse_with_a_graph() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("sales").join("transfers");
    let publication = Publication::external(&root, "transfers");
    publication.create(&schema()).expect("creating");
    publication
        .append(1, "part-0000.parquet", &transfers(), Lsn::new(4))
        .expect("publishing");

    let declaration = Declaration::new("payments", vec![DeclaredEdge {
        table: "transfers".to_string(),
        spec: EdgeSpec::new("payer", "payee", "paid")
            .between("account", "account")
            .weighted_by("amount"),
    }]);
    catalogue::save(dir.path(), &declaration).expect("declaring the graph");
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

fn policy(role: &str, rows: Option<&str>) -> PolicySet {
    let mut rule = Rule::grant(
        tenant(),
        Role::new(role),
        TableRef::new("sales", "transfers"),
        Action::Read,
    );
    if let Some(rows) = rows {
        rule = rule.where_rows(rows);
    }
    PolicySet::new().with(rule)
}

fn server_over(dir: &tempfile::TempDir, policy: PolicySet) -> (Server, Vec<String>) {
    let (found, refused) = warehouse::discover(dir.path());
    assert!(refused.is_empty(), "the fixture must open: {refused:?}");
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = warehouse::servable(&found, Lsn::new(u64::MAX), &cache);
    assert!(unreadable.is_empty(), "the fixture must read: {unreadable:?}");
    let mut settings = settings(dir.path());
    settings.roles = [("ana".to_string(), vec!["reader".to_string()])]
        .into_iter()
        .collect();
    let (server, cube_complaints) =
        Server::with_tables(settings, policy, warehouse::describe(&found), servable)
            .adopting_cubes(dir.path());
    assert!(cube_complaints.is_empty(), "no cube is declared: {cube_complaints:?}");
    let (server, complaints) = server.adopting_graphs(dir.path());
    server
        .authenticate(&[("user".to_string(), "ana".to_string())], Some(b"x"))
        .expect("authenticated");
    (server, complaints)
}

fn anyone() -> Vec<(String, String)> {
    vec![("user".to_string(), "ana".to_string())]
}

fn column(result: &sankhya_api_pg::session::QueryResult, name: &str) -> Vec<String> {
    let at = result
        .fields
        .iter()
        .position(|field| field.name == name)
        .unwrap_or_else(|| panic!("no column `{name}` in {:?}", result.fields));
    result
        .rows
        .iter()
        .filter_map(|row| row[at].clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declared_graph_is_hydrated_at_startup_and_traversed() {
    // The whole milestone in one statement: a table's rows become edges, the server builds
    // the epoch before anybody connects, and a `SELECT` finds it.
    let dir = warehouse_with_a_graph();
    let (server, complaints) = server_over(&dir, policy("reader", None));
    assert!(complaints.is_empty(), "the graph must hydrate: {complaints:?}");

    let reached = server
        .query(
            "SELECT * FROM graph_reachable('payments', 'a', 'max_depth=3')",
            &Caller::new(&anyone()),
        )
        .expect("the graph answers");

    let mut vertices = column(&reached, "vertex");
    vertices.sort();
    assert_eq!(
        vertices,
        vec!["a", "b", "c", "d", "e"].into_iter().map(str::to_string).collect::<Vec<String>>(),
        "everything `a` reaches in three steps, itself at depth zero: {reached:?}"
    );
    // And the depths, which is what says the edges point the way the declaration said. A
    // graph built from the columns swapped reaches the same five vertices.
    let depths = column(&reached, "depth");
    let at = |vertex: &str| -> String {
        let row = reached
            .rows
            .iter()
            .position(|row| row[0].as_deref() == Some(vertex))
            .unwrap_or_else(|| panic!("no row for {vertex}"));
        depths[row].clone()
    };
    assert_eq!((at("a"), at("b"), at("c"), at("d")), (
        "0".to_string(),
        "1".to_string(),
        "2".to_string(),
        "3".to_string()
    ));

    // And the provenance, which is columns rather than query metadata --- a flag beside the
    // result gets dropped by the first projection that does not mention it.
    assert!(
        column(&reached, "truncated").iter().all(|said| said == "f"),
        "nothing was truncated at this size: {reached:?}"
    );
    assert!(
        column(&reached, "snapshot").iter().all(|said| said != "0"),
        "the epoch carries the snapshot it was built from: {reached:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_weight_a_declaration_names_reaches_the_shortest_path() {
    // `weighted_by` is a column name in a declaration and a weight in the adjacency, and
    // nothing between the two is typed. A path test is what proves the column arrived: the
    // cheapest route from `a` to `d` is the only route, and its cost is the sum of the
    // weights the table holds rather than a hop count.
    let dir = warehouse_with_a_graph();
    let (server, _) = server_over(&dir, policy("reader", None));

    let path = server
        .query(
            "SELECT * FROM graph_shortest_path('payments', 'a', 'd', 'max_depth=5')",
            &Caller::new(&anyone()),
        )
        .expect("the graph answers");
    assert_eq!(
        column(&path, "vertex"),
        vec!["a", "b", "c", "d"].into_iter().map(str::to_string).collect::<Vec<String>>(),
        "the route, in order: {path:?}"
    );
    // **The cost, not the hop count.** `a -> b -> c -> d` weighs 10 + 20 + 30; unweighted it
    // would be three. Asserting the number is the only thing that proves the column named in
    // the declaration reached the adjacency, and a graph built without it answers a plausible
    // number to a different question.
    let cost: f64 = column(&path, "path_cost")
        .last()
        .and_then(|said| said.parse().ok())
        .unwrap_or_default();
    assert_eq!(cost, 60.0, "the sum of the weights the table holds: {path:?}");
    assert_eq!(
        column(&path, "path_hops").last().map(String::as_str),
        Some("3"),
        "three hops, which is what the cost would be if the weight had been dropped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_graph_is_not_offered_to_somebody_a_policy_filters() {
    // **The epoch holds every row**, because it is built once and shared. A caller whose
    // policy filters the source table must therefore not be handed it: a traversal over edges
    // they may not see is a disclosure through reachability, and an invisible one --- every
    // vertex it returns is real, and nothing in the answer says it came from rows they are
    // filtered out of.
    //
    // Told it does not exist rather than told they may not see it, for the reason every
    // listing here follows: saying so confirms it exists.
    let dir = warehouse_with_a_graph();
    let (server, complaints) = server_over(&dir, policy("reader", Some("amount > 25")));
    assert!(complaints.is_empty(), "the graph still hydrates: {complaints:?}");

    let refused = server
        .query(
            "SELECT * FROM graph_reachable('payments', 'a', 'max_depth=3')",
            &Caller::new(&anyone()),
        )
        .expect_err("a filtered caller is not offered the graph");
    assert!(
        refused.message.contains("no graph named"),
        "and is told what an unregistered name is told: {}",
        refused.message
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_graph_over_a_table_nobody_granted_is_not_offered() {
    // The same rule a cube follows. A principal with no grant on the source table has no
    // reason to learn that a graph over it exists.
    let dir = warehouse_with_a_graph();
    let (server, _) = server_over(
        &dir,
        // Granted a table this warehouse does not have, so the session opens and the graph
        // is the only thing out of reach.
        PolicySet::new()
            .with(Rule::grant(
                tenant(),
                Role::new("reader"),
                TableRef::new("sales", "transfers"),
                Action::Read,
            ))
            .with(Rule::grant(
                tenant(),
                Role::new("nobody"),
                TableRef::new("sales", "transfers"),
                Action::Read,
            )),
    );

    // The control: `ana` holds `reader` and sees it.
    let seen = server.query(
        "SELECT * FROM graph_reachable('payments', 'a', 'max_depth=1')",
        &Caller::new(&anyone()),
    );
    assert!(seen.is_ok(), "the control must pass: {seen:?}");

    let stranger: Vec<(String, String)> = vec![("user".to_string(), "mallory".to_string())];
    let hidden = server.query(
        "SELECT * FROM graph_reachable('payments', 'a', 'max_depth=1')",
        &Caller::new(&stranger),
    );
    assert!(
        hidden.is_err(),
        "somebody with no grant on the source table must not traverse it: {hidden:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_graph_naming_a_table_this_server_does_not_serve_is_complained_about() {
    // Reported at startup, where somebody will read it, and **not** silently dropped. A graph
    // that vanishes because its table moved answers every traversal with "no graph named
    // that", which sends the reader to fix a name that is not wrong.
    let dir = warehouse_with_a_graph();
    catalogue::save(
        dir.path(),
        &Declaration::new("ghosts", vec![DeclaredEdge {
            table: "nowhere".to_string(),
            spec: EdgeSpec::new("payer", "payee", "paid"),
        }]),
    )
    .expect("declaring a graph over a table that is not there");

    let (server, complaints) = server_over(&dir, policy("reader", None));
    assert!(
        complaints.iter().any(|said| said.contains("ghosts") && said.contains("nowhere")),
        "the complaint must name the graph and the table: {complaints:?}"
    );

    // And the graph that *is* hydratable is unaffected. One broken declaration must not take
    // the others down with it --- a warehouse that will not start because one graph is
    // misdeclared is a warehouse nobody can fix.
    assert!(
        server
            .query(
                "SELECT * FROM graph_reachable('payments', 'a', 'max_depth=1')",
                &Caller::new(&anyone())
            )
            .is_ok(),
        "the other graph still answers"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_graph_declared_over_a_column_that_is_not_there_is_refused_rather_than_empty() {
    // **The failure this whole tier is arranged against, arriving where the reading loop
    // cannot see it.** `Hydration::absorb` applies each spec only to batches whose schema
    // satisfies it and treats the rest as contributing nothing --- right, because a scan may
    // deliver several tables and each spec reads the ones it recognises. It also means a
    // declaration naming a column that exists nowhere hydrates empty and **succeeds**: the
    // graph registers, resolves, and answers every traversal with no rows, which reads
    // exactly like a traversal that found nothing.
    let dir = warehouse_with_a_graph();
    catalogue::save(
        dir.path(),
        &Declaration::new("typos", vec![DeclaredEdge {
            table: "transfers".to_string(),
            // `payor`, not `payer`.
            spec: EdgeSpec::new("payor", "payee", "paid"),
        }]),
    )
    .expect("declaring a graph over a column that is not there");

    let (server, complaints) = server_over(&dir, policy("reader", None));
    assert!(
        complaints
            .iter()
            .any(|said| said.contains("typos") && said.contains("payor")),
        "the complaint must name the graph and the column: {complaints:?}"
    );

    // Registered and unpublished, so a traversal is told to **wait** rather than told the
    // name is wrong. The two mean different things and the catalogue distinguishes them
    // deliberately: one is a typo in the statement, the other a graph still building.
    let refused = server
        .query(
            "SELECT * FROM graph_reachable('typos', 'a', 'max_depth=1')",
            &Caller::new(&anyone()),
        )
        .expect_err("nothing is published for it");
    assert!(
        refused.message.contains("no epoch published"),
        "declared and unhydrated is its own state: {}",
        refused.message
    );
}
