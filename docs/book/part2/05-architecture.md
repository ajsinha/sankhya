# 5. Architecture

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> This chapter designs the machine. Its central claim is that a layer boundary with no stated
> failure mode is decoration: every one of the seven layers below is defined by what must never
> happen in it and what breaks when that rule is violated, and the boundaries are enforced by a
> build gate rather than by review. From that follow a crate DAG of fifty-four crates in seven
> tiers, four disjoint runtimes with reserved rather than prioritised capacity, a role split
> between coordinator and executor whose one leak is named, and two invariants — query safety
> and source safety — that every other decision defers to.

## 5.1 The two invariants

Everything below exists to hold two statements true. They are gated by chaos tests rather than
by review, and they are stated first because every trade-off in this chapter resolves in their
favour.

> **INV-1 — Query safety.** No query, at any concurrency, resource level, plan shape or tenant,
> can cause the transactional primary to lose availability or durability.

> **INV-2 — Source safety.** SANKHYA can never bloat, wedge or exhaust the storage of the
> database it replicates from — through its replication slot, its own long-running queries, or
> its own maintenance.

INV-2 deserves its mechanism spelled out, because it is the worst outcome this architecture can
produce and it arrives through a path that looks harmless. If the change consumer stalls —
starved of processor time by a runaway analytical query, for instance — PostgreSQL retains
write-ahead log indefinitely until its volume fills, at which point it shuts down. **The failure
mode is an analytical query taking down the transactional system.**

Two mechanisms compound it. A logical replication slot pins the catalog transaction horizon,
bloating system catalogs and slowing planning for every query on the instance — and the remedy
for that is draining the slot, not vacuuming user tables, which is not where an operator looks
first. And SANKHYA introduces a third vector by design: its own strongly-consistent reads and
its initial snapshot export hold open transactions that block reclamation of ordinary dead
tuples.

This is not hypothetical. A slot was invalidated by exactly this mechanism during a bulk load in
this project's own testing, reporting `wal_status = 'lost'`.

> **Key idea**
> The ordering rule that falls out of INV-2 is applied everywhere in the system: **the source
> outranks the analytical tier, the analytical tier outranks maintenance, and maintenance
> outranks nothing — except when it is defending the source.** In any conflict between
> maintenance and the applier, maintenance backs off and the applier never does. That is a
> safety rule, not a fairness rule.

## 5.2 The eight principles

Eight principles govern the design; five of them are load-bearing in this chapter and are
restated here in the form they are actually applied.

| # | Principle | Where it bites |
|---|---|---|
| P1 | One authoritative writer | All mutations go to the transactional store; everything downstream is a derived, versioned, reproducible projection. **There are no cross-engine transactions and none are needed.** Split-brain between the ledger and its projection is impossible by construction |
| P2 | Freshness is a read-path property, not a write-path property | The commit interval is tuned for storage efficiency; freshness comes from an in-memory tier spliced at query time (Chapter 8) |
| P3 | Never let a fast-moving upstream type into a slow-moving contract | Applied twice: storage libraries supply metadata only, and the extension API defines its own function traits |
| P6 | Immutability is what makes caching free | Data files are never rewritten in place, so a cache keyed by path needs no invalidation protocol |
| P8 | Structural prevention beats procedural care | Where a mistake would be catastrophic, make it impossible to *express* rather than forbidden by convention |

P8 has three concrete instances worth naming now because they recur: a security context that
cannot be omitted, because there is no other constructor for a table provider; an archival
authorization whose only two constructors constitute a complete audit of how data can leave the
system; and a purge primitive that cannot emit a delete event, because it deletes no rows.

## 5.3 The layer model

Dependencies point downward only, enforced mechanically in the build. Layers 0 and 1 are pure:
no I/O, no async runtime, no filesystem, no network.

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
                                  authz · crypto · audit · telemetry
  L1.5    extension API           the only crate with a stable-version
                                  commitment
  L1      pure logic              graph algorithms · numeric · rules ·
                                  apply planning · read planning · config
  L0      vocabulary              types · errors · schema · capture model ·
                                  ports (traits only)
```

The purity of L0 and L1 is what makes the hardest logic in the system — policy evaluation,
protocol decoding, graph algorithms, numeric reduction, canonical encoding — testable in
milliseconds and amenable to property testing and fuzzing. That is not an aesthetic claim; it
is the reason the apply planner can be exercised against thousands of randomised crash and
interleaving scenarios in the time an integration test takes to start a database.

### L0 — vocabulary

**Responsibility.** Identifier newtypes, log positions, table and snapshot references, the error
taxonomy with stable codes, the logical schema model and type mapping, the capture event model
with its byte decoder, and the trait definitions that form every testing seam.

**Never here.** I/O of any kind.

**Failure mode if violated.** A vocabulary crate that performs I/O cannot be used in a property
test, and the components above it inherit the constraint. The purity is what buys the test
speed, and it is lost the first time it is relaxed.

### L1 — pure logic

**Responsibility.** Graph algorithms over an adjacency snapshot passed in. Numeric reduction,
order statistics and fixed-point arithmetic. The rule and detector engine. Apply planning —
decoded event stream in, table mutation plan out. Read planning: the pure routing function that
decides which tiers serve a query. Configuration schema and validation.

**Never here.** I/O, async, filesystem, network.

**Failure mode if violated.** The apply-planning seam is described in this project's own
documents as *the highest-leverage testability decision in the design*, because it makes the
majority of synchronisation logic testable without a database. The seam only exists while the
crate is pure. Read planning has the same property for a different reason: routing is where a
query silently acquires the wrong answer — read the wrong tiers and the result is well-formed,
plausible and missing rows. A pure function can be exhaustively tested against a table of cases;
a decision scattered through a planner that also does I/O cannot.

### L1.5 — the extension API

**Responsibility.** SANKHYA's own function traits, the logical-type registry, the pack
contribution surface. Versioned independently; the only component carrying a stable-version
commitment while the rest of the system is pre-1.0.

**Never here.** Any re-export of an upstream engine's traits.

**Failure mode if violated.** Re-exporting the query engine's traits directly would break every
pack in existence on every engine upgrade, several times a year. This is P3 applied at the point
where the cost is highest, because the breakage lands on somebody else's code.

### L2 — adapters

**Responsibility.** Each adapter owns exactly one external system and one heavy dependency.
The transactional adapter owns database lifecycle and pooling; the capture adapter owns slot
lifecycle and replication transport; the table-format adapters own *metadata resolution and
nothing else*; the object-store adapter owns credentials, retries, caching and the conformance
probe. The catalog is the single choke point for table resolution and policy rewriting. The
read-path planner performs tier splicing.

**Never here.** A second crate depending on the same heavy dependency.

**Failure mode if violated.** A heavy dependency leaking into more than one crate makes the
blast radius of an upstream breaking change unbounded. The pin set treats the
Arrow/Parquet/DataFusion/`object_store` family as an exact-pinned unit for the same reason: two
Arrow majors cannot coexist in one process, identically-named types become incompatible, and
**the trait-identity problem is worse than the type problem** — a table provider implementing one
generation's trait cannot be registered with the other generation's session at all. Changing any
of those pins is a project-level event, not a dependency bump.

### L3 — engines

**Responsibility.** Query session construction, memory pools, admission and cancellation. Graph
hydration and traversal. The ingest pipeline. Materialised views. The maintenance scheduler. The
tiering engine.

### L4 — API surfaces

**Responsibility.** Protocol adapters over one shared request model.

**Never here.** Policy or planning. **They are parsers and serialisers.**

**Failure mode if violated.** Policy in an API surface means policy enforced on one door and not
the other, and the security model then becomes whichever door the caller chose. The same
argument carries to the SDKs: a binding contains no logic the server does not also enforce, and
the test of that is that deleting every SDK changes nothing about what the system permits,
refuses or audits (Chapter 20).

This is not a theoretical hazard here. Arrow Flight SQL was documented, tested and served by
*nothing* for the whole of two milestones — a client had nowhere to send a `GetFlightInfo` —
because the reachability check looked for crates registering SQL functions, which Flight does
not. Widening the check to plain reachability found it in a minute.

### L5 — composition root

**Responsibility.** Wiring only, deliberately small. It is the only place a pack is named.

### Packs

Three rules, and the second is the interesting one.

1. No core crate may depend on any pack.
2. A pack may depend only on a strictly limited set of core crates. **When a pack legitimately
   needs another, the build fails — and that failure *is* the signal that the extension API has a
   gap.** It is treated as an API design task, never as grounds to widen the allowance.
3. Packs self-register. No core crate contains a dispatch on pack identity, so rules 1 and 2
   cannot be quietly circumvented by "temporarily" adding a branch.

> **Pitfall**
> The layer that gets violated first is L2, and it happens through a sequence that sounds
> reasonable at every step: one crate needs a small piece of metadata from a format library, then
> a type from it, then the library itself. The enforcement is a gate rather than a convention,
> because *a convention applied by hand held in three places and lapsed in four*. That sentence
> is this project's summary of its own experience with atomic file publication, and it
> generalises exactly.

## 5.4 The crate DAG, and how it is enforced

Every crate declares its layer in its manifest. A crate without one fails the build. Four rules
are checked:

| Rule | What it refuses | The message it gives |
|---|---|---|
| `PACK DEP` | A pack depending outside the allowed set | *"This failure means the extension API has a gap. Widen the API, not the allowance."* |
| `CORE->PACK` | Any core crate depending on a pack | — |
| `->TOOLING` | Anything depending on tooling | *"tooling is not a dependency"* |
| `UPWARD DEP` | A crate depending on a higher layer | *"dependencies point downward only"* |

The rule is **strictly downward or same-layer, and acyclic**. Same-layer dependencies are
permitted because several vocabulary crates legitimately build on one another; a separate
depth-first cycle check exists as defence in depth and specifically catches cycles that Cargo
tolerates — notably through dev-dependencies.

The fifty-four crates fall out as follows.

| Layer | Crates |
|---|---|
| **L0** vocabulary | `types`, `error`, `schema`, `cdc-model`, `ports`, `version`, `leases`, `atomicfs`, `alloc` |
| **L1** pure logic | `plan`, `cdc-apply`, `cube-algo`, `graph-algo`, `math`, `stats`, `config`, `governor`, `metrics`, `tls`, `testkit` |
| **L1.5** extension API | `ext`; then `pack` at 16 |
| **L2** adapters | `catalog`, `readpath`, `table`, `table-delta`, `table-memory`, `cdc-pg`, `oltp-pg`, `objectstore`, `authz`, `audit`, `session`, `clone` |
| **L3** engines | `olap`, `ingest`, `publish`, `maintenance`, `tiering`, `graph`, `cube`, `mv`, `backup`, `feed`, `diagnostic`, `datagen` |
| **L4** API surfaces | `api-flight`, `api-pg`, `api-grpc`, `api-rest`, `cube-sql`, `graph-sql` |
| **L5** composition root | `server`, `cli` |
| **Packs** | `pack-ref-logistics`, `pack-ref-telemetry`, `pack-adversarial` |

Two crates at L1 have **zero dependencies at all**, and it is deliberate: `graph-algo` and
`cube-algo`. Both hold pure functions of a declaration — the additivity algebra, the
ancestor-answering predicate, traversal over an adjacency passed in by reference — so their
property tests are fast enough to *exhaust* a space rather than sample it. Nothing in
`graph-algo` allocates a graph.

One crate is a special case in the other direction. `sankhya-alloc` holds the counting allocator
and **the only unsafe code in this system**. The workspace sets `unsafe_code = "forbid"`, and
`forbid` cannot be relaxed locally — which is why it is used rather than `deny`. So the one
place unsafe is unavoidable is a crate of its own, and the boundary is visible in the dependency
graph rather than in a comment.

### The file-length ceiling

No source file may exceed **1,500 lines of code**, excluding comments and blank lines, with a
**warning above 800** — which is what prevents a file arriving at 1,499 lines overnight.
Counting uses a real code-line counter rather than a comment-prefix heuristic, because string
literals containing comment markers otherwise corrupt the count.

The reason is stated as an invariant rather than a style preference: *a file nobody will read in
one sitting is a file whose invariants nobody knows.*

### Empty crates, and the rule that keeps them honest

Two crates are empty on purpose and say so in their own documentation, with a date. That is the
required form: **every crate is reachable from something that ships, or listed with a
milestone.** The narrower version of that rule — checking only crates that register SQL
functions — let about 2,600 lines through: a REST surface, a capture source, a counting
allocator nothing installed, a set of port traits, and an entire declarative pack tier. *A crate
is a claim the repository makes about itself, and a milestone is what makes the claim keepable.*

- `sankhya-objectstore` is empty by intent and dated: everything published today goes to a local
  filesystem, and it is scheduled for M12. Its documentation already states the property the
  future implementation must have — a commit claims its version with a primitive that must fail
  when the name is taken, which on an object store is a conditional put — and adds that a store
  offering no conditional put cannot host a warehouse safely, and the system should find that out
  at configuration time rather than during a race.
- `sankhya-mv` is **empty and deliberately undecided**, blocked on an open design question in
  [ADR-0014](../../adr/0014-materialized-views-and-the-cube-lifetime.md). It is neither adopted
  with a date nor deleted; it is listed with a reason, which is the honest state for a crate
  whose design question is still open — and the deliberate difference from the nine empty crates
  resolved alongside it.

## 5.5 Runtime layering is not crate layering

Crate layering is a compile-time property. The runtime has a different and equally deliberate
partition: **four runtimes, not one**, because the sync path and the query path both want
processor time, memory and I/O, and the failure is asymmetric — a stalled applier stops log
reclamation, which is INV-2.

| Runtime | Sizing | Purpose | Why isolated |
|---|---|---|---|
| Control | Small | Supervision, health, election, admin, metrics | Must stay responsive when everything else is saturated, or an orchestrator kills a healthy node |
| Capture | **Reserved cores** | Replication stream, decode, apply | Reservation, not prioritisation — **priority schemes fail under sustained saturation and reservations do not** |
| Network | Proportional | Accept loops, handshakes, framing, object-store I/O | Latency-sensitive, not compute-bound |
| Execution | Remainder | Query execution | Compute-bound; tolerates queuing |

Three further partitions follow the same logic. **Memory is four disjoint pools with no lending
between them.** **I/O isolation is physical first and quota second** — a query that fills the
spill volume must be incapable of filling the log volume. And connection pools are separate and
individually capped, with the replication slot reserved and never shared.

## 5.6 The role split, and its one honest leak

One binary, three roles, selected by configuration.

| Role | Owns | Cardinality | Recovery |
|---|---|---|---|
| **Coordinator** | The transactional connection, the change applier, the maintenance scheduler, the archive engine, the catalog | Exactly one active | Leader election; database failover |
| **Executor** | **Nothing durable — caches only** | Many | Trivial; any node serves any query |
| **Graph** | Hydrated in-memory graph epochs | Partitioned by tenant | Rebuild from published tables; the recovery time is published |

The honest statement about this split is worth quoting in full, because a "one binary" claim
invites the objection it answers:

> There is no configuration in which several nodes share writable state with zero coordination.
> Either the object store is the coordinator, through atomic conditional writes, or the
> transactional database is. The constraint is satisfied in **packaging** — one artifact, one
> configuration file, one process per node — and cannot be satisfied in **topology**, where a
> multi-writer cluster has exactly one logical coordinator by definition. SANKHYA's answer is
> that the coordinator is a **role of the same binary**.

Two independent lines of analysis arrived at this: commit serialisation for the table format,
and coordination of maintenance jobs. Convergence from unrelated directions is good evidence the
conclusion is correct. And the reason coordination exists at all is maintenance rather than
querying: the query path is genuinely stateless, and *"who compacts this table"* is not
answerable without a coordinator.

> **Pitfall**
> The split leaks in exactly one place, and it is named rather than glossed. **The arrival
> buffer lives on the node running the applier. Executors do not have it.** Maximum-freshness
> reads are therefore routed to the coordinator, while executors serve pinned-snapshot and
> relaxed-freshness reads. This is a documented constraint and one of the seams identified for
> future scaling, not a property that emerges correctly by accident.

### Deployment shapes

| Shape | Transactional tier | Nodes | Intended use |
|---|---|---|---|
| Solo | Managed child process | 1, library-embedded | Development, test, edge, single-user analysis |
| Node | Managed child process | 1, with listeners | Small deployments, appliances |
| Cluster | Attached, externally managed | Coordinator + N executors + graph nodes | Production at scale |

All three share one codebase and one configuration schema; **the shape is a configuration value,
not a build variant**, and solo must require no network listener at all.

**Managed mode is single-node.** That is a documented product boundary rather than a defect: a
highly-available multi-node deployment requires a highly-available transactional tier, which
means an externally managed cluster.

Multi-node deployment, leader election, failover and executor scale-out are **M12 and need a
second machine**; Chapter 4 states the boundary and Chapter 26 the status.

## 5.7 The other split: in, stored, out

The architecture draws a second boundary that is easy to miss and is enforced more strictly than
the layer DAG, because violating it has cost this project real defects.

| Crate | Responsibility |
|---|---|
| `sankhya-ingest` | **Everything by which data arrives.** PostgreSQL change capture today; JSON and CSV files, Kafka streams and API invocations are further front-ends onto the same hand-off. It decodes, conditions and batches — it makes arriving data *handleable* — and then it publishes |
| `sankhya-publish` | **The one writer to the warehouse.** Layout, partitioning, statistics, the commit and its rebasing all live here, and nothing else writes a data file or a log action |
| *(not yet built)* | **Everything by which data leaves.** Designed as its own crate for the same reason ingest is |

The evidence for the single-writer rule is concrete. While the ingest pipeline wrote its own
files, its tables carried no partition columns and violated the partitioning requirement that
every table published through the other path satisfied. The soak harness had its own writer too,
so its warehouses were flat and its ten-gigabyte runs reported `PASS` against a layout the
product does not produce.

Routing both through one writer then surfaced defects **in the writer itself**: a creating
commit that omitted its protocol action, and file-name recovery that could not parse a
partitioned path and would have restarted a sequence at zero over live files. Neither was
findable while the paths were separate, because each path only ever agreed with itself.

> **Key idea**
> Two implementations of one contract do not check each other; they *ratify* each other. The
> only way a second writer is caught is by a third party — which is why the Delta kernel is a
> dev-dependency oracle (Chapter 6) and why the reconciliation harness's expected side must
> share no code with the pipeline (Chapter 9).

Enforcement is a gate: any crate that writes a data file or commits a log action must be named
in an allowlist **with a reason**, and the list is required to *shrink*. An entry that stops
being needed is reported as stale, which is how the ingest entry came to be deleted rather than
forgotten.

## 5.8 The security choke point

```
  request ──▶ authenticate ──▶ Principal + SecurityContext
                    ▼
              CATALOG  ← the ONLY path to a table
              policy rewrite: row filter · column mask · projection limit
                    ▼
       SQL engine   ·   graph engine   ·   tiering engine
```

**It is impossible to reach a table without a security context, enforced by the type system:**
the catalog's resolution function takes one and there is no other constructor for a provider.
This is P8 applied to the highest-consequence path in the system.

Enforcement happens once, at plan construction, not separately in three engines. The graph tier
resolves through the same catalog, so an unauthorised edge is never materialised for that
tenant. Defence in depth is mandatory here: the tenant predicate is injected by an analyser rule
*and* independently asserted by the provider, which fails if it is absent — and the presence of
the row filter in the final *physical* plan is asserted, not assumed.

The reason for asserting rather than trusting is on record. The first implementation handed the
policy predicate to the provider as a pushdown filter, and the in-memory table provider
*declines* filters — so every row came back, no error was raised anywhere, and the table was
secured in name only.

One consequence is stated here because it recurs in Chapter 10: **two principals may
legitimately see different totals for the same cell.** That is correct rather than a bug to
design around, and it is made visible by a completeness measure on every aggregate rather than
left to be inferred.

There is also a documented hole, stated plainly because a security model with an unmentioned
hole is worse than one with a documented boundary: **external engines reading the warehouse
bypass row- and column-level enforcement entirely.** The compensating controls are storage-level
access control as the real enforcement boundary for external readers, per-tenant prefixes with
per-tenant scoped credentials so a path-construction defect cannot cross-read, and column-level
encryption so an unauthorised reader obtains ciphertext rather than data. Chapter 13 develops
this.

## 5.9 Backpressure, as a ladder

The subsystems produce at different rates, and the pressure between them is carried on a typed
bus to a single, centrally-evaluated escalation ladder. Making the bus explicit — rather than
letting each subsystem read others' metrics ad hoc — is what makes the behaviour testable.

| Level | Trigger | Action |
|---|---|---|
| Normal | — | Full admission; maintenance at normal duty |
| Watch | Lag or compaction debt above warning | Defer optional maintenance; increase batch size |
| Constrain | Lag high, or buffer filling | Reduce admission; suspend re-clustering; lengthen commit interval — freshness still served by the buffer |
| Protect | Retained log or buffer critical | **Stop admitting new queries**; existing queries run to deadline; all resources to the applier; page |
| Sacrifice | Retained log or freeze age near the limit | **Sacrifice the analytical tier to save the source**: advance the slot with a recorded gap marker, mark affected tables for re-snapshot, begin it automatically, and report the gap in provenance until closed |

**Threshold ordering is the important part.** SANKHYA degrades on its own terms *before* the
database invalidates the slot unilaterally, because an invalidated slot cannot be resumed and
forces a full re-snapshot of every replicated table.

The primary lever is the **commit interval**, not the admission rate, because it attacks the
cause: fewer, larger files reduce compaction load, metadata volume and planning latency
simultaneously. It is safe *precisely because the arrival buffer preserves freshness as the
commit rate falls* — the system can slow its writes without becoming stale. That is the payoff
of the tiered read path, and it is why the buffer earns its complexity.

The lever is bounded by a coupling constraint that must not be split across two configuration
sections:

$$\text{buffer\_bytes} \approx \text{write\_rate} \times \text{commit\_interval} \times \text{avg\_change\_size} \times \text{safety\_factor}$$

The interval cannot grow without bound, because it is bounded by buffer memory. If the two
parameters are owned by different configuration sections they will drift, and the failure will
occur under exactly the load that triggered the backpressure.

## 5.10 Determinism, and what one test covers

> The same scenario run twice produces byte-identical committed metadata and byte-identical
> query output.

One test, enormous coverage: it detects hash iteration order leaking into results, wall-clock
creeping into metadata, unsorted directory listings, and non-deterministic parallel reduction.
It is only possible because clock and identifier generation are injected seams — which is why
that decision is mandatory rather than stylistic.

Determinism is a partitioning property before it is a numerical one. On the numeric side, every
reducing kernel goes through a compensated, ordered sum, because parallel floating-point
reduction is non-deterministic by default: reduction order varies with partition completion
order, so the same aggregate query returns different values run to run. The cost of that
difference is stated precisely in this system's own source: *it is too small to notice and too
large to reconcile, so it surfaces as a figure that will not tie out and nobody can explain.*

Measured, that guarantee turns out to rest almost entirely on compensation rather than on
ordering: across 3,000 randomised inputs spanning 120 orders of magnitude, the number of
permutations for which compensation alone gave a different total was **zero**, as was the number
of hand-built adversarial cases. The canonical sort changes no behaviour that could be observed
— and it stays, because "no observable difference on the cases we tried" is not the same
guarantee as "order-independent by construction".

## 5.11 Where this chapter's decisions are recorded

| Decision | Record |
|---|---|
| The exact-pinned dependency family | [ADR-0001](../../adr/0001-dependency-pin-set.md) |
| Atomic publication and the claim-fails-rather-than-replaces rule | [ADR-0013](../../adr/0013-concurrency-and-data-safety.md) |
| A table reference resolves beneath exactly one log | [ADR-0015](../../adr/0015-the-shard-set-seam.md) |
| Flight SQL as the bulk data plane | [ADR-0006](../../adr/0006-flight-sql.md) |
| A cryptographic hash for the audit chain | [ADR-0003](../../adr/0003-cryptographic-hash-for-audit.md) |
| The client contract, and what an SDK may not contain | [ADR-0017](../../adr/0017-the-client-contract.md) |
