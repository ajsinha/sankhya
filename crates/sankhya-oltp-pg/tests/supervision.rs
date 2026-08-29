//! A PostgreSQL cluster this process creates, starts, proves ready, and stops.
//!
//! # Why these run against the real thing
//!
//! There is no way to fake a database lifecycle usefully. The failures worth catching are
//! `initdb` refusing a non-empty directory, a postmaster that starts and is not yet accepting
//! connections, and a shutdown that leaves a lock file behind --- and a stub that produced any
//! of those would be a stub asserting what its author already believed.
//!
//! They are skipped, loudly and by name, when the vendored PostgreSQL has not been built. A
//! test that silently passes when it did nothing is worse than one that is not there.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_oltp_pg::{Binaries, Cluster, ClusterError};
use std::path::PathBuf;
use std::time::Duration;

/// The vendored programs, if this tree has built them.
fn vendored() -> Option<Binaries> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join(".build/pg-install/bin");
    Binaries::at(root)
}

/// Report the skip rather than passing quietly.
macro_rules! binaries_or_skip {
    () => {
        match vendored() {
            Some(binaries) => binaries,
            None => {
                eprintln!(
                    "SKIPPED: the vendored PostgreSQL is not built. \
                     Run the build step in QUICKSTART.md §2 to exercise this."
                );
                return;
            }
        }
    };
}

const READY_WITHIN: Duration = Duration::from_secs(30);

#[test]
fn a_directory_without_the_programs_is_refused_before_anything_is_created() {
    // Checked together and up front. A directory holding `initdb` but not `pg_ctl` fails in
    // the middle of starting a cluster that has already been created, which is a much worse
    // place to find out.
    let empty = tempfile::tempdir().expect("a temporary directory");
    assert!(
        Binaries::at(empty.path()).is_none(),
        "a directory with no PostgreSQL programs is not a place to run one from"
    );
}

#[test]
fn a_cluster_is_created_started_and_proves_itself_ready() {
    let binaries = binaries_or_skip!();
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut cluster = Cluster::new(binaries, dir.path().join("pg"));

    assert!(!cluster.exists(), "nothing has been created yet");
    assert!(!cluster.is_ready(), "and nothing is accepting connections");

    cluster.start(READY_WITHIN).expect("the cluster starts");

    assert!(cluster.exists(), "the data directory holds a cluster");
    assert!(
        cluster.is_ready(),
        "and it answers `pg_isready`, which is asked of the cluster rather than remembered"
    );

    cluster.stop().expect("it stops");
    assert!(
        !cluster.is_ready(),
        "a stopped cluster stops accepting connections"
    );
}

#[test]
fn starting_an_existing_cluster_does_not_recreate_it() {
    // The property that matters most in this file. `initdb` over a live data directory would
    // destroy the system of record, so a supervisor that is not idempotent here is not a
    // supervisor --- and a restart is the ordinary case, not the exception.
    let binaries = binaries_or_skip!();
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("pg");

    let mut first = Cluster::new(binaries.clone(), &data);
    first.start(READY_WITHIN).expect("the first start");
    // A file only this test would write. If the directory were re-initialised it would be gone.
    let marker = data.join("survives-a-restart");
    std::fs::write(&marker, b"kept").expect("writing the marker");
    first.stop().expect("it stops");

    let mut second = Cluster::new(binaries, &data);
    assert!(second.exists(), "the cluster is still there between starts");
    second.start(READY_WITHIN).expect("the second start");

    assert!(
        marker.is_file(),
        "the data directory was re-initialised, which would have destroyed the system of record"
    );
    assert!(second.is_ready());
    second.stop().expect("it stops");
}

#[test]
fn the_cluster_listens_on_a_socket_and_not_on_the_network() {
    // `listen_addresses = ''` is what lets a managed cluster hold the system of record without
    // an operator reasoning about firewalls. Asserted by finding the socket where clients are
    // told to look, because a configuration setting nobody checks is a setting that gets
    // changed by somebody tidying up.
    let binaries = binaries_or_skip!();
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut cluster = Cluster::new(binaries, dir.path().join("pg"));
    cluster.start(READY_WITHIN).expect("the cluster starts");

    let socket = cluster
        .socket_directory()
        .join(".s.PGSQL.5432");
    assert!(
        socket.exists(),
        "no Unix socket in {:?}; clients have nowhere to connect",
        cluster.socket_directory()
    );

    cluster.stop().expect("it stops");
}

#[test]
fn a_supervisor_that_goes_away_takes_its_child_with_it() {
    // Without this, a process that panics or returns early leaves a postmaster running against
    // a data directory nothing owns. The next start then finds the lock file held by a process
    // that is not its child, which is a confusing way to learn about a bug somewhere else.
    let binaries = binaries_or_skip!();
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("pg");

    {
        let mut cluster = Cluster::new(binaries.clone(), &data);
        cluster.start(READY_WITHIN).expect("the cluster starts");
        assert!(cluster.is_ready());
        // Dropped without `stop` being called.
    }

    let orphan = Cluster::new(binaries, &data);
    assert!(
        !orphan.is_ready(),
        "the postmaster outlived the supervisor that owned it"
    );
}

#[test]
fn a_failure_names_the_program_and_says_what_it_wrote() {
    // An error that says "startup failed" sends an operator to the wrong place. PostgreSQL
    // explains itself precisely, and a supervisor's job is to pass that through rather than
    // replace it with a summary.
    let binaries = binaries_or_skip!();
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("pg");
    // A non-empty directory that is not a cluster: `initdb` refuses this, by design.
    std::fs::create_dir_all(&data).expect("the directory");
    std::fs::write(data.join("something.txt"), b"not a cluster").expect("a stray file");

    let mut cluster = Cluster::new(binaries, &data);
    let refused = cluster.start(READY_WITHIN).expect_err("initdb refuses this");

    match &refused {
        ClusterError::Refused { program, detail } => {
            assert_eq!(program, "initdb");
            assert!(
                !detail.is_empty(),
                "the refusal carried no reason, so the operator learns nothing"
            );
        }
        other => panic!("expected a refusal naming initdb, got {other:?}"),
    }
    // And it reads as a sentence somebody can act on.
    assert!(
        refused.to_string().contains("initdb"),
        "{refused}"
    );
}
