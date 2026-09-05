<!-- GENERATED FILE — DO NOT EDIT.
     Produced by `cargo xtask write-catalogues` from crates/sankhya-metrics/src/catalogue.rs.
     `cargo xtask check-catalogues` fails the build if this file and that source disagree. -->

# SANKHYA — Metric catalogue

Every metric this build exports. A metric absent from this document is not merely undocumented — it cannot be recorded, because recording one requires passing its declaration.

**No label may carry tenant data.** A label is either restricted to a named set of values, in which case anything else is refused, or it holds a deployment-scoped identifier under a cap. There is no third kind, so a label that varies per row has no way to be declared.

## `sankhya_queries_total`

Statements that reached execution, by how they ended.

| | |
|---|---|
| Type | counter |
| Unit | count |
| Group | query behaviour |
| Label `outcome` | one of `ok`, `error`, `refused`, `cancelled` — anything else is refused |
| Pages | no |

## `sankhya_query_duration_seconds`

Wall-clock time from receiving a statement to sending its last row.

| | |
|---|---|
| Type | histogram |
| Unit | seconds |
| Group | query behaviour |
| Buckets | 0.001, 0.005, 0.025, 0.1, 0.5, 2.5, 10, 60 (`+Inf` implicit) |
| Label `outcome` | one of `ok`, `error`, `refused`, `cancelled` — anything else is refused |
| Pages | no |

## `sankhya_rows_returned_total`

Rows rendered to clients. Paired with query duration, this separates a slow query from a large one.

| | |
|---|---|
| Type | counter |
| Unit | count |
| Group | query behaviour |
| Labels | none |
| Pages | no |

## `sankhya_connections_active`

Client connections currently open.

| | |
|---|---|
| Type | gauge |
| Unit | count |
| Group | resource pressure |
| Labels | none |
| Pages | no |

## `sankhya_audit_records_total`

Entries appended to the hash-chained audit. A count that stops rising while queries continue means the audit is not recording them.

| | |
|---|---|
| Type | counter |
| Unit | count |
| Group | query behaviour |
| Labels | none |
| Pages | no |

## `sankhya_audit_unwritten_total`

Audit records that were made and could not be written to disk. Any value above zero means the chain on disk is shorter than the chain in memory.

| | |
|---|---|
| Type | counter |
| Unit | count |
| Group | query behaviour |
| Labels | none |
| **Pages** | yes — [`audit-unwritten`](runbooks/audit-unwritten.md) |
| Consequence | the audit on disk is incomplete, and a restart loses everything that could not be written |
| Lead time | none --- the first failure is already a gap |

## `sankhya_table_live_files_max`

The largest number of files any one table currently consists of. Unlabelled on purpose: a per-table breakdown enumerates the warehouse to an unauthenticated endpoint, and is available under `server.metrics_detail`.

| | |
|---|---|
| Type | gauge |
| Unit | count |
| Group | maintenance debt |
| Labels | none |
| **Pages** | yes — [`compaction-debt`](runbooks/compaction-debt.md) |
| Consequence | query latency on the affected table roughly doubles as the file count passes a thousand, and keeps climbing |
| Lead time | days, at ordinary write rates --- run the diagnostic, which names the table, or turn on `server.metrics_detail` where the port is private |

## `sankhya_table_live_files`

Files a table currently consists of. A scan pays per file — opening it, reading its footer, deciding whether to prune it — so this is what compaction debt costs. Emitted only under `server.metrics_detail`: the label is the table's name, and `/metrics` is unauthenticated.

| | |
|---|---|
| Type | gauge |
| Unit | count |
| Group | maintenance debt |
| Label `table` | a deployment-scoped name, at most 200 distinct values; past that, new series are refused and counted |
| **Pages** | yes — [`compaction-debt`](runbooks/compaction-debt.md) |
| Consequence | query latency on the affected table roughly doubles as the file count passes a thousand, and keeps climbing |
| Lead time | days, at ordinary write rates — which is why the diagnostic reports a date rather than a value |

## `sankhya_memory_in_use_bytes`

Bytes allocated and not yet freed, counted at the global allocator. Everything the process allocates passes through it, including what the query engine's own accounting cannot see.

| | |
|---|---|
| Type | gauge |
| Unit | bytes |
| Group | resource pressure |
| Labels | none |
| Pages | no |

## `sankhya_memory_peak_bytes`

The highest the allocated total has been since this process started. A limit is set against a peak, never against an average.

| | |
|---|---|
| Type | gauge |
| Unit | bytes |
| Group | resource pressure |
| Labels | none |
| Pages | no |

## `sankhya_metrics_rejected_total`

Recordings the registry refused. Non-zero means a call site disagrees with the catalogue, or a label has outgrown its cap and the metric is now incomplete.

| | |
|---|---|
| Type | counter |
| Unit | count |
| Group | resource pressure |
| Label `reason` | one of `value_not_permitted`, `label_not_declared`, `label_missing`, `over_cap` — anything else is refused |
| Pages | no |

---

## Named in the architecture and not emitted

`ARCHITECTURE.md` §17.1 names four metrics that receive paging alerts. One of them — compaction debt, above — is emitted. The other three are listed here rather than declared, because a metric permanently reading zero is indistinguishable from a healthy subsystem.

**retained log volume.** Nothing in this process drives ingest, so no replication slot retains anything to measure. The escalation ladder that acts on it exists and is tested; the running loop that would feed this metric does not.

**transaction-identifier freeze age.** Read from the transactional store by a supervisor that is not built. The value is a property of PostgreSQL rather than of this system, so emitting it requires a connection this process does not open.

**archive jobs awaiting attention.** Archival is M8. A gauge reading zero here would be indistinguishable from a healthy archive, and there is no archive.

