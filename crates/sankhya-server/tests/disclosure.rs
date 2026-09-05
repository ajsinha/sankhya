//! What a refusal, a catalogue and a listing say to somebody who may not read the thing.
//!
//! # The shape all of these share
//!
//! None of them returns a row the caller may not see. Every one of them *tells* the caller
//! something about rows they may not see: the columns of a table, the schemas a name is claimed
//! by, the SQL of a definition, the tables a snapshot pins, the file a feed reads. A control
//! that governs only the rows governs the least interesting half.
//!
//! # Why these go over a socket
//!
//! Because the property is about what reaches a client, and every one of these leaks was inside
//! a message that a unit test would have had to assert on to see at all.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{start_with, write_warehouse, Running, Session};

/// A server on which `ana` reads and `mallory` holds no role at all.
fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let config = dir.path().join("application.yaml");
    std::fs::write(
        &config,
        format!(
            "warehouse: {}\nlisten: 127.0.0.1:0\nserver:\n  users:\n    ana: reader\n    quickstart: reader\n",
            warehouse.display()
        ),
    )
    .expect("writing the configuration");
    let server = start_with(
        &warehouse,
        &dir.path().join("data"),
        &[("SANKHYA_CONFIG", config.to_str().expect("a path"))],
    );
    (dir, server)
}

#[test]
fn a_misspelt_column_is_not_answered_with_the_list_of_real_ones() {
    // `SEC-16`. `SELECT nosuchcol FROM orders` produced *"Schema error: No field named
    // nosuchcol. Valid fields are orders.id, orders.region, …"* and the whole of it reached the
    // client --- every column of every table in the plan's scope, to anybody who could name one
    // table and guess one column wrong. The only mention of that phrase in the repository
    // sniffed for it to choose a SQLSTATE and passed it on.
    let (_dir, server) = running();

    let refused = Session::open_as(server.port, "ana")
        .run("SELECT nosuchcol FROM orders")
        .expect_err("a column that is not there");

    // The half the caller needs is kept: they typed it, and telling them they did is the whole
    // usefulness of the message.
    assert!(
        refused.contains("nosuchcol"),
        "the caller must still be told which name did not resolve: {refused}"
    );
    // And the half they did not type is gone.
    assert!(
        !refused.contains("Valid fields"),
        "the planner's enumeration of real column names must not reach a client: {refused}"
    );
    for column in ["region", "period", "margin_pct", "sank_data_date"] {
        assert!(
            !refused.contains(column),
            "`{column}` is a real column of this table and must not be named in a refusal \
             about a different one: {refused}"
        );
    }

    // Not vacuous, and this is the assertion that keeps the marker honest: the planner still
    // produces the message this cuts, so a version of DataFusion that reworded it would fail
    // here rather than silently leaking again.
    let real = Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders")
        .expect("a column that is there");
    assert!(real > 0, "the fixture must have rows for this to mean anything");
}

#[test]
fn a_name_is_contested_only_by_tables_the_caller_can_see() {
    // `SEC-17`, the half with a string in it. `claims` and `contested` were built from **all**
    // servable tables and the guard ran afterwards, so a refusal telling a caller to qualify an
    // ambiguous name enumerated the schemas of a warehouse they had no grant on.
    //
    // There is a second channel with no string at all, and it is the one worth fearing: because
    // the count included tables the caller cannot read, a hidden `payroll.orders` made the
    // caller's own `sales.orders` stop resolving under its bare name. Anybody could ask whether
    // a table of a given name existed somewhere they could not look, and read the answer off
    // whether their own query planned.
    let (_dir, server) = running();

    // `mallory` holds no role, so every table is invisible to them --- which means no bare name
    // can be contested for them, and a refusal must say so without naming anything.
    let refused = Session::open_as(server.port, "mallory")
        .run("SELECT * FROM orders")
        .expect_err("mallory may read nothing");
    assert!(
        !refused.contains("sales.orders"),
        "a refusal to somebody with no grant must not name a qualified table: {refused}"
    );

    // And the oracle: `ana`'s own query must plan under the bare name whatever else exists on
    // the server that she cannot see.
    let planned = Session::open_as(server.port, "ana")
        .run("SELECT count(*) FROM orders")
        .expect("ana's own table must resolve under its bare name");
    assert!(planned > 0, "and must actually answer: {planned}");
}

#[test]
fn a_listing_shows_what_the_caller_may_read_and_nothing_else() {
    // `SEC-18`. Five listings were unfiltered. `derived()` emits the SQL text of every
    // definition and the tables it reads; `SHOW SNAPSHOTS` emits the qualified name of every
    // table a snapshot pins; `SHOW FEEDS` emits what a halted source was doing, which is a
    // filesystem path. `register_derived` was already gating on scope four lines away in the
    // same file.
    let (_dir, server) = running();

    Session::open_as(server.port, "ana")
        .run("CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS")
        .expect("ana may take one");

    // The control, first: ana sees her own.
    assert_eq!(
        Session::open_as(server.port, "ana")
            .run("SHOW SNAPSHOTS")
            .expect("ana may list"),
        1,
        "a listing that showed nobody anything would pass every assertion below"
    );

    // And mallory, who may read nothing, sees nothing --- rather than seeing a row naming
    // `sales.orders` as a table that is being pinned.
    assert_eq!(
        Session::open_as(server.port, "mallory")
            .run("SHOW SNAPSHOTS")
            .expect("the statement is answered, not refused"),
        0,
        "a snapshot row names the tables it pins, so it goes to whoever may read them"
    );

    // `SHOW SNAPSHOTS` is answered rather than refused, and that is deliberate: it needs no
    // session, so a caller who may read nothing is told there are no snapshots they may see
    // rather than being refused --- which would say there are some.
    //
    // Anything that needs a session is refused earlier and for a different reason: a principal
    // who may read no table at all gets one sentence saying so, before any listing is reached.
    // That is existing behaviour and the right behaviour, and it is asserted here so that a
    // change to it shows up as a change to this file rather than as a quiet widening.
    let refused = Session::open_as(server.port, "mallory")
        .run("SELECT * FROM cubes()")
        .expect_err("a principal who may read nothing gets no session");
    assert!(
        refused.contains("may not read any table"),
        "and is told that, once, rather than being shown an empty catalogue: {refused}"
    );
}

#[test]
fn a_feed_is_listed_to_everybody_and_its_halt_reason_is_not() {
    // `SEC-18`, and the one that took two attempts. Filtering the *rows* by the same rule as the
    // other listings removed a feed whose target table does not exist --- and a feed that halted
    // because its table is missing is exactly what an operator opens this statement to find. A
    // control that hides the thing it is meant to report is not a control.
    //
    // So the name and the state go to everybody, because somebody configured that feed and it is
    // not tenant data, and the reason is what is withheld: `ADR-0018` halts a feed when a record
    // does not fit, and saying so means saying which file and what was in it.
    let (_dir, server) = running();

    // No feed is declared on this fixture, so what is asserted here is that the statement is
    // answered rather than refused --- the half that filtering the rows would have broken, and
    // the half a caller with no grant still gets.
    let listed = Session::open_as(server.port, "mallory")
        .run("SHOW FEEDS")
        .expect("a listing of what the server is doing is answered, not refused");
    assert_eq!(listed, 0, "this fixture declares no feeds: {listed}");

    // And for a reader, likewise --- the control that says the statement works at all.
    Session::open_as(server.port, "ana")
        .run("SHOW FEEDS")
        .expect("a reader may ask too");
}
