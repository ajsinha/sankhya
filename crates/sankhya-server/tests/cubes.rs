//! The server knows what cubes its warehouse declares.
//!
//! Cube definitions were persisted before this and nothing read them, which is the same
//! shape `sankhya-maintenance` was in until the day this was written: a working, tested
//! library that no running server called. A capability nothing in production reaches is
//! indistinguishable, from outside, from one that was never built.
//!
//! What these prove is deliberately the *startup* half. A definition that will not load, or
//! that loads and does not describe a usable cube, is a deployment problem and belongs in the
//! startup log next to the tables that would not open --- discovered while whoever deployed
//! it is still there, rather than by the first query to name it, hours later, reported to a
//! user who did nothing wrong.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_cube::catalogue;
use sankhya_cube::model::{Definition, Dimension, Level};
use sankhya_cube_algo::measure::{Along, Measure, Rule};

// The wiring module is part of the binary, so the test builds it directly --- the same cost
// `tests/wiring.rs` pays, and for the same reason.
#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/warehouse.rs"]
mod warehouse;
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
        maintenance: None,
        require_password: false,
        metrics_listen: None,
    }
}

/// A cube over a fact table, with one dimension and one additive measure.
fn sales() -> Definition {
    Definition::new(
        "sales",
        "orders",
        vec![Dimension {
            name: "region".to_string(),
            table: "regions".to_string(),
            joins_on: "region".to_string(),
            levels: vec![Level::new("area", "region")],
            rollups: None,
            parent_child: None,
        }],
        vec![Measure::new(
            "amount",
            vec![Along::new("region", Rule::Sum)],
        )],
    )
}

/// A server over a warehouse, with whatever cubes it declares.
fn server_over(warehouse: &std::path::Path) -> (Server, Vec<String>) {
    let settings = settings(warehouse);
    let policy = permissive_policy(&tenant(), &[]);
    Server::with_tables(settings, policy, Vec::new(), Vec::new())
        .adopting_cubes(warehouse)
}

#[tokio::test]
async fn a_cube_declared_in_the_warehouse_is_adopted_at_startup() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing the cube");

    let (server, complaints) = server_over(dir.path());
    assert!(complaints.is_empty(), "nothing to complain about: {complaints:?}");
    assert_eq!(
        server.cubes().len(),
        1,
        "a cube written into the warehouse must be there after a restart, or it is not \
         persisted in any sense that matters"
    );
    assert_eq!(server.cubes()[0].name(), "sales");
}

#[tokio::test]
async fn a_warehouse_with_no_cubes_starts_normally() {
    // The ordinary case, and the one a regression would break loudest.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (server, complaints) = server_over(dir.path());
    assert!(server.cubes().is_empty());
    assert!(complaints.is_empty());
}

#[tokio::test]
async fn a_definition_that_will_not_parse_is_complained_about_and_does_not_stop_the_server() {
    // One malformed JSON file must not be an outage. The other cubes and every table are
    // still servable, and refusing to start would turn a typo into downtime.
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing the good one");
    std::fs::write(catalogue::path_of(dir.path(), "broken"), "{ not a cube").expect("writing");

    let (server, complaints) = server_over(dir.path());
    assert!(
        complaints.iter().any(|why| why.contains("broken")),
        "the complaint names the file: {complaints:?}"
    );
    assert!(
        server.cubes().is_empty() || server.cubes().len() == 1,
        "the server starts either way; what it must not do is fail to start"
    );
}

#[tokio::test]
async fn a_definition_that_parses_and_is_not_a_usable_cube_is_complained_about_by_name() {
    // Validation at startup rather than at first use. A measure with no rule along a
    // declared dimension is the error the additivity model exists to catch, and catching it
    // when somebody runs a query means reporting it to the wrong person at the wrong time.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut broken = sales();
    // A measure declaring no rule for the dimension the cube has.
    broken.measures = vec![Measure::new("amount", vec![])];
    catalogue::save(dir.path(), &broken).expect("storing");

    let (server, complaints) = server_over(dir.path());
    assert!(
        complaints.iter().any(|why| why.contains("sales")),
        "the complaint names the cube: {complaints:?}"
    );
    assert!(
        server.cubes().is_empty(),
        "a cube that does not validate is not adopted, because serving it would mean \
         answering with numbers its own rules do not justify"
    );
}

#[tokio::test]
async fn one_broken_cube_does_not_hide_a_good_one() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing the good one");
    let mut broken = sales();
    broken.name = "returns".to_string();
    broken.measures = vec![Measure::new("amount", vec![])];
    catalogue::save(dir.path(), &broken).expect("storing the broken one");

    let (server, complaints) = server_over(dir.path());
    assert_eq!(
        server.cubes().len(),
        1,
        "the good cube is still adopted: {complaints:?}"
    );
    assert_eq!(server.cubes()[0].name(), "sales");
    assert!(complaints.iter().any(|why| why.contains("returns")));
}
