//! Taking, listing and dropping a snapshot, through the real binary and the real protocol.
//!
//! # Why these go over a socket
//!
//! Because five separate surfaces in this repository have been built, unit-tested,
//! mutation-tested, and unreachable through the front door --- a statement that a client cannot
//! send is not a feature, however well the function behind it is tested. `sankhya-snapshot`'s
//! own tests hold the model still; these check that a deployment does any of it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

// The snapshot module is part of the binary, so the test builds it directly --- the same
// pattern `tests/feeds.rs` uses. That is the cost of a composition root living in a binary
// crate, and it is cheaper than never testing it.
#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/adopt.rs"]
mod adopt;
#[path = "../src/clones.rs"]
mod clones;
#[path = "../src/driver.rs"]
mod driver;
#[path = "../src/warehouse.rs"]
mod warehouse;
#[path = "../src/wiring.rs"]
mod wiring;
#[path = "../src/feeds.rs"]
mod feeds;
#[path = "../src/snapshots.rs"]
mod snapshots;

use common::{query_outcome, start, text_rows, write_warehouse, Running, Session};

/// A warehouse with the sample tables, and a running server.
fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

#[test]
fn a_snapshot_is_taken_listed_and_dropped() {
    let (_dir, server) = running();

    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");

    let listed = text_rows(server.port, "SHOW SNAPSHOTS");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0][0].as_deref(), Some("eod"));
    assert_eq!(listed[0][1].as_deref(), Some("live"));
    assert_eq!(
        listed[0][2].as_deref(),
        Some("quickstart"),
        "a snapshot holds storage on somebody's behalf, and the cost has an owner"
    );
    assert!(
        listed[0][5].as_ref().is_some_and(|count| count != "0"),
        "it pinned nothing: {listed:?}"
    );

    query_outcome(server.port, "DROP SNAPSHOT eod").expect("dropping");
    assert!(text_rows(server.port, "SHOW SNAPSHOTS").is_empty());
}

#[test]
fn a_snapshot_names_the_tables_it_pinned() {
    // What it pins is reported rather than left to be inferred from its existence. A snapshot
    // whose contents nobody can see is a storage cost with no visible owner, which is the shape
    // `RSK-35` describes.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 7 DAYS").expect("taking");

    let listed = text_rows(server.port, "SHOW SNAPSHOTS");
    let pins = listed[0][6].clone().expect("the tables it pinned");
    assert!(pins.contains("sales.orders"), "{pins}");
    assert!(
        pins.contains('.'),
        "pinned by its qualified name, because a bare one is unambiguous only until a second \
         schema grows a table of that name: {pins}"
    );
}

#[test]
fn a_snapshot_of_a_name_already_taken_is_refused_rather_than_replaced() {
    // Replacing it would silently move the instant every report quoting that name reads from
    // --- so the reports would keep working and quietly mean something else.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");

    let refused = query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 30 DAYS")
        .expect_err("refused");
    assert!(refused.contains("42P07"), "{refused}");
    assert!(refused.contains("already exists"), "{refused}");
    assert!(refused.contains("silently move the instant"), "{refused}");
}

#[test]
fn a_snapshot_with_no_expiry_is_refused_and_says_why_there_is_no_default() {
    // `ADR-0019` Decision 3. Somebody who omitted it did not forget a keyword; they expected a
    // default, and there is deliberately none. A message naming a missing token would send them
    // looking for syntax rather than telling them why.
    let (_dir, server) = running();

    let refused = query_outcome(server.port, "CREATE SNAPSHOT eod").expect_err("refused");
    assert!(refused.contains("no default and no unbounded form"), "{refused}");
    assert!(refused.contains("pins files"), "{refused}");
}

#[test]
fn a_lifetime_this_system_will_not_honour_is_refused() {
    let (_dir, server) = running();

    for (sql, expected) in [
        ("CREATE SNAPSHOT a EXPIRE AFTER 0 DAYS", "at least one day"),
        ("CREATE SNAPSHOT a EXPIRE AFTER 900 DAYS", "not technical"),
    ] {
        let refused = query_outcome(server.port, sql).expect_err("refused");
        assert!(refused.contains(expected), "`{sql}` said: {refused}");
    }
    assert!(
        text_rows(server.port, "SHOW SNAPSHOTS").is_empty(),
        "a refused statement created one anyway"
    );
}

#[test]
fn dropping_a_snapshot_that_is_not_there_is_named_unless_the_statement_allowed_it() {
    let (_dir, server) = running();

    let refused = query_outcome(server.port, "DROP SNAPSHOT nosuch").expect_err("refused");
    assert!(refused.contains("42704"), "{refused}");
    assert!(refused.contains("SHOW SNAPSHOTS"), "it names how to find them: {refused}");

    query_outcome(server.port, "DROP SNAPSHOT IF EXISTS nosuch").expect("permitted");
}

#[test]
fn setting_a_snapshot_that_exists_is_honoured_rather_than_refused_or_ignored() {
    // This asserted a **refusal** until reading as of a snapshot was built, and the refusal was
    // right while it was: `ADR-0019` Decision 6 allows two answers for `SET SNAPSHOT` --- refuse
    // it, or honour it --- and accepting it as a no-op is the one thing it forbids, because a
    // caller who asked for one instant and was served the present has no way to tell.
    //
    // It is honoured now. The `0A000` refusal is gone because the reason for it is.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");

    let mut session = Session::open(server.port);
    session.run("SET SNAPSHOT = 'eod'").expect("honoured");
    session.run("SELECT id FROM sales.orders").expect("reads as of it");
    session.run("RESET SNAPSHOT").expect("reads the present again");
}

#[test]
fn another_show_still_reaches_the_layer_that_answers_it() {
    // This parser sits in front of every `SHOW`, `CREATE` and `DROP` a client sends, and a
    // catalogue-browsing driver sends several on connection. One answered here would break the
    // client to answer a question it never asked.
    let (_dir, server) = running();

    assert!(query_outcome(server.port, "SHOW server_version_num").is_ok());
    assert!(query_outcome(server.port, "SHOW FEEDS").is_ok());
    assert!(query_outcome(server.port, "SHOW DEPENDENTS OF sales.orders").is_ok());
    query_outcome(server.port, "CREATE TABLE q3 CLONE sales.orders").expect("clone DDL");
    query_outcome(server.port, "DROP TABLE sales.q3").expect("clone drop");
}

#[test]
fn a_snapshot_survives_a_restart() {
    // Durable rather than in memory, because the overnight run that quotes a snapshot is not
    // the process that took it. In the warehouse rather than beside it, so a backup that copied
    // the tables carries it too.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);

    {
        let server = start(&warehouse, &dir.path().join("data"));
        query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");
    }

    let server = start(&warehouse, &dir.path().join("data"));
    let listed = text_rows(server.port, "SHOW SNAPSHOTS");
    assert_eq!(listed.len(), 1, "the snapshot did not survive the restart");
    assert_eq!(listed[0][0].as_deref(), Some("eod"));
}

#[test]
fn what_a_snapshot_pins_reaches_the_sweeper() {
    // The load-bearing half, and the one that was missing. A snapshot that pins nothing is a
    // promise the system does not keep --- the reports it exists to reproduce would stop
    // reproducing when the sweeper reclaimed what they read.
    //
    // The running server told its sweeper **nothing**: `Maintainer::among` existed and only a
    // soak test called it, so the maintenance thread ran with an empty lineage set. An empty
    // set pins nothing, which means a *clone's* files were reclaimable too. Snapshots would
    // have arrived into the same hole.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");

    // Asked of the same function the maintenance thread reads, so this is the sweeper's own
    // input rather than a second calculation that could agree by accident.
    let listed = text_rows(server.port, "SHOW SNAPSHOTS");
    let pinned: usize = listed[0][5]
        .as_ref()
        .and_then(|count| count.parse().ok())
        .expect("a count of pinned tables");
    assert!(pinned > 0, "the snapshot pinned no tables at all: {listed:?}");

    // And dropping it releases them, so a snapshot is not a one-way ratchet on storage.
    query_outcome(server.port, "DROP SNAPSHOT eod").expect("dropping");
    assert!(text_rows(server.port, "SHOW SNAPSHOTS").is_empty());
}

#[test]
fn an_expired_snapshot_pins_nothing() {
    // The whole point of the expiry. A snapshot that went on pinning after its day would be
    // `RSK-35` at warehouse scale --- storage held forever by a name somebody typed once --- and
    // the mandatory lifetime would be decoration.
    //
    // Tested directly rather than through the server, because the difference is a *day* and an
    // integration test cannot wait one. `pinned_versions` takes `today` as a parameter for
    // exactly this reason: a component that reads a clock cannot be replayed, and this one
    // decides whether files may be reclaimed.
    use std::collections::BTreeMap;
    use sankhya_snapshot::model::{Pinned, Snapshot};

    let taken = Snapshot::new(
        "eod",
        "ana",
        0,
        130,
        BTreeMap::from([("sales.orders".to_owned(), Pinned { version: 412 })]),
    );

    let live = snapshots::pinned_versions(std::slice::from_ref(&taken), 130);
    assert_eq!(
        live.get("sales.orders").map(Vec::as_slice),
        Some([412].as_slice()),
        "a snapshot still live on its last day pinned nothing"
    );

    let after = snapshots::pinned_versions(std::slice::from_ref(&taken), 131);
    assert!(
        after.is_empty(),
        "an expired snapshot went on pinning: {after:?}"
    );
}

#[test]
fn two_snapshots_pinning_one_table_contribute_both_versions() {
    // Reclamation asks one question and two things can answer yes. A union that dropped one
    // answer would delete a file the other still reads.
    use std::collections::BTreeMap;
    use sankhya_snapshot::model::{Pinned, Snapshot};

    let monday = Snapshot::new(
        "mon",
        "ana",
        0,
        200,
        BTreeMap::from([("sales.orders".to_owned(), Pinned { version: 412 })]),
    );
    let tuesday = Snapshot::new(
        "tue",
        "bo",
        0,
        200,
        BTreeMap::from([("sales.orders".to_owned(), Pinned { version: 500 })]),
    );

    let pinned = snapshots::pinned_versions(&[monday, tuesday], 100);
    let versions = pinned.get("sales.orders").expect("the table is pinned");
    assert!(versions.contains(&412), "{versions:?}");
    assert!(versions.contains(&500), "{versions:?}");
}

// --- reading as of one -----------------------------------------------------

#[test]
fn reading_as_of_a_snapshot_reads_the_past_and_not_the_present() {
    // The property the whole feature exists for, and the only test that can tell the two
    // apart: the table must **change** after the snapshot is taken. A test over an unchanging
    // warehouse passes whether the snapshot is honoured or ignored.
    use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use sankhya_publish::Publication;
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let before: usize = query_outcome(server.port, "SELECT id FROM sales.orders").expect("reads");
    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");

    // A commit *after* the snapshot, written through the product's own writer.
    let columns = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("period", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, false),
        Field::new("margin_pct", DataType::Float64, false),
    ]));
    let publication = Publication::external(warehouse.join("sales").join("orders"), "orders");
    let ids: Vec<i64> = (10_000..10_050).collect();
    #[allow(clippy::cast_precision_loss)]
    let amounts: Vec<f64> = ids.iter().map(|id| *id as f64).collect();
    let batch = RecordBatch::try_new(
        Arc::clone(&columns),
        vec![
            Arc::new(Int64Array::from(ids.clone())),
            Arc::new(StringArray::from(vec![Some("north"); ids.len()])),
            Arc::new(StringArray::from(vec![Some("q3"); ids.len()])),
            Arc::new(Float64Array::from(amounts.clone())),
            Arc::new(Float64Array::from(amounts)),
        ],
    )
    .expect("a batch");
    publication
        .append(
            publication.next_version(),
            "part-0004.parquet",
            &batch,
            sankhya_types::Lsn::new(5_000),
        )
        .expect("publishing after the snapshot");

    let after: usize = query_outcome(server.port, "SELECT id FROM sales.orders").expect("reads");
    assert!(after > before, "the fixture did not change: {before} then {after}");

    // And now the point --- on **one connection**, because a session setting is per connection
    // and a helper that reconnects between statements would pass whether it is honoured or not.
    let mut session = Session::open(server.port);
    session.run("SET SNAPSHOT = 'eod'").expect("setting");
    let as_of = session.run("SELECT id FROM sales.orders").expect("reads");
    assert_eq!(
        as_of, before,
        "reading as of the snapshot saw rows committed after it: {as_of} against {before}"
    );

    // `RESET` reads the present again, so a session is not stuck in the past.
    session.run("RESET SNAPSHOT").expect("resetting");
    assert_eq!(session.run("SELECT id FROM sales.orders").expect("reads"), after);

    // And a *different* connection was never in the past at all.
    assert_eq!(
        query_outcome(server.port, "SELECT id FROM sales.orders").expect("reads"),
        after,
        "a session setting leaked to another connection"
    );
}

#[test]
fn setting_a_snapshot_that_does_not_exist_is_refused_at_the_set() {
    // Not at the next statement. A `SET` that succeeded and a query that then failed sends
    // somebody to look at the query --- the same reasoning `ADR-0017` Decision 5 applies to
    // version skew: fail where a person can act, not where the consequence is noticed.
    let (_dir, server) = running();

    let refused = query_outcome(server.port, "SET SNAPSHOT = 'nosuch'").expect_err("refused");
    assert!(refused.contains("42704"), "{refused}");
    assert!(refused.contains("SHOW SNAPSHOTS"), "{refused}");

    // And the session was not changed by a statement that was refused --- on **one**
    // connection, because a helper that reconnects would discard the setting either way and
    // pass whether the refusal took effect or not.
    let mut session = Session::open(server.port);
    session
        .run("SET SNAPSHOT = 'nosuch'")
        .expect_err("refused on this session too");
    session
        .run("SELECT id FROM sales.orders")
        .expect("a refused SET must leave the session reading the present");
}

#[test]
fn a_snapshot_setting_with_no_name_says_what_the_statement_reads() {
    let (_dir, server) = running();
    let refused = query_outcome(server.port, "SET SNAPSHOT").expect_err("refused");
    assert!(refused.contains("needs the name of a snapshot"), "{refused}");
    assert!(refused.contains("RESET SNAPSHOT"), "it names the way back: {refused}");
}
