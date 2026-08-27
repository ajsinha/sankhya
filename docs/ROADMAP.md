<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# SANKHYA — Roadmap

**Document ID:** SNK-RM-001
**Version:** 0.1.0
**Status:** Implementation — M0–M5 complete, M6 closing, M7 in progress
**Date:** 2026-08-26
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

**Available:** multi-node deployment with stateless executor scale-out · leader election · high availability and failover · cross-region recovery with measured objectives · metering.

**What 1.0 means here:** the extension API carries a stability commitment. Everything else may still evolve, but a pack written against 1.0 keeps working.

---

### 1.1 — *Lifecycle*
**Theme: data leaves the transactional tier safely, or not at all.**

**Available:** tiering with source purge · exhaustive verification · the archival registry · transparent cross-tier queries · rehydration · command and scheduled invocation with plan digests, blast-radius limits and an anomaly guard · segregated authorization and evidence packs.

**Gated, explicitly:** this release ships only after reconciliation has run clean in production for a sustained period across every table class, restore drills have passed repeatedly, and an archive attestation drill has passed. **Purging the system of record before the copy is provably correct is indefensible, and the gate exists so that schedule pressure cannot quietly make that decision.**

The recommended configuration even after release is *archive and verify continuously; purge deliberately, rarely, under dual control*. A deployment that never advances past archiving still gets most of the benefit at none of the risk.

---

### 1.2 — *Domain packs*
**Theme: the mechanism, used in anger.**

**Available:** risk analytics and financial-crime packs, built on the same published extension API available to third parties, using no privileged access.

**Why they come after 1.0 rather than defining the product:** they exist as much to prove the extension mechanism is real as to serve their industries. A pack that required core changes would falsify the architecture; these must not.

---

## 4. Capability timeline

| Capability | 0.1 | 0.2 | 0.3 | 0.4 | 0.5 | 0.6 | 0.7 | 1.0 | 1.1 | 1.2 |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| Transactional store, managed or attached | ▪ | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Automatic capture and onboarding | ▪ | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Read-your-own-writes | | ● | ● | ● | ● | ● | ● | ● | ● | ● |
| Exactly-once, reconciliation-proven | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● |
| Schema evolution with quarantine | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● |
| Source-safety escalation | | | ● | ● | ● | ● | ● | ● | ● | ● |
| Analytical SQL at published performance | ▪ | ▪ | ▪ | ● | ● | ● | ● | ● | ● | ● |
| Time travel and as-of queries | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● |
| External-engine readability | | ▪ | ● | ● | ● | ● | ● | ● | ● | ● |
| Automatic maintenance | | | ▪ | ● | ● | ● | ● | ● | ● | ● |
| Graph engine | | | | | ● | ● | ● | ● | ● | ● |
| Extension API and declarative packs | | | | | ● | ● | ● | ● | ● | ● |
| Multi-tenancy and row/column security | | | | | ▪ | ● | ● | ● | ● | ● |
| Columnar and wire-protocol surfaces | | ▪ | ▪ | ▪ | ▪ | ● | ● | ● | ● | ● |
| Audit and encryption | | | | | | ● | ● | ● | ● | ● |
| Operability and packaging | | | | | | ▪ | ● | ● | ● | ● |
| Multi-node and high availability | | | | | | | ▪ | ● | ● | ● |
| Data tiering with purge | | | | | | | | | ● | ● |
| Domain packs | | | | | | | | | ▪ | ● |

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
