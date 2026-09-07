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

/// Audit records that could not be written to disk.
///
/// # Why this is a metric rather than only a log line
///
/// Because the failure is silent by nature. An audit that has stopped recording looks exactly
/// like a quiet server, and the difference is only visible if something counts the difference.
/// `AUDIT_RECORDS_TOTAL` rising while this one rises too is a chain that exists in memory and
/// not on disk --- which is the state `SEC-07` found shipped as the only state.
pub static AUDIT_UNWRITTEN_TOTAL: Metric = Metric {
    name: "sankhya_audit_unwritten_total",
    kind: Kind::Counter,
    unit: Unit::Count,
    labels: &[],
    group: Group::Query,
    help: "Audit records that were made and could not be written to disk. Any value above \
           zero means the chain on disk is shorter than the chain in memory.",
    alert: Some(Alert {
        runbook: "audit-unwritten",
        consequence: "the audit on disk is incomplete, and a restart loses everything that \
                      could not be written",
        lead_time: "none --- the first failure is already a gap",
    }),
};

/// The largest live-file count any table currently carries.
///
/// # Why this exists beside the per-table gauge
///
/// Because the per-table one names every table on the server, and `/metrics` is unauthenticated
/// by Prometheus's convention. The endpoint's own module claimed *"no label may carry tenant
/// data --- the metric catalogue enforces it structurally"*, and it does not: a label is bounded
/// by **cardinality**, not by content, and this one was filled with the fully-qualified name of
/// every servable table. `SEC-08`.
///
/// What pages is a table with too many files, and the number that says so is the largest one.
/// That number needs no label, so this is what the alert is on --- and the breakdown that says
/// *which* table is behind `server.metrics_detail`, for a deployment whose metrics port is
/// genuinely private.
pub static TABLE_LIVE_FILES_MAX: Metric = Metric {
    name: "sankhya_table_live_files_max",
    kind: Kind::Gauge,
    unit: Unit::Count,
    labels: &[],
    group: Group::Maintenance,
    help: "The largest number of files any one table currently consists of. Unlabelled on \
           purpose: a per-table breakdown enumerates the warehouse to an unauthenticated \
           endpoint, and is available under `server.metrics_detail`.",
    alert: Some(Alert {
        runbook: "compaction-debt",
        consequence: "query latency on the affected table roughly doubles as the file count \
                      passes a thousand, and keeps climbing",
        lead_time: "days, at ordinary write rates --- run the diagnostic, which names the \
                    table, or turn on `server.metrics_detail` where the port is private",
    }),
};

/// Live files per table --- the compaction debt that pages.
pub static TABLE_LIVE_FILES: Metric = Metric {
    name: "sankhya_table_live_files",
    kind: Kind::Gauge,
    unit: Unit::Count,
    labels: &[Label::identifier("table", TABLE_CAP)],
    group: Group::Maintenance,
    help: "Files a table currently consists of. A scan pays per file — opening it, reading \
           its footer, deciding whether to prune it — so this is what compaction debt costs. \
           Emitted only under `server.metrics_detail`: the label is the table's name, and \
           `/metrics` is unauthenticated.",
    alert: Some(Alert {
        runbook: "compaction-debt",
        consequence: "query latency on the affected table roughly doubles as the file count \
                      passes a thousand, and keeps climbing",
        lead_time: "days, at ordinary write rates — which is why the diagnostic reports a \
                    date rather than a value",
    }),
};

/// Bytes the process has allocated and not yet freed.
///
/// Counted at the allocator, which is the only place that sees all of it. The query
/// engine's own pool tracks what its operators reserve, and decode buffers, network
/// buffers, graph arenas and every third-party allocation sit outside that pool --- so a
/// query can stay inside its reservation and still exhaust the machine.
pub static MEMORY_IN_USE_BYTES: Metric = Metric {
    name: "sankhya_memory_in_use_bytes",
    kind: Kind::Gauge,
    unit: Unit::Bytes,
    labels: &[],
    group: Group::Resource,
    help: "Bytes allocated and not yet freed, counted at the global allocator. Everything \
           the process allocates passes through it, including what the query engine's own \
           accounting cannot see.",
    alert: None,
};

/// The highest the allocated total has been since the process started.
///
/// What a memory limit has to be set against. An average says nothing about whether a
/// workload fits, because the moment it does not fit is a peak.
pub static MEMORY_PEAK_BYTES: Metric = Metric {
    name: "sankhya_memory_peak_bytes",
    kind: Kind::Gauge,
    unit: Unit::Bytes,
    labels: &[],
    group: Group::Resource,
    help: "The highest the allocated total has been since this process started. A limit is \
           set against a peak, never against an average.",
    alert: None,
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

/// How many maintenance passes have completed.
///
/// # Why a counter of passes is worth exporting
///
/// Because `sankhya_table_live_files_max` pages on the *consequence* of maintenance not
/// keeping up, and its runbook opens by telling you the alert "almost never means compaction
/// is broken --- it usually means the duty cycle is too low". That was a claim an operator
/// had no way to check: the maintainer's tick, reclaim, decline and failure counts lived on
/// `MaintenanceHandle` and were read by two tests and a soak run, and by nothing that a
/// scrape could reach. A thread that had died and a duty cycle that was too low produced the
/// same alert and the same evidence.
///
/// A rate of zero on this counter is the distinction: passes are not happening at all.
pub static MAINTENANCE_TICKS_TOTAL: Metric = Metric {
    name: "sankhya_maintenance_ticks_total",
    kind: Kind::Counter,
    unit: Unit::Count,
    labels: &[],
    group: Group::Maintenance,
    help: "Maintenance passes completed since this server started. A rate of zero while \
           tables are being written means the maintainer is not running, which is a \
           different fault from a duty cycle that is too low.",
    alert: None,
};

/// Space returned to the filesystem by retiring superseded files.
pub static MAINTENANCE_BYTES_RECLAIMED_TOTAL: Metric = Metric {
    name: "sankhya_maintenance_bytes_reclaimed_total",
    kind: Kind::Counter,
    unit: Unit::Bytes,
    labels: &[],
    group: Group::Maintenance,
    help: "Bytes returned to the filesystem by retiring superseded files. Flat while file \
           counts rise means compaction is running and reclaiming nothing, which is what a \
           reader holding every version looks like.",
    alert: None,
};

/// Passes that declined to act because something was still reading.
///
/// Not a fault: declining is the sweeper honouring a lease, a clone or a snapshot, and is
/// the mechanism that stops a reader's files being reclaimed underneath it. It is here
/// because it is the benign explanation for reclaimed bytes staying flat, and separating it
/// from `sankhya_maintenance_failures_total` is what makes that counter mean one thing.
pub static MAINTENANCE_DECLINED_TOTAL: Metric = Metric {
    name: "sankhya_maintenance_declined_total",
    kind: Kind::Counter,
    unit: Unit::Count,
    labels: &[],
    group: Group::Maintenance,
    help: "Passes that declined to reclaim because a lease, clone or snapshot still reads \
           the files. Expected and healthy; it explains reclaimed bytes staying flat.",
    alert: None,
};

/// Passes that failed.
///
/// # Why this pages and the other three do not
///
/// Because it is the only one of the four whose non-zero value is unambiguous. A pass that
/// failed did not compact and did not reclaim, and nothing a user issues will report it ---
/// maintenance runs on its own thread, so a failure surfaces only as file counts climbing
/// until `sankhya_table_live_files_max` pages, days later, with a runbook that will send the
/// reader to raise a duty cycle that was never the problem.
pub static MAINTENANCE_FAILURES_TOTAL: Metric = Metric {
    name: "sankhya_maintenance_failures_total",
    kind: Kind::Counter,
    unit: Unit::Count,
    labels: &[],
    group: Group::Maintenance,
    help: "Maintenance passes that failed. Above zero means compaction and reclamation are \
           not happening for at least one table, and file counts are rising unopposed.",
    alert: Some(Alert {
        runbook: "maintenance-stalled",
        consequence: "files accumulate unopposed until reads slow and the disk fills; the \
                      compaction-debt page arrives days later and blames the duty cycle",
        lead_time: "days --- file counts climb before any read is slow enough to notice",
    }),
};

/// The whole catalogue, in the order it is documented.
pub static ALL: &[&Metric] = &[
    &QUERIES_TOTAL,
    &QUERY_DURATION_SECONDS,
    &ROWS_RETURNED_TOTAL,
    &CONNECTIONS_ACTIVE,
    &AUDIT_RECORDS_TOTAL,
    &AUDIT_UNWRITTEN_TOTAL,
    &TABLE_LIVE_FILES_MAX,
    &TABLE_LIVE_FILES,
    &MAINTENANCE_TICKS_TOTAL,
    &MAINTENANCE_BYTES_RECLAIMED_TOTAL,
    &MAINTENANCE_DECLINED_TOTAL,
    &MAINTENANCE_FAILURES_TOTAL,
    &MEMORY_IN_USE_BYTES,
    &MEMORY_PEAK_BYTES,
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
