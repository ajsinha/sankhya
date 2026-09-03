//! Two users, two roles, two different answers.
//!
//! # What was true until now
//!
//! The subject travelled inward correctly --- `authenticate` read the connection's user, refused
//! an empty one because an unattributable connection cannot be audited, and a `Caller` carried
//! it to the query path. And **every user was handed the same role**, a literal `reader`, so
//! nothing downstream could tell two subjects apart. The plumbing was finished and the feature
//! was not: an audit chain that differs by user, over an authorization decision that cannot.
//!
//! `STATUS` said so in as many words --- *authorization does not vary by subject* --- which is
//! the right way to carry an unfinished thing and is not a substitute for finishing it.
//!
//! # The rule, and why the map's presence is the switch
//!
//! An operator who has written down **no** users has not decided anything about roles, and gets
//! this build's behaviour: one role for everybody. An operator who has written down **one** has
//! decided that the list is the list, so a user absent from it holds no role and every rule that
//! grants by role passes them by.
//!
//! A separate flag would be a flag somebody forgets, and forgetting it in this direction grants
//! access. Adding a user is a change somebody notices; silently granting one is not.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{start_with, write_warehouse, Running};

fn running(users: &str) -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let config = dir.path().join("application.yaml");
    std::fs::write(&config, users).expect("a configuration");
    let server = start_with(
        &warehouse,
        &dir.path().join("data"),
        &[("SANKHYA_CONFIG", &config.display().to_string())],
    );
    (dir, server)
}

/// Whether this user can see the fixture's tables at all.
fn sees_tables(port: u16, user: &str) -> bool {
    let mut session = common::Session::open_as(port, user);
    session
        .run("SELECT count(*) FROM sales.orders")
        .is_ok()
}

#[test]
fn a_user_the_operator_named_reads_and_one_they_did_not_does_not() {
    // The whole property in one test. Both connect, both are authenticated, both are audited --
    // and only one of them can read, which is what *authorization varying by subject* means.
    let (_dir, server) = running("server:\n  users:\n    alice: reader\n");

    assert!(
        sees_tables(server.port, "alice"),
        "alice holds `reader` and the fixture's policy grants `reader`"
    );
    assert!(
        !sees_tables(server.port, "mallory"),
        "mallory holds no role, because the operator wrote a list and did not put her in it. \
         A user absent from a list somebody wrote down is a user they did not name"
    );
}

#[test]
fn with_no_users_configured_everybody_is_a_reader() {
    // The compatibility rule, and it is a decision rather than an oversight: an operator who
    // has written down nothing has not decided anything, and this is what every deployment
    // before today did. Made a test so that changing it is a change somebody has to make on
    // purpose.
    let (_dir, server) = running("server:\n  require_password: false\n");
    assert!(sees_tables(server.port, "anybody"), "no map, so the old behaviour");
    assert!(sees_tables(server.port, "somebody_else"), "for everybody");
}

#[test]
fn a_user_may_hold_several_roles() {
    // Comma-separated, for the reason every other list in this configuration is: a YAML
    // sequence and a scalar are different shapes to read, and one shape is fewer.
    let (_dir, server) = running("server:\n  users:\n    alice: analyst, reader\n");
    assert!(
        sees_tables(server.port, "alice"),
        "one of her roles matches the grant, which is enough"
    );
}

#[test]
fn a_role_that_grants_nothing_reads_nothing() {
    // The other direction, and the one that makes the test above mean something: a user with a
    // role the policy has never heard of is refused. Without this, `alice: reader` passing could
    // be a server that ignores roles entirely.
    let (_dir, server) = running("server:\n  users:\n    alice: bystander\n");
    assert!(
        !sees_tables(server.port, "alice"),
        "`bystander` is not a role the fixture's policy grants anything to"
    );
}
