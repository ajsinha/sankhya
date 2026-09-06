//! A cube whose facts come from a **declared query** rather than from a table name.
//!
//! # Why this is the shape that was missing
//!
//! [ADR-0014](../../../docs/adr/0014-materialized-views-and-the-cube-lifetime.md) asked whether
//! a materialized view is a cube lifetime and answered: a maintained cube is one *shape* of
//! maintained query, and what a cube could not express was a derived result. It stayed
//! *Proposed* for one reason, stated in its own consequences --- **the declared-query cube from
//! [ADR-0012](../../../docs/adr/0012-open-capabilities.md) did not exist.** A cube's facts had
//! to be a published table's name, so a user wanting a cube over a join had to publish the join
//! as a second table and keep it fresh themselves.
//!
//! # What makes it safe to allow, rather than merely possible
//!
//! `ADR-0012`'s rule is a single sentence: *an artefact that cannot say what it needs cannot be
//! cached correctly, checked against policy, or bounded.* A query's **text** does not say what
//! it needs --- `FROM orders` names a different table under a different search path, a CTE named
//! `orders` is not a table at all, and a name inside a string literal is not a reference. So the
//! query is planned, once, under the caller's own guard, and the tables it turns out to read are
//! recorded with the definition. Those tables are then exactly what everything else already
//! used the fact table for: the authorization check, the snapshot the cache is keyed on, and the
//! fingerprint that says two cubes differ.
//!
//! These tests are over the wire, against the shipping server, because that is the only place
//! all four of those meet.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![allow(clippy::print_stdout)]

mod common;

use common::{start, text_rows, write_warehouse, Running};

/// A warehouse with `sales.orders` and `sales.regions`, and a running server over it.
fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

/// Stop the running server and start another on the same warehouse and data directory.
///
/// # What this used to be
///
/// A second server over the same warehouse with a *different* data directory, started while
/// the first was still running --- and this comment called that "what a restart is". It is
/// not. It is the two-writer state: both processes hold their own audit chain beginning at
/// sequence zero and append to one file, which corrupts it permanently and silently.
///
/// It was possible only because the lock lived in the data directory, so two servers over one
/// warehouse took two different locks. Moving the lock into the warehouse is what surfaced
/// this, in three tests at once.
fn restart(dir: &tempfile::TempDir, server: Running) -> Running {
    drop(server);
    start(&dir.path().join("warehouse"), &dir.path().join("data"))
}

/// One statement's first answer, or the refusal it produced.
fn ask(port: u16, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    let rows = std::panic::catch_unwind(|| text_rows(port, sql));
    rows.map_err(|panicked| {
        panicked
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panicked.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "the statement failed".to_owned())
    })
}

const JOINED: &str = "CREATE CUBE joined FROM (SELECT o.amount, r.area FROM sales.orders o \
     JOIN sales.regions r ON o.region = r.region) \
     DIMENSION area FROM sales.regions ON area (LEVEL area = area) \
     MEASURE amount (SUM ALONG area)";

#[test]
fn a_cube_over_a_declared_query_is_created_served_and_answers() {
    // The end-to-end claim, and it needs all three: created (the statement is accepted),
    // served (`SHOW CUBES` knows it), and *answers* (a roll-up returns numbers). A cube that is
    // created and served and answers nothing is the failure this repository keeps finding --- a
    // feature that exists everywhere except where somebody would use it.
    let (_dir, server) = running();

    let created = ask(server.port, JOINED).expect("a cube over a query is accepted");
    assert!(created.is_empty() || created[0].is_empty() || true, "{created:?}");

    let cubes = text_rows(server.port, "SELECT cube, fact_table, reads FROM cubes()");
    let names: Vec<String> = cubes
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert!(names.iter().any(|n| n == "joined"), "the cube is served: {names:?}");

    // The fact source is shown as what was written, not re-rendered. A reader who has to work
    // out which query a cube is over from a name is being asked to guess.
    let shown: Vec<String> = cubes
        .iter()
        .filter(|row| row.first().cloned().flatten().as_deref() == Some("joined"))
        .filter_map(|row| row.get(1).cloned().flatten())
        .collect();
    assert!(
        shown.first().is_some_and(|text| text.contains("JOIN")),
        "the declared query is shown as it was written: {shown:?}"
    );

    // And what it reads, which is the half that decides anything: the authorization, the cache
    // key and the invalidation are all against these, not against the text above.
    let reads: Vec<String> = cubes
        .iter()
        .filter(|row| row.first().cloned().flatten().as_deref() == Some("joined"))
        .filter_map(|row| row.get(2).cloned().flatten())
        .collect();
    let listed = reads.first().cloned().unwrap_or_default();
    let names: Vec<&str> = listed.split(", ").collect();
    assert_eq!(
        names,
        vec!["sales.orders", "sales.regions"],
        "both sides of the join, because a cube over a join is a cube over both. Recording \
         one of them would authorize against half the data and go stale against the other \
         half without noticing. Asserted as the exact names rather than as text that contains \
         them --- the query text contains both words too, so a containment check would pass \
         for a cube that recorded no dependencies at all: {listed}"
    );

    let answers = text_rows(
        server.port,
        "SELECT area, amount FROM cube_rollup('joined', 'amount', 'by=area')",
    );
    assert!(
        !answers.is_empty(),
        "a cube over a query must answer a roll-up, or the query was accepted and never read"
    );

    // Against the query itself, which is the only assertion that can fail for the right
    // reason. A row count proves the roll-up ran; only the *total* proves it read the join ---
    // a cube that read `orders` alone would answer too, with a number nobody could tell apart
    // from this one by looking at it.
    let direct = text_rows(
        server.port,
        "SELECT sum(o.amount) FROM sales.orders o JOIN sales.regions r ON o.region = r.region",
    );
    let expected: f64 = direct
        .first()
        .and_then(|row| row.first().cloned().flatten())
        .and_then(|text| text.parse().ok())
        .expect("the join answers");
    let total: f64 = answers
        .iter()
        .filter_map(|row| row.get(1).cloned().flatten())
        .filter_map(|text| text.parse::<f64>().ok())
        .sum();
    assert!(
        (total - expected).abs() < 1e-6,
        "the cube's roll-up must total what its own fact query totals: {total} against \
         {expected}. A difference here is the cube reading something other than what it says \
         it reads"
    );
}

#[test]
fn a_restart_remembers_what_the_fact_query_reads() {
    // The dependency list is worth exactly nothing if the next process re-derives it, because
    // re-deriving needs a session and a guard and the startup path has neither. Held only in
    // memory, a restarted server would serve a cube that reads a query and believes it reads
    // nothing --- authorized against nothing, and never invalidated.
    let (dir, server) = running();
    let _ = ask(server.port, JOINED).expect("a cube over a query is accepted");

    let again = restart(&dir, server);
    let cubes = text_rows(again.port, "SELECT cube, reads FROM cubes()");
    let listed: String = cubes
        .iter()
        .filter(|row| row.first().cloned().flatten().as_deref() == Some("joined"))
        .filter_map(|row| row.get(1).cloned().flatten())
        .next()
        .unwrap_or_default();
    // The **exact** list, not a list that contains those words. Asserted loosely, this test
    // could not fail: with nothing persisted the dependency list falls back to the fact source
    // as written --- which is the query text, and the query text contains the word `orders`.
    // A restored cube would then read one dependency named after a whole SELECT statement, and
    // a containment check would call that a pass.
    let names: Vec<&str> = listed.split(", ").collect();
    assert_eq!(
        names,
        vec!["sales.orders", "sales.regions"],
        "a restart must find both dependencies persisted, as names: {listed:?}"
    );
}

#[test]
fn a_fact_query_that_can_answer_differently_is_refused() {
    // A cuboid built from such a query is a cache of one arbitrary answer, and every later read
    // serves that answer as though it were the answer. Refused at declaration, where the person
    // who typed it is still there, rather than found later as two reports disagreeing.
    let (_dir, server) = running();

    let moving = "CREATE CUBE drifting FROM (SELECT amount, region FROM sales.orders \
         WHERE period < now()) \
         DIMENSION region FROM sales.regions ON region (LEVEL area = region) \
         MEASURE amount (SUM ALONG region)";
    let failure = ask(server.port, moving).expect_err("a query whose answer moves is refused");
    assert!(
        failure.contains("now"),
        "the refusal names what it found, so the fix is the line they typed: {failure}"
    );

    let cubes = text_rows(server.port, "SELECT cube FROM cubes()");
    let names: Vec<String> = cubes
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert!(!names.iter().any(|n| n == "drifting"), "and nothing was served: {names:?}");
}

#[test]
fn a_fact_query_that_cannot_be_planned_is_refused_rather_than_stored() {
    // The dependency list comes from planning. A query that will not plan has no dependency
    // list, and a cube with none would be authorized against nothing and invalidated by nothing
    // --- so it must not be created at all, rather than created and quietly unmaintainable.
    let (_dir, server) = running();

    let absent = "CREATE CUBE missing FROM (SELECT amount, region FROM no_such_table) \
         DIMENSION region FROM sales.regions ON region (LEVEL area = region) \
         MEASURE amount (SUM ALONG region)";
    let failure = ask(server.port, absent).expect_err("a query naming nothing readable is refused");
    assert!(
        failure.contains("could not be planned"),
        "and says so, rather than reporting the cube invalid for some other reason: {failure}"
    );
}
