//! Measuring what a run consumes.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::float_cmp)]

use sankhya_diagnostic::soak::measure::watched;
use sankhya_diagnostic::soak::sample::tree_bytes;

#[test]
fn a_tree_is_measured_including_what_is_nested_in_it() {
    // Partitioned tables put every file one directory down. A measurement that stopped at
    // the top would report a warehouse of nothing while the disk filled.
    let dir = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir_all(dir.path().join("sank_data_date=2026-08-28")).expect("creating");
    std::fs::write(dir.path().join("top.parquet"), vec![0u8; 100]).expect("writing");
    std::fs::write(
        dir.path().join("sank_data_date=2026-08-28/nested.parquet"),
        vec![0u8; 900],
    )
    .expect("writing");

    let measured = tree_bytes(dir.path()).expect("a size");
    assert!(measured >= 1000.0, "nested files were not counted: {measured}");
}

#[test]
fn a_directory_that_does_not_exist_has_no_size_rather_than_a_size_of_zero() {
    // Before the first fill there is no warehouse. Reporting zero would make "not yet
    // written" indistinguishable from "written and empty", and the judge would take the
    // first sample of a ramp from a moment that never happened.
    let dir = tempfile::tempdir().expect("a temporary directory");
    assert_eq!(tree_bytes(&dir.path().join("nowhere")), None);
    assert_eq!(tree_bytes(dir.path()), Some(0.0), "an empty directory is zero");
}

#[test]
fn the_warehouse_is_a_watched_measure_with_a_stated_budget() {
    // The run before this one exhausted its disk and died writing its own log, with a
    // zero-byte report. Nothing was watching the one resource it ran out of.
    let declared = watched("warehouse_bytes").expect("declared");
    assert_eq!(declared.unit, "bytes");
    assert!(
        declared.means.contains("incident"),
        "the meaning should say what it costs: {}",
        declared.means
    );
}
