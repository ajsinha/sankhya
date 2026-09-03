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
#[path = "../src/cubes.rs"]
mod cubes;
#[path = "../src/aggregations.rs"]
mod aggregations;
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

/// Publish `rows` more orders, through the product's own writer.
///
/// A commit written by hand would encode the storage layout into the fixture and go on
/// encoding the *old* layout after it changed --- so a test whose fixture cannot have the
/// write path's bug is testing less than it looks like it is.
fn append_orders(warehouse: &std::path::Path, rows: i64) {
    use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use sankhya_publish::Publication;
    use std::sync::Arc;

    let columns = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("period", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, false),
        Field::new("margin_pct", DataType::Float64, false),
    ]));
    let publication = Publication::external(warehouse.join("sales").join("orders"), "orders");
    let ids: Vec<i64> = (10_000..10_000 + rows).collect();
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
    let version = publication.next_version();
    publication
        .append(
            version,
            &format!("part-{version:04}.parquet"),
            &batch,
            sankhya_types::Lsn::new(5_000 + u64::try_from(rows).unwrap_or(0)),
        )
        .expect("publishing after the snapshot");
}

#[test]
fn reading_as_of_a_snapshot_reads_the_past_and_not_the_present() {
    // The property the whole feature exists for, and the only test that can tell the two
    // apart: the table must **change** after the snapshot is taken. A test over an unchanging
    // warehouse passes whether the snapshot is honoured or ignored.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let before: usize = query_outcome(server.port, "SELECT id FROM sales.orders").expect("reads");
    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");

    // A commit *after* the snapshot, written through the product's own writer.
    append_orders(&warehouse, 50);

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
fn one_snapshot_holds_two_tables_at_one_instant_while_both_move_on() {
    // **The property that makes a snapshot not a clone**, and nothing demonstrated it. The
    // test above moves *one* table, which proves the setting is honoured and says nothing
    // about the thing the feature exists for: a run that reads a population, a set of rates,
    // a set of curves and a hierarchy must read all of them as of one instant, or the
    // reconciliation problem this system exists to remove reappears inside a single query.
    //
    // A snapshot that pinned each table at whatever version it happened to reach would be a
    // clone with extra steps, and one table cannot tell the two apart.
    use arrow_array::{RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use sankhya_publish::Publication;
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let orders_before = query_outcome(server.port, "SELECT id FROM sales.orders").expect("reads");
    let regions_before =
        query_outcome(server.port, "SELECT region FROM sales.regions").expect("reads");
    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");

    // **Both** tables move after it, and by different amounts, so a reader that answered one
    // from the past and one from the present is visible in the numbers rather than only in a
    // total that happens to match.
    append_orders(&warehouse, 60);
    let members = Arc::new(Schema::new(vec![
        Field::new("region", DataType::Utf8, false),
        Field::new("area", DataType::Utf8, false),
    ]));
    let regions = Publication::external(warehouse.join("sales").join("regions"), "regions");
    let batch = RecordBatch::try_new(
        Arc::clone(&members),
        vec![
            Arc::new(StringArray::from(vec!["east", "west"])),
            Arc::new(StringArray::from(vec!["east", "west"])),
        ],
    )
    .expect("a batch");
    regions
        .append(
            regions.next_version(),
            "part-0001.parquet",
            &batch,
            sankhya_types::Lsn::new(9_000),
        )
        .expect("publishing members after the snapshot");

    let orders_now = query_outcome(server.port, "SELECT id FROM sales.orders").expect("reads");
    let regions_now =
        query_outcome(server.port, "SELECT region FROM sales.regions").expect("reads");
    assert!(orders_now > orders_before, "the fact table did not move");
    assert!(regions_now > regions_before, "the dimension table did not move");

    // One connection, one setting, two tables.
    let mut session = Session::open(server.port);
    session.run("SET SNAPSHOT = 'eod'").expect("setting");
    assert_eq!(
        session.run("SELECT id FROM sales.orders").expect("reads"),
        orders_before,
        "the fact table was answered from the present"
    );
    assert_eq!(
        session.run("SELECT region FROM sales.regions").expect("reads"),
        regions_before,
        "the dimension table was answered from the present --- which is the failure a snapshot \
         exists to prevent, arriving through the mechanism meant to prevent it"
    );

    // And a join across the two, which is where a mixed instant does its real damage: it
    // returns rows, and they look like an answer.
    assert_eq!(
        session
            .run(
                "SELECT o.id FROM sales.orders o JOIN sales.regions r ON o.region = r.region"
            )
            .expect("reads"),
        session
            .run("SELECT id FROM sales.orders WHERE region IN ('north', 'south')")
            .expect("reads"),
        "the join saw members that did not exist when the snapshot was taken"
    );

    session.run("RESET SNAPSHOT").expect("resetting");
    assert_eq!(session.run("SELECT id FROM sales.orders").expect("reads"), orders_now);
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

// --- history, and reading one table at a version ---------------------------

#[test]
fn a_tables_history_lists_its_commits_and_what_each_did() {
    // The surface that makes pinning legible. Without it a person cannot see which versions
    // exist in order to reason about which to keep --- and "which to keep" is the whole of the
    // snapshot decision.
    let (_dir, server) = running();

    let history = text_rows(server.port, "SHOW HISTORY OF sales.orders");
    assert!(history.len() >= 5, "the fixture has several commits: {history:?}");
    assert_eq!(history[0][0].as_deref(), Some("0"), "oldest first");
    assert_eq!(history[0][1].as_deref(), Some("created"), "the first commit declares a schema");
    assert!(
        history.iter().any(|row| row[1].as_deref() == Some("appended")),
        "no commit was reported as an append: {history:?}"
    );

    // The column that keeps this honest: a commit in the log is not data on disk.
    assert!(
        history[0].len() >= 8,
        "the history must say what is keeping a version alive: {history:?}"
    );
}

#[test]
fn a_history_says_which_versions_something_is_keeping_alive() {
    // A commit remaining in the log is **not** the same as its data remaining on disk.
    // Retirement deletes what a merge replaced once nothing references it, so a history that
    // did not say what is pinned would invite somebody to read a version that is gone.
    let (_dir, server) = running();

    let before = text_rows(server.port, "SHOW HISTORY OF sales.orders");
    assert!(
        before.iter().all(|row| row[7].as_deref() == Some("")),
        "nothing is pinned yet: {before:?}"
    );

    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 30 DAYS").expect("taking");

    let after = text_rows(server.port, "SHOW HISTORY OF sales.orders");
    // **By name.** The column once said the word `snapshot`, which is true and useless: a
    // person reading it is deciding what to drop to release the storage, and on a warehouse
    // with a dozen snapshots that answer sends them to `SHOW SNAPSHOTS` to work out which.
    assert!(
        after.iter().any(|row| row[7].as_deref() == Some("eod")),
        "the snapshot pinned a version and the history does not name it: {after:?}"
    );

    // And a clone keeps a version alive exactly as a snapshot does --- reclamation asks one
    // question and both answer yes, so both appear in one column.
    query_outcome(server.port, "CREATE TABLE sales.frozen CLONE sales.orders").expect("cloning");
    let cloned = text_rows(server.port, "SHOW HISTORY OF sales.orders");
    assert!(
        cloned
            .iter()
            .any(|row| row[7].as_deref().is_some_and(|kept| kept.contains("sales.frozen"))),
        "a clone keeps a version alive and the history does not name it: {cloned:?}"
    );
}

#[test]
fn a_history_of_a_table_that_does_not_exist_is_refused_by_name() {
    let (_dir, server) = running();
    let refused = query_outcome(server.port, "SHOW HISTORY OF nosuch").expect_err("refused");
    assert!(refused.contains("42P01"), "{refused}");
    assert!(refused.contains("nosuch"), "{refused}");
}

#[test]
fn one_table_can_be_read_at_a_version_without_a_snapshot() {
    // The other half of what a person means by "look at the past": a version, not a tag. Only
    // the table named is resolved differently --- everything else reads the present, because
    // that is what the session asked for.
    let (_dir, server) = running();

    let mut session = Session::open(server.port);
    let now = session.run("SELECT id FROM sales.orders").expect("reads");

    session
        .run("SET VERSION OF sales.orders = 2")
        .expect("a version whose files are still there");
    let older = session.run("SELECT id FROM sales.orders").expect("reads");
    assert!(
        older < now,
        "reading at version 2 saw as much as the present: {older} against {now}"
    );

    session.run("RESET VERSION OF sales.orders").expect("resetting");
    assert_eq!(session.run("SELECT id FROM sales.orders").expect("reads"), now);
}

#[test]
fn a_version_the_table_does_not_have_is_refused_rather_than_answered_with_the_newest() {
    let (_dir, server) = running();

    let mut session = Session::open(server.port);
    // A version beyond the log. `live_files_at` replays *up to* a version and stops, so this
    // silently answered with the newest --- a version nobody has, served as though they had it.
    let refused = session
        .run("SET VERSION OF sales.orders = 9999")
        .expect_err("a version this table does not have");
    assert!(refused.contains("42704"), "{refused}");
    assert!(refused.contains("no version 9999"), "{refused}");
    assert!(
        refused.contains("SHOW HISTORY OF"),
        "the refusal names how to see what it does have: {refused}"
    );

    // And the session was not changed by a statement that was refused.
    session
        .run("SELECT id FROM sales.orders")
        .expect("a refused SET must leave the session reading the present");
}

#[test]
fn a_version_whose_files_were_reclaimed_is_refused_rather_than_answered_short() {
    // The rule a user must understand: **history is readable only where something is keeping
    // it alive.** Retirement deletes the files a merge replaced, so a version still listed in
    // the log may name files that are gone --- and resolving it anyway would answer a
    // historical query silently missing whatever had been reclaimed. The wrong answer that
    // looks most like a right one, because it has rows in it.
    // Staged **before the server starts**, and with the log primitive rather than the
    // maintainer: a test that ran a real compaction against a live table would be a second
    // writer, which is how a disk was once filled. Here the fixture is finished before
    // anything is serving it.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let root = warehouse.join("sales").join("orders");

    // A commit that lets a file go, exactly as a merge's removal half does --- after which
    // the present does not name it and version 1 still does.
    let now = sankhya_table_delta::live_files(&root).expect("the present resolves");
    let version = now.version.expect("a version") + 1;
    let released = now.files.first().expect("a file to release").path.clone();
    sankhya_table_delta::commit(
        &root,
        version,
        &[sankhya_table_delta::Action::Remove(
            sankhya_table_delta::RemoveFile::rewritten(released.clone(), 1),
        )],
    )
    .expect("releasing a file");

    // And then retirement takes it off the disk, which is the state this refusal is about.
    std::fs::remove_file(root.join(&released)).expect("reclaiming");

    let server = start(&warehouse, &dir.path().join("data"));
    let mut session = Session::open(server.port);
    let refused = session
        .run("SET VERSION OF sales.orders = 1")
        .expect_err("a version whose data is gone");
    assert!(refused.contains("42704"), "{refused}");
    assert!(
        refused.contains("is in the log and its data is not"),
        "the refusal separates the two --- the commit is there, the files are not: {refused}"
    );
    assert!(
        refused.contains("snapshot") || refused.contains("clone"),
        "and it names what would have kept it alive: {refused}"
    );

    // The present is unaffected. Retirement took an old version's files, not the table.
    session.run("SELECT id FROM sales.orders").expect("the present still reads");
}

#[test]
fn a_leading_comment_does_not_hide_a_statement_this_server_implements() {
    // Every statement this server defines itself --- `SHOW FEEDS`, `CREATE SNAPSHOT`,
    // `SHOW HISTORY OF`, `CREATE TABLE ... CLONE`, `CREATE CUBE`, `SET VERSION OF` --- is
    // recognised by matching the start of the text, because none of them is SQL and nothing
    // downstream will accept them. Matched against the *raw* text, one `--` line made the
    // server fail to recognise its own statement and answer with a syntax error.
    //
    // Commenting a statement is not exotic. Every example this repository ships does it, every
    // migration tool does it, and a person explaining a `CREATE CUBE` does it. The feature
    // worked only for somebody who did not write down what they were doing.
    let (_dir, server) = running();
    let mut session = Session::open(server.port);

    for sql in [
        "-- take one\nCREATE SNAPSHOT commented EXPIRE AFTER 7 DAYS",
        "-- what is there\nSHOW SNAPSHOTS",
        "/* a block comment */ SHOW HISTORY OF sales.orders",
        "-- one\n-- two\nSHOW FEEDS",
        "/* a /* nested */ comment */ SHOW SNAPSHOTS",
        "\t  \n-- indented, after a blank line\nSHOW LINEAGE OF sales.orders",
        "-- and away\nDROP SNAPSHOT commented",
    ] {
        session
            .run(sql)
            .unwrap_or_else(|refused| panic!("`{sql}` was refused: {refused}"));
    }

    // A comment and nothing else is still nothing to run, and says so rather than being
    // claimed by whichever handler happens to match an empty string.
    assert!(session.run("-- only a comment").is_err());
}
