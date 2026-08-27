//! What the checks report, and the order an operator reads it in.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_diagnostic::check::{
    compaction_debt, human_bytes, replication_lag, storage_headroom, Finding, Report, Severity,
    FILES_BEFORE_LATENCY_SUFFERS, HEADROOM_WORTH_MENTIONING,
};
use sankhya_diagnostic::projection::{Confidence, Observation, Projection, Trend, Unknown};

const DAY: i64 = 24 * 3_600 * 1_000_000;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn series(values: &[f64]) -> Trend {
    Trend::of(values.iter().enumerate().map(|(d, v)| {
        #[allow(clippy::cast_possible_wrap)]
        Observation::new(d as i64 * DAY, *v)
    }))
}

fn last_day(values: &[f64]) -> i64 {
    #[allow(clippy::cast_possible_wrap)]
    let d = (values.len() as i64 - 1) * DAY;
    d
}

// --- FR-OPS-17: a time, not a value -------------------------------------

#[test]
fn compaction_debt_reports_when_rather_than_how_much() {
    let files = [400.0, 500.0, 600.0, 700.0, 800.0, 900.0];
    let finding = compaction_debt("sales.orders", &series(&files), last_day(&files))
        .expect("900 files rising 100 a day reaches 1,000");

    let Projection::Crossing { seconds, .. } = finding.projection else {
        panic!("the finding must carry a date, and carries {:?}", finding.projection);
    };
    assert_eq!(seconds / 86_400, 1);
    assert_eq!(finding.severity, Severity::Warning);
    assert!(finding.observed.contains("900 live files"));
    assert!(
        finding.describe().contains("; at the current rate, about 1 day.\n"),
        "the line an operator reads must contain the time, singular: {}",
        finding.describe()
    );
}

#[test]
fn a_table_already_over_the_line_is_critical_rather_than_projected() {
    let files = [900.0, 1_000.0, 1_100.0, 1_200.0];
    let finding = compaction_debt("sales.orders", &series(&files), last_day(&files))
        .expect("already past the threshold");
    assert_eq!(finding.projection, Projection::Already);
    assert_eq!(finding.severity, Severity::Critical);
}

#[test]
fn a_table_being_compacted_faster_than_it_grows_is_not_a_finding() {
    // The whole point of a projection: a large number that is shrinking needs no attention,
    // and a diagnostic that reports it teaches an operator to skim.
    let files = [900.0, 700.0, 500.0, 300.0];
    assert_eq!(
        compaction_debt("sales.orders", &series(&files), last_day(&files)),
        None
    );
}

#[test]
fn a_table_far_from_the_line_with_no_rate_yet_is_silent() {
    // First run, 40 files. Nothing to say, and saying it anyway is how the diagnostic gets
    // ignored on the day it matters.
    assert_eq!(compaction_debt("sales.orders", &series(&[40.0]), 0), None);
}

#[test]
fn a_table_near_the_line_with_no_rate_yet_still_speaks_up() {
    // 990 of 1,000 files on a first run. Silence here reads as health, and it is not —
    // but neither is a date, because one sample has no rate. What comes out is the whole
    // available truth: near the line, direction unknown.
    let finding = compaction_debt("sales.orders", &series(&[990.0]), 0)
        .expect("990 of 1,000 files is worth saying even undated");
    assert_eq!(finding.severity, Severity::Note, "one sample is not a trend");
    assert!(matches!(
        finding.projection,
        Projection::Unknown {
            reason: Unknown::TooFewObservations { have: 1 }
        }
    ));
    assert!(
        finding.projection.describe().contains("a time needs a rate"),
        "it must say why there is no date"
    );
}

#[test]
fn near_the_line_stays_a_note_and_never_outranks_a_dated_finding() {
    let mut report = Report::new();
    let near = compaction_debt("a", &series(&[990.0]), 0).expect("near");
    let files = [400.0, 500.0, 600.0, 700.0, 800.0, 900.0];
    let dated = compaction_debt("b", &series(&files), last_day(&files)).expect("dated");
    report.found(near);
    report.found(dated);

    assert_eq!(report.findings()[0].subject, "table b", "dated comes first");
    assert_eq!(report.findings()[1].subject, "table a");
}

// --- headroom and lag ---------------------------------------------------

#[test]
fn storage_headroom_projects_the_day_the_disk_fills() {
    let free = [500.0 * GIB, 400.0 * GIB, 300.0 * GIB, 200.0 * GIB, 100.0 * GIB];
    let finding =
        storage_headroom(&series(&free), last_day(&free)).expect("100 GiB falling 100 a day");
    let Projection::Crossing { seconds, .. } = finding.projection else {
        panic!("a falling headroom reaches zero");
    };
    assert_eq!(seconds / 86_400, 1);
    assert!(finding.observed.contains("100.0 GiB free"));
}

#[test]
fn headroom_that_is_growing_is_not_a_finding() {
    let free = [100.0 * GIB, 200.0 * GIB, 300.0 * GIB, 400.0 * GIB];
    assert_eq!(storage_headroom(&series(&free), last_day(&free)), None);
}

#[test]
fn a_nearly_full_disk_speaks_up_on_the_first_run() {
    let finding = storage_headroom(&series(&[HEADROOM_WORTH_MENTIONING - 1.0]), 0)
        .expect("under the watch line");
    assert_eq!(finding.severity, Severity::Note);
    assert!(finding.remediation.contains("retention"));
}

#[test]
fn replication_lag_is_measured_against_the_objective_not_an_absolute() {
    // Ninety seconds behind is fine for a nightly objective and an incident for a
    // two-minute one. The check takes the objective rather than assuming one.
    let lag = [10.0, 30.0, 50.0, 70.0, 90.0];
    let now = last_day(&lag);
    assert_eq!(
        replication_lag(&series(&lag), 3_600.0, now),
        None,
        "90s behind a one-hour objective, and receding is not the reason — it is far off"
    );

    let finding = replication_lag(&series(&lag), 120.0, now).expect("90s against a 120s objective");
    assert!(matches!(finding.projection, Projection::Crossing { .. }));
    assert!(finding.observed.contains("objective 120s"));
}

// --- FR-OPS-16: every finding says what to do ---------------------------

#[test]
fn every_finding_carries_a_remediation_that_names_a_next_step() {
    let files = [400.0, 500.0, 600.0, 700.0, 800.0, 900.0];
    let free = [500.0 * GIB, 400.0 * GIB, 300.0 * GIB, 200.0 * GIB, 100.0 * GIB];
    let lag = [10.0, 30.0, 50.0, 70.0, 90.0];
    let findings = [
        compaction_debt("sales.orders", &series(&files), last_day(&files)),
        storage_headroom(&series(&free), last_day(&free)),
        replication_lag(&series(&lag), 120.0, last_day(&lag)),
        compaction_debt("sales.orders", &series(&[990.0]), 0),
    ];

    for finding in findings.iter().flatten() {
        assert!(
            finding.remediation.len() > 40,
            "{} gives no remediation worth the name: {:?}",
            finding.check,
            finding.remediation
        );
        assert!(
            finding.describe().contains(&finding.remediation),
            "the remediation must appear in the line an operator reads"
        );
    }
    assert_eq!(findings.iter().flatten().count(), 4);
}

#[test]
fn the_compaction_remediation_names_the_table_and_the_durable_fix() {
    // A command an operator can paste, and the reason not to keep pasting it.
    let files = [400.0, 500.0, 600.0, 700.0, 800.0, 900.0];
    let finding = compaction_debt("sales.orders", &series(&files), last_day(&files))
        .expect("a finding");
    assert!(finding.remediation.contains("--table sales.orders"));
    assert!(finding.remediation.contains("duty cycle"));
}

// --- how the report is ordered and what it admits -----------------------

fn finding_with(subject: &str, severity: Severity, projection: Projection) -> Finding {
    Finding {
        check: "test",
        subject: subject.to_string(),
        severity,
        observed: "measured".to_string(),
        projection,
        remediation: "do the thing".to_string(),
    }
}

#[test]
fn findings_sort_by_when_not_by_how_bad() {
    // The property the module comment claims. A note that becomes an outage tomorrow is
    // read before a critical that has been stable for a month, because the operator is
    // reading a schedule.
    let mut report = Report::new();
    report.found(finding_with(
        "unknown-critical",
        Severity::Critical,
        Projection::Unknown {
            reason: Unknown::TooFewObservations { have: 1 },
        },
    ));
    report.found(finding_with(
        "far-critical",
        Severity::Critical,
        Projection::Crossing {
            seconds: 90 * 86_400,
            confidence: Confidence::Firm,
            fit: 1.0,
        },
    ));
    report.found(finding_with(
        "soon-note",
        Severity::Note,
        Projection::Crossing {
            seconds: 86_400,
            confidence: Confidence::Weak,
            fit: 1.0,
        },
    ));
    report.found(finding_with("already-note", Severity::Note, Projection::Already));
    report.found(finding_with("receding", Severity::Critical, Projection::Receding));

    let order: Vec<&str> = report
        .findings()
        .iter()
        .map(|f| f.subject.as_str())
        .collect();
    assert_eq!(
        order,
        [
            "already-note",
            "soon-note",
            "far-critical",
            "receding",
            "unknown-critical"
        ]
    );
}

#[test]
fn a_check_that_could_not_run_is_not_a_check_that_found_nothing() {
    // Both produce an empty finding list, and they are opposite facts. A diagnostic that
    // conflates them reports "all clear" for a storage layer it could not reach.
    let mut clean = Report::new();
    clean.clean("storage-headroom");

    let mut blind = Report::new();
    blind.skipped("storage-headroom", "the data directory is not readable");

    assert!(clean.is_clean() && blind.is_clean(), "neither found anything");
    assert!(clean.could_not_run().is_empty());
    assert_eq!(blind.could_not_run().len(), 1);
    assert!(blind.summary().contains("1 check(s) could not run"));
    assert!(clean.summary().contains("1 check(s) clean"));
}

#[test]
fn the_summary_counts_how_many_findings_actually_have_a_date() {
    // The number an operator needs to know before trusting the list: how much of this is a
    // schedule and how much is "come back tomorrow".
    let mut report = Report::new();
    report.found(finding_with("dated", Severity::Warning, Projection::Already));
    report.found(finding_with(
        "undated",
        Severity::Warning,
        Projection::Unknown {
            reason: Unknown::NotLinear { fit: 0.1 },
        },
    ));
    assert!(report.summary().contains("2 finding(s) of which 1 have a date"));
}

#[test]
fn has_critical_is_about_now_and_not_about_soon() {
    let mut report = Report::new();
    report.found(finding_with(
        "soon",
        Severity::Warning,
        Projection::Crossing {
            seconds: 60,
            confidence: Confidence::Firm,
            fit: 1.0,
        },
    ));
    assert!(!report.has_critical());
    report.found(finding_with("now", Severity::Critical, Projection::Already));
    assert!(report.has_critical());
}

#[test]
fn byte_counts_are_shown_in_the_unit_a_person_reads() {
    assert_eq!(human_bytes(0.0), "0.0 B");
    assert_eq!(human_bytes(1_536.0), "1.5 KiB");
    assert_eq!(human_bytes(3.0 * GIB), "3.0 GiB");
    assert_eq!(human_bytes(2_048.0 * GIB), "2.0 TiB");
    // Beyond the table, rather than a panic or an empty unit.
    assert!(human_bytes(1e30).ends_with("TiB"));
}

#[test]
fn the_latency_threshold_is_a_named_constant_an_operator_can_find() {
    // Not an assertion about the value so much as about it having a name. The number is a
    // judgement about a query mix, and a judgement buried in a comparison cannot be argued
    // with.
    assert_eq!(FILES_BEFORE_LATENCY_SUFFERS, 1_000.0);
}
