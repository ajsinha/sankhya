# SANKHYA — System Architecture

**Document ID:** SNK-AD-001
**Version:** 0.1.0 (draft for review)
**Status:** Implementation — M0–M4 complete, M5 in progress
**Date:** 2026-08-26
**Companion documents:** `REQUIREMENTS.md` (SNK-RD-001), `IMPLEMENTATION_PLAN.md`, `ROADMAP.md`

---

## 1. Purpose and reading order

This document describes *how* SANKHYA is built. It does not restate *what* it must do — that is `REQUIREMENTS.md`, and every design element here traces to at least one requirement identifier.

Read in this order:

1. **§2 Principles and invariants** — the small set of rules everything else obeys.
2. **§3 Context and topology** — what SANKHYA is deployed alongside.
3. **§4 Component model** — the layered decomposition.
4. **§5 The read path** — the single most distinctive part of the design.
5. **§6 The write path** — how data arrives and becomes queryable.
6. Everything else is elaboration of those five.

Where a design choice was contested during review, this document states the alternative that was rejected and why. Architecture documents that record only the chosen path are unreviewable, because the reader cannot tell which decisions were considered.

---

## 2. Principles and invariants

### 2.1 Principles

| # | Principle | Consequence when applied |
|---|---|---|
| **P1** | **One authoritative writer.** All mutations go to the transactional store. Everything downstream is a derived, versioned, reproducible projection | There are no cross-engine transactions and none are needed. Split-brain between the ledger and its projection is impossible by construction |
| **P2** | **Freshness is a read-path property, not a write-path property** | The commit interval is tuned for storage efficiency; freshness comes from an in-memory tier spliced at query time |
| **P3** | **Never let a fast-moving upstream type into a slow-moving contract** | Applied twice: the storage libraries supply metadata only, and the extension API defines its own function traits |
| **P4** | **The core knows nothing about any domain** | Domain semantics arrive through a published extension API, and the claim is tested by reference packs, not asserted |
| **P5** | **Correct, then fast** | Exact arithmetic and deterministic reduction are defaults; approximation is opt-in and labelled |
| **P6** | **Immutability is what makes caching free** | Data files are never rewritten in place, so a cache keyed by path needs no invalidation protocol |
| **P7** | **Degrade predictably and observably** | Every precondition violation produces a named degradation reason, never a silent miss |
| **P8** | **Structural prevention beats procedural care** | Where a mistake would be catastrophic, make it impossible to express rather than forbidden by convention |

Principle **P8** deserves emphasis because it recurs throughout: a security context that cannot be omitted because there is no other constructor; an archival authorization whose only two constructors constitute a complete audit of how data can leave the system; a purge primitive that cannot emit a delete event because it does not delete rows.

### 2.2 Invariants

Two invariants sit above ordinary requirements because they protect the transactional system everything else depends on. Both are gated by chaos tests rather than by review.

> **INV-1 — Query safety.** No query, at any concurrency, resource level, plan shape or tenant, can cause the transactional primary to lose availability or durability.

> **INV-2 — Source safety.** SANKHYA can never bloat, wedge or exhaust the storage of the database it replicates from — through its replication slot, its own long-running queries, or its own maintenance.

**Why INV-2 needs stating.** A logical replication slot retains write-ahead log from its restart position forward. If SANKHYA's consumer stalls — starved of processor time by a runaway analytical query, for instance — the database retains log indefinitely until its volume fills, at which point it shuts down. The failure mode is **an analytical query taking down the transactional system**, and it is the worst outcome this architecture can produce.

Two further mechanisms compound it. A logical slot pins the catalog transaction horizon, bloating system catalogs and slowing planning for every query on the instance — the remedy for which is draining the slot, not vacuuming user tables. And SANKHYA introduces a third vector *by design*: its own strongly-consistent reads and its initial snapshot export hold open transactions that block reclamation of ordinary dead tuples.

The mitigations appear in §14.

---

## 3. Context and deployment topology

### 3.1 System context

```
        ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
        │ Applications │   │  BI / tools  │   │   Notebooks  │
        └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
               │ SQL, gRPC        │ Flight SQL       │ Flight SQL
               │                  │ Postgres wire    │ Postgres wire
        ┌──────┴──────────────────┴──────────────────┴───────┐
        │                    S A N K H Y A                    │
        │  one binary · one config · one security model       │
        └──────┬──────────────────────────────────┬──────────┘
               │ supervised child                 │ open table format
               │   or attached                    │
        ┌──────┴───────┐                   ┌──────┴───────────────┐
        │  PostgreSQL  │                   │   Object storage      │
        │  system of   │                   │   or local filesystem │
        │  record      │                   └──────┬────────────────┘
        └──────────────┘                          │ direct read
                                            ┌─────┴──────────────┐
                                            │ Spark, Trino,      │
                                            │ DuckDB, others     │
                                            └────────────────────┘
```

The dashed relationship on the right is deliberate and is a product decision, not an implementation detail: **external engines read the published tables directly**, with no SANKHYA process in the path. SANKHYA is a participant in a data estate rather than a replacement for it. §7.3 specifies what that costs and how it is made safe.

### 3.2 Roles

One binary, three roles, selected by configuration.

| Role | Owns | Cardinality | Recovery |
|---|---|---|---|
| **Coordinator** | The transactional connection, the change applier, the maintenance scheduler, the archive engine, the catalog | Exactly one active | Leader election; database failover |
| **Executor** | Nothing durable — caches only | Many | Trivial; any node serves any query |
| **Graph** | Hydrated in-memory graph epochs | Partitioned by tenant | Rebuild from published tables; RTO is published |

**The honest statement about the single-binary constraint.** There is no configuration in which several nodes share writable state with zero coordination. Either the object store is the coordinator, through atomic conditional writes, or the transactional database is. The constraint is satisfied in **packaging** — one artifact, one configuration file, one process per node — and cannot be satisfied in **topology**, where a multi-writer cluster has exactly one logical coordinator by definition. SANKHYA's answer is that the coordinator is a **role of the same binary**.

Two independent lines of analysis arrived at this: commit serialization for the table format, and coordination of maintenance jobs. Convergence from unrelated directions is good evidence the conclusion is correct.

### 3.3 Deployment shapes

| Shape | Transactional tier | Nodes | Intended use |
|---|---|---|---|
| **Solo** | Managed child process | 1, library-embedded | Development, test, edge, single-user analysis |
| **Node** | Managed child process | 1, with listeners | Small deployments, appliances |
| **Cluster** | Attached, externally managed | Coordinator + N executors + graph nodes | Production at scale |

All three share one codebase and one configuration schema; the shape is a configuration value, not a build variant. Solo must require no network listener at all.

**Managed mode is single-node.** This is a documented product boundary rather than a defect: a highly-available multi-node deployment requires a highly-available transactional tier, which means an externally managed cluster.

### 3.4 Process supervision

In managed mode SANKHYA supervises its own process tree.

Startup is an explicit, observable sequence: acquire an exclusive lock on the data directory; verify and extract embedded assets against their recorded checksums; initialize the database if absent, or validate its catalog version; start the database and wait for readiness with bounded backoff; run schema migrations; recover the change-capture position; open listeners; report ready.

Three failure modes are handled explicitly because each is fatal if missed:

- **Two processes over one data directory.** Prevented by the directory lock, which is taken before anything else.
- **An orphaned database process.** At startup, if a process record exists: adopt the process if live and healthy, stop and restart it if live and unhealthy, clear the record if stale. Getting this wrong produces either corruption or a boot loop.
- **A supervisor that dies leaving its child running.** Parent-death signalling on platforms that support it, plus the boot-time check above, because signalling does not survive every termination path.

Shutdown drain order is a **correctness property**, not an implementation detail, and is specified in §16.3.

### 3.5 Data directory layout

One root; nothing is written outside it.

```
${SANKHYA_DATA}/
  sankhya.lock          exclusive directory lock
  version.json          binary version, schema versions, asset checksums
  pg/                   database data directory and its socket
  pg-bin/               extracted database binaries + checksum stamp
  pg-bin-prev/          previous major, retained for in-place upgrade
  wal-archive/          point-in-time recovery archive
  cdc/                  slot state, checkpoints, dead-letter spill
  staging/              un-merged change log (NOT the published warehouse)
  cache/                object cache, footer cache        [regenerable]
  spill/                query spill files                 [regenerable]
  graph/                serialized epochs for fast rehydration [regenerable]
  audit/                local audit spool before shipping
  keys/                 wrapped data keys only, never raw material [secret]
  logs/
  tmp/
```

Each area is classified **durable**, **regenerable** or **secret**. The classification drives backup scope, disaster-recovery design and container volume layout. Regenerable areas are cleared on boot after an unclean shutdown.

**The published warehouse is not under this root** when object storage is in use, and is conceptually separate even when it is local. See §7.1.

---

## 4. Component model

### 4.1 Layering

Dependencies point downward only, enforced mechanically in continuous integration. Layers 0 and 1 are pure: no I/O, no async runtime, no filesystem, no network. This is what makes the hardest logic — policy evaluation, protocol decoding, graph algorithms, numeric reduction, canonical encoding — testable in milliseconds and amenable to property testing and fuzzing.

```
  PACKS   packs/*                 may depend ONLY on the extension API
                                  and the vocabulary crates
 ─────────────────────────────────────────────────────────────────────
  L5      composition root        the only place a pack is named
  L4      API surfaces            Flight SQL · Postgres wire · gRPC · REST
  L3      engines                 query · graph · ingest · views ·
                                  maintenance · tiering
  L2      adapters                transactional · capture · table format ·
                                  object store · catalog · read path ·
                                  authz · crypto · audit · telemetry ·
                                  sandbox hosts
  L1.5    extension API           the only crate with a stable-version
                                  commitment
  L1      pure logic              graph algorithms · numeric · rules ·
                                  apply planning · read planning · config
  L0      vocabulary              types · errors · schema · capture model ·
                                  ports (traits only)
```

Three additional rules govern packs:

1. **No core crate may depend on any pack.**
2. **A pack may depend on a strictly limited set of core crates.** When a pack legitimately needs another, the build fails — and that failure *is* the signal that the extension API has a gap. It is treated as an API design task, never as grounds to widen the allowance.
3. **Packs self-register.** No core crate contains a dispatch on pack identity, so rules 1 and 2 cannot be quietly circumvented by "temporarily" adding a branch.

### 4.2 Component responsibilities

**Layer 0 — vocabulary.** Identifier newtypes, log positions, table and snapshot references, the error taxonomy with stable codes, the logical schema model and type mapping, the capture event model with its byte decoder, and the trait definitions that form every testing seam. Nothing here performs I/O.

**Layer 1 — pure logic.** Graph algorithms over an adjacency snapshot passed in. Numeric reduction, order statistics and fixed-point arithmetic. The rule and detector engine. Apply planning: decoded event stream in, table mutation plan out — **this seam is what makes the majority of synchronization logic testable without a database, and it is the highest-leverage testability decision in the design.** Read planning: the pure routing function that decides which tiers serve a query. Configuration schema and validation.

**Layer 1.5 — the extension API.** SANKHYA's own function traits, the logical-type registry, the pack contribution surface. Versioned independently. The only component carrying a stable-version commitment while the rest of the system is pre-1.0.

**Layer 2 — adapters.** Each owns exactly one external system and one heavy dependency, so that a breaking change upstream has a bounded blast radius. The transactional adapter owns database lifecycle and pooling. The capture adapter owns slot lifecycle and the replication transport. The table-format adapters own metadata resolution. The object-store adapter owns credentials, retries, caching and the conformance probe. The catalog is the single choke point for table resolution and policy rewriting. The read-path planner performs tier splicing.

**Layer 3 — engines.** Query session construction, memory pools, admission and cancellation. Graph hydration and traversal. The ingest pipeline. Materialized views. The maintenance scheduler. The tiering engine.

**Layer 4 — API surfaces.** Protocol adapters over one shared request model. They are parsers and serializers; they contain no policy and no planning.

**Layer 5 — composition root.** Wiring only, deliberately small.

### 4.3 The seams

Every trait that forms a testing seam lives in Layer 0 and has a deterministic fake:

`Clock` · `IdGen` · `ObjectStoreProvider` · `TransactionalStore` · `CaptureSource` · `TableFormat` · `Catalog` · `PolicyEngine` · `KeyProvider` · `AuditSink` · `GraphStore` · `ReadPathPlanner`

`Clock` and `IdGen` are injected everywhere and enforced by lint. Without them the determinism guarantee of §17.4 is impossible, and with them it is nearly free.

---

## 5. The read path

This is the most distinctive part of the architecture and the part most likely to be misunderstood, so it is specified first.

### 5.1 The problem it solves

The naive way to reduce analytical lag is to commit more often. That produces small files, inflates version counts and grows metadata — and **metadata cost lands on query planning, not on scanning**. The result is the well-known failure mode in which a "real-time lakehouse" is either stale or slow, and every attempt to fix the staleness makes it slower.

| Commits per table | Versions per day | Effect |
|---|---|---|
| 1 per minute | 1,440 | Negligible |
| 1 per second | 86,400 | Noticeable; snapshot resolution begins to cost |
| 10 per second | 864,000 | Metadata dominates; **planning becomes the tail latency** |

### 5.2 The resolution

Decouple the two. The table format commits on a slow, efficient cadence tuned for storage; freshness is served from an in-memory tier; a planner splices them.

```
        write                    ┌──────────────────────────────┐
      ─────────────────────────▶ │  T0  Ledger (PostgreSQL)     │
                                 │      authoritative, current   │
                                 └──────────────┬───────────────┘
                                                │ logical replication
                                                ▼
                                 ┌──────────────────────────────┐
                                 │  T1  Arrival buffer          │
                                 │      in-memory Arrow,        │
                                 │      covers (committed, now] │
                                 └──────────────┬───────────────┘
                                                │ batched commit
                                                ▼
                                 ┌──────────────────────────────┐
                                 │  T2  Published tables        │
                                 │      covers [0, committed]   │
                                 └──────────────┬───────────────┘
                                                │ refresh
                                                ▼
                                 ┌──────────────────────────────┐
                                 │  T3  Materialized aggregates │
                                 └──────────────────────────────┘

   query ──▶ catalog (policy rewrite) ──▶ read planner ──▶ spliced plan
```

Every tier is defined by the same abstraction: **a body of data plus the log-position interval it covers.** That uniformity is what makes the splice tractable.

### 5.3 The correctness rule

> The planner selects, for each table, a set of tiers whose coverage intervals are **contiguous, non-overlapping, and collectively cover `[0, target_position]`**.

Because every committed snapshot records the exact log position it contains — the same metadata that provides exactly-once semantics — and every buffer epoch records its range, **the boundary is exact rather than approximate**. There is no double-counting window and no gap.

Three properties follow, and the third is the one that makes this safe for a ledger:

- **No double counting and no gaps**, by construction.
- **Provenance is exact.** Every response reports which tiers served it and over which intervals.
- **Transactional atomicity survives the splice.** A source transaction touching several tables carries one commit position. Since a single target position is applied to every tier and every table in the request, either the whole transaction is visible or none of it is. **A design splicing on wall-clock time would lose this**, and could show one leg of a transaction without the other.

If a required interval cannot be covered — the buffer epoch was retired but no committed snapshot yet covers it — the planner **fails the query with a typed error**. It never returns a partial answer. A coverage gap is a correctness event and surfaces as one.

### 5.4 The arrival buffer

A single-writer, many-reader, epoch-based immutable ring. The applier appends into an open epoch; on seal the epoch becomes immutable and is published by an atomic pointer swap. Readers take a reference and are never blocked by, and never block, the writer — which matters because the applier is the one thing that must never stall.

The buffer holds **change events, not merged state**: key, operation, position, and payload. Merging is a read-path operation. This makes the writer trivially fast — append-only, no index maintenance — and pushes cost to the reader, where it is small because the buffer is small by construction.

It is hard-capped in bytes with per-tenant sub-caps. On approaching the cap the escalation is: seal and commit early, then apply backpressure to the reader, then enter degraded freshness in which maximum-freshness requests fail explicitly. **It never drops data** — it is not a cache of the truth, it is the not-yet-durable part of the truth.

Each epoch maintains a compact digest of the keys it touched, so the planner can skip the splice entirely when a query's predicates provably do not intersect the buffer. **The common case — a historical query over old data — must cost exactly nothing for the buffer's existence.** Without this, the buffer taxes every query; with it, it taxes only the queries that need freshness.

**A topology consequence.** The buffer lives on the node running the applier. Executors do not have it. Therefore maximum-freshness reads are routed to the coordinator, while executors serve pinned-snapshot and relaxed-freshness reads. This is a documented constraint and one of the seams identified for future scaling.

#### 5.4.1 The retention rule, and why it is not an eviction policy

**A segment may be released only once a durable tier covers it.** Not when it is old, not when memory is tight, not when it has been read.

This inverts the usual cache relationship, and the inversion is the point. A cache evicts under pressure and takes a miss. This tier has nothing to miss *to* until publication has happened, so evicting under pressure does not degrade an answer — it destroys one. Worse, releasing a segment from the middle of the interval opens a coverage gap, and the splice is a proof of exact cover that cannot be talked into approximating one. The query would be refused.

So when memory runs short and nothing is releasable, the only correct response is to push back on ingest. That is reported as a distinct condition rather than absorbed, because it is a **publication** problem wearing a memory problem's clothes: the tier is full because publication has stalled, and adding memory treats the symptom.

The escalation has a deliberate gap between its soft and hard limits, so ingest gets a chance to lengthen its commit interval — the highest-leverage response, since it reduces publication *and* compaction load simultaneously — before it is stopped rather than running normally into a wall.

#### 5.4.2 Coverage is trimmed; data is not

The buffer physically retains segments the published tier already covers, because releasing them is governed by the rule above. But it **declares** coverage starting at the durable frontier, so the two tiers abut exactly and the splice succeeds. Declaring the physical extent instead would overlap, and the planner rejects overlapping tiers rather than guessing which to believe.

The consequence is that a scan must filter **per row** — `durable_through < lsn <= target` — not per segment. A segment straddling the frontier is half durable and half not; returning it whole would double-count its durable half against the published tier. This is the same defect, one layer up, as suppressing duplicates per batch rather than per row, which is a mistake this system has already made once.

A straddling segment is retained whole rather than split. Splitting costs a copy to reclaim memory the next publication frees anyway.

### 5.5 Merge strategies

Selected by declared table capability, never by heuristic:

- **Union only**, for append-only tables. No deduplication, no sort, no key comparison. Cost is essentially zero. Most high-volume tables are append-only, so **most queries take this path**.
- **Latest-version-per-key**, for mutable tables. Because the buffer is tiny relative to published data, the efficient shape is an anti-join: scan the published side excluding keys touched in the buffer, then union the buffer's resolved rows. This turns a full merge into a hash probe against a small build side.

### 5.6 Routing

The routing decision is a pure function of query shape, read mode, session pin, table capabilities and freshness state — deterministic and unit-testable with no I/O, even though the planner that applies it performs I/O.

| Query shape | Read mode | Tiers |
|---|---|---|
| Point lookup, small range | Strong | Ledger |
| Point lookup | Fresh | Buffer, falling through to a pruned published scan |
| Analytical scan or aggregate | Fresh | Published + buffer |
| Analytical scan or aggregate | Pinned snapshot | **Published only** — the buffer is excluded by definition, which is exactly why pinned reads are deterministic and replayable |
| Aggregate matching a materialized view | Fresh | Derived + buffer, if the view is splice-able; otherwise fall back |
| Aggregate matching a materialized view | Pinned snapshot | Derived only — and the only mode where result caching pays |
| Graph traversal | any | Published-derived epoch; the buffer is **not** spliced |
| Write | — | Ledger, always |

**The graph tier does not splice the buffer.** A hydrated adjacency structure is a bulk immutable object; incrementally patching it per request is a research problem, not a v1 feature. Graph results therefore report their epoch age, and the graph's freshness objective is stated separately rather than implied to match the analytical tier's. Pretending otherwise would be exactly the kind of overclaim this design is trying to avoid.

### 5.7 The archival dimension

Data tiering (§13) adds a second, orthogonal axis to the same planner:

| Axis | Coverage rule | Authority |
|---|---|---|
| **Log position** (freshness) | Contiguous, non-overlapping, covering `[0, target]` | Commit metadata and buffer epochs |
| **Key range** (archival) | Hot and cold extents disjoint, together covering the declared domain | Transactional catalog (hot), archival registry (cold) |

The symmetry is exact, and presenting it that way is what keeps the planner comprehensible as it grows.

---

## 6. The write path

### 6.1 Capture

SANKHYA speaks the database's streaming replication protocol directly, in-process, using the built-in logical decoding plugin. There is no message broker, no connector framework and no external process.

```
  PostgreSQL WAL
        │  START_REPLICATION ... LOGICAL   (CopyBoth)
        ▼
  ┌─────────────────┐   bytes    ┌──────────────────┐   events   ┌──────────────┐
  │ transport       │ ─────────▶ │ decoder          │ ─────────▶ │ apply planner │
  │ (vendored,      │            │ (ours, pure,     │            │ (ours, pure) │
  │  behind a trait)│            │  fuzzed)         │            └──────┬───────┘
  └─────────────────┘            └──────────────────┘                   │
                                                                        ▼
                                                    ┌───────────────────────────┐
                                                    │ arrival buffer + landing  │
                                                    │ writer (append-only)      │
                                                    └───────────────────────────┘
```

**The decoder is ours.** The transport may be vendored — the available crates are young, pre-1.0 and thinly maintained — but the decoder parses untrusted bytes from a network socket and is therefore both the largest attack surface and the most correctness-critical component in the ingest path. It lives in a pure crate, is property-tested for round-trip fidelity, and is continuously fuzzed. Neither of the mainstream database client crates offers replication support, so this was never optional.

**The apply planner is pure.** Decoded event stream in, table mutation plan out. This seam allows thousands of randomized crash and interleaving scenarios to run in milliseconds against an in-memory table implementation, which is the only practical way to gain confidence in exactly-once behaviour.

### 6.2 Exactly-once, concretely

Delivery is at-least-once; application is idempotent; the composition is effectively exactly-once. Two rules carry the guarantee:

1. **Every commit records its log position in the table's own commit metadata.** On restart the applier reads the last committed position from the table's history. No external state is consulted, so there is nothing to fall out of sync.
2. **The slot position is advanced only after the corresponding commit is durable.** Reversing this ordering is silent data loss, and it is the most common defect in hand-built capture pipelines.

### 6.3 Why the landing zone is append-only

Both mainstream table libraries handle row-level mutation badly today:

- The Delta library **reads and preserves deletion vectors but cannot emit them**; update and delete are copy-on-write, rewriting whole files. Updating a thousand rows scattered across a thousand large files rewrites a billion rows to change a thousand.
- Iceberg equality deletes are anti-joined against every earlier data file, costing substantial time before any query work begins, and the format is itself moving away from them.
- The Iceberg Rust library is append-only and **cannot compact at all**.

So the applier writes an **append-only change log**: key, position, operation, payload. No deletion vectors, no delete files. Current state is produced by SANKHYA's own merge-on-read, and a background job compacts the log into a clean published table by bulk partition rewrite — efficient precisely because it is bulk rather than scattered.

This has a strategic effect beyond avoiding two library limitations: it reduces both formats to versioned file containers with metadata, which is what makes the format choice reversible and the arrival buffer format-independent.

### 6.4 Two published surfaces

The append-only landing zone creates a conflict with external readability: an external engine reading un-merged change rows would compute wrong answers — duplicated rows for every update, resurrected rows for every delete. Silent wrong answers for every external consumer, which is a correctness problem rather than a performance one.

**The resolution is to publish the change log rather than hide it**, as a sibling table with a distinct name:

```
  <warehouse_root>/<schema>/<table>/            merged current state   — correct standalone
  <warehouse_root>/<schema>/<table>__changes/   append-only change log — correct standalone

  ${SANKHYA_DATA}/hotwal/, spill/               in-flight, node-local, never on shared storage
```

Publishing beats hiding on three counts. Each byte reaches shared storage **once** rather than twice, because the apply path writes the change log and compaction reads it to build the base. **No external reader can obtain a wrong answer from either path**, because each is exactly what its name declares — whereas a hidden staging area relies on external readers not finding it, which is a convention rather than a guarantee. And the change log gives external consumers a **genuinely fresh path**, since appending requires no merge.

The change log is independently valuable: it *is* the change-data feed and the immutable audit record.

| Stage | Written by | Write pattern | Freshness | Partitioned by |
|---|---|---|---|---|
| `<table>__changes/` | Apply loop, every batch | Append only | **Batch interval — seconds** | Commit time, always available without schema knowledge |
| `<table>/` | Publish and compaction | Partition-scoped rewrite | Publish cadence | The table's own specification |

The two coverage ranges are **disjoint by construction** — the base covers up to its high-water mark, the delta covers strictly beyond it — so double-counting is impossible rather than unlikely.

**Append-only and keyless tables have no second stage.** With no primary key there is no "current row", so the base *is* the append target and its freshness equals the batch interval at zero merge cost. This is the correct model for event and telemetry data.

SANKHYA publishes the merge definition in its catalog and in each table's identity sidecar, so an external engine can register it and obtain seconds-fresh data with no SANKHYA process involved.

| Contract | Read | Freshness |
|---|---|---|
| **Simple** | The base alone | Publish cadence — always correct, zero knowledge required |
| **Fresh** | Base ∪ change log via the published merge | Batch interval — seconds |
| **SANKHYA's own readers** | Base ∪ change log ∪ arrival buffer | Sub-second |

**The disclosure that must not be discovered during an integration:** external readers taking the simple path see *mutable* tables at publish cadence — minutes, not seconds. This is not configurable away; it is copy-on-write mutation meeting the correct-standalone requirement. Three escapes exist and all are supported: read the Fresh contract, shorten the publish interval and pay measured write amplification, or declare the table append-only where semantics permit.

### 6.4.1 Adaptive batching, and the ratio gate

Two control mechanisms govern how much damage ingest does to the analytical tier. Both are load-bearing.

**Adaptive batching.** The apply loop flushes on first-to-fire across a size trigger, a **size-gated** time trigger, a hard freshness backstop, a row bound and a transaction-count bound. The size gate on the time trigger is the part that is easy to omit and expensive to omit: without it, a table receiving a trickle emits hundreds of tiny commits per day, spending more on metadata than on data.

The interval adapts to the observed ingest rate so that each commit targets a sensible file size. Overrides apply in strict precedence, and two of them move in *opposite* directions for good reason:

| Condition | Action | Why |
|---|---|---|
| Source log pressure rising | **Shorten** the interval | Shorter batches drain faster, advancing the replication position sooner. Deliberately accepts analytical damage to protect the source |
| Shared storage is the bottleneck | **Lengthen** the interval | Fewer, larger writes are more efficient when the *store* is slow |
| **Query planning latency measurably regressing** | **Lengthen** the interval, and raise a named signal | This is "do not overload the analytical tier" expressed as a control law rather than an aspiration |
| Partition fan-out excessive | Do not shorten; engage fan-out guards | §6.4.2 |

Hysteresis is required — a minimum dwell and a two-window persistence rule — or the loop oscillates against its own effect on the signal it is reading.

**Why planning latency is the right signal.** File count, version count and metadata size all land on *planning*, which is a fixed cost paid before any data is read. Its impact is inversely proportional to query size: negligible on a multi-second aggregation, and dominant on a short interactive query. **A high commit rate is a tax that is invisible on the queries nobody watches and severe on the queries everybody watches.**

**The ratio gate.** Partition scoping alone does not solve write amplification. If updates are uniformly distributed across a large base, every partition is touched anyway and scoping saves nothing. The gate is standard log-structured-merge economics: publish a partition when its accumulated change is large **relative to that partition**, not on a fixed clock.

```
publish partition P when
     accumulated_change_bytes(P) >= ratio × base_bytes(P)     ← bounds cost
  OR age_of_oldest_unpublished_change(P) >= publish_interval  ← bounds staleness
  OR P has been sealed and not yet finalized
```

**Both conditions are required**: the ratio bounds cost, the interval bounds lag, and neither alone is sufficient. The ratio is a single dial trading write amplification against read-side merge overhead, and a moderate default reduces amplification by roughly two orders of magnitude relative to a fixed-cadence merge.

**Write amplification is measured and exported per table, with an alarm and an actionable message.** And one case must be stated honestly rather than papered over: **a workload with uniformly-distributed updates across a very large base and a tight external-freshness requirement is fundamentally unsuited to copy-on-write storage.** Such a table should accept a longer publish interval, be served from the transactional tier directly, or wait for delete-vector write support. Saying so is more useful than implying a configuration exists that fixes it.

### 6.4.2 Partition fan-out

A batch touching very many partitions writes very many tiny files. Four guards apply: a cap on partitions written per batch with the remainder deferred and accumulated per partition; a minimum file size below which a partition is not written unless its deferral age is exceeded; an alarm on sustained excessive fan-out; and a bypass routing bulk operations through a path that sorts by partition first, so each partition is written once in full.

The first two convert fan-out into per-partition batching. **The alarm is the important one**: sustained high fan-out is a *symptom* that the partition scheme violates the minimum-partition-size guardrail. The guards buy time; the alarm gets the design fixed. Silently absorbing it would be the failure.

### 6.5 Automatic onboarding

A table created in the source becomes analytically queryable with no configuration step. The mechanism has three parts:

- **A publication covering all tables**, so tables created later are captured automatically.
- **Onboarding triggered by the first relation-metadata message for an unknown relation**: map the column list to a logical schema, create the target table, register it in the catalog. This fires exactly when the user first writes to the table.
- **A schema-change log written by a database event trigger**, which is itself replicated and therefore arrives in-band through the same stream. This catches changes that produce no row events — a table created but not yet written to, or an alteration.

Replica identity is a hazard here and is handled explicitly: a table with no primary key and default replica identity causes the *database* to reject updates and deletes. Onboarding detects this and either remediates with a documented write-amplification cost or onboards the table append-only with a clear diagnostic.

### 6.6 Schema evolution

Additive and compatible changes apply automatically. Incompatible changes **quarantine the affected table**: the applier stops applying to it, the last consistent version remains queryable, a named error and remediation are surfaced, and an explicit operator action resolves it.

This is not timidity. Intent is genuinely unknowable from the change alone — a dropped column may mean "stop capturing this" or "erase it from history", and guessing wrong is either a data-loss incident or a compliance breach. A renamed column is indistinguishable from a drop-and-add without tracking attribute numbers. A narrowing type change silently loses data. Quarantine converts an unbounded problem into a bounded one and is *safer* than the alternative, because a destructive change receives a human decision.

**The coupled requirement that is easy to miss:** with a single replication slot there is one cursor. If a quarantined table stalls it, retained log grows without bound and fills the source database's volume. Quarantined events are therefore routed to a **durable dead-letter store** and the cursor is advanced, with replay on resolution.

---

## 7. Storage architecture

### 7.1 Warehouse layout

```
<warehouse_root>/
  <schema>/                    mirrors the source schema name
    <table>/                   self-contained; the unit of external readability
      _<format metadata>/
      <partition dirs>/
        <data files>
```

One name spans four naming domains — source identifier, object path, catalog namespace, and the name a user types — so a table's origin is identifiable without a lookup table.

Consequences that must be engineered rather than assumed:

- **Escaping is human-legible, not hashed.** The point is relatability.
- **Collisions are refused loudly at onboarding**, never silently merged. Two distinct source identifiers mapping to one path is an error.
- **Case folding is defined** and safe on case-insensitive filesystems.
- **Rename is classified with incompatible schema changes** and requires explicit operator action. With name-based paths, silently moving data is expensive and breaks external readers' saved paths, while silently leaving it destroys the naming guarantee. Neither is acceptable as a default.

**The warehouse root contains only externally-meaningful published tables.** Internal state lives elsewhere, and a foreign object appearing under the warehouse root is detected at startup and refused rather than ignored.

### 7.2 File geometry

Physical layout decisions are made at write time and are expensive to undo, so they are architecture rather than tuning.

| Decision | Direction | Rationale |
|---|---|---|
| Target file size | Large enough that per-request latency amortizes; small enough to preserve scan parallelism and pruning granularity | Object storage charges per request and has tens of milliseconds of first-byte latency |
| Row-group size | Larger for scan-heavy tables; smaller for point-lookup tables | Trades footer count against pruning granularity |
| Page index | Enabled | It is what makes intra-row-group pruning possible |
| Compression | Heavier for cold and remote data; lighter for hot and cached data | Below the decode/IO crossover, smaller wins; above it, faster wins |
| Encoding | Dictionary for low-cardinality dimensions; delta for sorted keys and timestamps; byte-stream-split for floats | Byte-stream-split materially improves float compression at negligible decode cost |
| Bloom filters | Column-specific, never table-wide | They pay only at high selectivity on non-sort-key columns; elsewhere they are pure overhead |
| Sort and clustering | Multi-dimensional clustering keys, applied by the compactor, not by the ingest path | Clustering requires a sort; ingest appends unsorted and compaction sorts |

**Two verified constraints shape this.** Multi-dimensional clustering in the Delta library has an open row-duplication defect and **must not be used until resolved**. And enabling deletion vectors **silently disables predicate pushdown** — a second, independent reason to keep them off, and the finding most likely to be lost if it is not written down.

### 7.3 External readability, and what it costs

External engines reading the warehouse **bypass row- and column-level enforcement entirely**. This is stated plainly rather than obscured, because a security model that has an unmentioned hole is worse than one with a documented boundary.

The compensating controls:

- Storage-level access control is the real enforcement boundary for external readers.
- Per-tenant prefixes with per-tenant scoped credentials, so a path-construction defect cannot cross-read.
- Column-level encryption, so an unauthorized reader obtains ciphertext rather than data.
- A published-versus-private classification determining what is externally readable at all.

An automated test writes with SANKHYA, reads with an independent engine and asserts identical results. Interoperability is a claim; this makes it a test.

Archived data additionally uses a **conservative format profile** — no exotic encodings, no proprietary extensions — with the exact format version recorded. A seven-year retention obligation means the files must be readable in seven years by something other than SANKHYA, and the answer to "what if this project is abandoned" should be "any compliant reader opens these files."

### 7.4 The table-format abstraction

```
  resolve_snapshot(table, as_of)            -> SnapshotHandle
  scan(snapshot, projection, filters, ...)  -> file list + statistics + selections
  append(table, batches, idempotency_key)   -> Version
  commit(expected, changes, metadata)       -> Result<Version, TypedConflict>
  evolve_schema(table, change)              -> Result<(), Unsupported>
  compact(table, options)                   -> Report
  expire(table, retain, honoring: LeaseSet) -> Report
  create_ref / drop_ref                     -> Result<(), Unsupported>
  capabilities()                            -> Capabilities
```

Two design constraints keep the abstraction from becoming a lie:

- **`Capabilities` is the load-bearing type.** Consumers branch on declared capability, never on format identity.
- **`Unsupported` is a first-class error, not a panic**, and callers have a correct generic fallback.

**Conflicts are typed.** Compaction that loses a commit race to the applier is a logical no-op over disjoint files and should rebase and retry; a conflict where the inputs themselves were modified is fatal and must reschedule. Distinguishing them in the type is what makes aggressive compaction safe.

**In any conflict between maintenance and the applier, maintenance backs off; the applier never does.** An applier starved by maintenance retries is an applier falling behind, which is INV-2 territory. This is a safety rule, not a fairness rule.

### 7.5 Why the storage library is metadata-only

The released Delta and Iceberg libraries pin an Arrow generation two majors behind the query engine's. Two Arrow majors cannot coexist in one process — identically-named types become incompatible, and the trait-identity problem is worse than the type problem, since a table provider implementing one generation's trait cannot be registered with the other generation's session at all.

The resolution is to use the storage library **only for metadata** — snapshot resolution, file lists, per-file statistics, delete-vector payloads, schema and partition specification — and run scan execution on the query engine's own Parquet machinery at the current Arrow version. **No bulk data crosses a version boundary; only small metadata structures, which convert trivially.**

The kernel-level Delta library has no query-engine dependency at all and supports the current Arrow generation behind a feature flag, so the following graph is internally consistent with **zero duplicate versions**:

```
datafusion 55 + arrow 59 + parquet 59 + object_store 0.13 + delta_kernel 0.27 (arrow-59)
```

This is better than a workaround, and that is worth being explicit about. Owning the provider is the only way to inject our own distinct-value statistics into the optimizer — neither vendor provider supplies them, which is the root cause of poor join ordering; to perform partition-transform inversion and derived-column correlation; to wire the Parquet reader to our own cache; to order files by statistics so top-N queries can stop early; and to turn delete vectors into a plan-time row selection rather than a post-filter.

**The skew is chronic rather than transient.** The upstream fix exists on the Delta library's main branch but has not been released for months, and the Iceberg equivalent trails further. The architecture accommodates a permanently-lagging storage library rather than waiting for a release.

### 7.6 Caching

Data files are immutable and never rewritten in place. Therefore:

> **A cache keyed by object path requires no invalidation protocol. Entries never go stale; they only become unreferenced.**

Correctness is free; only eviction policy remains, and eviction policy is a performance question. This is a large simplification and is stated explicitly because engineers who have built caches over mutable stores will otherwise design an invalidation protocol this system does not need.

| Layer | Contents | Invalidation |
|---|---|---|
| Table metadata | Log entries, manifests, checkpoints | Immutable per version |
| Footer and page index | Parquet metadata | Immutable per file |
| Byte range | Compressed column chunks, memory + local disk | Immutable per file |
| Decoded batches | Hot dimensions, materialized views, working sets | Keyed by snapshot |
| Result | Final batches | Keyed by snapshot, tenant, entitlements |

**One mutable key exists in the entire design**: the mapping from a table to its latest version. Its time-to-live is bounded by the freshness objective. It is named explicitly because it is the single place a stale cache produces a stale answer.

Two security requirements on cache keys, both of which are breach mechanisms if omitted:

- The **policy bundle version** must be in the plan-cache key. Without it, a revocation does not take effect for any query whose plan is already cached — data served after it was forbidden, with a passing test suite.
- The **evaluated entitlement set** must be in the result-cache key.

Content for encrypted columns is cached as ciphertext, so on-disk cache retains the protection level of the object store.

The local disk cache admits on second access, so a single full scan cannot evict the working set, plus unconditional admission for freshly compacted files, which are hot by definition. Eviction resists the scan-once pattern that plain least-recently-used handles badly.

---

## 8. Query engine

### 8.1 Structure

```
  SQL / Flight SQL / Postgres wire / gRPC
        │
        ▼
  parse and bind
        │
        ▼
  catalog resolution  ──▶  policy rewrite (row filter + column masking)
        │                   ▲
        │                   └── the ONLY path to a table provider
        ▼
  read-path planner   ──▶  tier splice + archival extent resolution
        │
        ▼
  logical optimization ──▶ SANKHYA analyzer rules
        │
        ▼
  physical planning    ──▶ SANKHYA operators (as-of join, graph functions,
        │                   vector aggregates, bounded exact quantile)
        ▼
  admission control    ──▶ estimate, queue or reject
        │
        ▼
  execution            ──▶ vectorized, morsel-parallel, spilling
```

### 8.2 Where security is enforced

**All three engines resolve tables exclusively through the catalog, which returns a policy-rewritten provider.** Row-level security becomes a filter conjoined into the scan — not an optimizer hint, and its presence in the final physical plan is asserted. Column-level security becomes projection restriction plus a masking rewrite.

**It is impossible to reach a table scan without a security context**, and this is enforced by the type system rather than by review: the catalog's resolution function takes a security context, and there is no other constructor for a provider.

The graph tier resolves through the same catalog, so an unauthorized edge is never materialized in memory for that tenant.

**Defence in depth is mandatory on this path.** The tenant predicate is injected by an analyzer rule *and* independently asserted by the provider, which fails if it is absent.

### 8.3 Correctness rules the engine enforces

Four rules are enforced by the planner rather than left to the query author, because each is easy to violate and expensive to discover:

1. **A linear aggregate may not be applied to a precomputed non-linear measure across a grouping key.** The function of a sum is not the sum of the functions. The general mechanism is a fixed-size numeric list column whose element-wise sum *is* additive, so a rollup computes every hierarchy level in one pass and the non-linear function is applied independently at each.
2. **Sketch-based aggregates are rejected at planning time when the session requires exactness**, with an error naming the exact replacement. They are approximate *and* merge-order dependent, so the same query returns different values on different runs — both properties are disqualifying where results must be reproducible.
3. **Floating-point reduction is deterministic**: fixed partition count recorded with the result, partials merged in ascending partition index rather than completion order, compensated summation.
4. **A completeness measure is attachable to any aggregate**, with a threshold below which the query fails. Missing data that silently improves a result is among the most dangerous defect classes in analytics.

### 8.4 Configuration that must be asserted, not documented

The query engine ships with **filter pushdown and filter reordering disabled by default**. Late materialization — evaluating predicates on filter columns and fetching payload columns only for surviving rows — is the single largest scan optimization available, and shipping without it silently forfeits that win.

Therefore SANKHYA **asserts its required engine configuration at startup and fails loudly on unexpected values**, rather than setting it once and trusting it. A configuration default that changes upstream between versions would otherwise be an invisible performance regression.

### 8.5 Resource governance

**Admission control is mandatory rather than advisory**, because hash joins in the underlying engine do not spill. An unbounded build side terminates the process, taking every other tenant's work — and, in managed mode, the database — with it.

The controller estimates peak memory from plan cardinality and **queues or rejects**; it never admits a query it cannot afford. Rejection is a typed error with a retry hint. Unbounded queueing is not an alternative: it converts a throughput problem into a timeout storm.

Memory is governed by a global pool subdivided into per-tenant sub-pools with floors and caps. Because the pool does not observe every allocation — decode paths, network buffers, graph arenas and third-party allocations sit outside it — a **counting allocator provides true accounting, with a load-shedding brake that sheds work before the operating system intervenes.** An out-of-memory termination in managed mode takes the database down too; that is an outage, not a degradation.

Spill files live on a **separate filesystem from the database's write-ahead log**, so that a runaway query filling the spill volume cannot stop the transactional system.

Every query carries an end-to-end deadline propagated into execution and into sandboxed user code, and cancellation takes effect within a bounded time — tested, including for queries inside graph traversal and inside user functions.

### 8.5.1 Where the two engines disagree, and the one that is dangerous

The system presents one copy of the data through two engines, so the same question asked
of the transactional tier and of the analytical tier is expected to get the same answer.
It usually does. That is what makes the exceptions dangerous: nobody re-checks a figure
that has agreed a thousand times.

The differences are enumerated in a test that runs both engines and pins the agreements
as well as the divergences — a list of differences is only trustworthy if somebody
checked the rest, and without the agreements pinned a *new* divergence is a discovery
later rather than a failure now.

**Three of them return a wrong number rather than an error.**

| | Transactional | Analytical |
|---|---|---|
| Summing past a 64-bit integer | exact, widened accumulator | **wraps to a large negative** |
| Multiplying past a 64-bit integer | refuses | **returns zero** |
| Summing decimals past 38 digits | exact | **loses exactness** |

The third contradicts a stated principle. Fixed-point decimal is used *because* money must
be exact, and on overflow the analytical tier returns a number close to the right one
instead of refusing. An error is recoverable; a plausible wrong number in a report is not.

> **This is a real limitation of the current design, not a note about an edge case.** The
> tier that exists to answer questions about money can answer one wrongly, silently, and
> the tier of record would have refused the same question.

**The mitigation is predictive rather than detective**, because detection is not on offer:
by the time the wrong number exists it is already in a result set. The statistics
catalogue bounds the total from the column's range and row count, and reports whether an
overflow is *possible*. It deliberately errs toward "possible" — a false alarm costs a
refused query, a missed one costs a wrong figure nobody notices.

Its limit is that bounds are held as 64-bit integers, so a decimal column beyond about
nineteen digits has no representable bound and the check answers "unknown". The columns
most able to overflow a 38-digit decimal are exactly the ones it cannot reason about.
Widening the bound type closes it.

Three further differences change precision or ordering without making a figure wrong:
`avg` over integers is arbitrary-precision against a 64-bit float; division to a repeating
fraction gives twenty significant digits against sixteen; and text orders by the
database's collation against byte order, so a paged or ranked result over text appears in
a different order in the two tiers.

### 8.6 Defaults that switch off the mechanism they belong to — and one that should stay off

Some settings in the stack default to off and, when off, silently disable a mechanism. They produce no symptom but slowness, so they are asserted at startup rather than configured and trusted: an upstream default can change between versions and the resulting regression would be invisible.

| Setting | Default | Consequence of leaving it |
|---|---|---|
| Parquet writer **page row-count limit** | effectively unlimited | The page index stores bounds *per page*. With no row cap, a narrow column packs enormous row counts into one page — a boolean can fit tens of millions — and the index degenerates to a single entry covering everything. **Page pruning silently does nothing.** Not yet measured; read the claim as unverified |
| Parquet reader **bloom filters** | disabled | Equality predicates on high-cardinality columns cannot skip row groups that bounds cannot exclude. Measured neutral on TPC-H, which has no query of that shape — the mechanism is untested here rather than shown to be worthless |
| Query-engine **filter reordering** | disabled | Filters run in written order, so an expensive predicate may be evaluated against rows a cheap one would have eliminated. Measured neutral on TPC-H |

With a row cap in place a typical row group yields dozens of pages per column, so a selective predicate skips almost all of them. The cost is disk only: the page index lives in its own section and is read on demand.

#### 8.6.1 Filter pushdown, which was required and should not have been

This is the second correction to this section and it goes further than the first.

Late materialization — evaluating predicates inside the Parquet decoder so payload columns are materialized only for surviving rows — is widely described as the single largest scan optimization available, and it defaults to off. An earlier draft of this document said it was worth roughly an order of magnitude. Measured on a synthetic scan, it was **1.02× — neutral**. It was pinned on anyway, on the reasoning that neutral is not harmful and the benefit was expected on wider payloads.

Measured on TPC-H at scale factor 1, it is not neutral. It is a cost, at every selectivity tried:

| Rows surviving the filter | Off | On | |
|---|---|---|---|
| 1 in ~6,000,000 | 5.6 ms | 5.5 ms | 1.02× |
| 1 in ~1,500 | 4.5 ms | 4.8 ms | 0.94× |
| 1 in ~60 | 4.0 ms | 4.6 ms | 0.87× |
| 1 in ~7 | 116.6 ms | 162.0 ms | **0.72×** |
| all rows | 110.6 ms | 111.7 ms | 0.99× |

On the full queries the effect is larger still, because filter reordering compounds it: with both on, Q6 goes from 351 ms to 917 ms at eight clients — **2.6× slower**.

**The reason matters more than the number.** Late materialization saves the decode of payload columns for rows a predicate eliminates. On this data those rows have already been eliminated, by row-group and page statistics, before any decoding begins — which is why the highly selective queries above finish in four to six milliseconds. Pushdown cannot save work that is not being done; what it adds is per-row bookkeeping on the scan that remains.

> **The two mechanisms are not complementary here. The cheaper one has already won.**

That is a property of well-maintained statistics and sorted-enough data, which is what the rest of this system exists to produce. It would look different on data with no useful bounds — and that is where the setting should be reconsidered, **per query from the statistics**, rather than pinned on for everyone.

It is therefore left at the engine's default rather than pinned off: pinning a setting off is still pinning it, and the evidence supports *not always* rather than *never*.

**What this says about the practice, not the setting.** Both errors came from the same place — a mechanism with a good reputation, asserted on reasoning rather than on a measurement of this system's own data. The first measurement was too narrow to contradict the reasoning; the second was a recognisable workload and did. A setting worth asserting at startup is worth measuring on something somebody else designed.

---

## 9. Storage physical design

### 9.1 Tiered compaction

Lakehouse compaction has the write-amplification shape of a log-structured merge tree, and the naive approach is catastrophic: recompacting a whole large partition every hour while it receives a small increment rewrites the entire partition per hour.

```
  L0   micro-batch files, arrival order, small
        │  merge many
        ▼
  L1   sorted within file, medium
        │  merge several
        ▼
  L2   sorted across the partition, full statistics, bloom filters where warranted
        │
        ▼
  SEALED — never rewritten again
```

Each byte is written once at each level, giving roughly **3× total write amplification instead of two orders of magnitude**. When a partition's newest data falls behind a watermark it is compacted once to the top level and **sealed**; a sealed partition is never rewritten. This bounds total compaction work to a function of data volume rather than of data volume multiplied by elapsed time.

#### 9.1.1 Compaction adds; a separate operation removes

The rule that makes frequent compaction safe is that **a merge never deletes anything**. It writes a new file and leaves its inputs in place, so a reader holding a snapshot continues reading files that are still there. There is no window in which a file under a reader disappears.

Deleting the inputs is a distinct operation with distinct preconditions, all of which must hold for a given file:

1. **The replacement verifies.** Its row count is re-read from its footer at retirement time, not trusted from the merge. A merge may have completed hours earlier.
2. **No retained snapshot can resolve to the input.** Time travel and long sessions both pin a position; a file a pinned snapshot may reach is kept however old it is.
3. **The grace period has elapsed.** A reader that listed files a moment before the merge is entitled to open them and has no way to announce that it is doing so. The grace period must exceed the longest query the deployment permits.

An input failing any precondition is **retained with a reason**, which is a correct outcome rather than a failure — retirement is an optimisation, and declining it costs only disk. The one case that is an error is a missing or short replacement: that means the compaction did not actually happen, and nothing may be removed at all.

Separating the two operations means the frequent, cheap one carries essentially no risk, and the dangerous one runs rarely and under stricter conditions.

#### 9.1.2 What compaction is worth, measured

The claim in §9.3 is specific: small files cost query **planning** — listing, footer reads, metadata resolution — rather than scanning. That predicts a roughly *fixed* penalty per query, which should therefore dominate short queries and amortise away on long ones.

Measured over 20,000,000 rows, comparing 400 fragments against the single file they merge into:

| Query | 400 files | 1 file | Ratio | Absolute overhead |
|---|---|---|---|---|
| Short — one narrow range | 16.9 ms | 3.8 ms | **4.42×** | 13.1 ms |
| Long — full aggregation | 121.0 ms | 97.3 ms | **1.24×** | 23.7 ms |

The prediction holds. The overhead stays within the same order across a query that does thirty times more work, while the *ratio* collapses from 4.42× to 1.24×. Fragmentation is therefore an interactive-latency problem, not a throughput one — which is what makes it worth paying attention to, since interactive latency is the thing anyone notices.

This required scaling the fixture before it was a real test. An earlier run over 1,000,000 rows showed 4.43× and 3.77× — apparently uniform, and it would have been read as "more files are slower". The long query simply was not long enough for planning to amortise against. A measurement that cannot distinguish the hypothesis from its negation is not evidence.

Merging also reduced the data by **2.23×**, largely through better compression across a larger block and less per-file overhead.

#### 9.1.3 A directory listing is not a file set

A direct consequence of the add-only rule, and easy to miss because the naive version works perfectly until the first compaction runs.

Between a merge and the retirement of its inputs, the directory holds **both** — the file that was written and the files it replaced, the same rows twice. That window lasts at least a full grace period and exists by design. So anything answering "which files belong to this table" by listing the directory is wrong for the whole of it:

- **A planner given a listing** will plan a merge whose inputs include files an earlier merge already superseded, and the result contains those rows twice — permanently, this time.
- **A reader given a listing** double-counts every merged row for the duration of the window.

The live set is therefore a first-class value carried across maintenance ticks, not something derived from storage. A tick moves it forward: superseded inputs leave, the new output arrives, and files the tick did not touch are carried through unchanged including their declared coverage.

> **This is the concrete reason the system needs a table log rather than merely liking the idea.** "Which files are live" is not answerable from the filesystem once compaction has run, and both correctness properties above depend on answering it. A directory of Parquet files is a storage layout; it is not a table.

The published tier accordingly names its files individually rather than pointing at a directory. Both behaviours are tested, including the negative one: a query registered against the directory is shown to return the merged rows twice while the same query against the live set returns them once.

#### 9.1.4 The log is written by hand, and validated by the kernel

The table log is emitted by SANKHYA directly — a few hundred lines covering `protocol`, `metaData`, `add` and `remove`, one JSON object per line, staged and renamed so a reader never observes a partial commit. Concurrency control is the protocol's own: a writer picks the next version and fails if someone took it, and the loser rebases because its decisions were made against a state that no longer exists.

The kernel is a **dev-dependency**, used as an independent oracle: it reads the log SANKHYA wrote and must agree about the schema, the version and the live set. This arrangement is what DEC-06's metadata-only coupling actually asks for — the storage library supplies a definition of correctness, not an I/O layer — and it keeps eighty-four packages and a duplicated HTTP client out of the shipped binary. That the dependency stays test-only is checked mechanically rather than left to review.

**The oracle earned its place on its first run.** The log this system wrote was invalid: the `add` action's `partitionValues` field is non-nullable and had been omitted. It round-tripped through SANKHYA's own reader perfectly, because a reader ignores a field it never writes. Two implementations agreeing is worth nothing when the same author wrote both sides.

**The log is checkpointed.** Every ten versions the reconciled state is written as a single Parquet file with a `_last_checkpoint` pointer, and readers start from it. This is worth ten times the read cost at fifty thousand commits, and the beneficiary is mostly *other engines* — they have no cache and start cold on every query, so without a checkpoint an external reader opens one file per commit before it reads a row.

A checkpoint holds exactly what replay produces, which makes it safe in a specific way: **it can always be discarded.** A missing file, a corrupt pointer, or one left behind by a table dropped and recreated at the same path all fall back to the log and cost a replay rather than an answer. Nothing is permitted to depend on a checkpoint being present or even parseable — which is what makes writing the format by hand a defensible risk rather than a reckless one.

Writing it is a *maintenance* job, not part of committing. A commit that had to checkpoint could fail for a reason that does not matter.

**Bounds and null counts are written into the log**, alongside the row count. This reverses an earlier decision in this document, and the reversal is worth recording rather than quietly making.

They were withheld on the grounds that a wrong bound silently drops rows and that bounds go wrong quietly under type coercion. That is true, and it is why every bound written comes from code that refuses to produce one it cannot justify: an unrecognised type gets no bound, an unorderable value gets no bound, a merge that would narrow a bound drops it instead, and a value the protocol cannot represent exactly — a non-finite float, bytes that are not text — is omitted rather than approximated.

What the original reasoning did not weigh is the cost of withholding them. **An external engine can prune only on what the log tells it.** Keeping bounds private to SANKHYA means every other reader scans everything, which undercuts the reason for choosing an open format at all. The bar is higher now rather than lower: a malformed statistic costs *other people* answers, in engines that cannot be fixed from here.

The cardinality sketch stays out, because the protocol has nowhere to put it. A column read back from the log therefore reports zero distinct values, which is a trap for whatever reads that figure first.

One detail worth stating because getting it wrong is silent: a compaction's `remove` actions declare `dataChange: false`. Compaction rewrites files without changing rows, and a reader streaming changes from the table would otherwise see every compacted row as a deletion followed by a re-insertion — a flood of spurious changes proportional to how well maintenance is working.

#### 9.1.5 Metadata-only coupling, and what it buys

The provider is SANKHYA's own. The table format library says **which files exist and what is in them**; it does not read them, does not decode them, and does not appear in the execution plan. Scan execution is the query engine's Parquet source, unmodified.

The reason is version skew, and it is concrete rather than stylistic. A table format library and a query engine move on independent schedules and both expose Arrow types in their signatures. Coupling to both *execution* surfaces makes every upgrade of either a coordinated upgrade of the pair, several times a year. Coupling to one for metadata and the other for execution means a format upgrade touches a file list and an engine upgrade touches a plan — neither is a negotiation.

**The measurable consequence is that planning does no file I/O.** Row counts come from the log, which already records them. The alternative is one footer read per file before a single row is read — the small-file penalty of §9.1.2, moved somewhere compaction cannot help.

| Files | Provider | Directory listing | |
|---|---|---|---|
| 50 | 0.54 ms | 1.15 ms | **2.2×** |
| 200 | 0.63 ms | 3.02 ms | **4.8×** |
| 800 | 1.37 ms | 10.33 ms | **7.5×** |

The advantage widens with file count, which is the shape the claim predicts. Note that the provider is **not flat**: sixteen times the files costs about 2.5× more planning, because replaying the log grows with commit count. The cost has been moved from one seek per file to one sequential read of a log, not abolished — and it was the argument for log checkpoints, which are now built: a checkpoint collapses the replay to one read of a summary plus the commits after it.

Two details the provider gets right and a naive one would not:

- **Statistics are marked exact only when nothing can be filtered out.** A query pinned below what the tiers hold has an upper bound, not a count. Reporting it as exact lets the optimizer order joins on a number that is simply wrong — a slow plan chosen confidently, which is harder to notice than a slow plan chosen for want of information.
- **The commit-position column is read when the query pins a position**, because the target filter is evaluated on it, and projected away afterwards. Time travel genuinely costs a column the caller did not ask for, and hiding that would be dishonest about its price. When no tier holds anything past the target the filter provably removes nothing, and neither the filter nor the column read is planned at all — which matters because the cost is per table and therefore compounds with join arity.
- **The scan reports its own statistics, not the table's.** These are different numbers arriving at different times: the table's are read during logical planning, the scan's during physical planning, and join selection reads the second. A provider that supplies only the first leaves every table looking unmeasurable at the moment the engine decides how to join it — so it repartitions tables it could broadcast, and because tables reporting no size are ordered against tables that do, one absent figure moves every join in the query. The scan's figures are also counted over the files that survived pruning, so a selective predicate is reflected in the number the decision actually uses.
- **File grouping is left to the engine above its own threshold.** The engine splits file groups by byte range, which balances on size and beats anything a provider can do by counting files — but only for scans large enough to be worth splitting, below which it leaves a single group alone, and a single group is a single partition. So the provider deals files out only below that threshold. Doing both is worse than either: the engine then rebalances an arrangement already unbalanced by file count.

### 9.2 Commit cadence scales with volume

A fixed commit interval is wrong for small tables, where metadata then dominates the data itself. The cadence is derived rather than configured:

```
commit_interval = clamp(target_landing_file_size / observed_ingest_rate, floor, ceiling)
```

A table receiving a trickle commits rarely; a table receiving a torrent commits often. One rule, no per-table tuning, and it directly prevents the pathology where a small table's metadata exceeds its data.

### 9.3 Metadata economics

Metadata cost is not a rounding error and it compounds in a useful direction.

- Commit log entries accumulate continuously; checkpoints are periodic snapshots of live state.
- **Checkpoint size is proportional to live file count.** Therefore small-file compaction reduces metadata cost **quadratically** — fewer files makes each checkpoint smaller *and* permits checkpointing less often.
- Two configuration changes plus an effective compaction policy reduce daily metadata volume by well over an order of magnitude on a busy table.

**A format observation that is independent of library maturity**, and therefore worth recording separately from `DEC-10`: under continuous micro-batch ingest, appending a small commit record is structurally cheaper than rewriting a whole metadata document on every commit. One format does the former, the other the latter. This is a quantified argument, not a preference.

### 9.4 Compression and encoding

Codecs operate on already-encoded bytes, so ratios are lower than raw-data intuition suggests. The decision rule falls out of the decode-versus-transfer crossover in the requirements document:

> **Heavier compression wins when I/O-bound — which is the normal case for object storage and for any node with many cores. Lighter compression wins only when CPU-bound**, meaning many concurrent queries saturating every core against warm local data.

Accordingly: moderate compression for published data, light and fast for short-lived landing files and for query spill, heavier for archived data where the compression-time knee justifies it.

Per-column encoding is selected **from measured statistics**, refreshed at each top-level compaction — distinct-value counts, sortedness and average width. This requires no domain knowledge whatsoever, which is exactly right for an engine that must serve arbitrary schemas. Two rules carry most of the benefit:

- **Split-stream encoding for floating-point columns.** Domain-neutral and consistently valuable, whether the floats are sensor traces, embeddings, simulation output or measurements.
- **Dictionary encoding disabled where it cannot pay** — high distinct-value ratios, known-unique columns, and floating-point columns, where split-stream encoding strictly dominates. This *saves* write time as well as space, because the writer no longer builds a dictionary it will discard.

### 9.5 Statistics

Row-group and page-level bounds come from the file format. **Truncation of statistics values is important and non-obvious**: a table with a wide text column can otherwise accumulate megabytes of statistics per column per file, and truncation remains sound because a truncated lower bound rounds down and a truncated upper bound rounds up.

The gap the file format does not fill is **distinct-value counts**, which the optimizer needs for join ordering and which neither the file format nor the table log carries. SANKHYA therefore maintains its own per-column, per-partition statistics — a mergeable cardinality sketch, bounds, null fraction, average width and a quantile sketch — refreshed at compaction, when the data has already been read and the marginal cost is near zero.

This is what makes join ordering work on arbitrary user schemas where nobody has run an analysis command.

#### 9.5.1 The rule statistics live under

**A statistic may make a query slower. It may never make a query wrong.**

This is what justifies keeping statistics *out* of the table log. The log says which files a table consists of, and getting that wrong makes queries fail or double-count, so it is written conservatively and never guessed at. Statistics are different in one respect that changes everything: they are **rebuildable**. A wrong statistic can be recomputed from the data, and until it is, the worst outcome should be a slow plan.

That is only true if the asymmetry is enforced rather than intended:

- **Bounds may skip a file only when they prove nothing in it can match.** Anything uncertain — an absent bound, a type that does not line up, a comparison that cannot be made — means the file is read. A needless read costs time; a wrong skip costs an answer, and nothing downstream can detect it.
- **Unknown is not unbounded.** An absent bound means "may match anything", and filling it in with a default turns a missing statistic into a wrong one.
- **A merge may not narrow a bound.** Where both sides hold values and either lacks a bound, the merged bound is absent — inheriting one side's bound would claim a limit the other may exceed. This matters because compaction *merges* statistics rather than recomputing them, so a defect here appears only after maintenance has run, on data that was correct when it was written.
- **Distinct-value estimates never touch pruning.** They are approximate by construction, so no decision that changes an answer may depend on one however convenient it looks.

The safety property is property-tested directly — if `can_skip` returns true, no value in the file satisfies the predicate — and separately over merged statistics. The converse is deliberately *not* asserted: an implementation that never skipped anything would be slow and correct, and only one direction is a defect.

#### 9.5.2 The cardinality estimate

Distinct-value counts are what neither the file format nor the table log carries, and they are what the optimizer needs to order joins on schemas where nobody has run an analysis command.

The estimate is a HyperLogLog sketch: 4,096 registers per column, merging by register-wise maximum so a merged file's sketch equals the sketch of its inputs' union exactly — which is what makes statistics maintainable at compaction with no value re-read. Accuracy measured within 5% from 10 to 100,000 distinct values.

Two properties matter more than accuracy. The sketch **merges exactly**, so maintenance never degrades it. And the hash is **fixed and process-independent**: a seed that varies per process would make two nodes disagree about a plan, and that disagreement would present as a bug in the optimizer rather than as what it is.

### 9.6 Bloom filters

Bloom filters help only for equality predicates on columns where bounds-based pruning fails — that is, high-distinct-value columns not used as the sort key, where every file's range spans the whole domain.

> They pay when a value is likely **absent** from most row groups. The threshold is a ratio of distinct values to row-group count, and below it they are pure overhead.

They are therefore **off by default and enabled per column**, subject to observed query patterns, the ratio test, exclusion of the sort key, and a hard cap on the number of bloomed columns per table — because each one costs a small percentage of the row group, which is cheap for a few and ruinous for many. The filter must be sized from the *measured* distinct-value count; a wrong estimate either wastes space or destroys the false-positive rate.

### 9.7 Sort order without domain knowledge

The engine knows nothing about the schema, so the default sort key is derived in priority order:

1. Partition columns are excluded — they are constant within a partition.
2. **The commit position.** Always present, always monotonic, and **free**, because data already arrives in that order. It gives perfect pruning for every as-of query, which is the one predicate shape guaranteed to exist. This is a genuinely useful default, not a placeholder.
3. The primary key where one exists, enabling point-lookup pruning and turning merges into sorted merges.
4. A low-cardinality column prepended, giving bounds-based pruning for the commonest filter shape.
5. An observed key, proposed by the profiler after sufficient query samples and applied at the next top-level compaction.

Declaration paths exist for the cases that genuinely need knowledge, in precedence order: an explicit tenant declaration, a source-side schema comment (so the policy travels with the schema and survives a dump and restore), a domain-pack hint, then inference, then the default.

**Sorting costs approximately one additional pass over the data, once per partition, at top-level compaction.** Re-clustering historical data costs a full read and write of the table and is therefore an explicit, scheduled operator action, never automatic.

**What it buys, measured.** On TPC-H Q6, which selects one year in seven of a date column:

| Layout | Single query | p95 at 8 clients |
|---|---|---|
| Arrival order | 222 ms | 1819 ms |
| Sorted by the filtered column | **31 ms** | **234 ms** |

**7.8× at concurrency**, entirely from row groups skipped on their statistics before any decoding. It is also the difference between missing `NFR-PERF-02`'s 250 ms and meeting it.

That objective names bloom filters and late materialization as its preconditions. Neither turned out to be the lever: bloom filters do not apply to a query with no equality predicate, and late materialization *costs* on this data (§8.6.1). Sorting was the third thing, and it was the one that mattered — which is worth recording, because the objective's own list of preconditions would have sent someone to build the wrong two.

**Multi-dimensional interleaved ordering is not used**, for two independent reasons: an open row-duplication defect in the implementation, and — separately — interleaving defeats the delta encoding on the sort columns, so it compresses worse than plain lexicographic ordering while also being harder for the optimizer to exploit.

### 9.8 Partitioning

Guardrails, all domain-neutral: a target partition size range; a ceiling on partition count per table, because every partition value is recorded in table metadata; a distinct-value ceiling above which partitioning causes path explosion; a null-fraction ceiling; a minimum table size below which partitioning is counterproductive; and a maximum depth.

What is automatic: whether to partition at all, the time-bucket granularity given a chosen time column, hash-bucket counts, and the choice of time column when exactly one qualifies — falling back to the commit timestamp and logging the ambiguity when several do.

What must be declared: partitioning on a business-meaningful column. **The engine cannot distinguish "a meaningful query boundary" from "merely a low-cardinality column"**, so the default is to sort on it rather than partition by it.

> **The governing bias, and it matters for an engine facing unknown schemas: partitioning is a physical commitment that is expensive or impossible to undo; clustering is cheap to change at the next compaction. When uncertain, prefer the reversible decision — sort, do not partition.**

### 9.9 Mirror naming, mechanically

The requirement is that one name spans four naming domains. It turns out to be nearly free, because the database folds unquoted identifiers to lower case — so for the large majority of tables the source identifier, the directory name, the catalog name and the SQL name are **the same string with no transformation at all**.

The work is entirely in the tail:

- **Identity mapping** for identifiers that are already valid path segments — the common case, byte-for-byte.
- **A legible substitution** for the remainder: normalize, strip marks, lower case, replace anything outside the safe set with a separator, collapse runs, trim, and prefix reserved or hidden-prefixed names. The separator chosen is not legal in an unquoted source identifier, so **its presence is itself a signal that a transformation occurred**.
- **Reserved names are refused**, including format metadata directory names, names beginning with the hidden-file prefix — such a table would be **invisible to whole families of external readers** — and platform device names, because the local-filesystem backend must work everywhere.

**Identity is recoverable from the table directory alone**, with no catalog and no SANKHYA process running, via both table properties and a sidecar document under a hidden-prefixed subdirectory. Relatability must survive the system being switched off.

**Collisions are refused, never disambiguated.** Distinct identifiers can map to one segment — differing only by case, or by a character that becomes the separator. When that happens the table is quarantined with an alert naming both identifiers, and an operator resolves it with an explicit mapping, an exclusion, or a source-side rename. **Automatic disambiguation by suffix is precisely the failure mode that destroys relatability**, which is why an earlier hash-suffix proposal was withdrawn: it guaranteed uniqueness by destroying the readability that was the entire point.

A preflight check scans the whole source catalog **before** onboarding and reports every collision, every transformed identifier, every reserved-name conflict and every case-only distinction — the last of which is legal in the database and fatal on a case-insensitive filesystem. It runs in continuous integration against a schema dump and belongs in the onboarding checklist, so collisions surface at install time rather than in the middle of the night.

### 9.10 Renames

> **The warehouse path is a published interface.** Additive schema changes are backward-compatible for consumers and are applied automatically. A table rename is a **breaking interface change to consumers SANKHYA cannot see**, and breaking changes require a human.

A rename is therefore a **quarantining event** with no automatic default in production. Four resolutions are available, each with a stated cost: relocate the objects, repoint to a new directory leaving history in place, pin the original path and record an alias, or recreate from a fresh snapshot. The alert suggests one by size but never applies it.

**Ingest continues throughout the quarantine.** This is only possible because ingest is keyed by a stable table identity rather than by name — changes keep landing, the replication cursor keeps advancing, and no log pressure accumulates while a human decides. Only *publication* pauses. Without that property this feature would trade a naming problem for an availability problem.

Two constraints worth recording because they are easy to miss:

- **Objects under immutability retention cannot be moved.** Relocation therefore moves only the mutable tier, and must report how many immutable objects will remain under the old prefix rather than silently splitting a table.
- **A column rename and a table rename have opposite policies**, and for a good reason: with field identifiers enabled, a column rename is metadata-only and fully automatic; a table rename breaks a path. Same word, different contracts.

### 9.11 Drops and re-creates

A dropped table's directory is retained for a configured window, indefinitely if under legal hold or immutability retention, and **its path segment stays reserved for the whole window**. On expiry it moves to a separate area rather than being deleted outright, keeping the live warehouse namespace clean.

Re-creating a dropped name is the nastiest case for name-based identity: a different table wearing the same name. The new table receives a new identity, and if the old directory is still within retention the situation is quarantined pending an explicit resolution. A fast path auto-resolves the common development case — an empty dropped table, an expired retention, or a non-production environment — because nobody should file a ticket over dropping a scratch table.

---

## 10. Graph engine

### 10.1 Structure

An Arrow-backed compressed sparse row structure with a reverse index, dense internal identifiers, and attributes held as separate Arrow arrays.

**Typed vertices and typed edges, with per-edge-type adjacency segments.** A single homogeneous graph is a domain-shaped assumption: it suits ownership and counterparty networks, and fails for physical networks, provider networks, telemetry topologies and routing graphs, all of which are heterogeneous and multi-relational. Under a single-type model a pack would have to encode edge types into weights, destroying both type safety and traversal performance.

**Edges carry validity intervals**, and edges are stored **sorted by source and time**. That single layout choice pays three times:

1. "Edges of a vertex after time *t*" becomes a binary search plus a contiguous slice, which is what makes time-respecting traversal affordable.
2. The same sort order gives the best data skipping for the corresponding table.
3. It eliminates the dominant cost of hydration, which is otherwise the sort.

**Why not a general-purpose graph library on the critical path.** Adjacency stored as linked structures costs a cache miss per edge and drags attribute payloads through cache whether needed or not. A compressed sparse row layout touches a few bytes per edge, sequentially, within a vertex's adjacency run — near-perfect prefetch. The difference is roughly an order of magnitude, and the traversal targets depend on it. A general-purpose library remains useful for algorithms we do not wish to write, applied over a converted view, and as a differential-testing oracle.

### 10.2 Epochs

A hydrated graph is an **immutable, identified epoch** bound to a table snapshot, published by atomic swap, reference-counted, freed when the last reader releases it.

Full hydration builds into a **shadow epoch and swaps atomically**, never blocking queries and never mutating a live epoch — so the memory budget must account for two epochs during rebuild. Incremental hydration applies deltas through a copy-on-write overlay, rebuilding fully when the overlay exceeds a bounded fraction.

**A property test asserts that incremental application and full rehydration produce identical graphs.** This is the single most valuable test in the graph tier, because incremental hydration is where the subtle defects live.

### 10.3 Consistency

Every graph result reports its epoch, its source snapshot and its lag. Three modes are offered, and the **snapshot-consistent** mode — requiring the epoch to be at least as current as the query's snapshot — is required for any output used as evidence. Without it, a query can report an entity in its relational half that its graph half cannot see, and a conclusion is drawn from an inconsistent picture.

### 10.4 Composition with SQL

Graph results are exposed as **SQL table functions**, so they are first-class relations that join to relational plans and appear over every API surface for free. The round trip is:

```
  relational predicate  ──▶  candidate vertex set (Arrow array)
                        ──▶  seeded traversal on the induced subgraph at a pinned epoch
                        ──▶  Arrow batches
                        ──▶  joined back into the SQL plan as a table
```

Four contract terms are mandatory:

- **Seeds may be a subquery**, requiring a two-phase operator that drains the seed stream before traversal.
- **Every traversal has a hard result limit and time budget**, and results carry an explicit truncation flag. Path enumeration is exponential; **a truncated result must never be mistakable for an absence of results**.
- **The function reports statistics**, or the planner orders the downstream join badly.
- **Monotone predicates are pushed into the traversal** as pruning bounds rather than applied afterwards — the difference between milliseconds and minutes on a deep search.

### 10.5 Degree suppression

In a power-law network a small number of vertices have enormous degree. A multi-hop traversal through one touches most of the graph, blows every latency budget, and returns paths that are analytically meaningless — connection through a universal hub is not a relationship. Every traversal therefore supports a degree cap and an exclusion list, and reports which vertices were suppressed.

### 10.6 Tenancy

Graphs are hydrated **per tenant**. A traversal leaving the tenant's identifier space is an invariant violation, not a filtered result.

Traversing a shared graph and filtering afterwards is **not offered**, even as an optimization: it leaks existence and topology through timing and through path structure even when payloads are hidden.

---

## 11. Extension architecture

### 11.1 The boundary

```
   ┌──────────────────────────────────────────────────────────┐
   │  packs/    risk · financial-crime · telemetry · logistics │
   │            (and anything a third party writes)            │
   └───────────────────────────┬──────────────────────────────┘
                               │ may depend ONLY on:
                               ▼
   ┌──────────────────────────────────────────────────────────┐
   │  sankhya-ext   the published extension API                │
   │  — SANKHYA's OWN function traits                          │
   │  — a curated, pinned Arrow subset                         │
   │  — the logical-type registry                              │
   └───────────────────────────┬──────────────────────────────┘
                               ▼
   ┌──────────────────────────────────────────────────────────┐
   │  the core — knows nothing about any domain                │
   └──────────────────────────────────────────────────────────┘
```

**The extension API defines its own function traits and re-exports only a curated Arrow subset.** Re-exporting the query engine's traits directly would break every pack in existence on every engine upgrade, several times a year. This is the same principle as the metadata-only storage coupling, applied a second time: **never let a fast-moving upstream type into a slow-moving contract.** It is the single most important constraint on the extension API.

### 11.2 What a pack may contribute

An enumerated set, and nothing outside it: table and schema definitions; logical types; scalar, aggregate and window functions; graph algorithms; view and materialized-view definitions; rules and detectors; named parameterized endpoints; policy vocabulary.

The logical-type registry resolves an otherwise intractable tension. The core forbids bare primitives in public signatures, but a pack must be able to define its own types, which the core cannot name. The resolution is that the core moves Arrow arrays paired with an **opaque logical-type identifier**, and the pack owns validation, coercion and formatting. The extension surface therefore never passes bare scalars.

### 11.3 Keeping the API from rotting

An extension API rots by accretion rather than by breaking, so the mechanisms are structural:

- **A hard size budget.** Crude, and the only mechanism that reliably survives to year three.
- **The two-domain rule.** Nothing enters until two packs *from different domains* need it. One pack's need is a pack-local helper.
- **No escape hatches.** Type-erased downcasting, free-form document values and open-ended string maps are prohibited — these are how interfaces rot without ever changing shape.
- **A restricted dependency allowance for packs.** When a pack legitimately needs more, the build fails, and that failure *is* the signal that the API has a gap. It is an API design task, never grounds to widen the allowance.
- **Compiling examples on every public item**, so bloat has a visible recurring cost.
- **Use it or lose it.** Anything the reference packs do not exercise is removed at the next major version.
- **Mechanical breaking-change detection**, not merely a reviewed diff.

### 11.4 Packaging tiers

| Tier | Form | Sandboxed | Hot-reload | Build cost |
|---|---|---|---|---|
| **Declarative** | Signed bundle: schemas, views, materialized views, SQL functions, rules, policy vocabulary, endpoints. **No code** | It is data | Yes | None |
| **Sandboxed module** | Compiled to a portable sandboxed target for logic the declarative form cannot express | Full: fuel metering, memory cap, deadline interruption, no ambient authority | Yes | None to the server |
| **Compiled** | Built into the binary behind a feature | **None** — pack code is core code | No | The only tier that adds build time |

**The declarative tier is expected to express the substantial majority of a real pack**, because most of what a domain *is* consists of schemas, views, aggregations, rules and thresholds. Building that tier well is what keeps the other two exceptional rather than routine — and it is what allows a domain analyst rather than a systems engineer to deliver a pack.

**Dynamically-loaded native extensions are rejected**, and the reasons are recorded so the decision is not relitigated annually: the language has no stable binary interface, so the crate that would be required is effectively unmaintained; a version mismatch is undefined behaviour rather than an error; a fault kills the process with no isolation; the entire Arrow type surface would have to be projected across the boundary; and every extension would need a per-compiler-version build matrix. The only benefit over the sandboxed tier is a modest constant factor.

### 11.5 Proving the core is actually general

The claim is tested, not asserted, by four mechanisms of increasing strength:

1. **A naming lint** rejecting domain vocabulary in core identifiers, filenames and documentation.
2. **A pack-free build** of the full core test suite, preventing a core test from depending on pack fixtures.
3. **Two reference packs, deliberately opposite** — one high-volume, narrow, time-series-shaped with essentially no graph; one entity-heavy with a physical network graph and string-heavy joins. Neither is financial. **The acceptance test is mechanical: the change that adds a reference pack must touch zero core files.**
4. **An adversarial pack** attempting what packs must not be able to do — read another tenant's data, escape its sandbox, register a non-terminating or panicking function, exceed its budget, shadow a core name — each rejected with a named error.

> **The lint catches leakage; the reference packs catch shape.** A core can be immaculately neutral in its naming and still be structurally bent toward one domain — which is exactly what happened to the graph model during review, and exactly what no lint would have caught. Both mechanisms are needed and only the second is hard.

The adversarial pack matters because once third parties author packs, **the extension API is a security boundary** and must be tested as one. That is the difference between a plugin system and a remote code execution feature.

---

## 12. Security architecture

### 12.1 The choke point

```
  request ──▶ authenticate ──▶ Principal + SecurityContext
                                      │
                                      ▼
                            ┌──────────────────────┐
                            │      CATALOG         │  ← the ONLY path to a table
                            │  policy rewrite:     │
                            │   • row filter       │
                            │   • column mask      │
                            │   • projection limit │
                            └──────────┬───────────┘
                                       │
                   ┌───────────────────┼───────────────────┐
                   ▼                   ▼                   ▼
              SQL engine          graph engine        tiering engine
```

**It is impossible to reach a table without a security context**, enforced by the type system: the catalog's resolution function takes one and there is no other constructor for a provider. This is principle **P8** — structural prevention rather than procedural care — applied to the highest-consequence path in the system.

Enforcement happens **once, at plan construction**, not separately in three engines. The graph tier resolves through the same catalog, so an unauthorized edge is never materialized for that tenant.

**Defence in depth is mandatory here**: the tenant predicate is injected by an analyzer rule *and* independently asserted by the provider, which fails if it is absent.

### 12.2 Testing security

A **negative test suite** is a first-class deliverable. For every policy fixture it asserts that forbidden rows, columns and edges are absent from results, absent from the physical plan, and absent from graph memory.

**Mutation testing is applied to the policy component.** A surviving mutant means a test that passes for the wrong reason — which, on this component, is a data breach with a green build.

### 12.3 The external-reader boundary

External engines reading the warehouse **bypass row- and column-level enforcement entirely**. This is stated as an architectural limitation rather than obscured, because a security model with an unmentioned hole is worse than one with a documented boundary. §7.3 lists the compensating controls.

### 12.4 Personal data

The primary mechanism is design rather than deletion: **direct identifiers live only in the transactional store**, with surrogate keys downstream. Erasure becomes a transactional delete plus a vault purge, leaving analytical history, time travel and retention entirely untouched. This resolves the immutability conflict outright for most cases and cannot be retrofitted affordably.

Where an identifier must exist downstream, per-subject encryption keys permit cryptographic erasure. Rewriting history is a last resort, is a distinct job class with distinct authorization, checks retention and holds first, and records that history before a given date is no longer reproducible.

> **The ordinary maintenance scheduler is structurally incapable of destroying retained history.** Erasure is not a priority level of expiry; it is a different job class with a different authorization path. Anything less, and a misconfigured retention default eventually deletes records that were legally required to persist.

Because different domains impose *contradictory* obligations — some records must be retained and may not be erased, others must be erased on request, frequently on different columns of the same table — a **per-column retention-and-erasure policy engine is core capability**, not a compliance afterthought. Packs declare retention classes; the core enforces them.

---

## 13. Data tiering

### 13.1 What changes when data is purged

Everywhere else the published tier is *derived*: if it is wrong, rebuild it from the source. That safety net is what makes capture defects survivable. Tiering removes it — once a partition is purged, the published copy is the only copy, and any defect in it is permanent and undetectable after the fact.

> **The prime directive.** Data may not be removed from the system of record until its replacement is proven durable, complete, byte-faithful, immutable and covered by the applicable retention obligation. The proof is machine-checked, recorded, and **there is no flag to skip it.**

### 13.2 The trap, and four layers against it

The capture path replicates deletes. An archival purge implemented as a row deletion would propagate and **erase from the published tier exactly the data the purge existed to preserve** — quietly.

| Layer | Mechanism | Property |
|---|---|---|
| **Primitive** | Purge is partition detach then drop. Row deletion is never used for archival | The purge *cannot* emit a delete event, because it deletes no rows |
| **Publication guard** | Tiering-eligible tables exclude delete and truncate from their publication | Even a defective code path cannot propagate a deletion |
| **Applier tripwire** | The applier holds the archival extent map and treats any delete in an archived range as a **fatal alarm** | Catches a mis-scoped publication or a manually created slot |
| **Attestation** | A transactional marker committed with the registry change | Provenance and ordering. **Observability, never safety** |

Two candidate mechanisms were evaluated and rejected, and both rejections are recorded because both are plausible:

- **Marker-bracketed suppression**, where deletes are emitted and the applier suppresses them, fails if a marker is lost, reordered, or the applier restarts mid-bracket. **Never make a safety property depend on a message arriving.**
- **Session-level replication role** does not work at all: it disables triggers and rules and has **no effect on logical decoding**, which reads the write-ahead log directly. The deletes would still be decoded and propagated. It is recorded explicitly so nobody re-proposes it.

### 13.3 Eligibility

A table is tiering-eligible only if it is **append-only by contract** and **range-partitioned on the tiering key**.

There is a convergence worth noting: partitioning is independently required on high-volume time-shaped tables to make retention a metadata operation rather than a bulk delete that generates enormous bloat. **The same schema decision serves both purposes**, both are made at design time, and both are expensive to retrofit.

### 13.4 The gated state machine

```
Proposed → Frozen → Replicated → Verified → Durable → Sealed
         → Detaching → Detached → Quarantined → Dropped → Complete
                     ↘ NeedsAttention  (terminal until an operator acts)
```

Every transition is committed before the corresponding real-world action; every phase is idempotent and resumable **including within a phase**, so a crash late in a long verification does not restart it.

**Verification is exhaustive, not sampled**: row count, primary-key set equality via a digest over sorted blocks, and per-column checksums over a **canonical byte encoding**. Count equality alone is not evidence. Routine reconciliation may sample; purge verification may not.

The canonical encoding carries a **lossless-or-reject rule**. Types that cannot round-trip faithfully make a table ineligible, checked at policy creation rather than at purge time — discovering at purge time that a column cannot round-trip is discovering it too late.

**Quarantine is mandatory.** The detached partition is retained for a grace period during which re-attachment is trivial. It costs disk for a week and buys reversible recovery from a defect found late; against permanent loss of a retained record, it is the cheapest insurance in the system.

**Verification failure is terminal until a human acts.** There is no automatic retry, because failure means a defect exists and retrying is the wrong response.

### 13.5 Three gates, only one irreversible

| Gate | Effect | Reversible |
|---|---|---|
| **Archive** | Copy, verify, tag. Nothing is removed | Fully — a no-op on the source |
| **Purge** | Detach. Data leaves the live table but remains on disk | Trivially — re-attach |
| **Drop** | Remove from quarantine | **Never** |

Separating them, and time-delaying the third, is what makes the feature safe to operate.

> **The recommended production configuration is: schedule enabled, stop at Archive, purge performed deliberately by a human a few times a year under dual control.** This delivers continuous automatic proof that the published copy is complete and correct — the valuable half — while keeping the irreversible half rare and considered. A deployment that never advances past Archive still gets most of the benefit at none of the risk.

### 13.6 Structural prevention of accidental purge

The state machine's entry point requires an authorization value whose **only two constructors** are the command path and the schedule evaluator. No maintenance job can synthesize one.

Consequently, enumerating the constructors of that type is a **complete audit of every way data can leave the system of record** — a review procedure that takes seconds and cannot be circumvented by adding a caller.

### 13.7 Cross-tier queries

After a purge, queries spanning hot and archived ranges are unioned automatically. The authority rule eliminates an entire class of drift defects:

> **The source catalog is authoritative for whether data is still hot; the archival registry is authoritative for provenance and the cold side.** Both are read within the same source snapshot used for the hot scan — the registry lives in the same database, so this is free — making hot extent and hot scan consistent with no distributed agreement.

The tie-break rule is **total**: an uncovered range intersecting the predicate fails with a typed error; a range the registry believes cold but the catalog shows attached — a restored backup resurrecting purged rows — is read once from the source, so **there is no double counting even in the failure case**, while the underlying inconsistency is separately flagged.

A subtlety worth recording, because a reviewer will assume the opposite: serving the cold portion of a strongly-consistent read from the published tier does not weaken the guarantee. Archived data is immutable by policy, so nothing can change it, so a snapshot read is equivalent to a linearizable one. There is no consistency traded — only a change of storage.

### 13.8 Corrections and rehydration

Corrections default to a **compensating entry in the hot tier** referencing the original. This is how record-keeping already works: a posted entry is reversed, not erased. It preserves the audit trail completely and requires no rewrite.

Rehydration loads into a schema **excluded from every publication** — structurally incapable of being re-captured as duplicates — is never attached to the live parent, is read-only, and **carries a mandatory expiry**. Without the expiry, rehydrated copies accumulate into a shadow system of record over a multi-year horizon.

---

## 14. Runtime architecture

### 14.1 Isolation between the sync path and the query path

These two are natural enemies: both want processor time, memory and I/O. And the failure mode is asymmetric — a stalled applier stops log reclamation, which can take down the source database (**INV-2**).

**Four runtimes, not one.**

| Runtime | Sizing | Purpose | Why isolated |
|---|---|---|---|
| **Control** | Small | Supervision, health, election, admin, metrics | Must stay responsive when everything else is saturated, or an orchestrator kills a healthy node |
| **Capture** | **Reserved cores** | Replication stream, decode, apply | Reservation, not prioritization — priority schemes fail under sustained saturation |
| **Network** | Proportional | Accept loops, handshakes, framing, object-store I/O | Latency-sensitive, not compute-bound |
| **Execution** | Remainder | Query execution | Compute-bound; tolerates queuing |

Graph algorithms run on a **separate compute pool**, because they are long-running and non-yielding by nature.

**Memory is four disjoint pools with no lending between them**, capture's allocation never lent out, and maintenance permitted to borrow from execution only inside low-duty windows.

**I/O isolation is physical first, quota second.** The database's write-ahead log, query spill, and cache each live on separate filesystems or devices. A query that fills the spill volume must be incapable of filling the log volume.

**Connection pools are separate and individually capped** for transactional writes, replication, analytical reads and maintenance, with the replication slot reserved and never shared. A runaway analytical workload must be structurally unable to exhaust connection slots and lock out the transactional writer — a real availability vector that is easy to miss.

### 14.2 Backpressure and escalation

A typed pressure bus carries signals from producers to a single, centrally-evaluated escalation ladder. Making the bus explicit — rather than letting each subsystem read others' metrics ad hoc — is what makes the behaviour testable.

| Level | Trigger | Action |
|---|---|---|
| **Normal** | — | Full admission; maintenance at normal duty |
| **Watch** | Lag or compaction debt above warning | Defer optional maintenance; increase batch size |
| **Constrain** | Lag high, or buffer filling | Reduce admission; suspend re-clustering; lengthen commit interval — freshness still served by the buffer |
| **Protect** | Retained log or buffer critical | **Stop admitting new queries**; existing queries run to deadline; all resources to the applier; page |
| **Sacrifice** | Retained log or freeze age near the limit | **Sacrifice the analytical tier to save the source**: advance the slot with a recorded gap marker, mark affected tables for re-snapshot, begin it automatically, report the gap in provenance until closed |

**Threshold ordering is the important part**: SANKHYA degrades on its own terms **before** the database invalidates the slot unilaterally, because an invalidated slot cannot be resumed and forces a full re-snapshot of every replicated table.

The ordering rule, applied everywhere: **the source outranks the analytical tier, the analytical tier outranks maintenance, and maintenance outranks nothing — except when it is defending the source**, where freeze and log reclamation escalate above queries by design.

### 14.3 Why the commit interval is the primary lever

Under sustained pressure the first and highest-leverage action is to **lengthen the commit interval**. It attacks the cause rather than the symptom: fewer, larger files reduce compaction load, metadata volume and planning latency simultaneously.

It is safe **precisely because the arrival buffer preserves freshness as the commit rate falls**. The system can slow its writes without becoming stale. That is the payoff of the tiered read path, and it is why the buffer earns its complexity.

The coupling constraint must be respected:

```
buffer_bytes ≈ write_rate × commit_interval × avg_change_size × safety_factor
```

The interval cannot grow without bound, because it is bounded by buffer memory. **These two parameters must be tuned together.** If they are owned by different configuration sections they will drift, and the failure will occur under exactly the load that triggered the backpressure.

### 14.4 Failure isolation

Unwinding rather than aborting, with query tasks wrapped so that a fault fails one request rather than the process. Any shared state a fault could have left inconsistent is poison-flagged rather than silently reused.

**Raw task spawning is prohibited by lint.** All spawning goes through a supervisor that registers the task, attaches tracing context, and applies a declared policy per subsystem — restart with backoff for capture and hydration workers, fail-the-request for query tasks, and shutdown for the durability path, where continuing after a fault is worse than stopping.

A crash-loop detector prevents thrashing: after repeated rapid failures a subsystem enters a degraded state and stops retrying, which is more useful than an infinite restart loop that looks healthy from outside.

---

## 15. Maintenance architecture

One scheduler covers **both** the transactional and the analytical sides. This is not tidiness: both draw from the same machine budget and must be prioritized against each other. A freeze emergency and a compaction backlog cannot be arbitrated by two independent schedulers.

| Class | Examples | Budget |
|---|---|---|
| **Safety** | Transaction-identifier freeze, slot-lag remediation | **May preempt queries** |
| **Availability** | Log and disk reclamation, emergency compaction | **May preempt queries**, audited |
| **Performance** | Compaction, delete merging, statistics | Within duty cycle |
| **Housekeeping** | Expiry, orphan cleanup, metadata maintenance, partition rotation, tiering | Within duty cycle, windows preferred |
| **Optional** | Re-clustering, cold view refresh | Windows only; first deferred |

The preemption exception is deliberate and explicit: **a wraparound emergency or a full volume is worse than a slow query**, and a scheduler that cannot express that will eventually make the wrong call.

Jobs checkpoint at natural granularity and resume; a job that can only run to completion will never complete on a busy system. **Every job is safe to run twice**, and a job killed at any instant leaves no corruption — at worst unreferenced files, which the orphan cleaner reclaims after an age threshold exceeding the maximum possible commit duration.

**Maintenance is why a multi-node deployment needs coordination at all.** The query path is genuinely stateless; "who compacts this table" is not answerable without a coordinator. Election runs through the transactional store rather than a bespoke consensus implementation — correct, small, and using infrastructure already present.

**Maintenance quality is the analytical latency budget, not a background nicety.** Small-file accumulation and unmerged deletes are the two leading causes of slowness, and both are produced by the sync path itself. The system therefore contains a structural feedback loop — sync creates the mess, maintenance clears it, queries pay if maintenance falls behind — which is why compaction is a first-class subsystem with its own objectives.

The diagnostic reports **time until a problem becomes user-visible** rather than only its current value, because "compaction debt is large" is far less actionable than "at the current write rate, latency on this table doubles in about nine days."

---

## 16. Consistency model

### 16.1 Read modes

| Mode | Semantics | Served from |
|---|---|---|
| **Strong** | Linearizable with respect to source commits | The transactional store |
| **Fresh** *(default)* | Bounded staleness; blocks until lag is within bound, or fails explicitly | Published + arrival buffer |
| **Snapshot** | A pinned, immutable version — deterministic and replayable | Published only |

**Snapshot mode is the mode reproducible outputs must use**, and it is deterministic precisely *because* it excludes the arrival buffer.

### 16.2 Read-your-own-writes

A write returns a session token carrying its commit position. Passing that token to a subsequent analytical query sets the target position; the planner selects tiers covering it, and the change is almost certainly in the buffer rather than in a committed snapshot.

**The client therefore waits for capture, not for a commit** — a difference of seconds — and receives an answer that is exactly rather than approximately fresh.

Without this, the very first demonstration anyone attempts — write a row, then query it — shows the row missing, and they will reasonably conclude the system is broken. It is the classic failure of this architecture pattern and it is entirely preventable.

### 16.3 Shutdown ordering

The drain order is a correctness property and is specified normatively:

1. Report not-ready; wait for load balancers to stop sending work. Liveness stays healthy.
2. Stop accepting new queries; let in-flight queries run to their deadline, then cancel with a typed error.
3. Stop the capture source, but **finish applying the in-flight batch**. A partial batch is rolled back entirely, never half-committed.
4. **Persist the applied position strictly after the commit is durable.** This ordering *is* the exactly-once guarantee.
5. Flush and close writers; release leases.
6. Drop graph epochs — derived state never blocks shutdown.
7. Stop the database gracefully; verify exit.
8. Flush telemetry. An unflushed exporter loses the traces of the incident being debugged.

A second termination signal escalates to abort **and logs exactly what was abandoned**. Termination by force must always be safe; crash consistency is the real requirement.

### 16.4 Snapshot registry and leases

A durable registry maps table versions to commit positions and wall-clock times, and is the **single join point** between the three vocabularies a user might use to say "as of". As-of queries always resolve through it — never by inferring from file modification times, which is a well-known source of subtly wrong answers.

Snapshots held by a running query or a hydrated graph are **leased with a bounded time-to-live**, and expiry refuses to delete files covered by a live lease. The bound is what stops a forgotten session from indefinitely blocking space reclamation.

---

## 17. Cross-cutting concerns

### 17.1 Observability

Metrics fall into four groups: query behaviour, resource pressure, pipeline health, and maintenance debt. **Four receive paging alerts** — retained log volume, transaction-identifier freeze age, compaction debt, and any archive job awaiting attention — because each precedes a user-visible failure by a predictable interval.

Tracing spans a request from client through planning to storage requests, with spans per stage rather than per operator: per-operator spans on a plan with thousands of batches cost more than the query.

**No log line, trace attribute or metric label may contain tenant data.** Query text is data: a normalized plan hash is logged by default, with full text only under explicit policy and routed to the audit store rather than to standard output.

### 17.2 Health

Distinct startup, liveness and readiness signals. **Readiness accounts for pipeline lag; liveness does not** — otherwise a lagging pipeline causes an orchestrator to kill a healthy node, converting a degradation into an outage.

A separate status endpoint reports the full version matrix: binary, database, schema, table protocol, policy bundle, pack versions.

### 17.3 Backup and recovery

Three artifacts must agree: the transactional backup, the table snapshots, and the key generation. A backup produces a **manifest binding all three to a consistent point**, verified on restore. Three backups that do not agree with each other are worse than one.

Snapshots referenced by a backup are protected from expiry for its lifetime. **Restore drills are automated and periodic with retained evidence** — an untested backup is a rumour.

### 17.4 Determinism

A deterministic mode fixes the clock, seeds identifier generation, sorts listings and pins reduction order, such that:

> The same scenario run twice produces byte-identical committed metadata and byte-identical query output.

One test, enormous coverage: it detects hash iteration order leaking into results, wall-clock creeping into metadata, unsorted directory listings, and non-deterministic parallel reduction. It is only possible because clock and identifier generation are injected seams, which is why that decision is mandatory rather than stylistic.

---

## 18. Failure model

| Failure | Behaviour |
|---|---|
| Query exceeds memory | Rejected at admission or spilled. **Never** process termination |
| Runaway query | Cancelled within a bounded time, including inside traversal and sandboxed code |
| Applier crash mid-batch | Batch rolled back; resume from the last durable position; idempotent replay |
| Applier stalls | Escalation ladder; source protected even at the cost of analytical continuity |
| Slot invalidated | Gap marker recorded; automatic re-snapshot; stale data served with explicit provenance |
| Incompatible schema change | Table quarantined; last consistent version remains queryable; events dead-lettered so the cursor still advances |
| Storage unavailable | Retryable errors within the deadline; never an unbounded hang |
| Storage lacks conditional write | Detected at startup; multi-writer mode refused |
| Compaction interrupted | Resumes from checkpoint; at worst unreferenced files, reclaimed after an age threshold |
| Compaction conflicts with the applier | Compaction rebases and retries; **the applier never backs off** |
| Node loss (executor) | Transparent; stateless |
| Node loss (graph) | Cache miss; rebuild with a published recovery time |
| Node loss (coordinator) | Election; database failover |
| Database failover | Slot survives on supported versions; otherwise re-snapshot with a published recovery time |
| Verification failure during archive | Terminal until an operator acts. **No automatic retry** |
| Restored backup resurrects purged rows | Hot extent wins; read once, no double counting; inconsistency flagged; unified queries on that table refused until resolved |
| Pack fault | Contained by the sandbox tier; a compiled pack is core code and has no isolation, which is why the tier exists |

---

## 19. Scale and evolution

### 19.1 Ordered scaling limits

Executors scale out over shared storage, so scan throughput is not the first wall. In order:

1. **Metadata and planning.** Cost grows with file count. Mitigated by compaction, which reduces it *quadratically* — fewer files makes each checkpoint smaller and permits checkpointing less often — and by commit cadence scaled to volume.
2. **The single-writer commit path.** One applier commits one version at a time per table. Absorbed by lengthening the commit interval, which the arrival buffer makes safe. Beyond that, partition the applier by table.
3. **Maintenance throughput.** Compaction of a very large warehouse may exceed one coordinator's duty cycle, forcing a maintenance-worker role. This is the most likely place the architecture must change first.
4. **Local cache capacity** relative to the working set.
5. **Single-node query capacity** for queries that cannot be pruned.
6. **Graph memory** per tenant.

### 19.2 Seams to design now, build later

Two are near-zero cost today and expensive retrofits:

- **Keep the commit path per-table**, never globally serialized, so the applier can be partitioned without restructuring.
- **Allow a table reference to resolve to a shard set**, so a hot table can be split behind one logical name.

Three are legitimate future work and are named so they are not promised prematurely: replicating the arrival buffer to executors, distributed query execution, and a distributed graph with cross-shard traversal — the last being the hardest and the most likely to require a redesign rather than an extension.

### 19.3 Distribution

Single-node execution with query routing, with an explicit ceiling: the largest single query is bounded by one node's memory and cores. Distribution is introduced only when a *measured* workload exceeds it, and the preferred path expresses distribution as exchange operators inside otherwise-normal plans, preserving the single-node code path.

The trade-off, stated plainly: scale-up gives lower latency, far simpler failure semantics, simpler memory accounting and simpler security, and costs the ability to run one query larger than one node. Scale-out inverts every one of those.

---

## 20. Decision index

| ID | Decision | Where |
|---|---|---|
| `DEC-01` | Native in-process capture; no broker, no external framework | §6.1 |
| `DEC-02` | Embedded means a supervised child process; attached is first-class | §3.3, §3.4 |
| `DEC-03` | Three roles of one binary | §3.2 |
| `DEC-04` | Domain-agnostic core; domains are packs | §11 |
| `DEC-05` | Multi-relational, temporal graph model | §10.1 |
| `DEC-06` | Own the table provider; storage libraries supply metadata only | §7.5 |
| `DEC-07` | Append-only landing; merge on read; bulk compaction | §6.3 |
| `DEC-08` | Two published surfaces, two freshness contracts | §6.4 |
| `DEC-09` | Freshness is a read-path property | §5 |
| `DEC-10` | One writable format day one, behind a seam | §7.4 |
| `DEC-11` | Warehouse layout mirrors operational naming | §7.1, §9.9 |
| `DEC-12` | Analytical correctness rules enforced by the planner | §8.3 |
| `DEC-13` | Time-respecting traversal is a core primitive | §10.1 |
| `DEC-14` | Single-node execution with routing; distribution deferred | §19.3 |
| `DEC-15` | Tiering is explicit and gated | §13 |
| `DEC-16` | Sandboxed by default; native dynamic extensions rejected | §11.4 |
| `DEC-17` | The extension API defines its own traits | §11.1 |
| `DEC-18` | Domain-neutral public benchmarks are primary | Requirements §6 |
| `DEC-19` | File-length limit, enforceable form | Requirements §4 |
| `DEC-20` | Coverage and data-loss claims become measured quantities | §17.4, Requirements §6.6 |
| `DEC-21` | Personal data designed out of the analytical tier | §12.4 |
| `DEC-22` | Two named safety invariants | §2.2 |
| `DEC-23` | Purge by partition detach, never row deletion | §13.2 |
| `DEC-24` | After purge, the published tier is the system of record | §13.1 |
| `DEC-25` | Cross-tier queries unified with a total tie-break rule | §13.7 |

---

## 21. Open architectural questions

Recorded because a design document that presents only settled decisions is not reviewable.

| # | Question | Blocks | Owner |
|---|---|---|---|
| ~~1~~ | ~~Is the arrival buffer cleanly retrofittable behind the read-path planner interface?~~ **Answered: yes.** The tier was built against the existing `TierRef`/`plan_splice` interface with no change to the planner, and the two tiers are shown to splice. The retrofit question is closed; §5.4.1 records what governs the tier instead | — | — |
| 2 | Capacity model and scaling roadmap for warehouses in the hundreds of terabytes: node sizing per size tier, metadata footprint, compaction throughput required, and whether maintenance must scale out | Capacity documentation; possibly node roles | Architect and query specialist |
| 3 | Whether managed cloud database offerings preserve replication slots across failover | Any availability commitment in attached mode on managed cloud databases | Database specialist |
| 4 | Whether the alternative format's library pushes down decimal predicates and surfaces distinct-value statistics | Whether that format is viable at all as a second implementation | Database specialist |
| 5 | Whether liquid-style incremental clustering can be written by the chosen library | The strongest argument for the day-one format evaporates if not | Database specialist |
| 6 | Whether the read path needs a dedicated metadata index at very large file counts | Query planning latency at scale | Query specialist |

---

*This document is maintained under version control. Architectural changes require a decision record and a corresponding requirements amendment.*
