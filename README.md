<div align="center">

# SANKHYA

### सांख्य

**One binary. Three engines. One reckoning.**

*A general-purpose unified OLTP + OLAP + Graph data server — one binary, written entirely in Rust.*

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-design%20phase-orange.svg)](docs/ROADMAP.md)
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
| **Transact** | PostgreSQL — embedded in-process or external | The authoritative system of record. Strict ACID, foreign keys, row-level locking, full audit. |
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
                │  DataFusion (OLAP) │◀▶│   Graph engine (AML)   │
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

**Early implementation.** The architecture and requirements were reviewed and amended by a panel covering systems architecture, database internals, analytical query engines and Rust engineering practice. Foundations are now built and under test; the analytical engine is not.

What works today, all of it exercised by tests rather than by a running process: a
verified dependency set that compiles, a `pgoutput` wire decoder validated against a real
PostgreSQL 17.11 stream, an apply path whose transaction invariant is property-tested,
lossless type mapping, mirror naming that refuses collisions rather than disambiguating
them, capture that reconciles against its source and survives a crash at any point,
an **open table log** that the Delta kernel reads — so the open-storage claim is tested
rather than asserted — **compaction** that plans, merges, commits and converges without
changing a single answer, a **maintenance scheduler** that arbitrates it against the
machine budget, and a **tiered read** that answers one SQL statement from memory and
Parquet at once, or refuses when the tiers do not cover the query.

What does not: the server itself, the streaming transport, the table provider, the
statistics catalogue, the graph engine, the API surfaces, tenancy and security. The
correctness contracts are built; the machinery that runs them continuously is not.
[`docs/QUICKSTART.md`](docs/QUICKSTART.md) and [`docs/STATUS.md`](docs/STATUS.md) are
explicit about the boundary, including the defects found along the way.

Start here:

| Document | What it covers |
|---|---|
| [`docs/QUICKSTART.md`](docs/QUICKSTART.md) | Build it, load ten gigabytes, watch capture reconcile — and what does not work yet |
| [`docs/STATUS.md`](docs/STATUS.md) | What is actually built today, what is not, and what broke along the way |
| [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) | The amended, traceable functional and non-functional requirements |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | System architecture, crate decomposition, consistency and security models |
| [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) | Milestones, work breakdown, sizing and acceptance gates |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Release themes and the capability timeline |
| [`docs/initial_reqmt.docx`](docs/initial_reqmt.docx) | The original brief, preserved for provenance |

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

<div align="center">

*"To count is to make completely known."*

</div>
