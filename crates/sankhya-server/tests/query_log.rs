//! One line per statement, for whoever is looking at a slow server.
//!
//! `OPS-24`. There was no query log at all. The audit chain records every statement and is
//! the right home for *evidence* --- hash-linked, durable, tamper-evident --- and it is the
//! wrong thing to read when a server is slow: reading it means reading a chain rather than
//! grepping a log, and it carries no duration. So "which statements are slow?" and "is this
//! server busy?" had no answer anywhere in the system.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{start_with, write_warehouse, Session};
use std::time::Duration;

const WITHIN: Duration = Duration::from_secs(10);

#[test]
fn every_statement_leaves_a_line_naming_who_ran_it_and_what_it_cost() {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start_with(&warehouse, &dir.path().join("data"), &[]);

    let rows = Session::open(server.port)
        .run("SELECT region FROM orders")
        .expect("the fixture answers");
    assert!(rows > 0, "the fixture must have rows for this to mean anything");

    assert!(
        server.wait_until_said("a statement finished", WITHIN),
        "no statement was logged; the server said: {:?}",
        server.said_matching("statement").collect::<Vec<_>>()
    );
    let line = server
        .said_matching("a statement finished")
        .next()
        .expect("a line")
        .to_owned();

    // Who ran it, so a busy server can be attributed rather than guessed at.
    assert!(line.contains("subject="), "the line must name the caller: {line}");
    // What it cost, which is the whole reason this is not the audit: the audit records that
    // a statement happened and has no idea how long it took.
    assert!(line.contains("millis="), "the line must say what it cost: {line}");
    assert!(line.contains("row_count="), "and how much came back: {line}");
    assert!(line.contains("outcome=\"answered\""), "and how it ended: {line}");
    // The shape, and never the statement. `ARCHITECTURE` §17.1 makes query text tenant data
    // and `SEC-07` settled the same question for the audit: a statement's text in a second
    // place is a second place the data lives.
    // The kind, and only the kind. `select region` would name a column the caller chose,
    // and `select 'a-secret'` would name a value --- so the second word is kept only when
    // it is one of ours.
    assert!(line.contains("shape=select"), "the shape identifies the kind: {line}");
    assert!(!line.contains("shape=select region"), "and nothing the caller chose: {line}");
    assert!(
        !line.contains("FROM orders") && !line.contains("from orders"),
        "the statement itself must not reach the log: {line}"
    );
}

#[test]
fn a_refused_statement_is_logged_as_refused_and_without_its_reason() {
    // A log that records only successes cannot show a server refusing everything, which is
    // one of the two shapes an outage takes. And the refusal's *detail* stays out: a
    // planner's message frequently quotes what the caller typed --- `SEC-16` was exactly
    // that leak reaching a client --- so the log says refused and the audit says which.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start_with(&warehouse, &dir.path().join("data"), &[]);

    let _ = Session::open(server.port)
        .run("SELECT nosuchcolumn FROM orders")
        .expect_err("a column that is not there");

    assert!(
        server.wait_until_said("outcome=\"refused\"", WITHIN),
        "a refusal was not logged; the server said: {:?}",
        server.said_matching("a statement finished").collect::<Vec<_>>()
    );
    let line = server
        .said_matching("outcome=\"refused\"")
        .next()
        .expect("a line")
        .to_owned();
    assert!(
        !line.contains("nosuchcolumn"),
        "the caller's own text must not reach the log: {line}"
    );
}

#[test]
fn the_lines_are_not_wrapped_in_terminal_escapes() {
    // Found by a test that asserted on a field and could not see it.
    // `tracing_subscriber` defaults to ANSI on, and this server's output normally goes to a
    // file, to journald, or to a collector --- where every line carries escape sequences and
    // a field an operator filters on reads as `\x1b[3mfeed\x1b[0m\x1b[2m=\x1b[0mpostings`.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start_with(&warehouse, &dir.path().join("data"), &[]);

    let _ = Session::open(server.port).run("SELECT region FROM orders");
    assert!(server.wait_until_said("a statement finished", WITHIN), "no statement was logged");

    for line in server.said_matching("a statement finished") {
        assert!(
            !line.contains('\u{1b}'),
            "the server is colouring output that is not going to a terminal: {line:?}"
        );
    }
}
