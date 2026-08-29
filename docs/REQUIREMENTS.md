<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# SANKHYA — Requirements Specification

**Document ID:** SNK-RD-001
**Version:** 0.1.0 (draft for review)
**Status:** Implementation — M0–M7 complete, M8 next
**Date:** 2026-08-26
**Supersedes:** `docs/initial_reqmt.docx` ("Unified Enterprise Data Architecture & Requirements Document", URARD)

---

## 0. About this document

### 0.1 Provenance

This specification is the amended successor to the original brief in `docs/initial_reqmt.docx`. That brief established the vision and is preserved unchanged in the repository for provenance. This document replaces it as the normative requirements source.

The amendment process engaged four independent reviewers, each of whom produced a written critique with specific recommended amendments:

| Reviewer | Remit |
|---|---|
| Principal systems architect | Process and deployment model, runtime topology, consistency model, security architecture, crate decomposition, maintenance architecture, data tiering |
| VLDB / database-internals specialist | Change data capture, table formats, storage layout, schema management, durability and reconciliation |
| OLAP and graph-engine specialist | Query execution, performance modelling, SLO derivation, analytical correctness, graph algorithms and semantics |
| Senior Rust engineer | Buildability, workspace design, dependency management, testing strategy, CI/CD, delivery sequencing and sizing |

Every ecosystem claim in this document was verified against crates.io, upstream repositories and issue trackers on **2026-08-25**. Claims that could not be verified are marked **[UNVERIFIED]** and carry an owner and a deadline. A requirement resting on an unverified claim is not ready to build.

### 0.2 How to read this document

- **Requirement identifiers are stable and permanent.** `FR-xxx-nn` for functional, `NFR-xxx-nn` for non-functional, `CON-nn` for constraints, `DEC-nn` for decision records, `RSK-nn` for risks. Identifiers are never reused, renumbered, or reassigned. A withdrawn requirement is marked `WITHDRAWN` and retained.
- **RFC 2119 keywords.** MUST / MUST NOT / SHALL are mandatory. SHOULD / SHOULD NOT are strong recommendations requiring written justification to deviate. MAY is optional.
- **Every NFR carries a test identifier.** An NFR without a test identifier is not a requirement; it is an aspiration. This rule is enforced in review.
- **Every performance figure names its conditions.** Hardware, dataset, cache state, concurrency, and preconditions. A performance number without conditions is not falsifiable and is therefore excluded.

### 0.3 The single most important framing decision

> **SANKHYA is a general-purpose data engine. Risk management, financial crime detection and counterparty analytics are *use cases*, delivered as optional domain packs. They are not the system.**

Nothing in the SANKHYA core may encode knowledge of a trade, a desk, a counterparty, a transaction, a patient or a shipment. The core deals in tenants, schemas, tables, rows, columns, vertices, edges, versions and policies. Domain semantics arrive exclusively through a published extension API (§9).

This framing is load-bearing and is tested, not merely asserted: see `FR-EXT-09` (the reference non-financial pack) and `NFR-EXT-01` (the domain-leakage gate).

---

## 1. Vision and scope

### 1.1 Problem statement

Enterprises operating on large transactional datasets are forced into a three-system architecture: a relational database for operational mutations, a separate analytical cluster for reporting, and a specialised graph store for relationship analysis. This fragmentation produces four costs that compound:

- **Replication lag.** Analytical and graph views trail the operational system by minutes to hours. Decisions are made on stale data, and the staleness is rarely quantified.
- **Divergent copies.** The same entity exists in three or more stores with independent schemas, independent bugs and independent retention. Reconciliation becomes a permanent operational function rather than a one-time integration cost.
- **Fragmented governance.** Three engines mean three authorization models, three audit trails and three answers to "who saw this data". No single system can answer a compliance question completely.
- **Operational surface.** Message brokers, connector clusters, schedulers and JVM analytical estates require an operational investment that frequently exceeds the analytical workload itself.

### 1.2 Solution statement

SANKHYA is a single self-contained binary, written in Rust, that provides transactional, analytical and graph access over one governed copy of the data:

- **PostgreSQL** as the authoritative transactional system of record, either supervised in-process or attached externally.
- **A native in-process change data capture bridge** streaming PostgreSQL's write-ahead log into versioned columnar storage, with no message broker, no connector cluster and no JVM.
- **An open lakehouse table format** on local or object storage, laid out so that external engines can read it directly.
- **Apache DataFusion over Apache Arrow** for vectorized analytical execution.
- **An Arrow-backed in-memory graph engine** for relationship analysis.
- **A unified API** exposing all three models through one authentication, authorization and audit path.

### 1.3 In scope for v1

Transactional data management; automatic and continuous synchronization to analytical storage; analytical SQL; graph traversal and analytics; time travel and as-of queries; data lifecycle tiering with source purge; multi-tenancy; authentication, authorization, row- and column-level security; audit; the domain-pack extension API; a reference non-financial pack; reference risk and financial-crime packs; single-node and small multi-node deployment; operator tooling.

### 1.4 Explicitly out of scope for v1

Recording non-goals is a requirement of this document, because a specification without non-goals grows without bound.

| ID | Non-goal | Rationale |
|---|---|---|
| `NG-01` | SANKHYA is not a distributed OLTP database | PostgreSQL is the system of record; sharding the customer's ledger is their architectural decision, not ours |
| `NG-02` | SANKHYA is not a durable graph database | The graph is a derived, rebuildable projection of the tables. It has no independent durability contract |
| `NG-03` | SANKHYA is not a stream-processing engine | It ingests change data; it does not provide general stream transformation, windowed joins over unbounded streams, or a streaming DSL |
| `NG-04` | No cross-region active-active writes | Single-writer topology only. Cross-region is disaster recovery, not active-active |
| `NG-05` | No multi-source CDC beyond PostgreSQL in v1 | Other sources, if ever, arrive as optional external feeders through a documented staging interface |
| `NG-06` | Not a golden source of record for any domain | SANKHYA stores what it is given; it is not the authoring system for trades, claims, shipments or any other domain entity |
| `NG-07` | No pricing, valuation, scenario-generation or model libraries | Domain packs may consume such outputs; the engine does not produce them |
| `NG-08` | No case management, workflow or alerting UI | SANKHYA produces results and events; presentation and workflow are downstream |
| `NG-09` | Windows is not a supported server platform in v1 | Signal handling, process semantics, file locking and PostgreSQL supervision differ enough to roughly double the integration matrix. CLI and client SDK are supported on Windows |
| `NG-10` | No distributed query execution in v1 | Single-node execution with query routing. See `DEC-14` for the trigger and the migration path |
| `NG-11` | Exact betweenness and closeness centrality on large graphs | `O(V·E)` is computationally infeasible at the target scale. Approximate variants are provided instead |
| `NG-12` | No bespoke graph query language | Structured API and SQL table functions in v1; the ISO SQL/PGQ standard in v2 |

---

## 2. Definitions

| Term | Definition |
|---|---|
| **Core** | The domain-agnostic SANKHYA engine. May not contain domain semantics |
| **Domain pack** | An optional, versioned extension contributing schemas, functions, algorithms, views, rules and policy vocabulary through the extension API |
| **Ledger tier (T0)** | PostgreSQL itself — the authoritative, always-current transactional store |
| **Arrival buffer (T1)** | In-memory Arrow-formatted captured changes not yet committed to the table format |
| **Published tier (T2)** | Committed tables in the open table format on shared storage. The externally-readable tier |
| **Derived tier (T3)** | Materialized aggregates, themselves stored as tables |
| **LSN** | PostgreSQL log sequence number. The global, transaction-consistent ordering coordinate used throughout SANKHYA |
| **Snapshot** | An immutable, identified version of a table |
| **Epoch** | An immutable, identified version of a hydrated graph, bound to a table snapshot |
| **Splice** | The operation joining several tiers into one logically consistent query result |
| **Coverage interval** | The LSN range a given tier is known to contain |
| **Lease** | A time-bounded claim on a snapshot preventing its files from being reclaimed |
| **Tiering** | Lifecycle migration of data from the ledger tier to the published tier, with purge from the source |
| **Pack extension API** | The stable, semver-governed interface through which domain packs contribute capability |

---

## 3. Constraints

Constraints are externally imposed and not negotiable within this project.

| ID | Constraint | Source |
|---|---|---|
| `CON-01` | The system SHALL be implemented in Rust | Original brief |
| `CON-02` | No JVM component SHALL be required anywhere in the deployed system | Original brief |
| `CON-03` | The deployed system SHALL be a single binary artifact with a single configuration file, requiring no message broker, connector cluster, scheduler or external coordinator | Original brief |
| `CON-04` | Apache Arrow SHALL be the universal in-memory data format across all engines | Original brief |
| `CON-05` | PostgreSQL SHALL be the authoritative store for all mutations | Original brief |
| `CON-06` | No source file SHALL exceed 1,500 lines of code excluding comments and blank lines | Original brief; see `DEC-19` for the amended and enforceable form |
| `CON-07` | The system SHALL be modular, with appropriately designed crates and extensive test coverage | Original brief; see `NFR-QUAL-*` for the measurable form |
| `CON-08` | Analytical storage SHALL be directly readable by external engines including Apache Spark | Owner directive, 2026-08-25 |
| `CON-09` | Analytical table naming SHALL be relatable to operational table naming | Owner directive, 2026-08-25 |
| `CON-10` | The architecture SHALL be general-purpose; domain capabilities are use cases | Owner directive, 2026-08-25 |
| `CON-11` | The analytical tier MAY lag the transactional tier by a few seconds | Owner directive, 2026-08-25 — this is a *relaxation*, and it is load-bearing for `DEC-08` |
| `CON-12` | **Proprietary**, owned by Ashutosh Sinha; not open source, and no right granted without express written authorisation. Dependencies remain under their own licences, which this does not displace, and their obligations MUST still be honoured | Repository `LICENSE`; owner directive, 2026-08-28 |

---

## 4. Decision records

The original brief contained several statements that cannot all be true simultaneously, and several that rest on assumptions the ecosystem does not support. Each is resolved below. These decisions are normative: the requirements in §5 onward depend on them.

Each record states the original position, the problem, the decision, and the cost of the decision. Recording the cost is mandatory — a decision record that lists only benefits is advocacy, not engineering.

---

### DEC-01 — Change data capture is native and in-process; Debezium and Kafka are removed

**Original position.** "CDC via Debezium (Kafka Connect) streaming into a lightweight Rust Kafka consumer" (§3.2), alongside "zero JVM overhead" (§1), "the delta bridge code will be baked in the sankhya binary and we do not want any separate component" (§3.2), and "complete elimination of JVM-based analytical clusters" (§5.2).

**Problem.** Debezium runs on Kafka Connect. Kafka Connect is a JVM process. Kafka is a JVM cluster. Three of the four statements above are violated by the fourth. This is not a tension to be balanced; it is a contradiction.

**Decision.** SANKHYA implements a native Rust logical-replication consumer speaking the PostgreSQL streaming replication protocol (`START_REPLICATION ... LOGICAL` over `CopyBoth`) with the built-in `pgoutput` output plugin, in-process. Debezium and Kafka are removed from the architecture.

**Verified constraint.** `tokio-postgres` 0.7.18 has **no** logical replication support — the string "replication" does not appear in its source, and `postgres-protocol` 0.6.12 has no `CopyBoth`, no `XLogData` and no logical-replication message parsing. `sqlx` 0.9.0 likewise lacks `CopyBoth` support. Available third-party options (`pgwire-replication` 0.4.0, `pg_walstream` 0.8.1) are under a year old, pre-1.0 and thinly maintained; `pg_walstream` additionally carries a libpq C dependency that conflicts with single-binary goals.

**Consequent requirement.** The `pgoutput` **decoder** is SANKHYA's own code, in a pure crate with no I/O, fuzzed continuously (`FR-CDC-02`). Only the connection and `CopyBoth` transport may come from a third party, and it sits behind a trait so it can be replaced or in-housed without disturbing callers.

**Cost.** We forgo Debezium's connector ecosystem, its schema registry, its DDL handling and a decade of production hardening. We must build ourselves: slot lifecycle, LSN checkpointing, standby status heartbeats, initial snapshot handoff, TOAST handling, replica-identity policy, DDL capture and idempotent apply. This is real work and it is concentrated in the most correctness-critical part of the system.

**Reversibility.** High. `FR-CDC-01` places the source behind a `CdcSource` trait. If multi-source ingest is ever required, it arrives as an optional external feeder writing to a documented staging interface — never by readmitting Kafka into the core.

---

### DEC-02 — "Embedded PostgreSQL" means a supervised child process

**Original position.** "Postgres is locally compiled and included in the Sankhya binary. There will not be a separate postgres instance anywhere." (§3.1)

**Problem.** PostgreSQL is a multi-process, fork-based C server. A postmaster forks a backend per connection plus background workers (checkpointer, background writer, WAL writer, autovacuum launcher, logical replication launcher, statistics collector), coordinates through shared memory, and requires a filesystem data directory. Its process model is inseparable from its concurrency and crash-safety design. It cannot become Rust functions in our address space, and no amount of engineering makes it so.

**Verified.** `postgresql_embedded` 0.21.0's `bundled` feature embeds the PostgreSQL *archive* into the binary at compile time; at runtime it extracts that archive and **spawns `postgres` as a child operating-system process**. Without `bundled`, it downloads binaries from GitHub on first run.

**Decision.** Two first-class modes behind one `OltpProvider` trait:

- **Managed mode.** PostgreSQL binaries are carried inside the SANKHYA executable as a compressed archive, extracted on first boot into the SANKHYA data directory, and run as a **supervised child process** whose entire lifecycle — `initdb`, start, readiness, health, backup, upgrade, shutdown — is owned by SANKHYA. The postmaster listens only on a Unix domain socket inside the data directory (`listen_addresses = ''`). To an operator this is one process tree, one artifact, one configuration file, no external PostgreSQL installation and no DBA action.
- **Attached mode.** SANKHYA connects to an externally managed PostgreSQL cluster. **This is the production mode for multi-node deployments** — see `DEC-03`.

The managed-mode claim is true, defensible and a genuine differentiator. The original literal claim is not, and building an architecture document on it would produce a specification no engineer could implement.

**Cost.** Redistributing PostgreSQL binaries creates obligations: licence notices, SBOM entries, and **responsibility for shipping a patch release whenever PostgreSQL issues a security release**. Major-version upgrades require both old and new binaries present simultaneously for `pg_upgrade`, inflating artifact size. See `FR-OPS-11`.

**Related decision — minimum version.** PostgreSQL **17 or later** is required, not "16+". PostgreSQL 17 introduced failover-capable logical replication slots; without them, a routine database failover destroys the replication slot and forces a full re-snapshot of every replicated table — a multi-hour outage of the analytical tier triggered by an ordinary HA event. **[UNVERIFIED — confirm against PostgreSQL release notes before publication. Owner: VLDB reviewer.]**

---

### DEC-03 — Node roles replace the flat "stateless nodes" model

**Original position.** "Horizontal compute scalability by deploying stateless SANKHYA server nodes backed by shared cloud object storage." (§5.2)

**Problem.** A node hosting a PostgreSQL primary is the most stateful component in the deployment. A node holding a hydrated graph in RAM is stateful, though rebuildable. These cannot be the same node type as a stateless query executor.

**Decision.** One binary, three roles:

| Role | State | Scaling | Availability |
|---|---|---|---|
| **Coordinator** | Owns or points at the OLTP primary; runs the CDC apply loop; runs maintenance; hosts the table catalog | Exactly one active | Leader election; PostgreSQL failover |
| **Executor** | None beyond caches | Genuinely stateless; scales linearly | Trivial — any node serves any query |
| **Graph** | RAM-resident hydrated graph | Partitioned by tenant | Loss is a cache miss; rebuild has a stated RTO |

**The honest statement, which belongs in the architecture document verbatim:** there is no configuration in which N nodes share writable state with zero coordination. Either the object store is the coordinator (via atomic conditional writes) or PostgreSQL is. The "no separate component" constraint (`CON-03`) is honoured in **packaging** — one binary, one artifact, one configuration file, one process per node — and cannot be honoured in **topology**, where a multi-writer cluster has exactly one logical coordinator by definition. SANKHYA's answer is that the coordinator is a **role of the same binary**, not a different component.

Two independent lines of analysis reached this conclusion: table-commit serialization, and maintenance-job coordination. Convergence from unrelated directions is strong evidence the conclusion is correct.

**Cost.** Multi-node deployments require attached, highly-available PostgreSQL. Managed mode is single-node only. This is a documented product boundary, not a defect.

---

### DEC-04 — The core is domain-agnostic; domains are packs

**Original position.** The brief describes "a general-purpose, unified embedded server architecture" but devotes its detailed sections to market risk, VaR, AML, UBO and counterparty credit, in enough depth that a reader would reasonably build a risk system.

**Owner directive (2026-08-25).** "This system needs to have a general purpose architecture. Risk, AML etc. are use cases. Do not design something which is just for risk."

**Decision.** A strict two-tier structure:

- **Core.** Tenants, schemas, tables, rows, columns, typed vertices, typed edges, versions, policies, jobs. The core may not name a domain concept.
- **Packs.** Optional, versioned bundles contributing schemas, logical types, functions, graph algorithms, materialized views, rules, endpoints and policy vocabulary through the published extension API (§9).

**Generalization rule.** Every domain requirement is restated as the general capability beneath it, with the domain use retained as an illustrative example:

| Domain framing | General capability (core) |
|---|---|
| VaR / Expected Shortfall | Exact order statistics with a named interpolation convention, and a labelled exact-versus-approximate contract |
| Scenario P&L vector aggregation | Fixed-size numeric list columns with element-wise aggregation, where reduction order is semantically significant |
| Ultimate beneficial ownership | Weighted path-product aggregation with damping, cycle tolerance and a pruning threshold |
| Laundering-chain detection | Time-respecting path traversal over edges with validity intervals |
| Structuring / smurfing detection | Temporal motif matching: a windowed pattern over typed edges with numeric predicates |
| Risk coverage checks | Data-completeness measures attachable to any aggregate |
| Counterparty exposure paths | k-shortest loopless paths over a typed edge subset |

**The discriminating test, for use in review:** if a parameter's *name* contains a domain noun, it belongs in a pack. `threshold: Decimal` is core; `control_threshold_pct` is a pack.

**Cost.** Approximately +20–25% on v1 core effort (§13). The return begins at the second pack and is decisive by the third: a new domain costs 8–12 engineer-weeks rather than a rewrite, and — more valuably — most of that work is declarative, so it can be delivered by a domain analyst rather than a systems engineer.

---

### DEC-05 — The graph model is multi-relational and temporal, not homogeneous

**Origin.** This decision was not present in any early draft. It emerged from `DEC-04` and is the most consequential consequence of it.

**Problem.** A single homogeneous property graph — one node type, one edge type, a plain CSR — is adequate for ownership and counterparty networks, which are near-homogeneous. It is a **finance-shaped assumption**. Logistics networks, healthcare provider networks, telecommunications fraud graphs and IoT topologies are heterogeneous, multi-relational and temporal. Under a single-edge-type model a pack must encode edge types into edge weights, destroying both type safety and traversal performance.

**Decision.** The core graph model supports **typed vertices and typed edges** with per-edge-type CSR segments, plus **edge validity intervals** enabling time-respecting traversal. Traversal primitives accept an edge-type mask. Memory accounting is per-edge-type.

**Cost.** +4 to +6 engineer-weeks in the graph milestone over a single-type design, and a more complex memory model that the capacity documentation must publish per edge type.

**Why it must be decided now.** Retrofitting is brutal: every pack's traversal calls and every persisted graph snapshot would change. Note also that this is precisely the class of problem a naming lint cannot detect — the identifiers would have been immaculately neutral while the abstraction was bent toward one domain. **Naming lints catch leakage; reference packs catch shape.**

---

### DEC-06 — SANKHYA owns the table provider; format crates are a metadata-only dependency

**Problem.** DataFusion 55.0.0 is built on Arrow 59.2.0. The released `deltalake-core` 0.32.4 and `iceberg` 0.10.1 are built on Arrow 58, and their DataFusion integrations pin `datafusion ^53.1.0` — two majors behind. Two Arrow majors cannot coexist in one binary: `RecordBatch`, `ArrayRef` and `SchemaRef` would be distinct, incompatible Rust types, and the trait-identity problem is worse than the type problem — `deltalake::DeltaTable` implements `datafusion@53::TableProvider`, which a `datafusion@55::SessionContext` cannot accept at all. Additionally, `object_store` appears in DataFusion's public API, and there is no FFI escape hatch for a trait object. Using either vendor table provider therefore forces the entire engine down to DataFusion 53.1 / Arrow 58.

**Verified, 2026-08-25.**

```
delta-rs main/Cargo.toml:   arrow = "59"  parquet = "59"  datafusion = "55"
                            buoyant_kernel 0.25.x, features = ["arrow-59"]
delta_kernel 0.27.1:        datafusion dependency: NONE
                            arrow ^58 OR ^59, feature-gated
deltalake-core 0.32.4:      arrow ^58, parquet ^58, object_store ^0.13.2,
                            datafusion ^53.1.0 (optional)
iceberg-datafusion 0.10.1:  datafusion ^53.1.0, parquet ^58
```

Two facts change the picture: the fix is **already merged upstream in delta-rs `main`** and awaits release, and `delta_kernel` has **no DataFusion dependency at all** while already supporting Arrow 59.

**The price of pinning down.** Remaining on DataFusion 53.1 forfeits: the entire statistics-driven execution generation (NDV extracted from Parquet metadata, the pluggable `StatisticsRegistry`, statistics-ordered TopK early stopping, corrected partial-aggregate counts); intra-file early stopping via statistics and dynamic filters; dynamic filters for range-partitioned joins; morsel-driven Parquet scans (~2× on skewed data); sort-merge join semi/anti improvements (20–50× on near-unique left and full joins); TopK pushdown through joins; the upstream memory-limiting Parquet metadata cache; and pluggable spill backends. Mapped onto the SLO tiers of §6, this is the difference between meeting and missing three of seven.

**Decision.** SANKHYA implements its own DataFusion `TableProvider`. Format crates are used **only for metadata** — snapshot resolution, file listing, per-file statistics, deletion-vector payloads, schema and partition specification. Scan execution runs on DataFusion's own `ParquetSource` / `DataSourceExec` at the workspace's current Arrow version. **No bulk Arrow data crosses a crate-version boundary; only small metadata structures, which convert trivially.**

We are not writing a scan engine. DataFusion still provides `ParquetSource`, `FileScanConfig`, `DataSourceExec`, `PruningPredicate`, `RowFilter`, `RowSelection`, page-index and row-group pruning, morsel-driven parallelism, dynamic-filter integration and the `ParquetFileReaderFactory` hook. We are writing *metadata → (file list, row selections, statistics)*.

**Estimated cost.** 3,000–5,500 lines including tests; 3–5 engineer-months for the Delta path, plus 2–3 engineer-months to add a second format.

**This is better than the vendor wrapper, not merely a workaround.** Owning the provider is the only way to (a) inject SANKHYA's own NDV and histograms into the optimizer — neither vendor provider supplies NDV, which is the root cause of bad join ordering; (b) perform partition-transform inversion and derived-column correlation that the vendor providers do not; (c) wire the Parquet reader to our own cache hierarchy; (d) order files by statistics to enable TopK early stopping; and (e) attach deletion-vector-derived `RowSelection` at plan time so deletes prune pages rather than acting as a post-filter.

**Cost.** Approximately four thousand lines of code we own and must maintain, including the correctness burden of statistics translation and delete resolution.

---

### DEC-07 — The CDC landing zone is append-only; merge happens on read

**Problem.** A CDC applier must apply updates and deletes. Both formats handle this badly today:

- **Verified:** delta-rs **cannot write deletion vectors**. Upstream issue delta-io/delta-rs#4512 (open, created 2026-06-03) states that delta-rs reads and preserves deletion vectors, but `merge`, `update` and `delete` all use copy-on-write, rewriting entire affected Parquet files. Updating 1,000 rows scattered across 1,000 files of one million rows each rewrites 10⁹ rows to change 1,000 — write amplification of roughly 10⁶. Unusable on a continuous ingest path.
- **Iceberg equality deletes** are a predicate anti-joined against every lower-sequence-number data file. Ten million delete keys produce an 80 MB hash table — far outside last-level cache, so every probe is a DRAM round trip — costing roughly 0.5–0.6 seconds on 32 cores *before any query work begins*. The Iceberg project is itself moving away from them.
- **Verified:** `iceberg-rust` 0.10.1 is **append-only**: no `RowDelta`, no `Overwrite`, no `RewriteDataFiles`. Copy-on-write and merge-on-read remain an open upstream epic.

**Decision.** The CDC landing zone is **append-only**: an insert-only change log carrying `(primary_key, commit_lsn, operation, payload)`. No deletes, no updates, therefore no deletion vectors and no equality deletes. Current state is produced by **SANKHYA's own merge-on-read** — a partition-local latest-version-per-key resolution. A background compaction job periodically materializes the change log into a clean **published table** by bulk whole-partition rewrite, which is efficient precisely because it is bulk rather than scattered.

**Why this is strategically important beyond avoiding the two problems above.** It removes the dependency on either format's mutation semantics, reducing both formats to versioned file containers with metadata. That makes the format choice **substantially reversible**, and it makes the arrival-buffer design format-independent.

**Bounded cost.** The merge operator's cost is bounded by the change-log size, which is bounded by the compaction interval. At 2,000 updates per second with five-minute compaction the log holds 600,000 rows; a partition-local deduplication over 600,000 rows is single-digit milliseconds.

**This decision has a direct consequence for `CON-08` — see `DEC-08`.**

---

### DEC-08 — Two published surfaces, two freshness contracts

**Problem.** `CON-08` requires that tables be directly readable by external engines such as Spark with no SANKHYA process involved. `DEC-07` resolves latest-version-per-key at read time inside SANKHYA. An external engine reading the raw change log would see un-merged rows and compute wrong answers. These requirements are in direct conflict.

**Decision.** The un-merged change data is **not hidden**. It is published as a **sibling table with a distinct name**, so that every directory under the warehouse root is correct for an external reader standing alone:

```
<warehouse_root>/<schema>/<table>/            merged current state — correct standalone
<warehouse_root>/<schema>/<table>__changes/   append-only change log — correct standalone
${SANKHYA_DATA}/hotwal/, spill/               in-flight, node-local, never on shared storage
```

**Why a published sibling rather than a hidden staging area.** An earlier form of this decision placed un-merged data in an internal staging area on shared storage. Publishing it instead is strictly better on three counts:

1. **Each byte is written to shared storage once, not twice.** The apply path writes the change log; compaction reads it to build the base.
2. **No external reader can obtain a wrong answer from either path.** One is the merged state; the other is exactly the change log its name declares. A hidden staging area relies on external readers not finding it — which is a convention, not a guarantee.
3. **It gives external consumers a genuinely fresh path that a hidden area could not.** Appending requires no merge, so the change log is as fresh as the batch interval.

The change log is independently valuable: it *is* the change-data feed, and it is the immutable audit record.

**Two external contracts, both stated explicitly:**

| Contract | Read | Freshness | Correctness |
|---|---|---|---|
| **Simple** | The base table alone | Publish cadence | **Always correct standalone.** Zero knowledge required |
| **Fresh** | Base and change log, via a merge definition SANKHYA publishes in its catalog and in the table's identity sidecar | Batch interval — seconds | Correct if the documented merge is applied |

The two coverage ranges are **disjoint by construction** — the base covers up to its high-water mark and the delta covers strictly beyond it — so double-counting is impossible rather than merely unlikely.

**Append-only and keyless tables have no second stage at all.** A table with no primary key has no "current row", so the base *is* the append target and its freshness equals the batch interval at zero merge cost. This is the correct model for event and telemetry data and covers a large fraction of tables in a general-purpose deployment.

**The honest disclosure**, which belongs in the specification rather than being discovered during an integration: external readers taking the simple path see **mutable** tables at publish cadence — minutes, not seconds. This is not a configuration choice; it follows directly from copy-on-write mutation meeting the correct-standalone requirement. Three responses are supported: read the Fresh contract; shorten the publish interval and pay measured write amplification; or declare the table append-only where the semantics permit, in which case there is no merge and no lag.

---

### DEC-09 — Freshness is a read-path property, not a write-path property

**Problem.** The naive way to reduce analytical lag is to commit to the table format more often. That produces small files, inflates version counts and grows metadata — and metadata cost lands on *query planning* latency, not scan latency. The result is the well-known failure mode where a "real-time lakehouse" is either stale or slow, and attempts to fix staleness make it slower.

**Quantified.**

| Commits per table | Versions/day | Effect |
|---|---|---|
| 1 per minute | 1,440 | Negligible |
| 1 per second | 86,400 | Noticeable; snapshot resolution begins to cost |
| 10 per second | 864,000 | Metadata dominates; **planning latency becomes p99** |

**Decision.** The commit interval is tuned for **storage efficiency**, defaulting to 10–60 seconds, and freshness is served from an in-memory arrival buffer spliced into queries at an exact LSN boundary. The CDC batch interval is therefore an **architectural parameter, not a tuning detail** — it simultaneously governs file size, version count and metadata cost, which are three of the four things that determine analytical speed.

**The correctness property that makes this safe.** Every committed snapshot records the exact LSN it contains; every buffer epoch records its LSN range. The planner selects tiers whose coverage intervals are **contiguous, non-overlapping and collectively cover `[0, target_lsn]`**. The boundary is exact, not approximate: there is no double-counting window and no gap. Because the splice coordinate is the LSN — a global, transaction-consistent ordering — a multi-table transaction is either wholly visible or wholly invisible. A design splicing on wall-clock time would lose this and could show one leg of a transaction without the other.

**The pressure valve this buys.** Under load, the system lengthens the commit interval — producing larger files and fewer versions, improving both metadata cost and scan performance — while the buffer continues to serve freshness. The system can slow its writes without becoming stale. This is the primary backpressure lever against the compaction death spiral.

**Coupling constraint that must appear in the capacity model:**

```
arrival_buffer_bytes ≈ write_rate × commit_interval × avg_change_size × safety_factor
```

The commit interval cannot grow without bound because it is coupled to buffer memory. These two parameters MUST be tuned together; if they are owned by different configuration sections they will drift, and the failure will occur under exactly the load that triggered the backpressure.

**Open decision — v1 scope.** `CON-11` permits a few seconds of lag, which admits a simpler variant: commit every 2–5 seconds with no arrival buffer, and implement read-your-own-writes as a bounded wait for the applier to reach the session LSN. This is materially less machinery, at the cost of losing the pressure valve. The deciding question — whether the buffer is cleanly retrofittable behind the read-path planner interface — is under review. **Owner: systems architect. Required before the storage milestone begins.** Regardless of outcome, `FR-CDC-12` (LSN coverage metadata on every tier) and the read-mode API shape MUST be built in v1, because they are nearly free now and expensive later.

---

### DEC-10 — Delta Lake first, Iceberg behind the same seam

**Owner directive (2026-08-25).** "See that we can use Apache Iceberg in place of raw Parquet or Delta."

**Decision.** Build the `TableFormat` seam on day one; ship **exactly one** writable implementation in v1.

**Rationale, in order of weight.**

1. **Write-path maturity.** `deltalake` 0.32.4 provides MERGE with disk spilling, change data feed, column mapping, log compaction and `OPTIMIZE` with Z-order. `iceberg` 0.10.1 is append-only, with `RowDelta`, `Overwrite`, `RewriteDataFiles` and snapshot-validation conflict detection all still on the roadmap. Estimated effort as a CDC sink: **2–4 engineer-weeks for Delta; 2–4 weeks plus 12–20+ weeks and ongoing for Iceberg** to build the mutation path ourselves — against a pre-1.0 API that breaks each release, in the worst possible place to write our own code, since commit-protocol bugs corrupt data silently and are discovered by an auditor rather than a test.
2. **`DEC-07` softens the comparison but does not invert it.** With an append-only landing zone we depend far less on either format's mutation semantics. But compaction still writes, and Delta's tooling for it is materially more mature.
3. **The version-skew problem is symmetric** and therefore does not discriminate: both crates pin Arrow 58 / DataFusion 53.1 identically. `DEC-06` neutralizes it for both.
4. **`delta_kernel` is already Arrow-59-capable with no DataFusion dependency**, which makes the `DEC-06` metadata-only read path implementable *today* on Delta. No equivalent exists for Iceberg.

**Iceberg's genuine advantages, recorded so the decision can be revisited honestly:**

- **Branches and tags**, which enable write-audit-publish: commit to a staging branch, validate and reconcile, then atomically fast-forward. Delta has no equivalent, and for a system producing auditable figures this is a real control.
- **Tags give durable point-in-time pinning** that survives snapshot expiry by construction. Under Delta the equivalent depends on a lease registry that external engines will ignore.
- **Hidden partitioning**, so a predicate on a raw timestamp prunes partitions without a derived column. Delta requires an explicit partition column; the gap is closable with an analyzer rule deriving the partition column from timestamp predicates, which we should build regardless.
- **Partition evolution** without a full rewrite.
- **A catalog**, which enables discovery and credential vending to external engines.

**Sequencing.** Delta as the sole writable format through the storage and query milestones. Iceberg added later as the interop and publication tier, behind the same seam, validated by a format conformance suite.

**Two design constraints on the seam, which are what keep it from becoming a lie:**

- **A `Capabilities` value is the load-bearing type.** Consumers branch on `caps.row_level_deletes`, `caps.named_refs`, `caps.partition_evolution` — never on a format enum.
- **`Unsupported` is a first-class error variant, not a panic.** The layer above must have a generic fallback: an upsert without row-level deletes becomes a read-modify-write of the affected partitions — correct, slower, and better than nothing.

**Cost.** Roughly 1–1.5 engineer-weeks up front for the seam. Two live implementations would have doubled the test matrix and, worse, silently pinned the feature set to the *intersection* of the two.

**[UNVERIFIED, and decision-relevant — resolve before the storage milestone.]** Whether `iceberg-rust` pushes down **decimal** predicates (if not, every numeric range predicate degrades to a full scan, and numeric columns are decimals); whether `iceberg-datafusion` surfaces Puffin NDV statistics; whether `delta-rs` can **write** liquid-clustered tables (this is the strongest single argument in Delta's favour and it evaporates if not). **Owner: VLDB reviewer.**

---

### DEC-11 — Warehouse layout mirrors operational naming

**Owner directive (2026-08-25).** "Each table in the OLAP layer will get its own folder so that it can be identified. Those folders can be inside another folder which carries the name of the OLTP schema." And: "In other words, make OLTP and OLAP relatable in terms of naming."

**Decision.** The published warehouse layout is:

```
<warehouse_root>/<oltp_schema>/<table>/
```

with each table self-contained in its own directory, named to match its source. One name spans all four naming domains — PostgreSQL identifier, object-store path, catalog namespace, and the name a SQL user types — so that anyone browsing the warehouse, or any external job, can identify a table's origin without a lookup table.

```
warehouse/
  sales/
    orders/
    customers/
  telemetry/
    device_readings/
```

**This settles an open design question.** Naming directories by an immutable identifier would be rename-safe but opaque. The directive is explicit that relatability wins.

**Consequences that must be specified (see `FR-STORE-10` through `FR-STORE-14`):** a human-legible escaping scheme for identifiers that are awkward as object-store keys, with **collision detection that refuses loudly at onboarding rather than silently merging**; case-folding rules for case-insensitive filesystems; and a rename policy. Rename is the hard case: physically moving data is expensive and breaks external readers' saved paths, while leaving it and recording an alias destroys the relatability just mandated. Rename is therefore classified with incompatible DDL and requires an explicit operator action.

**Separation requirement.** The warehouse root contains **only externally-meaningful published tables**. SANKHYA-private state — the arrival buffer, change-log staging, caches, spill files, the archival registry, catalog metadata, keys — lives outside it. See `FR-OPS-04`.

---

### DEC-12 — Analytical correctness rules that the original brief left implicit

Three requirements in the original brief are stated in a single clause each, and a competent engineer would implement all three incorrectly. Each is elevated to an explicit, tested requirement, and each is restated in domain-neutral terms per `DEC-04`.

**(a) Non-linear aggregates must aggregate the vector, then apply the function.** The brief says "dynamically pivot risk across desks, books, asset classes, and risk factors", which read naively implies summing a precomputed quantile up a hierarchy. That is wrong: for the general case of a non-linear function over a distribution, the function of the sum is not the sum of the functions. The engine MUST reject plans that apply `SUM` or `AVG` to a precomputed non-linear measure across a grouping key. **The general capability:** fixed-size numeric list columns whose element-wise sum is additive, so a `ROLLUP` computes every hierarchy level in a single pass and the non-linear function is applied independently at each level.

**(b) Approximate aggregates must never silently satisfy an exact request.** `approx_percentile_cont`, `approx_median` and `approx_distinct` exist in DataFusion 55 and are one autocomplete away from any query. They are sketch-based and, critically, **merge-order dependent** — so they are both approximate *and* non-deterministic, meaning the same query returns different values on different runs. The engine MUST reject them at planning time when the session is flagged as requiring exactness, and MUST watermark any result computed with them.

**(c) Parallel floating-point reduction is non-deterministic by default.** Reduction order varies with partition completion order, so the same aggregate query returns different values run to run. Any system whose outputs must be reproducible — for audit, for regulatory filing, for clinical or safety review, for billing dispute — requires deterministic reduction: a fixed partition count recorded with the result, partials merged in ascending partition index rather than completion order, and compensated summation. This is a general property of an analytical engine, not a finance requirement.

**(d) Missing data must not silently improve an aggregate.** A null vector contributing nothing to a sum makes the aggregate look better than reality. A completeness measure MUST be attachable to any aggregate, with a threshold below which the query fails rather than returning a flattering number.

---

### DEC-13 — Time-respecting traversal is a core primitive, not an option

**Problem.** The brief specifies "N-hop pathfinding" and "circular transaction rings". Plain breadth-first search over an aggregated edge set will happily report a path `A → B → C` when the `B → C` edge precedes the `A → B` edge in time. For any flow-of-value or flow-of-goods analysis, such a path is physically impossible. In practice most multi-hop paths found by static traversal are temporally invalid, so static search produces overwhelming false positives.

**Decision.** Edges carry validity intervals, and traversal supports a **time-respecting** mode in which successive edges must be non-decreasing in time, with optional maximum and minimum dwell constraints and an optional value-conservation constraint. Static traversal MUST NOT be exposed for flow analysis.

**Implementation consequence that ties three requirements together.** Storing edges in CSR sorted by `(source, timestamp)` makes "outgoing edges of `v` after time `t`" a binary search plus a contiguous slice. That same sort order is the one that gives the best data skipping for the corresponding table, and it eliminates the dominant cost of graph hydration. **One layout decision, three payoffs** — this is worth stating explicitly in the architecture document.

**Why it cannot be deferred.** Retrofitting temporal semantics into a traversal engine is a rewrite.

---

### DEC-14 — Single-node execution with query routing; distribution is a v2 decision

**Problem.** "Stateless horizontally scalable nodes" is compatible with two very different architectures. DataFusion is single-node; multi-node execution means either Ballista or `datafusion-distributed`, both separate projects.

**Decision.** v1 executes each query entirely on one node, with a router that consistent-hashes on tenant, primary table and snapshot for cache affinity, with load-based overflow. Scaling adds throughput, not per-query capacity. **The ceiling is explicit: the largest single query is bounded by one node's memory and cores.**

**v2 trigger and path.** Distribution is introduced only when a *measured* workload exceeds the ceiling. The preferred path is `datafusion-distributed`, which expresses distribution as exchange operators inside otherwise-normal plans, preserving the single-node code path — estimated 4–6 engineer-months. Building shuffle, exchange and a coordinator from scratch is 12–24 engineer-months and is not recommended.

**Trade-off, stated plainly.** Scale-up gives lower latency (no shuffle, no serialization), far simpler failure semantics, simpler memory accounting and simpler security. It costs the ability to run one query larger than one node. Scale-out inverts every one of those.

**Two seams to design now and build later**, both near-zero cost today and expensive retrofits: keep the applier's commit path per-table rather than globally serialized, and allow a table reference to resolve to a shard set.

---

### DEC-15 — Data tiering is explicit, gated and never automatic-by-default

**Owner directive (2026-08-25).** "Consider how migration of data from OLTP to OLAP will be done. When data, or rows in a table, or a whole table is migrated, the contents will move to OLAP and be purged from OLTP." And: "This migration can be done via command line or schedule."

**Decision.** SANKHYA supports lifecycle tiering in which data is migrated to the published tier and purged from the transactional source, invoked **by explicit operator command or by a configured schedule — never implicitly**. After purge, the published copy becomes the system of record for that data.

**The trap this creates, and its resolution.** The CDC path replicates deletes. An ordinary `DELETE` used to purge tiered data would faithfully propagate and **erase from the published tier exactly the data the purge was meant to preserve**. The archival purge MUST be distinguishable from a business delete *structurally*, not by convention. Partition `DETACH` is the leading candidate because it removes rows without emitting row-level delete events. See `FR-TIER-04`.

**Safety principle.** Data is never removed from the system of record until it is provably durable and correct elsewhere. The purge is gated on a verification sequence: confirm the applier has passed the relevant LSN; verify presence in the published tier by row count, key-set equality and per-column checksum; confirm the snapshot is committed, backed up, and consistent with retention and legal-hold policy; create an immutable marker for the archived range; only then purge; then record the result in an archival registry. Every step must be crash-safe and resumable.

**Query transparency.** After a purge, a query may span hot and archived ranges. SANKHYA unions them automatically using the archival registry to route by key range, guaranteeing no gaps and no double-counting. Without this the feature would be a footgun.

**Scope for v1.** Partition granularity only. Partition detach is atomic and cheap; row-level purge is neither.

**Sequencing.** Tiering ships **after** continuous reconciliation is proven in production use. Purging the system of record before you can demonstrate the copy is correct is indefensible.

---

### DEC-16 — Untrusted code runs in WebAssembly; Python runs out of process; native plugins are rejected

**Original position.** "Ability to inject a user defined aggregation written in Python or Rust." (§3.3)

**Problem.** In-process Python holds the GIL, serializing user-function evaluation across every partition and collapsing a 32-partition plan to single-thread throughput, while stalling upstream and downstream operators. A segmentation fault in any C extension terminates the process — including the CDC apply loop and every other tenant's queries. And a Python function cannot be cancelled, breaking the query-kill guarantee. Loading arbitrary Rust shared objects is worse: Rust has no stable ABI, so a plugin built against a different compiler version, feature set or Arrow version is undefined behaviour by construction.

**Decision — three tiers.**

1. **Compiled Rust (trusted).** First-party functions and pack code compiled into the binary. Full speed, full trust.
2. **WebAssembly (default for untrusted code).** `wasmtime` 48.0.1 with fuel metering *and* epoch interruption for deadline enforcement, a pooling allocator with bounded instance memory, memory protection keys, and no ambient capabilities. Epoch interruption is what keeps the query-kill guarantee true for user code.
3. **Python (opt-in, out of process).** A separate worker process, Arrow IPC over a Unix domain socket or shared memory, resource-limited and crash-isolated, with vectorized signatures only. **Never in process.** No Python aggregates in v1.

**Native dynamic plugins are explicitly rejected**, and the rejection is recorded so it is not relitigated annually. `abi_stable` — the crate that would be required — was last released 2023-10-12 and is effectively unmaintained. A host/plugin version mismatch would be undefined behaviour rather than an error, a plugin panic would kill the process with no isolation, the entire Arrow type surface would have to be projected across the ABI boundary, and every plugin would need a per-compiler-version build matrix. The only benefit over WebAssembly is a modest constant factor.

**Every user-supplied function is a versioned, auditable artifact** with a content hash, an author, an approval record and a declared determinism flag. A non-deterministic function is excluded from result caching and from materialized-view definitions and is never constant-folded — the optimizer MUST honour this or produce wrong answers. Function versions appear in query provenance.

---

### DEC-17 — The extension API must not re-export fast-moving upstream types

**Problem.** If the pack extension API re-exports DataFusion's function traits directly, then **every DataFusion major release breaks every pack in existence** — two to four times per year.

**Decision.** The extension crate defines **SANKHYA's own** scalar, aggregate and window function traits, and re-exports only a curated, pinned Arrow subset. The query engine adapts them internally.

This is the `DEC-06` insight applied a second time: **never let a fast-moving upstream type leak into a slow-moving contract.** It is the single most important design constraint on the extension API.

**Supporting mechanisms**, because an extension API rots by accretion rather than by breaking:

- A hard size budget on the extension crate. Exceeding it requires a written decision. A size cap on an API crate is crude and is the only mechanism that reliably survives to year three.
- **The two-domain rule:** nothing enters the API until two packs *from different domains* need it. One pack's need is a pack-local helper.
- **No escape hatches.** Downcasting to `Any`, free-form JSON values and `HashMap<String, String>` extension fields are banned — these are how extension APIs rot without ever changing shape.
- **Packs may depend on exactly three core crates.** This is a feedback mechanism, not hygiene: when a pack legitimately needs a fourth, the build fails, and that failure *is* the signal that the extension API has a gap. Every such failure is an API design ticket, never a reason to widen the allowlist.
- Every public item carries a compiling documentation example, so bloat has a visible recurring cost.
- Anything the reference packs do not exercise is deleted at the next major version.

---

### DEC-18 — Domain-neutral public benchmarks are the primary performance gates

**Decision.** TPC-H, TPC-DS, ClickBench and LDBC SNB are the **primary, gating** performance suites. Domain benchmarks are supplementary and live inside their packs.

**Rationale.** Public suites exercise query shapes a domain suite never will — and they are the only numbers a prospective user can compare against DuckDB, ClickHouse, Trino or Neo4j. An uncomparable benchmark is marketing, not engineering. Equally, a general-purpose engine benchmarked only on one domain will be dismissed on exactly that basis.

**Consequence for how NFRs are written.** Every NFR names a query from a public suite wherever one exists. "Sub-second group-by over tens of millions of rows" becomes "ClickBench Q7/Q8, p95 under one second on the reference node". "Sub-50 ms graph traversal under 100,000 nodes" becomes "LDBC SNB Interactive IS-1…IS-7, p95 under 50 ms at SF10". These are verifiable, comparable, and they survive the domain-pack restructuring because they never referenced a domain.

**Verified tooling.** `tpchgen` / `tpchgen-arrow` 3.0.0 is pure Rust with zero dependencies and emits Arrow directly, so TPC-H data can be generated in-process during CI with nothing committed and no external tooling — which means TPC-H SF1 is affordable in the *pull-request* pipeline, not only nightly. **No Rust generator exists for TPC-DS or ClickBench**; both require pre-generated cached artifacts. **No Rust reference implementation of LDBC SNB exists**, so the query set must be implemented against our own API — budget 2–3 engineer-weeks and treat the harness as a deliverable.

**Note on publication.** TPC benchmark results carry publication rules distinguishing audited results from derived ones. Internal use is unrestricted; external claims must be labelled correctly.

---

### DEC-19 — The file-length constraint, in enforceable form

**Original position.** "Ideally no code should exceed 1500 lines – minus comments." (`CON-06`)

**Decision.** The intent is endorsed and made mechanical, with an explicit guard against the failure mode it invites.

- Hard failure above 1,500 code lines; **warning above 800**, which is what prevents a file arriving at 1,499 lines overnight. Counting uses a real code-line counter, not a comment-prefix heuristic, because string literals containing comment markers will otherwise corrupt the count.
- Generated code, test fixtures and snapshot files are excluded by path.
- Exceptions carry **an owner and an expiry date**; expired exceptions fail the build. Exceptions that cannot rot become permanent, and permanent exceptions become the norm.
- **A companion crate-size limit**, because forty 300-line files in one crate satisfies the letter of the rule and measures nothing.

**The failure mode, stated in the requirement itself.** Line-count rules reliably produce artificial fragmentation: a single state machine split across three files, visibility widened so the halves can see each other's internals, and invariants separated from their enforcement. That is a net loss in correctness traded for a green metric, and in a system whose value proposition is correctness it is a bad trade. Therefore:

> **A split that requires widening visibility, or that separates an invariant from the code enforcing it, is a violation of this rule, not compliance with it, and MUST be rejected in review.**

**Natural compliance.** Three habits satisfy the limit without contorting production code: move test modules into sibling files (tests are typically 40–60% of a well-tested file); express large mapping tables as data rather than as code; and keep generated code in its own excluded directory.

---

### DEC-20 — "Extensive test coverage" and "zero data loss" become measured quantities

**Problem.** Both phrases appear in the original brief as assertions. Neither is falsifiable as written.

**Decision — coverage.** Differentiated targets by layer, with a ratchet rather than a cliff, plus per-change diff coverage. **And mutation testing on the crates where correctness matters most**, because a test that executes a line without asserting anything about it scores perfectly on coverage and zero on mutation. Mutation score is the honest metric for "extensive test coverage".

**Decision — zero data loss.** Replaced by a definition and a continuously measured metric:

> For every committed source transaction with commit LSN `L`, once the pipeline reports `applied_lsn ≥ L`, a read of the published table at the corresponding version returns exactly the rows a read of the source at snapshot `L` would return — no missing rows, no duplicates, no stale values.

Verified by a reconciliation harness comparing against an **independent expected-state model** maintained by the test itself, not by a second query against the source — otherwise a bug in the source reader hides a bug in the pipeline. The same comparison ships as an operator command and as a continuous background job exporting a mismatch metric, alerting above zero.

**One correctness trap in the harness, worth recording because it is easy to get wrong and silent when wrong:** the per-row checksum must be combined with an **order-independent but duplicate-sensitive** operator — wrapping addition, not XOR. XOR cancels duplicate pairs and would therefore hide precisely the at-least-once duplication bug the harness exists to catch.

---

### DEC-21 — Personal data is designed out of the analytical tier

**Problem.** Immutable versioned history and a legal right to erasure are directly contradictory. Worse, different domains impose *contradictory* obligations: some records must be retained for years and may not be erased, while others must be erased on request — frequently in different columns of the same table.

**Decision.** Three mechanisms, in priority order.

1. **A personal-data vault, as the primary design.** Direct identifiers live only in the transactional store, which supports real deletion. The analytical tier carries surrogate keys. Erasure becomes a transactional delete plus a vault purge, leaving analytical history, time travel and retention entirely untouched. **This resolves the conflict outright for the majority of cases and it is a design decision that cannot be retrofitted affordably.**
2. **Cryptographic erasure as backstop.** Where an identifier must exist in the analytical tier, encrypt it under a per-subject key and destroy the key. Preserves history and retention. Reclaims no space, and is defensible but not universally accepted as erasure — it requires sign-off rather than an engineering assertion.
3. **Erasure compaction as last resort**, as a distinct job class with distinct authorization, which checks retention obligations and legal holds first and refuses if any apply, and which emits an audit record stating that history before a given date is no longer reproducible.

**Architectural expression of the decision:** the ordinary maintenance scheduler MUST be *structurally incapable* of destroying retained history. Erasure is not a priority level of expiry; it is a different job class with a different authorization path and an approval gate. Anything less and a misconfigured retention default eventually deletes records that were legally required to persist.

**Consequently, a configurable retention-and-erasure policy engine is core v1 capability**, not a compliance footnote. Packs declare retention classes; the core enforces them.

---

### DEC-22 — Two named safety invariants

Two invariants are elevated above ordinary requirements because they protect the transactional system that everything else depends on. Both are stated as architectural invariants and both are gated by chaos tests, not by review.

> **INV-1 (Query safety).** No query — at any concurrency, resource level, plan shape or tenant — can cause the transactional primary to lose availability or durability.

> **INV-2 (Source safety).** SANKHYA can never bloat, wedge or exhaust the storage of the PostgreSQL instance it replicates from — including through its own replication slot, its own long-running queries, or its own maintenance.

**Why these need naming.** A logical replication slot retains write-ahead log from its restart position forward. If SANKHYA's consumer stalls — starved of CPU by a runaway analytical query, for instance — PostgreSQL retains WAL indefinitely until the filesystem fills, at which point it shuts down. **The failure mode is that an analytical query takes down the transactional system**, which in any production deployment is the worst possible outcome and is not mentioned anywhere in the original brief.

A logical slot additionally pins the catalog transaction horizon, bloating system catalogs and slowing planning for every query on the instance. And SANKHYA introduces a third vector *by design*: its own long-running transactional reads and its initial snapshot export hold open transactions that block reclamation of dead tuples.

**Mitigations are specified in `FR-CDC-20` through `FR-CDC-26` and `FR-OPS-20` through `FR-OPS-26`:** a dedicated CDC runtime with reserved cores, separate connection pools so analytical load cannot exhaust connection slots, bounded statement and transaction lifetimes on every SANKHYA-originated session, a five-level escalation ladder, and threshold ordering such that **SANKHYA degrades on its own terms before PostgreSQL invalidates the slot unilaterally** — because an invalidated slot cannot be resumed and forces a full re-snapshot of every replicated table.

---

### DEC-23 — Archival purge uses partition detach, never row deletion

**Problem.** `DEC-15` establishes tiering with source purge. The CDC path replicates deletes. An archival purge implemented as `DELETE` would propagate downstream and erase from the published tier exactly the data the purge existed to preserve. This is the single most likely way to lose retained history irrecoverably, and it would happen quietly.

**Candidate mechanisms evaluated.**

| Mechanism | Verdict |
|---|---|
| **Partition `DETACH` + `DROP`** | **Correct.** `DETACH` is a catalog-only operation emitting no row-level events. With root-level publication, the detached leaf leaves the published set, so the subsequent `DROP` emits nothing either. The purge is invisible to logical decoding **by construction, not by convention** |
| Publication excluding delete operations | **Valuable as a second layer**, not as the primitive |
| Transactional marker messages bracketing the purge | **Useful as attestation, dangerous as a safety mechanism.** A scheme in which deletes are emitted and the applier is expected to suppress them fails if a marker is lost, reordered, or the applier restarts mid-bracket. Never make a safety property depend on a message arriving |
| `session_replication_role = 'replica'` | **Does not work, and must be recorded as a trap.** It disables triggers and rules and has **no effect on logical decoding**, which reads the write-ahead log directly. The deletes would still be decoded and propagated. This is listed explicitly so that nobody re-proposes it |
| A purge path the replication slot does not observe | **No such path exists for row-level DML.** Every row change on a logged, published table is written to the WAL and decoded. `TRUNCATE` is decoded too. Only catalog-level operations are invisible — which is precisely why detach is the answer |

**Decision — four layers, only the first load-bearing:**

1. **Primitive.** Purge is always partition detach followed by drop. Row-level deletion is never used for archival in any code path.
2. **Publication guard.** Tiering-eligible tables are published with delete and truncate excluded from the publication entirely, so even a defective code path cannot propagate a deletion.
3. **Applier tripwire.** The applier holds the archival extent map and refuses to apply any delete or truncate whose key falls in an archived range, treating it as a fatal alarm rather than a warning.
4. **Attestation.** A transactional marker committed alongside the registry change, giving the applier a positive, correctly-ordered record that a purge occurred — for provenance only, never for safety.

**Consequent eligibility constraint.** A table is tiering-eligible only if it is **append-only by contract** — an immutable record table where corrections are new rows rather than mutations. In practice this is exactly true of the tables anyone wants to tier and exactly false of the tables nobody should tier.

**A convergence worth noting.** Tiering requires range partitioning on the tiering key. Bloat avoidance independently requires range partitioning on high-volume time-shaped tables. **The same schema decision serves both**, both are made at design time, and both are expensive to retrofit. They are presented in this document as one requirement with two justifications.

---

### DEC-24 — After purge, the published tier is the system of record

**The framing shift.** Everywhere else in this architecture the published tier is *derived*: if it is wrong, it can be rebuilt from PostgreSQL. That safety net is what makes CDC defects survivable. Tiering removes it. Once a partition is purged, the published copy is the only copy, and any defect in it — a lost row, a truncated decimal, a shifted timezone — is permanent and undetectable after the fact.

> **The tiering prime directive.** Data may not be removed from the system of record until its replacement is proven durable, complete, byte-faithful, immutable and covered by the applicable retention obligation. The proof is machine-checked, recorded, and **there is no flag to skip it**.

**Consequent requirements** (specified in `FR-TIER-*`):

- A durable, resumable state machine with a registry row per archive unit, where every transition is committed before the corresponding real-world action.
- **Exhaustive** verification before purge — row count, primary-key set equality via a Merkle digest over sorted blocks, and per-column checksums over a **canonical byte encoding**. Count equality alone is not evidence. Routine reconciliation may sample; purge verification may not.
- A **canonical encoding per logical type**, with a lossless-or-reject rule. Types that cannot round-trip faithfully make a table ineligible. Unconstrained arbitrary-precision numerics, locale-dependent money types, infinite timestamps and non-normalizing JSON are reject-by-default.
- A **mandatory quarantine grace period** (default seven days) during which the detached partition is retained on disk and re-attachment is trivial. It costs a week of disk and buys reversible recovery from a defect discovered late. Against permanent loss of a retained record, this is the cheapest insurance in the system.
- Verification failure is **terminal until a human acts**. There is no automatic retry of a failed purge, because failure means either the pipeline or the tiering logic has a defect and retrying is exactly the wrong response.

**Delivery gate.** Tiering may not ship until continuous reconciliation has run clean in production across every table class for a sustained period, the restore drill has passed repeatedly, and an archive attestation drill has passed. The gate is explicit so that schedule pressure cannot quietly make this decision.

---

### DEC-25 — Cross-tier queries are unified automatically, with a total tie-break rule

**Decision.** After a purge, a query whose predicate spans hot and archived ranges is served by automatically unioning the tiers. Anything else is a footgun: a user who queries six years of history and silently receives two has been handed a wrong answer by a system that knew better.

**The mechanism, and its pleasing symmetry with `DEC-09`.** The read-path planner already splices tiers along the **LSN axis** for freshness. Tiering adds a second, orthogonal **key-range axis** for archival:

| Axis | Coverage rule | Authority |
|---|---|---|
| LSN (freshness) | Intervals contiguous, non-overlapping, covering `[0, target_lsn]` | Commit metadata and buffer epochs |
| Key range (archival) | Hot and cold extents disjoint, together covering the declared key domain | PostgreSQL catalog (hot), archival registry (cold) |

**The authority rule, which eliminates an entire class of drift bugs:** *the PostgreSQL catalog is authoritative for whether data is still hot; the archival registry is authoritative for provenance and for the cold side.* Both are read inside the same PostgreSQL snapshot used for the hot scan — the registry lives in PostgreSQL, so this costs nothing — making hot extent and hot scan consistent by MVCC with no distributed agreement.

**The tie-break rule is total**, producing either a correct answer or an explicit error in every direction of disagreement:

- Range covered by neither tier, and the predicate intersects it → **fail** with a coverage-gap error. Never return a silently short answer.
- Registry says cold but the catalog says attached (a restored backup resurrecting purged rows) → **hot wins**; the range is read exactly once, so there is no double-counting even in the failure case. The underlying inconsistency is separately flagged.
- Catalog detached and registry agrees → read cold. No gap.

**A subtlety worth recording, because a reviewer will otherwise assume the opposite.** Serving the cold portion of a strongly-consistent read from the published tier does not weaken the consistency guarantee: archived data is immutable by policy, so nothing can change it, so a snapshot read of it is equivalent to a linearizable one. There is no consistency being traded — only a change of storage.

**Operator-visible consequence.** A query with no predicate on the tiering key scans both tiers in full. The planner must flag this and may reject it under a per-tenant quota. This is the tiering equivalent of a missing partition filter, and it should surface as a warning long before it surfaces as a forty-minute query.

---

## 5. Functional requirements

Requirements are grouped by subsystem. Each carries a priority: **M** (mandatory for v1), **S** (should have, v1 if capacity permits), **L** (later release).

### 5.1 Transactional tier — `FR-OLTP`

| ID | Pri | Requirement |
|---|---|---|
| `FR-OLTP-01` | M | The system SHALL provide a transactional store with full ACID semantics, foreign-key integrity, row-level locking and transactional DDL, backed by PostgreSQL 17 or later |
| `FR-OLTP-02` | M | All mutations **to a managed table** SHALL be applied to the transactional store first; for such a table it is the sole authoritative writer of record, and no other component may write to its published tier except the change applier and maintenance jobs. *(Amended 2026-08-27 — see `FR-OLTP-02a`. The original wording made every table managed, which would require a terabyte backfill to pass row by row through a transactional store to produce Parquet the loader could have written directly, and would foreclose any deployment whose data already lands in an object store. The amendment scopes the guarantee to the tables that want it rather than removing it.)* |
| `FR-OLTP-02a` | M | The system SHALL support **external tables**: published directly to the open table format by a writer outside this system and discovered by walking the warehouse. An external table SHALL be read-only to this system, and a write to one SHALL be refused rather than accepted into a tier that is not authoritative for it |
| `FR-OLTP-02b` | M | A table's class SHALL be declared in **its own log**, not in this system's configuration, so that two nodes reading one warehouse cannot disagree about it and a restart cannot forget it. **Absence of the declaration SHALL mean external.** A directory into which files were dropped is not managed by this system, and defaulting the other way would have a table claim a transactional tier it does not have |
| `FR-OLTP-02c` | M | A request for a strongly-consistent read of an external table SHALL be **refused by a named error** stating that the table has no transactional tier and which read modes it does support. It SHALL NOT be served from published data, which would assert a currency the table cannot offer with nothing in the result to say so |
| `FR-OLTP-02e` | M | The system SHALL provide a **publishing library and command-line tool** for writing external tables, and this SHALL be the supported path for external publication. `CON-08`'s openness requirement is about reading: a reader that misunderstands the format is wrong for itself and recoverably, while a writer that misunderstands it corrupts the table for everyone, permanently, and undetectably — as happened to this system writing its own format with the specification open |
| `FR-OLTP-02f` | M | The system SHALL provide **verification** of a table's log against the invariants the publishing library maintains, reporting *what* is wrong rather than only *whether* it is: which action is missing which field, which files carry no statistics, whether the schema round-trips. Verification SHALL be separate from reading, because a table that fails it may still be readable, and coupling them would make a table that is largely correct unreadable |
| `FR-OLTP-02g` | S | The system SHOULD provide **repair** for a table whose log is deficient, restricted to repairs that can be **derived** from evidence that already exists. It SHALL refuse anything requiring a guess, stating why it cannot be derived and what a person must decide. It SHALL NOT delete a file, an action or a version; SHALL append a new version rather than rewrite a committed one, so the broken state stays readable and the repair is revertible; SHALL change nothing unless explicitly asked; and SHALL re-verify its own result rather than assume it |
| `FR-OLTP-02d` | S | A managed table SHOULD be bulk-loadable by writing files directly and registering them with the coordinator, supplying the log positions they cover, so that the table keeps every managed guarantee without the load passing row by row through the transactional store |
| `FR-OLTP-03` | M | The system SHALL support **managed mode**: PostgreSQL binaries carried inside the SANKHYA executable, extracted on first boot, and run as a supervised child process listening only on a Unix domain socket within the data directory |
| `FR-OLTP-04` | M | The system SHALL support **attached mode**: connecting to an externally managed PostgreSQL cluster, with a documented minimum privilege set and graceful degradation when an optional privilege is absent |
| `FR-OLTP-05` | M | In managed mode the system SHALL own the complete PostgreSQL lifecycle: initialization, start, readiness detection, health monitoring, configuration, backup, restore, upgrade and shutdown |
| `FR-OLTP-06` | M | The system SHALL acquire an exclusive lock on its data directory at startup. Two SANKHYA processes MUST NOT be able to open the same data directory |
| `FR-OLTP-07` | M | At startup the system SHALL detect and correctly handle an orphaned postmaster: adopt it if live and healthy, stop and restart it if live and unhealthy, and clear the stale record if not running. Failure here yields either data-directory corruption or a boot loop |
| `FR-OLTP-08` | M | The system SHALL maintain **separate, individually capped connection pools** for transactional writes, the replication connection, analytical reads and maintenance, so that analytical load cannot exhaust connection slots and lock out the transactional writer. The replication pool slot is reserved and never shared |
| `FR-OLTP-09` | M | Every SANKHYA-originated database session SHALL set a statement timeout, a lock timeout and an idle-in-transaction timeout at checkout, not in server configuration where they can drift |
| `FR-OLTP-10` | M | Release builds SHALL embed the PostgreSQL archive at compile time. Runtime download of database binaries is disqualifying for air-gapped deployment and is a supply-chain risk. The archive checksum is verified at build and at extraction |
| `FR-OLTP-11` | S | The system SHALL provide an online table-rewrite capability for bloat remediation. `VACUUM FULL` SHALL NOT be issued automatically under any circumstances, as it takes an exclusive lock for the duration of a full rewrite |
| `FR-OLTP-12` | M | High-volume, time-shaped tables owned by SANKHYA SHALL use native range partitioning by time, so that retention is a metadata operation rather than a bulk delete |
| `FR-STORE-20` | M | Every analytical table SHALL carry **`sank_data_date`** of type `DATE`, and SHALL be partitioned on it. See [ADR-0004](adr/0004-the-date-axis.md) for why `DATE` rather than an encoded integer: partition paths are the Hive convention external engines parse natively, date arithmetic works where `20240301 - 7` does not, and no timezone is implied |
| `FR-STORE-21` | M | The value of `sank_data_date` SHALL be **declared per table, never defaulted per row**. A table naming a source column SHALL use it for every row, and a null there SHALL be an error rather than a substitution of the current date. A table naming none SHALL use the ingest date **and record that it does**. A per-row default makes the column mean "when this happened" in some rows and "when we received it" in others, in one table, inseparably |
| `FR-STORE-22` | M | Partition granularity SHALL be declarable as day, month or year, defaulting to day. An unrecognised granularity SHALL be refused rather than defaulted, because a monthly table silently becoming daily is repartitioned on its next write — a full rewrite, for a typo |
| `FR-STORE-23` | M | `sank_` SHALL be a reserved column-name prefix. A source column so named SHALL be refused at onboarding rather than shadowed, because a shadowed column means the source's data disappears behind a system value with no error anywhere |
| `FR-STORE-24` | M | Managed tables created by this system SHALL carry the column natively. **Attached tables SHALL NOT be altered to add it** — the column is derived during ingest and exists on the analytical side, which is where partitioning happens. `ALTER TABLE` on a cluster somebody else manages breaks inserts without column lists, changes `SELECT *`, and may exceed the granted privilege set |

### 5.2 Change data capture — `FR-CDC`

| ID | Pri | Requirement |
|---|---|---|
| `FR-CDC-01` | M | Change capture SHALL be implemented natively in-process using PostgreSQL logical replication with the built-in `pgoutput` plugin. No message broker, connector framework or JVM component may be required. The source SHALL sit behind a trait permitting alternative implementations |
| `FR-CDC-02` | M | The replication protocol decoder SHALL be a pure function of its input bytes, in a crate with no I/O, and SHALL be continuously fuzz-tested. It parses untrusted bytes from a network socket and is therefore a primary attack surface |
| `FR-CDC-03` | M | The decoder SHALL correctly handle all protocol message types including transaction boundaries, relation metadata, insert, update, delete, truncate, origin, type, streamed in-progress transactions and two-phase commit messages |
| `FR-CDC-04` | M | The system SHALL correctly handle **unchanged TOASTed values**, which are absent from the change record. Writing nulls over real data in this case is silent corruption and MUST NOT occur |
| `FR-CDC-05` | M | The system SHALL implement a per-table replica-identity policy, detect tables lacking a usable identity, and either remediate automatically with a documented write-amplification cost or onboard the table append-only with a clear diagnostic. A table with no primary key and default replica identity causes PostgreSQL itself to reject updates and deletes |
| `FR-CDC-06` | M | Initial backfill SHALL use an exported snapshot with a gapless, duplicate-free handoff to streaming at a known log position |
| `FR-CDC-07` | M | The backfill SHALL be chunked rather than executed as a single long transaction, because a long-running transaction pins the cleanup horizon and blocks reclamation system-wide |
| `FR-CDC-08` | M | Delivery SHALL be at-least-once and application SHALL be idempotent, yielding effectively-exactly-once semantics. The applied log position SHALL be recorded **inside the table commit metadata**, so that recovery reads it from the table's own history with no external state |
| `FR-CDC-09` | M | The replication slot position SHALL be advanced **only after** the corresponding table commit is durable. Reversing this ordering is silent data loss and is the most common defect in hand-built capture pipelines |
| `FR-CDC-10` | M | A source transaction SHALL be applied atomically. A commit batch may span several source transactions but MUST NOT split one across two commits |
| `FR-CDC-11` | M | The applier SHALL accumulate changes and commit on a size, count or time trigger. The batch interval is a first-class configuration parameter governing file size, version count and metadata cost simultaneously |
| `FR-CDC-12` | M | Every tier SHALL publish its log-position coverage interval as metadata. A tier without a declared coverage interval cannot be spliced safely and MUST NOT be readable |
| `FR-CDC-13` | S | The batch interval SHOULD adapt to load — lengthening under high write rate to produce larger files and fewer commits, shortening when idle. Bounds and the control loop SHALL be specified and observable |
| `FR-CDC-14` | M | A single commit batch SHALL NOT produce unbounded file fan-out. A batch touching many partitions MUST NOT write one tiny file per partition without a guard |
| `FR-CDC-15` | M | Schema changes SHALL be detected and handled. Logical replication does not stream DDL, so the system SHALL use an event trigger writing to a change-log table which is itself replicated, delivering DDL events in stream order |
| `FR-CDC-16` | M | Additive and compatible schema changes SHALL be applied automatically. Incompatible or destructive changes SHALL place the affected table in **quarantine** with a named error and an explicit operator verb to resolve. Intent cannot be inferred: a dropped column may mean "stop capturing" or "erase from history", and guessing wrong is either data loss or a compliance breach |
| `FR-CDC-17` | M | Quarantined-table events SHALL be routed to a **durable dead-letter store** and the replication cursor advanced. With a single slot there is one cursor; if a quarantined table stalls it, retained log volume grows without bound and fills the source database's disk |
| `FR-CDC-18` | M | The system SHALL support automatic table onboarding: a table created in the source becomes analytically queryable with no configuration step, no manual registration and no operator action |
| `FR-CDC-19` | M | The system SHALL derive analytical schemas automatically from source schemas, with a documented type mapping and an explicit, enumerated list of supported types. Unsupported types SHALL be rejected loudly at onboarding, never mapped approximately |
| `FR-CDC-20` | M | The applier SHALL run on a **dedicated runtime with reserved CPU**, isolated from query execution. Reservation, not prioritization: priority schemes fail under sustained saturation |
| `FR-CDC-21` | M | The system SHALL monitor replication lag in **both seconds and retained log bytes**, and SHALL raise a paging alert on the byte measure |
| `FR-CDC-22` | M | The system SHALL implement a documented escalation ladder culminating in sacrificing the analytical tier to protect the source database, with thresholds ordered so that SANKHYA degrades on its own terms **before** the database invalidates the slot unilaterally. An invalidated slot cannot be resumed and forces a full re-snapshot |
| `FR-CDC-23` | M | When the analytical tier is sacrificed, the system SHALL record a durable, auditable **gap marker** identifying the missing log range, mark affected tables as requiring re-snapshot, begin re-snapshot automatically, and report the gap in query provenance until it is closed. A silent hole in a dataset is worse than an outage |
| `FR-CDC-24` | M | The applier SHALL never drop change events. Under pressure it SHALL escalate through larger batching, spill to local disk, and halting slot advancement with an alert |
| `FR-CDC-25` | M | Replication slots SHALL be named with an installation identifier. At startup the system SHALL report slots matching its naming scheme belonging to other installations, and in managed mode SHALL reclaim them after a grace period |
| `FR-CDC-26` | M | The system SHALL publish a measured re-snapshot recovery-time estimate per deployment, established at commissioning rather than estimated |

### 5.3 Analytical storage — `FR-STORE`

| ID | Pri | Requirement |
|---|---|---|
| `FR-STORE-01` | M | Analytical data SHALL be stored in an open lakehouse table format over Parquet, on local filesystem or object storage, behind a `TableFormat` abstraction |
| `FR-STORE-02` | M | Delta Lake SHALL be the sole writable format in v1. The abstraction SHALL exist from the first commit; a second implementation SHALL NOT be built until the first is complete and conformance-tested |
| `FR-STORE-03` | M | The abstraction SHALL expose a `Capabilities` value. Consumers SHALL branch on declared capabilities, never on a format identity. `Unsupported` SHALL be a first-class error, never a panic, and callers SHALL have a correct generic fallback |
| `FR-STORE-04` | M | The CDC landing zone SHALL be append-only. The applier SHALL NOT emit deletion vectors, position deletes or equality deletes |
| `FR-STORE-05` | M | Current state SHALL be produced by SANKHYA's own merge-on-read, resolving latest-version-per-key partition-locally |
| `FR-STORE-06` | M | Compaction SHALL materialize the change log into a **published table** by bulk partition rewrite. The published table SHALL be self-consistent and correct for any compliant external reader with no SANKHYA process involved |
| `FR-STORE-07` | M | Equality deletes SHALL NOT appear in any query-visible snapshot, under any format |
| `FR-STORE-08` | M | The published warehouse layout SHALL be `<warehouse_root>/<schema>/<table>/`, one self-contained directory per table |
| `FR-STORE-09` | M | Table naming SHALL be consistent across the source identifier, the object-store path, the catalog namespace and the SQL-visible name, so that a table's origin is identifiable without a lookup |
| `FR-STORE-10` | M | Identifier-to-path mapping SHALL use a human-legible escaping scheme, not hashing. Two distinct source identifiers mapping to the same path SHALL be **detected and refused at onboarding**, never silently merged |
| `FR-STORE-11` | M | Case-folding behaviour SHALL be defined and SHALL be safe on case-insensitive filesystems |
| `FR-STORE-12` | M | A table rename SHALL be treated as an incompatible schema change requiring explicit operator action, because name-based paths make silent rename either expensive or destructive to the naming guarantee |
| `FR-STORE-13` | M | The warehouse root SHALL contain only externally-meaningful published tables. Internal state — arrival buffer, change-log staging, caches, spill, archival registry, catalog metadata, key material — SHALL reside outside it |
| `FR-STORE-14` | M | Published tables SHALL be directly readable by external engines including Spark, Trino and DuckDB. The system SHALL declare which format protocol versions and table features it writes, and SHALL NOT enable features that would exclude common readers without explicit configuration |
| `FR-STORE-15` | M | External readability SHALL be verified by an automated test that writes with SANKHYA, reads with an independent engine, and asserts identical results |
| `FR-STORE-16` | M | Archived data SHALL use a conservative, widely-supported Parquet profile with no exotic encodings, and the registry SHALL record the exact format version. A seven-year retention obligation means the files must be readable in seven years by something other than SANKHYA |
| `FR-STORE-17` | M | The system SHALL specify and implement target file size, row-group size, page size, page-index settings, per-type encoding selection, compression codec and level, statistics collection and bloom-filter policy — each with a documented write-time cost and read-time benefit |
| `FR-STORE-18` | M | Compaction SHALL be automatic, with triggers on file count per partition, average file size, bytes written since last compaction, and elapsed time |
| `FR-STORE-19` | M | Compaction cadence SHALL be tiered by partition temperature, and the cadence SHALL be treated as a **read-path service-level input**, not a background convenience |
| `FR-STORE-20` | M | Bin-packing compaction and re-clustering SHALL be distinct job classes with distinct budgets. Re-clustering SHALL run only on settled partitions and only where the query log shows the cluster keys are used as predicates |
| `FR-STORE-21` | M | **Compaction SHALL only add files; a separate, later job SHALL remove them.** Physical deletion SHALL apply only to files unreferenced by any retained snapshot, not covered by a live lease, and older than the maximum of the retention horizon, the longest permitted query duration and the lease time-to-live |
| `FR-STORE-22` | M | Orphan-file cleanup SHALL use an age threshold exceeding the maximum possible commit duration including retries. Getting this wrong deletes live data, and the failure is silent until the affected partition is queried |
| `FR-STORE-23` | M | Compaction losing a commit race to the applier SHALL rebase and retry rather than abort, since it is a logical no-op. The abstraction SHALL return a **typed** conflict distinguishing retryable from fatal |
| `FR-STORE-24` | M | In any conflict between maintenance and the applier, **maintenance backs off; the applier never does** |
| `FR-STORE-25` | M | Where the format supports named references, the system SHALL use write-audit-publish for outputs requiring assurance, and SHALL create a durable tag per official close with retention tied to the regulatory horizon |
| `FR-STORE-26` | M | If the storage backend enforces object-lock retention or legal hold, physical deletion will fail. The system SHALL detect this, switch to logical expiry, and report zero bytes reclaimed with an explanatory status rather than retrying indefinitely |
| `FR-STORE-27` | M | The system SHALL ship a storage-conformance probe verifying conditional put, conditional update, read-after-write and list-after-write consistency, and SHALL refuse to enter multi-writer mode against a non-conforming store |

### 5.4 Query engine — `FR-QUERY`

| ID | Pri | Requirement |
|---|---|---|
| `FR-QUERY-01` | M | The system SHALL provide vectorized analytical SQL execution over Arrow, using DataFusion |
| `FR-QUERY-02` | M | SANKHYA SHALL implement its own table provider. Format crates SHALL be used only for metadata. No bulk Arrow data may cross a crate-version boundary |
| `FR-QUERY-03` | M | The provider SHALL implement filter pushdown correctly, returning an inexact classification unless row-exact filtering is guaranteed. Claiming exactness when only files are pruned is a silent wrong-answer defect |
| `FR-QUERY-04` | M | The provider SHALL supply statistics for every tier it exposes, including the in-memory arrival buffer, whose exact row count and per-column bounds are free to compute |
| `FR-QUERY-05` | M | The system SHALL maintain its own statistics catalog including distinct-value estimates, histograms and cross-column correlation, injected into the optimizer. Neither vendor provider supplies distinct-value estimates, which is the root cause of poor join ordering |
| `FR-QUERY-06` | M | Tenant and entitlement predicates SHALL be injected by an analyzer rule **and** independently asserted by the table provider, which SHALL fail if the predicate is absent. Defence in depth is mandatory on this path |
| `FR-QUERY-07` | M | The system SHALL support exact order statistics with a **named, documented interpolation convention**. There are several standard definitions and they disagree at the tail, which is where they matter |
| `FR-QUERY-08` | M | Exact order statistics over large inputs SHALL use a bounded-memory algorithm, not full buffering |
| `FR-QUERY-09` | M | Sketch-based approximate aggregates SHALL be **rejected at planning time** when the session requires exactness, with an error naming the function and its exact replacement. Results computed with them SHALL be watermarked as approximate |
| `FR-QUERY-10` | M | Floating-point reduction SHALL be deterministic: fixed partition count recorded with the result, partials merged in ascending partition index rather than completion order, and compensated summation |
| `FR-QUERY-11` | M | Fixed-precision decimal SHALL be used for all values requiring exact arithmetic. Floating point SHALL NOT be used for such values, including as an intermediate |
| `FR-QUERY-12` | M | The system SHALL support fixed-size numeric list columns with element-wise aggregation, and SHALL reject plans applying a linear aggregate to a precomputed non-linear measure across a grouping key |
| `FR-QUERY-13` | M | A data-completeness measure SHALL be attachable to any aggregate, with a threshold below which the query fails rather than returning a flattering result |
| `FR-QUERY-14` | M | The system SHALL support grouping sets, rollup and cube; window functions with row and range frames; and lateral joins |
| `FR-QUERY-15` | S | The system SHOULD provide an as-of (temporal) join operator, which does not exist in DataFusion and for which range joins are known to perform poorly |
| `FR-QUERY-16` | M | The system SHALL support bitemporal data with distinct validity and knowledge time axes, and as-of-knowledge queries. Restatement is a normal event and a system that cannot express "what did we know on date X" cannot support audit |
| `FR-QUERY-17` | M | The system SHALL implement a memory pool with per-tenant sub-pools under a global cap, and SHALL spill to disk under pressure |
| `FR-QUERY-18` | M | **Admission control is mandatory, not optional**, because hash joins do not spill in DataFusion. The system SHALL estimate peak memory from the plan and queue or reject rather than admitting a query it cannot afford. A pathological aggregation SHALL return a resource error, never cause process termination |
| `FR-QUERY-19` | M | Every query SHALL carry an end-to-end deadline propagated into execution and into every user-supplied function. Cancellation SHALL take effect within a bounded time, and this SHALL be tested — including for queries inside graph traversal and inside sandboxed user code |
| `FR-QUERY-20` | M | The system SHALL implement a caching hierarchy: table metadata, Parquet footers and page indexes, decoded batches, and results. Because files are immutable and every key embeds a snapshot identifier, **a new commit cannot produce a stale hit** — the key simply misses. No invalidation protocol is required |
| `FR-QUERY-21` | M | The one mutable cache key — the mapping from a table to its latest version — SHALL have a time-to-live bounded by the freshness objective, and SHALL be identified as such in the design |
| `FR-QUERY-22` | M | The plan cache key SHALL include the policy bundle version. Omitting it means a revocation does not take effect for any query whose plan is already cached, which is a data breach with a passing test suite |
| `FR-QUERY-23` | M | The result cache key SHALL include the tenant and the evaluated entitlement set |
| `FR-QUERY-24` | S | Result caching SHOULD be enabled by default only for pinned-snapshot reads. For continuously-fresh reads the target position moves constantly, so the key almost never repeats and the cache spends memory to achieve nothing |
| `FR-QUERY-25` | M | The system SHALL provide a local disk cache for remote Parquet, with admission on second access plus unconditional admission for freshly-compacted files, and an eviction policy resistant to the scan-once pattern that plain least-recently-used handles badly |
| `FR-QUERY-26` | M | Cached content for columns under column-level encryption SHALL be cached as ciphertext, so that on-disk cache retains the protection level of the object store |
| `FR-QUERY-27` | S | The system SHOULD provide materialized aggregates with declarative definition, incremental refresh driven by the change stream, and an explicit staleness contract. Only aggregates forming a commutative monoid may declare incremental refresh; the catalog SHALL reject mis-declarations at definition time |
| `FR-QUERY-28` | L | Automatic query rewrite onto materialized aggregates is deferred. v1 uses explicit addressing, which ships in weeks with near-zero correctness risk; automatic subsumption is a multi-month project requiring a property-test suite proving rewritten and base results agree |
| `FR-QUERY-29` | M | Every result SHALL carry provenance: per-table snapshot identifiers, tiers consulted with coverage intervals, plan hash, policy version, function versions, engine version and freshness at plan time |

### 5.5 Multidimensional analysis — `FR-CUBE`

*Added 2026-08-27 by owner directive: native cubing --- slice and dice on demand, roll-up and
consolidation --- is a capability this system is meant to be differentiated by, and it was
absent from this document. `FR-QUERY-14` covers SQL's `GROUP BY CUBE` and `ROLLUP`, which are
grouping constructs. They are not a cube: there is no dimension, no hierarchy, no declared
measure and no consolidation.*

**Why this system in particular.** Three properties it already has are the three a cube
engine most needs, and no product on the market has all three in one process:

- **Parent-child hierarchies are graphs.** A ragged organisation tree, an account structure
  with alternate roll-ups, a shared member appearing under two parents --- these are exactly
  what `FR-GRAPH`'s engine traverses. A consolidation path is a traversal.
- **Consolidation is a large floating-point reduction**, and `FR-QUERY-10` already requires
  those to be deterministic. Whether two runs of the same roll-up tie out is the question
  finance asks first and most products answer badly.
- **Every table already has a date axis** (`FR-OLTP-02a`), so a time dimension exists on
  everything without anybody declaring one.

| ID | Priority | Requirement |
|---|---|---|
| `FR-CUBE-01` | M | A cube SHALL be a **declared view over published tables**, not a second store. It SHALL hold no data of its own, exactly as the graph engine holds none: a cell exists because rows exist |
| `FR-CUBE-02` | M | A cube SHALL declare its dimensions, hierarchies, levels and measures. A dimension SHALL resolve to a column or to a table joined to the fact source; nothing SHALL be inferred from a column's name |
| `FR-CUBE-03` | M | **Every measure SHALL declare its aggregation rule for every dimension.** A measure with no declared rule SHALL be refused at definition time. Defaulting to summation is wrong for every balance, every rate and every ratio, and the resulting numbers are plausible |
| `FR-CUBE-04` | M | The system SHALL distinguish **additive**, **semi-additive** and **non-additive** measures. A semi-additive measure SHALL name the dimension it is not additive over and the rule that applies there — typically *last* or *average* over time for a balance |
| `FR-CUBE-05` | M | A plan applying an additive roll-up to a non-additive measure SHALL be **rejected at planning time**, naming the measure and the dimension. This is the same rule `FR-QUERY-12` states for precomputed measures, and a cube is where it is violated most often |
| `FR-CUBE-06` | M | Hierarchies SHALL support both **level-based** (fixed depth) and **parent-child** (ragged, arbitrary depth) forms. Ragged hierarchies SHALL be supported natively rather than padded to a fixed depth, because padding invents members that do not exist and they appear in results |
| `FR-CUBE-07` | M | A parent-child hierarchy SHALL be validated as acyclic **at definition time**, with the cycle reported. A cycle discovered during consolidation is an unbounded traversal, and the symptom is a query that never returns |
| `FR-CUBE-08` | M | Alternate roll-ups and shared members SHALL be supported. A member reachable by two paths SHALL contribute **once** to any ancestor, and this SHALL be tested; double-counting through an alternate hierarchy is the classic silent cube defect |
| `FR-CUBE-09` | M | Consolidation SHALL use the deterministic reduction of `FR-QUERY-10`. The same roll-up over the same snapshot SHALL produce bit-identical results |
| `FR-CUBE-10` | M | A cube SHALL support **both** on-demand computation and materialised cuboids, per cuboid rather than per cube, with the mode selectable at three levels: pinned in the cube definition, budgeted in server configuration, and overridable as a session preference |
| `FR-CUBE-11` | M | A materialised cuboid SHALL be keyed by *(cube definition version, snapshot identifier, cuboid specification)*. Per `FR-QUERY-20` a new commit therefore **cannot produce a stale hit** — the key misses and the answer is computed. There SHALL be no invalidation protocol and no time-to-live on a materialised cuboid |
| `FR-CUBE-12` | M | **An aggregate SHALL be computed only over rows the principal may read.** Two principals querying the same cell may legitimately see different totals. A total computed over rows the caller cannot see is a disclosure through arithmetic, and it is invisible |
| `FR-CUBE-13` | M | Where row-level policy reduces an aggregate's input, the result SHALL carry a completeness measure per `FR-QUERY-13`, so a filtered total is distinguishable from a complete one rather than being presented as the total |
| `FR-CUBE-14` | M | Slice, dice, roll-up, drill-down and pivot SHALL be expressible **without moving data out of the system** and without a separate cube-build step preceding the query |
| `FR-CUBE-15` | M | Cube storage SHALL be sparse. A dense representation is unusable past a handful of dimensions, and the number of dimensions is not something a user should have to ration |
| `FR-CUBE-16` | M | A cube SHALL be queryable from SQL, the system's primary surface. **MDX is deliberately not planned**: it is a large language with a small and shrinking client population, and the cost is a multi-month parser and semantics implementation for compatibility with tools this system does not target |
| `FR-CUBE-17` | M | Every cube result SHALL carry provenance per `FR-QUERY-29`, including the cube definition's version. A roll-up whose definition changed between two runs is a different number, and nothing else in the result says so |
| `FR-CUBE-18` | S | The system SHOULD support write-back to a cube cell for planning and what-if analysis, as a **separate, explicitly-versioned overlay** over the published facts. It SHALL NOT modify published data, and a query SHALL state whether an overlay was applied |
| `FR-CUBE-19` | M | Cube definitions SHALL be versioned and auditable. A change to a consolidation rule changes reported figures, and an audit that cannot say when a rule changed cannot explain why a number moved |
| `FR-CUBE-20` | M | **A cube SHALL return bit-identical results whether or not any cuboid is materialised.** This is what makes materialisation a cache rather than a second source of truth, and it SHALL be tested by running queries both ways and comparing bits — not by comparing within a tolerance |
| `FR-CUBE-21` | M | A query MAY be answered from a materialised **ancestor** cuboid only where the measure is additive along **every** dimension being further rolled up. A non-additive measure SHALL be answered from base data. This is the engine's principal silent-wrong-answer surface and SHALL be property-tested against the base-data answer |
| `FR-CUBE-22` | M | A semi-additive measure SHALL be answerable from an ancestor **only along the dimensions it is additive over**. Being correct along most dimensions and wrong along one is worse than being wrong everywhere, because it survives casual checking |
| `FR-CUBE-23` | M | Cuboid selection MAY be automatic, under an operator-set budget for space and refresh concurrency, informed by the observed query log rather than by a static estimate. A lattice of a thousand cuboids is not a thing a person can choose well from, and the attempt tunes the cube for imagined queries |
| `FR-CUBE-24` | M | **Automation SHALL extend only to decisions whose being wrong costs latency.** Aggregation rules, hierarchy definitions, measure semantics and the completeness contract SHALL never be inferred, because being wrong about those changes an answer |
| `FR-CUBE-25` | M | A session SHALL be able to disable use of materialised cuboids. With `FR-CUBE-11`'s keying this is a cost control rather than a correctness one, and it is the mechanism by which an audit run **proves** the materialised and computed paths agree |
| `FR-CUBE-26` | M | A materialised cuboid SHALL be stored as an ordinary published table in the warehouse, readable by external engines like any other. The open-storage commitment SHALL NOT have an exception for the fast path |
| `FR-CUBE-27` | M | Incremental refresh of a materialised cuboid SHALL be permitted only where the measure forms a commutative monoid, per `FR-QUERY-27`, and a mis-declaration SHALL be rejected at definition time |
| `FR-CUBE-28` | M | A result SHALL record whether it was answered from base data, from an exact materialised cuboid, or by rolling up an ancestor, as part of the provenance `FR-QUERY-29` requires |

---

### 5.6 Graph engine — `FR-GRAPH`

| ID | Pri | Requirement |
|---|---|---|
| `FR-GRAPH-01` | M | The graph SHALL be an Arrow-backed compressed sparse row structure with a reverse index, dense internal identifiers, and attributes held as separate Arrow arrays. General-purpose graph libraries whose storage is not Arrow SHALL NOT be on the critical path |
| `FR-GRAPH-02` | M | The graph model SHALL support **typed vertices and typed edges** with per-edge-type adjacency segments. Traversal primitives SHALL accept an edge-type mask. A single homogeneous graph is a domain-specific assumption and cannot represent heterogeneous networks without encoding types into weights |
| `FR-GRAPH-03` | M | Edges SHALL carry validity intervals, and traversal SHALL support a **time-respecting** mode in which successive edges are non-decreasing in time, with optional dwell and value-conservation constraints. Static traversal SHALL NOT be exposed for flow analysis |
| `FR-GRAPH-04` | M | Edges SHALL be stored sorted by source and time, so that "edges of a vertex after time *t*" is a binary search plus a contiguous slice |
| `FR-GRAPH-05` | M | Hydration SHALL produce an immutable, identified **epoch** bound to a table snapshot, published by atomic swap, reference-counted, and freed when the last reader releases it |
| `FR-GRAPH-06` | M | Hydration SHALL cost at most one linear pass per epoch, and attribute arrays SHALL be borrowed rather than copied. This SHALL be asserted by a test bounding allocation during construction |
| `FR-GRAPH-07` | M | Full hydration SHALL build into a shadow epoch and swap atomically, never blocking queries and never mutating a live epoch. The memory budget SHALL therefore account for two epochs during rebuild |
| `FR-GRAPH-08` | S | Incremental hydration SHOULD apply deltas via a copy-on-write overlay, rebuilding fully when the overlay exceeds a bounded fraction |
| `FR-GRAPH-09` | M | A property test SHALL assert that incremental application and full rehydration produce identical graphs. This is the single most valuable graph test, because incremental hydration is where subtle defects live |
| `FR-GRAPH-10` | M | Every graph result SHALL report its epoch, source snapshot version and lag. Three consistency modes SHALL be offered, and a **snapshot-consistent** mode requiring the epoch to be at least as current as the query snapshot SHALL be available and SHALL be required for any output used as evidence |
| `FR-GRAPH-11` | M | Graphs SHALL be hydrated per tenant. A traversal leaving the tenant's identifier space SHALL be an invariant violation, not a filtered result. Traversing a shared graph and filtering afterwards SHALL NOT be offered, as it leaks topology through timing and path structure |
| `FR-GRAPH-12` | M | Graph results SHALL be exposed as SQL table functions so they compose with relational plans, and SHALL report statistics, or the planner will order the downstream join badly |
| `FR-GRAPH-13` | M | Seeds SHALL be acceptable as a subquery, requiring a two-phase operator that drains the seed stream before traversal |
| `FR-GRAPH-14` | M | Every traversal SHALL enforce a hard result limit and time budget, and SHALL report truncation explicitly. **A truncated result must never be mistakable for an absence of results** |
| `FR-GRAPH-15` | M | Traversal SHALL support a maximum-degree cap and an exclusion list. In a power-law network a small number of vertices have enormous degree, and a multi-hop traversal through one touches most of the graph while returning analytically meaningless paths |
| `FR-GRAPH-16` | M | Monotone predicates SHALL be pushed into the traversal as pruning bounds, not applied afterwards. This is the difference between milliseconds and minutes on a deep search |
| `FR-GRAPH-17` | M | Core graph primitives SHALL include: bounded breadth-first and depth-first traversal, weighted shortest path, k-shortest loopless paths, simple-cycle enumeration, strongly and weakly connected components, degree and PageRank centrality, approximate betweenness, community detection, time-respecting traversal, temporal motif matching, and weighted path-product aggregation with damping and a pruning threshold |
| `FR-GRAPH-18` | M | Exact betweenness and closeness centrality SHALL be documented as out of scope at scale, with approximate variants provided |
| `FR-GRAPH-19` | M | A hydration exceeding its declared memory budget SHALL fail at build time with a clear error, never at query time with an allocation failure |
| `FR-GRAPH-20` | M | The system SHALL serve analytical and transactional traffic while a graph is rehydrating, returning a typed unavailable-or-rebuilding error for graph requests rather than a generic failure or a hang |
| `FR-GRAPH-21` | L | A standards-based graph query language is deferred to a later release and SHALL be implemented as a rewrite onto the v1 table functions, not as a second execution engine |

### 5.7 Data tiering — `FR-TIER`

| ID | Pri | Requirement |
|---|---|---|
| `FR-TIER-01` | M | The system SHALL support lifecycle migration of data to the published tier with purge from the source, at partition granularity in v1 |
| `FR-TIER-02` | M | Tiering SHALL be invoked **only** by explicit operator command or by a named, enabled, change-controlled schedule. It SHALL NOT occur as a side effect of any other operation. No maintenance, retention, compaction, vacuum or expiry job may originate a purge |
| `FR-TIER-03` | M | This SHALL be enforced structurally: the purge state machine's entry point requires an authorization value whose only constructors are the command path and the schedule evaluator. Enumerating those constructors SHALL constitute a complete audit of every way data can leave the system of record |
| `FR-TIER-04` | M | Purge SHALL be implemented as partition detach followed by drop. Row-level deletion SHALL NOT be used for archival in any code path |
| `FR-TIER-05` | M | Tiering-eligible tables SHALL be published with delete and truncate excluded from the publication, so that a defective code path cannot propagate a deletion |
| `FR-TIER-06` | M | The applier SHALL hold the archival extent map and SHALL treat any delete or truncate falling in an archived range as a **fatal alarm**, not a warning and not a skipped record |
| `FR-TIER-07` | M | A table SHALL be tiering-eligible only if it is append-only by contract and range-partitioned on the tiering key. Eligibility SHALL be validated at policy creation and re-validated before each purge |
| `FR-TIER-08` | M | Purge SHALL follow a durable, resumable state machine in which every transition is committed before the corresponding real-world action, and each phase is idempotent and resumable including within a phase |
| `FR-TIER-09` | M | Verification before purge SHALL be exhaustive and SHALL comprise row count, primary-key set equality via a Merkle digest over sorted blocks, and per-column checksums over a canonical byte encoding. Count equality alone is not evidence |
| `FR-TIER-10` | M | A canonical byte encoding SHALL be defined per logical type, with a lossless-or-reject rule. Types that cannot round-trip faithfully SHALL make a table ineligible, detected by a pre-flight check **at policy creation**, not at purge time |
| `FR-TIER-11` | M | Purge SHALL be gated on the published snapshot being committed, covered by a completed backup or caught-up replication, and covered by the applicable retention basis with legal-hold status resolved |
| `FR-TIER-12` | M | An immutable archive marker SHALL be created before detach, and the registry entry SHALL be mirrored to write-once storage |
| `FR-TIER-13` | M | A detached partition SHALL be retained in quarantine for a mandatory grace period, during which re-attachment is a simple operation |
| `FR-TIER-14` | M | Verification failure SHALL be terminal until an operator acts. There SHALL be no automatic retry, because failure indicates a defect and retrying is the wrong response |
| `FR-TIER-15` | M | **There SHALL be no flag that skips verification.** Verification is structurally absent from every path that could bypass it |
| `FR-TIER-16` | M | Queries spanning hot and archived ranges SHALL be unioned automatically, using the source catalog as authority for the hot extent and the archival registry as authority for the cold extent, both read within one source snapshot |
| `FR-TIER-17` | M | The extent tie-break rule SHALL be total: an uncovered range intersecting the predicate fails with a coverage-gap error; a range the registry believes cold but the catalog shows attached is read once from the source, with the inconsistency flagged separately |
| `FR-TIER-18` | M | Mutations targeting an archived range SHALL fail with a typed error naming the archive and the correction mechanism. Reporting zero rows affected is a silent wrong answer |
| `FR-TIER-19` | M | Corrections to archived data SHALL default to a compensating entry in the hot tier referencing the original. Controlled rewrite SHALL retain the prior version and record an amendment link |
| `FR-TIER-20` | M | Rehydration SHALL load into a schema excluded from every publication, SHALL never be attached to the live parent, SHALL be read-only, and SHALL carry a mandatory expiry after which the copy is dropped automatically |
| `FR-TIER-21` | M | After whole-table migration the table SHALL remain visible in the catalog under the same name, backed by the published tier, marked cold and read-only. A table that vanishes breaks every downstream tool and saved query |
| `FR-TIER-22` | M | Snapshot expiry SHALL be structurally incapable of removing a snapshot referenced by an archival registry entry until its retention basis has lapsed and legal hold is clear |
| `FR-TIER-23` | M | On startup and after any restore, the system SHALL reconcile the archival registry against the live catalog. On conflict it SHALL refuse unified queries on the affected table with a typed error rather than serving a plausible wrong answer |
| `FR-TIER-24` | M | Backup manifests SHALL record archival registry state, and restore SHALL report the archive delta before serving traffic |
| `FR-TIER-25` | M | A table carrying unvaulted direct identifiers SHALL be ineligible for tiering by default, since tiering converts a cheap erasure into an expensive one |
| `FR-TIER-26` | M | The planning command SHALL always be a dry run, SHALL report what moves, what remains, every failing precondition rather than the first, and SHALL emit an expiring plan digest |
| `FR-TIER-27` | M | Non-interactive invocation SHALL require a cluster assertion matching the server's declared identity, a valid unexpired plan digest bound to the exact ranges, and a change-management reference |
| `FR-TIER-28` | M | Scheduled tiering SHALL be disabled by default. A new schedule SHALL execute in plan mode first and SHALL require human approval of the resulting digest before it may run live |
| `FR-TIER-29` | M | Before each scheduled run the system SHALL compare the candidate set against the schedule's history and SHALL halt for re-approval if it exceeds the trailing median by a configured factor. This is what catches a clock error, a timezone defect or a mis-edited policy **before** it archives years of data in one pass |
| `FR-TIER-30` | M | Blast-radius limits SHALL apply per run and cumulatively per day. On reaching a limit the job SHALL stop cleanly at the limit and report |
| `FR-TIER-31` | M | Scheduled runs SHALL default to stopping before purge. The recommended production configuration is continuous automatic archive and verification with purge performed deliberately by a human — which delivers the valuable half of tiering at none of the risk |
| `FR-TIER-32` | M | Kill switches SHALL exist at schedule, runtime and configuration level. A kill switch SHALL stop new phases and SHALL NEVER abort a job mid-detach |
| `FR-TIER-33` | M | Defining, approving, executing, purging, dropping, rehydrating and retiring SHALL be **distinct permissions**. The principal who defines a policy SHALL NOT be a principal who approves it or who executes a purge under it |
| `FR-TIER-34` | M | A scheduled run SHALL be attributable to people: audit records SHALL name the service principal **and** the human definer and approver of the schedule version in force. "The scheduler did it" is not an acceptable audit answer |
| `FR-TIER-35` | M | The system SHALL produce a signed evidence pack per archive, generatable years later from the write-once manifest alone |

### 5.8 API surfaces — `FR-API`

| ID | Pri | Requirement |
|---|---|---|
| `FR-API-01` | M | The system SHALL expose **Arrow Flight SQL** as the primary bulk data plane, giving zero-copy streaming results and compatibility with existing Flight SQL drivers |
| `FR-API-02` | M | The system SHALL expose a **PostgreSQL wire protocol** front door, so that existing clients, tools and drivers work without a bespoke driver. This is the highest-adoption-value surface in the product |
| `FR-API-03` | M | The wire-protocol front door SHALL emulate enough system-catalog surface for mainstream tooling, verified by a tool-compatibility matrix as an acceptance gate. This is the difference between "a command-line client connects" and "a BI tool works" |
| `FR-API-04` | M | The system SHALL expose a **gRPC control plane** for administration, tenancy, policy, catalog, health, jobs and archive operations, and for the graph API, which is not relational in shape |
| `FR-API-05` | S | The system SHOULD expose a thin REST gateway over the control plane for administration and small ad-hoc queries |
| `FR-API-06` | M | Bulk data SHALL NOT be offered over JSON. Result size on the REST surface SHALL be hard-capped, returning a Flight ticket for anything larger. Serializing analytical results as JSON destroys the zero-copy premise and defines published benchmarks downward |
| `FR-API-07` | M | Streaming results SHALL be demand-driven. A result set SHALL NOT be fully materialized server-side |
| `FR-API-08` | M | The graph API in v1 SHALL be a structured traversal specification plus SQL table functions, designed as a future compilation target for a standard query language |
| `FR-API-09` | M | The system SHALL publish a stable error catalog with distinct codes mapped to protocol statuses, each with documented remediation. The catalog SHALL be snapshot-tested so that removing or renumbering a code breaks the build |
| `FR-API-10` | M | Wire APIs SHALL be additively versioned within a major version, with breaking-change detection in continuous integration and a published client compatibility matrix that is tested, not asserted |
| `FR-API-11` | M | Every request SHALL carry a deadline. Every response SHALL carry provenance |
| `FR-API-12` | M | The system SHALL support three read modes — strongly consistent, bounded-freshness, and pinned-snapshot — selectable per request, with pinned-snapshot being the mode required for reproducible outputs |
| `FR-API-13` | M | A write SHALL return a session token carrying its commit position. Passing that token to a subsequent analytical query SHALL guarantee the write is visible, by waiting bounded by the request deadline. **Without this the first demonstration anyone attempts shows their own write missing**, and they will reasonably conclude the system is broken |
| `FR-API-14` | M | A session SHALL be able to pin a snapshot for repeatable reads. A pinned session reads committed data only; requesting maximum freshness within a pinned session is a contradiction and SHALL be rejected rather than silently reconciled |
| `FR-API-15` | M | Snapshot pins SHALL be leased with a bounded time-to-live, so that an abandoned session cannot indefinitely block space reclamation |
| `FR-API-16` | M | The client-facing snapshot token SHALL be opaque and format-agnostic. Leaking a format-specific version identifier into the wire format would make a later format addition a breaking change |
| `FR-API-17` | M | As-of queries SHALL be expressible by version, timestamp or log position, always resolved through a durable snapshot registry — never by inferring from file modification times |

### 5.9 Security, tenancy and governance — `FR-SEC`

| ID | Pri | Requirement |
|---|---|---|
| `FR-SEC-01` | M | The system SHALL be multi-tenant. Tenant identity SHALL be a parameter on every internal interface from the first commit, even where enforcement lands later. Retrofitting tenancy is the classic multi-month tax |
| `FR-SEC-02` | M | A single principal type SHALL be resolved at the edge and carried through every layer. **It SHALL be impossible to reach a table scan without a security context**, enforced by the type system: the catalog's resolution function takes a security context and there is no other constructor |
| `FR-SEC-03` | M | Authentication SHALL support federated identity tokens, mutual TLS, and scram authentication on the wire-protocol door |
| `FR-SEC-04` | M | Authorization SHALL be centralized in a pure, exhaustively testable policy component with no I/O |
| `FR-SEC-05` | M | Row- and column-level security SHALL be enforced **at plan construction, in one place**, not separately in three engines. All engines SHALL resolve tables exclusively through the catalog, which returns a policy-rewritten provider |
| `FR-SEC-06` | M | Row-level predicates SHALL be conjoined into the scan, not supplied as an optimizer hint, and their presence in the final physical plan SHALL be asserted |
| `FR-SEC-07` | M | The graph tier SHALL resolve through the same catalog, so that an unauthorized edge is never materialized in memory for that tenant |
| `FR-SEC-08` | M | A negative test suite SHALL be a first-class deliverable, asserting for every policy fixture that forbidden rows, columns and edges are absent from results, absent from the physical plan, and absent from graph memory. Mutation testing SHALL be applied to the policy component, because a surviving mutant means a test that passes for the wrong reason — which here is a data breach with a green build |
| `FR-SEC-09` | M | Tenant isolation in storage SHALL use both a per-tenant prefix and per-tenant scoped credentials, so that a path-construction defect cannot cross-read |
| `FR-SEC-10` | M | Per-tenant resource governance SHALL cover concurrency, memory, processor time, result bytes, scanned bytes and traversal frontier size, with typed errors naming the exceeded quota |
| `FR-SEC-11` | M | The audit record SHALL capture the authorization decision and the **data version**, including resolved snapshot identifiers for every table touched. Without these, the question "show exactly what this user saw on that date" is unanswerable |
| `FR-SEC-12` | M | Audit SHALL be append-only and hash-chained, mirrored to immutable storage. In strict mode an audit write failure SHALL fail the request |
| `FR-SEC-13` | M | Transport SHALL be encrypted. Local sockets SHALL rely on filesystem permissions with peer credential verification |
| `FR-SEC-14` | M | Data at rest SHALL be protected by volume encryption for the transactional data directory, server-side encryption for object storage, and column-level encryption for sensitive columns. The absence of transparent encryption in community PostgreSQL SHALL be stated plainly, with volume encryption as the compensating control |
| `FR-SEC-15` | M | Key management SHALL sit behind a provider abstraction with envelope encryption. Key rotation SHALL NOT require rewriting data. **No key material in configuration files, ever** — configuration carries key references |
| `FR-SEC-16` | M | Direct personal identifiers SHALL be designed out of the analytical tier by default, held only in the transactional store with surrogate keys downstream. This resolves the erasure-versus-immutability conflict for most cases and cannot be retrofitted affordably |
| `FR-SEC-17` | M | A per-column retention-and-erasure policy engine SHALL be core capability. Different domains impose contradictory obligations — some records must be retained and may not be erased, others must be erased on request — frequently on different columns of the same table |
| `FR-SEC-18` | M | Erasure SHALL be a distinct job class with distinct authorization, which checks retention obligations and legal holds first and refuses if any apply, and which records that history before a given date is no longer reproducible |
| `FR-SEC-19` | M | The ordinary maintenance scheduler SHALL be structurally incapable of destroying retained history |
| `FR-SEC-20` | M | An erasure index SHALL map subjects to affected tables, columns, keys, retention basis and hold status, so that a request can be checked against obligations before execution |
| `FR-SEC-21` | M | Data residency SHALL be enforceable per tenant, including in logs and traces |
| `FR-SEC-22` | M | **No log line, trace attribute or metric label may contain tenant data.** Query text is data: a normalized plan hash is logged by default, with full text only under explicit policy and routed to the audit store |
| `FR-SEC-23` | M | External engines reading the warehouse directly **bypass row- and column-level enforcement**. This limitation SHALL be stated plainly rather than obscured, and the compensating controls SHALL be specified: storage-level access control as the real enforcement boundary, per-tenant prefixes and scoped credentials, column encryption so unauthorized readers obtain ciphertext, and a published-versus-private classification determining what is externally readable at all |
| `FR-SEC-24` | M | The extension API is a **security boundary** once third parties author packs, and SHALL be tested as one by an adversarial pack attempting to read another tenant's data, escape its sandbox, register a panicking or non-terminating function, exceed its budget, and shadow a core name — each rejected with a named error |

### 5.10 Operations — `FR-OPS`

| ID | Pri | Requirement |
|---|---|---|
| `FR-OPS-01` | M | The system SHALL start from a single command against an empty directory, initialize itself, and be ready without manual database preparation |
| `FR-OPS-02` | M | A first-time user SHALL reach a running server with sample data and a successful query within five minutes, on a clean machine with no container runtime, message broker, object store or cloud credentials. **This SHALL be a timed test in continuous integration so it cannot rot** |
| `FR-OPS-03` | M | Configuration SHALL come from file, environment and command line with documented precedence, validated against a published schema. **An unknown configuration key SHALL be an error, not a warning** — silently ignored typos are a leading cause of production incidents and cost nothing to prevent |
| `FR-OPS-04` | M | The data directory SHALL have a defined layout classifying each area as durable, regenerable or secret. This classification drives backup, disaster recovery and container volume design |
| `FR-OPS-05` | M | Regenerable areas SHALL be cleared on boot after an unclean shutdown |
| `FR-OPS-06` | M | Startup SHALL be a defined, observable sequence with a distinct recovering state separate from ready, reporting progress and remaining work |
| `FR-OPS-07` | M | Shutdown SHALL follow a defined drain order — stop admitting, drain queries by deadline, finish the in-flight apply batch, persist the applied position **strictly after** the commit is durable, flush writers, release leases, stop the database gracefully, flush telemetry. **This ordering is a correctness property, not an implementation detail** |
| `FR-OPS-08` | M | Termination by signal at any instant SHALL be safe. Crash consistency is the real requirement and SHALL be tested by repeated hard kills under load |
| `FR-OPS-09` | M | Health SHALL be reported through distinct startup, liveness and readiness endpoints. Readiness SHALL account for replication lag; **liveness SHALL NOT**, or a lagging pipeline will cause an orchestrator to kill an otherwise healthy node in a restart loop |
| `FR-OPS-10` | M | The system SHALL expose metrics covering query latency, admission rejections by reason, memory high-water and spill, replication lag in seconds and bytes, slot state, commit attempts and conflicts, compaction debt, graph hydration and residency, storage request latency and errors, per-tenant consumption, and runtime scheduling delay |
| `FR-OPS-11` | M | Four version axes SHALL be managed independently — the internal schema, the database major version, table format protocol versions, and wire APIs — each with a documented compatibility and migration policy |
| `FR-OPS-12` | M | The system SHALL refuse to write a table whose protocol version it does not fully support, and SHALL report read-only degradation rather than silently misreading |
| `FR-OPS-13` | M | Backup SHALL produce a **manifest binding the transactional backup, table snapshots and key generation to a consistent point**, verified on restore. Three backups that do not agree with each other are worse than one |
| `FR-OPS-14` | M | Table snapshots referenced by a backup SHALL be protected from expiry for the backup's lifetime |
| `FR-OPS-15` | M | Restore drills SHALL be automated and periodic, with retained evidence. An untested backup is a rumour |
| `FR-OPS-16` | M | The system SHALL provide a diagnostic command reporting environment problems, resource limits, storage reachability and conformance, database configuration, replication health, tables lacking a usable replica identity, maintenance health, and archival consistency — each with an actionable remediation |
| `FR-OPS-17` | M | The diagnostic SHALL report **time until a problem becomes user-visible**, not merely its current value. "Compaction debt is 400 GB" is far less actionable than "at the current write rate, query latency on this table will double in about nine days" |
| `FR-OPS-18` | M | The system SHALL provide a support bundle: redacted configuration, logs, metrics, catalog state, task registry |
| `FR-OPS-19` | M | The system SHALL provide a self-benchmark command running the acceptance suite on the operator's own hardware and reporting pass or fail against the stated objectives |
| `FR-OPS-20` | M | Maintenance SHALL be automatic and internal, covering both transactional and analytical sides under one scheduler, since both draw on the same machine budget and must be prioritized against each other |
| `FR-OPS-21` | M | Maintenance jobs SHALL be classed by priority, with safety and availability classes permitted to preempt queries and all other classes bounded by a duty cycle |
| `FR-OPS-22` | M | The maintenance budget SHALL never borrow from the change-capture allocation |
| `FR-OPS-23` | M | Maintenance jobs SHALL checkpoint progress and resume after interruption. A job that can only run to completion will never complete on a busy system |
| `FR-OPS-24` | M | Every maintenance job SHALL be safe to run twice, and SHALL leave no corruption if killed at any instant |
| `FR-OPS-25` | M | Maintenance SHALL run on exactly one elected node per table, elected through the transactional store rather than a bespoke consensus implementation |
| `FR-OPS-26` | M | The system SHALL detect and remediate transaction-identifier exhaustion risk automatically in managed mode, and detect and alert in attached mode. Left unhandled this takes the database offline and requires single-user recovery |
| `FR-OPS-27` | M | In attached mode the system SHALL operate under a three-level consent model — observe, advise, act — defaulting to observe, with a hard prohibition on modifying server configuration, running blocking rewrites, terminating sessions it did not open, or dropping objects it did not create |
| `FR-OPS-28` | M | Under sustained write pressure exceeding maintenance throughput, the system SHALL apply backpressure in a defined order, beginning with lengthening the commit interval — the highest-leverage lever, because writing fewer larger files attacks the cause rather than the symptom |

### 5.11 Extensibility — `FR-EXT`

| ID | Pri | Requirement |
|---|---|---|
| `FR-EXT-01` | M | The core SHALL contain no domain-specific concept. Domain capability SHALL arrive exclusively through a published extension API |
| `FR-EXT-02` | M | A pack SHALL be able to contribute schemas, logical types, scalar/aggregate/window functions, graph algorithms, view and materialized-view definitions, rules, named endpoints and policy vocabulary — and nothing outside that enumerated set |
| `FR-EXT-03` | M | The extension API SHALL define **SANKHYA's own function traits** and re-export only a curated, pinned subset of Arrow types. Re-exporting the query engine's traits would break every pack on every engine upgrade, several times a year |
| `FR-EXT-04` | M | The extension API SHALL be the only component carrying a stable-version commitment while the rest of the system is pre-1.0, versioned independently, additive-only within a major, with mechanical breaking-change detection in continuous integration |
| `FR-EXT-05` | M | A pack SHALL depend on a **strictly limited set of core crates**. When a pack legitimately requires another, the build fails — and that failure is the signal that the extension API has a gap. It SHALL be treated as an API design task, never as grounds to widen the allowance |
| `FR-EXT-06` | M | Nothing SHALL enter the extension API until two packs **from different domains** require it. One pack's need is a pack-local helper |
| `FR-EXT-07` | M | Type-erased downcasting, free-form document values and open-ended string maps SHALL be prohibited in the extension API. These are how extension interfaces rot without ever changing shape |
| `FR-EXT-08` | M | No core component SHALL reference a pack by name. Packs SHALL self-register, so that no dispatch on pack identity can exist anywhere in the core |
| `FR-EXT-09` | M | Two reference packs SHALL be built and tested continuously, deliberately chosen to be **opposites** — one high-volume, narrow, time-series and graph-free; one entity-heavy with a physical network graph and string-heavy joins. Neither may be financial. **The acceptance test is mechanical: the change that adds a reference pack must touch zero core files** |
| `FR-EXT-10` | M | An extension conformance suite SHALL be generated for every pack, asserting that every declared contribution type is honoured |
| `FR-EXT-11` | M | A naming lint SHALL reject domain vocabulary in core identifiers, file names and documentation. **It SHALL be documented that this catches leakage, not shape** — a core component can be immaculately neutral in its naming and still be bent toward one domain. The lint catches naming; the reference packs catch shape |
| `FR-EXT-12` | M | Packs SHALL be deliverable in three tiers: declarative bundles containing no code, sandboxed modules for logic the declarative form cannot express, and compiled extensions reserved for genuinely hot paths |
| `FR-EXT-13` | M | The declarative tier SHALL be complete enough to express the substantial majority of a real pack, and SHALL be hot-loadable without a server rebuild |
| `FR-EXT-14` | M | Dynamically-loaded native extensions SHALL NOT be supported, and the reasons SHALL be recorded so the decision is not periodically relitigated |
| `FR-EXT-15` | M | A continuous integration job SHALL build and test the core with **zero packs enabled**, preventing a core test from depending on pack fixtures |
| `FR-EXT-16` | M | Reference domain packs for risk analytics and financial-crime detection SHALL be delivered as packs, on the same extension API available to third parties, and SHALL exist as much to prove the API is real as to serve their industries |

---

## 6. Non-functional requirements

### 6.1 How performance requirements are written

The original brief stated "sub-50 ms for graph path traversals under 100,000 nodes" and "sub-second response times for complex analytical group-bys over tens of millions of rows". Neither is a requirement, for opposite reasons: the first is met by any competent implementation on day one and therefore constrains nothing, while the second is undefined — *complex* how, cold or warm, what cardinality, what selectivity, on what hardware — and therefore cannot be failed.

Every performance requirement in this document therefore states:

1. **A named public benchmark query wherever one exists.** Public suites exercise query shapes a domain suite never will, and they are the only numbers comparable against other engines. An uncomparable benchmark is marketing.
2. **The reference hardware.**
3. **The dataset and scale factor.**
4. **The cache state** — cold or warm.
5. **The concurrency level.**
6. **The preconditions** under which the target holds.
7. **A test identifier.**

> **`NFR-META-01`** — No performance requirement SHALL be recorded without all seven. A target without conditions is not falsifiable and is excluded from this document.

> **`NFR-META-02`** — When a precondition is violated, the engine SHALL degrade **predictably and observably**, reporting a degradation reason in response metadata and in metrics. Silent misses are prohibited.

### 6.2 Reference hardware

| Parameter | Reference node |
|---|---|
| Processors | 32 physical cores |
| Memory | 256 GB |
| Local storage | NVMe, ≥ 3 GB/s, sized for the hot working set |
| Network | 10 Gb/s |
| Object storage | Same region, 1–3 GB/s effective aggregate |

### 6.3 Performance model

Published so that targets can be re-derived rather than trusted.

| Quantity | Planning value |
|---|---|
| Parquet decode and decompress, primitive or dictionary, per core | 0.8–2.0 GB/s decoded |
| — strings and nested types | 3–5× worse |
| Snappy decompression | ~1.5–2.5 GB/s per core |
| Zstd level 1 decompression | ~0.8–1.2 GB/s per core |
| Hash aggregation, integer key, low cardinality, per core | 50–150 M rows/s |
| — high-cardinality or string keys | 10–30 M rows/s |
| Object storage first-byte latency | 20–50 ms |
| Local NVMe first-byte latency | ~100 µs |
| CSR topology traversal, per core | 200 M–1 G edges/s |
| Pointer-chasing adjacency traversal, per core | 20–80 M edges/s |

**Two conclusions follow directly, and both shape the architecture.**

*First, the object-store latency ratio is 200–500×, not a bandwidth ratio.* This is why a local cache is the highest-return optimization available and why request coalescing matters more than raw throughput.

*Second, there is a crossover.* On a 32-core node, aggregate decode capacity is roughly 48 GB/s decoded, or about 14 GB/s of compressed input. Below that, the system is I/O-bound and clever kernels buy nothing — spend the effort on avoidance and caching, and prefer heavier compression. Above it, the system is decode-bound and encoding choice starts to matter. Object storage at 1–3 GB/s sits far below the crossover; page cache sits far above it.

### 6.4 Service-level objectives

Targets are stated against the reference node, and each is conditional on its stated preconditions.

| ID | Class | Preconditions | Concurrency | Target |
|---|---|---|---|---|
| `NFR-PERF-01` | Primary-key point lookup | Bloom filter or index on the key; warm | 64 | p99 < 5 ms |
| `NFR-PERF-02` | Selective needle lookup over a large table | Bloom filters enabled; late materialization enabled; warm | 8 | p95 < 250 ms |
| `NFR-PERF-03` | Multi-dimensional pivot, warm, pruned | Partition predicate present; deleted-row fraction ≤ 10%; ≤ 8 uncompacted files per partition | 8 | p95 < 1 s |
| `NFR-PERF-04` | Wide scan, warm, local cache | Sort key aligned with predicate | 4 | p95 < 3 s |
| `NFR-PERF-05` | Cold scan from object storage | None | 1 | p95 < 15 s — **explicitly not sub-second, and stated as such** |
| `NFR-PERF-06` | Aggregation over fixed-size numeric vectors with exact order statistics | Working set resident in memory | 4 | p95 < 2 s |
| `NFR-PERF-07` | Same, served from materialized aggregates | Staleness within contract | 32 | p95 < 300 ms |
| `NFR-PERF-08` | Streaming evaluation against materialized baselines plus bounded graph expansion | Baselines materialized; graph epoch warm | 500/s | p99 < 50 ms |
| `NFR-PERF-09` | Bounded time-respecting traversal, small graph | Warm epoch | 1 | p95 < 50 ms |
| `NFR-PERF-10` | Same, large graph | Warm epoch; degree cap applied | 8 | p95 < 500 ms |
| `NFR-PERF-11` | Seeded multi-hop neighbourhood, large graph | Warm epoch | 4 | p95 < 500 ms |
| `NFR-PERF-12` | Whole-graph connected components | Batch | 1 | Bounded, published |
| `NFR-PERF-13` | Cold full graph hydration | Non-blocking via shadow epoch and atomic swap | — | Bounded, published |
| `NFR-PERF-14` | Incremental graph epoch update | — | — | < 1 s for a bounded delta |
| `NFR-PERF-15` | End-to-end capture latency, steady state | Within configured commit interval | — | p99 within the stated freshness budget |
| `NFR-PERF-16` | Query cancellation | — | — | Effective within 200 ms, including inside graph traversal and sandboxed user code |

**Explicitly out of scope**, recorded so the boundary is visible: exact betweenness and closeness centrality on large graphs, and unpruned scans of the entire warehouse.

> **`NFR-PERF-17`** — The freshness objective SHALL be stated separately for internal readers and for external engines reading the warehouse directly (`DEC-08`), because they differ by design and publishing a single figure would be false.

> **`NFR-PERF-18`** — The graph tier's freshness objective SHALL be stated separately from the analytical tier's, since a hydrated epoch does not splice the arrival buffer. Implying otherwise would overclaim.

### 6.5 Scale and capacity

| ID | Requirement |
|---|---|
| `NFR-SCALE-01` | The analytical tier SHALL operate over warehouses in the hundreds of terabytes |
| `NFR-SCALE-02` | A published capacity model SHALL state, for each warehouse size tier, the required node memory, local cache size, metadata footprint, expected file count, and the compaction throughput needed to sustain a given write rate |
| `NFR-SCALE-03` | Targets that are **size-invariant** SHALL be distinguished from those that **degrade with total warehouse size**. A pruned point lookup should be unaffected by total size; anything metadata-bound or unpruned is not |
| `NFR-SCALE-04` | The system SHALL publish the ordered list of what fails first as scale increases, with the specific limit each encounters. This list is the capacity model and the scaling roadmap |
| `NFR-SCALE-05` | Where a scale limit is structural rather than a tuning failure — a write rate at which compaction cannot keep up on one node regardless of budget — it SHALL be named explicitly as a hard capacity limit |
| `NFR-SCALE-06` | Memory accounting SHALL include components outside the process — notably the database's shared buffers and per-connection working memory, which live in the same container. The accounting identity SHALL be published, since most embedded-database deployments exhaust memory because nobody wrote it down |
| `NFR-SCALE-07` | Metadata caches scale with **file count**, not query volume, and SHALL be sized accordingly in the capacity model |
| `NFR-SCALE-08` | The graph tier SHALL publish a memory budget per vertex and per edge type, so that hardware can be sized before purchase |
| `NFR-SCALE-09` | Statistics collection SHALL remain affordable at full scale, becoming incremental or sampled where exhaustive collection is not viable. Poor statistics at scale produce catastrophically bad join orders |

### 6.6 Reliability and consistency

| ID | Requirement |
|---|---|
| `NFR-REL-01` | **INV-1.** No query, at any concurrency, resource level, plan shape or tenant, SHALL be able to cause the transactional primary to lose availability or durability. Gated by a chaos suite, not by review |
| `NFR-REL-02` | **INV-2.** The system SHALL never bloat, wedge or exhaust the storage of the database it replicates from — through its replication slot, its own long-running queries, or its own maintenance |
| `NFR-REL-03` | For every committed source transaction, once the pipeline reports the corresponding position applied, a read of the published table at that version SHALL return exactly the rows a source read at that position would return — no missing rows, no duplicates, no stale values |
| `NFR-REL-04` | `NFR-REL-03` SHALL be continuously measured in production and SHALL alert on any nonzero discrepancy. **It is a metric, not a design claim** |
| `NFR-REL-05` | The reconciliation harness SHALL compare against an independent expected-state model maintained by the harness, never a second query against the source, or a defect in the source reader will conceal a defect in the pipeline |
| `NFR-REL-06` | Row digests SHALL be combined with an order-independent but **duplicate-sensitive** operator. An operator that cancels duplicate pairs would conceal precisely the duplication defect the harness exists to detect |
| `NFR-REL-07` | Recovery-point and recovery-time objectives SHALL be stated per tier and per deployment mode, and SHALL be measured at commissioning rather than estimated |
| `NFR-REL-08` | A query spanning several tiers SHALL be consistent at exactly one log position, resolved once at plan start and applied to every tier and every table in the request |
| `NFR-REL-09` | Tier coverage intervals SHALL be contiguous and non-overlapping. An uncoverable range SHALL fail the query with a typed error, never return a partial answer |
| `NFR-REL-10` | Transactional atomicity SHALL be preserved across a spliced query: a source transaction touching several tables is either wholly visible or wholly invisible |
| `NFR-REL-11` | Three distinct times SHALL be recorded and never conflated: business event time, source commit time and publication time. All in a single timezone, all explicit |
| `NFR-REL-12` | Given a recorded snapshot identifier, engine version, configuration hash and extension versions, re-executing a query SHALL produce an identical result. Reproducibility is required for audit, regulatory filing, clinical and safety review, and billing dispute alike |

### 6.7 Quality, buildability and process

| ID | Requirement |
|---|---|
| `NFR-QUAL-01` | Line coverage SHALL meet differentiated targets by layer, highest for pure logic crates, enforced as a ratchet against the main-branch baseline rather than a cliff, plus a per-change diff-coverage gate |
| `NFR-QUAL-02` | **Mutation testing SHALL be applied** to the numeric, protocol-decoding, apply-logic and policy crates with a stated minimum score. Coverage is necessary and not sufficient: a test that executes a line without asserting anything scores perfectly on coverage and zero on mutation |
| `NFR-QUAL-03` | Property-based testing SHALL cover protocol decoding, fixed-point arithmetic, order-statistic conventions, commit-conflict resolution, canonical type encoding, and the equivalence of incremental and full graph hydration |
| `NFR-QUAL-04` | The protocol decoder, the wire-protocol frame parser, Arrow and Parquet metadata decoding, the table-log parser, the SQL parser and the configuration parser SHALL be continuously fuzz-tested. **A crash is a priority-one defect with a same-week fix commitment** |
| `NFR-QUAL-05` | Fault injection SHALL be a first-class mechanism, with each scenario asserting a **named recovery behaviour** rather than merely absence of a crash |
| `NFR-QUAL-06` | A deterministic mode SHALL exist under which the same scenario run twice produces byte-identical committed metadata and byte-identical query output. One test, enormous coverage: it detects iteration order leaking into results, wall-clock in metadata, unsorted listings and non-deterministic reduction order |
| `NFR-QUAL-07` | Clock and identifier generation SHALL be injected everywhere, enforced by lint. Without this, `NFR-QUAL-06` is impossible |
| `NFR-QUAL-08` | Semantic differences between the transactional and analytical engines SHALL be enumerated in a documented, tested list. Anything not on the list is a defect. Users will encounter these, and "the same query gave two answers" destroys trust faster than an outage |
| `NFR-QUAL-09` | The pull-request pipeline SHALL complete within twenty minutes. Beyond that engineers stop reading it, and every quality gate downstream becomes theatre |
| `NFR-QUAL-10` | Duplicate versions of the Arrow, Parquet, object-store, query-engine and RPC dependency families SHALL fail the build. This is a **correctness gate**, not hygiene: two Arrow majors in one process make identically-named types incompatible |
| `NFR-QUAL-11` | Dependency and toolchain versions SHALL be pinned at workspace level; no member may name its own version. Query-engine major upgrades SHALL be planned work with a documented procedure, not automated dependency updates |
| `NFR-QUAL-12` | A standing quarterly capacity allowance SHALL be budgeted for dependency maintenance, with a named rotating owner. If it is not in the plan it will be done badly under time pressure |
| `NFR-QUAL-13` | Library crates SHALL use typed errors; only the composition root may use erased error types. Every error variant SHALL be classifiable into a retry-or-fail taxonomy, proven by an exhaustive test over all variants |
| `NFR-QUAL-14` | Unhandled failure in one subsystem SHALL NOT terminate the process. Task supervision SHALL be mandatory with a declared restart policy per subsystem, and raw task spawning SHALL be prohibited by lint |
| `NFR-QUAL-15` | The reactor SHALL never be blocked. A stall detector SHALL ship in every build |
| `NFR-QUAL-16` | Unsafe code SHALL be forbidden by default, with per-crate exceptions requiring a written decision, a designated reviewer, documented safety justification per block, and undefined-behaviour checking in continuous integration |
| `NFR-QUAL-17` | Every performance-sensitive change SHALL post before-and-after measurements, checked against **allocation counts and bytes scanned as well as wall-clock**. A five percent latency win that doubles allocations is a deferred outage |
| `NFR-QUAL-18` | Latency budgets SHALL be decomposed and attributable per stage, turning "it got slower" into "this stage got slower" |
| `NFR-QUAL-19` | Documentation SHALL be updated in the same change as the behaviour it describes. A change that alters behaviour without altering documentation is incomplete. Where documentation can be generated from code it SHALL be, with a staleness check in continuous integration |
| `NFR-QUAL-20` | Every user-reachable error SHALL have documented remediation, generated from the same source as the error catalog so the two cannot diverge |
| `NFR-QUAL-21` | Supply-chain controls SHALL include a software bill of materials, signed release artifacts, advisory and licence scanning, reproducible builds, and a pinned toolchain. The system ships its own database binaries and therefore inherits responsibility for their security updates |
| `NFR-QUAL-22` | Release testing SHALL include upgrade from the previous version against a fixture dataset, asserting identical reconciliation digests and identical query results, plus a documented and tested rollback procedure |

---

## 7. Verified ecosystem baseline

All figures verified against crates.io, upstream repositories and issue trackers on **2026-08-25**. This table is normative for the initial dependency pin set and SHALL be re-verified each quarter.

### 7.1 Versions

| Crate | Version | Note |
|---|---|---|
| `datafusion` | 55.0.0 | Built on Arrow 59. Majors roughly every 6–8 weeks |
| `arrow` / `parquet` / `arrow-flight` | 59.2.0 | |
| `deltalake` / `deltalake-core` | 0.32.4 | Released pins: Arrow 58, `datafusion ^53.1.0`, `object_store ^0.13.2` |
| `deltalake` (upstream `main`) | unreleased | **Already moved to Arrow 59 / Parquet 59 / DataFusion 55** |
| `delta_kernel` | 0.27.1 | **No DataFusion dependency. Arrow 58 or 59, feature-gated** |
| `buoyant_kernel` | 0.25.1 | The kernel distribution `deltalake-core` actually depends on |
| `iceberg` and catalog crates | 0.10.1 | Pre-1.0; `iceberg-datafusion` pins `datafusion ^53.1.0` |
| `petgraph` | 0.8.3 | Last release 2025-09-30 |
| `object_store` | **0.13.2** (0.14.1 is published but unused) | DataFusion 55 and `delta_kernel` both require the 0.13 line — see §7.1.1 |
| `tokio-postgres` | 0.7.18 | **No logical replication support** |
| `sqlx` | 0.9.0 | **No `CopyBoth` support** |
| `postgresql_embedded` | 0.21.0 | Spawns a child process; `bundled` embeds the archive |
| `pgwire` | 0.40.7 | |
| `wasmtime` | 48.0.1 | Sets the highest language-version floor in the tree |
| `pyo3` | 0.29.2 | |
| `tpchgen` / `tpchgen-arrow` | 3.0.0 | Pure Rust, zero dependencies, emits Arrow directly |
| `sqllogictest` | 0.29.1 | |
| `testcontainers` / `proptest` / `insta` / `criterion` | 0.28.0 / 1.11.0 / 1.48.0 / 0.8.2 | |
| `cargo-semver-checks` | 0.50.0 | Required for the extension API gate |
| Local toolchain | rustc / cargo 1.97.1 | |

### 7.1.1 The consistent pin set

The `DEC-06` decision is not merely advisable, it is **buildable today with zero duplicate dependency versions**. Verified directly from the dependency metadata:

```
datafusion       55.0.0   ->  arrow ^59.2.0, parquet ^59.2.0, object_store ^0.13.2
delta_kernel     0.27.1   ->  feature "arrow-59" = [arrow_59, parquet_59, object_store_13]
```

Therefore the following graph is internally consistent, with `deltalake-core` and `iceberg-datafusion` **absent from the read path entirely**:

```
datafusion 55  +  arrow 59  +  parquet 59  +  object_store 0.13  +  delta_kernel 0.27 (arrow-59)
```

**A correction worth recording, because the opposite was assumed earlier in review.** `object_store` is **not** a second version-skew axis. DataFusion 55 itself requires the 0.13 line, matching `delta_kernel` exactly. The incompatibility is confined to `arrow`, `parquet` and `datafusion`. This matters practically: a single object-store instance, credential provider and byte-range cache can be shared across every layer of the system.

**The skew is chronic, not transient.** delta-rs `main` is already on DataFusion 55 and Arrow 59 but has not been released in roughly eleven weeks; the Iceberg equivalent trails one generation behind that. The architecture must therefore accommodate a permanently-lagging storage library rather than waiting for one release to resolve it.

**Two consequent reversals of earlier positions**, recorded because each was recommended and then withdrawn on evidence:

1. **From "track one major behind the query engine" to "track head."** The earlier caution was premised on depending on vendor table providers. Once SANKHYA owns the read path, being current *is* the benefit, and the lag has no compensating advantage. The cost of tracking head is real and must be budgeted — DataFusion 55 removed a method from the grouped-accumulator interface, so user-defined aggregate migration is a recurring maintenance item.
2. **From "the format choice is a headline architectural decision" to "the format is a pluggable metadata adapter."** Under `DEC-06` and `DEC-07`, the storage library supplies snapshot resolution and file lists and nothing else. Delta versus Iceberg becomes a reversible adapter choice rather than a foundational commitment — which is also the correct posture for a general-purpose engine. `DEC-10` therefore selects a day-one default rather than a permanent direction.

### 7.2 Verified constraints and defects

These are the findings that changed design decisions. Each carries its evidence.

| # | Finding | Evidence | Consequence |
|---|---|---|---|
| 1 | Released `deltalake-core` and `iceberg` both pin Arrow 58 / DataFusion 53.1, two majors behind head | crates.io dependency listings | `DEC-06`: own the table provider; format crates for metadata only |
| 2 | `delta_kernel` 0.27.1 has **no DataFusion dependency** and supports Arrow 59 | crates.io dependency listing | Makes `DEC-06` implementable today with zero version drag |
| 3 | delta-rs `main` already uses Arrow 59 / DataFusion 55 | Upstream `Cargo.toml` | The version wall is temporary; the release lags the fix |
| 4 | **delta-rs cannot write deletion vectors**; `merge`, `update` and `delete` are copy-on-write | delta-io/delta-rs#4512, open, 2026-06-03 | `DEC-07`: append-only landing, merge on read |
| 5 | **DataFusion 55 disables filter pushdown and filter reordering by default** | `branch-55` configuration source: `pushdown_filters: default = false`, `reorder_filters: default = false` | Enabled and asserted at startup. **Measured neutral (1.02×)** on a 5M-row, 523 MiB scan — the order-of-magnitude figure this table originally carried was not borne out, and the corrected measurement is recorded in `ARCHITECTURE.md` §8.6 |
| 6 | DataFusion 55 enables bloom filters on read by default | Same source: `bloom_filter_on_read: default = true` | No action required; verify it is not disabled |
| 7 | **Hash joins do not spill** in DataFusion | Upstream proposal, not implemented | `FR-QUERY-18`: admission control is mandatory |
| 8 | **`iceberg-rust` cannot compact**, and lacks row-level update, overwrite and conflict validation | Upstream status matrix and transaction-action listing | Settles the day-one format choice on capability, not on maturity argument |
| 9 | **delta-rs Z-order has an open row-duplication defect** | Upstream issue tracker | Z-order clustering SHALL NOT be used until resolved |
| 10 | **Enabling deletion vectors silently disables predicate pushdown** | Upstream behaviour | A second, independent reason to keep them off — and the finding most likely to be silently lost |
| 11 | delta-rs: a conflicting concurrent delete raises an error **but performs the delete anyway** | delta-io/delta-rs#2509, open since 2024-05-13 | Serialize all writes to a table through one leader; never rely on the library's concurrency control alone |
| 12 | Approximate percentile and distinct functions are sketch-based and **merge-order dependent** | DataFusion function documentation | `FR-QUERY-09`: reject at planning time when exactness is required |
| 13 | `tokio-postgres` and `sqlx` both lack logical replication support | Crate sources | `DEC-01`: own the decoder; vendor only the transport |
| 14 | `abi_stable` last released 2023-10-12 | crates.io | `FR-EXT-14`: native dynamic extensions rejected |
| 15 | `postgresql_embedded` spawns a child process; bundled binaries are dynamically linked | Crate documentation and build scripts | `DEC-02`; and a fully static single binary containing the database is not achievable |
| 16 | No Rust generator exists for two of the four public benchmark suites, and no Rust reference implementation exists for the graph suite | Ecosystem survey | Benchmark harness is a budgeted deliverable, not a script |

### 7.3 Open verification items

Each blocks a decision and carries an owner. **A requirement resting on an unverified claim is not ready to build.**

| # | Item | Blocks | Priority |
|---|---|---|---|
| 1 | Whether managed cloud PostgreSQL offerings preserve logical replication slots across failover | Any availability commitment in attached mode on managed cloud databases | **Highest** |
| 2 | Whether `iceberg-rust` pushes down decimal predicates | If not, every numeric range predicate degrades to a full scan, and numeric columns are decimals | High |
| 3 | Whether the Iceberg DataFusion integration surfaces distinct-value statistics | Join-ordering quality under that format | High |
| 4 | Whether delta-rs can write liquid-clustered tables | The strongest argument in Delta's favour evaporates if not | High |
| 5 | PostgreSQL minimum version claim regarding failover-capable slots | `DEC-02` version floor | High |
| 6 | Bundled database binary linkage and platform archive contents | Release checklist and platform matrix | Medium |
| 7 | Certified-cryptography posture of the chosen TLS stack | Any related procurement claim | Medium |
| 8 | Public dataset licensing for redistribution in continuous integration | Benchmark automation | Low |

### 7.4 Estimates versus measurements

The compression ratios, encoding effects and file-geometry figures in this document are **engineering planning estimates, not measurements on a representative workload**. The capacity tooling SHALL re-measure them on a sample per deployment. This document does not assert them as measured facts.

---

## 8. Risk register

Ordered by severity then likelihood. Every entry carries a mitigation and an early-warning signal, because a risk without an observable precursor cannot be managed.

| ID | Risk | Sev | Lik | Mitigation | Early warning |
|---|---|---|---|---|---|
| `RSK-01` | A runaway query starves the capture loop, retained log grows, the source database halts — **an analytical query takes down the transactional system** | Critical | High | Dedicated runtime with reserved cores; separate connection pools; escalation ladder; paging alert on retained bytes | Replication lag trending up; applier restart loop |
| `RSK-02` | Hand-built capture loses or duplicates data without the safety net of a mature framework | Critical | High | Position recorded inside commit metadata; slot advanced only after durable commit; deterministic simulation testing; continuous reconciliation | Any reconciliation discrepancy; non-monotonic applied position |
| `RSK-03` | A schema change silently corrupts published data | Critical | High | Event-trigger change log delivered in stream order; quarantine on incompatible change; refusal to apply an unrecognized shape | Quarantine events; unexpected relation metadata |
| `RSK-04` | Capture delete propagation erases archived data during a purge | Critical | High without action | Four independent layers: detach primitive, publication excludes deletes, applier tripwire, attestation | Applier tripwire firing at all |
| `RSK-05` | Purge completes but the published commit is lost — permanent loss of the system of record | Critical | Low | Strict ordering: commit, tag and verify before detach; quarantine grace; startup reconciliation | Registry and catalog disagreement |
| `RSK-06` | Type-fidelity loss discovered after purge — silent corruption of the record | Critical | Medium | Canonical encoding; pre-flight check at policy creation; digests over canonical encoding; quarantine window | Pre-flight failures; digest mismatch |
| `RSK-07` | Maintenance expires a snapshot referenced by an archive | Critical | Low | Registry-referenced snapshots structurally unexpirable | Expiry job touching a registry-referenced version |
| `RSK-08` | Row- or column-level security bypassed via the graph tier or a second resolution path | Critical | Medium | Single choke point; type-enforced security context; per-tenant hydration; negative tests with mutation testing | Any code path constructing a provider without a security context |
| `RSK-09` | Non-deterministic reduction makes outputs irreproducible | Critical | High without action | Deterministic reduction trees; fixed partition count recorded; compensated summation; reproducibility gate | Repeated identical queries returning differing values |
| `RSK-10` | An approximate aggregate reaches an output requiring exactness | Critical | Medium | Planning-time rejection; result watermarking; test suite | Approximate function in a plan flagged for exactness |
| `RSK-11` | Missing data silently improves an aggregate | Critical | Medium | Mandatory completeness measure with a hard threshold | Completeness ratio below threshold |
| `RSK-12` | Two processes open the same data directory, or an orphaned database process persists | Critical | Medium | Directory lock; orphan detection and adoption; parent-death signalling | Startup lock contention; unexpected process at boot |
| `RSK-13` | Dependency version divergence breaks the build or silently forfeits performance | Critical | Certain without action | Pinned workspace set; duplicate-version gate; upgrades as planned work | Duplicate-version gate output non-empty |
| `RSK-14` | The core is domain-agnostic in name only — neutral naming, domain-shaped abstractions | High | High | Two deliberately opposite reference packs against an unmodified core; conformance suite; naming lint; pack-free build | A reference pack's change touching core; a pack encoding domain types into a generic field |
| `RSK-15` | The extension API becomes a dumping ground or breaks packs on upgrade | High | High | Own function traits rather than re-exported engine traits; size budget; two-domain rule; no escape hatches; breaking-change gate | API approaching its budget; an open-ended map field proposed |
| `RSK-16` | Both storage libraries are pre-1.0 with frequent breaking changes | High | High | Format abstraction confines blast radius; exact pins; conformance suite detects semantic drift; named maintenance owner | Upstream migration guides; deprecation notices |
| `RSK-17` | Small-file accumulation degrades analytical performance until it becomes a crisis | High | High | Commit interval as an architectural parameter; tiered compaction cadence; file-count metric with alerting; time-to-impact reporting | File count per partition growing superlinearly |
| `RSK-18` | Freshness and scan-speed conflict discovered late | High | Medium | Tiered read path designed in the first milestone; both metrics tracked from the start | Commit interval being tuned down "temporarily" |
| `RSK-19` | Storage backend lacks atomic conditional write, making the commit protocol unsafe | High | Medium | Startup conformance probe; refusal to enter multi-writer mode | Probe failure; unusual storage endpoint |
| `RSK-20` | Query memory exhaustion terminates the process, taking the database with it | High | Medium | Admission control; per-tenant pools; counting allocator with a load-shedding brake; capped spill on separate storage | Memory high-water approaching the brake |
| `RSK-21` | Graph exceeds memory at realistic scale | High | High | Arrow-backed CSR; published per-edge-type budget; tenant partitioning; documented ceiling with graceful degradation | Hydration duration and residency trending up |
| `RSK-22` | Compaction cannot keep pace — the degradation is self-reinforcing | High | Medium | Backpressure beginning with a longer commit interval; escalating job class; admission shedding; time-to-impact alerting | Compaction debt rising while write rate is flat |
| `RSK-23` | A restored pre-purge backup resurrects purged rows | High | Medium | Tie-break rule prevents double counting; startup reconciliation; refusal on conflict; manifest delta reporting | Registry and catalog disagreement after restore |
| `RSK-24` | A destructive command run against the wrong environment | High | Medium | Server-reported environment in the prompt; cluster assertion required in automation | Assertion mismatches in logs |
| `RSK-25` | A scheduled policy defect selects far more than intended | Critical | Medium | Anomaly guard against trailing history; blast radius per run and per day; archive-only default | Anomaly guard trips |
| `RSK-26` | Automated approval degenerates into a standing purge licence | High | High without action | Plan digest binding, expiring and range-scoped | Digest rejections; repeated identical digests |
| `RSK-27` | Mutation against an archived range silently affects zero rows | High | High without action | Write-path guard raising a typed error | Guard firing |
| `RSK-28` | Immutability controls silently removed by a later storage policy change | High | Medium | Continuous control attestation; periodic attestation drill | Attestation check failing |
| `RSK-29` | Semantic drift between engines — the same query gives two answers | High | Medium | Cross-engine differential test suite; documented accepted-difference list | A difference not on the list |
| `RSK-30` | Tenant data leaks into logs, traces or metrics | Medium | Medium | Prohibition, lint, review checklist, pre-release log scrape | Any query text in standard output |
| `RSK-31` | Build times destroy the development loop and feedback quality | Medium | High | Minimal default features; compilation caching; heavy stages moved off the pull-request path; a hard time budget | Pipeline duration creeping past budget |
| `RSK-32` | Coverage theatre — high coverage, weak assertions | Medium | High | Mutation testing on critical crates; diff-coverage gate; review checklist | Coverage rising while mutation score falls |
| `RSK-33` | Scope growth from an ambitious remit | Medium | High | Milestones with explicit entry and exit criteria; documented non-goals; a demonstrable increment each milestone | A milestone with no demonstration |
| `RSK-34` | Documentation drifts from behaviour | Medium | High | Generated documentation with a staleness check; same-change requirement; definition-of-done item | Any generated-documentation difference |
| `RSK-35` | Rehydrated copies accumulate into a shadow system of record | Medium | Medium | Mandatory expiry on rehydration | Rehydration count and age |
| `RSK-36` | Third-party pack code escalates privilege or destabilizes the process | High | Medium | Sandboxed tier by default; native tier requires review and cannot be tenant-installed; adversarial pack in the test suite | Adversarial test failures |
| `RSK-37` | Regulatory and standards targets move | Medium | Certain | Versioned, configurable pack content; no hard-coded calibrations; quarterly re-verification of the ecosystem baseline | Quarterly review findings |

---

## 9. Acceptance

### 9.1 Definition of done

A requirement is satisfied when all of the following hold:

1. Implemented, with tests at the appropriate level for its layer.
2. Its test identifier exists, runs in continuous integration, and gates the build.
3. Documentation updated **in the same change**.
4. For performance requirements: benchmarked on the reference node with results recorded against the committed baseline.
5. For security requirements: a negative test proving the failure mode is prevented, not merely that the success path works.
6. For requirements with a stated failure mode: a fault-injection test asserting the **named** recovery behaviour.

### 9.2 Traceability

Every requirement traces forward to a design element in the architecture document, an implementation task in the plan, and a test identifier. Every test traces back to at least one requirement. Orphans in either direction are reported by an automated check and are defects in this specification, not in the code.

### 9.3 Review status

| Area | Status |
|---|---|
| Contradictions in the original brief | Resolved — `DEC-01` to `DEC-03` |
| General-purpose restructuring | Resolved — `DEC-04`, `DEC-05` |
| Dependency and version strategy | Resolved — `DEC-06` |
| Capture and storage write path | Resolved — `DEC-07`, `DEC-08`, `DEC-09` |
| Table format selection | Resolved for day one — `DEC-10`; four verification items open |
| Warehouse layout and interoperability | Resolved — `DEC-11` |
| Analytical correctness | Resolved — `DEC-12` |
| Graph semantics | Resolved — `DEC-13` |
| Distribution | Deferred with a stated trigger — `DEC-14` |
| Tiering | Resolved — `DEC-15`, `DEC-23`, `DEC-24`, `DEC-25` |
| Extensibility | Resolved — `DEC-16`, `DEC-17` |
| Scale to hundreds of terabytes | **Under review** — capacity model and scaling roadmap outstanding |
| Freshness architecture, simplified variant | **Under review** — `DEC-09`, pending retrofit analysis |

---

*This specification is maintained under version control. Amendments require a decision record. Requirement identifiers are permanent.*
