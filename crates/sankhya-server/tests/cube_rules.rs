//! A declared rule reaches the value, over the wire.
//!
//! # Why this file exists
//!
//! An adversarial review on 2026-09-01 found that a measure declared `MAX ALONG region` returned
//! the **sum** --- `15,687` where the maximum was `373.5`. The composability half of the model
//! was enforced correctly, and the cell was then read with a hardcoded summation one layer
//! below, so the declared rule never reached the number. *That is the exact failure the cube
//! model exists to prevent, arriving beneath where the model checks for it.*
//!
//! It was fixed, and nothing pinned the fix. The book went on documenting it as a live defect a
//! reader must know about before declaring a non-`SUM` measure --- for two days, correctly by
//! its own lights, because no test said otherwise. A defect that is fixed and unpinned is a
//! defect that comes back, and in the meantime the documentation is wrong in the more damaging
//! direction: it tells people a working feature does not work.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{start, text_rows, write_warehouse, Running};

fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

/// One column of one query, as numbers keyed by the first column.
fn keyed(port: u16, sql: &str) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = text_rows(port, sql)
        .iter()
        .filter_map(|row| {
            let key = row.first().cloned().flatten()?;
            let value: f64 = row.get(1).cloned().flatten()?.parse().ok()?;
            Some((key, value))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Declare a cube of one measure under one rule, and read it back.
fn under(server: &Running, name: &str, rule: &str) -> Vec<(String, f64)> {
    let statement = format!(
        "CREATE CUBE {name} FROM sales.orders \
         DIMENSION region FROM sales.regions ON region (LEVEL area = region) \
         MEASURE amount ({rule} ALONG region)"
    );
    let _ = text_rows(server.port, &statement);
    keyed(
        server.port,
        &format!("SELECT region, amount FROM cube_rollup('{name}', 'amount', 'by=region')"),
    )
}

#[test]
fn a_measure_declared_max_returns_the_maximum_and_not_the_sum() {
    let (_dir, server) = running();
    let cube = under(&server, "biggest", "MAX");
    let direct = keyed(
        server.port,
        "SELECT region, max(amount) FROM sales.orders WHERE region IS NOT NULL GROUP BY region",
    );

    assert!(!direct.is_empty(), "the fixture has regions");
    assert_eq!(
        cube, direct,
        "a cube declaring MAX must answer the maximum. It answered the sum until 2026-09-01, \
         and the sum is a number of the right magnitude and the right sign with no meaning"
    );
}

#[test]
fn a_measure_declared_mean_returns_the_mean_and_not_the_sum() {
    let (_dir, server) = running();
    let cube = under(&server, "average", "MEAN");
    let direct = keyed(
        server.port,
        "SELECT region, avg(amount) FROM sales.orders WHERE region IS NOT NULL GROUP BY region",
    );

    assert!(!direct.is_empty(), "the fixture has regions");
    for (mine, theirs) in cube.iter().zip(direct.iter()) {
        assert_eq!(mine.0, theirs.0, "same regions, in the same order");
        assert!(
            (mine.1 - theirs.1).abs() < 1e-9,
            "a cube declaring MEAN must answer the mean: {} against {}",
            mine.1,
            theirs.1
        );
    }
    assert_eq!(cube.len(), direct.len());
}

#[test]
fn a_measure_declared_min_returns_the_minimum() {
    // The third of the family, because two could agree by accident on a fixture where the
    // numbers happen to be close and a third makes that much less likely.
    let (_dir, server) = running();
    let cube = under(&server, "smallest", "MIN");
    let direct = keyed(
        server.port,
        "SELECT region, min(amount) FROM sales.orders WHERE region IS NOT NULL GROUP BY region",
    );
    assert_eq!(cube, direct, "a cube declaring MIN must answer the minimum");
}
