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

use common::{query_outcome, start, start_with, text_rows, write_warehouse};

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

// --- what a refusal says, which is what a client branches on ---------------

#[test]
fn a_failure_carries_the_sqlstate_its_kind_has_rather_than_the_class_default() {
    // A driver's behaviour is driven by these five characters, never by the message. Every
    // user-class failure used to answer `42601`, *syntax_error* --- so a migration tool asking
    // for a table that is not there yet was told its generated SQL was malformed, instead of
    // `42P01`, which is what every one of them branches on to mean "create it".
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    for (sql, expected, why) in [
        ("SELECT * FROM no_such_table", "42P01", "undefined_table"),
        ("SELECT no_such_column FROM orders", "42703", "undefined_column"),
        ("SELECT 1/0", "22012", "division_by_zero"),
        ("SELECT CAST('abc' AS BIGINT)", "22P02", "invalid_text_representation"),
        ("TRUNCATE orders", "0A000", "feature_not_supported, not a syntax error"),
    ] {
        let refused = query_outcome(server.port, sql).expect_err("refused");
        assert!(
            refused.contains(expected),
            "`{sql}` should answer {expected} ({why}), and said: {refused}"
        );
    }
}

#[test]
fn a_refusal_carries_its_code_once() {
    // `Display` for a catalogue error already renders `[code] message`, and prefixing again
    // produced `[SNK-C0006] [SNK-C0006] ...` --- which reads like a bug in the thing reporting
    // the bug.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let refused = query_outcome(server.port, "TRUNCATE orders").expect_err("refused");
    assert_eq!(refused.matches("SNK-C0006").count(), 1, "{refused}");
}

#[test]
fn a_construct_the_engine_parses_and_ignores_is_refused_rather_than_answered() {
    // The worst failure available: `TABLESAMPLE BERNOULLI (1)` asked for one per cent and was
    // answered with the whole table, reported as success. Nothing in the result said the
    // sampling had not happened, so a caller doing statistical work got the population with a
    // confidence interval computed as though it were a sample.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let refused = query_outcome(
        server.port,
        "SELECT count(*) FROM orders TABLESAMPLE BERNOULLI (1)",
    )
    .expect_err("refused rather than answered over the whole table");
    assert!(refused.contains("0A000"), "{refused}");

    // And a *value* that happens to contain the word is not a construct. Structure and data
    // are different things, which is the lesson the catalogue recogniser learned the hard way.
    assert_eq!(
        query_outcome(
            server.port,
            "SELECT count(*) FROM orders WHERE region = 'TABLESAMPLE'"
        )
        .expect("an ordinary query"),
        1
    );
}

#[test]
fn the_statements_a_driver_sends_around_a_query_are_answered() {
    // Every connection pool opens with `SET extra_float_digits`, wraps work in
    // `BEGIN`/`COMMIT`, and returns a connection with `DISCARD ALL`. Refusing them refused the
    // driver --- and `SET` was refused as `XX000`, a *fatal server configuration error*, which
    // makes a pool discard the connection and try again forever.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    for sql in [
        "BEGIN",
        "COMMIT",
        "SET extra_float_digits = 3",
        "SET application_name = 'thing'",
        "RESET ALL",
        "DISCARD ALL",
    ] {
        query_outcome(server.port, sql).unwrap_or_else(|why| panic!("`{sql}` was refused: {why}"));
    }
}

#[test]
fn rollback_is_refused_because_it_is_the_one_that_would_be_a_lie() {
    // This server writes nothing, so `BEGIN` and `COMMIT` are true statements about a
    // transaction of one statement. `ROLLBACK` is not: a client that asks to undo and is told
    // it worked has been lied to about the only thing it wanted.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let refused = query_outcome(server.port, "ROLLBACK").expect_err("refused");
    assert!(refused.contains("25P01"), "{refused}");
    assert!(refused.contains("nothing to roll back"), "{refused}");
}

#[test]
fn the_cube_surface_answers_on_a_warehouse_that_has_no_cubes() {
    // `SELECT * FROM cubes()` answered "table function 'cubes' not found" when the warehouse
    // held none -- so a client could not tell **"no cubes yet"** from **"this server does not
    // do cubes"**, which are opposite facts with opposite responses, on the one warehouse
    // where the question is most likely to be asked: a new one.
    //
    // The same defect as a projected query over an empty table, one level up. Registering a
    // catalogue whose contents are empty is not pretending -- the graph functions do exactly
    // this, deliberately, and say so.
    // A warehouse with a table and no cube -- which is every warehouse on its first day, and
    // the state the sample fixture does not have because it declares one.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    table(&warehouse, "sales", "orders", 3);
    let server = start(&warehouse, &dir.path().join("data"));

    let listed = text_rows(server.port, "SELECT * FROM cubes()");
    assert!(listed.is_empty(), "this warehouse has no cubes: {listed:?}");

    // And it answers with the shape a picker binds to, rather than nothing at all.
    assert_eq!(
        query_outcome(server.port, "SELECT * FROM cubes()").expect("the function resolves"),
        0
    );
}

#[test]
fn a_statement_that_outruns_its_deadline_is_stopped_rather_than_left_running() {
    // An adversarial review showed one client typing a cheap, arbitrarily expensive statement
    // and hanging up, after which the server went on burning a whole core to completion.
    // Nothing in the query path had a deadline: `collect` awaits every batch, and
    // `block_in_place` holds a runtime worker while it does. Enough of those starve every
    // other connection, and the client that started it is gone.
    //
    // `sankhya-governor` has `Budget`, `Deadline` and `Cancel`, and the query path used none
    // of them --- they are a polling model and nothing here polls. This is the bound the
    // execution actually admits.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    table(&warehouse, "sales", "orders", 3);
    let server = start_with(
        &warehouse,
        &dir.path().join("data"),
        // One second, so the test is about the deadline rather than about waiting.
        &[("SANKHYA_STATEMENT_TIMEOUT_SECONDS", "1")],
    );

    let refused = query_outcome(
        server.port,
        "SELECT count(*) FROM generate_series(1, 400000000) a, generate_series(1, 40) b",
    )
    .expect_err("a statement that outruns its deadline is stopped");
    assert!(refused.contains("57014"), "query_canceled is the code: {refused}");
    assert!(refused.contains("was stopped"), "{refused}");

    // And the connection is still usable: a stopped statement is not a broken session.
    assert_eq!(
        query_outcome(server.port, "SELECT id FROM sales.orders").expect("still serving"),
        3
    );
}
