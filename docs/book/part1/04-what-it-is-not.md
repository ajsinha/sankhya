# 4. What it is not

> A system defined only by what it does will be evaluated against everything it does not, and
> lose. This chapter states SANKHYA's boundaries: the things it will not become, the things it
> is not yet, and the things it is *choosing* not to be. The distinction between those three
> is the chapter's whole point — a permanent non-goal, an unbuilt capability with a milestone,
> and a refused design each deserve a different reaction from a reader deciding whether to
> depend on this. Each boundary is given with the reason, so the reader can judge whether the
> reason applies to them.

## 4.1 Three kinds of boundary

Mixing these three is how a technical document becomes untrustworthy, so they are kept apart
throughout this book.

| Kind | What it means | How this book marks it |
|---|---|---|
| **Non-goal** | Will not be built, and the reason is architectural rather than temporal | Named here, with the reason |
| **Not yet** | Designed or planned, not built; a milestone owns it | Marked inline in the chapter that describes it, with the milestone |
| **Refused** | Considered, designed, and rejected — with the alternative that was chosen instead | Named here and in the relevant ADR |

The third category is the one usually missing. A refused design is more informative than a
non-goal, because somebody already did the work of finding out what it costs.

## 4.2 It is not a distributed database

This is the largest boundary and the one most likely to disqualify the system for a given
workload, so it goes first.

**Execution is single-node.** The largest single query is bounded by one node's memory and
cores. Routing with cache affinity scales *read* throughput across nodes; a single query does
not span them.

**A table reference resolves to file groups beneath exactly one log.** Sharding is a physical
partitioning *below* the commit boundary and never above it.
[ADR-0015](../../adr/0015-the-shard-set-seam.md) considered the alternative — a shard as an
independently committed log — and **refused it rather than deferring it**, for v1 and for the
described v2. The reasoning is worth reproducing because it is the shape of several decisions in
this book:

- N independently committed logs means N version sequences, and a write spanning shards must
  claim a version in each atomically or not at all. That is a distributed commit protocol, and
  none of its proof exists.
- The cheapest correct cross-shard commit is a lock over the shard set — which is the design
  the concurrency milestone spent its entire budget proving must not exist, arrived at from a
  different direction. A shard set is a smaller blast radius than a warehouse, and that is a
  difference of degree in the one property established as non-negotiable.
- The preferred path to distribution expresses it as *exchange operators inside otherwise-normal
  plans*, preserving the single-node code path. Exchange operators distribute execution across
  file groups. They do not ask the catalog for N logs. The seam, as written, would have
  prepared for a v2 that the same decision rules out.

> **Key idea**
> The trigger to reopen this is a **measured** workload whose single-table *write* throughput
> exceeds one node's commit path. Read throughput does not qualify. Requiring the trigger to be
> measured rather than anticipated is what stops a scaling design being built against an
> imagined load.

**It is also not a distributed OLTP database.** Sharding a customer's ledger is their
architectural decision, not this system's. And **cross-region active-active writes** are not
planned: the topology is single-writer, and cross-region is recovery rather than active-active.

## 4.3 It is not a replacement for your data estate

SANKHYA is a participant in a lakehouse, not a substitute for one. That is a design commitment
with a mechanism behind it: analytical tables are ordinary Delta tables in a layout that mirrors
operational naming, and another engine reads them directly with no SANKHYA process in the path.

Two consequences follow that are easy to miss.

**The exit is `SELECT`.** Depending on this system does not mean the data becomes unreachable
without it. That is the answer to the single-vendor objection, and it is answered mechanically
rather than by promise.

**There is one documented exception, and it is a real cost.** A clone's log names *none* of its
origin's files — it records an origin and a version, and a read splices. A foreign Delta reader
pointed at a clone's directory therefore sees the files the clone wrote and not the rows it
inherited. **The open-storage claim holds for ordinary tables and not for clones.** A clone that
must be readable elsewhere is *materialised*, which is an explicit copy. Chapter 12 gives the
reasoning; the alternative bought kernel-readable clones with absolute paths that break the
moment a warehouse is restored somewhere else, trading a correctness property for an
interoperability one.

## 4.4 It is not a stream processor

SANKHYA ingests change data. It does not offer general stream transformation, a streaming
language, or windowed computation over an unbounded input as a product surface.

The boundary is drawn where it is because ingest is required to be a *producer*, not a storage
path. Everything an ingest path does ends in the one publishing library, because a configuration
that reached storage another way would be a second writer — and a second writer is the defect
this repository has met more than once, most expensively when a soak harness with its own writer
reported `PASS` for hours against a warehouse layout the product does not produce.

Nor is there **multi-source capture** beyond the primary transactional engine. Adding a second
capture source would reintroduce the operational estate the design exists to remove. If it is
ever needed it arrives as an optional external feeder through a documented interface, not as a
second pipeline inside the process.

## 4.5 It is not a graph database

The graph tier holds no durable state. An epoch is built by scanning published tables, carries
the snapshot it was built from, and is dropped on shutdown. There is no graph write path.

That is a *feature* with a cost, and both halves should be said. The feature: the graph cannot
disagree with SQL, because an edge exists only because a row exists. There is no hydration lag to
reason about at query time, no second durability contract, and no answer to the question "what
happens when the graph is behind" — because it cannot be behind in a way the snapshot does not
record. The cost: no graph writes, no persistent graph-native indexes, and a rebuild after
restart.

Two related non-goals:

- **No bespoke graph query language.** A structured API and SQL table functions now; the ISO
  standard (GQL) later if it comes, implemented as a rewrite onto those functions rather than as
  a second engine with its own semantics.
- **No exact betweenness or closeness centrality at scale.** Computationally infeasible at the
  target sizes; approximate variants are provided and are named as estimates in the API rather
  than presented as exact.

## 4.6 It is not an MDX server

Cubes are addressed from SQL, and MDX is deliberately not planned. It is a large language with
subtle semantics — the implicit current member, `.CurrentMember` context, solve orders — and
implementing it adequately is a multi-month project. It buys compatibility with a client
population that is small and shrinking, and which this system does not target.
[ADR-0007](../../adr/0007-the-cube-model.md) records the decision.

Related, and refused for a different reason: **automatic rewriting of arbitrary queries onto
materialised aggregates**. Explicit addressing — a cube named in a table function — ships in
weeks with near-zero correctness risk. Automatic subsumption is a multi-month project whose
failure mode is a plausible wrong number. It is revisited after 1.0, not before.

## 4.7 It does not load native extensions

Domain packs run through a capability-starved extension API. Dynamically-loaded native
extensions are not planned, for three reasons that are each individually sufficient: there is no
stable binary interface, so a version mismatch is undefined behaviour rather than an error; a
fault in one kills the process with no isolation; and the sandboxed tier already provides the
capability safely.

The mechanism is worth stating because it is stronger than a policy. An extension function
receives values and an invocation context, and holds **no handle** to a catalog, a connection, a
file or a clock. *The absence of the capability is the enforcement, which is stronger than a
check, because a check can be wrong.*

> **Pitfall**
> The honest cost of that design is stated rather than hidden: cancellation inside pack code is
> met *with a stated cost*. The call runs on its own thread and is **abandoned** when its bound
> passes, so a genuinely non-terminating extension function leaks one thread until the process
> ends. The runtime counts them rather than pretending otherwise.

## 4.8 It does not run a server on every platform

The command line and the client libraries are supported broadly. The **server** is not offered
on desktop-oriented platforms, because process, signal and file-locking semantics differ enough
to roughly double the integration matrix for a deployment target nobody runs in production.

There is a second, sharper limit that is a *not yet* rather than a non-goal, and it is the kind
of thing usually discovered at install time: the current build requires a newer glibc than the
declared baseline, so it would not start on older enterprise distributions. Closing that needs a
build against an old sysroot, which is release-pipeline work. `PLATFORMS.md` is generated from
the declaration and is authoritative.

## 4.9 What it is not *yet*

These are milestones, not boundaries. They are listed together here so that a reader evaluating
the system sees them in one place, and each is developed in the chapter that owns it.

| Not yet | Milestone | Note |
|---|---|---|
| The streaming replication transport | M2's remainder | Changes are drained through a SQL function today. Neither mainstream Rust PostgreSQL client supports the replication protocol, so this is real work rather than wiring |
| The slot lifecycle driver and the backfill reader | M2's remainder | The decision functions exist and have no caller on a timer |
| A measured graph benchmark against a public suite | M4's one carried-forward criterion | Carried as **unmet**, not reinterpreted. The primitives are correct against brute force and bounded by construction; they are not measured at scale |
| The pack bundle loader inside a running server | M4's remainder | Packs are built and tested; nothing in a running process loads a bundle directory |
| The gRPC control plane | Not built | The REST gateway is refused by design rather than pending |
| Partitioning on the **streaming arrival path** | Sized, not started | The batch publish path *is* partitioned — every published table writes `sank_data_date=YYYY-MM-DD/` directories and the log's add paths carry them |
| Bloom filters, the result cache, quantile sketches, leader election | Inside the M3 breakdown | Deliberately unbuilt; the result cache's *key* exists, because its correctness is a security property |
| A timer driving graph hydration or ingest | Not built | "The correctness contracts are built and tested and the machinery that runs them continuously is not" |
| Federated identity | M14 | A `Principal` is a fixed tenant; mutual TLS puts a client certificate where a door can see it without anything deriving an identity from it |
| An ephemeral cube lifetime | M14 | Ephemeral is the *intended* default and is not what `CREATE CUBE` does today — it persists a definition visible to every connection. Until the syntax exists, drop what you declare |
| Destructive purge against a system of record | Armed at M11 | Built, demonstrated, and **disabled by decision**. Building the purge path and arming it are two decisions |
| Multi-node, HA, failover, executor scale-out, metering | M12 | Needs a second machine |
| QR, SVD, eigendecomposition | Deliberately unbuilt | "They are where an in-house implementation is worse than none, because a subtly wrong SVD produces plausible singular values" |

## 4.10 Three defects it does not currently prevent

A boundaries chapter that lists only design choices is flattering. These are open, they produce
a **wrong number rather than an error**, and they are enumerated by a test that runs the same
cases against both tiers and compares.

| Case | Transactional tier | Analytical tier |
|---|---|---|
| Summing past a 64-bit integer | `9223372036854775808` | `-9223372036854775808` |
| Multiplying past a 64-bit integer | refuses | `0` |
| Summing decimals past 38 digits | `100000000000000000000000000000000000000` | `99999999999999997748809823456034029569` |

The third is the one that matters most. Fixed-point decimal was chosen *because* money must be
exact, and on overflow the analytical tier returns something close to the right answer instead
of refusing. Close is the wrong kind of wrong.

A partial mitigation exists and is not wired in: the statistics catalogue can predict, before a
query runs, whether summing a column *can* overflow. Its limit is worth stating plainly —
bounds are held as 64-bit integers, so a decimal column exceeding about nineteen digits has no
representable bound and the check answers "unknown". The columns most likely to overflow a
38-digit decimal are exactly the ones it cannot reason about. Widening the bound type to 128
bits would close that and has not been done, and nothing yet consults the check. Chapter 19
carries this as an open defect.

Three further differences change precision or ordering without making a figure wrong: `avg` of
integers is arbitrary-precision on one side and a 64-bit float on the other; division to a
repeating fraction gives twenty significant digits against sixteen; and text orders by the
database's collation on one side and by bytes on the other — so **any paged or ranked result
over text is in a different order in the two tiers**.

## 4.11 It is not open source

The licence is proprietary. Third-party dependencies remain under their own licences, which this
licence does not displace. This is stated in a boundaries chapter rather than a footnote because
it is a real constraint on how the system can be adopted, and a reader deciding whether to
depend on it deserves to meet the fact early.
