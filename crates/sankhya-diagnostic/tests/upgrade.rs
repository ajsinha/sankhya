//! A history written by another release.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_diagnostic::history::{History, HistoryError, Measure, HISTORY_FILE};

fn wrote(dir: &std::path::Path, contents: &str) {
    std::fs::write(dir.join(HISTORY_FILE), contents).expect("write");
}

fn files_of(table: &str) -> Measure {
    Measure::new("compaction-debt", format!("table {table}"))
}

#[test]
fn a_history_written_before_the_header_existed_is_format_one() {
    // The absence of a header means the original format, not an unknown one. Treating it as
    // unknown would refuse every file written by the release that predates this change,
    // which is every file in existence at the moment the change lands.
    let dir = tempfile::tempdir().expect("a temporary directory");
    wrote(
        dir.path(),
        "0\tcompaction-debt\ttable sales.orders\t400.0\n\
         86400000000\tcompaction-debt\ttable sales.orders\t900.0\n",
    );

    let history = History::read(dir.path()).expect("an unheadered history reads");
    assert_eq!(history.format(), 1);
    assert_eq!(history.len(), 2);
    assert_eq!(history.damaged_lines(), 0);
    assert_eq!(
        history.trend(&files_of("sales.orders")).latest().map(|o| o.value),
        Some(900.0)
    );
}

#[test]
fn a_history_from_a_newer_release_is_refused_before_a_single_line_is_read() {
    // Otherwise the future format is discovered as a column that will not parse, counted as
    // damage, and reported as a corrupt file — sending an operator to look for a bad disk
    // when the answer is to upgrade the binary.
    let dir = tempfile::tempdir().expect("a temporary directory");
    wrote(
        dir.path(),
        "# sankhya diagnostic history, format 7\n\
         0\tcompaction-debt\ttable sales.orders\t{\"value\":400,\"unit\":\"files\"}\n",
    );

    let refused = History::read(dir.path()).expect_err("format 7 is refused");
    let HistoryError::FromTheFuture { why, .. } = &refused else {
        panic!("a newer format must not be reported as damage: {refused:?}");
    };
    assert!(why.contains("format 7"), "{why}");
    assert!(why.contains("Upgrade"), "{why}");
    assert!(
        !refused.to_string().contains("could not be read ("),
        "and not as an I/O problem: {refused}"
    );
}

#[test]
fn a_new_file_gets_a_header_and_an_existing_one_does_not_get_a_second() {
    // Appending a header to an existing file would put it in the middle, where it is
    // neither a header nor an observation.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut history = History::new();
    for at in [0_i64, 1_000, 2_000] {
        history
            .append(
                dir.path(),
                files_of("a"),
                sankhya_diagnostic::projection::Observation::new(at, 1.0),
            )
            .expect("appended");
    }

    let text = std::fs::read_to_string(dir.path().join(HISTORY_FILE)).expect("read");
    let headers = text.lines().filter(|line| line.starts_with('#')).count();
    assert_eq!(headers, 1, "one header, at the top");
    assert!(text.lines().next().expect("a line").starts_with('#'));
    assert_eq!(History::read(dir.path()).expect("reads").len(), 3);
}

#[test]
fn compaction_keeps_the_header() {
    // Compaction rewrites the file. A rewrite that dropped the header would silently demote
    // the file to "unheadered", which reads as format 1 — right today and wrong the moment
    // the format moves.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut history = History::new();
    for at in 0..600_i64 {
        history
            .append(
                dir.path(),
                files_of("a"),
                sankhya_diagnostic::projection::Observation::new(at * 1_000, 1.0),
            )
            .expect("appended");
    }
    assert!(history.should_compact());
    history.compact(dir.path()).expect("compacted");

    let text = std::fs::read_to_string(dir.path().join(HISTORY_FILE)).expect("read");
    assert!(text.lines().next().expect("a line").starts_with('#'));
    assert_eq!(History::read(dir.path()).expect("reads").format(), 1);
}

#[test]
fn a_comment_that_is_not_a_header_is_skipped_rather_than_counted_as_damage() {
    // An operator annotating the file — which is a thing people do to a plain-text log — must
    // not make the diagnostic report the file as corrupt.
    let dir = tempfile::tempdir().expect("a temporary directory");
    wrote(
        dir.path(),
        "# sankhya diagnostic history, format 1\n\
         # rebuilt by hand after the incident on the 14th\n\
         0\tcompaction-debt\ttable a\t10.0\n",
    );
    let history = History::read(dir.path()).expect("reads");
    assert_eq!(history.len(), 1);
    assert_eq!(history.damaged_lines(), 0, "a comment is not damage");
}
