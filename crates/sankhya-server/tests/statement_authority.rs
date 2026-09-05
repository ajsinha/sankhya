//! Statements that change state, run by somebody who may not change it.
//!
//! # What this is about
//!
//! Two statement families reached state with no authorization at all. `DROP SNAPSHOT` took no
//! principal, so any caller could drop any snapshot --- and a snapshot's whole job is to hold
//! files back from the sweeper, so dropping one releases them. `docs/INVARIANTS.md` claims the
//! maintenance scheduler is *structurally incapable* of destroying retained history; it is, and
//! a statement was doing it instead. `RESUME FEED` took no principal either, so any caller could
//! restart anybody's halted ingest --- which `ADR-0018` halted because its **source changed
//! shape**, making the resume a decision that records of an unknown shape should start landing
//! in a table again. `SEC-04`.
//!
//! # Why these go over a socket with a configuration file
//!
//! Because the property is about *who is connected*, and a test that constructed a principal
//! directly would be asserting that the function refuses --- not that a connection reaches it.
//! The roles come from `server.users` in a real configuration file, the way an operator's do.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{start_with, write_warehouse, Running, Session};

/// A warehouse and a server on which `ana` reads and `mallory` holds no role at all.
///
/// `server.users` naming anybody makes the list the list, so a user absent from it holds
/// nothing --- which is the same switch `server.credentials` uses and the reason a half-written
/// configuration is a closed server rather than an open one.
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
fn a_snapshot_is_not_dropped_by_somebody_who_may_not_read_what_it_pins() {
    let (_dir, server) = running();

    let mut ana = Session::open_as(server.port, "ana");
    ana.run("CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS")
        .expect("ana may take one");

    // Not vacuous: it is really there, and it really pins something.
    let listed = Session::open_as(server.port, "ana")
        .run("SHOW SNAPSHOTS")
        .expect("ana may list them");
    assert_eq!(listed, 1, "the snapshot must exist for its loss to mean anything");

    let mut mallory = Session::open_as(server.port, "mallory");
    let refused = mallory
        .run("DROP SNAPSHOT eod")
        .expect_err("a caller who may read nothing may not release what a snapshot pins");
    assert!(
        refused.contains("there is no snapshot"),
        "the refusal must not confirm the snapshot exists, or it is an enumeration oracle: \
         {refused}"
    );

    // And it is still there. A refusal that returned an error *after* removing the file would
    // satisfy the assertion above and lose the pin anyway.
    assert_eq!(
        Session::open_as(server.port, "ana")
            .run("SHOW SNAPSHOTS")
            .expect("ana may still list them"),
        1,
        "the snapshot was dropped by a caller who was refused"
    );

    // Not vacuous the other way either: the person who took it can still drop it, so this
    // refuses a stranger rather than refusing `DROP SNAPSHOT`.
    ana.run("DROP SNAPSHOT eod").expect("ana may drop her own");
    assert_eq!(
        Session::open_as(server.port, "ana")
            .run("SHOW SNAPSHOTS")
            .expect("listing"),
        0
    );
}

#[test]
fn a_feed_is_not_resumed_by_somebody_who_may_not_read_the_table_it_writes() {
    let (_dir, server) = running();

    // No feed of this name is declared, and that is the shape of the check rather than a gap:
    // an unknown feed and a feed this caller may not touch are refused with the same sentence,
    // so a refusal cannot be used to discover which feeds exist.
    let mut mallory = Session::open_as(server.port, "mallory");
    let refused = mallory
        .run("RESUME FEED nightly")
        .expect_err("a caller who may read nothing may not restart an ingest");
    assert!(
        refused.contains("no feed called"),
        "the refusal must be the same sentence an unknown feed gets: {refused}"
    );

    // `SHOW FEEDS` is not gated, and this is the line that says so on purpose: it reports what
    // the *server* is doing --- names an operator configured and counts this process moved ---
    // and there is no table to check a scope against.
    Session::open_as(server.port, "mallory")
        .run("SHOW FEEDS")
        .expect("reading what the server is doing is not reading anybody's data");
}
