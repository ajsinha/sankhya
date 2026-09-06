<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Glossary

**Document ID:** SNK-GL-001
**Version:** 0.1.0
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Date:** 2026-09-06

---

## Why this document exists

This repository coins vocabulary and then uses it before defining it. *Epoch*, *cuboid*,
*live set* and *arrival buffer* each appear in prose dozens of pages before anything says
what they are, and two of them — *shape* and *window* — carry **two** unrelated meanings
in different crates, which is worse than an undefined term because a reader who guesses
right once will guess wrong later.

Milestone numbers and finding codes have the same problem in a sharper form: they are
identifiers, so they look like they are defined somewhere, and until now they were not.

Each entry below names where the term is *used in code*, so that a definition which drifts
from the implementation can be caught by reading one file rather than by trusting this one.

---

## 1. Milestones

A milestone is a unit of the engineering plan in
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md), gated on **exit criteria** rather than
on a date. It is not a release: releases are themes in [`ROADMAP.md`](ROADMAP.md), and one
release can span several milestones or none. The authoritative state of every milestone is
the table at the top of [`STATUS.md`](STATUS.md), which is also what
`cargo run -p xtask -- check-docs` parses to decide whether every document's `**Status:**`
line is stale — see `xtask/src/status.rs`, `unfinished_milestones`.

Milestone numbers are **allocation order, not execution order**. M13, M17 and M21 were
added by owner directive after M9 had started and are being built ahead of M11, M12, M15
and M16. A higher number does not mean later.

| | Name | State, in one word |
|---|---|---|
| **M0** | Foundations, spikes, walking skeleton | Complete |
| **M1** | Zero-configuration sync and read-your-own-writes | Complete |
| **M2** | Ingest correctness and durability | Substantially built |
| **M3** | Query engine and storage performance | Complete |
| **M4** | Graph engine and the extension mechanism | Complete |
| **M5** | Tenancy, security and API surfaces | Closed on four of five |
| **M6** | Operability, packaging and hardening | Closed on six of seven |
| **M7** | Multidimensional analysis — cubes, slice, dice, roll-up | Complete |
| **M8** | Concurrency and data safety | Complete on six of eight |
| **M9** | Tiering | Built and demonstrated; gate held |
| **M10** | Zero-copy cloning | Complete |
| **M11** | Production reconciliation | Not schedulable by development |
| **M12** | Scale-out, HA, disaster recovery, production-like acceptance | Needs a second machine |
| **M13** | Config-driven ingest, from files | Substantially built |
| **M14** | The client contract and the Python SDK | In progress |
| **M15** | Ingest without a file | Not started |
| **M16** | The Java and Rust SDKs | Not started |
| **M17** | Named snapshots | In progress |
| **M18** | Derived results | In progress |
| **M19** | The data lifecycle policy | Not started |
| **M20** | What changed between two versions | Not started |
| **M21** | The built-in function catalogue | Design gate met |

Two of these mean something specific and are easy to misread. **M11 is not late** — it is
*unschedulable*, because its criteria require a production deployment that does not exist;
it holds M9's remaining gate criterion and the decision to arm destructive purge, and no
amount of development work advances it. **M12 is not deferred work** — it is the set of
criteria that need a **second machine**, which this project has one of; M8's criteria 7 and
8 moved there whole rather than being reinterpreted into something one host could pass.

---

## 2. Finding codes

A finding code identifies one defect found by the twelve production-readiness audits
consolidated in [`AUDIT_REPORT.md`](AUDIT_REPORT.md). Codes are **stable and permanent**, so
that [`REMEDIATION.md`](REMEDIATION.md) can sequence `COR-08` without restating it and a
later document can cite it without copying its text.

The prefix names the **audit axis** that found it, not the severity and not the subsystem.
Severity is expressed by which *tier* of `AUDIT_REPORT.md` the finding sits in — Tier 0 is
silent data loss, Tier 11 is the first-run experience — so a `SEC-` finding can appear in
Tier 1 and another in Tier 4.

| Prefix | Axis | Codes in the report | First finding |
|---|---|---:|---|
| `COR-` | Correctness and data safety | 19 | `COR-01` A clone's pins never reach its origin's sweeper |
| `SEC-` | Security, authorization and disclosure | 18 | `SEC-01` No password is ever verified |
| `OPS-` | Operability and failure modes | 21 | `OPS-09` Backup manifests pin nothing |
| `CLM-` | Claims versus reality | 18 | `CLM-21` There is no CI |
| `RUN-` | First-run experience | 15 | `RUN-01` Every entry point dies |
| `ING-` | Ingest, change capture and reconciliation | 12 | `ING-00` There is no change-capture runtime |
| `FMT-` | Format evolution and upgrade safety | 9 | `FMT-01` A schema change is adopted in memory and never written to the log |
| `FEA-` | Feature completeness against intent | 8 | `FEA-01` There is no write path |
| `PERF-` | Performance claims and benchmark integrity | 7 | `PERF-01` The 24.9× wrapper claim does not reproduce — measured 2.2× |

> **The counts above sum to 127, and the report is headed "129 findings".** Both numbers are
> stated as found: 127 is how many distinct codes appear anywhere in `AUDIT_REPORT.md`, and 129
> is what that document and [`REMEDIATION.md`](REMEDIATION.md) both head-line. Only 102 of the
> codes carry a section heading of their own; the rest are cited inside another finding's prose.
> The discrepancy is not resolved here because counting cannot resolve it — and the report says
> the total is **a lower bound** in any case, so a reader should treat all three numbers as
> "about a hundred and thirty" rather than as an inventory.

Three conventions worth knowing before reading a finding. **`CONFIRMED` means an auditor
traced it to the load-bearing lines; `EXECUTED` means it was run and the wrong output
observed; `SUSPECTED` means the mechanism looks wrong and could not be fully verified** —
and `VERIFIED BY LEAD` marks the fifteen checked by hand before publishing, on the grounds
that a wrong critical finding costs more than a missed one. Findings are grouped by *what
happens if they are not fixed*, so two auditors finding the same thing independently share
one identifier and the corroboration is recorded, because it is evidence. And the count is
**a lower bound**: `REMEDIATION.md` says so itself.

> **`DEC-` is not a finding code.** It appears nowhere in `AUDIT_REPORT.md`. `DEC-01`
> through `DEC-25` are **decisions** recorded in [`REQUIREMENTS.md`](REQUIREMENTS.md) —
> settled questions that would otherwise have produced a specification nobody could
> implement. `DEC-02` is the one that settles what "embedded PostgreSQL" means, quoted at
> the top of `crates/sankhya-oltp-pg/src/lib.rs`; `DEC-23` is the one quarantine expiry gets
> no exception from. They are cited alongside finding codes often enough that the collision
> is worth stating: a `DEC-` number is a thing that was *decided*, not a thing that is
> *wrong*.

---

## 3. Coined terms

### epoch

An **epoch** is an immutable graph, hydrated by scanning published tables and frozen at the
snapshot it was built from — `crates/sankhya-graph/src/epoch.rs`. It is immutable for a
specific reason: every query holds a reference to the adjacency arrays and reads them
*without a lock*, which is only sound because nothing can modify them. A hydration builds a
**new** epoch and swaps a pointer; readers on the old one keep reading it until they finish,
and the reference count is what frees it. So a long traversal never has the ground moved
under it, and a rebuild never waits for one to end.

Every epoch carries its source snapshot and the lag at build time, and every result reports
them, because a graph derived from tables is always *as of* something — a result that does
not say which snapshot it came from cannot be reconciled against a relational result taken
at a different moment. Note the word is also used in its ordinary Unix sense throughout
`crates/sankhya-tiering/` ("microseconds from the epoch"); those are timestamps and have
nothing to do with graphs.

> **Not built:** nothing hydrates an epoch on a timer. The five `graph_*` SQL functions
> exist and refuse by name until something does. See [`STATUS.md`](STATUS.md).

### cuboid

A **cuboid** is one materialised aggregation of a cube — the cube's measures grouped along
some subset of its dimensions. The point of the term is that cuboids form a lattice, and a
query for a cuboid nobody materialised can often be answered by aggregating a **finer** one:
`crates/sankhya-cube-algo/src/ancestor.rs` decides exactly when, which is what makes one
stored cuboid answer many queries rather than one. A row returned by a cube function says
whether it came from a cuboid or from the base data, so the saving is visible rather than
assumed.

### arrival buffer

The **arrival buffer** — called the *arrival tier* in code,
`crates/sankhya-table-memory/src/lib.rs` — holds recently applied changes in Arrow form,
in memory, until a durable tier covers them. It exists because publication is batched: a
Parquet file per transaction would produce exactly the small-file pathology compaction exists
to clean up, but a query issued between two publications must still see the writes in
between, or the system has read-your-own-writes-*eventually*, which is a different and much
weaker promise.

One rule governs it, and it is the opposite of a cache's: **a segment may be released only
once a durable tier covers it** — not when it is old, not when memory is tight, not when it
has been read. Any other rule can open a gap in the middle of the coverage interval, and a
gap makes the read path refuse the query rather than answer it short. So when memory runs
short and nothing is releasable, the tier refuses new work and names publication as the
cause, instead of evicting and taking a miss. There is no backing store to miss *to* until
publication has happened.

> **Not built:** what exists is the retention contract — coverage, per-row filtering at the
> durable frontier, and the release rule. The epoch ring, the per-epoch key digests and the
> per-tenant sub-caps that `ARCHITECTURE.md` §5.4 describes do not exist, and nothing wires
> the tier into the ingest path. See [`STATUS.md`](STATUS.md).

### live set

The **live set** of a table is the set of data files that are current at a given version —
what you get by replaying the table log's adds and removes up to that point, rather than by
listing the directory. `crates/sankhya-table-delta/src/log.rs` computes it, and
`crates/sankhya-table-delta/src/cache.rs` keeps the last one it computed so a warm process
pays for what changed rather than for the whole history.

The distinction between *the live set* and *the files on disk* is load-bearing, not
pedantic. Compaction merges four fragments into one and commits a version where only the
merged file is live — the four superseded files are **still on disk**, because a reader
holding the older version still resolves through them. That is why reclamation is a separate,
guarded operation, why a snapshot or a clone can keep a file alive that no current version
names, and why a directory listing is not an answer to "what is in this table".

### snapshot versus version

A **version** is one table's position in its own log: a monotonic integer per table,
produced by one commit, resolvable by `SET VERSION OF <table> = <n>`. A **snapshot** is a
*name for one instant across many tables* — `crates/sankhya-snapshot/src/lib.rs` — holding
a version per table and the day it stops being honoured, and nothing else. No rows, no
schema, no files.

The two are not the same thing at different scales, and the distinction is the whole reason
snapshots exist. A calculation that reads a population of records, a set of rates, a set of
curves and the hierarchy they roll up through must read **all of them as of one instant**,
or the reconciliation problem this system exists to remove reappears *inside a single query*:
four tables, four moments, one number that reconciles to nothing. A snapshot that pinned each
table at whatever version it happened to reach would be a clone with extra steps.

Two nearby terms are distinct again. A **clone** freezes a *thing* — one table at one
version, appearing in the catalogue under its own name. A snapshot freezes a *moment*, and
is quoted by a query (`SET SNAPSHOT`) rather than queried. And a table created *after* a
snapshot is **refused** when read through it, rather than answered as empty, because a table
that did not exist is not a table that was empty. See
[ADR-0019](adr/0019-named-snapshots.md).

### `sank_data_date`

`sank_data_date` is the one date axis every analytical table carries: a column of type
`DATE`, declared per table and never defaulted per row —
`crates/sankhya-schema/src/datedate.rs`. It is the **business** date of a record, not the
moment it arrived, and the difference is the point: if the two are conflated, then
`WHERE sank_data_date = '2024-03-01'` returns a mixture of rows meaning "this happened on
the first" and rows meaning "we heard about this on the first", which is a wrong answer that
looks like a right one.

One guaranteed column is what lets partitioning, retention and tiering be written **once**
rather than per table, and it is written in the Hive convention
(`sank_data_date=2024-03-01`) that external readers already parse.

> **Not built:** the declaration exists and directory partitioning does not — the streaming
> arrival path writes flat. This is why `NFR-PERF-03`'s partition-predicate precondition
> cannot be satisfied by any query today. See [`STATUS.md`](STATUS.md).

### shape

**Shape** means two unrelated things, and both are in daily use.

In the analytical tier it is a **matrix's dimensions**. A matrix column is stored flat, as a
`FixedSizeList<Float64, rows × columns>`, so the shape has to come from somewhere: it comes
from Arrow's canonical `arrow.fixed_shape_tensor` field metadata, whose value is
`{"shape":[rows,columns]}` — `crates/sankhya-olap/src/matrices.rs`. A column with no shape
metadata is **refused rather than assumed square**, because that guess is wrong for every
rectangular matrix and produces numbers from values that were never in the same row. Shape
is part of a matrix's *type*, which is why `mat_of`'s dimensions must be literals and a wrong
element count is refused when the query is planned rather than partway through a scan.

In ingest (M13) it is **the structure a feed declares that arriving records must have** — the
middle clause of a feed declaration, which names the source, the shape of what arrives, and
where it lands. A record that does not fit the shape is quarantined whole under
[ADR-0018](adr/0018-a-record-that-does-not-fit.md), rather than being coerced.

### window

**Window** also means two things.

Over a series column it is the **span of elements a rolling statistic is computed across** —
`crates/sankhya-functions/src/multi.rs`, where a rolling statistic takes a series and a
window in and returns a series out, and a window that could not be filled carries a hole
rather than a number.

In graph traversal it is a **time bound on a path**. A time-respecting path is one whose
edges occur in non-decreasing time; a window constrains how much time may pass between
consecutive edges — `crates/sankhya-graph-algo/src/traverse.rs`. The two senses share no
code and no type, and a sentence using the word without saying which tier it is about is
ambiguous.

---

## Where to go next

| Document | What it covers |
|---|---|
| [`STATUS.md`](STATUS.md) | What works today, and the single canonical list of what is not built |
| [`QUICKSTART.md`](QUICKSTART.md) | The on-ramp: build it, start it, query it |
| [`GUIDE.md`](GUIDE.md) | Every feature by worked example |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Where each of these terms sits in the design |
| [`AUDIT_REPORT.md`](AUDIT_REPORT.md) | Every finding, by code |
| [`ROADMAP.md`](ROADMAP.md) | What each release is for |

---

*This glossary is maintained under version control. A term that acquires a second meaning
should acquire a second paragraph here in the same commit.*
