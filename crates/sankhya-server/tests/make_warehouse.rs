//! Not a test: a way to write a real warehouse to a known path, so the server can be
//! started against it by hand and driven with a real client.
//!
//! Ignored by default, because it writes to a path given in the environment, and a test
//! that writes outside its own temporary directory is one that surprises somebody.
//!
//! # Why this calls the fixture instead of building a warehouse of its own
//!
//! It used to build one. That is how the documentation came apart from the tests.
//!
//! This file wrote `sales.orders` with `(id, region, amount)`. The fixture every gate runs
//! against --- `common::write_warehouse` --- writes the same table with `period` and
//! `margin_pct` as well, plus a dimension table, a risk table and the `sales` cube. So
//! `guide.rs`, `sdk_examples.rs` and `book_sql.rs` all passed against a warehouse **no
//! reader could produce**, while the reader following `docs/QUICKSTART.md` got a warehouse
//! with no cube in it: three of twenty-one tutorial SQL blocks ran, and the tutorials' own
//! promise that "every SQL example is executed by a test" made the failure read as the
//! reader's mistake.
//!
//! Two sources for one fact are two sources that will one day disagree. There is now one.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![allow(clippy::print_stdout)]

mod common;

#[test]
#[ignore = "writes a warehouse to SANKHYA_WAREHOUSE; run deliberately"]
fn write_a_warehouse() {
    let root = std::path::PathBuf::from(
        std::env::var("SANKHYA_WAREHOUSE").expect("set SANKHYA_WAREHOUSE"),
    );
    std::fs::create_dir_all(&root).expect("creating the warehouse root");
    common::write_warehouse(&root);
    println!(
        "wrote the sample warehouse to {} --- sales.orders, sales.regions, sank.risk, the \
         quarantine, and the `sales` cube",
        root.display()
    );
}
