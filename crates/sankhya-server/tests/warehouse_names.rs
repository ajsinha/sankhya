//! Resolving the name a client used to the table it means.
//!
//! # The defect these exist for
//!
//! Discovery reads `<schema>/<table>/` and a session registers each table under its **bare**
//! name, so a client says `orders` and means `sales/orders`. Cloning resolved the same name one
//! level up, as `warehouse/orders`, which is a directory no deployment has --- so
//! `CREATE TABLE ... CLONE` could only name a table this server does not serve.
//!
//! It was tested against a warehouse whose tables sat at the root, so every test passed. The
//! feature had never worked against a table anybody could query, and nothing could see it,
//! because the fixture's shape was not the product's shape.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_schema::{DataType, Field, Schema};
use sankhya_publish::Publication;
use std::sync::Arc;

// `warehouse` names `execute::ServableTable`, and `execute` names `wiring`, so all three come
// in. That is the cost of a composition root living in a binary crate, and it is cheaper than
// the alternative of never testing it.
#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/adopt.rs"]
mod adopt;
#[path = "../src/clones.rs"]
mod clones;
#[path = "../src/feeds.rs"]
mod feeds;
#[path = "../src/driver.rs"]
mod driver;
#[path = "../src/snapshots.rs"]
mod snapshots;
#[path = "../src/wiring.rs"]
mod wiring;
#[path = "../src/warehouse.rs"]
mod warehouse;

use warehouse::{place_beside, qualified_name, resolve, Misplaced, Resolved};

/// Create an empty table at `<schema>/<table>`, through the product's own writer.
fn table(root: &std::path::Path, schema: &str, name: &str) {
    let columns = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    Publication::external(root.join(schema).join(name), name)
        .create(&columns)
        .expect("creating");
}

#[test]
fn a_bare_name_finds_the_table_a_session_registered_under_it() {
    let dir = tempfile::tempdir().expect("a directory");
    table(dir.path(), "sales", "orders");

    assert_eq!(
        resolve(dir.path(), "orders"),
        Resolved::One(dir.path().join("sales").join("orders"))
    );
}

#[test]
fn a_qualified_name_names_its_own_schema() {
    let dir = tempfile::tempdir().expect("a directory");
    table(dir.path(), "sales", "orders");

    assert_eq!(
        resolve(dir.path(), "sales.orders"),
        Resolved::One(dir.path().join("sales").join("orders"))
    );
    // And a schema that does not hold it is absent rather than a search elsewhere: a qualified
    // name is a statement about where the table is, not a hint.
    assert_eq!(resolve(dir.path(), "finance.orders"), Resolved::Absent);
}

#[test]
fn a_name_two_schemas_hold_is_ambiguous_rather_than_the_first_one_found() {
    // Picking one would clone the wrong table the day a warehouse grew a second `orders`, and
    // it would do it silently and correctly-looking.
    let dir = tempfile::tempdir().expect("a directory");
    table(dir.path(), "sales", "orders");
    table(dir.path(), "finance", "orders");

    assert_eq!(
        resolve(dir.path(), "orders"),
        Resolved::Ambiguous(vec!["finance.orders".to_owned(), "sales.orders".to_owned()]),
        "both named, in a stable order, so the caller can say which"
    );
}

#[test]
fn a_directory_without_a_log_is_not_a_table() {
    // Object stores hold all sorts of things. A directory that is not a table must not be
    // resolvable as one, or a clone would be made of whatever happened to be there.
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::create_dir_all(dir.path().join("sales").join("orders")).expect("a directory");

    assert_eq!(resolve(dir.path(), "orders"), Resolved::Absent);
}

#[test]
fn a_bookkeeping_schema_is_not_searched() {
    // `_`-prefixed schemas hold cube definitions and materialised cuboids, exactly as
    // `discover` treats them. A clone of a materialised cuboid is not a thing.
    let dir = tempfile::tempdir().expect("a directory");
    table(dir.path(), "_cubes", "orders");

    assert_eq!(resolve(dir.path(), "orders"), Resolved::Absent);
}

#[test]
fn a_warehouse_that_does_not_exist_resolves_nothing_rather_than_failing() {
    assert_eq!(
        resolve(std::path::Path::new("/nonexistent-warehouse"), "orders"),
        Resolved::Absent
    );
}

#[test]
fn a_new_clone_lands_beside_the_table_it_came_from() {
    // The only answer that needs no guess. A clone put in a default schema would sit somewhere
    // its origin is not, and the first thing anybody does with a clone is compare the two.
    let dir = tempfile::tempdir().expect("a directory");
    let origin = dir.path().join("sales").join("orders");

    assert_eq!(
        place_beside(dir.path(), "q3_frozen", &origin),
        Ok(dir.path().join("sales").join("q3_frozen"))
    );
}

#[test]
fn a_qualified_name_naming_the_origins_own_schema_is_the_same_placement() {
    // Saying out loud where it was going anyway is not a different statement.
    let dir = tempfile::tempdir().expect("a directory");
    let origin = dir.path().join("sales").join("orders");

    assert_eq!(
        place_beside(dir.path(), "sales.q3_frozen", &origin),
        Ok(dir.path().join("sales").join("q3_frozen"))
    );
}

#[test]
fn a_clone_may_not_be_placed_in_another_schema() {
    // A clone is a reference to its origin's files and is authorized through them. One placed
    // under another schema would have its name governed by one policy and its data by another,
    // and nobody could then say which rule applies to it.
    let dir = tempfile::tempdir().expect("a directory");
    let origin = dir.path().join("sales").join("orders");

    let refused = place_beside(dir.path(), "archive.q3_frozen", &origin)
        .expect_err("another schema is refused");
    assert_eq!(
        refused,
        Misplaced::OtherSchema { asked: "archive".to_owned(), origin: "sales".to_owned() }
    );
    assert!(
        refused.to_string().contains("stays in its origin's schema"),
        "{refused}"
    );
}

#[test]
fn a_root_is_rendered_as_the_name_a_message_can_be_unambiguous_about() {
    let dir = tempfile::tempdir().expect("a directory");

    assert_eq!(
        qualified_name(dir.path(), &dir.path().join("sales").join("orders")),
        Some("sales.orders".to_owned())
    );
    // A table at the warehouse root has no schema to name, and inventing one would be a name
    // that does not resolve.
    assert_eq!(
        qualified_name(dir.path(), &dir.path().join("orders")),
        Some("orders".to_owned())
    );
}
