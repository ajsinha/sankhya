<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/wordmark-dice-dark.png">
  <img src="docs/assets/wordmark-dice.png" alt="SANKHYA — सांख्य" width="440">
</picture>

*To count is to make completely known.*

*A general-purpose unified OLTP + OLAP + Graph data server — one binary, written entirely in Rust.*

**One binary, three engines, one reckoning — that is the design. Today, one engine runs.**

[![License](https://img.shields.io/badge/license-Proprietary-red.svg)](LICENSE) [![Status](https://img.shields.io/badge/status-M0%2C%20M1%2C%20M3%2C%20M4%2C%20M7%2C%20M10%20complete%3B%20M2%2C%20M5%2C%20M6%2C%20M8%2C%20M13%20partial%3B%20M9%2C%20M14%2C%20M17%2C%20M18%20in%20progress-yellow.svg)](docs/STATUS.md) [![Rust](https://img.shields.io/badge/rust-1.97%2B-b7410e.svg)](https://www.rust-lang.org) [![JVM](https://img.shields.io/badge/JVM-none-success.svg)](#design-principles)

</div>

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

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

SANKHYA is a single self-contained Rust binary that is *designed* to give you three data
models over one governed copy of your data. The engine is **domain-agnostic**: it knows about
tables, rows, columns, edges, versions and tenants — and nothing else.

The table below is the design. The **Today** column is what a process you can start actually
does, and it is the column to read first.

| | Engine | Purpose | Today |
|---|---|---|---|
| **Transact** | PostgreSQL — supervised child process, or attached to an external cluster | The authoritative system of record **for managed tables**. Strict ACID, foreign keys, row-level locking, full audit. A table published directly to the open format by an external writer has no transactional half, and says so rather than pretending. | **Not built into the server.** See below |
| **Analyse** | Apache DataFusion over Arrow + an open lakehouse table format | Vectorized, SIMD-accelerated OLAP. Ad-hoc SQL, high-cardinality aggregation and multi-dimensional pivots over billions of rows. | **Runs.** This is the engine you get |
| **Relate** | In-memory graph engine over Arrow-backed adjacency | Network topology, k-hop traversal, cycle detection, weighted transitive closure, centrality and community detection — with time-respecting paths as a first-class primitive. | **A library, not a service.** Nothing hydrates a graph on a timer |

> **Not built: "embedded PostgreSQL", and the phrase itself was wrong.**
> `crates/sankhya-oltp-pg/src/lib.rs` refutes it in its own opening words: PostgreSQL *"is not
> linked into this binary and does not run inside this process. It is a **child process whose
> entire lifecycle SANKHYA owns**"* — `initdb`, start, readiness, health, shutdown — so that an
> operator sees one process tree and one artifact. That is a real and defensible claim;
> *embedded* in the literal sense is not, and `REQUIREMENTS.md` settles it as `DEC-02`.
>
> Today even the supervisor is unreachable. It is a **dev-dependency** of the server —
> `crates/sankhya-server/Cargo.toml` says so, with the comment *"nothing in the server wires it
> yet"* — `Settings` has no transactional configuration, and **the server never starts a
> database**. It is one of ten crates on the `UNREACHED` list in `xtask/src/surfaces.rs`, which
> is the mechanical, build-checked statement of exactly this.

Between the transactional and analytical halves sits a **native Rust change-data-capture
bridge**, designed to stream PostgreSQL's write-ahead log directly into versioned columnar
storage — no Kafka, no Debezium, no Connect cluster, **no JVM anywhere in the stack**.

> **Not built: the bridge has no runtime.** The `pgoutput` wire decoder, the apply path,
> reconciliation, idempotence, crash safety, schema evolution and the source-safety ladder are
> all built and tested — several of them against a real PostgreSQL 17.11 replication stream.
> What does not exist is the **driver that runs them on a timer**. `sankhya-cdc-pg`,
> `sankhya-cdc-apply`, `sankhya-cdc-model` and `sankhya-ingest` are not dependencies of
> `sankhya-server` at all; the server's only background loops are maintenance and file feeds.
> The audit records this as `ING-00`, *"there is no change-capture runtime"*.

```
                    ┌──────────────────────────────────────────┐
   writes ╌╌╌╌╌╌╌╌╌▷│   PostgreSQL — system of record (ACID)   │
                    └───────────────────┬──────────────────────┘
                                        ╎  native logical replication
                                        ╎  (pgoutput, in-process — no JVM)
                                        ▽
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

  ───▶  built, and reachable from a running server
  ╌╌╌▷  designed and tested, with no runtime that drives it
```

**Legend.** Solid edges carry data in a process you can start. **Dashed edges do not exist as
a running path**: nothing writes to a database this server supervises, and nothing decodes its
write-ahead log on a timer. Everything below the Delta Bridge is reached today by pointing the
server at a warehouse that something else published. Of the four surfaces in the box, two are
built — the PostgreSQL wire protocol and Arrow Flight SQL; the gRPC control plane is unbuilt
and the REST gateway is refused by design.

Everything is one process, one config file, one binary, one security model.

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

Nothing in the SANKHYA core knows what a trade, a shipment or a patient is. Domain semantics
arrive as **packs** — optional, versioned bundles that contribute schemas, aggregate functions,
graph algorithms, materialized views, detection rules and policy vocabulary through a stable
extension API. The engine stays general; the domain stays pluggable.

**Three packs exist, and none of them is a flagship.** They exist to test the extension API
rather than to serve an industry, and two of the three are deliberately *unlike* finance:

| Pack | What it is |
|---|---|
| `packs/pack-ref-telemetry` | Devices, readings, thresholds and windows. Scalar time-series, no graph. Its manifest says *"deliberately non-financial"* |
| `packs/pack-ref-logistics` | Shipments, depots and routes. Graph-heavy, and also *"deliberately non-financial"* |
| `packs/pack-adversarial` | A **hostile** pack. Every attempt it makes must be refused with a named error, so that "the boundary holds" is a test rather than an assertion |

That is the whole of it. If the core can serve two unrelated industries with no changes to the
core, and refuse a hostile pack by name, the general-purpose claim holds. That is the test.

> **Not built: the risk and financial-crime packs.** Earlier versions of this README described
> two "flagship packs" — a Risk pack with exact order statistics, and a Financial-crime pack
> with time-respecting paths and beneficial-ownership tracing — as shipping reference
> implementations. **Neither exists.** There is no `risk` pack and no AML pack anywhere in the
> repository. They are a **1.4** commitment answering `FR-EXT-16`; see
> [`docs/ROADMAP.md`](docs/ROADMAP.md) §1.4.
>
> This passage is called out rather than quietly deleted because it was the one place in this
> repository written to impress rather than to inform, and it was also the false one. The
> sentence that followed it — *"a toy domain pack deliberately unlike finance is built and
> tested in CI. If the core can serve it with no changes to the core, the general-purpose claim
> holds. That is the test, not the assertion"* — inverted reality exactly: the deliberately
> non-financial packs are the ones that exist, and the flagships were the assertion.

The **capabilities** those packs would be built from are real, and most are built and tested
today as engine primitives — which is the honest version of the claim:

| Domain concept | The general capability underneath | Built? |
|---|---|---|
| VaR / Expected Shortfall | Exact order statistics with a declared interpolation convention | Yes — three conventions, shown to disagree at the 99th percentile |
| Scenario P&L vectors | Fixed-size numeric list columns with element-wise aggregation, where reduction order is semantically significant | Yes — every reduction bit-deterministic under permutation |
| Ultimate beneficial ownership | Weighted transitive closure with multiplicative edge weights, cycle tolerance and a pruning threshold | Yes, as a graph primitive — but nothing hydrates a graph on a timer |
| Laundering-chain detection | Time-respecting path traversal | Yes, as a graph primitive — same caveat |
| Structuring / smurfing | Windowed pattern matching over a time-ordered edge stream | Yes, as a graph primitive — same caveat |
| Risk coverage checks | Data-completeness measures attached to any aggregate | Yes — every cube row carries the completeness it was computed under |

The same primitives serve supply-chain tier-N dependency tracing, telecom fraud, healthcare
claims, IoT telemetry, logistics and retail analytics — because they were never really about
finance.

---

## Open storage, readable by everyone

The analytical tier is not a black box. Tables live in an open lakehouse format in a layout
that mirrors your operational schema, so **OLTP and OLAP names line up**:

```
warehouse/
  sales/                    # the OLTP schema name
    orders/                 # one self-contained folder per table
    customers/
  telemetry/
    device_readings/
```

SANKHYA writes the Delta transaction log itself, so these tables are readable with no SANKHYA
process in the path.

**What is actually tested is one external reader.** The `delta_kernel` crate — a
*dev-dependency*, used as an independent oracle rather than as a library this product ships —
reads tables this system wrote and is required to agree about the schema, the version and the
live file set, including across a compaction where four superseded fragments are still on disk.
It reads a checkpoint written by hand and is proven to *use* it rather than tolerate it: the
commits the checkpoint covers are deleted and the table still resolves. The tests are
`crates/sankhya-table-delta/tests/oracle.rs` and
`crates/sankhya-maintenance/tests/kernel_oracle.rs`, and one of them asserts that the kernel
reads all 1,000 rows rather than merely listing the files.

Spark, Trino, DuckDB, Snowflake and Athena read the Delta format, and nothing in this
repository executes any of them. An earlier version of this section said those five engines
*"read these tables **directly**"*, which stated a compatibility conclusion as a tested result.
The honest claim is narrower and still worth something: **an independent implementation of the
format reads what this system writes, on every build, and disagreeing with it fails the gate.**

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

**M0, M1, M3, M4, M7 and M10 are complete; M2 and M13 are substantially built; M5 closed on four of five exit criteria, M6 on six of seven, and M8 on six of eight with its scale-out half moved to M12 for want of a second machine; M9 is built and demonstrated with its gate deliberately held for M11; and M14, M17 and M18 are in progress.** What runs today is the analytical half — a server `psql` connects to, authenticates against and queries over real Parquet, with cubes, clones, named snapshots, maintenance on a timer, Arrow Flight SQL and a hash-chained audit. What does not run is everything upstream of it: no change-capture runtime, no transactional tier wired into the server, and no second node — so of the three engines, one runs, one is a library nothing hydrates, and one is a supervised process nothing starts.

[`docs/STATUS.md`](docs/STATUS.md) leads with a one-screen *what works today* and holds the **single canonical inventory of what is not built**, with the evidence for each entry; every other document links there rather than keeping its own copy.

---

## Measured, and what the measurement actually asserts

This repository's habit is to gate a claim rather than publish it, and where that has not
happened the number below says so. The distinction matters: a **gated** figure fails the build
when it regresses; a **reported** figure is a transcript of one run on one machine.

**Analytical performance.** The three objectives are asserted by a build gate against a
**budget**, at TPC-H scale factor 1, on twelve cores where the requirements name a
thirty-two-core reference node:

| Objective | Budget, asserted | Observed, reported |
|---|---|---|
| `NFR-PERF-02` needle lookup, 8 clients | 250 ms | 13 ms |
| `NFR-PERF-03` pivot, 8 clients | 1,000 ms | 796 ms |
| `NFR-PERF-04` wide scan, 4 clients | 3,000 ms | 648 ms |

The budgets are asserted in `crates/sankhya-olap/tests/tpch.rs`, in a test named
*the performance objectives are met*, and driven by `cargo xtask check-performance`. **The
observed figures are not asserted by anything** — they come from a separate measurement test
marked *"a measurement, not an assertion"*, whose only assertion is that it found any rows at
all. And `check-performance` is deliberately **not** part of `check-all`, which is what CI
runs — so the budget gate does not run in GitHub CI. Two further TPC-H queries are published
without being gated at all, and [`docs/STATUS.md`](docs/STATUS.md) says which and why.

**Concurrency.** Every concurrency claim is taken twice in the same run on the same machine —
once as the code stands, once with the same work forced through one mutex — because a single
lock over the warehouse satisfies every *safety* property while destroying concurrency itself.
The **floors are asserted**: commit scaling ≥ 3.0× and ≥ 2× the serialized control; the loaded
reader holding ≥ 0.4 of its idle rate with a p99 within 4× of idle, while the lock-sharing
control's p99 is ≥ 20× worse. The **figures usually quoted** — 4.8× against a serialized 0.91×,
a reader holding 0.59–0.80 with a p99 of 227 µs against a control at 0.00–0.07 waiting
seconds — are printed outputs of particular runs, transcribed. They are evidence, not gates.

**Soak.** A forty-five-minute run at twenty gigabytes passes, with 1.16 billion rows scanned
and 21.5 GB reclaimed. Resident memory was **not flat**, and this README said it was: it rose
from 1,223 MB at t+478s to 2,458 MB at t+2671s and settled at 2,271 MB, inside its bound
throughout. The claim worth keeping is the one [`docs/SOAK.md`](docs/SOAK.md) actually makes —
that this lands within 25 MB of the ten-gigabyte run's steady figure at **double** the data, so
the working set is bounded by the machinery rather than by the dataset. The multi-day run is
not done, and moved to M12.

---

## Audited

Twelve independent production-readiness audits reported **129 findings** in
[`docs/AUDIT_REPORT.md`](docs/AUDIT_REPORT.md), verdict *not production-ready*. The count is a
**lower bound**, and the report says so.

They are not all open. [`docs/REMEDIATION.md`](docs/REMEDIATION.md) sequences the repair, and
**Phases 0, 1 and 2 are done** — the status lies, the missing CI and the silent-green gate, and
then the whole of the data-loss tier: the kernel now reads what compaction wrote, `fsync`
exists on both halves, four ways to answer *"nothing reads this"* when something did are
closed, a data-file name is used once, one server per warehouse is enforced. Phases 3 through 6
— wrong answers, security, operability, and closing the gap between what is claimed and what is
true — have each partly landed; this rewrite is an item in Phase 6.

The verdict paragraph is worth reading in full, because it is the most useful sentence in this
repository: *the reason is not the defect count — it is that a green gate did not see any of
this.* 2,807 tests, 741 mutations and twenty checks, against silent data loss on three <!-- figures-as-measured-then -->
production paths, a door with no lock, and a summation kernel that returned zero for a real
number. The tests were not absent. They were calling the code differently from the way
production calls it.

---

## Start here

Thirteen documents. There is no separate book; it was deleted in September 2026 because it
duplicated these and was the stale copy wherever the two disagreed.

| Document | What it covers |
|---|---|
| [`docs/QUICKSTART.md`](docs/QUICKSTART.md) | Build it, start it, query it — every transcript reproducible from one fixture |
| [`docs/TUTORIALS.md`](docs/TUTORIALS.md) | Five tutorials, in order, from the first hour onwards. Every example executed by a test |
| [`docs/GUIDE.md`](docs/GUIDE.md) | Every feature by worked example, each one executed by a test |
| [`docs/GLOSSARY.md`](docs/GLOSSARY.md) | Milestone numbers, finding codes, and the terms this repository coins |
| [`docs/STATUS.md`](docs/STATUS.md) | What works today; the canonical list of what does not; and what broke along the way |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | What each release is *for*, gated on exit criteria rather than dates |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | System architecture, crate decomposition, consistency and security models |
| [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) | The amended, traceable functional and non-functional requirements |
| [`docs/TESTING.md`](docs/TESTING.md) | What is verified, how, and what is not verified |
| [`docs/DEVELOPING.md`](docs/DEVELOPING.md) | Working in the repository: layout, gates, and the rules the build enforces |
| [`docs/AUDIT_REPORT.md`](docs/AUDIT_REPORT.md) | Twelve production-readiness audits, 129 findings |
| [`docs/REMEDIATION.md`](docs/REMEDIATION.md) | The sequenced plan that closes them, and what has landed |
| [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) | Every third-party package, its licence, and the upstream `NOTICE` files |

Reference material, generated from the code and checked against it on every build:

| Document | What it covers |
|---|---|
| [`docs/METRICS.md`](docs/METRICS.md) | Every exported metric, its unit, its cardinality bound, and whether it pages |
| [`docs/ERRORS.md`](docs/ERRORS.md) | Every error code, its class, and what to do about it |
| [`docs/PLATFORMS.md`](docs/PLATFORMS.md) | Where the server runs, what a build must satisfy, and where only a client does |
| [`docs/VERSIONS.md`](docs/VERSIONS.md) | The four version axes, every on-disk format, and whether an upgrade can be undone |
| [`docs/FUNCTIONS.md`](docs/GUIDE.md) | Every built-in function, where each is reachable from, and what is still planned |
| [`docs/POSTGRES.md`](docs/POSTGRES.md) | Exactly what SANKHYA changes about PostgreSQL, and what it will never do to it |
| [`docs/SOAK.md`](docs/SOAK.md) | The long-run method, its results, and four attempts' worth of what it taught |
| [`docs/INVARIANTS.md`](docs/TESTING.md) | What the build gate enforces, and §6: what it only *intends* |
| [`docs/runbooks/`](docs/runbooks/) | One per alert that can page — enforced, not aspirational |
| [`docs/adr/`](docs/adr/) | Architecture decision records, including what each one deliberately leaves open |
| [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) | Milestones, work breakdown, sizing and acceptance gates |
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
