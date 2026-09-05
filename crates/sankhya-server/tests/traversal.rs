//! A name in a statement is not a path, and three statement families used to treat it as one.
//!
//! # What this is about
//!
//! `warehouse.join(DIRECTORY).join(format!("{name}.json"))`, over a name a client typed, with
//! no constraint on that name beyond being non-empty and whitespace-free. `Path::join` replaces
//! the whole path when the component is absolute and honours `..` when it is not, so a name was
//! a way to write and delete files anywhere the server's user could reach. `SEC-06`.
//!
//! # Why these go over a socket
//!
//! Because a unit test on the path builder proves the builder refuses. It does not prove the
//! statement cannot reach a builder that does not --- which is what was wrong: the check for a
//! usable name existed in `sankhya-clone` and in none of the three places that needed it.
//!
//! # Why nothing here targets a path outside the test's own directory
//!
//! A test that proves a traversal is refused by *attempting* one has to attempt a real write,
//! and a test that is wrong writes wherever it aimed. Every target here is inside the warehouse
//! or its parent temporary directory, so the test that fails leaves a stray file in a directory
//! that is about to be deleted rather than somewhere on the machine.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{
    query_outcome, start, start_with_user_functions, text_rows, write_warehouse, Running,
};

/// A body that is a valid aggregation, so a refusal below is about the name and nothing else.
///
/// The traversal cases use it too. A malformed body would have them refused whatever the name
/// check did, which is the shape of vacuous test this file exists to avoid.
const BODY: &str = "LANGUAGE PYTHON AS $$
def initial():
    return {'n': 0}

def accumulate(state, values):
    state['n'] += len(values)
    return state

def finish(state):
    return float(state['n'])
$$";

/// The bookkeeping directory a traversal climbs out of, made to exist.
///
/// Without this the escape fails on `ENOENT` rather than on the check, because the kernel
/// resolves `_snapshots/../_cubes` by walking `_snapshots` first --- so the test passes on a
/// server with no check at all, which is the shape of vacuous test this file exists to avoid.
/// A warehouse in service has these directories; a fresh one does not, and the difference is
/// not the property under test.
fn bookkeeping(warehouse: &std::path::Path, directory: &str) {
    std::fs::create_dir_all(warehouse.join(directory)).expect("the bookkeeping directory");
}

/// A warehouse with the sample tables, and a running server.
fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

/// The names that used to reach a path. Relative ones climb out of the bookkeeping directory;
/// the absolute one replaces the path entirely, which is the half `..` checking would miss.
fn traversals(inside: &std::path::Path) -> Vec<String> {
    vec![
        "../_cubes/regional".to_string(),
        "..".to_string(),
        "../../escaped".to_string(),
        "sub/dir".to_string(),
        inside.join("planted").display().to_string(),
    ]
}

#[test]
fn a_snapshot_name_cannot_reach_another_directory() {
    let (dir, server) = running();
    let warehouse = dir.path().join("warehouse");

    // Something worth deleting, in the directory the relative names climb into. A snapshot
    // document is the worst target of the three and this is why: an absent snapshot pins
    // nothing, so deleting one releases the files the sweeper was holding back --- the
    // deletion the whole retention mechanism exists to prevent.
    let cubes = warehouse.join("_cubes");
    std::fs::create_dir_all(&cubes).expect("the cube directory");
    let victim = cubes.join("regional.json");
    std::fs::write(&victim, "{}").expect("a cube definition to aim at");
    bookkeeping(&warehouse, "_snapshots");

    for name in traversals(dir.path()) {
        let taken = query_outcome(server.port, &format!("CREATE SNAPSHOT {name}"));
        assert!(
            taken.is_err(),
            "`CREATE SNAPSHOT {name}` was accepted, and a name is not a path: {taken:?}"
        );
        let dropped = query_outcome(server.port, &format!("DROP SNAPSHOT {name}"));
        assert!(
            dropped.is_err(),
            "`DROP SNAPSHOT {name}` was accepted, and a name is not a path: {dropped:?}"
        );
    }

    assert!(
        victim.exists(),
        "a `DROP SNAPSHOT` deleted a cube definition, which is `SEC-06` exactly"
    );
    assert!(
        !dir.path().join("planted.json").exists(),
        "a `CREATE SNAPSHOT` wrote outside the warehouse"
    );

    // Not vacuous: an ordinary name still works, so this refuses traversals rather than
    // refusing `CREATE SNAPSHOT`.
    query_outcome(server.port, "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS").expect("taking");
    assert_eq!(text_rows(server.port, "SHOW SNAPSHOTS").len(), 1);
    query_outcome(server.port, "DROP SNAPSHOT eod").expect("dropping");
}

/// The shape of a valid cube over the sample warehouse, so a refusal is about the name.
fn create_cube(name: &str) -> String {
    format!(
        "CREATE CUBE {name} FROM orders \
         DIMENSION region FROM regions ON region (LEVEL area = region) \
         MEASURE amount (SUM ALONG region)"
    )
}

#[test]
fn a_cube_name_cannot_reach_another_directory() {
    let (dir, server) = running();
    let warehouse = dir.path().join("warehouse");
    let snapshots = warehouse.join("_snapshots");
    std::fs::create_dir_all(&snapshots).expect("the snapshot directory");
    let victim = snapshots.join("regional.json");
    std::fs::write(&victim, "{}").expect("a snapshot document to aim at");
    bookkeeping(&warehouse, "_cubes");

    // `CREATE` rather than `DROP`, and the difference is worth naming. `DROP CUBE` looks the
    // more dangerous of the two and is not reachable: it refuses a cube that is not in the
    // served set before it builds any path at all, so a name that traverses is a name of no
    // cube. `CREATE` builds the path from whatever was typed, which is where the write goes.
    //
    // **Quoted**, and that is the whole reachability question. The cube tokenizer builds a bare
    // word out of alphanumerics and `_`, so `CREATE CUBE ../x` is a syntax error and looks
    // safe. A quoted identifier is copied verbatim --- which is what makes a cube called
    // `"Level"` expressible, and what carries a separator straight to the path builder.
    for name in traversals(dir.path()) {
        let created = query_outcome(server.port, &create_cube(&format!("\"{name}\"")));
        assert!(
            created.is_err(),
            "`CREATE CUBE \"{name}\"` was accepted, and a name is not a path: {created:?}"
        );
        // And unquoted, which the tokenizer should refuse on its own. Asserted rather than
        // assumed: the two paths are refused by different code and only one of them is the
        // check this test is about.
        let bare = query_outcome(server.port, &create_cube(&name));
        assert!(
            bare.is_err(),
            "`CREATE CUBE {name}` was accepted, and a name is not a path: {bare:?}"
        );
    }

    assert!(
        victim.exists(),
        "a cube definition was written over a snapshot document, releasing the files it was \
         holding back"
    );
    assert!(
        !dir.path().join("planted.json").exists(),
        "a `CREATE CUBE` wrote outside the warehouse"
    );

    // Not vacuous: an ordinary name is accepted, so this refuses traversals rather than
    // refusing `CREATE CUBE`.
    query_outcome(server.port, &create_cube("regional_sales")).expect("an ordinary cube name");
}

#[test]
fn an_aggregation_name_cannot_reach_another_directory() {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    // The door has to be open, or every statement below is refused for the other reason and
    // the test proves nothing about names.
    let server = start_with_user_functions(&warehouse, &dir.path().join("data"));

    let cubes = warehouse.join("_cubes");
    std::fs::create_dir_all(&cubes).expect("the cube directory");
    let victim = cubes.join("regional.json");
    std::fs::write(&victim, "{}").expect("a cube definition to aim at");
    bookkeeping(&warehouse, "_aggregations");

    for name in traversals(dir.path()) {
        let created = query_outcome(
            server.port,
            &format!("CREATE AGGREGATION {name} {BODY}"),
        );
        assert!(
            created.is_err(),
            "`CREATE AGGREGATION {name}` was accepted, and a name is not a path: {created:?}"
        );
        let dropped = query_outcome(server.port, &format!("DROP AGGREGATION {name}"));
        assert!(
            dropped.is_err(),
            "`DROP AGGREGATION {name}` was accepted, and a name is not a path: {dropped:?}"
        );
    }

    assert!(victim.exists(), "a `DROP AGGREGATION` deleted a cube definition");
    assert!(
        !dir.path().join("planted.json").exists(),
        "a `CREATE AGGREGATION` wrote outside the warehouse"
    );

    // Not vacuous: the door is open and an ordinary name is accepted through it.
    query_outcome(server.port, &format!("CREATE AGGREGATION tally {BODY}"))
        .expect("an ordinary aggregation is accepted");
}

#[test]
fn a_clone_name_cannot_reach_another_directory() {
    // The fourth site, and the one the audit did not name --- because it does not escape
    // upward, and the reason is an accident worth removing. `..` holds a dot, so a name
    // containing one is read as `schema.table` and refused for naming the wrong schema rather
    // than for traversing. That is a proof about `split_once('.')`, and it stops holding the
    // day the qualified form gets smarter.
    //
    // What is wrong without the check even today is `sub/dir`: it holds no dot, so it lands
    // the clone in a directory the catalogue does not scan --- a table that exists and cannot
    // be found.
    let (dir, server) = running();

    for name in traversals(dir.path()) {
        let cloned = query_outcome(
            server.port,
            &format!("CREATE TABLE \"{name}\" CLONE orders"),
        );
        assert!(
            cloned.is_err(),
            "`CREATE TABLE \"{name}\" CLONE orders` was accepted: {cloned:?}"
        );
    }
    assert!(
        !dir.path().join("planted").exists(),
        "a clone was placed outside the warehouse"
    );
    assert!(
        !dir.path().join("warehouse").join("sales").join("sub").exists(),
        "a clone was placed in a directory the catalogue does not scan"
    );

    // Not vacuous: an ordinary name still clones, so this refuses traversals rather than
    // refusing `CREATE TABLE ... CLONE`.
    query_outcome(server.port, "CREATE TABLE orders_copy CLONE orders")
        .expect("an ordinary clone name");
}
