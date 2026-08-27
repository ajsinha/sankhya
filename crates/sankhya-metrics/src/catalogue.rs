//! Every metric this system exports.
//!
//! # The rule that keeps this honest
//!
//! **A metric declared here must be recorded somewhere in the source**, and
//! `cargo xtask check-catalogues` fails the build if one is not. Without that rule this file
//! drifts into a wishlist: a catalogue naming metrics nothing emits, published as
//! documentation, built on by dashboards that then show nothing and cannot say why.
//!
//! The rule bites. `ARCHITECTURE` §17.1 names four metrics that receive paging alerts ---
//! retained log volume, transaction-identifier freeze age, compaction debt, and archive jobs
//! awaiting attention --- and only **compaction debt** is declared below. The other three
//! measure machinery that does not run in this process yet: nothing drives ingest, so there
//! is no retained log to measure, and archival is `M8`. Declaring them anyway would produce
//! three metrics permanently reading zero, which is indistinguishable from three healthy
//! subsystems.
//!
//! See [`NOT_YET_EMITTED`] for the gap, stated rather than filled.

use crate::metric::{Alert, Group, Kind, Label, Metric, Unit};

/// How a query ended. Closed, because an outcome is a dimension and never a message.
const OUTCOMES: &[&str] = &["ok", "error", "refused", "cancelled"];

/// How many tables may be labelled before the metric stops adding series.
///
/// Two hundred is not a prediction. It is the point past which a per-table gauge costs more
/// than it informs --- nobody reads a two-hundred-line panel --- and reaching it is itself
/// worth knowing, which is why crossing it is counted rather than ignored.
const TABLE_CAP: usize = 200;

/// Queries that reached execution, by outcome.
pub static QUERIES_TOTAL: Metric = Metric {
    name: "sankhya_queries_total",
    kind: Kind::Counter,
    unit: Unit::Count,
    labels: &[Label::closed("outcome", OUTCOMES)],
    group: Group::Query,
    help: "Statements that reached execution, by how they ended.",
    alert: None,
};

/// How long queries take, end to end.
pub static QUERY_DURATION_SECONDS: Metric = Metric {
    name: "sankhya_query_duration_seconds",
    kind: Kind::Histogram {
        // Spread over four orders of magnitude, because both ends matter: a catalogue lookup
        // belongs in the first bucket and an analytical scan in the last, and a range that
        // covers only one of them makes the other invisible.
        buckets: &[0.001, 0.005, 0.025, 0.1, 0.5, 2.5, 10.0, 60.0],
    },
    unit: Unit::Seconds,
    labels: &[Label::closed("outcome", OUTCOMES)],
    group: Group::Query,
    help: "Wall-clock time from receiving a statement to sending its last row.",
    alert: None,
};

/// Rows sent to clients.
pub static ROWS_RETURNED_TOTAL: Metric = Metric {
    name: "sankhya_rows_returned_total",
    kind: Kind::Counter,
    unit: Unit::Count,
    labels: &[],
    group: Group::Query,
    help: "Rows rendered to clients. Paired with query duration, this separates a slow \
           query from a large one.",
    alert: None,
};

/// Client connections currently being served.
pub static CONNECTIONS_ACTIVE: Metric = Metric {
    name: "sankhya_connections_active",
    kind: Kind::Gauge,
    unit: Unit::Count,
    labels: &[],
    group: Group::Resource,
    help: "Client connections currently open.",
    alert: None,
};

/// Records appended to the audit chain.
pub static AUDIT_RECORDS_TOTAL: Metric = Metric {
    name: "sankhya_audit_records_total",
    kind: Kind::Counter,
    unit: Unit::Count,
    labels: &[],
    group: Group::Query,
    help: "Entries appended to the hash-chained audit. A count that stops rising while \
           queries continue means the audit is not recording them.",
    alert: None,
};

/// Live files per table --- the compaction debt that pages.
pub static TABLE_LIVE_FILES: Metric = Metric {
    name: "sankhya_table_live_files",
    kind: Kind::Gauge,
    unit: Unit::Count,
    labels: &[Label::identifier("table", TABLE_CAP)],
    group: Group::Maintenance,
    help: "Files a table currently consists of. A scan pays per file — opening it, reading \
           its footer, deciding whether to prune it — so this is what compaction debt costs.",
    alert: Some(Alert {
        runbook: "compaction-debt",
        consequence: "query latency on the affected table roughly doubles as the file count \
                      passes a thousand, and keeps climbing",
        lead_time: "days, at ordinary write rates — which is why the diagnostic reports a \
                    date rather than a value",
    }),
};

/// Recordings the registry refused.
pub static METRICS_REJECTED_TOTAL: Metric = Metric {
    name: "sankhya_metrics_rejected_total",
    kind: Kind::Counter,
    unit: Unit::Count,
    labels: &[Label::closed(
        "reason",
        &[
            "value_not_permitted",
            "label_not_declared",
            "label_missing",
            "over_cap",
        ],
    )],
    group: Group::Resource,
    help: "Recordings the registry refused. Non-zero means a call site disagrees with the \
           catalogue, or a label has outgrown its cap and the metric is now incomplete.",
    alert: None,
};

/// The whole catalogue, in the order it is documented.
pub static ALL: &[&Metric] = &[
    &QUERIES_TOTAL,
    &QUERY_DURATION_SECONDS,
    &ROWS_RETURNED_TOTAL,
    &CONNECTIONS_ACTIVE,
    &AUDIT_RECORDS_TOTAL,
    &TABLE_LIVE_FILES,
    &METRICS_REJECTED_TOTAL,
];

/// Metrics `ARCHITECTURE` §17.1 names and this build does not emit, and why.
///
/// Stated as data rather than as prose so the generated documentation carries it, and so a
/// reader comparing the architecture against the catalogue finds the difference explained
/// instead of finding it themselves.
pub static NOT_YET_EMITTED: &[(&str, &str)] = &[
    (
        "retained log volume",
        "Nothing in this process drives ingest, so no replication slot retains anything to \
         measure. The escalation ladder that acts on it exists and is tested; the running \
         loop that would feed this metric does not.",
    ),
    (
        "transaction-identifier freeze age",
        "Read from the transactional store by a supervisor that is not built. The value is \
         a property of PostgreSQL rather than of this system, so emitting it requires a \
         connection this process does not open.",
    ),
    (
        "archive jobs awaiting attention",
        "Archival is M8. A gauge reading zero here would be indistinguishable from a healthy \
         archive, and there is no archive.",
    ),
];
