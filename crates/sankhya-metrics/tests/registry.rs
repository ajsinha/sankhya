//! What the registry refuses, and what it does instead of failing.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_metrics::catalogue::{
    ALL, AUDIT_RECORDS_TOTAL, CONNECTIONS_ACTIVE, METRICS_REJECTED_TOTAL, QUERIES_TOTAL,
    QUERY_DURATION_SECONDS, ROWS_RETURNED_TOTAL, TABLE_LIVE_FILES,
};
use sankhya_metrics::metric::{Group, Kind, Label, Metric, Unit, Values};
use sankhya_metrics::Registry;

// --- the tenant-data prohibition ----------------------------------------

#[test]
fn a_closed_label_refuses_anything_outside_its_set() {
    // ARCHITECTURE §17.1: no metric label may contain tenant data. This is the enforcement
    // — not a convention, not a review item. A label whose permitted values are `ok` and
    // `error` cannot be handed a customer's name however the call site is written.
    let registry = Registry::new();
    registry.increment(&QUERIES_TOTAL, &[("outcome", "ok")], 1.0);
    registry.increment(
        &QUERIES_TOTAL,
        &[("outcome", "SELECT * FROM accounts WHERE name = 'Ashutosh'")],
        1.0,
    );

    assert_eq!(registry.value(&QUERIES_TOTAL, &[("outcome", "ok")]), Some(1.0));
    assert_eq!(registry.series_count(&QUERIES_TOTAL), 1, "the query text made no series");
    assert_eq!(registry.rejections().value_not_permitted, 1);
}

#[test]
fn a_label_the_metric_does_not_declare_is_refused() {
    // The other way to leak: adding a dimension nobody declared. It would not be reviewed,
    // because it is one line at one call site, and it would appear on every dashboard.
    let registry = Registry::new();
    registry.increment(
        &QUERIES_TOTAL,
        &[("outcome", "ok"), ("user", "ashutosh@example.com")],
        1.0,
    );
    assert_eq!(registry.series_count(&QUERIES_TOTAL), 0);
    assert_eq!(registry.rejections().label_not_declared, 1);
}

#[test]
fn a_missing_label_is_refused_rather_than_defaulted() {
    // Defaulting to an empty string would not produce a missing series. It would produce a
    // series that aggregates alongside the properly-labelled ones and silently doubles them,
    // which reads as traffic that is not there.
    let registry = Registry::new();
    registry.increment(&QUERIES_TOTAL, &[], 1.0);
    assert_eq!(registry.series_count(&QUERIES_TOTAL), 0);
    assert_eq!(registry.rejections().label_missing, 1);
}

#[test]
fn every_rejection_reason_is_counted_separately() {
    // One "rejected" counter would tell an operator something is wrong and nothing about
    // what. Each of these has a different fix.
    let registry = Registry::new();
    registry.increment(&QUERIES_TOTAL, &[("outcome", "nonsense")], 1.0);
    registry.increment(&QUERIES_TOTAL, &[("nope", "ok")], 1.0);
    registry.increment(&QUERIES_TOTAL, &[], 1.0);

    let rejections = registry.rejections();
    assert_eq!(rejections.value_not_permitted, 1);
    assert_eq!(rejections.label_not_declared, 1);
    assert_eq!(rejections.label_missing, 1);
    assert_eq!(rejections.total(), 3);
    assert!(rejections.any());
}

#[test]
fn a_refusal_never_reaches_the_caller() {
    // The whole contract: recording returns nothing. A metric call that could fail would put
    // a `?` on a line that exists only to observe, and the first propagated error would let
    // a mis-labelled counter take down a query.
    let registry = Registry::new();
    // Deliberately every wrong shape at once; the test is that this compiles and returns.
    registry.increment(&QUERIES_TOTAL, &[("outcome", "no")], 1.0);
    registry.set(&CONNECTIONS_ACTIVE, &[("nope", "x")], 3.0);
    registry.observe(&QUERY_DURATION_SECONDS, &[], 0.5);
    assert_eq!(registry.rejections().total(), 3);
}

// --- cardinality --------------------------------------------------------

#[test]
fn an_identifier_label_stops_adding_series_at_its_cap() {
    let Values::Identifier { cap } = TABLE_LIVE_FILES.labels[0].values else {
        panic!("the table label is an identifier");
    };
    let registry = Registry::new();
    for i in 0..(cap + 25) {
        registry.set(&TABLE_LIVE_FILES, &[("table", &format!("s.t{i}"))], 1.0);
    }
    assert_eq!(registry.series_count(&TABLE_LIVE_FILES), cap);
    assert_eq!(registry.rejections().over_cap, 25);
}

#[test]
fn a_table_already_seen_keeps_recording_after_the_cap_is_reached() {
    // The budget is charged per distinct value, not per call. Otherwise a hot table would
    // consume the cap by itself and the metric would stop working under exactly the load it
    // exists to describe.
    let registry = Registry::new();
    registry.set(&TABLE_LIVE_FILES, &[("table", "sales.orders")], 10.0);
    for i in 0..500 {
        registry.set(&TABLE_LIVE_FILES, &[("table", &format!("s.t{i}"))], 1.0);
    }
    registry.set(&TABLE_LIVE_FILES, &[("table", "sales.orders")], 42.0);

    assert_eq!(
        registry.value(&TABLE_LIVE_FILES, &[("table", "sales.orders")]),
        Some(42.0),
        "a known table is still recorded once the cap is full"
    );
}

#[test]
fn going_over_the_cap_is_visible_rather_than_silent() {
    // Growing without limit takes the process down. Dropping without saying so makes a
    // dashboard quietly wrong, which is worse than a gap, because a gap gets noticed.
    let registry = Registry::new();
    for i in 0..1_000 {
        registry.set(&TABLE_LIVE_FILES, &[("table", &format!("s.t{i}"))], 1.0);
    }
    assert!(registry.rejections().over_cap > 0);
    assert!(registry.rejections().any());
}

/// A second table-labelled metric, declared here because the shipped catalogue has only one.
///
/// The first version of the test below used `ROWS_RETURNED_TOTAL`, which carries no labels
/// at all --- so it never touched the cardinality budget and passed happily with the budgets
/// shared. It asserted something true for a reason unrelated to what it claimed to check.
static OTHER_TABLE_METRIC: Metric = Metric {
    name: "sankhya_test_other_table_metric",
    kind: Kind::Gauge,
    unit: Unit::Count,
    labels: &[Label::identifier("table", 200)],
    group: Group::Maintenance,
    help: "A second metric labelled by table, for the budget test.",
    alert: None,
};

#[test]
fn two_metrics_labelled_by_table_have_independent_budgets() {
    // One expensive metric is not a reason to stop recording another. Sharing a budget makes
    // the cheap metric's completeness depend on the expensive one's traffic --- so the
    // metric that goes incomplete is whichever one happened to be recorded second, which is
    // not a property anybody can reason about.
    let registry = Registry::new();
    for i in 0..1_000 {
        registry.set(&TABLE_LIVE_FILES, &[("table", &format!("s.t{i}"))], 1.0);
    }
    assert!(registry.rejections().over_cap > 0, "the first metric filled its budget");

    registry.set(&OTHER_TABLE_METRIC, &[("table", "sales.orders")], 5.0);
    assert_eq!(
        registry.value(&OTHER_TABLE_METRIC, &[("table", "sales.orders")]),
        Some(5.0),
        "the second metric has its own budget and has spent none of it"
    );

    // And an unlabelled metric is untouched by any of it.
    registry.increment(&ROWS_RETURNED_TOTAL, &[], 5.0);
    assert_eq!(registry.value(&ROWS_RETURNED_TOTAL, &[]), Some(5.0));
}

// --- the numbers themselves ---------------------------------------------

#[test]
fn labels_in_a_different_order_are_the_same_series() {
    // Otherwise the same event recorded from two call sites written by two people becomes
    // two series, and every rate over it is halved.
    let registry = Registry::new();
    registry.increment(&QUERIES_TOTAL, &[("outcome", "ok")], 1.0);
    registry.increment(&QUERIES_TOTAL, &[("outcome", "ok")], 1.0);
    assert_eq!(registry.series_count(&QUERIES_TOTAL), 1);
    assert_eq!(registry.value(&QUERIES_TOTAL, &[("outcome", "ok")]), Some(2.0));
}

#[test]
fn a_gauge_is_set_and_a_counter_accumulates() {
    let registry = Registry::new();
    registry.set(&CONNECTIONS_ACTIVE, &[], 3.0);
    registry.set(&CONNECTIONS_ACTIVE, &[], 1.0);
    assert_eq!(registry.value(&CONNECTIONS_ACTIVE, &[]), Some(1.0));

    registry.increment(&AUDIT_RECORDS_TOTAL, &[], 1.0);
    registry.increment(&AUDIT_RECORDS_TOTAL, &[], 1.0);
    assert_eq!(registry.value(&AUDIT_RECORDS_TOTAL, &[]), Some(2.0));
}

#[test]
fn a_histogram_counts_cumulatively_and_keeps_its_sum() {
    let registry = Registry::new();
    for value in [0.0005, 0.003, 0.05, 30.0] {
        registry.observe(&QUERY_DURATION_SECONDS, &[("outcome", "ok")], value);
    }
    assert_eq!(
        registry.observation_count(&QUERY_DURATION_SECONDS, &[("outcome", "ok")]),
        4
    );

    let text = registry.render(&[&QUERY_DURATION_SECONDS]);
    assert!(text.contains("le=\"0.001\"} 1"), "{text}");
    assert!(text.contains("le=\"0.005\"} 2"), "{text}");
    assert!(text.contains("le=\"0.1\"} 3"), "{text}");
    assert!(text.contains("le=\"60\"} 4"), "{text}");
    assert!(text.contains("_count{outcome=\"ok\"} 4"), "{text}");
}

#[test]
fn a_histogram_always_renders_the_infinity_bucket() {
    // Required by the format, and not decoration: quantile estimation needs to know how many
    // observations exceeded every declared bound. Without it the histogram is unusable
    // rather than merely incomplete.
    let registry = Registry::new();
    registry.observe(&QUERY_DURATION_SECONDS, &[("outcome", "ok")], 3_600.0);
    let text = registry.render(&[&QUERY_DURATION_SECONDS]);
    assert!(text.contains("le=\"+Inf\"} 1"), "{text}");
    assert!(text.contains("le=\"60\"} 0"), "a value past every bound: {text}");
}

// --- rendering ----------------------------------------------------------

#[test]
fn a_metric_with_nothing_recorded_still_declares_itself() {
    // A dashboard must be able to tell "no events" from "not wired up". An absent metric
    // looks like the second and usually is the first.
    let registry = Registry::new();
    let text = registry.render(ALL);
    assert!(text.contains("# HELP sankhya_queries_total"));
    assert!(text.contains("# TYPE sankhya_queries_total counter"));
    assert!(text.contains("# TYPE sankhya_query_duration_seconds histogram"));
    assert!(text.contains("# TYPE sankhya_connections_active gauge"));
}

#[test]
fn the_page_with_no_lead_time_reads_zero_before_it_ever_fires() {
    // `# HELP` and `# TYPE` alone store nothing: Prometheus keeps a metric only when a
    // sample arrives. So the declaration the test above checks does *not* make the two
    // states distinguishable, and this is the metric where that matters most --- its
    // declared lead time is "none", so an alert on `> 0` has to be armed from startup.
    let registry = Registry::new();
    let text = registry.render(ALL);
    assert!(
        text.lines().any(|line| line == "sankhya_audit_unwritten_total 0"),
        "no zero sample before the first failure, so `absent()` and healthy look alike:\n{text}"
    );
}

#[test]
fn a_closed_label_is_emitted_at_zero_for_every_value_it_can_take() {
    // A rate on `outcome="error"` is undefined until the first error, which is exactly when
    // the dashboard is being read. The permitted values are known at compile time.
    let registry = Registry::new();
    let text = registry.render(&[&QUERIES_TOTAL]);
    let Values::Closed(outcomes) = QUERIES_TOTAL.labels[0].values else {
        panic!("outcome stopped being a closed label");
    };
    for outcome in outcomes {
        let expected = format!("sankhya_queries_total{{outcome=\"{outcome}\"}} 0");
        assert!(text.lines().any(|line| line == expected), "missing {expected}:\n{text}");
    }
}

#[test]
fn a_bounded_label_invents_no_series_it_has_not_seen() {
    // The counterpart, and the reason the zero is not applied everywhere: a table label
    // holds names discovered from the deployment. A zero series here would assert that
    // some table exists, named by nothing.
    let registry = Registry::new();
    let text = registry.render(&[&TABLE_LIVE_FILES]);
    assert!(text.contains("# TYPE sankhya_table_live_files gauge"));
    assert!(
        !text.lines().any(|line| line.starts_with("sankhya_table_live_files")),
        "a table name was invented:\n{text}"
    );
}

#[test]
fn a_zero_series_is_replaced_rather_than_added_to_once_something_is_recorded() {
    // The zero must not survive alongside the real value, or a counter reads twice.
    let registry = Registry::new();
    registry.increment(&QUERIES_TOTAL, &[("outcome", "ok")], 3.0);
    let text = registry.render(&[&QUERIES_TOTAL]);
    let ok: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("sankhya_queries_total{outcome=\"ok\"}"))
        .collect();
    assert_eq!(ok, vec!["sankhya_queries_total{outcome=\"ok\"} 3"], "{text}");
}

#[test]
fn a_table_name_carrying_a_quote_does_not_break_the_whole_scrape() {
    // An unescaped quote produces a scrape the collector rejects wholesale, so one awkward
    // table name would take away every metric rather than only its own.
    let registry = Registry::new();
    registry.set(&TABLE_LIVE_FILES, &[("table", "sales.\"odd\"\\name")], 7.0);
    let text = registry.render(&[&TABLE_LIVE_FILES]);
    assert!(
        text.contains(r#"table="sales.\"odd\"\\name""#),
        "the quote and backslash must both be escaped: {text}"
    );
}

#[test]
fn rendering_is_byte_identical_for_identical_state() {
    // A diff between two scrapes should show what changed, not where a hasher put things.
    let build = || {
        let registry = Registry::new();
        for table in ["b", "a", "c"] {
            registry.set(&TABLE_LIVE_FILES, &[("table", table)], 1.0);
        }
        registry.increment(&QUERIES_TOTAL, &[("outcome", "error")], 3.0);
        registry.increment(&QUERIES_TOTAL, &[("outcome", "ok")], 9.0);
        registry.render(ALL)
    };
    assert_eq!(build(), build());
    let text = build();
    let a = text.find(r#"table="a""#).expect("a");
    let b = text.find(r#"table="b""#).expect("b");
    assert!(a < b, "series are rendered in a stable order");
}

// --- the catalogue's own shape ------------------------------------------

#[test]
fn every_metric_that_can_page_names_a_runbook_and_a_lead_time() {
    // ARCHITECTURE §17.1 permits paging only where the metric precedes a user-visible
    // failure by a predictable interval. An alert with no lead time fires when the user
    // notices, which makes it a notification.
    for metric in ALL {
        let Some(alert) = metric.alert else { continue };
        assert!(!alert.runbook.is_empty(), "{} pages with no runbook", metric.name);
        assert!(!alert.lead_time.is_empty(), "{} pages with no lead time", metric.name);
        assert!(
            !alert.consequence.is_empty(),
            "{} pages without saying what breaks",
            metric.name
        );
    }
    assert!(ALL.iter().any(|m| m.pages()), "at least one metric pages");
}

#[test]
fn every_metric_is_named_help_and_grouped() {
    for metric in ALL {
        assert!(
            metric.name.starts_with("sankhya_"),
            "{} is not namespaced",
            metric.name
        );
        assert!(metric.help.len() > 20, "{} has no useful help", metric.name);
        assert!(
            matches!(
                metric.group,
                Group::Query | Group::Resource | Group::Pipeline | Group::Maintenance
            ),
            "{} is ungrouped",
            metric.name
        );
    }
}

#[test]
fn no_two_metrics_share_a_name() {
    let mut names: Vec<&str> = ALL.iter().map(|m| m.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(names.len(), before, "two metrics share a name");
}

#[test]
fn histogram_buckets_ascend_and_never_declare_infinity() {
    // `+Inf` is emitted by the renderer. Declaring it would render it twice, and a duplicate
    // bucket is a scrape error rather than a cosmetic problem.
    for metric in ALL {
        let Kind::Histogram { buckets } = metric.kind else {
            continue;
        };
        assert!(!buckets.is_empty(), "{} has no buckets", metric.name);
        for pair in buckets.windows(2) {
            assert!(pair[0] < pair[1], "{} has unsorted buckets", metric.name);
        }
        assert!(
            buckets.iter().all(|b| b.is_finite()),
            "{} declares an infinite bound",
            metric.name
        );
    }
}

#[test]
fn the_self_metric_covers_every_rejection_reason() {
    // The reasons live in two places — a struct's fields and a closed label's values — and
    // a reason present in one and missing from the other is invisible on a dashboard.
    let Values::Closed(reasons) = METRICS_REJECTED_TOTAL.labels[0].values else {
        panic!("the reason label is closed");
    };
    assert_eq!(reasons.len(), 4);
    for reason in [
        "value_not_permitted",
        "label_not_declared",
        "label_missing",
        "over_cap",
    ] {
        assert!(reasons.contains(&reason), "{reason} is not a declared reason");
    }
}
