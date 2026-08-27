//! A run against real tables on disk.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_diagnostic::collect::{run, TableUnderReview, COMPACTION_DEBT};
use sankhya_diagnostic::history::{History, Measure, HISTORY_FILE};
use sankhya_diagnostic::projection::Projection;
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata, RemoveFile};
use std::path::Path;

const DAY: i64 = 24 * 3_600 * 1_000_000;

/// A table with `files` live files, built through the real log.
fn table_with(root: &Path, files: usize) {
    let metadata = Metadata::new(
        "11111111-1111-1111-1111-111111111111",
        r#"{"type":"struct","fields":[]}"#,
        0,
    );
    let mut actions = create(metadata);
    for i in 0..files {
        actions.push(Action::Add(AddFile::new(
            format!("part-{i:05}.parquet"),
            1_024,
            0,
        )));
    }
    commit(root, 0, &actions).expect("the table is created");
}

fn measure(name: &str) -> Measure {
    Measure::new(COMPACTION_DEBT, format!("table {name}"))
}

#[test]
fn a_first_run_records_values_and_promises_no_dates() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("the data directory");
    let table = dir.path().join("sales").join("orders");
    table_with(&table, 990);

    let report = run(
        &data,
        &[TableUnderReview::new("sales.orders", &table)],
        0,
    );

    assert_eq!(report.findings().len(), 1, "990 of 1,000 is worth saying");
    assert!(
        matches!(report.findings()[0].projection, Projection::Unknown { .. }),
        "and saying it without a date, because one run has no rate"
    );
    assert!(data.join(HISTORY_FILE).exists(), "the observation was recorded");
}

#[test]
fn a_second_run_projects_from_what_the_first_recorded() {
    // The end-to-end shape of FR-OPS-17: two runs, real tables, and a date that comes out
    // of the difference between them rather than out of either one.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("the data directory");
    let table = dir.path().join("sales").join("orders");
    table_with(&table, 400);
    let under_review = [TableUnderReview::new("sales.orders", &table)];

    let monday = run(&data, &under_review, 0);
    assert!(monday.is_clean(), "400 files is nothing to report");

    // Five days pass and five hundred files arrive.
    let mut actions = Vec::new();
    for i in 400..900 {
        actions.push(Action::Add(AddFile::new(
            format!("part-{i:05}.parquet"),
            1_024,
            0,
        )));
    }
    commit(&table, 1, &actions).expect("committed");

    let saturday = run(&data, &under_review, 5 * DAY);
    assert_eq!(saturday.findings().len(), 1);
    let finding = &saturday.findings()[0];
    let Projection::Crossing { seconds, .. } = finding.projection else {
        panic!("two runs make a rate, and gave {:?}", finding.projection);
    };
    assert_eq!(seconds / 86_400, 1, "900 files, 100 a day, 1,000 is the line");
    assert!(finding.observed.contains("900 live files"));
    assert!(finding.remediation.contains("--table sales.orders"));
}

#[test]
fn compaction_between_runs_shows_up_as_receding_and_is_not_reported() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("the data directory");
    let table = dir.path().join("sales").join("orders");
    table_with(&table, 900);
    let under_review = [TableUnderReview::new("sales.orders", &table)];

    let first = run(&data, &under_review, 0);
    assert_eq!(first.findings().len(), 1, "900 is near the line, undated");

    // Compaction: 900 files become one.
    let mut actions: Vec<Action> = (0..900)
        .map(|i| Action::Remove(RemoveFile::rewritten(format!("part-{i:05}.parquet"), 1)))
        .collect();
    actions.push(Action::Add(AddFile::new("part-compacted.parquet", 921_600, 1)));
    commit(&table, 1, &actions).expect("committed");

    let second = run(&data, &under_review, DAY);
    assert!(
        second.is_clean(),
        "a table that was just compacted is not a finding: {:?}",
        second.findings()
    );
    assert!(second.summary().contains("1 check(s) clean"));
}

#[test]
fn a_table_that_cannot_be_read_is_named_rather_than_counted_as_healthy() {
    // The failure this module exists to prevent: a table nobody could look at landing in
    // the same empty finding list as a table that is fine.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("the data directory");
    let good = dir.path().join("sales").join("orders");
    let bad = dir.path().join("sales").join("missing");
    table_with(&good, 10);

    let report = run(
        &data,
        &[
            TableUnderReview::new("sales.orders", &good),
            TableUnderReview::new("sales.missing", &bad),
        ],
        0,
    );

    assert!(report.is_clean(), "the good table is fine");
    let skipped = report.could_not_run();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, COMPACTION_DEBT);
    assert!(
        skipped[0].1.contains("sales.missing"),
        "the report must name the table it could not look at: {}",
        skipped[0].1
    );
    assert!(
        report.summary().contains("1 check(s) could not run"),
        "and the summary must not read as an all-clear: {}",
        report.summary()
    );
}

#[test]
fn one_bad_table_does_not_stop_the_others_being_observed() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("the data directory");
    let bad = dir.path().join("sales").join("missing");
    let good = dir.path().join("sales").join("orders");
    table_with(&good, 990);

    // The unreadable one first, so a run that stops at the first failure never reaches the
    // finding — which is exactly the shape of the bug.
    let report = run(
        &data,
        &[
            TableUnderReview::new("sales.missing", &bad),
            TableUnderReview::new("sales.orders", &good),
        ],
        0,
    );
    assert_eq!(report.findings().len(), 1);
    assert_eq!(report.could_not_run().len(), 1);

    let history = History::read(&data).expect("read");
    assert_eq!(
        history.trend(&measure("sales.orders")).latest().map(|o| o.value),
        Some(990.0)
    );
}

#[test]
fn a_run_records_before_it_reports_so_it_reports_on_what_it_just_saw() {
    // Recording after reporting throws away the newest sample, and the projection is then
    // permanently one run behind. It looks right, because it is right about last time.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("the data directory");
    let table = dir.path().join("sales").join("orders");
    table_with(&table, 990);

    let report = run(&data, &[TableUnderReview::new("sales.orders", &table)], 0);
    assert!(
        report.findings()[0].observed.contains("990"),
        "the report describes this run's observation, not the previous one"
    );
    assert_eq!(History::read(&data).expect("read").len(), 1);
}

#[test]
fn a_damaged_history_is_declared_rather_than_silently_projected_around() {
    // A history quietly losing lines still produces dates, drawn from fewer samples than the
    // file appears to hold. The dates are not obviously wrong, which is the problem.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("the data directory");
    let table = dir.path().join("sales").join("orders");
    table_with(&table, 10);
    std::fs::write(
        data.join(HISTORY_FILE),
        "0\tcompaction-debt\ttable sales.orders\t5.0\nnot a line at all\n",
    )
    .expect("write");

    let report = run(&data, &[TableUnderReview::new("sales.orders", &table)], DAY);
    let complaint = report
        .could_not_run()
        .iter()
        .find(|(check, _)| *check == "diagnostic-history")
        .expect("the damage is declared");
    assert!(complaint.1.contains("1 line(s)"));
    assert!(complaint.1.contains("fewer samples"));
}

#[test]
fn a_warehouse_with_no_tables_reports_nothing_and_claims_nothing() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("the data directory");
    let report = run(&data, &[], 0);
    assert!(report.is_clean());
    assert!(report.could_not_run().is_empty());
    assert!(report.summary().contains("0 check(s) clean"));
}
