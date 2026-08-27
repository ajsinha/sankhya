//! The history, and what it does when it is damaged.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_diagnostic::history::{History, HistoryError, Measure, HISTORY_FILE, OBSERVATIONS_KEPT};
use sankhya_diagnostic::projection::{Concern, Observation, Projection, Trend};
use std::fs;

const DAY: i64 = 24 * 3_600 * 1_000_000;

fn files_of(table: &str) -> Measure {
    Measure::new("compaction-debt", format!("table {table}"))
}

#[test]
fn a_first_run_finds_no_history_and_that_is_not_an_error() {
    // The commonest case on any new installation. Treating a missing file as a failure would
    // make the first run of the diagnostic report a problem with the diagnostic.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let history = History::read(dir.path()).expect("a missing history is an empty one");
    assert!(history.is_empty());
    assert_eq!(history.damaged_lines(), 0);
    assert_eq!(history.trend(&files_of("sales.orders")), Trend::default());
}

#[test]
fn what_one_run_records_the_next_run_can_project_from() {
    // The whole reason this module exists. Two separate `History` values, as two separate
    // processes would see them, and the second can do what the first could not.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let measure = files_of("sales.orders");

    let mut monday = History::new();
    monday
        .append(dir.path(), measure.clone(), Observation::new(0, 400.0))
        .expect("appended");
    assert!(
        matches!(
            monday.trend(&measure).time_until(1_000.0, Concern::RisingTo, 0),
            Projection::Unknown { .. }
        ),
        "one observation is not a rate, whatever it is written to"
    );

    let mut tuesday = History::read(dir.path()).expect("read back");
    assert_eq!(tuesday.len(), 1, "Monday's observation survived the process");
    tuesday
        .append(dir.path(), measure.clone(), Observation::new(5 * DAY, 900.0))
        .expect("appended");

    let projection = tuesday
        .trend(&measure)
        .time_until(1_000.0, Concern::RisingTo, 5 * DAY);
    let Projection::Crossing { seconds, .. } = projection else {
        panic!("two runs make a rate, and gave {projection:?}");
    };
    assert_eq!(seconds / 86_400, 1);
}

#[test]
fn a_torn_last_line_costs_one_observation_and_nothing_else() {
    // A process killed mid-append. Refusing to start here would take the diagnostic away at
    // exactly the moment somebody reaches for it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut history = History::new();
    let measure = files_of("sales.orders");
    for day in 0..4 {
        history
            .append(
                dir.path(),
                measure.clone(),
                #[allow(clippy::cast_precision_loss)]
                Observation::new(day * DAY, 100.0 * day as f64),
            )
            .expect("appended");
    }
    let path = dir.path().join(HISTORY_FILE);
    let mut text = fs::read_to_string(&path).expect("read");
    text.push_str("86400000000\tcompaction-de");
    fs::write(&path, text).expect("write");

    let recovered = History::read(dir.path()).expect("a torn line is survivable");
    assert_eq!(recovered.len(), 4, "the four whole lines are all there");
    assert_eq!(recovered.damaged_lines(), 1, "and the damage is counted, not hidden");
}

#[test]
fn a_line_with_a_nonsense_value_is_dropped_rather_than_parsed() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    fs::write(
        dir.path().join(HISTORY_FILE),
        "0\tcompaction-debt\ttable a\t100.0\n\
         1000\tcompaction-debt\ttable a\tnot-a-number\n\
         2000\tcompaction-debt\ttable a\t200.0\n",
    )
    .expect("write");

    let history = History::read(dir.path()).expect("read");
    assert_eq!(history.len(), 2);
    assert_eq!(history.damaged_lines(), 1);
}

#[test]
fn an_infinity_never_becomes_an_observation() {
    // A NaN or an infinity poisons a projection silently rather than loudly: every
    // comparison against a threshold is false, so the measure reads as "not there yet"
    // forever and the check goes quiet without ever saying why.
    let dir = tempfile::tempdir().expect("a temporary directory");
    fs::write(
        dir.path().join(HISTORY_FILE),
        "0\tc\ts\tinf\n1000\tc\ts\tNaN\n2000\tc\ts\t-inf\n3000\tc\ts\t7.5\n",
    )
    .expect("write");

    let history = History::read(dir.path()).expect("read");
    assert_eq!(history.len(), 1);
    assert_eq!(history.damaged_lines(), 3);
}

#[test]
fn two_checks_of_the_same_subject_are_two_series() {
    // Otherwise a file count and a byte count of one table would be interleaved into one
    // sequence, and the slope through them would be arithmetic on unrelated units.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut history = History::new();
    let files = Measure::new("compaction-debt", "table a");
    let bytes = Measure::new("table-bytes", "table a");
    history
        .append(dir.path(), files.clone(), Observation::new(0, 10.0))
        .expect("appended");
    history
        .append(dir.path(), bytes.clone(), Observation::new(0, 1e9))
        .expect("appended");

    let read = History::read(dir.path()).expect("read");
    assert_eq!(read.trend(&files).latest().map(|o| o.value), Some(10.0));
    assert_eq!(read.trend(&bytes).latest().map(|o| o.value), Some(1e9));
    assert_eq!(read.measures().count(), 2);
}

#[test]
fn a_subject_containing_a_tab_cannot_forge_a_field() {
    // The separator is the only structure the format has, so a subject carrying one would
    // shift every later field along and silently rename a measure.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut history = History::new();
    let sneaky = Measure::new("compaction-debt", "table a\t999999\tother-check");
    history
        .append(dir.path(), sneaky, Observation::new(0, 10.0))
        .expect("appended");

    let read = History::read(dir.path()).expect("read");
    assert_eq!(read.damaged_lines(), 0);
    assert_eq!(read.measures().count(), 1);
    let only = read.measures().next().expect("one measure");
    assert_eq!(only.check, "compaction-debt");
    assert!(!only.subject.contains('\t'));
    // The assertions above all held while the tabs were being written through, because the
    // extra fields simply shifted every later one along: the subject read back as
    // "table a", which contains no tab, and the *value* read back as 999999. Both look
    // plausible. So the test has to pin the whole line, not the parts that survive.
    assert_eq!(only.subject, "table a 999999 other-check");
    assert_eq!(
        read.trend(only).latest().map(|o| o.value),
        Some(10.0),
        "the value must be the one recorded, not a field forged by the subject"
    );
}

#[test]
fn values_survive_the_round_trip_bit_for_bit() {
    // A rate computed from rounded samples is not the rate the samples describe, and the
    // difference shows up as a projection that moves when nothing has changed.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut history = History::new();
    let measure = files_of("a");
    let awkward = [0.1 + 0.2, 1e-300, 9.007_199_254_740_993e15, -0.0];
    for (i, value) in awkward.iter().enumerate() {
        history
            .append(
                dir.path(),
                measure.clone(),
                #[allow(clippy::cast_possible_wrap)]
                Observation::new(i as i64, *value),
            )
            .expect("appended");
    }

    let read = History::read(dir.path()).expect("read");
    let back: Vec<f64> = read
        .trend(&measure)
        .observations()
        .iter()
        .map(|o| o.value)
        .collect();
    assert_eq!(back.len(), awkward.len());
    for (wrote, got) in awkward.iter().zip(back.iter()) {
        assert_eq!(wrote.to_bits(), got.to_bits(), "{wrote} came back as {got}");
    }
}

#[test]
fn the_history_is_bounded_so_the_diagnostic_is_not_itself_a_disk_problem() {
    let mut history = History::new();
    let measure = files_of("a");
    for i in 0..(OBSERVATIONS_KEPT + 50) {
        #[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss)]
        history.record(measure.clone(), Observation::new(i as i64 * 1_000, i as f64));
    }
    assert_eq!(history.len(), OBSERVATIONS_KEPT);
    #[allow(clippy::cast_precision_loss)]
    let newest = (OBSERVATIONS_KEPT + 49) as f64;
    assert_eq!(
        history.trend(&measure).latest().map(|o| o.value),
        Some(newest),
        "the newest is kept and the oldest dropped"
    );
}

#[test]
fn compaction_rewrites_the_file_without_losing_what_is_kept() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut history = History::new();
    let measure = files_of("a");
    for i in 0..(OBSERVATIONS_KEPT * 3) {
        #[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss)]
        history
            .append(
                dir.path(),
                measure.clone(),
                Observation::new(i as i64 * 1_000, i as f64),
            )
            .expect("appended");
    }
    let before = fs::metadata(dir.path().join(HISTORY_FILE)).expect("stat").len();
    assert!(history.should_compact(), "the file has outgrown what is kept");

    history.compact(dir.path()).expect("compacted");
    let after = fs::metadata(dir.path().join(HISTORY_FILE)).expect("stat").len();
    assert!(after < before, "{after} is not smaller than {before}");

    let read = History::read(dir.path()).expect("read");
    assert_eq!(read.len(), OBSERVATIONS_KEPT);
    assert_eq!(read.damaged_lines(), 0);
    assert!(!read.should_compact(), "a compacted file does not want compacting again");
    assert!(
        !dir.path().join(format!("{HISTORY_FILE}.new")).exists(),
        "the temporary file is renamed, not left behind"
    );
}

#[test]
fn an_unwritable_history_says_what_the_next_run_loses() {
    // Not "permission denied". An operator needs to know the consequence, which is that
    // tomorrow's run projects across a gap it cannot see.
    let error = HistoryError::Unwritable {
        path: std::path::PathBuf::from("/var/lib/sankhya/diagnostic-history.tsv"),
        why: "permission denied".to_string(),
    };
    let text = error.to_string();
    assert!(text.contains("permission denied"));
    assert!(text.contains("diagnostic-history.tsv"));
    assert!(
        text.contains("next run will project from a gap"),
        "the message must state the consequence: {text}"
    );
}

#[test]
fn a_file_that_is_all_live_observations_is_not_rewritten() {
    // Rewriting on every run would be a lot of I/O to save nothing, and every rewrite is
    // another rename that can be interrupted.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut history = History::new();
    for i in 0..10 {
        history
            .append(dir.path(), files_of("a"), Observation::new(i * 1_000, 1.0))
            .expect("appended");
    }
    assert_eq!(history.lines_on_disk(), 10);
    assert!(!history.should_compact());
}
