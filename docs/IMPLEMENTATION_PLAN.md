<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Implementation Plan

**Document ID:** SNK-IP-001
**Version:** 0.1.0 (draft for review)
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Date:** 2026-08-26
**Companions:** `REQUIREMENTS.md` (SNK-RD-001), `ARCHITECTURE.md` (SNK-AD-001), `ROADMAP.md`

---

## 1. How this plan is organised

Milestones are numbered `M0`–`M9`. Each has **entry criteria**, a **work breakdown**, **exit criteria** and a **demonstration** — a thing a person can watch. A milestone with no demonstration is a milestone with no feedback, and it is where projects of this size go wrong.

Effort is stated in **engineer-weeks (ew)**. Ranges are honest ranges, not padding: the lower bound assumes the design holds, the upper assumes one significant rework.

Every exit criterion is **mechanically checkable**. "The team is satisfied" is not an exit criterion.

---

## 2. Why the original phasing is replaced

The original brief proposed: storage and ingest, then the analytical engine, then the graph engine, then the API and client SDK.

That ordering has four defects, and they compound:

1. **The API arrives last.** For three of four phases nothing is demonstrable end to end, integration risk is discovered at the worst possible time, and the API's requirements never feed back into engine design. This is the classic waterfall failure and it is entirely avoidable.
2. **Ingest is first, before anything can read what it produced.** Ingest is the hardest, most correctness-critical, most externally-coupled component in the system — and **you cannot meaningfully test it without a reader.** Building it first means building the riskiest thing blind.
3. **The single most credibility-defining demonstration spans three of the four phases.** "Create a table, insert a row, query it analytically" is what convinces anyone that the unified-system claim is real. Under the original ordering it arrives in month six or later.
4. **Nothing addresses multi-tenancy, security, operability or the extension boundary**, each of which is expensive or impossible to retrofit.

This plan therefore front-loads three things that are nearly free at the start and ruinous later: **the extension boundary, the tenancy parameter, and the benchmark harness.**

---

## 3. Sizing summary

| Milestone | Theme | Effort (ew) | Calendar (6 engineers) |
|---|---|---|---|
| **M0** | Foundations, spikes, walking skeleton | 10–12 | weeks 1–3 |
| **M1** | Zero-configuration sync and read-your-own-writes | 14–18 | weeks 3–6 |
| **M2** | Ingest correctness and durability | 24–28 | weeks 5–13 *(parallel with M3)* |
| **M3** | Query engine and storage performance | 28–34 | weeks 5–14 *(parallel with M2)* |
| **M4** | Graph engine and the extension mechanism | 26–32 | weeks 12–20 |
| **M5** | Tenancy, security and API surfaces | 22–28 | weeks 18–25 |
| **M6** | Operability, packaging and hardening | 18–22 | weeks 24–30 |
| **M7** | Multidimensional analysis — cubes, hierarchies, consolidation | 14–18 | weeks 28–34 |
| **M8** | **Concurrency and data safety**, crate hygiene | 9–12 | weeks 32–37 |
| **M9** | Tiering *(gated — see §13)* | 12–16 | after M8; criteria 2 and 3 of the gate |
| **M11** | Production reconciliation *(not schedulable by development)* | — | after a production deployment exists |
| **M12** | **Scale-out, HA and disaster recovery**, then production-like acceptance — 12 h, two machines, 100 GB, 50 readers, 20 writers | 20–26 | the project's exit criteria; **needs a second machine** |
| | **Total to a hardened first release** | **~150–190 ew** | **~7–8 months** |

**Team shape:** six engineers. Suggested specialisation — two on ingest and storage, two on query and graph, one on platform and operability, one on security and tenancy — with the extension API owned by whoever owns architecture.

**After the first release**, each domain pack costs roughly **8–12 ew**, runs in parallel, and sits off the critical path. Much of that work is declarative and can be delivered by a domain specialist rather than a systems engineer. **That conversion — of the project's scarcest resource into its most available one — is the return on the general-purpose investment, and it begins paying at the second pack.**

### 3.1 What the general-purpose restructuring cost

| | ew |
|---|---|
| Baseline estimate before the restructuring | 120–150 |
| *Removed from the core* — domain analytics, domain detection logic, domain benchmark suites | −18 to −24 |
| *Added to the core* — extension API and its versioning machinery, general rule engine, materialized views, declarative pack loader, sandboxed pack tier, logical-type registry, broadened graph primitives, **multi-relational and temporal graph model**, two reference packs plus conformance and adversarial suites, policy-vocabulary extension points, benchmark rework, boundary tooling | +47 to +64 |
| **Net** | **+29 to +40** |
| **Revised** | **150–190** |

Roughly 20–25% more up front, in exchange for a third domain costing eight to twelve weeks rather than a rewrite.

---

## 4. M0 — Foundations, spikes, walking skeleton

**Weeks 1–3 · 10–12 ew**

### Entry
Repository, team, toolchain decision.

### Work

**4.1 Workspace and enforcement (2 ew).** Crate skeletons across all layers. The layer-check tool enforcing the dependency direction, the pack-dependency restriction and the no-core-depends-on-pack rule. The file-length check with its warning tier and dated exceptions. The domain-vocabulary lint. The workspace-level dependency pin table. Duplicate-version detection as a **blocking** gate. Licence and advisory scanning.

**4.2 The pin-set spike (2 ew) — do this first; the architecture depends on it.**

Build the full dependency set together and prove the following graph resolves with **zero duplicate versions**:

```
datafusion 55 + arrow 59 + parquet 59 + object_store 0.13 + delta_kernel 0.27 (arrow-59)
```

Specifically verify that no native-library key collides — the compression libraries that Parquet pulls in are declared exactly once, and a duplicate is a hard build failure rather than a cost. **Record actual cold and warm build times** to replace estimates. This spike either confirms the read-path architecture or forces it to change, and two days spent here saves a month later.

**4.3 Testing infrastructure (2 ew).** Deterministic fakes for every seam — clock, identifier generation, object store with fault injection, transactional store, capture source, table format, policy engine. **The in-memory table format implementation**, which is what allows the apply-path property tests of M2 to run in milliseconds. Test-fixture generation.

**4.4 The benchmark harness (2 ew) — built now, not later.**

If performance is a headline requirement it cannot be a late-phase concern. The harness exists in M0 with a committed baseline, even though it currently benchmarks a stub. Retrofitting a performance gate is an order of magnitude harder than starting with one.

Includes the in-process generator for the public analytical suite — which is pure, dependency-free and emits columnar batches directly, so **that suite can run on every pull request** rather than only nightly.

**4.5 Process supervision and lifecycle (2 ew).** Data-directory lock. Asset extraction with checksum verification. Database initialization, start, readiness with bounded backoff. **Orphaned-process detection, adoption and recovery.** Graceful shutdown with the specified drain order. Signal handling. Health endpoints with distinct startup, liveness and readiness semantics.

**4.6 Decision records (0.5 ew).** Pin set and engine strategy; metadata-only storage coupling; table format; capture mechanism; node roles; consistency model; the core/pack boundary. **The consistency model must be decided now**, because it shapes every API signature that follows.

**4.7 The walking skeleton (1.5 ew).** Every layer a stub, but the wire is end to end: start, initialize the database, insert through a client, one change reaches a table on local disk, one query returns it.

### Exit
1. All enforcement checks green; pull-request pipeline under twenty minutes.
2. **Duplicate-version detection produces no output.**
3. Pin-set spike concluded, with measured build times recorded.
4. The benchmark harness runs in the pipeline with a committed baseline.
5. Startup, readiness and clean shutdown demonstrated; recovery after forced termination demonstrated.
6. Decision records merged.
7. Walking skeleton passes as an automated test.

### Demonstration
*"One row, all the way through, in one binary, with no configuration."*

### Risks addressed
Dependency incompatibility discovered late; performance treated as a late-phase concern; the extension boundary retrofitted.

---

## 5. M1 — Zero-configuration sync and read-your-own-writes

**Weeks 3–6 · 14–18 ew · the credibility milestone**

This is the owner's headline requirement made demonstrable, and it is deliberately the *second* milestone rather than an emergent property of four.

### Entry
M0 complete.

### Work

**5.1 Automatic onboarding (3 ew).** A publication covering all tables so later-created tables are captured automatically. Onboarding triggered by the first relation-metadata message for an unknown relation. A schema-change log written by a database event trigger, itself replicated so it arrives in-band. **Replica-identity detection and policy**, including the case where the database itself rejects updates on a table with no usable identity.

**5.2 Type mapping (3 ew).** The mapping across the source, in-memory and file-format type systems, with an **explicitly enumerated supported set**. Unsupported types are rejected loudly at onboarding, never mapped approximately. Property-tested for round-trip fidelity against a live database. Includes the pluggable handler point that packs extend.

**5.3 Naming and layout (2 ew).** The identifier-to-path mapping with its identity fast path and legible transformation. Reserved-name refusal. **Collision detection that refuses rather than disambiguates.** The identity sidecar written into each table directory so a table explains itself with no catalog and no running system. The preflight check that scans an entire source catalog and reports every collision, transformed identifier and case-only distinction before any onboarding occurs.

**5.4 Read-your-own-writes (3 ew).** The session token carrying a commit position. The read-mode enumeration on every query interface — **API-shaping, and it must exist now**. The bounded wait until the pipeline reaches the requested position.

**5.5 The tiered read path, first form (4 ew).** Coverage-interval metadata on every tier. The splice planner with its contiguity and disjointness rule. The union-only merge strategy. Provenance on every response.

**5.6 Demonstration automation (1 ew).** The scenario below as a scripted test running from a cold start in the pipeline.

### Exit
1. From a cold start: create a table in the source, insert, and query it analytically **with zero configuration steps**.
2. End-to-end visibility latency measured and recorded as a baseline.
3. Read-your-own-writes demonstrated with a session token.
4. Onboarding property-tested against the in-memory format.
5. A table with no usable replica identity produces a **named error with documented remediation**, not a silent failure.
6. The naming preflight runs in the pipeline against a schema dump containing deliberate collisions and exits non-zero.
7. **The demonstration uses a non-financial schema**, so the project's first demonstration is already evidence of domain neutrality.

### Demonstration
```
$ sankhya init && sankhya serve &
$ psql -h localhost -p 5433 sankhya
sankhya=# CREATE TABLE device_readings (
             id bigserial PRIMARY KEY, device_id text,
             reading numeric(12,4), observed_at timestamptz);
sankhya=# INSERT INTO device_readings (device_id, reading, observed_at)
          VALUES ('sensor-4417', 21.5500, now());
sankhya=# \q
$ sankhya sql -e "SELECT device_id, avg(reading) FROM device_readings GROUP BY 1"
 sensor-4417 | 21.5500
```
*No pipeline configuration. No table registration. No operator step.*

### Why here
It forces the whole vertical — capture, decode, apply, store, plan, query, serve — to exist by week six. Every subsequent milestone then improves something that already works rather than advancing toward something that does not yet exist.

---

## 6. M2 — Ingest correctness and durability

**Weeks 5–13 · 24–28 ew · parallel with M3**

### Entry
M1 demonstrable. Joined to M3 through the table-format trait and the in-memory implementation.

### Work

**6.1 The protocol decoder (4 ew).** Complete message handling including transaction boundaries, relation metadata, all row operations, truncate, origin, type, streamed in-progress transactions and two-phase commit. **Unchanged large-value placeholders** — writing nulls over real data here is silent corruption. Property tests for round-trip fidelity; a continuous fuzz target; a conformance suite against a reference decoder.

**6.2 Slot lifecycle and transport (3 ew).** Slot creation and naming with an installation identifier. Position tracking and advancement. Standby heartbeats. Restart and resume. Orphan-slot detection. **Detection and reporting of slots belonging to other installations.**

**6.3 Initial backfill (4 ew).** Exported-snapshot backfill with **gapless, duplicate-free handoff** to streaming. Chunked rather than one long transaction, because a long transaction blocks reclamation system-wide. Parallel extraction. Resumable.

**6.4 Idempotent apply (3 ew).** Commit position recorded **inside the table's own commit metadata**. Recovery reads it from the table's history with no external state. **Slot advancement strictly after durable commit.** Transaction atomicity across a batch.

**6.5 Schema evolution (3 ew).** Delivery of schema-change events in stream order. Automatic application of additive changes. **Quarantine of incompatible changes** with a named error and an explicit resolution verb. **Dead-letter spill for quarantined tables so the replication cursor still advances** — without this, one quarantined table fills the source database's volume.

**6.6 The source-safety ladder (3 ew).** Lag monitoring in seconds and bytes. The five-level escalation. Threshold ordering such that SANKHYA degrades before the database invalidates the slot. Gap markers, automatic re-snapshot, and provenance reporting until the gap closes. Freeze-age monitoring and automatic remediation in managed mode.

**6.7 Reconciliation (3 ew).** The three-level comparison — count, key-set digest, per-column checksum over canonical encoding — against an **independent expected-state model held by the harness**. Shipped both as an operator command and as a continuous background job exporting a discrepancy metric.

**6.8 Fault injection and deterministic simulation (3 ew).** Every scenario asserts a **named recovery behaviour**: termination between file write and commit; termination between commit and position persistence; a storage backend reporting failure after a successful write; volume exhaustion; clock movement; partition during a long query; database restart under load; slot lag past threshold; repeated forced termination at randomized points under load. Runs on a single-threaded deterministic executor so every failure reproduces from a printed seed.

### Exit
1. A multi-hour soak under synthetic load with induced faults ends with **zero reconciliation discrepancies** on every partition.
2. The applied position is monotonic across dozens of forced restarts.
3. The schema-evolution matrix is green, including changes applied while writes are in flight.
4. **The source-safety test passes as a release gate**: with the applier deliberately stalled under sustained write load, retained log stays bounded, the database continues accepting writes throughout, the ladder transitions in order and on time, and automatic re-snapshot completes with correct gap reporting.
5. The decoder fuzz target runs clean for its budgeted duration.
6. End-to-end latency measured against its decomposed per-stage budget.

### Demonstration
*"Write continuously. Kill the process at any point, repeatedly, under fault injection. Prove nothing was lost and nothing was duplicated — and prove the source database never came under threat."*

---

## 7. M3 — Query engine and storage performance

**Weeks 5–14 · 28–34 ew · parallel with M2**

### Entry
M0 complete; the table-format trait and in-memory implementation available.

### Work

**7.1 The table provider (8 ew).** Snapshot resolution, file listing, statistics translation and partition-predicate derivation, using the kernel library for metadata only. **Correct exact-versus-inexact filter classification** — claiming exactness when only files are pruned is a silent wrong-answer defect. Per-file statistics population, without which the engine's largest optimizations silently do nothing. Delete resolution into plan-time row selections. Ordering files by statistics to enable early termination.

**7.2 Physical storage design (5 ew).** File-size targets and the tiered compaction ladder. **Row-group and page geometry, including the page row-count limit** without which the page index degenerates and pruning silently stops working. Statistics truncation. Per-column encoding selected from measured statistics. Compression tiering. Bloom-filter policy with its ratio threshold and column cap. Default sort ordering, with declaration paths.

**7.3 Engine configuration assertion (0.5 ew).** **Assert required engine settings at startup and fail loudly on unexpected values** — including filter pushdown and reordering, which default to disabled. A configuration default that changes upstream would otherwise be an invisible regression.

**7.4 Statistics catalog (4 ew).** Per-column, per-partition cardinality sketches, bounds, null fractions, widths and quantile sketches, refreshed at compaction when the data has already been read. Injected into the optimizer. This is what makes join ordering work on arbitrary schemas where nobody has run an analysis command.

**7.5 Caching (4 ew).** Metadata, footer, byte-range, decoded-batch and result layers. Snapshot-keyed so invalidation is free. **Policy version in the plan-cache key** and **entitlement set in the result-cache key** — omitting either is a breach mechanism. Ciphertext caching for encrypted columns. Local disk cache with second-touch admission and scan-resistant eviction.

**7.6 Resource governance (4 ew).** Per-tenant memory pools under a global cap. **Admission control, mandatory** because hash joins do not spill. Counting allocator with a load-shedding brake. Spill on a separate filesystem. End-to-end deadlines and bounded cancellation.

**7.7 Correctness rules (4 ew).** Fixed-point arithmetic throughout. Exact order statistics with a **named interpolation convention** and a bounded-memory algorithm. Planning-time rejection of sketch aggregates when exactness is required, with result watermarking. Deterministic reduction. Fixed-size numeric list columns with element-wise aggregation, and rejection of linear aggregation over non-linear measures. Attachable completeness measures.

**7.8 Maintenance scheduler (4 ew).** Job classes and priorities. Duty-cycle budget. Checkpointed, resumable, twice-safe jobs. Leader election through the transactional store. Compaction triggers and the tiered ladder. Orphan cleanup with a safe age threshold. **Compaction adds; a separate job removes.** Typed conflicts with rebase-and-retry, and the rule that the applier never backs off.

### Exit
1. Performance objectives met **in the pipeline** on reference hardware, each against its named public-suite query where one exists.
2. Cancellation demonstrated within its bound at every point M3 controls: inside a running query, bounded at one batch per partition; across threads; and under periodic checking, within the stated interval. A query that cannot be admitted is refused immediately, with the reply saying whether retrying could ever help.
3. A deliberately hostile aggregation under a constrained memory limit is **rejected rather than terminating the process**.
4. Plan snapshots stable; the SQL-semantics corpus green.
5. Cross-engine semantic differences enumerated in a tested list.
6. Compaction demonstrated to hold file counts within policy under continuous ingest.

> **Amended 2026-08-26.** Exit criterion 2 originally read "including inside user code".
> User code does not exist until M4 builds the extension mechanism, so as written this
> milestone could not close on its own terms — it was gated on a later one. The clause
> has moved to M4 §8, exit criterion 8, where the mechanism it tests is built. What
> remains here is the part M3 owns: cancellation at the points the engine itself
> controls. Note that admission never blocks — it returns a decision and the caller
> waits — so "cancellation while queued" belongs to whoever writes that waiting loop,
> which is M5's server, and it is not silently claimed here. This is a correction to the plan, not a waiver — nothing is now untested
> that was going to be tested; the same test is asked for one milestone later, of the
> milestone that can actually answer it.

### Demonstration
*"Query the public analytical suite at scale, warm and cold, with published numbers — and show the plan proving that pruning, late materialization and dynamic filtering actually engaged."*

---

## 8. M4 — Graph engine and the extension mechanism

**Weeks 12–20 · 26–32 ew**

These are combined deliberately: the graph algorithm set is the most demanding consumer of the extension API, so building them together is what proves the API rather than merely asserting it.

### Entry
M2 and M3 complete.

### Work

**8.1 Graph core (8 ew).** Arrow-backed adjacency with a reverse index and dense internal identifiers. **Typed vertices and typed edges with per-type adjacency segments** — this is a general-purpose requirement, not a refinement, and retrofitting it after packs exist would change every pack's traversal calls and every persisted snapshot. **Edge validity intervals**, with edges sorted by source and time so that time-bounded expansion is a binary search plus a slice.

**8.2 Hydration (5 ew).** Streaming build from a published scan. Immutable epochs bound to snapshots, published by atomic swap and reference-counted. Shadow-epoch rebuild with atomic swap, never blocking queries. Incremental application through a copy-on-write overlay with a rebuild threshold. Per-tenant partitioning. Memory budget enforced **at build time, failing with a clear error rather than at query time with an allocation failure**.

**8.3 Primitives (6 ew).** Bounded traversal; weighted shortest path; k-shortest loopless paths; simple-cycle enumeration; connected components; degree and rank centrality; approximate betweenness; community detection; **time-respecting traversal**; temporal motif matching; weighted path-product aggregation with damping and a pruning threshold.

Two naming hazards in the general-purpose library are recorded in the design notes, because each would produce silently wrong analytics: an all-pairs-shortest-path routine whose name suggests cycle enumeration, and a k-shortest routine that returns walk lengths per vertex rather than loopless paths between a pair. Differential tests against independent reference implementations guard both.

**8.4 SQL composition (4 ew).** Table functions with fixed output schemas, **reported statistics** (without which the planner orders the downstream join badly), subquery seeds via a two-phase operator, mandatory result and time bounds with an **explicit truncation flag**, degree caps with suppression reporting, and monotone-predicate pushdown into the traversal.

**8.5 The extension API (5 ew).** The contribution surface. **SANKHYA's own function traits** rather than re-exported engine traits. The logical-type registry. Self-registration so no core component names a pack. Independent versioning with mechanical breaking-change detection. Layer enforcement of the pack dependency restriction.

**8.6 The declarative pack tier (4 ew).** Bundle format, loader, validation, hot reload, signing and verification. **This tier is expected to express the substantial majority of a real pack**, and building it well is what keeps the other tiers exceptional rather than routine.

**8.7 Reference and adversarial packs (4 ew).** Two deliberately opposite non-financial reference packs. The extension conformance suite. **The adversarial pack**, attempting cross-tenant reads, sandbox escape, non-terminating and panicking functions, budget exhaustion, and core-name shadowing — each rejected with a named error.

### Exit
1. Graph performance objectives met in the pipeline against the named public graph suite.
   *(**Carried forward to M5, 2026-08-27, as unmet.** No public graph suite is wired into
   the performance pipeline. The primitives are correct against an independent brute-force
   reference and bounded by construction, but they have not been timed at scale, and a
   correctness proof is not a performance measurement. Recorded as outstanding rather than
   reinterpreted into something the work does satisfy --- which is the failure mode this
   plan exists to prevent.)*
2. The incremental-equals-full-rehydration property test green.
3. **Memory budget per vertex and per edge type published**, so hardware can be sized before purchase.
4. Time-respecting traversal proven to return no time-violating path.
5. **Each reference pack's change touches zero core files** — the mechanical test of the general-purpose claim.
6. The adversarial pack's every attempt rejected with a named error.
7. The extension API within its size budget, with no escape-hatch types.
8. **Cancellation demonstrated within its bound inside pack code** — a pack function in a
   deliberate infinite loop is stopped, and the query returns an error naming the pack
   rather than hanging. *(Moved here from M3 §7 on 2026-08-26: this tests the extension
   mechanism, which M3 does not build. A pack that cannot be interrupted makes every
   bound elsewhere in the engine advisory, so it is an exit criterion and not a work
   item.)*

### Demonstration
*"Two packs from unrelated industries, both running on an unmodified core, one graph-heavy and one graph-free — plus a hostile pack that fails in exactly the ways it should."*

---

## 9. M5 — Tenancy, security and API surfaces

**Weeks 18–25 · 22–28 ew**

Tenancy is *enforcement* here, not retrofit — the tenant parameter has been present on every interface since M0, which is why this is twenty-odd weeks rather than sixty.

### Entry
M4 complete.

### Work

**9.1 Identity and policy (5 ew).** One principal type resolved at the edge and carried throughout. Federated tokens, mutual TLS, and wire-protocol authentication. The pure policy component with its decision model.

**9.2 Enforcement (5 ew).** Policy rewriting **in the catalog, once**, for all three engines. Row predicates conjoined into the scan with **presence asserted in the final physical plan**. Column masking. Per-tenant graph hydration. The **type-level guarantee** that a provider cannot be constructed without a security context.

**9.3 Negative testing (3 ew).** For every policy fixture, assert forbidden data is absent from results, from the physical plan and from graph memory. **Mutation testing on the policy component** — a surviving mutant is a test passing for the wrong reason, which here is a breach with a green build.

**9.4 Tenant isolation (3 ew).** Per-tenant storage prefixes with scoped credentials. Per-tenant memory pools, admission limits and consumption accounting. Quota enforcement with typed errors naming the exceeded limit.

**9.5 Audit and encryption (4 ew).** Audit capturing the authorization decision **and the data version**, hash-chained and mirrored to immutable storage. Key provider abstraction with envelope encryption and rotation that does not rewrite data. Column-level encryption. Residency enforcement.

**9.6 API surfaces (6 ew).** Columnar streaming as the primary data plane. The wire-protocol front door **with sufficient system-catalog emulation for mainstream tooling** — this is the difference between "a command-line client connects" and "a reporting tool works", and it is more work than it looks. The control plane. A thin administrative gateway with hard result caps. The stable error catalog with documented remediation.

> **The control plane and the REST gateway are deferred to M6, 2026-08-27, by owner
> decision.** `FR-API-04` says what the control plane exposes: *administration, tenancy,
> policy, catalog, health, jobs and archive operations*. Jobs belong to M6 §10.1 — the
> maintenance scheduler is a loop and nothing drives it on a timer. Health belongs to
> M6 §10.1–10.2, the diagnostic and the metric catalogue. Archive operations belong to M9,
> which is gated and not started.
>
> Building the surface first would mean endpoints for jobs no scheduler runs, archives that
> do not exist, and health for a system with no daemon — each a plausible-looking API
> returning a placeholder, which is precisely the kind of thing that gets believed. The
> REST gateway is `SHOULD` and is defined as a thin layer *over the control plane*, so it
> follows it rather than preceding it.
>
> Deferred, not dropped: they are M6 §10.8 and are listed in that milestone's exit criteria.

**9.7 Session semantics (2 ew).** The three read modes. Session tokens. Snapshot pinning with leases and bounded lifetimes.

### Exit
1. Mainstream client tooling connects and works, verified by a compatibility matrix.
2. Isolation **provably enforced across every surface**, including the graph tier, under every feature combination.
3. Mutation score on the policy component above its threshold.
4. Audit records reproduce exactly what a principal saw, including data versions.
5. Compatibility matrix between client and server versions tested, not asserted.
   *(**Carried forward to M6, 2026-08-27, as not met.** One server version exists, so there
   is no matrix to test. Recorded as honestly untestable rather than quietly satisfied by a
   matrix of one — which is the failure mode this plan exists to prevent. It becomes real
   when a second version ships.)*

### Demonstration
*"Two tenants, one deployment. Every attempt to reach the other's data — through SQL, through the columnar surface, through a graph traversal, through a cached result, through an error message — fails."*

---

## 10. M6 — Operability, packaging and hardening

**Weeks 24–30 · 18–22 ew**

> **Closed 2026-08-28.** Six of seven exit criteria met, criterion 4 accepted on a
> forty-five-minute judged run by owner decision, and criterion 7 carried into M8. Both are
> written out below rather than summarised, so that a reader a year from now can see what was
> accepted and what was deferred without reconstructing it from commits.
>
> **Progress, 2026-08-27. Five of seven exit criteria met.** §10.1 the diagnostic,
> §10.2 the metric and error catalogues, §10.3 backup and the restore drill, §10.4 the
> packaging checks, §10.5 the timed journey and §10.6 the version axes are built —
> criterion 3 as far as a single release allows, since running the *previous* binary needs
> one to exist.
>
> **Criterion 4 is not met.** §10.7's harness is built and proven to detect a leak, and a
> ten-gigabyte run is demonstrated; the criterion asks for **multi-day**, which is a
> scheduled pipeline. Counting the harness as the criterion was an overstatement made and
> corrected the same day, and it is the shape of error this milestone exists to prevent
> elsewhere.
>
> **2026-08-28: the first judged run, and still not the criterion.** Forty-five minutes at
> acceptance scale came back `PASS` with every one of the seven watched measures *steady* ---
> the first run of this harness to reach a verdict at all rather than dying inconclusive.
> 168 rounds, 1,680 files published, 2.58 billion rows scanned, resident memory ending at
> 779 MB, and maintenance reclaiming 29.91 GB across 911 ticks.
>
> The two runs before it are the reason that reads as evidence rather than as a number. One
> exhausted the disk at t+2833s and wrote a zero-byte report explaining why; the other died
> one sample short of `live_files`' first verdict. Both were fixed at the cause: nothing was
> driving retirement, and nothing was watching the resource the run could exhaust.
>
> **Forty-five minutes is not multi-day, and the criterion is accepted as met anyway.**
> Owner decision, 2026-08-28. The criterion as written asks for a scheduled pipeline; the
> evidence is one run started by hand. That gap is recorded here rather than argued away,
> because a criterion quietly redefined to match its evidence is the failure this milestone
> exists to prevent, and a criterion *deliberately* accepted on lesser evidence by the person
> who owns the bar is a different thing entirely.
>
> What was weighed: every watched measure came back steady over a full judged run, the two
> defects that made previous runs worthless were fixed at the cause rather than worked
> around, and the multi-day pipeline is scheduling work that gates nothing else in M6.
>
> **The residual risk is a slow leak that forty-five minutes cannot see.** The horizon on
> this run was two hours; anything with a doubling time longer than that is invisible to it.
> The scheduled multi-day run is the thing that would find it, and it is carried into M8 with
> the operability work rather than dropped.
>
> **Criterion 7 is not met, and is carried forward rather than waived.** §10.8's size
> decision and route table are built and tested; the gRPC transport and every write path are
> not. Closing M6 does not make this true, and it is not covered by the 2026-08-28 decision
> above, which was about criterion 4 only. The transport work moves to M8. `STATUS.md`
> records what each section found.

### Entry
M5 complete.

### Work

**10.1 The diagnostic (4 ew).** Environment checks, resource limits, storage reachability **and conformance probing**, database configuration, replication health, tables lacking a usable replica identity, naming collisions, maintenance debt. **Each finding reports time-until-impact, not merely a current value.**

**10.2 Observability completion (3 ew).** The full metric catalogue generated into documentation. Distributed tracing end to end. Structured logging with the tenant-data prohibition enforced by lint and a pre-release scrape.

**10.3 Backup and recovery (4 ew).** The manifest binding transactional backup, table snapshots and key generation. Snapshot protection for a backup's lifetime. **Automated restore drills with retained evidence.**

**10.4 Packaging (4 ew).** Two artifacts — one downloading database binaries, one fully self-contained for air-gapped use. Container images, orchestration manifests with correct termination grace, service definitions. Signing and bill of materials. **A build against an old platform baseline rather than a fully static binary**, because bundled database binaries are dynamically linked and a static binary containing a database is not achievable.

**10.5 The five-minute experience (2 ew).** A timed test in the pipeline, from a clean machine with no container runtime, broker, object store or cloud credentials. **It must be a test so it cannot rot.**

**10.6 Upgrade and migration (3 ew).** The four independent version axes. Upgrade from the previous release against a fixture, asserting identical reconciliation digests and identical query results. A documented and tested rollback procedure.

**10.8 The control plane and its gateway (5 ew).** Deferred here from M5 §9.6, because what a control plane exposes — jobs, health, archive operations — is built in this milestone and the next. gRPC for administration, tenancy, policy, catalog, health and jobs, plus the graph API which is not relational in shape. A thin REST gateway over it, with **result size hard-capped and anything larger returning a Flight ticket**: `FR-API-06` is explicit that serialising analytical results as JSON destroys the zero-copy premise and defines published benchmarks downward.

**10.7 Hardening (3 ew).** Fuzz corpus maturity. Mutation testing across the critical crates. Performance baselines locked. Operator runbooks — one per alert, as a shipped deliverable. And the soak, which is specified below because "a multi-day soak" was the whole of it and that is not an exit criterion anybody can fail.

#### 10.7a The soak

*Specified 2026-08-27 after the owner observed that a thousand-row fixture is too small to establish anything about volume. It is: the five-minute journey proves the documented path works and is explicit that no scaling regression is visible at that size.*

**Three sizes, three purposes, and conflating them is how a suite comes to prove nothing:**

| | Size | Runs | Establishes |
|---|---|---|---|
| `tests/five_minutes.rs` | 1,000 rows | Every build | The documented path works end to end |
| `cargo xtask check-performance` | TPC-H scale factor 1, ~1 GB | On a quiet machine | The stated latency objectives hold |
| **The soak** | **Ten tables, ten gigabytes** — `sankhya-datagen`'s acceptance scale | On a schedule, for days | **Nothing grows without bound, and nothing degrades** |

**What a soak is actually for.** Not "it did not crash" — that is what it reports, and it is nearly worthless on its own. The failures a soak exists to find are the ones that are invisible in any single sample and obvious across a week: memory that grows, file handles that are not returned, a cache with no eviction, a log with no rotation, an audit chain growing faster than the queries that feed it, metric cardinality climbing, compaction that never converges because it keeps losing to the applier.

Every one of those is something this system has a *bound* for. The soak is where the bound is found not to work.

**So the harness is the diagnostic, pointed at itself, sampling densely over days.** `§10.1` already has the machinery: `Trend`, `Projection`, `Concern::RisingTo`, and a refusal to project from too few observations or through a shape that is not a line. A soak is that with a sample every minute for a week instead of one a day.

That makes the pass criterion falsifiable, which "clean" was not:

> **A soak passes when no bounded measure has a projection that crosses its threshold within the observation horizon.**

Not "memory looked steady". A measure with an upward trend and a crossing three weeks out is a **failure**, and it is exactly the failure that ships and is diagnosed six months later in production.

**Measured, at one sample per minute:** resident memory · open file descriptors · live files per table · free space · audit records against queries served · the diagnostic's own history file · distinct metric series · query latency percentiles · retained log volume, once ingest runs on a timer.

**Load shape.** Writes, queries and maintenance **concurrently**. A soak that writes for three days and never queries proves the writer does not leak and nothing else; the interesting failures are contention failures, and they need contention.

**And the harness must be proven to detect a leak.** Otherwise it is an untested backup by another name: a green soak that would have been green anyway. The deliverable includes a test that injects a deliberately growing measure and asserts the harness fails on it.

**What cannot be delivered inside a build.** A multi-day run is a scheduled pipeline, not a `cargo test`. `§10.7` delivers the harness, the injected-leak proof, and a short run; the exit criterion is one clean multi-day run against the ten-gigabyte scale, recorded like a restore drill — evidence retained, failures kept.

### Exit
1. The five-minute experience passes as a timed test.
2. Restore drill automated and passing.
3. Upgrade and rollback tested.
4. One clean multi-day soak at the ten-gigabyte acceptance scale, with writes, queries and maintenance concurrent — judged by §10.7a's criterion that no bounded measure projects a crossing within the horizon, not by absence of a crash. The harness is itself proven, by a test that injects a leak and requires the soak to fail on it.
5. A runbook exists for every alert that can page.
6. Every user-reachable error has documented remediation, generated from the same source as the catalog.
7. The control plane serves administration, tenancy, policy, catalog, health and jobs, and the REST gateway refuses a result too large for JSON by returning a Flight ticket rather than the rows. *(Carried in from M5 §9.6 on 2026-08-27.)*

---

## 11. M7 — Multidimensional analysis: cubes, slice/dice, roll-up and consolidation

**Weeks 28–34 · 14–18 ew**

*Added 2026-08-27 by owner directive. Placed **before** scale-out deliberately: this is a
capability the system is meant to be differentiated by, and multi-node deployment is table
stakes. Shipping the differentiator after the table stakes gets the order backwards.*

### Entry
M6 complete. M3's read path and M4's graph engine are the two things this builds on, and both
are done.

### Structure

Three crates, mirroring the graph engine's split, which has earned itself:
`sankhya-graph-algo` has **zero dependencies**, which is what makes its property tests fast
enough to exhaust rather than sample.

| Crate | Layer | Why it is separate |
|---|---|---|
| **sankhya-cube-algo** | 1 | The lattice, the additivity algebra, the ancestor-answering predicate and cuboid selection are pure functions of a declaration. With no dependencies they can be property-tested exhaustively, and that matters because they are where wrong answers come from |
| **sankhya-cube** | 3 | Resolution against published tables, member sets, execution over Arrow, materialised-cuboid read and write, the budget manager |
| **sankhya-cube-sql** | 4 | The SQL surface |

Materialisation storage gets no crate: a materialised cuboid is a published table and that
machinery exists.

### Work

**11.1 The cube model (3 ew).** Dimensions, hierarchies, levels, members and measures as a
declared, versioned definition over published tables. No second store. Definition-time
validation: acyclic hierarchies with the cycle reported, and **a measure with no declared
aggregation rule refused** rather than defaulted to summation.

**11.2 Additivity (2 ew).** Additive, semi-additive and non-additive measures, with the
aggregation rule declared per dimension. Planning-time rejection of an additive roll-up over a
non-additive measure, naming the measure and the dimension. This is `FR-QUERY-12`'s rule in
the place it is most often violated.

**11.3 Hierarchy traversal on the graph engine (3 ew).** Parent-child hierarchies as typed
edges, consolidation paths as bounded traversals. Ragged hierarchies **native, never padded**
— padding invents members that do not exist and they appear in results. Alternate roll-ups
and shared members, with a member reachable by two paths contributing **once**, proven by
property test.

**11.4 Consolidation (3 ew).** Deterministic reduction per `FR-QUERY-10`, sparse
representation, and the slice, dice, roll-up, drill-down and pivot operations expressible from
SQL with no separate build step.

**11.5 Security and completeness (2 ew).** Aggregates computed only over rows the principal
may read, so two principals may legitimately see different totals; and a completeness measure
per `FR-QUERY-13` so a policy-filtered total is distinguishable from a complete one. A total
computed over rows the caller cannot see is a disclosure through arithmetic and is invisible.

**11.6 The lattice, and materialisation both ways (4 ew).** The cuboid lattice; answering a
query from a materialised **ancestor**, permitted only where the measure is additive along
every dimension being further rolled up; greedy selection under an operator budget informed
by the query log; and the three levels of control — pinned in the definition, budgeted in
configuration, overridable per session.

A materialised cuboid is keyed by *(definition version, snapshot, cuboid)*, so per
`FR-QUERY-20` a new commit cannot produce a stale hit and there is no invalidation protocol
to get wrong. It is stored as an ordinary published table, readable by Spark like anything
else — the open-storage commitment gets no exception for the fast path.

**11.7 Write-back overlay (1–3 ew, `SHOULD`).** A separately versioned overlay for planning
and what-if analysis. Never modifies published data; a query states whether one was applied.

### Exit
1. A cube over a ragged parent-child hierarchy with alternate roll-ups returns totals that
   reconcile against an independently computed answer, with **no member double-counted**.
2. A semi-additive measure rolled up across time by summation is **rejected at planning
   time**, not computed.
3. Two runs of the same consolidation over the same snapshot are **bit-identical**.
3a. **Every query returns bit-identical results with materialisation on and off.** This is
   the criterion that makes materialisation a cache rather than a second source of truth, and
   it is compared by bits rather than within a tolerance.
3b. A non-additive measure is **never** answered from a materialised ancestor, proven by
   property test against the base-data answer.
4. Two principals with different row policies see different totals for the same cell, and
   both results carry a completeness measure saying so.
5. Slice, dice, roll-up and drill-down are demonstrated from SQL against a cube with at least
   six dimensions, with no cube-build step preceding the query.
6. A measure defined without an aggregation rule is refused, with the refusal naming it.

See [`adr/0007-the-cube-model.md`](adr/0007-the-cube-model.md) for why cubes are declared
views rather than a store, and why MDX is deliberately not planned.

---

## 12. M8 — Concurrency and data safety

**Weeks 32–37 · 9–12 ew**

> **Scope reduced 2026-08-30 by owner decision**, from 25–32 ew: §12.2 and exit criteria 7–8
> move whole to [M12](#13c-m12--scale-out-and-production-like-acceptance-the-twelve-hour-two-machine-run),
> because both criteria need a second machine. The title loses *"then scale-out"* with them.

> **Rescoped 2026-08-28 by owner directive**, from 16–20 ew: *"look at the whole platform and
> make it concurrency safe end to end. This whole system needs very high level of concurrency
> and data safety."* The estimate increase was accepted explicitly rather than absorbed. The
> audit behind it and the properties it must deliver are
> [ADR-0013](adr/0013-concurrency-and-data-safety.md).

### 12.1 Concurrency and data safety (8–10 ew)

**This runs first, and the ordering is a decision.** Leader election is how a system *avoids
needing* concurrency safety, so it is tempting to do it first and declare the problem handled.
But M8's shape is multi-node with cache-affinity routing: many readers on other nodes, racing
with a leader's compaction and retirement, holding the paths it is deleting. Safety must exist
before the topology that stresses it, or the first failure arrives looking like a networking
fault and is debugged as one.

**12.1a One publishing helper (1 ew).** `publish` makes a file visible all at once; `claim`
does that *and* fails when the name is taken. The audit found the technique implemented
correctly three times and wrongly four, which is what a three-line technique does when it is
retyped instead of reused.

**12.1b The version claim (1 ew).** `commit` claims through `claim`, so a loser is told and
the rebase loop that already exists finally runs. The object-store equivalent — conditional
put — is specified alongside it so the two implementations stay honest against each other.

**12.1c Reclamation that waits for readers (3–4 ew).** A reader registers what it resolved;
reclamation skips what is registered. The elapsed-tick and version-space guards are **kept and
demoted to backstops** against a leaked registration, which is the job they are actually good
at. The pattern already exists here: the CDC ring's epoch-based reclamation, where readers
never block and are never blocked.

**12.1d `check-atomic-writes` (0.5 ew).** `fs::write` and `File::create` onto a live path, and
`exists()`-then-`rename`, refused outside the helper. A convention held in three places and
lapsed in four; this is why it becomes a gate.

**12.1e The concurrency suite (2–3 ew).** Every defect above was invisible to seventeen hundred
tests for one reason: **every test had a single writer.** Each fix gets its failing test first,
and the throughput properties are measured rather than asserted.

### 12.1f Crate hygiene: reachability as a gate (1–2 ew)

*Added 2026-08-28 after an owner-requested review of all 55 crates.*

The review found the M7 pattern again — **built, tested, unreachable** — this time at crate
scale rather than function scale, and one gate away from being impossible.

**About 2,600 lines nothing can reach:**

| Crate | Lines | What is stranded |
|---|---|---|
| `sankhya-pack` | 1,438 | The whole declarative pack tier: TOML bundles, an expression parser, validation, hot reload. No server depends on it, so a bundle cannot be loaded. The reference packs use `sankhya-ext`, the *compiled* API — the two are not duplicates, and only one is wired |
| `sankhya-api-rest` | 416 | A REST surface nothing depends on |
| `sankhya-cdc-pg` | 368 | A PostgreSQL capture source nothing depends on |
| `sankhya-ports` | 222 | Port traits nothing implements or calls |
| `sankhya-alloc` | 165 | A counting `GlobalAlloc` **never installed** — no `#[global_allocator]` anywhere, so every allocation figure it exists to provide is unavailable |

**Ten crates hold one line of source each** — a doc comment and nothing else:
`api-grpc`, `api-http`, `mv`, `objectstore`, `oltp-pg`, `rules`, `telemetry`, `testkit`,
`tiering`, and `cli` with an empty `main`. **None of the ten is named anywhere in this plan or
in `ARCHITECTURE.md`.** They are not roadmap placeholders; they are scaffolding from an early
layout that the documents then grew past.

Some have a real home even though nothing says so — `api-grpc` is M6's carried criterion 7,
`objectstore` and `tiering` belong to M9, `cli` is the maintenance CLI the owner has asked for.
Others duplicate something that exists: **`telemetry` overlaps `sankhya-metrics`** (789 lines,
built and used), and `oltp-pg` sits beside `api-pg` with a name that invites confusion between
a Postgres *lifecycle* and the Postgres *wire protocol*.

**Why this went unseen.** `check-surfaces` was built in M7 for exactly this failure, and its
scope is narrower than its purpose: it checks crates that **register SQL functions**. A REST
surface, a capture source, an allocator, a pack loader and a set of port traits all fall
straight through it.

**Done 2026-08-28, before M8 opened**, because a decision per crate is cheap while the review
is fresh and expensive once it is not:

| Disposition | Crates |
|---|---|
| **Deleted** — no code, no milestone, duplicated something that exists | `api-http` (a second HTTP surface beside the unreachable `api-rest`), `rules` (the declarative pack tier already is a rule engine), `telemetry` (`sankhya-metrics` is 789 lines and used) |
| **Adopted with a dated milestone**, stated in the crate's own header | `api-grpc` (M8 §12.2 — M6's carried criterion 7), `objectstore` (M8 §12.1 — conditional put is where ADR-0013's claim lands remotely), `oltp-pg` (M8 §12.2 — leader election runs *through* a store nothing supervises today), `testkit` (M8 §12.1e — no fault injection exists, which is why every concurrency defect went unseen), `cli` (M8), `tiering` (M9, gated) |
| **Listed with a reason, undecided** | `mv` — the machinery exists in `sankhya-cube` and a view is not a cube with a query for a fact table; see [ADR-0014](adr/0014-materialized-views-and-the-cube-lifetime.md) |

Fifty-five crates became fifty-two. An empty crate now says when it stops being empty, or why
that cannot be decided yet.

**Done 2026-08-29.**

1. **`check-surfaces` widened to reachability.** Every crate must be reachable from a binary,
   or listed with a reason **and a milestone**. It runs from every root that ships — the
   server, the CLI and each pack — because counting only the server would report the published
   extension API as dead code.
2. **Wired or decided, one per stranded crate, recorded.** `sankhya-alloc` is now the server's
   global allocator and emits `sankhya_memory_in_use_bytes` and `sankhya_memory_peak_bytes`.
   `sankhya-ports` is decided: **delete** — nothing implements a trait in it and its header
   asserts a property the workspace does not have. The other three are not M8's work and say
   so: `sankhya-pack` is **M4 §8.6's** loader, `sankhya-cdc-pg` is **M2's** slot-lifecycle
   driver, and `sankhya-api-rest` is **M8 §12.2** beside the rest of criterion 7.
3. **The empty crates were adopted or deleted**, above.
4. **The name collisions are resolved** — `telemetry` deleted in favour of `metrics`;
   `oltp-pg` and `api-pg` each say in their header which of the two Postgres concerns they are.

**What a decision costs when it is deferred**, recorded because two of the five turned out to
belong to *earlier* milestones. `sankhya-pack` is M4's declarative tier and `sankhya-cdc-pg` is
M2's slot lifecycle: both are built, tested and unreachable, and both were about to be
re-decided as M8 hygiene by somebody who did not know that. A crate with no owner drifts to
whoever notices it last.

**Not consolidation for its own sake.** The three-way splits — `cube`/`cube-algo`/`cube-sql` and
`graph`/`graph-algo`/`graph-sql` — are load-bearing and stay: the zero-dependency algebra crates
are what make their property tests fast enough to exhaust rather than sample. The finding is
about crates that are *unreachable* or *empty*, not about crates that are small.

### 12.2 Scale-out, availability and recovery — moved to M12 on 2026-08-30

> **Moved in its entirety to
> [M12](#13c-m12--scale-out-and-production-like-acceptance-the-twelve-hour-two-machine-run) by
> owner decision, 2026-08-30.** Attached mode, leader election,
> executor scale-out and routing, graph node partitioning, cross-region replication, key
> management, metering and chargeback, the REST gateway's transport and the multi-day run.
> Exit criteria 7 and 8 move with it.
>
> **Why, and it is not effort.** Criteria 7 and 8 both require a second machine, and the
> project has one. Most of the *work* is buildable on a single host — leader election, fencing
> and a lost lease are proven by contending processes, not by contending hosts — but criterion
> 7 asks for *"recovery objectives measured and published rather than estimated"*, and a
> recovery objective measured on one box excludes network detection, machine loss and clock
> skew. Publishing it would be the same species of claim as a contention threshold set below
> the contended figure, which this repository has shipped twice. Cross-region replication is
> not measurable here at all, by definition.
>
> **M12 is where it goes** rather than a milestone of its own, because M12 already declares
> the dependency — *"**M8** for the concurrency properties, attached mode and multi-node
> operation"* — already requires two machines, and had no work breakdown precisely because it
> assumed this section would deliver one. Both are now blocked on the identical missing
> resource, and splitting them across two milestones would have made that one fact look like
> two.
>
> **What stays here:** §12.1, which is complete, and exit criteria 1–6, which are met.

**The seam is decided rather than moved.** §12.2 carried one item that could not be safely
deferred: *"allowing a table reference to resolve to a shard set"*, described in this plan and
in `DEC-14` as near-free now and an expensive retrofit later. Parking an undesigned seam is
exactly what that sentence warns against, so it was designed before the parking and is
[ADR-0015](adr/0015-the-shard-set-seam.md).

Its conclusion is that the seam was **mislabelled**. Resolution is already multi-valued —
`plan_splice` resolves one reference to several sources and proves they cover the span exactly
once, and `AddFile.partition` already records every file's partition values — so shards as file
groups beneath one log are built. Shards as *independently committed logs* are the expensive
reading, and their cost is a cross-shard commit protocol rather than anything in the resolution
layer. That reading is refused, not deferred: `DEC-14`'s own preferred v2 path distributes
execution through exchange operators over file groups and never asks the catalog for N logs.
**No code change was required**, which is the finding rather than the convenient answer.

The sibling seam — keeping the commit path per-table rather than globally serialized — is no longer a seam. It is exit criterion 4 below, because the cheapest way to satisfy every safety criterion is one lock over the warehouse, and that is the outcome criterion 4 exists to forbid.

### Exit

**Safety.**
1. N writers racing for one commit version: exactly one wins and every loser is **told**, with no lost commit under sustained contention.
2. A reader never observes a partial file, for every file this system publishes, under a writer republishing continuously.
3. Reclamation running against continuous scans **never** deletes a file a reader holds — demonstrated under load, not argued from a grace period.

**Concurrency.**
4. Writers to different tables do not contend: throughput scales with writer count, and no global serialization point exists. — **met 2026-08-29.** Commits to eight tables run at 4.8× one table's rate; the same commits behind one warehouse lock run at 0.91×, measured in the same run.
5. Read latency is flat under write load — readers are never blocked by writers. — **met 2026-08-29.** A reader holds 0.59–0.80 of its idle rate under four writers with a p99 of 227 µs; sharing a lock with those writers it holds 0.00–0.07 and waits seconds.
6. Contention on a single table degrades by rebase-and-retry, bounded, so a runaway committer is a diagnosable failure rather than a hang. — **met 2026-08-29.** Sixteen writers on one contested version all commit, worst rebase count eleven; eight writers sustained on one table hold 24–51% of the uncontended rate and beat a single writer.

> **Each of the three is measured against a control taken in the same run** — the same work
> serialized through one mutex — because every safety criterion above them is satisfied by
> exactly that design. A threshold without the control is taste, and this repository has twice
> shipped a contention test whose threshold sat below the contended figure.
>
> Criterion 6 was the one that found something. Measuring degradation rather than asserting it
> exposed a write path that was **quadratic in a table's own history**: `next_version` walked
> from version zero on every append and every rebase. It was invisible to every test because
> every test had a short log.

**Scale-out. — both moved to [M12](#13c-m12--scale-out-and-production-like-acceptance-the-twelve-hour-two-machine-run) on 2026-08-30, with §12.2. Neither is met, and neither is reachable on a single machine.**

7. ~~Multi-node deployment with executor scale-out demonstrated; failover tested under load; recovery objectives measured and published rather than estimated.~~ — **moved.** Needs a second machine. A one-host measurement would exclude the failures the criterion exists to price.
8. ~~Soak criterion 7 carried from M6: the gRPC transport and every write path, plus the scheduled multi-day run.~~ — **moved.** Carried once already, from M6 to M8; carried a second time rather than quietly reinterpreted. The gRPC transport and Arrow Flight SQL *are* served, which is the part that was reachable here.

> **M8 completes on six of eight**, and says so rather than renumbering to eight of eight. The
> two that moved are moved because the hardware to judge them does not exist, which is the same
> reason M9's gate criterion 1 moved to M11 on 2026-08-28. A criterion that leaves a milestone
> for want of a machine is a schedule fact; a criterion that leaves it for want of an argument
> is how gates rot.

---

## 13. M9 — Tiering

**After M8, and gated**

### The gate

Tiering may not ship until **all** of the following hold:

1. Continuous reconciliation has run clean in production across every table class for a sustained period. — **moved to M11**, see below.
2. The restore drill has passed repeatedly.
3. An archive attestation drill has passed on a non-production archive. — **the drill is
   built, 2026-08-30**: `sankhya-backup::attest`, `sankhya-server attest <archive>`, and an
   `archive-attestation` check in `doctor`. It works by *attempting* the violations —
   overwrite, delete, truncate — and requires every one to be refused, because the failure
   `RSK-28` describes is a configuration that still reports the right thing and no longer does
   it. Reading the flag would pass in exactly the case the criterion exists to catch.

   **A run against a real archive is still required to clear this**, and cannot be faked from
   development: the drill refuses any store without a `_non_production` marker, since a missing
   control means the drill itself inflicts the loss.

**The gate is explicit so that schedule pressure cannot quietly make this decision.** Purging the system of record before the copy is provably correct is indefensible, and no amount of care in the tiering code substitutes for demonstrated reconciliation.

> **Amended 2026-08-28 by owner decision.** Criterion 1 cannot be met by development at all: it
> requires a production deployment, and there is not one. Holding M9 behind it would not make
> the system safer, only unfinished. So criterion 1 moves to **[M11](#13b-m11--production-reconciliation)**,
> a milestone of its own at the end of the plan, and M9 proceeds against criteria 2 and 3.
>
> **What does not move.** M9 builds, tests and drills the whole purge path — the state machine,
> the verification, the quarantine, the anomaly guard, the kill switches — and **destructive
> purge against a system of record stays disabled until M11 clears criterion 1.** Building it
> and arming it are two decisions, and only the first belongs to development. Recording the
> split here is what stops "M9 is done" from later being read as "purge is safe to enable".

### Work

~~The policy model and eligibility validation.~~ — **built 2026-08-30**, `sankhya-tiering::policy`.
Every rule is evaluated at policy creation and every reason is reported rather than the first.
`canonical_encoding` matches every logical type exhaustively, so adding one to `sankhya-schema`
fails to compile until somebody decides what archiving it means; `Float32`, `Float64` and `Json`
are refused, and refused rather than normalised — normalising would make the encoding canonical
and the archive **not byte-faithful to the source**, which is the property `FR-TIER-09` checks.

~~The durable, resumable state machine.~~ — **built 2026-08-30**,
`sankhya-tiering::{authorize, machine}`. The journal is written before the action, so a crash
leaves a record of something that may not have happened and resume re-runs it; the opposite
order leaves a partition detached with nothing recording it. Phases form a chain in which every
destructive one requires `Verified` behind it, which is `FR-TIER-15` as a property of the type
rather than of a review.

~~Exhaustive verification with canonical encoding.~~ — **built 2026-08-31**,
`sankhya-tiering::{encode, verify}`. Row count, primary-key set equality via a Merkle digest
over sorted blocks, and per-column checksums **taken in key order** --- because two rows with
their values exchanged pass every check that is not, leaving each column's multiset, the row
count and the key set unchanged. The plan's row count is compared too: two scans pointed at the
wrong place agree about everything, so source-against-archive alone passes when nothing was
read. `FR-TIER-15` is now the compiler's job rather than the chain's: `Purge::entering` refuses
`Phase::Verified` and `Purge::verified` takes a `Proof` that only a comparison finding nothing
can produce, and which is neither `Clone` nor `Copy` so it cannot be earned once and reused.
This found that the eligibility rules admitted a table with no primary key, which is a table
whose key set cannot be compared --- now `Ineligible::NoPrimaryKey`.

~~The archival registry.~~ — **built 2026-08-31**, `sankhya-tiering::registry`. The catalog is
authority for the hot extent and the registry for the cold one, and neither for both. Entries
may not overlap, ranges are half-open, and an uncovered sub-range is reported as a gap rather
than assumed hot. `FR-TIER-22` is a value rather than a check: expiry takes a `Pins`, which only
`Registry::pins` produces, so the registry is not something expiry remembers to ask. `FR-TIER-23`
produces a `Servable` witness per table, and a table nobody reconciled is not servable.
`FR-TIER-24` is `Registry::delta`, reported before serving. This needed a range to be *ordered*,
which the canonical encoding does not promise --- hence `Ineligible::TieringKeyNotOrdinal`, the
third eligibility rule found by building what sits downstream of it.

~~The four-layer purge defence.~~ — **built 2026-08-31**, `sankhya-tiering::defence` plus a
fourth eligibility rule. Only the first layer is load-bearing --- purge is detach then drop, and
the phase chain has no delete in it --- and the fourth says of itself that it is provenance
rather than safety, because a scheme depending on a marker arriving fails open when one does
not. A delete or truncate landing in an archived range halts the applier: not a warning, which
defers the decision to whoever reads the log, and not a skip, which leaves the two tiers
permanently disagreeing about a row nobody was told about. A truncate is fatal whether or not it
can be placed, and a delete whose key cannot be read fails closed.

~~Cross-tier query unification with the total tie-break rule.~~ — **built 2026-08-31**,
`sankhya-tiering::unify`. Four cases, each with an answer: hot only reads hot, cold only reads
cold, both reads hot **once** and flags the disagreement, and neither fails with a coverage gap
naming every hole. A property test over arbitrary extents asserts the segments cover the
predicate exactly or the refusal accounts for the rest, which is what "total" means as a
checkable claim. `plan` takes the `Servable` witness, so `FR-TIER-23` is not something a planner
remembers to check; and a mutation into an archived range is a typed refusal naming the archive
and the correction mechanism, because `FR-TIER-18` is explicit that reporting zero rows affected
is a silent wrong answer.

~~Quarantine and its reaper.~~ — **built 2026-08-31**, `sankhya-tiering::quarantine`. Detach is
undone by re-attaching and drop is undone by nothing, which is what the grace period is for: it
insures against the defect verification cannot catch, a policy that was wrong rather than a copy
that was. Age is not a sufficient condition to reap --- a partition the registry no longer claims
is the only copy there is, and no amount of age makes releasing it safe. `Grace::of(0)` is a
refusal, because a grace of nothing is the requirement unimplemented rather than configured. And
`reattach` withdraws the archival entry in the same call, so neither half can happen without the
other.

~~Rehydration with mandatory expiry.~~ — **built 2026-08-31**, `sankhya-tiering::rehydrate`.
All four of `FR-TIER-20`'s properties are types rather than checks: a target schema that refuses
to be capturable or to be the live parent's, a `ReadOnly` unit type with no second variant to
invite, and an `Expiry` that cannot be zero and cannot exceed ninety days. The expiry is the
load-bearing one, because `RSK-35` is a failure with no moment --- each rehydration is
individually reasonable and the accumulation is the problem. `FR-TIER-19`'s compensating entry
is the default correction, and a controlled rewrite cannot be constructed without retaining the
prior version and recording an amendment link.

~~Whole-table migration.~~ — **built 2026-08-31**, `sankhya-tiering::migrate`. The table keeps
its name because a table that vanishes breaks every dashboard, view and saved query that names
it, and `Cold`'s constructor is given one name for both sides so a rename would have to be
deliberate. The trap that creates is a visible table whose archive covers four of its five years
and answers four years of questions without mentioning the fifth --- so a migration is refused
unless the registry covers the whole declared key domain, reusing `Registry::coverage` so the
definition of *covered* cannot drift.

~~Command and schedule surfaces with plan digests, blast-radius limits, the anomaly guard and
kill switches. Segregated authorization and the evidence pack.~~ — **built 2026-08-31**,
`sankhya-tiering::{command, permission, schedule, evidence}`. Planning is always a dry run
because a `Proposal` has no method that acts; the digest is taken over the cluster, the policy
and every range in order, so an approval names what it approved and expires because a plan is a
statement about a table's contents at a moment. A schedule is disabled, unapproved and stopping
at `Archive` by default, since the safe configuration should be what somebody gets by not
deciding. The anomaly guard compares against the trailing **median** rather than the mean,
because the mean is moved by the very outlier being looked for. Blast radius stops cleanly at the
limit and names which one bound. The evidence pack is a projection of the write-once marker and
nothing else, sealed with `HMAC-SHA256` pinned by `RFC 4231`'s published vectors rather than by
inspection.

### Exit
~~Purge demonstrated end to end with verification, quarantine and rollback; the anomaly guard
demonstrated halting an intentionally-defective policy; every rejected purge path shown to fail
closed.~~ — **demonstrated 2026-08-31**, `sankhya-tiering/tests/end_to_end.rs`. Three tests that
walk the whole path with the real types rather than asserting each piece separately, because a
module can be right and the composition still wrong. The third enumerates nineteen ways a purge
is refused and requires every one of them to refuse.

**This does not clear the gate.** Criterion 3 still needs the attestation drill run against a
real non-production archive, criterion 1 is in [M11](#13b-m11--production-reconciliation), and
**destructive purge against a system of record stays disabled until M11**. M9's work is built and
demonstrated; M9 is not complete, and the distance between those two sentences is the gate
working.

---

## 13a. M10 — Zero-copy cloning

**After M9. Design-gated: no code before an accepted ADR.**

### What it is

`CREATE TABLE ... CLONE source AT VERSION n` produces a table that reads exactly what the
source read at that version, in constant time and constant space, by **referencing the same
Parquet files rather than copying them**. Writes to either side then diverge: each commits to
its own log, and neither observes the other.

The same mechanism at schema scope gives a branch of a whole warehouse, which is the shape
most of the demand actually takes — a pre-release environment seeded from this morning's
production, an analyst's scratch copy of real data, a what-if cube that must not perturb the
published one, a point to return to before a bulk correction.

### The gate

~~**An accepted ADR before any implementation**~~ — **cleared 2026-08-31**,
[ADR-0016](adr/0016-zero-copy-cloning.md). Shared-file lifetime is decided as **reachability over
the clone family**, scoped by a lineage record, rather than reference counting: a count that
drifts high loses disk and a count that drifts low deletes data a clone is the only reader of,
which is the silent loss this gate exists to prevent, and reference counting is the only
candidate that can produce it. Copy-on-maintenance was refused for giving up constant space
*quietly* --- a clone's cost would depend on maintenance activity its owner cannot see.

The decision is a no-op for every table that has never been cloned: its family is itself, its
reachable set stays empty, and the sweep does what it does today. Building the refusal list found
three the plan had not named --- a clone at a version the origin no longer retains, dropping an
origin a clone still references, and time travel on a clone before its creation.

It covered, as the gate required:

1. **Shared-file lifetime.** Who may delete a file that more than one table names, and how
   that is decided without a global scan.
2. **The maintenance interaction**, in detail, because this is where cloning breaks a system
   that already works.
3. **What a clone means** for backup and restore, for tiering and purge, for the audit chain,
   and for time travel on both sides of the split.
4. **What is refused.** A clone whose origin is being purged; a clone across tenants; a clone
   of a table mid-schema-evolution.

The gate exists because the failure mode is not a failed query. It is **silent data loss in a
table nobody was touching**, discovered when somebody reads a clone months later.

### Why maintenance is the hard part, and not an afterthought

Everything this warehouse does to keep itself in shape decides that a file may be removed by
consulting **one table's log**:

- **Retirement** removes an input a merge replaced, once its grace period has run and no
  retained snapshot of *that table* references it.
- **Orphan collection** removes a file *that table's* log has never named.
- **Purge**, at M9, removes source data after verification against *that table's* archive.

Every one of those is correct today because a file belongs to exactly one table. Under
cloning that premise is false, and each becomes a way to delete data a clone is still the
only reader of. Orphan collection is the most dangerous of the three: from the origin's point
of view, a file only the clone still names is indistinguishable from debris.

So the ADR has to answer the lifetime question first, and the plausible answers each cost
something:

- **Reference counting in the log** — exact, and now every clone and drop is a write that
  must be crash-safe and must not become a contention point.
- **Reachability across all logs at sweep time** — no bookkeeping to corrupt, and the sweep's
  cost grows with the number of tables rather than the size of one.
- **Copy-on-maintenance** — clones never share a file that maintenance wants to touch, which
  is simple and quietly gives up the "constant space" property that motivated the feature.

Choosing among those is architecture, not implementation, and choosing wrong is expensive to
undo once tables exist that depend on it.

### Work

~~The ADR.~~ — **accepted 2026-08-31**, [ADR-0016](adr/0016-zero-copy-cloning.md).

~~Its lineage record.~~ — **built 2026-08-31**, `sankhya-clone::{lineage, family}`. The lineage
is a table property under a `sankhya.clone.` prefix rather than a log action, because an action
nobody else knows is a bet that every reader ignores what it does not recognise --- and this
repository has a test asserting the Delta kernel reads these logs. A table whose lineage cannot
be *read* is refused rather than treated as an ordinary table, because those two answers differ
by exactly the sentence that deletes a clone's data. `readers_of` is a table and its clones
transitively; ancestors are excluded, with the argument written down. A warehouse with no clones
changes no reclamation decision.

~~Whatever the ADR chooses for shared-file lifetime, with maintenance taught to honour it.~~ —
**built 2026-08-31** for the two paths that reclaim on this node's own schedule. `retire_inputs`
and the orphan sweep now take `StillReferenced`, one value for a question asked along two axes
that do not convert into each other: positions a snapshot pins, and files a clone reads. The
sweeper resolves clone pins from **its own log** rather than the clone's, which is what Decision
1a makes possible. A regression test sweeps a table whose clone is the only remaining reader of a
superseded file, with the same table and no clone as the control.

~~The clone action in the log.~~ — **built 2026-08-31**, `sankhya-clone::action`. Creating a
clone commits lineage properties and **no `Add` actions**, which is Decision 1a as the thing
actually written. The schema is copied rather than referenced, because a clone diverges and the
first `ALTER` would otherwise change both.

~~The refusals enumerated above.~~ — **built 2026-08-31**, `sankhya-clone::refuse`. All seven,
plus a tangled lineage, each a pure predicate taking the facts as arguments — a refusal that had
to open a log to decide could not be tested against the case it exists for. Tenancy is checked
first because it is the only one that never becomes true by waiting.

~~Clone surfaces.~~ — **built 2026-08-31**, `sankhya-server`'s statement path. Intercepted
beside the cube DDL, authorized through `scope_for` because a clone is a reference and therefore
a read, and refused with the query path's own sentence so that a refusal cannot confirm a table
exists. The requested version is checked file by file, because the log's cheap bound is wrong in
the dangerous direction: a commit can survive its data. Nine tests, one of which proves ordinary
SQL still reaches the engine untouched.

~~The drop surface's refusal.~~ — **built 2026-08-31**. `DROP TABLE` is read by the clone parser
and answered **only when the table is a clone**; anything else is handed back and the server's
standing "this is a read path" refusal answers it, unchanged. A clone must be droppable because
it is creatable, and the drop is where `may_drop` --- which had existed with nothing calling it
--- now refuses removing a table other clones still read. Writing those tests found two defects
in the clone statement committed an hour earlier: a clone could be created and then not acted on,
because authorization checked its own name rather than what it references; and a name in use was
asked of the policy rather than the warehouse, so a second clone could be created over the first. ~~Backup and restore made clone-aware.~~ — **built 2026-08-31**. A manifest records what each
table is a clone of, and `Manifest::bind` refuses a backup containing a clone whose origin it does
not contain --- at bind time, where a backup that cannot be restored is refused rather than
counted. Restoring a clone without its origin would produce a table that is present, readable and
empty, which is the failure Decision 1a implies and nothing else would catch.

**Tiering's half is a refusal with no reachable path yet.** `may_purge` is written and tested, and
`M9`'s destructive purge stays disabled until `M11` clears its gate --- so there is nothing to
wire it into. Recorded rather than quietly skipped.

**The clone read path** --- the splice `ADR-0016`'s Decision 1a implies and did not name. A
clone's log holds only its own writes, so reading one means resolving the origin's live set at
the cloned version and unioning the clone's log over it. **Until this exists a clone reads as
empty**, which is the failure the backup check refuses and the live behaviour permits. Added to
this list on 2026-08-31, when planning the soak found it.

~~A soak that clones under load, writes to both sides, runs full maintenance, and verifies both
still read correctly afterwards.~~ — **built 2026-08-31**,
`sankhya-diagnostic/tests/clone_soak.rs`. Deliberately hostile maintenance — compaction and
orphan sweeps every tick, no retention grace — against a clone whose inherited files the origin
sees as debris. It runs on every suite rather than deliberately, because a correctness property
that runs when somebody remembers is one nobody is checking, and it reads values back out of the
files rather than counts out of the log.

**With the control beside it**: the same soak, differing only in whether maintenance is told
about the clone. Told, 120 inherited rows survive; not told, 0 — and the plan afterwards names a
file that is not there.

### Exit

~~A clone demonstrated at constant cost against a large table; divergent writes on both sides
verified independent; **maintenance run to completion on the origin with the clone proven to
read every row it could read before**; the same for orphan collection specifically; and every
refused clone path shown to fail closed.~~ — **all five met 2026-08-31.**

Walking them found two that were not: constant cost, and the fail-closed enumeration. Constant
cost is proved **structurally rather than timed** — Decision 1a means a clone's log holds no
`Add` actions, so its cost is one commit whatever the origin holds: 1 file and 410 bytes from
origins of 5 and 121 files. Timing it would have been a sixth throughput measurement, and this
milestone had just learned what those cost. Fail-closed is twelve refused paths in one list, with
a control asserting the permitted cases are permitted — because every such assertion is satisfied
by a feature that refuses everything.

---

## 13b. M11 — Production reconciliation

**After a production deployment exists. Not schedulable by development.**

### Why this is a milestone and not a checklist item

M9's gate criterion 1 — *continuous reconciliation has run clean in production across every
table class for a sustained period* — is the one requirement in this plan that **no amount of
engineering can satisfy**. It is not hard; it is not slow; it is impossible, because it asks for
evidence from a system that is running for real, and evidence cannot be written.

Leaving it inside M9's gate had a predictable failure mode: M9 would be finished in every
respect that development controls, the gate would still read unmet, and the pressure to
reinterpret the words would grow every week. That is precisely the pressure the gate was written
to resist, so the gate is better served by moving the criterion somewhere it can be honestly
tracked than by leaving it somewhere it can only be quietly redefined.

### Work

**Continuous reconciliation in production.** Every table class, sustained, with the results
recorded rather than summarised. The reconciler itself is M9's; what M11 adds is the running of
it, on real data, for long enough to mean something.

**The arming decision.** Destructive purge, disabled throughout M9, is enabled here or not at
all. It is an owner decision informed by the reconciliation record, and it is the only place in
this plan where a milestone completes by somebody choosing rather than by a test passing.

**A substitute is not a pass.** A sustained soak with reconciliation running clean is the
closest development can get, and it is worth doing — but it is evidence about a soak, not about
production, and this milestone is not met by it. Saying so here is the point of writing it down.

### Exit

Reconciliation has run clean in production across every table class for a sustained period, the
record of it is published rather than asserted, and the owner has made the arming decision with
that record in front of them.

---

## 13c. M12 — Scale-out and production-like acceptance: the twelve-hour, two-machine run

**The project's exit criteria.** Added 2026-08-29 by owner directive. **Absorbed M8 §12.2 —
scale-out, availability and recovery — on 2026-08-30**, taking its exit criteria 7 and 8 with
it; see [§12.2](#122-scale-out-availability-and-recovery--moved-to-m12-on-2026-08-30).

### What it is

Two machines drive one SANKHYA instance for **twelve hours**, together pushing **100 GB**
through **50 concurrent readers and 20 concurrent writers**, while cuboids are built, queried
and dropped underneath them and saved data is queried and updated.

Every number here is load-bearing and none is a round figure chosen for looking serious:

| | | Why this number |
|---|---|---|
| **2 machines** | driving one instance | A single process cannot produce true client concurrency: its clients share a runtime, a page cache and a clock. Two machines is the smallest count that makes the network real and the interleaving genuinely uncoordinated |
| **50 readers** | concurrent | Enough that the read path's shared structures are contended rather than merely used. §12.1's `LogCache`, `QueryLog` and `Hydrated` choke points are invisible at four readers and obvious at fifty |
| **20 writers** | concurrent | The commit path is table-scoped by protocol, so twenty writers guarantee sustained version contention — the exact condition under which the pre-M8 claim lost commits silently |
| **100 GB** | total | Beyond any page cache on either machine, so a read is a read |
| **12 hours** | duration | Long enough for `report::supported_horizon` to speak about days rather than hours, and long enough for reclamation, compaction and cuboid retirement to run thousands of cycles against live readers |

### Why the workload is mixed rather than clean

The run must do all of it **at once** — build cuboids, query them, drop them, query base data,
and update saved data — because every defect this project has found lived in an interaction,
not in a component:

- A cuboid **deleted while a query held it** is the failure `CUBOID_DRIFT_TOLERATED` guards
  against with a version-space heuristic. Fifty readers against continuous retirement is what
  turns that heuristic into a measurement.
- **Two writers claiming one version** is what the pre-M8 commit path lost silently. Twenty
  writers for twelve hours is the strongest available statement that it no longer can.
- **Compaction racing a reader** is the case `FR-STORE-23`'s typed conflict exists for, and it
  only arises when maintenance and ingest are both busy on a table somebody is reading.

A clean workload — readers alone, then writers alone — would pass while every one of those
remained broken. **The mixing is the test.**

### What it proves that nothing else does

M8 §12.1 states six properties and demonstrates each in isolation, with fault injection and
sixteen threads inside one process. That is the right way to *prove a mechanism* and it is not
evidence about a system. This run is the evidence: the same properties, on real hardware,
across a network, for half a day, with nothing rigged.

It is also the first thing in this plan that can fail for reasons no test suite can produce —
socket exhaustion, a clock stepping, one machine swapping, a network partition of a few
seconds. Those are the failures that matter in production and none of them can be unit-tested.

### Work — scale-out, availability and recovery (16–20 ew)

**Moved here whole from M8 §12.2 on 2026-08-30 by owner decision.** This milestone originally
had no work breakdown because it assumed M8 would deliver one. M8 could not: every criterion
below needs the second machine that this run also needs, so the build and the run that judges
it are now one milestone blocked on one thing.

Attached mode as the production configuration. Leader election through the transactional store.
Stateless executor scale-out and query routing with cache affinity. Graph node partitioning with
published rebuild times. Cross-region replication and recovery objectives per tier. Key
management integration. Metering and chargeback. The REST gateway's HTTP transport.

**What was already delivered under M8** and is not repeated here: the gRPC transport with Arrow
Flight SQL served, and the `sankhya-oltp-pg` supervisor tested against vendored PostgreSQL
17.11. Both were prerequisites for this work; neither closed a criterion.

**Not a seam.** ADR-0015 settled the one item in §12.2 that could not be deferred — see §12.2
for what it found and why nothing had to be built to keep it safe.

**Scaling is bounded by `DEC-14` and that is not revisited here.** Each query executes entirely
on one node; scale-out adds throughput, not per-query capacity. A workload that exceeds one
node's memory and cores is the *measured* trigger `DEC-14` names for adopting
`datafusion-distributed`, and it is a decision for whoever has that measurement.

### Exit, for the work above

7. Multi-node deployment with executor scale-out demonstrated; failover tested under load;
   recovery objectives measured and published rather than estimated. — *carried from M8.*
8. Soak criterion 7, carried from M6 to M8 to here: the gRPC transport and every write path,
   plus the scheduled multi-day run. — *the transport is served; the write paths and the run
   are not.*

### Depends on

**M8** for the concurrency properties — criteria 1–6, met and measured against controls. Attached
mode and multi-node operation are no longer inherited from M8; they are this milestone's own
work, above.

**A second machine.** Stated as a dependency rather than assumed, because it is the sole reason
§12.2 is here rather than finished.

~~**Cube DDL** — `CREATE CUBE` is not a statement yet, and this run needs cubes created and
dropped from SQL by a client rather than declared into a warehouse directory.~~ — **built
2026-08-30**, ahead of the rest of this milestone precisely because it was the one dependency
here that hardware did not block. `CREATE CUBE` and `DROP CUBE` are statements, recognised
before the engine is asked, and a drop reclaims the cuboids its cube materialised — which
nothing else ever would, because the ordinary sweep deliberately retains a cuboid whose cube it
cannot find a current version for. See [`GUIDE.md`](GUIDE.md).

### Exit

1. Twelve hours at 100 GB with 50 readers and 20 writers across two machines, **`PASS` on every
   declared measure**, judged against a horizon the run's own duration supports.
2. **Not one lost commit.** Every write that reported success is present in the log at exactly
   one version, verified by reconciliation after the run rather than by absence of complaint.
3. **Not one query failed for a file that was deleted underneath it.** Reclamation ran
   throughout; readers never saw it.
4. Every answer that came from a cuboid is **bit-identical** to the same answer computed from
   base data, sampled throughout the run and checked at the end — exit criterion 3a, at scale
   and under contention.
5. Read latency and write throughput are **reported as distributions, not means**, and the
   tail is explained rather than excluded.
6. The evidence pack is durable: the report, the reconciliation, and the failure of any
   measure, retained rather than summarised into a sentence.

**This is the project's exit criteria.** Everything before it is a milestone; this is the run
that says the system does what the documents claim.

---

## 13d. M13 — Config-driven ingest, from files

**After M10. Schedulable now.** Added 2026-08-31 by owner directive.

### On the numbering, before anything else

`M13` to `M15` come after `M11` and `M12` in the numbering and **before them in the ordering**,
because `M11` needs a production deployment and `M12` needs a second machine, and neither is
schedulable by development. The numbers are identity rather than priority: they are quoted in
ADR headers, in `REQUIREMENTS.md`, and in commit messages that cannot be rewritten, and
renumbering would silently falsify every sentence of the form *"M8 §12.2 moved to M12"*.

### What it is

A **configuration** declares a source, the shape of what arrives, and where it lands. Files
first: a directory of JSON documents, each a dictionary, with the config naming the target table,
the mapping from keys to columns, and a microbatch policy. Rows flow through `sankhya-publish` to
the published tier and are answerable by OLAP without a second step.

Nothing here is a new storage path. Ingest is a *producer* for the write path that already
exists, and a config that reached storage another way would be the second writer
`check-writers` refuses.

### The gate

**An accepted ADR before any implementation.** Written and accepted on 2026-08-31 as
[ADR-0018](adr/0018-a-record-that-does-not-fit.md), covering one question that has no obvious
answer:

> **What happens to a record that does not fit the config?**

Everything else in this system refuses rather than coerces --- a type that cannot round-trip
makes a table ineligible, an unreadable clone lineage is a refusal rather than an absence. **A
stream cannot refuse the way a statement can.** There is nobody to tell; the producer has moved
on; and stopping the pipeline for one bad record makes one malformed document an outage.

So the ADR must decide: what is quarantined, for how long, who is told, and when a run of bad
records stops the pipeline rather than sidelining them one at a time. A quarantine with no
lifetime is `RSK-35` again --- the accumulation nobody is responsible for --- so it needs the
expiry treatment rehydration got.

It must also decide **what a config may not do**: silently widen a type, invent a value for a
missing key, or accept a document whose keys it has never seen. Each is a way to turn a source
defect into published data that looks fine.

### Progress, 2026-08-31

**Built, tested and wired.** The declaration and its validation, the binder, the stop control,
the quarantine, the position, the source reader, the batch assembly, and the runner that joins
them. The server loads declarations from `config/feeds/`, runs each on its own cadence, and
drops a feed that stops rather than retrying it. `sankhya-feed` is off the `UNREACHED` list,
which is the mechanical statement that it is reached rather than merely built.

(Written without a count of this crate's own tests, deliberately: `check-doc-numbers` reads any
figure of the form *"N tests"* as the workspace total and rewrites it, so a per-crate figure in
prose becomes a false claim on the next sync.)

**Quarantine expiry is built** in `sankhya-maintenance::expire`, as partition detach rather than
row deletion --- `DEC-23` gets no exception here, and detaching stays reversible until
retirement's grace period runs, which is what makes doing it automatically defensible where
deleting would not be. It refuses wherever it would have to guess: a partition whose date cannot
be read, and a file at the table root belonging to no partition, are both left alone. A
partition is kept until the **longest** retention any feed declares has passed, because one
quarantine holds several feeds' records and the alternative is a feed destroying data it did not
produce by editing its own configuration. **The maintenance tick calls it**, on the same thread
that already retires and compacts, so quarantine expiry is not a separate schedule anybody has to
remember to arm.

**The soak has an ingest arm.** It writes documents into a spool every round, one in twenty
deliberately malformed --- under the stop rate so the feed runs throughout, above zero so the
quarantine path is exercised under load. It is the only arm whose statement is exact rather than
statistical: every document written is known, so the published and quarantined counts must
*equal* the sound and malformed counts, and a discrepancy is a row that arrived twice or did not
arrive.

### The command surface a halted feed needs, and what was built for it

`ADR-0018` requires that a stopped feed be **visible** and that resuming be an act somebody
performs. The halted set used to live in the server's feed task, which meant it was visible in a
log line at the moment it happened and nowhere afterwards --- an operator arriving an hour later
had no way to ask. Three requirements followed, and all three are now built: the state lives in
`sankhya-feed::state`, and `SHOW FEEDS` and `RESUME FEED <name>` reach it from a client.

1. **A feed is a thing with a state**, readable from a client: its name, whether it is running
   or halted, why, and what its last run did. Held where a query can reach it rather than in a
   task's local set.
2. **Resuming is a statement**, not a restart. Restarting the server to resume one feed takes an
   outage on every other.
3. **Resuming does not forget.** A feed that halted, was resumed, and halted again for the same
   reason is a different situation from one that halted once, and an operator should be able to
   tell without reading a log. `Feeds` therefore keeps the halt count across a resume rather than
   clearing it, and a resume of a feed nobody declared is refused by name rather than creating an
   entry for it.

Three things the ADR did not name turned up while building. A feed must **declare its date axis**,
because `DEC-34` requires the date to be declared per table and never defaulted, and a feed is
where a table's rows come from; a date column that is nullable, absent, or not a date is refused
by name. And the ADR's source-level stop control was written here as a *threshold* first, which
made a two-record file with one bad record stop the feed --- the same mistake as stopping on the
first malformed document, one level up. It is now an unambiguous condition: a source that
produced **nothing** usable stops the feed, and everything between that and one bad record is a
rate, which the window measures across sources.

The third is the one the soak found: the position is a high-water mark, so it **cannot**
distinguish a source that arrived late from one finished last week. The ADR is amended with the
decision that follows --- never re-ingest, count what was skipped, and say so.

### Work

Six decisions came out of the ADR, and two of them are work this section did not previously
name: the quarantine is **a table** rather than a directory beside the warehouse, and a file's
**position is committed with its rows in one commit**, so a restart is a question with an answer
rather than a reconciliation exercise. A halted pipeline is also a *state*, which means a command
surface to see it and to resume it, rather than a flag in a file.

**Built so far**, everything that has no I/O in it: the declaration and its validation, the
binder, the stop control and the quarantine's schema and fingerprint. Two decisions were made
while building that the ADR did not name. A feed must declare **where its rows' date comes
from** --- `DEC-34` requires that per table and a feed is where a table's rows come from, and
the two meanings produce the same column while answering the same query with different rows.
And the source-level stop is **not** a threshold: it fires only when a source produced nothing
usable at all, because a two-record file with one bad record trips any fraction worth setting,
and stopping a feed for that is the same mistake as stopping it for its first malformed
document. Everything between one bad record and an unusable source is a rate, and the window
measures rates --- it spans sources, so a run of half-bad files still stops the feed.

The config model and its validation, reporting every failing rule rather than the first. The
mapping from a JSON dictionary to typed rows against `sankhya-schema`'s logical types, refusing
what `canonical_encoding` refuses. Microbatch assembly bounded by size *and* by time, because
either alone stalls. The quarantine and its expiry. The command surface. A soak that ingests
under load and reconciles what arrived against what was published.

### Exit

Ingest demonstrated end to end from a file to an OLAP answer; a malformed record shown to be
quarantined rather than coerced or dropped; a run of bad records shown to stop the pipeline
loudly rather than quietly; every refused config shown to fail closed; and the quarantine shown
to expire rather than accumulate.

**Demonstrated 2026-09-01** in `crates/sankhya-server/tests/feed_exit.rs`, through the real
binary and the real wire protocol rather than through the library the criteria are about. That
distinction earned itself immediately: `SHOW FEEDS` had never worked over a socket, because the
wire layer answers `SHOW <anything>` as a session setting before the handler sees it, and every
test of the command surface called the handler directly.

One criterion cannot be shown by waiting. Expiry is measured in **days**, and a partition
written today is expired by no retention at all --- the cutoff is `today - retain_days`, and
today is never before itself, which is deliberate: a record refused an hour ago is precisely the
one somebody is about to come looking for. Hand-writing a partition directory with yesterday's
date would make the fixture encode the storage layout, and moving the process clock would make
every later failure a story about the clock. So expiry is demonstrated by calling the
deployment's own expiry with a *stated* day --- `plan` takes it as an argument for this reason
--- over a quarantine the real feed really wrote, with a separate test for the only thing that
leaves unproven: that the tick calls it with the real calendar, unasked.

---

## 13e. M14 — The client contract and the Python SDK

**After M13. Schedulable now.** Added 2026-08-31 by owner directive.

### What it is, from the outside

Somebody installs a package, connects to a running SANKHYA over a network, and uses it: lists
what is there, queries it, ingests into it, declares and materialises cubes, clones a table,
registers an aggregation of their own. **Python first**; Java and Rust follow in `M16` and are
the reason the contract matters more than the binding.

### The gate

Two things, both before implementation.

**`FR-SEC-03` is mandatory and half of it is now built.** Transport security landed on
2026-08-31 --- `sankhya-tls`, both doors, the wire protocol's negotiation, and the refusals
around a half-configured certificate. What follows is the original reasoning for the gate, kept
because the second half is still open.

**Nothing implements identity.** *"Authentication SHALL support federated
identity tokens, mutual TLS, and scram authentication on the wire-protocol door."* Neither door
offers TLS today, so a password crosses an unencrypted socket --- tolerable on a loopback, and
credential exposure the moment the client is somewhere else.

The dependency half is already paid: `rustls` 0.23.43 resolves as a **single version** in the
lock, pulled transitively through the object-store HTTP client, and `tonic` has a TLS feature of
its own. So the pin-set question under [ADR-0001](adr/0001-dependency-pin-set.md) is a check to
run rather than a risk to carry, and the work is wiring, certificate handling and the refusals
around them. **An SDK whose whole purpose is
connecting over a network cannot ship in front of it.**

**An accepted ADR for the client contract.** Written and accepted on 2026-08-31 as
[ADR-0017](adr/0017-the-client-contract.md), covering:

1. **What the SDK is allowed to know.** Three bindings are coming. Any validation the client
   performs and the server does not becomes a specification the other two will not share, and
   the divergence surfaces as "it worked in Python". The rule to decide: **the SDK contains no
   logic the server does not also enforce.**
2. **How an error survives the wire.** The server's refusals carry a `SQLSTATE`, a code and a
   remediation --- *"drop it first"*, *"materialise them first"*, *"plan again and have it
   read"*. An SDK that renders those as a string has thrown away the half that says what to do.
3. **How a large answer is returned.** `MAX_RESULT_ROWS` bounds a result today. A client asking
   for a hundred million rows must stream, and a binding that materialises before yielding turns
   a working query into an out-of-memory kill on the client.
4. **What happens to a long operation.** Materialising a cuboid, taking a backup, cloning a large
   table: whether the call blocks a connection, and what a client that disconnects mid-way has
   done.
5. **Version skew.** An SDK is installed independently of the server it talks to.
   `sankhya-version` already versions artefacts; the client contract needs the same treatment,
   and a mismatch must say so rather than fail somewhere specific.

### Where ADR-0010 gets built

[ADR-0010](adr/0010-external-aggregations.md) is `Proposed` and decides the hard half already:
an external aggregation is a **contract rather than a function** --- `accumulate`, `merge`,
`finish`, `state` --- where **the presence of `merge` is the composability declaration**, no
`merge` means `Rule::None`, what materialises is the *state* rather than the number, and
determinism is **exercised rather than trusted**.

It left exactly one thing open, and the owner decided it on 2026-08-31: **out of process, behind
Arrow IPC.** Slower per call, isolated and killable. A panicking or looping aggregation is a
sidecar that dies rather than a query engine that takes the audit chain and every other tenant
with it. Embedded `pyo3` stays available behind the same contract, later, justified by a
measurement rather than by preference.

### Cloning, which is built and not yet reachable from a client's hands

`M10` shipped `CREATE TABLE ... CLONE` and the drop that refuses to break a clone, so any client
that executes SQL already has both. Two things are missing for it to be *usable*:

- **Nothing surfaces lineage.** `Lineages` resolves it server-side and no client can ask *"what
  is this a clone of?"* or *"what still reads this?"*. A refusal that names what would break is
  no use if the client could not have known beforehand.
- **Time travel is refused and never offered.** `may_read_as_of` guards a moment before a clone
  existed, and there is no way to read a table as of a moment at all.

### The server grows first

[ADR-0017](adr/0017-the-client-contract.md)'s first consequence, and the thing that makes this
milestone larger than *"write a Python package"*: a binding may contain no logic the server does
not enforce, so anything a client shows must be something the server can be asked. Four of these
do not exist, and none of them is client work.

1. **Lineage, from a client.** `M10` records where a clone came from and `Lineages` resolves it
   server-side; nothing surfaces it. Without this a user can create a clone and never ask what
   it is a clone of, which makes its numbers unplaceable.
2. **Dependents, from a client.** `may_drop` refuses a drop that would strand a clone and names
   the clones --- *after* the attempt. A refusal that names what would break is no use to
   somebody who could not have asked beforehand, and an interface that creates clones freely and
   never surfaces them quietly grows a warehouse.
3. **Refusals as data.** The wire carries a sentence today. The contract needs `code`,
   `sqlstate`, `remediation` and **the names a refusal cites** as separate fields --- the last
   being the one that is cheap now and expensive later, because a client that must parse names
   out of prose turns the message into an API nobody may reword.
4. **A version handshake.** A mismatch must be refused at connection, naming both versions,
   rather than surfacing eleven calls later as a missing field.

Only then the binding, which is thin by construction --- and thin is what makes three of them
agree.

**Where it lives**, by owner directive 2026-09-01: `sdk/python/` at the repository root, beside
`sdk/java/` and `sdk/rust/` when `M16` builds them, each with **its own quickstart**. Not under
`crates/`, which is a Cargo workspace and would be a lie about what builds two of the three; not
three repositories, which would be three release cadences and three chances for a binding to
fall behind the server it is thin over. Recorded as `ADR-0017` Decision 7a.

### Work

TLS on both doors. The client contract and its ADR. The Python package: connect, discover, query
with streaming results, ingest, cube declaration and materialisation, clone and lineage,
registered aggregations through the out-of-process contract. Ephemeral cubes with a **mandatory
expiry**, because a cube that materialises cuboids and is never dropped is the accumulation
`RSK-35` describes wearing a different costume. A gate that runs the SDK's own tests against a
real server, because a client tested against a mock is a client tested against its author's
belief.

**A worked example per capability**, by owner directive 2026-08-31 --- not a README snippet but a
runnable `examples/` tree the owner can point at a server and use to exercise the product:
connecting, discovery, a streamed result larger than memory, ingest, each of the five cube
navigations, an ephemeral cube and a persisted one, a clone and its lineage, a registered
aggregation, and one example per refusal that shows the typed error and its remediation.

Examples are held to the same standard as tests, for a sharper reason: **an example that does not
run is documentation that lies**, and it lies to the person least able to tell. So they run in
the gate against a real server, and one that breaks fails the build like anything else.

### Exit

A user connects over TLS from an unmodified Python installation and, without touching the
server's filesystem: queries a table larger than the client's memory; ingests a file; declares a
cube, materialises it, and rolls it up; clones a table and reads the clone; registers an
aggregation whose `merge` is exercised and whose determinism is checked; and receives a typed,
remediable error for each refused path. Every one demonstrated against a running server rather
than a mock, and **every capability above has a runnable example in the tree** that the gate
executes.

---

## 13f. M15 — Ingest without a file: Kafka, and streaming from a client

**After M14.** Added 2026-08-31 by owner directive. **Streaming ingest from the SDK added
2026-09-01 by owner directive**, and put here rather than in `M14` because it is the same
problem as Kafka wearing different clothes.

### What it is

The same config-driven ingest as `M13`, from two sources that are not a directory of files. A
config names a topic; a client opens a stream. Everything downstream is `M13`'s --- which is
the point of having done files first.

### Why the two belong together

`M13`'s position is a **high-water mark over source names**, read in order, committed with the
rows. That is what makes a restart neither duplicate nor skip, and **neither of these sources
has a source name**. A topic has partitions and offsets; a client stream has nothing at all
until this decides what it has.

So both need the same three answers, and answering them once for one source and again for the
other is how two ingest paths come to disagree about what "already ingested" means:

1. **What a position is** when there is no file to anchor it to. An offset per partition is the
   obvious answer for Kafka and no answer at all for a client, which may reconnect as somebody
   else.
2. **What back-pressure means** when the producer is not a directory that waits patiently. A
   client that outruns the write path must be slowed rather than buffered, and a consumer that
   falls behind must be visible rather than merely slow.
3. **What a stop is** when there is nothing to stop reading. `ADR-0018` stops a feed and waits
   for a person; a client holding an open stream has to be *told*, in the refusal, rather than
   discovering it by writing into nothing.

### The client stream, specifically

Arrow Flight's `DoPut` is the mechanism and the columnar door already speaks it, so there is no
new transport. What is new is that the producer is a **caller** rather than a file: refusals go
back to somebody who is still connected and can fix the batch, which is the one thing a file
feed can never do --- and the reason a quarantine is not the whole answer here.

`ADR-0018` needs an amendment rather than a replacement: a record that does not fit is still
quarantined whole, and a client that is still on the line is also *told*, in the same statement,
which of its rows did not land.

### The gate

**A pin-set decision under [ADR-0001](adr/0001-dependency-pin-set.md).** A Kafka client is a
substantial dependency, and the usual one brings a C library with it. That is a project-level
event rather than a dependency bump.

**And an accepted ADR on how a consumer is tested without a broker.** This project vendors
PostgreSQL 17.11 to test capture against a real server, and its own rule is that a harness
driving its own code measures its own code. A fake broker is the easy answer and the one that
proves least; whether to vendor a broker, run one in the gate, or define a seam narrow enough
that the fake is honest is a decision to make before the code, not after.

### Work

The source, offset management and its durability, delivery semantics stated rather than assumed,
and the same quarantine `M13` built. A soak that consumes under load and reconciles.

### Exit

Messages consumed from a real broker and answerable by OLAP; a restart shown to resume without
loss or duplication; a malformed message quarantined; and the delivery guarantee stated and
demonstrated rather than claimed.

---

## 13g. M17 — Named snapshots

**Immediately after M14**, by owner directive 2026-09-01. Ahead of `M15` and `M16`, which is a
deliberate reordering: these four sections are what makes the system usable for the analysis it
was built for, and a Kafka consumer does not help anybody who cannot pin a consistent read.

### What it is

A **name** for a consistent position across many tables, pinned so the files it references stay
alive, and quotable by a query or a run.

### Why it is not a clone

A clone pins **one table at one version**. A market-risk run reads the trade population, the FX
rates, the curves and the hierarchy, and it must read all of them **as of one instant** ---
otherwise the reconciliation problem this system exists to remove reappears *inside a single
query*.

The machinery is already there and has no surface: the read path takes a target position and
splices tiers against it, and `read_as_of` is that position read once at startup. What is
missing is a way to name a position, keep it, and hand the name to something.

Nearly free, because nothing is copied: a snapshot is a label on a consistent point plus a rule
that keeps its files alive --- the same reclamation machinery a clone already uses.

### The gate

**An ADR before any code**, answering: what a snapshot pins when a table is created *after* it;
what a read of a snapshot that has been reclaimed says; whether a snapshot may be taken of
tables the caller cannot read; and whether a snapshot expires, given `RSK-35`.

**Written and accepted 2026-09-02** as [ADR-0019](adr/0019-named-snapshots.md). Six decisions.
A snapshot is a **position, not a table** --- a clone freezes a thing, a snapshot freezes a
moment. A table created after the snapshot is **refused rather than answered as empty**, because
a table that did not exist is not a table that was empty, and a join against it would return a
confident zero. The expiry is **mandatory**, the third mechanism here to carry one after the
quarantine and the ephemeral cube: anything that keeps data alive on somebody's behalf must say
for how long. A snapshot is **not a permission cache** --- every read is authorized against the
reader's own entitlements, so two people reading one snapshot may legitimately see different
rows. And it is quoted as a **session setting**, because a run reads one instant across many
statements rather than one.

**Its Decision 6 found a live defect and it is already fixed.** `SET` is accepted as a no-op
because nothing reads a session setting --- true, until `SET SNAPSHOT`, which would be the first
setting to change an answer. Accepting it quietly would serve the present to a caller who asked
for one instant, with no symptom. Settings that would change an answer are now refused by name
until `M17` honours them, which is `DEC-47`'s rule applied where it was about to be broken.

**Decision 7 was added 2026-09-02 by owner directive**, after the question *"is this a git-like
view of history?"*. A snapshot is a **tag**, not a log: `SHOW HISTORY OF` and `SET VERSION OF`
are in scope, and a **row-level diff is deferred to `M20`** rather than attempted.

### Exit criteria

Each is demonstrated **through the front door a user has** --- the shipped binary and the wire
protocol --- rather than through the library the criterion is about. That distinction is not
pedantry: it is what caught `SHOW FEEDS` in `M13`, where every unit test passed and the
statement had never once worked over a socket.

1. **A snapshot is taken, listed and dropped from a client**, and `SHOW SNAPSHOTS` reports what
   each pins and **who took it** --- a cost with no visible owner is one nobody reclaims.
2. **A read as of a snapshot answers the past across more than one table in one session.** More
   than one, because a snapshot that pinned each table at its own moment would be a clone with
   extra steps, and one table cannot show the difference.
3. **A table the snapshot does not name is refused**, in the same words as a table that does not
   exist, and never answered as empty.
4. **What a snapshot pins reaches the sweeper, and survives a restart.** A snapshot that pins
   nothing is a promise the system does not keep: the report it exists to reproduce stops
   reproducing when the files it read are reclaimed.
5. **A table's history is readable, says what is keeping each version alive by name, and refuses
   a version it cannot honour** --- both a version the log does not contain and a version whose
   files retirement has taken. Answering either would be a historical query silently missing
   what was compacted.
6. **The expiry cannot be omitted or evaded.** No default, no unbounded form, and a lifetime
   longer than this system will hold storage for is refused at the statement.

---

## 13g-b. M21 — The built-in function catalogue

**Owner directive 2026-09-02:** the catalogue must be *very wide and very expansive* — linear
algebra, matrices, calculus, statistics, mathematics, vector mathematics, Excel-style functions,
graph functions — reachable from **both** the SQL surface and every SDK, and vectorized.

Decided in [ADR-0020](adr/0020-the-built-in-function-catalogue.md).

### Why breadth is the point

A user who has to leave the warehouse to do arithmetic has left the warehouse: they pull the
rows into pandas, and from that moment this is a file server. Every function that exists here is
a reason for the computation to happen where the data already is, which is the only place it is
cheap.

### What was found on the first look

**Twelve kernels were already written, tested, and unreachable from SQL** — all five
element-wise vector operations, both quantile kernels, the whole of the calculus module bar one
integrator, `standardise`, `range` and `linear_fit`, the last being linear regression. This is
the failure this repository keeps finding: a surface built, unit-tested, mutation-tested, and
never given a front door.

**Closed 2026-09-02, and the recurrence made a build failure.** `check-kernels` reads every
`pub fn` in `sankhya-math` and requires that some crate outside it name the kernel, or that an
`INTERNAL_KERNELS` entry say why a user would never call it. "We will expose it later" is not
an accepted reason: that is what this section is for, and a kernel awaiting exposure should
fail the gate until it has a name.

The check has its own test, on a fixture where a stranded kernel is planted and the check must
fail — because a gate nobody has watched fail is the same defect it exists to catch, one level
up.

**And the reductions were the wrong shape.** Every vector function reduced through a sum that
sorted its input by magnitude, once per row. That is fixed — see ADR-0020 Decision 3, and the
`exact_sum` written for it, which is bit-identical, order-independent by construction, and
**3.3× / 3.0× / 3.0×** cheaper than the sorted-expansion route at 64, 512 and 4,096 values
([bench: sankhya-math/deterministic-sum]) --- a cost-of-route comparison, since the fallback runs
only where the fixed-point route declines. This said "1.3× to 3.6× faster", which `ADR-0020`
retracts as unreproducible.

### The work

1. **Expose what exists.** The ten kernels above, named and documented on the SQL surface.
2. **A crate of its own**, `sankhya-functions`, with one registration point and a `functions()`
   table function so a client can enumerate the catalogue — for the reason `cubes()` exists: a
   capability nobody can list is a reference manual nobody reads.
3. **Widen it.** Linear algebra beyond `solve` and `inverse` — decompositions, eigenvalues,
   rank, pseudo-inverse, norms. Statistics beyond the descriptive — regression with diagnostics,
   distributions, hypothesis tests, rank correlation. Calculus beyond the trapezoid —
   interpolation, root finding, smoothing. Time series and financial. **Excel-compatible**
   functions, under Decision 6's rule: agreeing with Excel including where Excel is arguably
   wrong, or named differently and saying why.
4. **Every function reachable from every binding**, with runnable examples, gated as tests.
5. **A benchmark per category**, because ADR-0020 Decision 3 requires a number rather than a
   claim.

The list of functions is deliberately **not** fixed here. Enumerating it in the plan would make
every addition a plan amendment; what is fixed is the shape, the guarantees and the two-surface
obligation.

### The gate

The ADR, accepted 2026-09-02, before any implementation.

---

## 13g-a. M20 — What changed between two versions

**Deferred here by owner directive 2026-09-02**, when `SHOW HISTORY OF` and `AS OF VERSION` were
scoped into `M17`. Named now rather than left as a wish, so the reason it is separate survives.

### Why it is not part of M17

`M17` gives a **tag** and the ability to read at one. Asking *what changed between two of them*
is a different question and it has no obvious answer, which is the bar for a design gate here.

The log records **file-level** adds and removes. A file is added, another is removed, and
nothing in the log says which *rows* differ --- a compaction rewrites files without changing a
single row, and would show as a total replacement. So a naive diff would report a compaction as
though the whole table had changed, which is worse than no diff at all: it looks like an answer.

### What it must decide

1. **What a difference is.** File-level is cheap, truthful and useless to a person. Row-level is
   what anybody means and needs a key --- and this system has no primary key, only a date axis.
2. **What it costs.** A row-level difference over two versions of a large table is a join over
   both, which is a query rather than a lookup, and pretending otherwise sets an expectation
   nothing can meet.
3. **Whether a compaction is a change.** It is not, to a reader, and it is to the log. Anything
   that reports it as one will be ignored within a week.

### The gate

**An ADR before any code**, answering those three. Until then, `SHOW HISTORY OF` reports what the
log honestly knows: versions, times, and files added and removed.

---

## 13h. M18 — Derived results: materialised queries and user merge functions

**After M17.** Two capabilities that share one body of machinery, which is why they are one
milestone rather than two.

### Materialised ordinary queries

[ADR-0014](adr/0014-materialized-views-and-the-cube-lifetime.md) has been **Proposed since
2026-08-28** and this closes it. A maintained cube is already a materialized view in every
respect that costs engineering effort --- declaration, versioning, a staleness target that is
checked rather than estimated, refresh with no caller, reclamation of superseded results,
serving under policy with completeness carried through.

What a cube cannot express is a derived result that is **not an aggregate**: a denormalising
join produces rows, not cells, and has no measures and no additivity. It therefore cannot answer
a coarser question from a finer stored one --- every query either matches the view or does not.
That is a real difference in what *refresh* and *serve* mean, and it is why "reuse the cube
lifetime" is nearly right and therefore dangerous.

### User-supplied merge functions

**Owner directive 2026-09-01.** A measure's composition rule becomes extensible: `Sum`, `Count`,
`Min`, `Max`, `First` and `Last` remain the arithmetic defaults, chosen explicitly and never
defaulted, and a measure may instead name a **user-supplied merge function**. Python first, then
Rust, then C++ and Java --- each through the same contract.

This runs through [ADR-0010](adr/0010-external-aggregations.md)'s decided mechanism: **out of
process, behind Arrow IPC**, killable. A looping or panicking merge is a sidecar that dies, not
a query engine that takes the audit chain and every other tenant with it.

**The contract is the author's.** A merge function must be associative and commutative where the
lattice composes in an order the planner chooses, and *by owner decision the author guarantees
that* --- the server does not verify it as a precondition. That is a deliberate trade: the
alternative refuses useful functions it cannot prove things about.

**What the server owes in return is visibility.** A cube composes a coarse answer from finer
cuboids in whatever order the plan picks, so a merge that is not associative returns different
numbers on different days, each individually plausible. So:

1. The rule is **declared and readable** --- it appears in `cube_measures()` and in the audit,
   never inferred.
2. A **diagnostic** composes a sample of real cuboids in several orders and reports disagreement.
   It reports; it does not block. An author who wants the check has it, and one who knows their
   function is fine pays nothing.
3. A merge that **fails** --- the sidecar dies, the call times out --- is a refusal, never a
   silent fall back to `Sum`. Falling back would answer with arithmetic the author explicitly
   rejected.

### Why it is worth the risk

The prize is composability for measures that have none today. A variance composes from
(count, sum, sum-of-squares); a P&L attribution or a netting rule composes by a rule only its
author knows. Without this each is `Rule::None` --- correct, and meaning *"rescan the base"*,
which is the cost the whole cuboid mechanism exists to avoid.

---

## 13i. M19 — The data lifecycle policy

**After M18.** Owner directive 2026-09-01.

### What it is

One declaration governing how data ages across **both** tiers: rows older than a stated age leave
the transactional store, a whole table can be pushed across on demand, and compaction, tiering
and expiry are hooks on the same policy rather than separate schedules.

### The reframe that makes it safe

**Nothing moves.** Capture already publishes every transactional row into the analytical tier, so
*"move rows older than N days to OLAP"* is really: **release** them on the transactional side,
because the analytical copy already exists.

That changes the risk completely. A move is a copy plus a delete with a window where both or
neither exist. A release is a deletion **gated on proof** that the data is already elsewhere ---
and reconciliation, a shipped command and a live metric, is what produces that proof.

### The four properties

1. **Release is gated on proof, never on a timer.** A partition leaves when reconciliation says
   every row of it is published and verified. Products that move on schedule and reconcile never
   are how an estate acquires a permanent function whose only output explains why two systems
   disagree on a row count.
2. **Release is a partition detach, never a `DELETE`.** `DEC-23` earns no exception on the
   transactional side either. Detached is reversible for a grace period, exactly as quarantine
   expiry is.
3. **A read of released data is refused by name.** *"That range moved to the analytical tier on
   2026-04-01"* --- never a short answer presented as complete. This is the one nearly every
   product gets wrong: data ages out, the application returns fewer rows, the query looks fine
   and nobody notices for a quarter.
4. **One document governs both tiers.** Today an estate keeps transactional retention in one
   team's cron and analytical retention in another's, and they drift. No vendor ships this
   because no vendor owns both halves.

### The gate

**An ADR before any code.** The question with no obvious answer is the third property: what a
query asking for released data is told, and whether a policy may instead make the two tiers
answer as one.

---

## 13j. M16 — The Java and Rust SDKs

**After M14.** Named here so that `M14`'s contract is written for three bindings rather than
retrofitted to them. Each is a binding over the contract `M14` specifies, and neither may carry
logic the server does not enforce.

---

## 14. Parallelisation and critical path

```
M0 ──┬── M1 ──┬── M2 ─────┐
     │        └── M3 ─────┼── M4 ── M5 ── M6 ── M7 ── M8 ── M9
     └── graph primitives (pure, no dependencies) ─────┘
```

- **M2 and M3 run in parallel**, joined by the table-format trait and the in-memory implementation.
- **Pure graph algorithm work has no dependencies and can start in week 1** as parallel work for anyone blocked.
- **Documentation scaffolding runs alongside everything.**
- **Critical path:** M0 → M1 → M3 → M4 → M5 → M6 → M7.

---

## 15. Engineering practice

### 14.1 Definition of done

1. Implemented with tests at the level appropriate to the layer.
2. The requirement's test identifier exists, runs in the pipeline, and gates the build.
3. **Documentation updated in the same change.** A change altering behaviour without altering documentation is incomplete.
4. Performance-sensitive changes post before-and-after measurements, checked against **allocation counts and bytes scanned as well as elapsed time** — a small latency win that doubles allocations is a deferred outage.
5. Security-relevant changes ship a negative test proving the failure mode is prevented.
6. Changes with a stated failure mode ship a fault-injection test asserting the **named** recovery behaviour.

### 14.2 Pipeline

Stages are ordered cheapest-first and the pull-request pipeline has a **hard twenty-minute budget**; beyond that engineers stop reading it and every downstream gate becomes theatre.

| Stage | Contents |
|---|---|
| Pre-flight | Formatting, layer check, file-length check, dependency-direction check, **domain-vocabulary lint**, documentation staleness |
| Lint | Full lint at the default feature set **and** at no default features — feature-combination breakage is the commonest pipeline gap |
| Unit | Pure-layer suites, which must complete in seconds |
| Doc tests | Run separately, because the main test runner does not execute them and examples silently rot otherwise |
| Integration | Real database across supported versions, object storage, sharded |
| SQL semantics | The conformance corpus, including isolation and cross-engine parity |
| Coverage | Ratchet against baseline plus per-change diff coverage |
| Supply chain | Advisories, licences, **duplicate-version detection producing no output**, bill of materials, breaking-change detection on the extension API |
| Performance | Deterministic instruction-count benchmarks, **blocking**; the public analytical suite at small scale, advisory |
| **Pack matrix** | Each pack built and tested **independently**, plus a **pack-free core build** |

Heavier work moves to the merge queue and nightly: full feature matrices, release builds, extended fuzzing, chaos and reconciliation, mutation testing, and large-scale benchmarks on a dedicated, pinned, non-virtualised runner.

### 14.3 Performance discipline

Three benchmark tiers: microbenchmarks per crate; **deterministic instruction-count benchmarks that are pull-request-blocking**, because elapsed time in shared infrastructure is far too noisy to gate on; and full-system benchmarks on dedicated hardware.

Baselines are committed in-repository and changed only by a reviewed change. **A baseline that updates itself measures nothing.**

Any change touching the scan path, the storage layer, the graph core or the protocol decoder must post a before-and-after table.

### 14.4 Testing

| Level | Scope |
|---|---|
| Unit | Pure layers; whole suite in seconds. If it slows, the seam is in the wrong place |
| Property | Protocol decoding; fixed-point arithmetic; order-statistic conventions; commit-conflict resolution; canonical encoding; incremental-equals-full graph hydration |
| Snapshot | Query plans; the error catalogue; the configuration schema; **the public API surface**, so a breaking change is a visible diff rather than a customer's discovery |
| Integration | Real database across versions; real object storage; **the embedded path tested separately**, since it is a different code path and the one customers will run |
| SQL semantics | Conformance corpus; the isolation suite under every feature combination; **cross-engine parity with a documented accepted-difference list** |
| Determinism | The same scenario twice, byte-identical metadata and output |
| Fuzz | Every boundary parsing untrusted bytes. **A crash is a priority-one defect with a same-week commitment** |
| Fault injection | Each scenario asserting a **named** recovery behaviour |
| Reconciliation | Against an independent model, under concurrent chaos, at increasing durations |
| Mutation | Numeric, protocol, apply and policy crates, with stated minimum scores |

### 14.5 Dependency maintenance

A **standing quarterly capacity allowance with a named rotating owner.** The query engine ships majors frequently and the storage libraries are pre-1.0 with breaking changes each release. If this is not in the plan it will be done badly under time pressure.

Upgrades follow a documented procedure: bump the pin set atomically; confirm no duplicate versions; fix API churn; **review plan snapshots rather than bulk-accepting them**, since plan churn is the signal that tells you what the optimizer changed; run the semantics corpus; run benchmarks against the baseline; update the decision record and changelog in the same change.

---

## 16. Top risks to delivery

| Risk | Response |
|---|---|
| The pin-set spike fails and the read-path architecture must change | **Two days in M0, before anything depends on it.** This is why it is first |
| Exactly-once capture is subtly wrong and found late | Reconciliation from M2, continuously, against an independent model; deterministic simulation with printed seeds |
| Performance treated as a late-phase concern | Harness and baseline in M0; pull-request-blocking gates from the start |
| The core is domain-agnostic in name only | Two opposite reference packs against an unmodified core; the zero-core-files test; a pack-free build |
| Extension API becomes a dumping ground | Size budget, two-domain rule, no escape hatches, mechanical breaking-change detection |
| Build times destroy the development loop | Minimal defaults, caching, heavy stages off the pull-request path, a hard time budget |
| Scope growth | Explicit exit criteria; documented non-goals; a demonstration every milestone |
| Tiering ships before reconciliation is proven | An explicit gate that schedule pressure cannot override |

---

*This plan is maintained under version control. Milestone changes require an update to the roadmap and, where scope changes, to the requirements.*
