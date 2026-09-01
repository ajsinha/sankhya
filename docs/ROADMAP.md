<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Roadmap

**Document ID:** SNK-RM-001
**Version:** 0.1.0
**Status:** Implementation — M0–M8, M10 and M13 complete; M8's scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14 in progress
**Date:** 2026-08-30
**Companions:** `REQUIREMENTS.md`, `ARCHITECTURE.md`, `IMPLEMENTATION_PLAN.md`

---

## 1. What this document is, and is not

This is the **release-facing** view: what capability becomes available when, and what each release is *for*. The engineering sequencing — milestones, work breakdown, effort, entry and exit criteria — is in `IMPLEMENTATION_PLAN.md`. The two are kept separate because they answer different questions and change at different rates: a milestone slips without the release theme changing, and a release theme can be re-cut without re-planning the work.

Dates are deliberately absent. Releases are gated on **exit criteria**, not on calendar. Where a sequence is stated, it is a dependency order rather than a schedule.

---

## 2. The thesis

Three engines, three copies, three security models and a permanent reconciliation cost is the standard enterprise data architecture, and it is a tragedy repeated at almost every organisation of scale. SANKHYA's bet is that the modern Rust data stack has finally made it possible to collapse that estate into **one deployable artifact** — without giving up transactional guarantees, without giving up analytical speed, and without giving up the audit trail.

Three commitments shape every release below:

1. **General-purpose core, domains as packs.** The engine knows about tenants, tables, columns, edges, versions and policies. It knows nothing about any industry. This is tested, not asserted.
2. **Open storage.** Analytical tables are readable by other engines directly, in a layout that mirrors operational naming. SANKHYA is a participant in a data estate, not a replacement for it.
3. **Provable correctness over asserted correctness.** "Zero data loss" is a continuously measured metric with an alert, not a sentence in a brochure.

---

## 3. Release themes

### 0.1 — *Walking skeleton*
**Theme: prove the wire exists.**

One row travels the full path — transactional write, capture, storage, query, response — in one binary with no configuration. Every layer is a stub; the point is that the seams are real and the dependency graph resolves.

**Available:** nothing usable. **Purpose:** de-risk the foundation before anything is built on it.

---

### 0.2 — *It just works*
**Theme: the claim, demonstrated.**

Create a table in the transactional store, insert a row, query it analytically. No pipeline configuration, no table registration, no operator step. Write and then immediately read your own write, and see it.

**Available:** automatic table onboarding · automatic schema derivation · read-your-own-writes · the tiered read path in first form · mirror naming with collision refusal.

**Why this is the second release rather than an emergent property of the fourth:** it is the demonstration that makes the unified-system claim credible, and everything after it improves something that already works rather than advancing toward something that does not yet exist.

---

### 0.3 — *Trustworthy ingest*
**Theme: prove nothing is lost.**

Continuous writes under deliberate fault injection — process kills at randomized points, storage failures, volume exhaustion, clock movement, database restarts — ending with zero reconciliation discrepancies.

**Available:** exactly-once capture · consistent initial backfill · automatic schema evolution with quarantine · dead-letter handling · the source-safety escalation ladder · continuous reconciliation as a shipped command and a live metric.

**The release gate that matters:** with the applier deliberately stalled under sustained write load, retained log stays bounded, the transactional database keeps accepting writes throughout, and the system degrades on its own terms rather than being degraded by the database. **An analytical component must never be able to take down the transactional one.**

---

### 0.4 — *Fast, and provably so*
**Theme: numbers anyone can compare.**

Published performance against public benchmark suites, on named hardware, with the plans showing that pruning, late materialization and dynamic filtering actually engaged.

**Available:** the SANKHYA-owned table provider · physical storage design with tiered compaction · the statistics catalogue · the full caching hierarchy · admission control and resource governance · exact order statistics with a named convention · deterministic reduction · automatic maintenance.

**Why public benchmarks are primary:** they exercise query shapes a domain suite never will, and they are the only numbers a prospective user can compare against alternatives. An uncomparable benchmark is marketing.

---

### 0.5 — *Relationships, and extensibility*
**Theme: the third data model, and proof the engine is general.**

**Available:** the multi-relational temporal graph engine · epoch-based hydration bound to snapshots · graph results as SQL relations · the extension API · the declarative pack tier · two reference packs from unrelated non-financial industries · the extension conformance suite.

**The test that matters:** each reference pack's change touches **zero core files**. If a core built for one industry can serve two unrelated others without modification, the general-purpose claim holds. If not, it does not — and better to learn that here than after a customer commits.

---

### 0.6 — *Multi-tenant and governed*
**Theme: safe to run for more than one party.**

**Available:** tenancy enforcement across all three engines from a single choke point · row- and column-level security · federated identity · audit capturing decisions and data versions · encryption and key management · the columnar data plane, the wire-protocol front door, and the control plane.

**The test that matters:** every attempt by one tenant to reach another's data — through SQL, through the columnar surface, through a graph traversal, through a cached result, through an error message — fails, under every feature combination.

---

### 0.7 — *Operable*
**Theme: someone other than the authors can run it.**

**Available:** the diagnostic reporting time-until-impact rather than raw values · complete metrics and tracing · backup manifests binding all three artifacts to a consistent point · automated restore drills · packaging including a self-contained artifact for air-gapped deployment · tested upgrade and rollback · runbooks.

**The test that matters:** a first-time user reaches a running server with sample data and a successful query in five minutes, on a clean machine, with no container runtime, broker, object store or cloud credentials — as a timed test in the pipeline, so it cannot rot.

---

### 0.8 — *Multidimensional*
**Theme: the analysis people actually do, without leaving the system.**

Slice, dice, roll up, drill down and pivot as navigation of one declared structure, rather than as a sequence of unrelated `GROUP BY` statements the user reassembles in a client. **On demand, with no cube-build step preceding the query.**

**Available:** cubes as declared views over published tables — no second store · level-based and parent-child hierarchies, ragged ones natively rather than padded · alternate roll-ups and shared members with nothing double-counted · additive, semi-additive and non-additive measures, with the aggregation rule declared per dimension · deterministic consolidation · **both on-demand and materialised cuboids, chosen per cuboid and controlled by definition, configuration and session preference** · adaptive selection over the cuboid lattice under an operator budget, informed by the query log · materialised cuboids stored as ordinary published tables that Spark can read · write-back overlays for planning that never touch published data.

**Why here rather than after scale-out:** this is a capability the system is meant to be differentiated by. Multi-node deployment is table stakes. Shipping the differentiator second gets the order backwards.

**Three commitments that will be unpopular and are not negotiable:**

- **A measure with no declared aggregation rule is refused**, not defaulted to summation. A closing balance summed across twelve months is a number that means nothing and looks exactly like a number that does.
- **A cube returns bit-identical answers whether or not anything is materialised.** Materialisation is therefore a cache with no semantic content, not a second source of truth — which is what makes it safe to choose automatically. Almost nothing else in this category can make this claim, because it requires a deterministic reduction underneath.
- **A materialised cuboid cannot go stale.** It is keyed by the snapshot it was built from, so a new commit does not produce a stale hit — it produces a miss. There is no invalidation protocol and no time-to-live, and the "the cube is stale" failure mode every product here has is structurally absent rather than carefully avoided.
- **Two people may legitimately see different totals for the same cell**, because an aggregate is computed only over rows that principal may read. A total computed over rows the caller cannot see is a disclosure through arithmetic, and nothing about it looks wrong.

**The test that matters:** every query returns bit-identical results with materialisation on and off, and a cube over a ragged hierarchy with alternate roll-ups reconciles against an independently computed answer with no member double-counted.

See [`adr/0007-the-cube-model.md`](adr/0007-the-cube-model.md).

---

### 1.0 — *Production*
**Theme: the first release intended to be depended upon.**

**Available:** end-to-end concurrency and data safety · multi-node deployment with stateless executor scale-out · leader election · high availability and failover · cross-region recovery with measured objectives · metering.

**Concurrency comes first, and is a stated property rather than an implementation detail.** A
commit is never lost, no reader ever sees a partial file, and no file is deleted while it is
being read. Writers to different tables do not contend, readers are never blocked by writers, and
contention on one table degrades by retry rather than by waiting. Those are measured, not
asserted — see [ADR-0013](adr/0013-concurrency-and-data-safety.md).

**What 1.0 means here:** the extension API carries a stability commitment. Everything else may still evolve, but a pack written against 1.0 keeps working.

**And 1.0 is earned by one run, not by a checklist.** The project's exit criteria is a
twelve-hour, two-machine acceptance test: 100 GB through 50 concurrent readers and 20
concurrent writers against a single instance, building cuboids, querying them and dropping them
while saved data is queried and updated — all at once, because every defect this project has
found lived in an interaction rather than in a component. Not one lost commit, not one query
failed for a file deleted underneath it, and every materialised answer bit-identical to the same
answer computed from base data. See `IMPLEMENTATION_PLAN.md` §13c.

---

### 1.1 — *Lifecycle*
**Theme: data leaves the transactional tier safely, or not at all.**

**Available:** tiering with source purge · exhaustive verification · the archival registry · transparent cross-tier queries · rehydration · command and scheduled invocation with plan digests, blast-radius limits and an anomaly guard · segregated authorization and evidence packs.

**Gated, explicitly:** this release ships only after reconciliation has run clean in production for a sustained period across every table class, restore drills have passed repeatedly, and an archive attestation drill has passed. **Purging the system of record before the copy is provably correct is indefensible, and the gate exists so that schedule pressure cannot quietly make that decision.**

The recommended configuration even after release is *archive and verify continuously; purge deliberately, rarely, under dual control*. A deployment that never advances past archiving still gets most of the benefit at none of the risk.

---

### 1.2 — *Zero-copy clones*
**Theme: a copy of a table that costs nothing until somebody writes to it.**

**Available:** cloning a table or a whole schema at a version, in constant time and constant space · writes to either side diverging without touching the other · clones as first-class tables for reading, maintenance and time travel · a lineage record saying what a clone came from and at which version.

**What it is for:** the things people currently do by copying a warehouse. An analyst branch to try a transformation against real data; a pre-release environment seeded from production this morning; a what-if scenario in a cube that must not perturb the published one; a point to roll back to before a bulk correction. Every one of those is affordable only if the copy is free.

**Why it is scheduled here and not earlier.** A clone shares physical files with its origin, and that single fact reaches into every part of the system that assumes a file belongs to one table. **Retirement and orphan collection are the sharpest case: both decide a file is unreferenced by consulting one table's log, and under sharing that decision becomes wrong — the file may be the only copy of data a clone still reads.** Reference counting, or an equivalent, is not an implementation detail here; it is the feature. Shipping cloning on top of maintenance that cannot see across tables would delete a clone's data and call it tidying.

**Gated on design, explicitly:** an accepted ADR covering shared-file lifetime, the maintenance interaction, and what a clone means for backup, tiering and the audit chain, before any code. This is the one capability on this roadmap whose failure mode is silent data loss in a table nobody was touching.

---

### 1.3 — *Ingest you configure, and a client you install*
**Theme: the two ends a user actually touches.**

**Available:** config-driven ingest from files, where a configuration names the source, the shape
of what arrives and the table it lands in · JSON documents mapped to typed columns, refusing what
cannot round-trip rather than coercing it · microbatch assembly bounded by size and by time · a
quarantine for what does not fit, with an expiry · **a Python SDK**: connect over TLS, query with
streaming results, ingest, declare and materialise cubes, clone a table and read its lineage, and
register an aggregation of your own.

**What it is for:** everything before this release assumes somebody already has data in the
warehouse and a way to reach it. Both assumptions are doing a great deal of work. A user's first
question is *how do I get my data in*, and their second is *how do I use it from my own code* ---
and until both have an answer, every capability behind them is reachable only by somebody willing
to write SQL over a socket.

**Why the client contract comes before the client.** Python is first and Java and Rust follow, and
three bindings that each decided for themselves what to validate would produce three subtly
different products. The rule is that **the SDK contains no logic the server does not also
enforce**, so a refusal is the same refusal everywhere and "it worked in Python" is not a
sentence anybody has to debug.

**Gated on a mandatory requirement nobody had met.** `FR-SEC-03` asks for federated identity,
mutual TLS and scram on the wire-protocol door, and there is no TLS anywhere in the system today.
On a loopback that is tolerable. For a client whose entire purpose is connecting from somewhere
else, it is credential exposure, and the SDK ships behind it rather than in front of it.

**Custom aggregations run out of process.** An aggregation supplied by a user is the one part of
a query this system did not write; in-process it shares an address space with the audit chain and
with every other tenant. [ADR-0010](adr/0010-external-aggregations.md) already made the
correctness half of that decision --- an external aggregation is a *contract* rather than a
function, and the presence of `merge` is what earns it the right to be rolled up --- and the
runtime half is decided the same way: a sidecar that panics is a sidecar that dies.

---

### 1.4 — *Domain packs*
**Theme: the mechanism, used in anger.**

**Available:** risk analytics and financial-crime packs, built on the same published extension API available to third parties, using no privileged access.

**Why they come after 1.0 rather than defining the product:** they exist as much to prove the extension mechanism is real as to serve their industries. A pack that required core changes would falsify the architecture; these must not.

---

## 4. Capability timeline

| Capability | 0.1 | 0.2 | 0.3 | 0.4 | 0.5 | 0.6 | 0.7 | 1.0 | 1.1 | 1.2 | 1.3 |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| Transactional store, managed or attached | ▪ | ● | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Automatic capture and onboarding | ▪ | ● | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Read-your-own-writes | | ● | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Exactly-once, reconciliation-proven | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Schema evolution with quarantine | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Source-safety escalation | | | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Analytical SQL at published performance | ▪ | ▪ | ▪ | ● | ● | ● | ● | ● | ● | ● | ● |
| Time travel and as-of queries | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| External-engine readability | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Automatic maintenance | | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● |
| Graph engine | | | | | ● | ● | ● | ● | ● | ● | ● |
| Extension API and declarative packs | | | | | ● | ● | ● | ● | ● | ● | ● |
| Multi-tenancy and row/column security | | | | | ▪ | ● | ● | ● | ● | ● | ● |
| Columnar and wire-protocol surfaces | | ▪ | ▪ | ▪ | ▪ | ● | ● | ● | ● | ● | ● |
| Audit and encryption | | | | | | ● | ● | ● | ● | ● | ● |
| Operability and packaging | | | | | | ▪ | ● | ● | ● | ● | ● |
| Multi-node and high availability | | | | | | | ▪ | ● | ● | ● | ● |
| Zero-copy clones | | | | | | | | | | ▪ | ● |
| Data tiering with purge | | | | | | | | | ● | ● | ● |
| Domain packs | | | | | | | | | ▪ | ● | ● |

● available · ▪ partial or in development

---

## 5. Explicitly not on this roadmap

Recorded because a roadmap without exclusions grows without bound, and because each of these is a reasonable thing to ask for.

| Not planned | Why |
|---|---|
| Distributed query execution | Single-node with routing covers the target workloads. Introduced only when a *measured* workload exceeds the published ceiling, and then by expressing distribution inside otherwise-normal plans rather than building a scheduler |
| Multi-source capture beyond the primary transactional engine | Would reintroduce the operational estate the design exists to remove. If ever needed, it arrives as an optional external feeder through a documented interface |
| A durable graph database | The graph is a derived, rebuildable projection with no independent durability contract |
| Stream processing | SANKHYA ingests change data; it does not offer general stream transformation or a streaming language |
| Cross-region active-active writes | Single-writer topology. Cross-region is recovery, not active-active |
| A bespoke graph query language | A structured API and SQL functions now; the ISO standard later, implemented as a rewrite onto those functions rather than a second engine |
| Dynamically-loaded native extensions | No stable binary interface; a version mismatch is undefined behaviour rather than an error; a fault kills the process with no isolation. The sandboxed tier gives the same capability safely |
| Automatic rewriting of queries onto materialized aggregates | Explicit addressing ships in weeks with near-zero correctness risk; automatic subsumption is a multi-month project. Revisited after 1.0 |
| Server support on desktop-oriented platforms | Process, signal and file-locking semantics differ enough to roughly double the integration matrix. The command line and client libraries are supported everywhere |
| Exact betweenness and closeness centrality at scale | Computationally infeasible at the target sizes. Approximate variants are provided instead |

---

## 6. What would change this roadmap

Honest triggers, so that a change of direction is recognisable as one rather than as drift:

- **The freshness simplification.** If the arrival buffer proves cleanly retrofittable, an earlier release commits every few seconds with no buffer at all — less machinery, at the cost of losing the primary backpressure lever. Under review.
- **Scale beyond the current model.** Warehouses in the hundreds of terabytes may require maintenance to scale out, which would introduce a fourth node role and change the topology story.
- **Upstream storage-library releases.** Several current constraints — a library that cannot emit delete vectors, another that cannot compact at all, a version lag between the storage libraries and the query engine — are upstream and temporary. Their resolution would simplify the write path and could bring the second table format forward.
- **A second format becoming necessary rather than desirable.** Currently a reversible metadata-adapter choice. If external interoperability requirements demand catalog-mediated access, it moves earlier.
- **The extension API failing its own test.** If a reference pack cannot be built without touching the core, the boundary is wrong and must be redrawn before any further pack work.

---

*This roadmap is maintained under version control. Changes to release themes require a corresponding update to the implementation plan.*
