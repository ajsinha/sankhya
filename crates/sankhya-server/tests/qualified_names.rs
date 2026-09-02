//! Naming a table the way the catalogue says it is named.
//!
//! # The defect these exist for
//!
//! A session registered every table under its **bare** name and discarded the schema. So
//! `information_schema.tables` reported `sales.orders`, and `SELECT ... FROM sales.orders`
//! answered *"table not found"* --- the catalogue said the table existed and the planner said
//! it did not, which is not a difference a client can work around.
//!
//! Two tables of the same name in different schemas were worse: both registered under one key
//! and the second replaced the first, so one became unreachable with nothing said.
//!
//! [ADR-0017](../../../docs/adr/0017-the-client-contract.md) makes this a contract question
//! rather than an inconvenience. *"Names are sometimes qualified, sometimes not, and sometimes
//! collide"* is not a rule three bindings can agree on.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use sankhya_publish::Publication;
use std::sync::Arc;

mod common;

use common::{query_outcome, start, text_rows, write_warehouse};

/// Put a table of `rows` rows at `<schema>/<name>`, through the product's own writer.
fn table(warehouse: &std::path::Path, schema: &str, name: &str, rows: i64) {
    let columns = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let publication = Publication::external(warehouse.join(schema).join(name), name);
    publication.create(&columns).expect("creating");
    let batch = RecordBatch::try_new(
        Arc::clone(&columns),
        vec![Arc::new(Int64Array::from((0..rows).collect::<Vec<i64>>()))],
    )
    .expect("a batch");
    publication
        .append(1, "part-0000.parquet", &batch, sankhya_types::Lsn::new(1))
        .expect("publishing");
}

#[test]
fn a_qualified_name_resolves_to_the_table_the_catalogue_lists() {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    // The catalogue says `sales.orders`. Asking for it by that name must work, or the
    // catalogue is telling clients about a table they cannot reach.
    let listed = text_rows(
        server.port,
        "SELECT table_schema, table_name FROM information_schema.tables",
    );
    assert!(
        listed.iter().any(|row| {
            row[0].as_deref() == Some("sales") && row[1].as_deref() == Some("orders")
        }),
        "{listed:?}"
    );

    assert_eq!(
        query_outcome(server.port, "SELECT id FROM sales.orders WHERE id < 10")
            .expect("the qualified name resolves"),
        10
    );
}

#[test]
fn the_bare_name_still_resolves_when_only_one_table_claims_it() {
    // Every statement this server has ever answered used the bare name — it is what the
    // guide's examples type. Fixing the qualified case by breaking this one would be an
    // exchange, not a fix.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    assert_eq!(
        query_outcome(server.port, "SELECT id FROM orders WHERE id < 10").expect("bare"),
        10
    );
}

#[test]
fn the_same_name_in_two_schemas_is_two_tables_and_both_are_reachable() {
    // The silent one. Both used to register under a single key, so the second replaced the
    // first and one table disappeared from the planner while staying in the catalogue.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    table(&warehouse, "sales", "orders", 3);
    table(&warehouse, "finance", "orders", 7);
    let server = start(&warehouse, &dir.path().join("data"));

    assert_eq!(
        query_outcome(server.port, "SELECT id FROM sales.orders").expect("sales"),
        3
    );
    assert_eq!(
        query_outcome(server.port, "SELECT id FROM finance.orders").expect("finance"),
        7,
        "the second table was replaced by the first rather than registered beside it"
    );
}

#[test]
fn a_contested_bare_name_is_refused_rather_than_resolved_to_one_of_them() {
    // The answer that cannot be wrong. Resolving it would hand back one of two tables on a
    // rule nobody wrote down, and the caller would have no way to know which they got.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    table(&warehouse, "sales", "orders", 3);
    table(&warehouse, "finance", "orders", 7);
    let server = start(&warehouse, &dir.path().join("data"));

    let refused = query_outcome(server.port, "SELECT id FROM orders").expect_err("ambiguous");
    assert!(refused.contains("orders"), "{refused}");
}

#[test]
fn a_clone_is_made_and_read_by_its_qualified_name() {
    // The two halves together: a clone lands beside its origin, and the name that reaches it
    // is the one the catalogue would print for it.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE sales.orders").expect("cloning");
    assert!(
        warehouse.join("sales").join("q3_frozen").join("_delta_log").exists(),
        "a clone lands in its origin's schema"
    );

    let chain = text_rows(server.port, "SHOW LINEAGE OF q3_frozen");
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0][1].as_deref(), Some("sales.orders"));
}

#[test]
fn a_clone_may_not_be_created_in_another_schema() {
    // The rule, from a client. A clone is a reference to its origin's files and is authorized
    // through them, so one placed under another schema would have its name governed by one
    // policy and its data by another.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let refused = query_outcome(server.port, "CREATE TABLE archive.q3 CLONE sales.orders")
        .expect_err("another schema is refused");
    assert!(refused.contains("stays in its origin"), "{refused}");
    assert!(!warehouse.join("archive").exists(), "and nothing was created");

    // Naming the origin's own schema is the same statement as not naming one.
    query_outcome(server.port, "CREATE TABLE sales.q3 CLONE sales.orders").expect("permitted");
    assert!(warehouse.join("sales").join("q3").join("_delta_log").exists());
}

#[test]
fn a_cube_may_not_be_declared_on_a_bare_name_two_schemas_claim() {
    // Authorization is the worse place to guess than resolution is. A cube declared on an
    // ambiguous name would be authorized against one of two policy rules and would then
    // hydrate from one of two tables, with nothing in the definition recording which --- and a
    // cube answers with a *number*, which is the answer nobody notices is from the wrong table.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    table(&warehouse, "sales", "orders", 3);
    table(&warehouse, "finance", "orders", 7);
    let server = start(&warehouse, &dir.path().join("data"));

    let refused = query_outcome(
        server.port,
        "CREATE CUBE totals FROM orders \
         DIMENSION id FROM orders ON id (LEVEL each = id) \
         MEASURE id (SUM ALONG id)",
    )
    .expect_err("an ambiguous fact table is refused");
    assert!(refused.contains("orders"), "{refused}");
}
