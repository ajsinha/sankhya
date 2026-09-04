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

/// The same as [`under`], but the cube is **maintained** — so the answer comes from a
/// materialised cuboid rather than from the facts.
fn maintained(server: &Running, name: &str, rule: &str) -> Vec<(String, f64)> {
    let statement = format!(
        "CREATE CUBE {name} FROM sales.orders \
         DIMENSION region FROM sales.regions ON region (LEVEL area = region) \
         MEASURE amount ({rule} ALONG region) MAINTAINED WITHIN 5 VERSIONS"
    );
    let _ = text_rows(server.port, &statement);
    keyed(
        server.port,
        &format!("SELECT region, amount FROM cube_rollup('{name}', 'amount', 'by=region')"),
    )
}

#[test]
fn a_maintained_measure_answers_its_rule_and_not_the_sum() {
    // `COR-05`, and the reason this file did not catch it: every test above declares its cube
    // **without** `MAINTAINED`, so all of them read the live path. One layer down,
    // materialisation stored `exact_sum()` whatever the rule said and read it back with
    // `add_reduced`, which answers with the stored value for every rule.
    //
    // So the 2026-09-01 defect this file exists to pin was alive again on the fast path, and
    // every test here passed. The rule reached the number; the number was computed before the
    // rule was consulted.
    let (_dir, server) = running();

    for rule in ["MAX", "MIN", "MEAN"] {
        let name = format!("maintained_{}", rule.to_lowercase());
        let cached = maintained(&server, &name, rule);
        let live = under(&server, &format!("live_{}", rule.to_lowercase()), rule);

        assert!(!live.is_empty(), "the fixture has regions");
        assert_eq!(
            cached, live,
            "a maintained cube declaring {rule} answered differently from the same cube \
             computed from the facts. Materialisation may change *where* an answer comes \
             from, never *what* it is"
        );
    }
}

#[test]
fn two_maintained_measures_of_one_cube_keep_their_own_numbers() {
    // `COR-04`. The materialised cuboid's key carried the cube, the definition, the snapshot
    // and the scope — and not the measure. So the first measure to be materialised wrote each
    // shape, every later one found `exists()` true and skipped, and reads built the same
    // measure-free key and labelled whatever came back with the measure they had asked for.
    //
    // On the shipped fixture a maintained `sales` cube returned `amount`'s numbers under the
    // name `ratio`. The in-memory catalog had this exact defect and was fixed by keying on
    // `(cube, measure)`; the on-disk key never got the same treatment.
    let (_dir, server) = running();
    let _ = text_rows(
        server.port,
        "CREATE CUBE twofold FROM sales.orders \
         DIMENSION region FROM sales.regions ON region (LEVEL area = region) \
         MEASURE amount (SUM ALONG region) \
         MEASURE margin_pct (MAX ALONG region) \
         MAINTAINED WITHIN 5 VERSIONS",
    );

    let summed = keyed(
        server.port,
        "SELECT region, amount FROM cube_rollup('twofold', 'amount', 'by=region')",
    );
    let other = keyed(
        server.port,
        "SELECT region, margin_pct FROM cube_rollup('twofold', 'margin_pct', 'by=region')",
    );

    if summed.is_empty() || other.is_empty() {
        // The measure syntax may not permit a second measure over the same column on this
        // build. Say so rather than passing quietly: a test that asserts nothing because its
        // fixture did not build is the shape this repository keeps finding.
        panic!("the two-measure fixture did not build: {summed:?} against {other:?}");
    }
    assert_ne!(
        summed, other,
        "two measures of one maintained cube returned the same numbers, so one cuboid is \
         answering for both"
    );

    // And each agrees with the same cube computed from the facts, which is the assertion that
    // says *which* of the two was wrong rather than only that they differ.
    let _ = text_rows(
        server.port,
        "CREATE CUBE twofold_live FROM sales.orders \
         DIMENSION region FROM sales.regions ON region (LEVEL area = region) \
         MEASURE amount (SUM ALONG region) \
         MEASURE margin_pct (MAX ALONG region)",
    );
    assert_eq!(
        other,
        keyed(
            server.port,
            "SELECT region, margin_pct FROM \
             cube_rollup('twofold_live', 'margin_pct', 'by=region')",
        ),
        "the maintained cube's second measure disagrees with the same measure from the facts"
    );
}
