# 8. The read path

> The naive cure for analytical lag is to commit more often, and it makes the system slower —
> because metadata cost lands on *planning*, which is a fixed price paid before a single row is
> read. This chapter gives the alternative: freshness is a read-path property, delivered by
> splicing an in-memory arrival tier onto published Parquet at query time, under a rule that
> proves exact coverage and refuses rather than approximating. It then describes the table
> provider that makes planning do no file I/O, and the measurement that overturned two of this
> project's own assumptions about what makes a scan fast.

## 8.1 The problem, with the arithmetic

| Commits per table | Versions per day | Effect |
|---|---|---|
| 1 per minute | 1,440 | Negligible |
| 1 per second | 86,400 | Noticeable; snapshot resolution begins to cost |
| 10 per second | 864,000 | Metadata dominates; **planning becomes the tail latency** |

Committing more often produces small files, inflates version counts and grows metadata. The
result is the well-known failure mode in which a "real-time lakehouse" is either stale or slow,
and every attempt to fix the staleness makes it slower.

The impact is inversely proportional to query size, which is what makes it insidious: a high
commit rate is a tax that is **invisible on the queries nobody watches and severe on the queries
everybody watches.**

> **Key idea**
> **Freshness is a read-path property, not a write-path property.** The commit interval is tuned
> for storage efficiency; freshness comes from an in-memory tier spliced in at query time. That
> single inversion is what lets the backpressure ladder of Chapter 5 lengthen the commit interval
> under load without the system becoming stale — which is the payoff that makes the arrival
> buffer worth its complexity.

## 8.2 The tier stack

```
  T0  Ledger (PostgreSQL)      authoritative, current
        │  logical replication
  T1  Arrival buffer           in-memory Arrow, covers (committed, now]
        │  batched commit
  T2  Published tables         Parquet + log, covers [0, committed]
        │  refresh
  T3  Materialized aggregates  cuboids and derived results

  query ──▶ catalog (policy rewrite) ──▶ read planner ──▶ spliced plan
```

Every tier is defined by the same abstraction: **a body of data plus the log-position interval
it covers.** That uniformity is what makes the splice tractable — the planner reasons about
intervals, not about tier identities.

## 8.3 The splice, and its correctness rule

> The planner selects, for each table, a set of tiers whose coverage intervals are
> **contiguous, non-overlapping, and collectively cover `[0, target_position]`**.

Because every committed snapshot records the exact log position it contains — the same metadata
that provides exactly-once semantics — and every buffer epoch records its range, **the boundary
is exact rather than approximate.** There is no double-counting window and no gap. Intervals are
half-open at the start, so adjacent tiers abut exactly: there is no position both contain and
none that neither does.

Three properties follow, and the third is the one that makes it safe for a ledger.

| Property | Statement |
|---|---|
| No double counting, no gaps | By construction, from the coverage proof |
| Provenance is exact | Every response reports which tiers served it and over which intervals |
| Transactional atomicity survives the splice | A source transaction touching several tables carries one commit position. Since one target position is applied to every tier and every table in the request, either the whole transaction is visible or none of it is |

That third property is the reason the axis is log position rather than wall-clock time. **A
design splicing on wall-clock time would lose it, and could show one leg of a transaction
without the other.**

### The refusal

If a required interval cannot be covered — the buffer epoch was released but no committed
snapshot yet covers it — the planner **fails the query with a typed error**. It never returns a
partial answer.

> **Key idea**
> This is the read-path half of INV-1. *An incomplete answer that looks complete is the worst
> outcome the system can produce, because nothing downstream can detect it. A refusal is loud,
> immediate and attributable.* A coverage gap is a correctness event and surfaces as one.

### The second axis

Data tiering adds an orthogonal dimension to the same planner, and the symmetry is exact — which
is what keeps the planner comprehensible as it grows.

| Axis | Coverage rule | Authority |
|---|---|---|
| **Log position** (freshness) | Contiguous, non-overlapping, covering `[0, target]` | Commit metadata and buffer epochs |
| **Key range** (archival) | Hot and cold extents disjoint, together covering the declared domain | Transactional catalog for hot; archival registry for cold |

## 8.4 Merging the tiers

The merge strategy is selected by **declared table capability, never by heuristic**.

**Union only**, for append-only tables. No deduplication, no sort, no key comparison; cost is
essentially zero. Most high-volume tables are append-only, so most queries take this path.

**Latest-version-per-key**, for mutable tables. Because the buffer is tiny relative to published
data, the efficient shape is an anti-join: scan the published side *excluding* keys touched in
the buffer, then union the buffer's resolved rows. That turns a full merge into a hash probe
against a small build side.

> **Pitfall**
> Unioning the tiers and stopping there returns a row that has been updated **twice** — once as
> it was, once as it is. `COUNT(*)` says two; `SUM` adds the old value to the new one. Nothing
> about the result indicates it, and *every* correctness property built around it still holds:
> exactly-once capture, reconciliation against the source, splice coverage. None of them is
> about **resolving** two versions of a row, so none of them fails. This was a real defect, and
> the reason the suite never caught it is that every test reading captured data used an
> append-only fixture.

The strategy is declared rather than inferred for a precise reason: inferring *"this table looks
append-only because no update has arrived yet"* is correct until the first update, at which point
every query silently starts double-counting.

## 8.5 The arrival buffer

A single-writer, many-reader, epoch-based immutable ring. The applier appends into an open
epoch; on seal the epoch becomes immutable and is published by an atomic pointer swap. Readers
take a reference and are never blocked by, and never block, the writer — which matters because
the applier is the one thing that must never stall (INV-2).

The buffer holds **change events, not merged state**: key, operation, position, payload. Merging
is a read-path operation. That makes the writer trivially fast — append-only, no index
maintenance — and pushes cost to the reader, where it is small because the buffer is small by
construction.

Each epoch maintains a compact digest of the keys it touched, so the planner can skip the splice
entirely when a query's predicates provably do not intersect the buffer.

> **Key idea**
> **The common case — a historical query over old data — must cost exactly nothing for the
> buffer's existence.** Without the digest the buffer taxes every query; with it, it taxes only
> the queries that need freshness.

### Retention, not eviction

> **A segment may be released only once a durable tier covers it.** Not when it is old, not when
> memory is tight, not when it has been read.

This inverts the usual cache relationship, and the inversion is the point. A cache evicts under
pressure and takes a miss. This tier has nothing to miss *to* until publication has happened, so
evicting under pressure does not degrade an answer — it destroys one. Worse, releasing a segment
from the middle of the interval opens a coverage gap, and the splice is a proof of exact cover
that cannot be talked into approximating one. The query would be refused.

So when memory runs short and nothing is releasable, the only correct response is to push back
on ingest. That is reported as a distinct condition rather than absorbed, because it is a
**publication problem wearing a memory problem's clothes**: the tier is full because publication
has stalled, and adding memory treats the symptom. The escalation ladder has a deliberate gap
between its soft and hard limits so that ingest gets a chance to lengthen its commit interval —
the highest-leverage response — before it is stopped rather than running normally into a wall.

The buffer is hard-capped in bytes with per-tenant sub-caps, and **it never drops data**. It is
not a cache of the truth; it is the not-yet-durable part of the truth.

### Coverage is trimmed; data is not

The buffer physically retains segments the published tier already covers, but it **declares**
coverage starting at the durable frontier, so the two tiers abut exactly and the splice succeeds.
Declaring the physical extent instead would overlap, and the planner rejects overlapping tiers
rather than guessing which to believe.

The consequence is that a scan must filter **per row** — `durable_through < lsn <= target` — not
per segment. A segment straddling the frontier is half durable and half not; returning it whole
would double-count its durable half against the published tier.

That is the same defect, one layer up, as suppressing duplicates per batch rather than per row
— a mistake this system has already made once, in the ingest pipeline (Chapter 9). The straddling
segment is retained whole rather than split, because splitting costs a copy to reclaim memory
the next publication frees anyway.

## 8.6 Routing

The routing decision is a **pure function** of query shape, read mode, session pin, table class,
table capabilities and freshness state — deterministic and unit-testable with no I/O, even though
the planner that applies it performs I/O.

Making it pure is not an aesthetic preference. Routing is where a query silently acquires the
wrong answer: read the wrong tiers and the result is well-formed, plausible, and missing rows. A
pure function can be exhaustively tested against a table of cases; a decision scattered through a
planner that also does I/O cannot.

### Managed tables

| Query shape | Read mode | Tiers |
|---|---|---|
| Point lookup, small range | Strong | Ledger |
| Point lookup | Fresh | Buffer, falling through to a pruned published scan |
| Analytical scan or aggregate | Fresh | Published + buffer |
| Analytical scan or aggregate | Pinned snapshot | **Published only** |
| Aggregate matching a materialised view | Fresh | Derived + buffer, if splice-able; otherwise fall back |
| Aggregate matching a materialised view | Pinned snapshot | Derived only — the only mode where result caching pays |
| Graph traversal | any | A published-derived epoch; the buffer is **not** spliced |
| Write | — | Ledger, always |

The first row is worth a sentence because the rule is unusually simple: a point lookup by key
goes to the transactional store because that is where it is *simultaneously fastest and
freshest*. A b-tree probe against the authoritative copy beats pruning a thousand Parquet files,
and it cannot be stale. The two considerations point the same way.

And the fourth row is a definition rather than an optimisation: **pinned reads exclude the buffer
by definition, which is exactly why they are deterministic and replayable.** Reproducible outputs
must use snapshot mode, and they are reproducible *because* of the exclusion.

### External tables

An external table has one tier, so coverage is trivially complete and the splice is a no-op —
not a special case bolted on, but what the splice reduces to when there is nothing to splice.
Strong reads and writes are **refused by name**, with a message saying that this table has no
transactional tier and which modes it does support.

Four guarantees depend on there *being* a transactional tier: read-your-own-writes,
strongly-consistent reads, the arrival splice (no change stream means no log position, which
means coverage is undefined — and refusing exactly that is the splice's entire purpose), and
write authorisation and audit.

None of those degrade gracefully. A strongly-consistent read served from published data alone
is not *slightly* stale; it is a claim about currency that is false, and nothing in the result
says so. **The refusals are the feature.** A user is meant to be oblivious to *which tier*
answered — that is the point of the splice. They cannot be oblivious to whether a transactional
tier exists at all, because that changes which guarantees are on offer.

The class is declared in the **table's own log**, in the metadata action's configuration map, not
in this system's configuration — a fact about a table belongs with the table. **Absence means
external**, because defaulting the other way would have a table claim a transactional tier it
does not have, and the conservative default is the one that under-claims.

### What the user types

A user writes `sales.orders` and never writes anything else. There is no `warehouse.sales.orders`
and deliberately never will be.

Putting the tier into the name would encode a *physical* fact in a *logical* identifier, and the
physical fact moves: a row written this morning is in the transactional store, and by this
afternoon it is in Parquet. A name that encodes where a row lives is a name whose meaning changes
underneath the query. And hand-written unions across a moving frontier either double-count the
overlap or miss the gap — the exact defect the splice exists to make impossible.

What varies is the *mode*, set on the session:

```sql
SET sankhya.read_mode = 'pinned';
SET sankhya.snapshot   = 41;
```

## 8.7 The table provider

The provider is SANKHYA's own. The table format library says **which files exist and what is in
them**; it does not read them, does not decode them, and does not appear in the execution plan.
Scan execution is the query engine's Parquet source, unmodified.

The reason is the version skew of Chapter 5 — the released Delta and Iceberg libraries pin an
Arrow generation two majors behind the query engine's, and the trait-identity problem is worse
than the type problem. Using the storage library for metadata only means **no bulk data crosses a
version boundary; only small metadata structures, which convert trivially.** The skew is chronic
rather than transient, and the architecture accommodates a permanently-lagging storage library
rather than waiting for a release.

But owning the provider is better than a workaround, and five things depend on it: injecting our
own distinct-value statistics into the optimiser — neither vendor provider supplies them, which
is the root cause of poor join ordering; partition-transform inversion and derived-column
correlation; wiring the Parquet reader to our own cache; ordering files by statistics so top-N
queries can stop early; and turning delete vectors into a plan-time row selection rather than a
post-filter.

### Planning does no file I/O

Row counts come from the log, which already records them. The alternative is one footer read per
file before a single row is read — the small-file penalty of Chapter 6, moved somewhere
compaction cannot help.

| Files | Provider | Directory listing | Speed-up |
|---|---|---|---|
| 50 | 0.54 ms | 1.15 ms | 2.2× |
| 200 | 0.63 ms | 3.02 ms | 4.8× |
| 800 | 1.37 ms | 10.33 ms | 7.5× |

The honest caveat: the provider is **not flat**. Sixteen times the files costs about 2.5× more
planning, because replaying the log grows with commit count. The cost has been moved from one
seek per file to one sequential read of a log, not abolished — which is what the checkpoints of
Chapter 6 address.

### Four details it gets right

**1. Statistics are marked exact only when nothing can be filtered out.** A query pinned below
what the tiers hold has an *upper bound*, not a count. Reporting it as exact lets the optimiser
order joins on a number that is simply wrong — a slow plan chosen confidently, which is harder to
notice than a slow plan chosen for want of information.

**2. The commit-position column is read only when the query pins a position**, because the target
filter is evaluated on it, and it is projected away afterwards. Time travel genuinely costs a
column the caller did not ask for, and hiding that would be dishonest about its price. When no
tier holds anything past the target, the filter provably removes nothing, and **neither the
filter nor the column read is planned at all** — which matters because the cost is per table and
therefore compounds with join arity.

**3. The scan reports its own statistics, not the table's.** These are different numbers arriving
at different times: the table's are read during logical planning, the scan's during physical
planning, and join selection reads the second. A provider supplying only the first leaves every
table looking unmeasurable at the moment the engine decides how to join it — so it repartitions
tables it could broadcast, and because tables reporting no size are ordered against tables that
do, **one absent figure moves every join in the query**. The scan's figures are also counted over
the files that survived pruning, so a selective predicate is reflected in the number the decision
actually uses.

**4. File grouping is left to the engine above its own threshold.** The engine splits file groups
by byte range, which balances on size and beats anything a provider can do by counting files —
but only for scans large enough to be worth splitting, below which it leaves a single group
alone, and a single group is a single partition. So the provider deals files out only *below*
that threshold. Doing both is worse than either: the engine then rebalances an arrangement
already unbalanced by file count.

### Three rules the provider enforces

**The splice is resolved at construction, not at scan.** A query pinned at a position must see
one consistent set of tiers, and resolving them inside `scan` would let two scans of the same
table in one query disagree — a self-join whose halves saw different data. The tiers are resolved
once, the proof is kept, and `scan` only builds a plan over what was already decided.

**A selected tier may reach past the target.** The planner is greedy from the origin and picks
the tier reaching furthest, so a published tier covering `(0, 100]` is a legitimate choice for a
query pinned at 60 — coverage is a statement about what a tier *contains*, not about what a query
should *see*. So every tier is filtered to `commit_lsn <= target` at the scan, **without
exception**: a tier that "obviously" cannot overshoot is filtered anyway, because the cost is a
predicate the engine pushes down and the alternative is a correctness argument that has to be
re-made every time a tier is added.

**Only selected tiers may be read.** A tier the planner rejected is rejected because reading it
would double-count. Registering every available tier and letting the query sort it out would
discard the proof entirely.

## 8.8 Pruning, and the measurement that overturned two assumptions

File pruning by recorded statistics, over 200 files of 2,000 rows:

| Predicate selects | With statistics | Without | Speed-up |
|---|---|---|---|
| One file in 200 | 1.26 ms | 9.21 ms | 7.3× |
| A tenth of the table | 3.89 ms | 10.81 ms | 2.8× |
| Everything | 17.52 ms | 17.71 ms | 1.0× |

The shape is right: pruning pays in proportion to what it eliminates, and costs nothing when it
eliminates nothing.

Filter pushdown and late materialisation are a different story, and it is the most instructive
measurement in this book. The query engine ships with both **disabled by default**, so SANKHYA
asserts its required engine configuration at startup and fails loudly on unexpected values,
rather than setting it once and trusting it — a configuration default that changes upstream
between versions would otherwise be an invisible performance regression.

Then the setting was measured, at TPC-H scale factor 1, pushdown on against off:

| Rows surviving the filter | Off | On | Ratio |
|---|---|---|---|
| 1 in ~6,000,000 | 5.6 ms | 5.5 ms | 1.02× |
| 1 in ~1,500 | 4.5 ms | 4.8 ms | 0.94× |
| 1 in ~60 | 4.0 ms | 4.6 ms | 0.87× |
| 1 in ~7 | 116.6 ms | 162.0 ms | **0.72×** |
| all rows | 110.6 ms | 111.7 ms | 0.99× |

With filter reordering compounding it, **Q6 went from 351 ms to 917 ms at eight clients — 2.6×
slower.**

> **Key idea**
> Late materialisation saves the decode of payload columns for rows a predicate eliminates. On
> this data those rows have already been eliminated, by row-group and page statistics, before any
> decoding begins. **Pushdown cannot save work that is not being done; what it adds is per-row
> bookkeeping on the scan that remains.** The two mechanisms are not complementary here — the
> cheaper one has already won.

The disposition is deliberately *not* "pinned off", because pinning a setting off is still
pinning it, and the evidence supports *not always* rather than *never*. The meta-lesson is the
part worth carrying: **a setting worth asserting at startup is worth measuring on something
somebody else designed.**

## 8.9 Where the read path refuses

Collected in one place, because a system whose dominant verb is refusal owes the reader a list.

| # | Refusal | Because |
|---|---|---|
| 1 | Coverage gap in the splice | An incomplete answer that looks complete is undetectable downstream |
| 2 | Strong read of an external table | It has no transactional tier; the message says which modes it does support |
| 3 | Write to an external table | This system is not the writer of record for that table |
| 4 | Maximum-freshness request under degraded freshness | The buffer cannot honour the bound, and saying so beats serving stale data silently |
| 5 | Admission control | Queues or rejects; never admits a query it cannot afford. Rejection is typed and carries a retry hint |
| 6 | A sketch aggregate under an exactness requirement | Rejected at planning time, with an error naming the exact replacement |
| 7 | Completeness below a stated threshold | Attachable to any aggregate |
| 8 | An uncovered archival range intersecting the predicate | Typed error |
| 9 | Fresh-mode lag exceeded | Blocks until lag is within bound, or fails explicitly |

## 8.10 Two defects worth carrying

**A projection over an empty table returned the whole schema.** The scan produced an empty
result carrying every column rather than the projected subset. Every existing test of that path
had selected *every* column — which is the one projection under which the bug cannot appear. It
is not a niche state: an empty table with a log is exactly what a newly created table is.

**The secured table was secured in name only.** The provider handed the policy predicate to the
engine as a pushdown filter, and the in-memory provider *declines* filters, so every row came
back with no error raised anywhere. That is why the presence of the row filter in the final
physical plan is now asserted rather than assumed, and why the tenant predicate is injected by an
analyser rule *and* independently asserted by the provider (Chapter 5, §5.8).

Both share a shape: **a test that exercises the happy path of a mechanism cannot see a mechanism
that silently does nothing.** Chapter 23 is about what is done with that.

## 8.11 What is not built

| Not built | Note |
|---|---|
| Bloom filters | Column-specific and off by default where built; not built here. They pay only when a value is likely absent from most row groups |
| The result cache | Its key exists; the cache does not. Chapter 6 §6.8 gives the reason |
| Delete resolution into plan-time row selections | Inside the M3 work breakdown, deliberately unbuilt |
| File ordering by statistics for early termination | Same |
| Quantile sketches, and per-column encoding chosen from measured statistics | Same |
| Bounded-memory exact order statistics | Exact order statistics currently buffer their input; the requirement asks for a bounded-memory algorithm over large inputs and this is not one |
| The exactness gate wired into a session | The check exists as a function with no caller |
| A catalog proper | Nothing routes a captured table to its resolved provider automatically, because there is no catalog mapping a table name to a provider |
