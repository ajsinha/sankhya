<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# SANKHYA — Implementation Plan

**Document ID:** SNK-IP-001
**Version:** 0.1.0 (draft for review)
**Status:** Implementation — M0–M5 complete, M6 in progress
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
| **M8** | Scale-out, high availability, disaster recovery | 16–20 | weeks 32–38 |
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

**10.7 Hardening (3 ew).** Fuzz corpus maturity. Mutation testing across the critical crates. A multi-day soak. Performance baselines locked. Operator runbooks — one per alert, as a shipped deliverable.

### Exit
1. The five-minute experience passes as a timed test.
2. Restore drill automated and passing.
3. Upgrade and rollback tested.
4. Multi-day soak clean.
5. A runbook exists for every alert that can page.
6. Every user-reachable error has documented remediation, generated from the same source as the catalog.
7. The control plane serves administration, tenancy, policy, catalog, health and jobs, and the REST gateway refuses a result too large for JSON by returning a Flight ticket rather than the rows. *(Carried in from M5 §9.6 on 2026-08-27.)*

---

## 11. M7 — Multidimensional analysis

**Weeks 28–34 · 14–18 ew**

*Added 2026-08-27 by owner directive. Placed **before** scale-out deliberately: this is a
capability the system is meant to be differentiated by, and multi-node deployment is table
stakes. Shipping the differentiator after the table stakes gets the order backwards.*

### Entry
M6 complete. M3's read path and M4's graph engine are the two things this builds on, and both
are done.

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

**11.6 Optional materialisation (2 ew).** Per-level, declared explicitly, never automatic,
carrying a staleness contract, with incremental refresh only where the measure forms a
commutative monoid per `FR-QUERY-27`.

**11.7 Write-back overlay (1–3 ew, `SHOULD`).** A separately versioned overlay for planning
and what-if analysis. Never modifies published data; a query states whether one was applied.

### Exit
1. A cube over a ragged parent-child hierarchy with alternate roll-ups returns totals that
   reconcile against an independently computed answer, with **no member double-counted**.
2. A semi-additive measure rolled up across time by summation is **rejected at planning
   time**, not computed.
3. Two runs of the same consolidation over the same snapshot are **bit-identical**.
4. Two principals with different row policies see different totals for the same cell, and
   both results carry a completeness measure saying so.
5. Slice, dice, roll-up and drill-down are demonstrated from SQL against a cube with at least
   six dimensions, with no cube-build step preceding the query.
6. A measure defined without an aggregation rule is refused, with the refusal naming it.

See [`adr/0007-the-cube-model.md`](adr/0007-the-cube-model.md) for why cubes are declared
views rather than a store, and why MDX is deliberately not planned.

---

## 12. M8 — Scale-out, availability and recovery

**Weeks 32–38 · 16–20 ew**

### Work

Attached mode as the production configuration. Leader election through the transactional store. Stateless executor scale-out and query routing with cache affinity. Graph node partitioning with published rebuild times. Cross-region replication and recovery objectives per tier. Key management integration. Metering and chargeback.

**Two seams are *designed* here and built later**, both near-free now and expensive retrofits: keeping the commit path per-table rather than globally serialized, and allowing a table reference to resolve to a shard set.

### Exit
Multi-node deployment with executor scale-out demonstrated; failover tested under load; recovery objectives measured and published rather than estimated.

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
