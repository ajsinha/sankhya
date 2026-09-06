# 1. Introduction

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> SANKHYA is a single Rust binary that holds a transactional store, a vectorised analytical
> engine and a temporal graph over one governed copy of the data, with no JVM and no sidecar.
> This chapter says what that means concretely, who the system is for, and what problem it is
> an answer to — the permanent reconciliation cost of an estate that keeps the same fact in
> several places. It also states, at the outset and without softening, which parts of the
> system exist today and which are designed and unbuilt, because a book about a system under
> construction is only useful if the reader can trust its tenses.

## 1.1 What it is

SANKHYA is one deployable artifact that provides three data models over one copy of the data.

| | Engine | What it is for |
|---|---|---|
| **Transact** | PostgreSQL — embedded in-process or attached externally | The authoritative system of record for managed tables. Strict ACID, foreign keys, row-level locking, full audit. |
| **Analyse** | Apache DataFusion over Arrow, above an open lakehouse table log | Vectorised, SIMD-accelerated OLAP: ad-hoc SQL, high-cardinality aggregation and multidimensional analysis over billions of rows. |
| **Relate** | An in-memory graph engine over Arrow-backed adjacency | Traversal, reachability, cycles, weighted transitive closure and centrality, with time-respecting paths as a first-class primitive. |

Between the first and the second sits a native Rust change-data-capture bridge that reads
PostgreSQL's write-ahead log directly through the `pgoutput` logical replication protocol and
publishes it to versioned columnar storage. There is no Kafka, no Debezium, no Connect cluster,
and no JVM anywhere in the stack.

Three properties distinguish the arrangement from a bundle of three products in one installer.

**One address space.** The three engines share one process, one memory representation and one
security model. A query does not cross a wire to reach a different engine, and there is no
protocol between them that can fail while every component reports healthy.

**One copy.** The analytical tier is a derived, versioned, reproducible view of the
transactional tier. The graph tier holds no durable state at all: an epoch is hydrated by
scanning published tables, carries the snapshot it was built from, and is dropped on shutdown.
There is no graph write path, so the graph cannot disagree with SQL — an edge exists because a
row exists.

**Open storage, mechanically.** Analytical tables are ordinary Delta tables in a directory
layout that mirrors the operational schema, so `sales.orders` in the transactional tier is
`warehouse/sales/orders/` on disk. Spark, Trino, DuckDB and Athena read them directly, with no
SANKHYA process in the path. The claim is exercised rather than asserted: the Delta kernel
reads the log in the test suite. Chapter 6 gives the layout and Chapter 12 names the one case
where the property does not hold.

## 1.2 The name

**संख्या** *(saṅkhyā)* is Sanskrit for *number*, but the root says more than the translation.
It is **सम्** *(sam,* "together, completely"*)* bound to **√ख्या** *(khyā,* "to make known, to
declare, to reckon"*)*. To enumerate a thing is to make it **completely known**.

From that root comes **सांख्य** *(Sāṅkhya)*, the oldest of the six *darśanas*, the school of
enumeration — which resolves the plurality of experience into twenty-five ordered *tattvas* so
that the whole may be understood through its parts, and whose central discipline is **विवेक**
*(viveka)*, discrimination: telling the observer from the observed, the essential from the
merely manifest.

Both halves are load-bearing here. To count, and to discern. A number alone is not the point;
knowing *which* number, *as of when*, and *why* is the point. That is why completeness travels
on the row, why a date column's provenance is declared rather than assumed, and why a measure
must say how it may be combined before anyone may combine it.

## 1.3 Who it is for

The engine is **domain-agnostic**. Nothing in the core knows what a trade, a shipment or a
patient is; it knows about tenants, tables, columns, rows, edges, versions and policies. A
build check fails if a domain noun appears in a core crate. Domain semantics arrive as
**packs**: versioned bundles that contribute schemas, aggregate functions, graph algorithms,
detection rules and policy vocabulary through a stable extension API.

The system is therefore for a *shape of problem* rather than an industry. That shape has four
markers, and a workload with fewer than three of them is probably better served by something
simpler.

| Marker | What it looks like |
|---|---|
| The same entity is both transacted and analysed | A trade is booked, and then aggregated across six hierarchies within the hour |
| Relationships are part of the question | The answer requires a traversal — an ownership chain, a payment path, a tier-N supplier dependency — not just a join |
| The gap between exact and approximate matters | A 99th percentile and an *approximation* of a 99th percentile are the difference between an answer and a finding |
| Somebody will ask what you knew, and when | The question "what did we report on the 14th?" must be a query, not an archaeology project |

Two reference packs ship from unrelated industries, and they exist as much to prove the
extension API is real as to serve their domains: **risk** (scenario vectors and sensitivities
pivoted across any hierarchy, with exact order statistics) and **financial crime**
(transaction networks, time-respecting paths, structuring detection, beneficial-ownership
tracing with materiality thresholds). A third, deliberately hostile pack — every attempt of
which is refused with a named error — is what tests the isolation claim. Chapter 21 covers all
three.

The general capabilities underneath are the point:

| A domain concept | The general capability |
|---|---|
| VaR / expected shortfall | Exact order statistics with a declared interpolation convention |
| Scenario P&L vectors | Fixed-size numeric list columns with element-wise aggregation, where reduction order is semantically significant |
| Ultimate beneficial ownership | Weighted transitive closure with multiplicative edge weights, cycle tolerance and a pruning threshold |
| Laundering-chain detection | Time-respecting path traversal |
| Structuring / smurfing | Windowed pattern matching over a time-ordered edge stream |
| Coverage checks | Data-completeness measures attached to any aggregate |

## 1.4 The problem

The estate SANKHYA is an argument against is described at length in Chapter 3. In summary, it
runs three or more engines, keeps a copy of the same fact in each, enforces a different
security model on each, and employs a permanent function whose only output is an explanation of
why they disagree. That function has no analytical output. Its cost grows quadratically in the
number of copies, because *n* copies produce *n(n−1)/2* reconciliations. And it exists because
of the architecture rather than because of carelessness: every copy was the locally correct
answer to a real question.

The bet SANKHYA makes is that four things changed at roughly the same time — Arrow as a shared
in-memory representation, a vectorised query engine available as a library rather than as a
cluster, open lakehouse table formats with a transaction log, and PostgreSQL logical
replication as a protocol one can implement natively — and that together they make the estate
collapsible into one artifact without giving up ACID, without giving up sub-second analytics,
and without giving up the audit trail a regulator will ask for. Chapter 2 defends that bet
clause by clause.

## 1.5 What exists today

This section is the one to read before believing anything else in this book.

The `**Status:**` line at the head of this chapter is the canonical one, and it is checked ---
`cargo xtask check-docs` fails when any document disagrees with it, and now fails when a
chapter of this book declines to carry it at all. Read it rather than this paragraph.

What that line says, in prose: **M0, M1, M3, M4, M7 and M10 are complete.** M2 and M13 are
substantially built. M5 closed on four of five exit criteria, M6 on six of seven, M8 on six of
eight with its scale-out half moved to M12 for want of a second machine. M9's work is built and
demonstrated and its gate is deliberately held for M11. M14, M17 and M18 are in progress. M11
needs a production deployment and M12 needs a second machine, so neither is schedulable here.

> **This paragraph used to read "Complete: M0 through M8, M10 and M13."** That sentence was
> wrong in thirteen documents at once, and correcting it is item 0.7 of
> [REMEDIATION.md](../../REMEDIATION.md). It survived here longer than anywhere else for a
> structural reason worth stating in the chapter that asks to be believed: the check that
> compares status lines only looked at documents which *declared* one, and no chapter of this
> book did. Opting out cost nothing, so the book opted out of the check written to stop
> exactly this. An absent header here is now a failure.

What runs today, exercised by tests and by clients rather than by assertion:

- A `pgoutput` wire decoder validated against a real PostgreSQL 17.11 stream; an apply path
  whose transaction invariant is property-tested; lossless type mapping; capture that
  reconciles against its source and survives a crash at any point.
- An **open table log** the Delta kernel reads, with compaction that plans, merges, commits and
  converges without changing an answer, and a maintenance scheduler that arbitrates it against
  the machine's budget.
- A **table provider** that plans from metadata alone, prunes files by recorded statistics,
  resolves updated and deleted rows to one current version each, and answers from memory and
  Parquet at once — or refuses when the tiers do not cover the query.
- **The server runs.** Real `psql` connects, authenticates and runs ordinary SQL against a
  provider wrapped in its policy decision, over real Parquet on disk. **Arrow Flight SQL**
  streams results as Arrow batches over gRPC on its own port. Both doors are served over TLS.
- **Cubes as a declared model.** `CREATE CUBE` and `DROP CUBE` are statements a client can
  send; `cube_rollup` and `cube_slice` are SQL table functions with no build step before the
  query; every row carries the completeness it was computed under.
- **Zero-copy cloning**, whose design gate was cleared by an ADR before any code was written,
  and whose five exit criteria are met.
- **Config-driven ingest from files**: a YAML declaration names the source, the shape of what
  arrives and where it lands; a record that does not fit is quarantined whole into a table with
  a mandatory expiry; a halted feed is visible and resumable from a client.
- Operability: `doctor` reports *when* a problem becomes user-visible rather than its current
  value; `backup` binds the transactional backup, the table versions and the key generation to
  one consistent point; `drill` proves a backup restores by reading the data back and
  recomputing its digest. Every error a client sees carries a permanent code and a remediation.

What does **not** exist, named here rather than discovered later:

| Not built | Where it sits |
|---|---|
| The streaming replication transport (changes are drained through a SQL function today) | M2's remainder |
| The slot lifecycle driver and the backfill reader | M2's remainder |
| A measured graph benchmark against a public suite | The one M4 exit criterion carried forward as **unmet**, not reinterpreted |
| The pack bundle loader that reads a bundle into a running server | M4's remainder |
| The gRPC control plane | Not built; the REST gateway is refused by design |
| Table partitioning on the **streaming arrival path** (the batch publish path is partitioned) | Sized, not started |
| Bloom filters, the result cache, quantile sketches, leader election | Inside the M3 work breakdown, deliberately unbuilt |
| A timer driving graph hydration | Not built |
| Federated identity — a `Principal` is a fixed tenant | M14 |
| Destructive purge against a system of record | Built and **disabled until M11**, by decision |

> **Pitfall**
> A negative list accretes. The repository's own *"what does not exist"* inventory carries a
> health warning saying so: several of its entries were written against an early milestone and
> have been answered since without being removed — *"no server"* sat eight bullets above *"the
> server runs and executes statements"*, which is the sort of contradiction a reader resolves by
> distrusting the whole document. The table above is reconciled against the milestone sections
> rather than copied from that list, and where the two disagree this book follows the milestone
> section and says which claim it is rejecting. **Auditing a negative list is work somebody has to
> schedule**, and an unaudited one is not evidence that everything unmarked is current.

Three places where the analytical tier returns a **wrong number** rather than an error — integer
sum overflow, integer multiplication overflow, and decimal sum past 38 digits — are enumerated
in a test and are open. Chapter 19 states them. They were found by a test that runs the same
fifteen cases against both tiers and compares.

## 1.6 How to read this book

This book is the whole of SANKHYA, and it replaces the scattered working documents it was
assembled from.

| Part | What it is |
|---|---|
| **I — The case** | Why this system should exist at all: the thesis, the estate, and the boundaries |
| **II — The machine** | How it works: architecture, storage, the date axis, the read path, ingest, cubes, the graph, cloning |
| **III — Operating it** | Security, observability, maintenance, backup, packaging |
| **IV — Using it** | Getting started, the SQL surface, the client contract, extensions |
| **V — The discipline** | Invariants, how this is tested, requirements, decisions, status |

| If you are | Start at |
|---|---|
| Deciding whether this is worth your time | Chapter 2 |
| An architect assessing the design | Chapter 5 |
| An engineer who wants to run a query today | Chapter 18 |
| Responsible for operating it | Part III |
| Wondering whether to believe any of it | Chapter 23 |

## 1.7 The honesty rule

Everything in this book obeys one rule, and it is worth stating as a rule because it is easy to
violate by accident.

> **Key idea**
> Where something is designed and not built, the text says so and names the milestone. Where a
> decision has been made and not implemented, it names the decision record. A document that
> describes an intention in the present tense lies to the person least able to tell — the
> person who has not read the code.

Two consequences follow. Chapters mark unbuilt work inline rather than collecting it into an
appendix nobody reads. And the repository's `STATUS.md` remains authoritative over this book on
any question of what exists, because it is updated in the same commit as the code and a book is
not.

The rule has teeth because it has been enforced against this project's own documents. An
adversarial review on 2026-09-01 found four claims of exactly this kind that were false — and,
more instructively, found four *defects* the same week, every one of which had passing tests
and several of which had passing mutation tests. None was found by the test suite. The pattern
did not vary: a surface's own tests call the surface, and the statement goes missing one layer
above it. Chapter 23 is about what that costs and what is done about it.
