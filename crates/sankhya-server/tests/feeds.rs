//! Loading declared feeds, and what a bad one costs the others.
//!
//! # The property worth stating
//!
//! One feed with a typo in it must not stop a server. The other feeds are somebody else's
//! data, and refusing to start would take an outage on every table to protect one --- which
//! is how a deployment ends up with the whole mechanism switched off.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;

// The feeds module is part of the binary, so the test builds it directly — the same pattern
// `tests/wiring.rs` uses. That is the cost of a composition root living in a binary crate,
// and it is cheaper than the alternative of never testing it.
// `feeds` names `wiring`'s refusal helpers, which name `execute` and `clones`. That is the
// cost of a composition root living in a binary crate, and it is cheaper than the alternative
// of never testing it.
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
#[path = "../src/snapshots.rs"]
mod snapshots;
#[path = "../src/audit.rs"]
mod audit;
#[path = "../src/wiring.rs"]
mod wiring;
#[path = "../src/feeds.rs"]
mod feeds;

/// Write a feed document under `config/feeds/`.
fn declare(configuration: &Path, name: &str, body: &str) {
    let directory = configuration.join("feeds");
    std::fs::create_dir_all(&directory).expect("a feeds directory");
    std::fs::write(directory.join(name), body).expect("a declaration");
}

const SOUND: &str = r#"
name: orders
from: /var/spool/orders
schema: sales
table: orders
date: ingest
columns:
  - name: id
    type: int64
  - name: amount
    from: total
    type: decimal(18,2)
"#;

#[test]
fn a_directory_with_no_feeds_is_not_a_complaint() {
    // The ordinary case for every deployment that has not declared one.
    let dir = tempfile::tempdir().expect("a directory");
    let (declared, complaints) = load(dir.path());

    assert!(declared.is_empty());
    assert!(complaints.is_empty(), "{complaints:?}");
}

#[test]
fn a_sound_declaration_loads_with_its_defaults() {
    let dir = tempfile::tempdir().expect("a directory");
    declare(dir.path(), "orders.yaml", SOUND);

    let (declared, complaints) = load(dir.path());

    assert!(complaints.is_empty(), "{complaints:?}");
    assert_eq!(declared.len(), 1);
    let feed = &declared[0];
    assert_eq!(feed.0, "orders");
    assert_eq!(feed.1, Path::new("/var/spool/orders"));
}

#[test]
fn one_broken_declaration_does_not_stop_the_others_loading() {
    let dir = tempfile::tempdir().expect("a directory");
    declare(dir.path(), "a-broken.yaml", "name: broken\nfrom: /spool\n");
    declare(dir.path(), "b-orders.yaml", SOUND);

    let (declared, complaints) = load(dir.path());

    assert_eq!(declared.len(), 1, "the sound one still loads");
    assert!(!complaints.is_empty(), "and the broken one is complained about");
    assert!(
        complaints.iter().any(|complaint| complaint.contains("a-broken.yaml")),
        "the complaint names the file: {complaints:?}"
    );
}

#[test]
fn a_declaration_that_fails_several_rules_is_complained_about_once_per_rule() {
    // Reporting the first would make fixing a declaration a sequence of restarts, which is
    // how somebody arrives at deleting the rules instead.
    let dir = tempfile::tempdir().expect("a directory");
    declare(
        dir.path(),
        "wrong.yaml",
        r#"
name: wrong
from: /spool
schema: sales
table: orders
columns:
  - name: id
    type: bigint
quarantine:
  retain_days: 0
  window: 100
  stop_above: 0.2
"#,
    );

    let (declared, complaints) = load(dir.path());

    assert!(declared.is_empty());
    // No date axis, an unwritable type, and a quarantine that never expires.
    assert!(complaints.len() >= 3, "one per rule: {complaints:?}");
    assert!(complaints.iter().any(|c| c.contains("sank_data_date")), "{complaints:?}");
    assert!(complaints.iter().any(|c| c.contains("bigint")), "{complaints:?}");
    assert!(complaints.iter().any(|c| c.contains("retain_days")), "{complaints:?}");
}

/// The loader, with each feed reduced to what these tests assert about it.
fn load(configuration: &Path) -> (Vec<(String, std::path::PathBuf)>, Vec<String>) {
    let (declared, complaints) = feeds::load(configuration);
    (
        declared
            .into_iter()
            .map(|feed| (feed.feed.name().to_owned(), feed.from))
            .collect(),
        complaints,
    )
}

#[test]
fn a_readme_beside_the_feeds_is_not_a_broken_feed() {
    // Found by starting the shipped server and reading its output. `config/feeds/README.md`
    // explains what a feed declaration is; the loader read every file in the directory, so
    // it parsed the explanation as a declaration and complained about it on **every**
    // startup:
    //
    //   feed not loaded --- config/feeds/README.md: invalid type: string "One file per
    //   feed. Each declares a source of newline-delimited JSON documents..."
    //
    // A complaint that is always present is one nobody reads, and the next complaint --- a
    // feed that really is malformed --- arrives in the same place and looks the same. That
    // is the damage: not the noise, but the real message it hides.
    let dir = tempfile::tempdir().expect("a directory");
    declare(dir.path(), "orders.yaml", SOUND);
    std::fs::write(
        dir.path().join("README.md"),
        "One file per feed. Each declares a source of newline-delimited JSON documents.\n",
    )
    .expect("writing the readme");
    std::fs::write(dir.path().join("notes.txt"), "scratch\n").expect("writing a note");

    let (declared, complaints) = load(dir.path());

    assert!(
        complaints.is_empty(),
        "a file that is not a declaration was read as a broken one: {complaints:?}"
    );
    assert_eq!(declared.len(), 1, "the real feed did not load");
    assert_eq!(declared[0].0, "orders");
}
