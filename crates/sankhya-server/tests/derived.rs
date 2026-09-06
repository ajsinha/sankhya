//! A derived result: a query given a name, over the wire.
//!
//! # What this closes
//!
//! [ADR-0014](../../../docs/adr/0014-materialized-views-and-the-cube-lifetime.md) has been
//! *Proposed* since 2026-08-28 on one question: is a materialized view a cube lifetime? Its
//! answer was Option A --- **a definition with no dimensions and no measures is simply a
//! maintained query** --- and it could not be built, because the declared-query cube it needed
//! did not exist. That was built on 2026-09-02; this is the other half.
//!
//! # Why it reuses the cube's machinery rather than getting its own
//!
//! The ADR is explicit about the failure mode, and it is not that a separate implementation
//! would not work: *"it will grow a second refresh loop, a second staleness rule and a second
//! reclamation path, and the two will drift. This warehouse has already paid for that twice."*
//! So a derived result is the same `Definition`, in the same catalogue, with the same declared
//! query, the same dependency list, the same snapshot key and the same fingerprint.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{start, text_rows, write_warehouse, Running};

fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

fn ask(port: u16, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    std::panic::catch_unwind(|| text_rows(port, sql)).map_err(|panicked| {
        panicked
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panicked.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "the statement failed".to_owned())
    })
}

const JOINED: &str = "CREATE DERIVED regional FROM ( \
     SELECT r.area, o.amount FROM sales.orders o \
     JOIN sales.regions r ON o.region = r.region )";

#[test]
fn a_derived_result_is_selected_from_like_a_table() {
    // The whole claim in one statement: a name, and then `SELECT ... FROM` it. A derived result
    // that is declared and cannot be read is a catalogue entry, which is what a materialized
    // view is not.
    let (_dir, server) = running();
    let _ = ask(server.port, JOINED).expect("a derived result is accepted");

    let listed = text_rows(server.port, "SELECT derived, query, reads, lifetime FROM derived()");
    assert_eq!(listed.len(), 1, "it is served: {listed:?}");
    assert_eq!(listed[0][0].as_deref(), Some("regional"));
    assert!(
        listed[0][1].as_deref().is_some_and(|q| q.contains("JOIN")),
        "the query is shown as it was written: {listed:?}"
    );
    assert_eq!(
        listed[0][2].as_deref(),
        Some("sales.orders, sales.regions"),
        "both sides of the join, as the exact names --- the query text contains both words too, \
         so a containment check would pass for one that recorded nothing"
    );
    assert_eq!(listed[0][3].as_deref(), Some("declared"), "nothing is materialised");

    // And it answers, against the query it stands for.
    let through = text_rows(
        server.port,
        "SELECT area, sum(amount) FROM regional GROUP BY area ORDER BY area",
    );
    let direct = text_rows(
        server.port,
        "SELECT r.area, sum(o.amount) FROM sales.orders o \
         JOIN sales.regions r ON o.region = r.region GROUP BY r.area ORDER BY r.area",
    );
    assert!(!direct.is_empty(), "the fixture has regions");
    assert_eq!(
        through, direct,
        "a derived result must answer what its own query answers. A row count would prove it \
         ran; only the totals prove it ran the right query"
    );
}

#[test]
fn a_derived_result_does_not_appear_among_the_cubes() {
    // It has no dimensions and no measures, so it would arrive in a cube picker as a row of
    // zeroes --- a thing a client would offer somebody and then fail to roll up.
    let (_dir, server) = running();
    let _ = ask(server.port, JOINED).expect("declared");

    let cubes: Vec<String> = text_rows(server.port, "SELECT cube FROM cubes()")
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert!(!cubes.iter().any(|n| n == "regional"), "not a cube: {cubes:?}");
}

#[test]
fn it_survives_a_restart() {
    let (dir, server) = running();
    let _ = ask(server.port, JOINED).expect("declared");

    // A **real** restart: the first server is stopped before the second starts, and the second
    // reuses the same data directory.
    //
    // This started a second server on the same warehouse with a *different* data directory
    // while the first was still running, and called that a restart. It is not --- it is the
    // two-writer state, in which both processes hold their own audit chain from sequence zero
    // and append to one file, corrupting it permanently. It was possible only because the lock
    // lived in the data directory; moving it into the warehouse is what surfaced this.
    drop(server);
    let again = start(&dir.path().join("warehouse"), &dir.path().join("data"));
    let listed = text_rows(again.port, "SELECT derived, reads FROM derived()");
    assert_eq!(listed.len(), 1, "still there after a restart: {listed:?}");
    assert_eq!(
        listed[0][1].as_deref(),
        Some("sales.orders, sales.regions"),
        "with its dependencies, which a restart cannot re-derive: it has no session and no guard"
    );
    assert_eq!(
        text_rows(again.port, "SELECT count(*) FROM regional").len(),
        1,
        "and it answers"
    );
}

#[test]
fn a_derived_result_over_a_table_name_is_refused() {
    // It would be that table with a second name --- and a catalogue entry, a fingerprint and a
    // dependency list to keep current for no gain.
    let (_dir, server) = running();
    let Err(said) = ask(server.port, "CREATE DERIVED copy FROM sales.orders") else {
        panic!("a derived result over a bare table name must be refused");
    };
    // The parser refuses it before validation is reached, and says what to write. The other way
    // in --- a definition read back from a catalogue, written by an older version or edited by
    // hand --- is guarded by `validate` and covered in `sankhya-cube/tests/definition.rs`.
    assert!(
        said.contains("parenthesised query"),
        "and says what to write instead: {said}"
    );
}

#[test]
fn a_derived_query_whose_answer_can_move_is_refused() {
    // The same rule a cube's fact query obeys, and for the same reason: a maintained result
    // built from such a query is a cache of one arbitrary answer.
    let (_dir, server) = running();
    let moving = "CREATE DERIVED drifting FROM ( SELECT amount FROM sales.orders \
         WHERE period < now() )";
    let Err(said) = ask(server.port, moving) else {
        panic!("a query whose answer moves is not a derived result");
    };
    assert!(said.contains("now"), "the refusal names what it found: {said}");
}

#[test]
fn the_word_and_the_thing_must_agree_when_dropping() {
    // A cube and a derived result share a namespace. They are different things to lose, and a
    // script that dropped the wrong one would report success.
    let (_dir, server) = running();
    let _ = ask(server.port, JOINED).expect("declared");

    let Err(said) = ask(server.port, "DROP CUBE regional") else {
        panic!("`DROP CUBE` must not remove a derived result");
    };
    assert!(said.contains("no cube"), "and says so as an absence: {said}");
    assert_eq!(text_rows(server.port, "SELECT derived FROM derived()").len(), 1, "still there");

    let _ = text_rows(server.port, "DROP DERIVED regional");
    assert!(text_rows(server.port, "SELECT derived FROM derived()").is_empty(), "gone");
    assert!(
        ask(server.port, "SELECT count(*) FROM regional").is_err(),
        "and no longer resolvable"
    );
}

#[test]
fn a_maintained_derived_result_is_refused_rather_than_accepted_and_ignored() {
    // The clause parses and sets a target lag, and the maintenance loop walks a cube's
    // *measures* to decide what to build --- a derived result has none. So it would build
    // nothing while `derived()` reported its lifetime as `maintained`.
    //
    // A clause accepted and ignored is the worst of the three available behaviours. Worse than
    // refusing it, and worse than not parsing it, because the operator has been *told* their
    // staleness bound is being honoured.
    let (_dir, server) = running();
    let Err(said) = ask(
        server.port,
        "CREATE DERIVED cached FROM ( SELECT amount FROM sales.orders ) \
         MAINTAINED WITHIN 5 VERSIONS",
    ) else {
        panic!("a maintained derived result must be refused until something maintains it");
    };
    assert!(
        said.contains("not built yet"),
        "and says so plainly rather than failing for some other reason: {said}"
    );
    assert!(
        said.contains("without `MAINTAINED`"),
        "and says what does work: {said}"
    );
    assert!(
        text_rows(server.port, "SELECT derived FROM derived()").is_empty(),
        "and nothing was stored"
    );
}
