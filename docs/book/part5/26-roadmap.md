# Roadmap and Status

> This chapter covers what is built, what is not, and what comes next. Its central claim is
> that a roadmap and a status report answer different questions and must be kept apart: a
> roadmap describes ambition, a plan describes intent, and neither tells you what runs
> today. So the release themes below are gated on exit criteria rather than dates, the
> milestone table records what each milestone actually closed on, and every milestone that
> is blocked says what blocks it — a second machine, a production deployment, a drill that
> cannot be run from development.

## 26.1 Three documents, three questions

| Document | Question it answers | Rate of change |
|---|---|---|
| `ROADMAP.md` | What capability becomes available when, and what each release is *for* | Slowly; a milestone slips without the theme changing |
| `IMPLEMENTATION_PLAN.md` | Milestones, work breakdown, effort, entry and exit criteria | Per milestone |
| `STATUS.md` | What actually runs today, and what was found wrong | Per commit |

Where they disagree, `STATUS.md` is right. That rule exists because the three were once
allowed to drift and **seven documents agreed on a status that was wrong** — agreement is not
accuracy, and `check-docs` now requires every document's status line to name every milestone
in progress.

**Dates are deliberately absent from the roadmap.** Releases are gated on exit criteria. Where
a sequence is stated it is a dependency order, not a schedule.

## 26.2 Milestone status

| Milestone | State |
|---|---|
| **M0** Foundations, spikes, walking skeleton | **Complete**, merged to `main` |
| **M1** Zero-configuration sync and read-your-own-writes | **Complete** |
| **M2** Ingest correctness and durability | **Substantially complete.** Batching invariants, the source-safety ladder, reconciliation, idempotence, crash safety, schema evolution and the backfill handoff exist and are tested. What remains is the slot *lifecycle driver* and the snapshot *reader* — the correctness contracts are in place; the machinery that runs them on a timer is not |
| **M3** Query engine and storage performance | **Complete**, six of six exit criteria, closed 2026-08-26. One criterion was corrected first: it required cancellation inside user code, which does not exist until M4 |
| **M4** Graph engine and the extension mechanism | **Complete**, with one criterion carried as unmet: graph performance against a named public suite. No public graph suite is wired into the pipeline |
| **M5** Tenancy, security and API surfaces | **Closed**, four of five criteria. The fifth needs a second server version to exist. Two of four API surfaces built |
| **M6** Operability, packaging and hardening | **Complete**, six of seven. Criterion 4 accepted on a forty-five-minute judged run by owner decision, with the gap from the multi-day pipeline written down rather than argued away. Criterion 7 carried into M8 |
| **M7** Multidimensional analysis | **Complete**, eight of eight — after being declared complete once on 2026-08-27 and **retracted the same day** |
| **M8** Concurrency and data safety | **Complete on six of eight.** S1–S3 and C1–C3 proven, each measured against a control taken in the same run. §12.2 and criteria 7–8 moved whole to M12 |
| **M9** Tiering | **In progress.** All eleven work items built and the exit criteria demonstrated on 2026-08-31. The gate is not cleared: criterion 3 needs the attestation drill against a real non-production archive |
| **M10** Zero-copy cloning | **Complete 2026-08-31.** Design gate cleared by ADR-0016 before any code; eight work items; all five criteria met. The first milestone since M8 to close on its own terms |
| **M11** Production reconciliation | **Not schedulable by development.** Needs a production deployment that does not exist |
| **M12** Scale-out, HA, disaster recovery, acceptance | **Needs a second machine.** Holds M8 §12.2 and criteria 7–8 |
| **M13** Config-driven ingest, from files | **Complete 2026-09-01.** All five criteria demonstrated through the real binary and the real wire protocol |
| **M14** The client contract and the Python SDK | **In progress.** Transport security built; federated identity is not |
| **M15–M19** | Planned; see §26.5 |

Three of those entries repay reading closely, because each is a case where a milestone could
have been declared closed and was not.

**M7 was declared complete and retracted the same day.** All eight exit criteria passed. Then
somebody asked whether the cube could read a published table, and the answer was no: a
`Definition` named a fact table and validated that the name was well formed, and nothing read
it. Every `Cells` in existence was built by a navigation operation or a test fixture. *A
criterion that never has to read a published table cannot tell you whether the cube can.*

**M8 lost two criteria to a machine, not to an estimate.** Criterion 7 requires recovery
objectives *measured and published rather than estimated*, and an objective measured on one box
silently excludes network detection, machine loss and clock skew. Publishing it would be the
same species of claim as a contention threshold set below the contended figure — which this
repository has shipped twice. So the criteria moved to M12, which already declared the
dependency and already requires two machines. One blocker, one milestone.

**M9's gate is not a formality.** The work is built and demonstrated: purge end to end with
verification, quarantine and rollback; the anomaly guard halting an intentionally defective
policy; nineteen refusal paths shown to fail closed. What is missing is a drill against a real
archive. **Destructive purge stays disabled until M11 clears it, because building the purge path
and arming it are two decisions.**

> **Key idea**
> A milestone closes on its own terms or it does not close. Three of the mechanisms above —
> retraction, a criterion moved to the milestone that can answer it, and a gate held open
> after the code is written — exist so that "complete" keeps meaning one thing.

## 26.3 Release themes

Each release is a *theme*, and each names the test that matters — the one whose failure would
falsify the release rather than delay it.

| Release | Theme | The test that matters |
|---|---|---|
| **0.1** Walking skeleton | Prove the wire exists | One row travels the full path in one binary with no configuration. Nothing usable ships |
| **0.2** It just works | The claim, demonstrated | Create a table, insert a row, query it analytically — no pipeline configuration, no registration, no operator step |
| **0.3** Trustworthy ingest | Prove nothing is lost | With the applier deliberately stalled under sustained write load, retained log stays bounded and the transactional database keeps accepting writes. **An analytical component must never be able to take down the transactional one** |
| **0.4** Fast, and provably so | Numbers anyone can compare | Published performance against public benchmark suites on named hardware, with plans showing pruning and late materialization actually engaged |
| **0.5** Relationships, and extensibility | The third data model | Each reference pack's change touches **zero core files**. If a core built for one industry can serve two unrelated others without modification, the general-purpose claim holds |
| **0.6** Multi-tenant and governed | Safe to run for more than one party | Every attempt by one tenant to reach another's data — through SQL, the columnar surface, a graph traversal, a cached result, an error message — fails, under every feature combination |
| **0.7** Operable | Someone other than the authors can run it | A first-time user reaches a running server, sample data and a successful query in five minutes on a clean machine, with no container runtime, broker, object store or cloud credentials — as a timed test in the pipeline, so it cannot rot |
| **0.8** Multidimensional | The analysis people actually do | Every query returns bit-identical results with materialisation on and off, and a ragged hierarchy with alternate roll-ups reconciles against an independently computed answer with nothing double-counted |
| **1.0** Production | The first release intended to be depended upon | Twelve hours, two machines, 100 GB, 50 concurrent readers, 20 concurrent writers, building and dropping cuboids while saved data is queried and updated. Not one lost commit, not one query failed for a file deleted underneath it |
| **1.1** Lifecycle | Data leaves the transactional tier safely, or not at all | Ships only after reconciliation has run clean in production across every table class, restore drills have passed repeatedly, and an archive attestation drill has passed |
| **1.2** Zero-copy clones | A copy that costs nothing until somebody writes to it | Gated on an accepted ADR covering shared-file lifetime *before any code*. The one capability here whose failure mode is silent data loss in a table nobody was touching |
| **1.3** Ingest you configure, and a client you install | The two ends a user actually touches | Gated on `FR-SEC-03`: there was no TLS anywhere in the system, and a client whose purpose is connecting from elsewhere ships behind it rather than in front of it |
| **1.4** Domain packs | The mechanism, used in anger | Risk analytics and financial-crime packs built on the same published extension API available to third parties, using no privileged access. A pack that required core changes would falsify the architecture |

Three sequencing decisions in that table are deliberate and would otherwise look like mistakes.

**0.2 comes second rather than emerging from the fourth**, because it is the demonstration that
makes the unified-system claim credible. Everything after it improves something that already
works rather than advancing toward something that does not yet exist.

**0.8 comes before scale-out.** Multidimensional analysis is a stated differentiator; multi-node
deployment is table stakes. Shipping the differentiator second gets the order backwards.

**1.2 comes late**, because a clone shares physical files with its origin, and that single fact
reaches into every part of the system that assumes a file belongs to one table. Retirement and
orphan collection are the sharpest case: both decide a file is unreferenced by consulting one
table's log, and under sharing that decision becomes wrong. Shipping cloning on top of
maintenance that cannot see across tables would delete a clone's data and call it tidying.

## 26.4 Three commitments that will be unpopular

The 0.8 release carries four statements that are not negotiable, and they are the clearest
example of the project preferring a refusal to a plausible number.

- **A measure with no declared aggregation rule is refused**, not defaulted to summation. A
  closing balance summed across twelve months is a number that means nothing and looks exactly
  like a number that does.
- **A cube returns bit-identical answers whether or not anything is materialised.**
  Materialisation is therefore a cache with no semantic content, which is what makes it safe to
  choose automatically. It requires a deterministic reduction underneath, which is why almost
  nothing else in this category can claim it.
- **A materialised cuboid cannot go stale.** It is keyed by the snapshot it was built from, so a
  new commit produces a *miss* rather than a stale hit. There is no invalidation protocol and no
  time-to-live, and the "the cube is stale" failure mode is structurally absent rather than
  carefully avoided.
- **Two people may legitimately see different totals for the same cell**, because an aggregate
  is computed only over rows that principal may read. A total computed over rows the caller
  cannot see is a disclosure through arithmetic, and nothing about it looks wrong.

## 26.5 What is queued after the current work

| Milestone | What it is | Why it is placed where it is |
|---|---|---|
| **M15** Ingest without a file | Streaming ingest from the SDK | It is Kafka's problem in different clothes. M13's position is a high-water mark over *source names*, and neither a topic nor a client stream has one. Answering "what is a position, what is back-pressure, what is a stop" once for both is what keeps two ingest paths from disagreeing about what *already ingested* means |
| **M16** The Java and Rust SDKs | Two more bindings | Named now so M14's contract is written for three bindings rather than retrofitted to them |
| **M17** Named snapshots | A name for a consistent position across many tables, pinned so its files stay alive | Immediately after M14. A clone pins one table at one version; a market-risk run reads trades, rates, curves and hierarchy and must read all of them as of one instant, or the reconciliation problem reappears inside a single query. Nearly free — the read path already splices against a target position, and nothing is copied |
| **M18** Derived results | Closes ADR-0014, plus user-supplied merge functions | A maintained cube is already a materialized view in every respect that costs effort; what it cannot express is a derived result that is *not an aggregate* |
| **M19** The data lifecycle policy | One declaration governing how data ages across **both** tiers | The reframe that makes it safe: nothing moves. Capture already published it, so ageing rows out of the transactional store is a **release** gated on reconciliation's proof that the analytical copy exists. Detach, never delete; reversible for a grace period; and a read of released data is **refused by name** rather than answered short, which is the failure nearly every product ships |

## 26.6 What is explicitly not on the roadmap

Recorded because a roadmap without exclusions grows without bound, and because each of these is
a reasonable thing to ask for.

| Not planned | Why |
|---|---|
| Distributed query execution | Single-node with routing covers the target workloads. Introduced only when a *measured* workload exceeds the published ceiling, and then by expressing distribution inside otherwise-normal plans |
| Multi-source capture beyond the primary transactional engine | Would reintroduce the operational estate the design exists to remove |
| A durable graph database | The graph is a derived, rebuildable projection with no independent durability contract |
| Stream processing | SANKHYA ingests change data; it does not offer general stream transformation |
| Cross-region active-active writes | Single-writer topology. Cross-region is recovery, not active-active |
| A bespoke graph query language | A structured API and SQL functions now; the ISO standard later, as a rewrite onto those functions rather than a second engine |
| Dynamically-loaded native extensions | No stable binary interface; a version mismatch is undefined behaviour rather than an error; a fault kills the process with no isolation |
| Automatic rewriting of queries onto materialized aggregates | Explicit addressing ships in weeks with near-zero correctness risk; automatic subsumption is a multi-month project. Revisited after 1.0 |
| Server support on desktop-oriented platforms | Process, signal and file-locking semantics differ enough to roughly double the integration matrix. Clients are supported everywhere |
| Exact betweenness and closeness centrality at scale | Computationally infeasible at the target sizes. Approximate variants provided instead |

## 26.7 What does not exist

The honest complement to §26.2. These are capabilities the architecture describes and the code
does not have.

- **No streaming transport.** Changes are drained through a SQL function rather than a
  replication connection. Neither mainstream Rust PostgreSQL client supports the replication
  protocol, so this is real work rather than a wiring exercise.
- **No slot lifecycle and no backfill reader.** The source-safety ladder is tested against a real
  slot; nothing drives it on a timer. The handoff contract is verified; nothing reads the
  existing rows, so only changes after a slot exists are captured.
- **The arrival tier is a retention contract, not the buffer the architecture describes.** What
  exists governs correctness — coverage, per-row filtering at the durable frontier, and the rule
  that publication alone releases memory. §5.4's epoch ring, per-epoch key digests and per-tenant
  sub-caps do not exist, and nothing wires the tier into the ingest path.
- **No table partitioning on the ingest path.** `FR-STORE-20` is met on the batch publish path
  and **not on the streaming arrival path, which is where most data lands**. There is also no
  timestamp to derive a date from: `_sankhya_commit_ts` is declared on every ingested table and
  written as literal `0` for every row.
- **The governor decides but governs nothing.** Admission and the pressure ladder are built and
  tested and nothing calls either — decision functions without callers.
- **The exactness gate is not wired into a session.** `check_exactness` has no caller.
- **No log cleanup and no multi-part checkpoints.** Nothing deletes the commits a checkpoint
  subsumes, so the log directory grows without bound even though nothing reads most of it.
- **Exact order statistics buffer their input.** `FR-QUERY-08` asks for a bounded-memory
  algorithm over large inputs and this is not one. Exact and bounded are independent properties,
  and only the first is delivered.
- **Two API surfaces of four.** The wire protocol and Flight SQL are served, both over TLS. The
  gRPC control plane is not built; the REST gateway is refused by design rather than pending.
- **Per-tenant graph epochs and envelope encryption are unreachable through the server.** Built
  and tested; nothing wires them to the front door.
- **No QR, SVD or eigendecomposition**, deliberately: they are where an in-house implementation
  is worse than none, because a subtly wrong SVD produces plausible singular values.

> **Pitfall**
> That list has itself accreted. Several entries were written against M1 and answered since
> without being removed — *"no server"* once sat eight bullets above *"the server runs and
> executes statements"*, which is the sort of contradiction a reader resolves by distrusting the
> whole document. The corrections found on 2026-08-31 are in; an item-by-item audit of the rest
> is outstanding work rather than a claim that everything unmarked is current.

## 26.8 What would change this roadmap

Honest triggers, so that a change of direction is recognisable as one rather than as drift.

- **The freshness simplification.** If the arrival buffer proves cleanly retrofittable, an earlier
  release commits every few seconds with no buffer at all — less machinery, at the cost of the
  primary backpressure lever. Under review.
- **Scale beyond the current model.** Warehouses in the hundreds of terabytes may require
  maintenance to scale out, which would introduce a fourth node role.
- **Upstream storage-library releases.** A library that cannot emit delete vectors, another that
  cannot compact at all, a version lag between the storage libraries and the query engine — all
  upstream and temporary. Their resolution would simplify the write path and could bring the
  second table format forward.
- **A second format becoming necessary rather than desirable.** Currently a reversible
  metadata-adapter choice.
- **The extension API failing its own test.** If a reference pack cannot be built without touching
  the core, the boundary is wrong and must be redrawn before any further pack work.

---

**Sizing, for completeness.** The plan estimates ~150–190 engineer-weeks to a hardened first
release with a team of six, of which the general-purpose restructuring accounts for a net +29 to
+40 — roughly 20–25% more up front, in exchange for a third domain costing eight to twelve weeks
rather than a rewrite. That conversion, of the project's scarcest resource into its most
available one, begins paying at the second pack.
