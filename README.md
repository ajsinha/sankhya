<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/wordmark-dice-dark.png">
  <img src="docs/assets/wordmark-dice.png" alt="SANKHYA — सांख्य" width="440">
</picture>

*To count is to make completely known.*

**One binary. Three engines. One reckoning.**

*A general-purpose unified OLTP + OLAP + Graph data server — one binary, written entirely in Rust.*

[![License](https://img.shields.io/badge/license-Proprietary-red.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-M0--M8%2C%20M10%20and%20M13%20complete%2C%20M9%2C%20M14%2C%20M17%20and%20M18%20in%20progress-yellow.svg)](docs/STATUS.md)
[![Rust](https://img.shields.io/badge/rust-1.97%2B-b7410e.svg)](https://www.rust-lang.org)
[![JVM](https://img.shields.io/badge/JVM-none-success.svg)](#design-principles)

</div>

---

## The name

In Sanskrit, **संख्या** *(saṅkhyā)* means *number* — but the root says far more than the translation. It is **सम्** *(sam,* "together, completely"*)* bound to the verb **√ख्या** *(khyā,* "to make known, to declare, to reckon"*)*. To enumerate a thing, in Sanskrit, is **to make it completely known**. Counting is not bookkeeping. Counting is how a thing becomes knowable.

From that root comes **सांख्य** *(Sāṅkhya)*, the oldest of the six *darśanas* of Indian philosophy — the school of **enumeration**. Sāṅkhya takes the bewildering plurality of experience and resolves it into twenty-five ordered *tattvas*, so that the whole may be understood through its parts. And its central discipline is **विवेक** *(viveka)* — **discrimination**: the art of telling the observer from the observed, the essential from the merely manifest.

That is this system's thesis, stated twenty-five centuries early.

> **सांख्ययोगौ पृथग्बालाः प्रवदन्ति न पण्डिताः**
> *"Only the childish call analysis and action two different things — not the wise."*
> — Bhagavad Gītā 5.4

An enterprise's data is a plurality: records, events, entities and the relationships between them — ledgers and trades, or shipments and suppliers, or devices and readings, or claims and providers. The industry's standard answer is to shatter it further — an OLTP database here, an analytical cluster there, a graph store somewhere else; three copies, three lags, three security models, three versions of the truth, and a small army employed to reconcile them.

SANKHYA takes the opposite view. **The enumeration should be a single act.** One runtime holds the transaction, the aggregate and the network in one address space over one copy of the data — so that booking a trade, aggregating an exposure and discerning a laundering ring are three faces of one reckoning, not three systems arguing at month-end.

And the second meaning matters as much as the first. A number alone is not the point. Knowing *which* number, *as of when*, and *why* — that is the point. SANKHYA is built for the kind of work where the gap between the 99th percentile and an *approximation* of the 99th percentile is the difference between an answer and a finding.

**Sankhya: to count, and to discern.**

---

## What it is

SANKHYA is a single self-contained Rust binary that gives you three data models over one governed copy of your data. The engine is **domain-agnostic**: it knows about tables, rows, columns, edges, versions and tenants — and nothing else.

| | Engine | Purpose |
|---|---|---|
| **Transact** | PostgreSQL — embedded in-process or external | The authoritative system of record **for managed tables**. Strict ACID, foreign keys, row-level locking, full audit. A table published directly to the open format by an external writer has no transactional half, and says so rather than pretending. |
| **Analyse** | Apache DataFusion over Arrow + an open lakehouse table format | Vectorized, SIMD-accelerated OLAP. Ad-hoc SQL, high-cardinality aggregation and multi-dimensional pivots over billions of rows. |
| **Relate** | In-memory graph engine over Arrow-backed adjacency | Network topology, k-hop traversal, cycle detection, weighted transitive closure, centrality and community detection — with time-respecting paths as a first-class primitive. |

Between them sits a **native Rust change-data-capture bridge** that streams PostgreSQL's write-ahead log directly into versioned columnar storage — no Kafka, no Debezium, no Connect cluster, **no JVM anywhere in the stack**.

Everything is one process, one config file, one binary, one security model.

```
                    ┌──────────────────────────────────────────┐
   writes ─────────▶│   PostgreSQL — system of record (ACID)   │
                    └───────────────────┬──────────────────────┘
                                        │  native logical replication
                                        │  (pgoutput, in-process — no JVM)
                                        ▼
                    ┌──────────────────────────────────────────┐
                    │  Delta Bridge  ──▶  arrival buffer (hot)  │
                    │                ──▶  lakehouse tables      │
                    │        versioned columnar · time travel   │
                    └───────┬──────────────────────┬───────────┘
                            │                      │
                   Arrow ◀──┘                      └──▶ Arrow
                            ▼                      ▼
                ┌────────────────────┐  ┌────────────────────────┐
                │  DataFusion (OLAP) │◀▶│   Graph engine (typed) │
                └────────────────────┘  └────────────────────────┘
                            └───────────┬──────────┘
                                        ▼
                        ╔══════════════════════════════════╗
                        ║      SANKHYA unified runtime     ║
                        ║  Flight SQL · gRPC · REST · psql ║
                        ╚══════════════════════════════════╝
                            │            │             │
                       Risk desks   AML compliance   Quants / BI
```

---

## Why it exists

Every large financial institution runs the same expensive tragedy:

- **The lag.** Risk sees a position hours after the trade is booked. AML sees a wire after the money has left.
- **The copies.** The same trade lives in Oracle, in the warehouse, in the graph store, in six extracts. They disagree, and reconciling them is a permanent cost centre.
- **The fragmentation of control.** Three engines means three permission models, three audit trails, and a compliance answer that starts with "well, it depends which system you ask."
- **The tax.** JVM analytical clusters, brokers, connectors, schedulers — an operational estate that dwarfs the problem it solves.

SANKHYA's bet is that the modern Rust data stack — Arrow, DataFusion, the lakehouse formats, PostgreSQL logical replication — has finally made it possible to collapse that estate into **one deployable artifact** without giving up ACID, without giving up sub-second analytics, and without giving up the audit trail a regulator will ask for.

---

## Domain packs, not a hardcoded domain

Nothing in the SANKHYA core knows what a trade, a shipment or a patient is. Domain semantics arrive as **packs** — optional, versioned bundles that contribute schemas, aggregate functions, graph algorithms, materialized views, detection rules and policy vocabulary through a stable extension API. The engine stays general; the domain stays pluggable.

Two flagship packs ship as reference implementations, and they exist as much to *prove the extension API is real* as to serve their industries:

- **Risk** — scenario vectors and sensitivities pivoted across any hierarchy, with **exact** order statistics. Quantiles aggregate the way risk actually composes: sum the vectors, *then* take the percentile — never sum the percentiles.
- **Financial crime** — transaction networks scored against historical baselines, with time-respecting paths (a chain that runs backwards in time is not a chain), structuring detection, and beneficial-ownership tracing with materiality thresholds.

The same primitives underneath serve supply-chain tier-N dependency tracing, telecom fraud, healthcare claims, IoT telemetry, logistics and retail analytics — because they were never really about finance:

| Domain concept | The general capability underneath |
|---|---|
| VaR / Expected Shortfall | Exact order statistics with a declared interpolation convention |
| Scenario P&L vectors | Fixed-size numeric list columns with element-wise aggregation, where reduction order is semantically significant |
| Ultimate beneficial ownership | Weighted transitive closure with multiplicative edge weights, cycle tolerance and a pruning threshold |
| Laundering-chain detection | Time-respecting path traversal |
| Structuring / smurfing | Windowed pattern matching over a time-ordered edge stream |
| Risk coverage checks | Data-completeness measures attached to any aggregate |

A "toy domain" pack deliberately unlike finance is built and tested in CI. If the core can serve it with no changes to the core, the general-purpose claim holds. That is the test, not the assertion.

---

## Open storage, readable by everyone

The analytical tier is not a black box. Tables live in an open lakehouse format in a layout that mirrors your operational schema, so **OLTP and OLAP names line up**:

```
warehouse/
  sales/                    # the OLTP schema name
    orders/                 # one self-contained folder per table
    customers/
  telemetry/
    device_readings/
```

Spark, Trino, DuckDB, Snowflake and Athena read these tables **directly**, with no SANKHYA process in the path. You are not buying another silo — you are buying an engine that happens to also be a good citizen of the lakehouse you already have.

---

## Design principles

1. **One binary.** No broker, no connector, no cluster manager, no JVM. If it needs a sidecar, it is not done.
2. **One copy of the truth.** PostgreSQL is authoritative for mutations; everything downstream is a derived, versioned, reproducible view of it.
3. **Correct beats fast, then be fast anyway.** Money is fixed-point decimal, never `f64`. Regulatory quantiles are exact, never t-digest approximations. Then we make it vectorized.
4. **Time is a first-class axis.** Every table is queryable *as of* a version, an LSN, or a wall-clock instant. "What did we know on the 14th?" is a query, not an archaeology project.
5. **Small, sharp crates.** A modular Cargo workspace with a strict dependency DAG and a hard ceiling of **1,500 lines per source file** (excluding comments), mechanically enforced in CI.
6. **Tested to the point of boredom.** Unit, property-based, snapshot, SQL-logic, integration, fault-injection and performance-regression gates — plus reconciliation tests that *prove* the zero-data-loss claim rather than asserting it.
7. **Documentation ships with the code.** Docs are updated in the same commit as the change they describe. A pull request that changes behaviour and not the docs is incomplete.

---

## Status

**Implementation — M0 through M8, M10 and M13 complete; M9, M14, M17 and M18 in progress.** M8 closed on
six of its eight exit criteria; the two that need a second machine, and the scale-out work behind
them, moved to M12. M9's eleven work items are built and its exit criteria demonstrated, and
**its gate is deliberately not cleared** — see below. **M10 — zero-copy cloning — is complete**:
its design gate was cleared by [ADR-0016](docs/adr/0016-zero-copy-cloning.md) before any code was
written, and all five exit criteria are met. **M13 — config-driven ingest from files — is
complete**: a YAML declaration names the source, the shape of what arrives and where it lands; a
record that does not fit is quarantined whole into a table with a mandatory expiry, under
[ADR-0018](docs/adr/0018-a-record-that-does-not-fit.md); and a feed that halts is visible and
resumable from a client rather than only from a log line. **M14 — the client contract and the
Python SDK — is in progress**: the contract is decided in
[ADR-0017](docs/adr/0017-the-client-contract.md), and transport security is built on both doors. The architecture and requirements were reviewed and amended by a panel covering
systems architecture, database internals, analytical query engines and Rust engineering
practice.

What works today, all of it exercised by tests rather than by a running process: a
`pgoutput` wire decoder validated against a real PostgreSQL 17.11 stream, an apply path
whose transaction invariant is property-tested, lossless type mapping, capture that
reconciles against its source and survives a crash at any point, an **open table log**
that the Delta kernel reads — so the open-storage claim is tested rather than asserted —
**compaction** that plans, merges, commits and converges without changing an answer, a
**maintenance scheduler** that arbitrates it against the machine budget, and a **table
provider** that plans from metadata alone, prunes files by statistics, resolves updated
and deleted rows to one current version each, and answers from memory and Parquet at once
or refuses when the tiers do not cover the query.

The **graph tier** holds no durable state: an epoch is hydrated by scanning published
tables, carries the snapshot it was built from, and is dropped on shutdown. There is no
graph write path, so the graph cannot disagree with SQL — an edge exists because a row
exists. Traversal is time-aware and bounded, and the algorithms are callable from SQL as
table functions that join against ordinary tables. The **extension mechanism** defines its
own function traits rather than re-exporting the query engine's, so a pack survives the
engine changing underneath it; two reference packs from unrelated industries and one
deliberately hostile pack, whose every attempt is refused with a named error, are what
test that claim rather than assert it.

The analytical tier is measured against TPC-H at scale factor 1, and the three
performance objectives are **asserted by a build gate** rather than reported: a needle
lookup at 13 ms against a 250 ms budget, a pivot at 796 ms against 1 s, a wide scan at
648 ms against 3 s — on twelve cores, where the requirements name a thirty-two-core
reference node. Cancellation is bounded, a hostile aggregation is refused rather than
taking the process down, and the places where this engine and PostgreSQL disagree are
enumerated in a test — which found three ways the analytical tier returns a wrong number.

**The server runs and answers queries.** Real `psql` connects, authenticates, and runs
ordinary SQL — aggregation, expressions, null semantics — against a provider wrapped in its
policy decision — over real Parquet on disk, through the read path that plans from the
table log alone and prunes files by recorded statistics. A table the caller may not read is
never registered, so naming it fails to resolve rather than confirming it exists; a policy
row predicate is enforced where no provider can decline it. Everything it did is recorded in
a tamper-evident hash chain.

**Arrow Flight SQL** streams results as Arrow batches over gRPC, so a bulk extract stays
columnar from the Parquet page to the client's buffer — the wire protocol is a row protocol
and converts at the last step, which is the whole cost of a large extract.

**It can be operated.** `sankhya-server doctor` reports *when* a problem becomes
user-visible rather than its current value — and refuses to invent a date it cannot support,
which on a first run means saying so. `backup` binds the transactional backup, the table
versions and the key generation to one consistent point and refuses to record an
inconsistency; `drill` proves that backup restores by reading the data back and recomputing
its digest, because a file-presence check passes on every failure that actually happens. A
`/metrics` endpoint exports a catalogue where recording requires passing the declaration, so
an undeclared metric is unrepresentable and no label can carry tenant data. Every error a
client sees carries a permanent code and the catalogue's own remediation.

**Cubes are a declared model rather than a `GROUP BY` convention.** A cube knows which
columns are dimensions, which are measures, and — the part that decides whether an answer is
correct — **how each measure may be combined along each dimension**. Summing a closing balance
across twelve months gives a number of the right magnitude, the right sign and no meaning; a
cube refuses it. Roll-up and slice are SQL table functions with no cube-build step preceding
the query — dice and drill-down are the two navigations still to come — every row carries the
completeness it was computed under, and `CREATE CUBE` / `DROP CUBE` are statements a client can
send.

**Concurrency is measured against a control rather than asserted.** Every concurrency claim is
taken twice in the same run on the same machine — once as the code stands, once with the same
work forced through one mutex — because a single lock over the warehouse satisfies every
*safety* property while destroying concurrency itself. Commits to eight tables run at 4.8×
one table's rate where the serialized control runs at 0.91×; a reader under four writers holds
0.59–0.80 of its idle rate with a p99 of 227 µs, where the control holds 0.00–0.07 and waits
seconds. A forty-five-minute soak at twenty gigabytes passes with resident memory flat.

What does not exist: the streaming transport, the gRPC control plane and the REST gateway.
**Tiering is M9 and still gated.** `sankhya-tiering` is no longer empty: the policy model,
the resumable purge state machine, exhaustive verification, the archival registry, the
four-layer defence against a propagated delete, cross-tier query unification, quarantine,
rehydration, whole-table migration and the command and schedule surfaces are all built, and the
exit criteria are demonstrated end to end. **Destructive purge against a system of record is
disabled until M11 regardless** — building the purge path and arming it are two decisions, and
the gate's remaining criterion needs an attestation drill run against a real non-production
archive, which development cannot produce. Also unbuilt inside work
already counted: bloom filters, table
partitioning, the result cache, leader election, a timer that drives graph hydration, and
a measured graph benchmark — the graph primitives are correct against brute force and
bounded by construction, but they have not been timed at scale, and that M4 criterion is
carried forward as unmet rather than reinterpreted. [`docs/GUIDE.md`](docs/GUIDE.md), [`docs/QUICKSTART.md`](docs/QUICKSTART.md) and [`docs/STATUS.md`](docs/STATUS.md)
are explicit about the boundary, including the defects found along the way — and about
the two TPC-H queries whose numbers are published without being gated, and why.

Start here:

| Document | What it covers |
|---|---|
| [`docs/QUICKSTART.md`](docs/QUICKSTART.md) | Build it, load ten gigabytes, watch capture reconcile — and what does not work yet |
| [`docs/tutorials/`](docs/tutorials/) | Hands-on, in order — start here. Every example executed by a test |
| [`docs/GUIDE.md`](docs/GUIDE.md) | Every feature by worked example, each one executed by a test |
| [`docs/STATUS.md`](docs/STATUS.md) | What is actually built today, what is not, and what broke along the way |
| [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) | The amended, traceable functional and non-functional requirements |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | System architecture, crate decomposition, consistency and security models |
| [`docs/POSTGRES.md`](docs/POSTGRES.md) | Exactly what SANKHYA changes about PostgreSQL, and what it will never do to it |
| [`docs/FUNCTIONS.md`](docs/FUNCTIONS.md) | Every built-in function, where each is reachable from, and what is still planned |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | What each release is *for*, gated on exit criteria rather than dates |

Generated from the code, and checked against it on every build:

| Document | What it covers |
|---|---|
| [`docs/METRICS.md`](docs/METRICS.md) | Every exported metric, its unit, its cardinality bound, and whether it pages |
| [`docs/ERRORS.md`](docs/ERRORS.md) | Every error code, its class, and what to do about it |
| [`docs/PLATFORMS.md`](docs/PLATFORMS.md) | Where the server runs, what a build must satisfy, and where only a client does |
| [`docs/VERSIONS.md`](docs/VERSIONS.md) | The four version axes, every on-disk format, and whether an upgrade can be undone |
| [`docs/SOAK.md`](docs/SOAK.md) | The long-run method, its results, and four attempts' worth of what it taught |
| [`docs/runbooks/`](docs/runbooks/) | One per alert that can page — enforced, not aspirational |
| [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) | Milestones, work breakdown, sizing and acceptance gates |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Release themes and the capability timeline |
| [`docs/initial_reqmt.docx`](docs/initial_reqmt.docx) | The original brief, preserved for provenance |

---

## License

**Proprietary and confidential.** Copyright (c) 2026 Ashutosh Sinha
<ajsinha@gmail.com>. All rights reserved. This software is **not** open source.

No right to use, copy, modify or distribute it is granted except under an express written
authorisation from the copyright holder. It is provided "as is", with no warranty of any
kind, and the copyright holder accepts no responsibility for any consequence of using it.
See [LICENSE](LICENSE) for the full terms, including the warranty disclaimer and the
limitation of liability.

Third-party dependencies remain under their own licences, which this licence does not
displace.

<div align="center">

*"To count is to make completely known."*

</div>
