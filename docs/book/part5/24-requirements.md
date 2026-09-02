# Requirements

> This chapter distils the requirement catalogue: what SANKHYA must do, organised by area,
> with what is met and what is not. Its central claim is that a requirement is only a
> requirement if it can be failed — so every non-functional requirement carries a test
> identifier, every performance figure names its seven conditions, and a target without
> conditions is excluded rather than softened. The catalogue holds twelve constraints,
> twenty-five decision records, roughly two hundred and forty functional requirements across
> eleven areas, and thirty-seven risks. The identifiers are kept here, because they are the
> traceability.

## 24.1 How the document is written

`REQUIREMENTS.md` is an amended successor to the original brief, produced after four
independent reviewers — a systems architect, a database-internals specialist, an OLAP and
graph-engine specialist and a senior Rust engineer — wrote critiques of it. Every ecosystem
claim was verified against upstream sources on a stated date; anything unverifiable is marked
`[UNVERIFIED]` with an owner and a deadline, because **a requirement resting on an unverified
claim is not ready to build**.

| Rule | Consequence |
|---|---|
| Identifiers are stable and permanent | Never reused or renumbered; a withdrawn requirement is marked `WITHDRAWN` and retained |
| RFC 2119 keywords | MUST/SHALL mandatory; SHOULD requires written justification to deviate |
| `NFR-META-01` — every NFR carries a test identifier | *An NFR without one is not a requirement; it is an aspiration* |
| Every performance figure names seven conditions | Benchmark query, hardware, dataset and scale, cache state, concurrency, preconditions, test id |
| `NFR-META-02` — a violated precondition degrades predictably and observably | Silent misses are prohibited; the reason appears in response metadata and metrics |

The condition rule was written against two figures in the original brief that failed for
opposite reasons. *"Sub-50 ms for graph traversals under 100,000 nodes"* is met by any competent
implementation on day one and constrains nothing. *"Sub-second for complex analytical group-bys
over tens of millions of rows"* is undefined — complex how, cold or warm, what selectivity, what
hardware — and therefore cannot be failed.

One paragraph in §0.3 governs everything else:

> **SANKHYA is a general-purpose data engine. Risk management, financial crime detection and
> counterparty analytics are *use cases*, delivered as optional domain packs. They are not the
> system.**

The framing is tested rather than asserted — by `FR-EXT-09` (two reference packs from unrelated
non-financial industries, whose change must touch zero core files) and by the domain-vocabulary
gate of Chapter 22.

> **Key idea**
> `DEC-04` gives the test a reviewer can apply in seconds: *if a parameter's name contains a
> domain noun, it belongs in a pack.* `threshold: Decimal` is core; `control_threshold_pct` is
> not.

## 24.2 Constraints and non-goals

| ID | Constraint |
|---|---|
| `CON-01`–`CON-03` | Rust; **no JVM anywhere**; single binary and single configuration file, with no broker, connector cluster, scheduler or external coordinator |
| `CON-04`–`CON-05` | Arrow as the universal in-memory format; PostgreSQL authoritative for all mutations |
| `CON-06`–`CON-07` | No source file beyond 1,500 code lines; modular crates and extensive test coverage |
| `CON-08`–`CON-09` | Analytical storage directly readable by external engines including Spark; analytical naming relatable to operational naming |
| `CON-10`–`CON-11` | The architecture is general-purpose; the analytical tier **may** lag by a few seconds — a relaxation, and load-bearing for `DEC-08` |
| `CON-12` | Proprietary; dependency licences remain in force |

`CON-03` needed a decision to survive contact with reality. `DEC-03` states the honest form:
**there is no configuration in which N nodes share writable state with zero coordination** —
either the object store is the coordinator, through atomic conditional writes, or PostgreSQL is.
`CON-03` is therefore honoured in *packaging*, not in *topology*, and the coordinator is a role
of the same binary.

Twelve non-goals (`NG-01`–`NG-12`) bound the other side, because a specification without
non-goals grows without bound: not a distributed OLTP database, not a durable graph database,
not a stream processor, no cross-region active-active writes, no multi-source capture in v1, no
valuation libraries, no case-management UI, no Windows *server* support, no distributed query
execution in v1, no exact betweenness at scale, no bespoke graph query language.

## 24.3 The functional catalogue

| Area | Count | Governs | State |
|---|---|---|---|
| `FR-OLTP` | 19 + 5 | Transactional tier, managed and attached modes, external tables, the date axis | Met; date axis **not** on the ingest path |
| `FR-CDC` | 26 | Native logical replication, backfill, schema evolution, source safety | Contracts met; no slot driver, no backfill reader, no streaming transport |
| `FR-STORE` | 27 | Table format, warehouse layout, external readability, compaction, reclamation | Met, less partitioning on ingest, deletion vectors, log cleanup |
| `FR-QUERY` | 29 | Statistics, exactness, determinism, caching, provenance | Met, less bounded-memory order statistics, the exactness gate and admission |
| `FR-CUBE` | 28 | Dimensions, hierarchies, measures, materialisation | Met, after a retraction |
| `FR-GRAPH` | 21 | Typed temporal graph, epochs, traversal bounds, primitives | Correct; **performance objectives not met and not claimed** |
| `FR-TIER` | 35 | Lifecycle tiering with purge, verification, quarantine, evidence | Built and demonstrated; **gate not cleared** |
| `FR-API` | 17 | Four surfaces, read modes, error catalogue, snapshot tokens | Two of four surfaces |
| `FR-SEC` | 24 | Tenancy, policy, audit, encryption, erasure, the extension boundary | Met, less federated identity |
| `FR-OPS` | 28 | Startup, shutdown, health, backup, diagnostics, maintenance | Met; the packaging baseline currently fails |
| `FR-EXT` | 16 | The extension API, three pack tiers, reference packs | Met; the pack loader is M4's remainder |

The rest of this section keeps the requirements that decide the system's character — the ones
that would change what SANKHYA *is* if removed.

### The transactional tier

`FR-OLTP-02` was **amended during implementation**, and the amendment is instructive: the
original wording made every table managed, which would force a terabyte backfill row by row
through a transactional store to produce Parquet a loader could have written directly. The
amendment scopes the guarantee to tables that want it and adds a family for **external tables**
(`02a`–`02g`), declared in the table's own log rather than in configuration so that two nodes
cannot disagree and a restart cannot forget, read-only, with a write **refused** rather than
accepted into a non-authoritative tier.

`FR-OLTP-02e` explains why a publishing library is mandatory: `CON-08`'s openness is about
*reading*. A bad reader is wrong for itself and recoverably. **A bad writer corrupts the table
for everyone, permanently and undetectably — as happened to this system, writing its own format
with the specification open.**

The date-axis block requires `sank_data_date` of type `DATE` on every analytical table,
partitioned on it, declared per table and never defaulted per row, with `sank_` reserved as a
column prefix and attached tables never altered. **Met on the batch publish path and not on the
streaming arrival path, which is where most data lands** — `sankhya-ingest` writes flat, and
there is no timestamp to derive a date from, because `_sankhya_commit_ts` is written as literal
`0` for every row.

### Capture, and the two named invariants

`DEC-22` elevates two invariants above ordinary requirements:

> **INV-1 (Query safety).** No query can cause the transactional primary to lose availability or
> durability.
> **INV-2 (Source safety).** SANKHYA can never bloat, wedge or exhaust the storage of the
> PostgreSQL instance it replicates from — including through its own replication slot, its own
> long-running queries, or its own maintenance.

The mechanism is specific. A logical slot retains write-ahead log from its restart position; if
the consumer stalls — starved of CPU by a runaway analytical query — PostgreSQL retains log
until the filesystem fills and then shuts down. **An analytical query takes down the
transactional system**, which was unmentioned in the original brief. `FR-CDC-20`–`26` answer it:
a dedicated runtime with **reserved** cores rather than prioritised ones, because priority
schemes fail under sustained saturation; lag monitored in seconds *and* retained bytes, with the
paging alert on the byte measure; and an escalation ladder whose thresholds are ordered so
**SANKHYA degrades on its own terms before PostgreSQL invalidates the slot** — an invalidated
slot cannot be resumed and forces a full re-snapshot of every replicated table.

Two more shape the pipeline. `FR-CDC-04`: an unchanged TOASTed value is absent from the change
record, and writing nulls over real data is silent corruption. `FR-CDC-16`: an incompatible
schema change **quarantines** the table with a named error and an explicit operator verb,
because intent cannot be inferred — a dropped column may mean *stop capturing* or *erase from
history*, and guessing is either data loss or a compliance breach.

### Storage

`FR-STORE-14`–`16` define the open-storage claim: readable by Spark, Trino and DuckDB, with the
protocol versions declared; **verified by an automated test** that writes with SANKHYA, reads
with an independent engine and asserts identical results; and archived data written in a
conservative Parquet profile with the format version recorded, because a seven-year retention
obligation means the files must be readable in seven years by something other than SANKHYA.

Two reclamation rules reappear in Chapter 23's incident record. `FR-STORE-21`: **compaction only
adds files; a separate later job removes them**, and only files unreferenced by any retained
snapshot, not under a live lease, and older than the maximum of the retention horizon, the
longest permitted query and the lease TTL. `FR-STORE-22`: the orphan threshold must exceed the
maximum commit duration including retries — *getting this wrong deletes live data, and the
failure is silent until the affected partition is queried.* And `FR-STORE-24` settles a conflict
rather than a mechanism: **maintenance backs off; the applier never does.**

### Query

Six requirements carry the correctness position, and all six exist because the alternative is a
number that is wrong and looks right.

- **`FR-QUERY-03`** — pushdown returns **inexact** classification unless row-exact filtering is
  guaranteed. Claiming exactness when only files are pruned is a silent wrong-answer defect.
- **`FR-QUERY-06`** — entitlement predicates are injected by an analyzer rule **and**
  independently asserted by the provider, which fails if the predicate is absent.
- **`FR-QUERY-09`** — approximate aggregates are rejected at planning time when the session
  requires exactness, and results computed with them are watermarked.
- **`FR-QUERY-10`** — deterministic reduction: fixed partition count recorded with the result,
  partials merged in ascending index, compensated summation.
- **`FR-QUERY-13`** — a completeness measure attachable to any aggregate, with a threshold below
  which the query **fails** rather than returning a flattering number.
- **`FR-QUERY-22`** — the plan cache key includes the policy bundle version. Omitting it means a
  revocation does not take effect for any query with a cached plan, which is *a data breach with
  a passing test suite*.

`FR-QUERY-20` is the property the materialisation design rests on: files are immutable and every
key embeds a snapshot identifier, so **a new commit cannot produce a stale hit — the key simply
misses**. `FR-QUERY-21` names the one exception, the table-to-latest-version mapping, and
requires it to carry a TTL and to be identified as the only mutable key.

Three are **not met**, and stated as such. `FR-QUERY-08` asks for exact order statistics in
bounded memory; what exists is exact and buffers its input — exact and bounded are independent
properties, and only the first is delivered. `check_exactness` has no caller, so `FR-QUERY-09`'s
gate is not wired into a session. And `FR-QUERY-18`'s admission control is built, tested, and
called by nothing.

### Cubes

`FR-CUBE` was added by owner directive after the observation that `FR-QUERY-14` covers SQL
`GROUP BY CUBE` and `ROLLUP`, which are *grouping constructs, not a cube*: no dimension, no
hierarchy, no declared measure, no consolidation rule. Four requirements will be argued with:

- **`FR-CUBE-03`** — every measure declares its rule for every dimension, or is refused at
  definition time.
- **`FR-CUBE-12`** — an aggregate is computed only over rows the principal may read, so two
  principals may legitimately see different totals for the same cell.
- **`FR-CUBE-20`** — bit-identical results with materialisation on and off, tested by comparing
  bits rather than within a tolerance.
- **`FR-CUBE-21`/`22`** — answering from a materialised **ancestor** is permitted only where the
  measure is additive along every dimension being further rolled up, and a semi-additive measure
  only along the dimensions it is additive over. Named as *the engine's principal
  silent-wrong-answer surface*, and property-tested against the base-data answer.

### Graph

`FR-GRAPH-03` could not have been retrofitted: edges carry validity intervals, traversal
supports a **time-respecting** mode, and **static traversal is not exposed for flow analysis**.
`DEC-13` gives the reason — plain BFS reports `A → B → C` even when `B → C` precedes `A → B`, and
for flow of value or goods such a path is physically impossible, so static search produces
overwhelming false positives. One layout decision pays three times: CSR sorted by `(source,
timestamp)` makes *"outgoing edges of v after t"* a binary search plus a slice, is the same sort
order giving the best data skipping for the corresponding table, and eliminates the dominant cost
of hydration. And `FR-GRAPH-14` states the rule Chapter 23 keeps meeting in other forms: **a
truncated result must never be mistakable for an absence of results.**

### Tiering

Thirty-five requirements, and the tightest gate in the document. `FR-TIER-03` makes the
authorization **structural**: the purge state machine's entry point requires a value whose only
constructors are the command path and the schedule evaluator, so *enumerating those constructors
constitutes a complete audit of every way data can leave the system of record.* `FR-TIER-15` is
one sentence and is the whole posture: **there shall be no flag that skips verification.**
`FR-TIER-29` is the control nobody plans for: compare each scheduled run's candidate set against
the schedule's history and **halt for re-approval if it exceeds the trailing median by a
configured factor**, which is what catches a clock error or a mis-edited policy *before* it
archives years of data in one pass. `FR-TIER-31` states the recommended production configuration
outright: scheduled runs default to stopping **before** purge — continuous archive and
verification is the valuable half of tiering at none of the risk.

All eleven M9 work items are built and the exit criteria demonstrated, including nineteen refusal
paths shown to fail closed. **The gate is not cleared**: criterion 3 needs an attestation drill
against a real non-production archive, which cannot be produced from development, and destructive
purge stays disabled until M11 clears it.

### API, security, operations, extensibility

`FR-API-02` names the wire protocol *the highest-adoption-value surface in the product*, and
`FR-API-03` makes the acceptance gate a **tool-compatibility matrix** — the difference between "a
client connects" and "a BI tool works". `FR-API-13` is the requirement whose absence would be
noticed immediately: a write returns a session token carrying its commit position, and passing it
to a later query guarantees visibility by waiting. *Without this the first demonstration anyone
attempts shows their own write missing.* **Two surfaces of four are built**; `pg_dump` is
recorded as *failing* rather than omitted.

`FR-SEC-02` is enforced by the type system: **it shall be impossible to reach a table scan
without a security context**, because the catalogue's resolution function takes one and there is
no other constructor. `FR-SEC-06` requires the row predicate's presence to be asserted **in the
final physical plan**, which is what caught the defect where it was offered to a provider that
declined it. `FR-SEC-08` applies **mutation testing to the policy component**, because a
surviving mutant is a test passing for the wrong reason, *which here is a data breach with a
green build.* And `FR-SEC-23` states what most systems would obscure: external engines reading
the warehouse directly **bypass row- and column-level enforcement**, with the compensating
controls named — storage-level access control as the real boundary, per-tenant prefixes and
scoped credentials, column encryption, and a published-versus-private classification.
`FR-SEC-03`'s federated identity is **not built**.

`FR-OPS-02` requires a first-time user to reach a running server, sample data and a successful
query **within five minutes**, as a timed test so it cannot rot. `FR-OPS-07` makes the drain
order a correctness property, including persisting the applied position **strictly after** the
commit is durable. `FR-OPS-09` — readiness accounts for replication lag and **liveness shall
not**, or a lagging pipeline makes an orchestrator kill a healthy node in a restart loop.
`FR-OPS-17` — the diagnostic reports **time until a problem becomes user-visible**: *"compaction
debt is 400 GB"* is far less actionable than *"at the current write rate, query latency on this
table will double in about nine days."*

`FR-EXT-05` is a feedback mechanism disguised as a restriction: a pack depends on a strictly
limited set of core crates, and needing another **fails the build** — that failure *is* the signal
the API has a gap, and it is always an API design task, never grounds to widen the allowance.
`FR-EXT-09` is the acceptance test for the general-purpose claim and it is mechanical: **the
change that adds a reference pack must touch zero core files.** `FR-EXT-11` states the limit of
the naming lint inside the requirement: it catches **leakage, not shape** — the lint catches
naming, the reference packs catch shape.

## 24.4 Non-functional requirements

### The performance model, published so targets can be re-derived

Parquet decode runs 0.8–2.0 GB/s per core, with strings and nested types three to five times
worse. Hash aggregation on an integer key runs 50–150 M rows/s per core, dropping to 10–30 M for
string keys. Object storage first-byte latency is 20–50 ms against local NVMe's ~100 µs. CSR
traversal runs 200 M–1 G edges/s per core.

Two conclusions shape the architecture. The object-store penalty is a **latency ratio of
200–500×, not a bandwidth ratio**, so the local cache is the highest-return optimisation and
request coalescing matters more than raw throughput. And there is a **crossover**: on the
thirty-two-core reference node, decode capacity is roughly 48 GB/s decoded, about 14 GB/s of
compressed input. Below it the system is I/O-bound and clever kernels buy nothing; above it,
encoding choice matters. Object storage sits far below; page cache sits far above.

### The service-level objectives

| ID | Class | Target | State |
|---|---|---|---|
| `NFR-PERF-02` | Selective needle lookup, warm, 8 concurrent | p95 < 250 ms | **Met at 13 ms**, by statistics pruning alone |
| `NFR-PERF-03` | Multi-dimensional pivot, warm, pruned | p95 < 1 s | **Met at 796 ms** |
| `NFR-PERF-04` | Wide scan, warm, local cache | p95 < 3 s | **Met at 648 ms** |
| `NFR-PERF-05` | Cold scan from object storage | p95 < 15 s | Explicitly *not* sub-second, and stated as such |
| `NFR-PERF-09`–`14` | Graph traversal, hydration, incremental update | 50 ms to bounded-and-published | **Not met and not claimed** |
| `NFR-PERF-16` | Cancellation, including inside sandboxed user code | Within 200 ms | Met, bounded at one batch per partition |

`cargo xtask check-performance` enforces the first three and fails the build. It was once
recorded as *failing*, and the correction is in Chapter 23: the numbers were real and the
**mapping** was wrong. `NFR-PERF-02` says selective needle lookup and was being measured against
a query applying three range predicates across every file in the table. `NFR-PERF-03` has a
stated precondition that a partition predicate be present, so the six-way join being measured
against it is now published and deliberately **not gated**.

> **Pitfall**
> An approximate mapping used as a gate is a gate on the wrong thing, in whichever direction it
> errs. The honest form of the earlier statement was never *"the system is too slow"*; it was
> *"these objectives have never been tested."*

### Reliability, and the definition that replaced a slogan

`NFR-REL-03` replaces *"zero data loss"* with something measurable:

> For every committed source transaction with commit LSN `L`, once the pipeline reports
> `applied_lsn ≥ L`, a read of the published table at the corresponding version returns exactly
> the rows a read of the source at snapshot `L` would return — no missing rows, no duplicates, no
> stale values.

`NFR-REL-04` makes it a continuously measured metric alerting on any nonzero discrepancy — *it is
a metric, not a design claim.* `NFR-REL-05` requires comparison against an **independent
expected-state model maintained by the harness**, never a second query against the source, or a
bug in the source reader hides a bug in the pipeline. `NFR-REL-06` names a trap worth the space:
the per-row digest must be combined with an order-independent but **duplicate-sensitive**
operator — wrapping addition, not XOR — because XOR cancels duplicate pairs and would conceal
precisely the at-least-once duplication bug the harness exists to catch. And `NFR-REL-11` requires
three times to be recorded and never conflated: **business event time, source commit time,
publication time.**

### Quality, buildability and process

These twenty-two requirements are what Chapters 22 and 23 implement.

| ID | Requirement |
|---|---|
| `NFR-QUAL-01` | Coverage as a **ratchet against the baseline rather than a cliff**, plus per-change diff coverage |
| `NFR-QUAL-02` | **Mutation testing** on the numeric, protocol-decoding, apply-logic and policy crates. Coverage is necessary and not sufficient |
| `NFR-QUAL-06` | A **deterministic mode** in which the same scenario twice produces byte-identical metadata and output — one test, enormous coverage: it detects iteration order leaking into results, wall-clock in metadata, unsorted listings and non-deterministic reduction |
| `NFR-QUAL-08` | Semantic differences between the two engines enumerated in a documented, tested list; anything not on the list is a defect |
| `NFR-QUAL-09` | The pull-request pipeline completes within twenty minutes — beyond that engineers stop reading it and every downstream gate becomes theatre |
| `NFR-QUAL-10` | Duplicate versions of the critical dependency families **fail the build** — a correctness gate, not hygiene |
| `NFR-QUAL-13` | Typed errors in library crates; every variant classifiable into a retry-or-fail taxonomy, proven by an exhaustive test |
| `NFR-QUAL-19` | Documentation updated **in the same change** as the behaviour. A behaviour change without a documentation change is incomplete |

`NFR-QUAL-07` — clock and identifier generation injected everywhere and enforced by lint — is
**not met**, and it is the enabling requirement for `NFR-QUAL-06`. That is exactly the property
`sankhya-ports`' own header claimed the workspace had, and the reason that crate is marked for
deletion.

## 24.5 Risks, acceptance, and defects in the catalogue

Thirty-seven risks, each with a mitigation and an **early-warning signal**, because a risk
without an observable precursor cannot be managed. Two of the signals are worth memorising:
`RSK-32`, coverage theatre, announces itself as **coverage rising while mutation score falls**;
`RSK-33`, scope growth, as **a milestone with no demonstration**. Two risks are rated *certain
without action*: non-deterministic reduction making outputs irreproducible, and dependency
version divergence.

`§9.1` defines done as six conditions holding together, three of which are why this book has a
Chapter 23: the requirement's **test identifier exists, runs in CI, and gates the build**;
documentation is updated in the same change; and for a security requirement there is **a negative
test proving the failure mode is prevented**, not merely that the success path works. `§9.2`
requires two-way traceability with **orphans in either direction reported by an automated check
and treated as defects in the specification, not in the code** — a check that does not exist yet.

Finally, the same honesty rule applies to the requirements document as to everything it governs.

| Defect | Substance |
|---|---|
| A live identifier collision | `FR-STORE-20`–`24` are defined **twice** with different content — the date-axis block in §5.1 and the compaction and reclamation block in §5.3 — while §0.2 states identifiers are permanent and never reused |
| A cited requirement that does not exist | §0.3 names `NFR-EXT-01` as the domain-leakage gate; no `NFR-EXT-*` requirement appears in §6 |
| A mis-citation | §5.5 cites `FR-OLTP-02a` for the date axis; that is the external-tables requirement |
| Date skew | The header is dated 2026-08-26; the body carries amendments from 08-27, 08-28 and 08-29 |

Four verification items also remain open, each blocking a decision: whether managed cloud
PostgreSQL preserves logical replication slots across failover; whether `iceberg-rust` pushes
down decimal predicates; whether the Iceberg DataFusion integration surfaces NDV statistics; and
whether delta-rs can write liquid-clustered tables. The PostgreSQL minimum-version claim about
failover-capable slots is itself marked `[UNVERIFIED]`.

> **Key idea**
> §7.4 keeps the rest of the numbers honest: the compression ratios, encoding effects and
> file-geometry figures in the document are **engineering planning estimates, not measurements on
> a representative workload**, and the document says so rather than asserting them as measured
> facts.
