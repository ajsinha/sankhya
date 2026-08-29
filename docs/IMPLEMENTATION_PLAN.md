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
**Status:** Implementation — M0–M7 complete, M8 next
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
| **M8** | **Concurrency and data safety**, crate hygiene, then scale-out, HA, disaster recovery | 25–32 | weeks 32–43 |
| **M9** | Tiering *(gated — see §13)* | 12–16 | after M8 plus the reconciliation gate |
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

## 12. M8 — Concurrency and data safety, then scale-out

**Weeks 32–43 · 25–32 ew**

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

**The remaining work.**

1. **Widen `check-surfaces` to reachability.** Every crate must be reachable from a binary, or
   listed with a reason and a milestone. That one change catches all five stranded crates and
   forces a decision on all ten empty ones, instead of leaving both to be rediscovered.
2. **Wire or delete**, one decision per stranded crate, recorded. Wiring `sankhya-alloc` is
   near-free and returns allocation figures the soak currently cannot see.
3. **Adopt or delete the empty crates.** A crate with no code and no milestone is a claim the
   repository makes about itself and does not keep — and it inflates a "55 crates" figure that
   should describe what exists.
4. **Resolve the name collisions** — `telemetry`/`metrics`, `oltp-pg`/`api-pg` — by deleting or
   renaming, so a reader does not have to open both to learn which is real.

**Not consolidation for its own sake.** The three-way splits — `cube`/`cube-algo`/`cube-sql` and
`graph`/`graph-algo`/`graph-sql` — are load-bearing and stay: the zero-dependency algebra crates
are what make their property tests fast enough to exhaust rather than sample. The finding is
about crates that are *unreachable* or *empty*, not about crates that are small.

### 12.2 Scale-out, availability and recovery (16–20 ew)

Attached mode as the production configuration. Leader election through the transactional store. Stateless executor scale-out and query routing with cache affinity. Graph node partitioning with published rebuild times. Cross-region replication and recovery objectives per tier. Key management integration. Metering and chargeback.

**One seam remains *designed* here and built later**, near-free now and an expensive retrofit: allowing a table reference to resolve to a shard set. The other — keeping the commit path per-table rather than globally serialized — is no longer a seam. It is exit criterion 4 below, because the cheapest way to satisfy every safety criterion is one lock over the warehouse, and that is the outcome criterion 4 exists to forbid.

### Exit

**Safety.**
1. N writers racing for one commit version: exactly one wins and every loser is **told**, with no lost commit under sustained contention.
2. A reader never observes a partial file, for every file this system publishes, under a writer republishing continuously.
3. Reclamation running against continuous scans **never** deletes a file a reader holds — demonstrated under load, not argued from a grace period.

**Concurrency.**
4. Writers to different tables do not contend: throughput scales with writer count, and no global serialization point exists.
5. Read latency is flat under write load — readers are never blocked by writers.
6. Contention on a single table degrades by rebase-and-retry, bounded, so a runaway committer is a diagnosable failure rather than a hang.

**Scale-out.**
7. Multi-node deployment with executor scale-out demonstrated; failover tested under load; recovery objectives measured and published rather than estimated.
8. Soak criterion 7 carried from M6: the gRPC transport and every write path, plus the scheduled multi-day run.

---

## 13. M9 — Tiering

**After M8, and gated**

### The gate

Tiering may not ship until **all** of the following hold:

1. Continuous reconciliation has run clean in production across every table class for a sustained period.
2. The restore drill has passed repeatedly.
3. An archive attestation drill has passed on a non-production archive.

**The gate is explicit so that schedule pressure cannot quietly make this decision.** Purging the system of record before the copy is provably correct is indefensible, and no amount of care in the tiering code substitutes for demonstrated reconciliation.

### Work

The policy model and eligibility validation. The durable, resumable state machine. Exhaustive verification with canonical encoding. The archival registry. Cross-tier query unification with the total tie-break rule. The four-layer purge defence. Quarantine and its reaper. Rehydration with mandatory expiry. Whole-table migration. Command and schedule surfaces with plan digests, blast-radius limits, the anomaly guard and kill switches. Segregated authorization and the evidence pack.

### Exit
Purge demonstrated end to end with verification, quarantine and rollback; the anomaly guard demonstrated halting an intentionally-defective policy; every rejected purge path shown to fail closed.

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

**An accepted ADR before any implementation**, covering at minimum:

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

The ADR. The clone action in the log and its lineage record. Whatever the ADR chooses for
shared-file lifetime, with maintenance taught to honour it. Clone and drop surfaces with the
refusals enumerated above. Backup, restore and tiering made clone-aware. A soak that clones
under load, writes to both sides, runs full maintenance, and verifies both still read
correctly afterwards.

### Exit

A clone demonstrated at constant cost against a large table; divergent writes on both sides
verified independent; **maintenance run to completion on the origin with the clone proven to
read every row it could read before**; the same for orphan collection specifically; and every
refused clone path shown to fail closed.

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
