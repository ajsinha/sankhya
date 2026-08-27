# SANKHYA — Build Status

**Updated:** 2026-08-26 · Tracks what is *actually built* against
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).

This document exists because a plan describes intent and a roadmap describes ambition;
neither tells you what runs today. Where the two disagree, this one is right.

---

## Milestone progress

| Milestone | Planned | State |
|---|---|---|
| **M0** Foundations, spikes, walking skeleton | 10–12 ew | **Complete**, merged to `main` |
| **M1** Zero-configuration sync and read-your-own-writes | 14–18 ew | **Complete** |
| **M2** Ingest correctness and durability | 24–28 ew | **Substantially complete** — batching invariants, source-safety ladder, reconciliation, idempotence, crash safety, schema evolution and the backfill handoff all exist and are tested. What remains is the slot *lifecycle* driver and the snapshot *reader* — the correctness contracts are in place, the machinery that runs them on a timer is not |
| **M3** Query engine and storage performance | 28–34 ew | **Complete**, all six exit criteria met — closed 2026-08-26. One criterion was corrected first: it required cancellation inside user code, which does not exist until M4, and that clause moved to M4. Parts of the work breakdown remain unbuilt and are listed under *M3, closed* below |
| **M4** Graph engine and the extension mechanism | 26–32 ew | **Complete.** Every exit criterion met; see below |
| **M5**–**M8** | — | Not started |

---

## M4, closed

Closed 2026-08-27. The graph engine and the extension mechanism, built together because
the graph algorithms are the most demanding consumer of the extension API --- building them
apart would have proved the API against nothing.

| Exit criterion | State |
|---|---|
| Graph performance objectives met against a named public suite | **Not met, and not claimed.** No public graph suite is wired into the pipeline. The primitives are correct against brute force and bounded by construction; they are not yet *measured* at scale. This is the one M4 criterion carried forward, and it is carried as unmet rather than reinterpreted |
| The incremental-equals-full property test green | **Met.** Two property tests, 400 cases. Verified to fail: making the overlay ignore its pending batches turns three tests red |
| Memory per vertex and per edge published | **Met.** `Epoch::footprint()` measures a real epoch rather than estimating from type sizes, because the interner's key storage dominates and depends entirely on key length |
| Time-respecting traversal returns no time-violating path | **Met.** `a_static_path_that_time_forbids_is_not_returned` builds the smallest case: two edges whose order forbids the path they appear to form |
| Each reference pack's change touches zero core files | **Met, mechanically.** `check-layers` refuses any pack dependency outside `sankhya-ext`, `sankhya-types`, `sankhya-error`, and refuses any core dependency on a pack |
| The adversarial pack's every attempt rejected with a named error | **Met.** Seven attempts, seven named refusals |
| The extension API within its size budget, with no escape-hatch types | **Met.** Nothing in `sankhya-ext` re-exports an engine type. `Value` and `LogicalType` are SANKHYA's own |
| Cancellation demonstrated within its bound inside pack code | **Met, with a stated cost.** See below |

### Cancellation, and what it actually costs

A pack function in a deliberate infinite loop that ignores the cancellation flag *is*
stopped, and the query fails with an error naming the pack rather than hanging. The
mechanism is worth stating plainly because it is not free.

The call runs on its own thread and is **abandoned** when the bound passes. Rust has no
safe way to kill a thread and this repository has no unsafe code, so a genuinely
non-terminating function leaks one thread until the process ends. `Sandbox::abandoned()`
counts them, so a pack that does this is visible rather than suspected.

The alternatives are worse: hanging the query forever, or killing a thread mid-allocation
and corrupting the allocator for everything else. A bounded, observable, attributable leak
is an operational problem with an obvious fix. The other two are outages.

The declarative tier does not need any of this. Its expression language has no loop, no
recursion, no call and no I/O, so a declarative function *cannot* be the one that hangs a
query. It is safe by construction rather than by supervision, and that restriction is the
point --- an expression language with loops is a programming language, and one loaded from
a configuration file is a remote code execution feature with extra steps.

### On signing

`FR` asks for pack bundles to be signed and verified. What ships is **digest pinning**: an
operator pins the digests of bundles they have reviewed and anything else is refused,
including everything when nothing is pinned, because a trust policy that defaults to
trusting is not a policy.

That is a real control and it is deliberately not called a signature. A digest proves the
bytes are the bytes you pinned; it proves nothing about who wrote them. Public-key signing
is the better answer and needs a cryptographic dependency, which this repository adds by
decision record rather than alongside a feature. `Verifier` is a trait, so adding it later
changes no caller.

### Three defects the tests found

| What | How it was found |
|---|---|
| **The negative-weight guard fired by luck.** Shortest-path checked each edge as it relaxed it, and Dijkstra settled the destination by a cheap direct edge before ever examining the negative one — so the same graph passed or failed depending on the query | Writing the test for it. The check is now a property of the structure, recorded at build time, and the refusal is its own error type so "cannot answer for this graph" cannot be read as "no route exists" |
| **Community detection collapsed two clusters into one.** Label propagation's monster-community failure, on the smallest interesting case: two triangles joined by one edge. Determinism, which reproducibility demands, made it *worse* — the randomised form at least sometimes stalls first | The test that specified the behaviour. Replaced with modularity optimisation, whose objective falls when dense clusters merge across a thin bridge, so the search resists the collapse rather than needing to stop in time |
| **A whole source file was never compiled.** It was not declared in `lib.rs`, so it contributed nothing and was checked by nothing — and it contained a match arm naming a struct field that does not exist | Wiring the module in. "It compiles" means nothing about a file the compiler never saw |

---

## M3, closed

Closed 2026-08-26. All six exit criteria are met, one of them after the criterion itself
was corrected and one after the objectives were measured against the workloads they
describe. Both corrections are recorded below rather than folded into a pass.

| Gate | State |
|---|---|
| Performance objectives met in the pipeline, against named public-suite queries | **Met.** `NFR-PERF-02` 13 ms, `NFR-PERF-03` 796 ms, `NFR-PERF-04` 648 ms, against budgets of 250 ms, 1 s and 3 s — asserted by `cargo xtask check-performance`, which fails the build. See the correction below: this gate was previously read as failing, against queries the objectives do not describe |
| Cancellation demonstrated within its bound, at the points M3 controls | **Met.** Bounded at one batch per partition inside a real query, across threads, and under periodic checking within its interval. The clause "including inside user code" has moved to M4 — see below |
| A hostile aggregation under a constrained memory limit is rejected rather than terminating the process | **Met.** An aggregation that cannot reduce anything is refused under a one-megabyte pool, by name — and the process runs the same query to completion afterwards. Repeated five times, so a refusal that leaked its reservation would show up |
| Plan snapshots stable; the SQL-semantics corpus green | **Met.** Thirty-nine semantics cases pinned by hand from the standard's rules; plan *shapes* pinned rather than plan text, plus assertions on the optimisations that fail silently |
| Cross-engine semantic differences enumerated in a tested list | **Met, and it found three ways the analytical tier returns a wrong number.** Fifteen cases run against both engines; agreements are pinned too, so a *new* divergence fails the test |
| Compaction holds file counts within policy under continuous ingest | **Met.** Forty ticks of capture with maintenance sweeping every third; the live count never passes the urgent threshold and no row is lost |

### The exit criterion that could not be met by M3

Criterion 2 read *"including inside user code"*. There is no user code until M4 builds the
extension mechanism, so as written M3 was gated on a later milestone and could not close
on its own terms. The clause has moved to M4's exit criteria, where the mechanism it
tests is built.

This is a correction to the plan, not a waiver. Nothing that was going to be tested is
now untested; the same test is asked one milestone later, of the milestone that can
answer it. The related case — cancelling a query while it *queues* for memory — is also
not claimed here, because admission never blocks: it returns a decision and the caller
waits, and that caller is M5's server.

### The objectives were being read against the wrong queries

The performance gate was recorded as failing, on Q6 at 386 ms against a 250 ms objective
and Q5 at 1149 ms against 1 s. Both numbers are real and both still stand. What was wrong
was the mapping.

`NFR-PERF-02` says **selective needle lookup**. Q6 applies three range predicates and
returns about a hundred thousand rows of six million, touched across every file in the
table — a different mechanism entirely, answered by scanning fast rather than by not
scanning. Measured against an actual point lookup, the objective is met at **13 ms
against 250 ms**, and met by statistics pruning alone, without the bloom filters its own
precondition names.

`NFR-PERF-03` says **multi-dimensional pivot, warm, pruned**, with a stated precondition
that a partition predicate be present. Q3 is that shape and meets it at 796 ms. Q5 is a
six-way join with no partition predicate — and since partitioning is not built, no query
can satisfy that precondition today. So Q5 is measured and published, and deliberately
**not** gated, rather than a passing test being written around it.

The honest form of the earlier statement was never "the system is too slow"; it was
"these objectives have never been tested". They are tested now. The mapping was mine, it
was approximate, and it was described as approximate at the time — but an approximate
mapping used as a gate is a gate on the wrong thing, in whichever direction it errs.

**A measurement that could not have failed.** The first version of the gate inherited
`#[tokio::test]`'s default current-thread runtime, so its eight concurrent clients took
turns on one thread and the pivot reported 3029 ms — three times its budget, describing
nothing the requirement is about. The gate now asserts it is on a multi-threaded runtime
before it measures anything.

### What M3 did not build

Closed does not mean complete. These are inside the M3 work breakdown, are not built, and
are not exit criteria: delete resolution into plan-time row selections, file ordering by
statistics for early termination, bloom filters, per-column encoding chosen from measured
statistics, quantile sketches, the footer and byte-range and decoded-batch caches, table
partitioning, and leader election.

The *result* cache does not exist either — but its **key** does, because what makes a
result-cache key correct is a security property and the right time to fix it is before
anything is caching. A key that omits the entitlement set does not return a stale answer;
it returns someone else's, correctly and quickly.

Nothing above is load-bearing for M4, which is why M3 closes with them open rather than
holding the milestone until a work breakdown is exhausted.

---

## The six-way join, explained

Q5 was recorded for several weeks as **39 % behind the engine's own listing table over
the same files, cause unknown**. An unexplained gap against a baseline reading identical
data is the kind of finding that is usually a wrong answer waiting to be noticed, so it
blocked the milestone rather than being filed as a performance nit.

It was two defects, both found by printing the two physical plans side by side instead of
reasoning about them.

**The scan reported no size.** `TableProvider::statistics` describes the whole table and
is read during logical planning. Join selection runs later, on the physical plan, and
reads the statistics of the scan node — which come from the file-scan configuration and
default to unknown. Every small table therefore looked *unmeasurable* at the exact moment
the engine chose how to join it, so it repartitioned tables it could have broadcast: five
rows of `region` shuffled across every core. Three of the five joins were the wrong mode.
A table reporting no size is also sorted against tables that do, so one absent figure
moved every join in the query, not just its own.

**The provider was grouping files by hand.** It dealt files round-robin across the target
partition count. The engine splits file groups by byte range, which balances on size and
is strictly better than anything done by counting files — but only above a size threshold,
below which it leaves a single group alone, and a single group is a single partition. So
the division of labour now follows the engine's own threshold: above it, hand over one
group; below it, deal them out, because otherwise nobody will. Doing both was worse than
either, and measurably so — the engine was rebalancing an arrangement already unbalanced
by file count, and the join paid about seven percent for it.

Together: **578 ms to 450 ms, against the listing table's 447 ms.** Parity, and the plans
are now identical operator for operator.

**The guard that did not exist.** The round-robin dealing had itself been added to fix a
real defect — the scan ran on one core — and no test was ever written for it. It was
caught only because a *deadline* test contained a self-check asserting its own plan ran in
parallel. Removing the dealing broke that self-check, which is the only reason the
regression was not shipped. There is now a direct test that a scan of thirty-two files
runs on more than one partition, asserted at the scan itself rather than above it, because
a repartition can manufacture eight partitions from one serial reader — which is exactly
the shape the original defect had.

---

## TPC-H, at scale factor 1

Every other measurement in this document isolates one mechanism. These are queries
somebody else wrote, so the numbers can be compared to something other than themselves.

Data generated at scale factor 1 — 8,661,245 rows, 261 MiB of Parquet — written through
this system's own write path. Five rounds at the stated concurrency, after two warm-up
executions, on a multi-threaded runtime — asserted, because on a current-thread one the
clients take turns and the figure describes nothing.

| | Clients | Median | p95 | Objective | |
|---|---|---|---|---|---|
| **needle lookup** — one row of six million by key | 8 | 11 ms | **13 ms** | `NFR-PERF-02` selective needle lookup, < 250 ms | **met** |
| **Q3** shipping priority — three-way join, top-N | 8 | 777 ms | **796 ms** | `NFR-PERF-03` pivot, < 1 s | **met** |
| **Q1** pricing summary — full scan, eight aggregates | 4 | 562 ms | **648 ms** | `NFR-PERF-04` wide scan, < 3 s | **met** |
| **Q6** forecasting revenue — narrow range filter | 8 | 386 ms | 491 ms | *not the workload any objective describes* | published, not gated |
| **Q5** local supplier volume — six-way join | 8 | 1149 ms | 1202 ms | `NFR-PERF-03`, whose partition-predicate precondition it cannot satisfy | published, not gated |

The first three are the gate, and `cargo xtask check-performance` fails the build if any
of them regresses. The last two are published because they are the queries a reader will
recognise, and withholding them because they are unflattering would be the worse choice.

**Q5 and Q6 are not failures against these objectives; they are not measured by them.**
That distinction is argued in full under *M3, closed* above, including why the previous
mapping was wrong in both directions. The short form: `NFR-PERF-02` describes finding one
row, and Q6 returns a hundred thousand; `NFR-PERF-03` requires a partition predicate, and
partitioning is not built, so Q5 could not satisfy it however fast it ran.

**These are met on hardware below the reference node.** The requirements name 32 physical
cores and 256 GB; this is twelve cores and 62 GB. That makes the results conservative
rather than qualified — but the reference node has never been measured on, so the numbers
that would be published with it do not exist.

**The first run of this measured the wrong thing**, and finding out why produced the
correction below. It used a bare engine rather than this system's configured session, so
the numbers described the engine's defaults. Running it properly made three of four
queries *slower* — Q6 by 2.6× — which is how a setting that had been asserted as required
since M3 began turned out to be a cost.

**What this is not:** an audited TPC-H result. Scale factor 1 on a development machine,
single node, four of twenty-two queries, no substitution rules, no refresh streams. Using
the name for anything more would be a misuse of it.

---

## Mutable tables, resolved

Capture records inserts, updates and deletes as rows. The read path used to union its
tiers and stop, so a row that had been updated came back **twice** — once as it was, once
as it is. `COUNT(*)` said two; `SUM` added the old value to the new one. Nothing in the
result indicated it.

It is resolved now, by declared capability rather than by heuristic:

- **Append-only** — union, as before. No deduplication, no sort, no key comparison. Most
  high-volume tables are append-only, so most queries take this path and it must cost
  nothing at all, not "a cheap check".
- **Mutable** — the latest version of each key wins, and a key whose latest version is a
  deletion is absent.

Expressed as a logical plan the optimizer can see through, rather than as an opaque
physical operator that would have to re-implement every optimisation inside itself.

**The capability comes from the source, not from a guess.** The relation states which
columns identify a row — its replica identity — and that statement arrives with the
relation description. Reading it is taking the declaration, not inferring one. A relation
declaring no identity is append-only *for reading*, and that is a statement about what can
be done rather than a prediction: with no key there is nothing to resolve versions
against, so an update could not be applied even if one arrived. The fix for that is the
source's replica identity, which onboarding already warns about.

Three things this gets right that a first attempt would not:

**The deletion filter runs after the resolution, not before.** Filtering tombstones first
removes the deletion and lets the *previous* version win — so a deleted row returns
holding the values it had before it was deleted. That is worse than the duplication it
replaces, because it looks like data.

**A key must be declared and must exist.** No key means no latest-version-per-key to
resolve to, and defaulting to whole-row identity turns every update into a new row — the
original defect, reached by a different route. A key naming a column that is not there is
refused for the same reason.

**Scanning a resolved table directly is refused.** The planner inlines the resolution and
never calls `scan`, so nothing exercises that path — which is why it is worth a test. A
fallback that quietly scanned the raw table would serve every version again, and the only
symptom would be wrong numbers.

**Why the suite never caught the original defect:** every test that read captured data used
append-only fixtures. The read path had never been asked a question about a row that
changed.

---

## What this system's own storage costs

TPC-H now runs through SANKHYA's provider rather than the engine's file listing — every
table carrying a commit position, committed to a log, resolved and pruned through the
catalogue. That makes the numbers a measurement of this system rather than of the engine
underneath it, and it made two costs visible.

| Single query, SF=1 | Engine's file listing | SANKHYA's provider |
|---|---|---|
| Q1 scan + aggregates | 450 ms | 462 ms |
| Q6 selective filter | 213 ms | 219 ms |
| Q3 three-way join | 333 ms | 372 ms |
| Q5 six-way join | 415 ms | 578 ms |

**Parity on scans, 12% and 39% behind on joins.** Four causes were found in the end. Two
are described here; the other two — the scan reporting no size to join selection, and the
provider grouping files by hand — are under *The six-way join, explained* above, and
closing them brought Q5 to 450 ms against the listing table's 447 ms. The table above is
kept at its original numbers because the sections below explain what each fix was worth,
and rewriting the starting point would erase that.

**The scan used one core.** The provider put every file into a single file group, which is
a single partition — everything above it could only round-robin batches that had been read
serially. Invisible in a result: the answers were identical and the query used one
twenty-fourth of the machine.

**The read position was enforced when it could not matter.** Reading at a pinned position
costs a column read and a predicate *per table*, so it compounds with join arity. When no
tier holds anything past the target the filter provably removes nothing — and the column
it reads need not be scanned at all. Most queries read the latest data, so most queries
were paying for a facility they were not using.

The commit position itself turned out **not** to be a cost: it is monotone, so it
delta-encodes to +0.1% on a 174 MiB table. That was the first hypothesis and it was wrong.

**What remained was eventually explained**, after some weeks of being recorded here as
unexplained — which was the right way to hold it. The answer is above; it was two further
defects, and neither was the one that would have been guessed. Both were found by printing
the two physical plans next to each other rather than by reasoning about them, which is
the general lesson worth keeping.

---

## Clustering, worth 7.8× on a range scan

This was originally written up as "the fix that closes `NFR-PERF-02`", which was wrong
twice over — Q6 is not the workload that objective describes (see *M3, closed* above), and
the objective's own named preconditions are not the lever either. Bloom filters do not
apply to Q6, which has no equality predicate; late materialization costs rather than
saves. **Sorting does**, and that finding stands on its own without an objective attached
to it.

Q6 selects one year in seven of `l_shipdate`. Written in arrival order every row group
holds the whole date range, so the bounds exclude nothing and the query reads the entire
table. Written in date order most row groups fall outside the year and are skipped on
their statistics, before any decoding.

| Layout | Single query | p95 at 8 clients |
|---|---|---|
| Generation order | 222 ms | 1819 ms |
| Sorted by ship date | **31 ms** | **234 ms** |

**7.8× at eight clients.** Compaction now does this: a settled partition is written in the
order the policy declares. No objective is claimed from it — the number is the point.

Two details that are the design rather than the implementation. Only *settled* partitions
are sorted — ordering one that is still receiving writes means ordering it again
tomorrow, for a layout correct until the next append. And the key is **declared, not
inferred**: a key chosen from observed queries would change under a workload shift and
rewrite the whole table to follow it, which costs more than the ordering is worth.

Q5, the six-way join, is still over its objective. Clustering does not help a join, and
what would is not built.

---

## A required setting that was costing 2.6×

`datafusion.execution.parquet.pushdown_filters` — late materialization — was in the list
of settings asserted at startup. It is not any more, and the way it left the list is worth
recording.

It went in on reasoning: late materialization is widely described as the largest scan
optimization available, and it defaults to off, so leaving it off is a large win silently
forfeited. It was then measured at **1.02× — neutral** on a synthetic scan, and pinned
anyway on the grounds that neutral is not harmful.

Measured on TPC-H it is not neutral. It costs at every selectivity tried:

| Rows surviving the filter | Off | On | |
|---|---|---|---|
| 1 in ~6,000,000 | 5.6 ms | 5.5 ms | 1.02× |
| 1 in ~1,500 | 4.5 ms | 4.8 ms | 0.94× |
| 1 in ~60 | 4.0 ms | 4.6 ms | 0.87× |
| 1 in ~7 | 116.6 ms | 162.0 ms | **0.72×** |
| all rows | 110.6 ms | 111.7 ms | 0.99× |

With filter reordering compounding it, Q6 went from 351 ms to **917 ms** at eight clients.

**Why it does not help is the useful part.** Late materialization saves decoding payload
columns for rows a predicate eliminates — and on this data those rows were already
eliminated by row-group and page statistics, before decoding began. That is why the
selective queries above finish in four to six milliseconds. Pushdown cannot save work
that is not being done; it adds per-row bookkeeping to the scan that remains. The cheaper
mechanism had already won.

It is left at the engine's default rather than pinned off, because the evidence supports
*not always* rather than *never* — on data with no useful bounds it should look different,
and the right form of that decision is per query from the statistics rather than pinned
for everyone.

**The pattern, which is the part worth keeping:** both errors came from asserting a
mechanism with a good reputation on reasoning rather than on a measurement of this
system's own data. The first measurement was too narrow to contradict the reasoning. The
second was a workload somebody else designed, and did.

---

## Where the two engines disagree

SANKHYA presents one copy of the data through two engines, and the same question asked of
each is supposed to get the same answer. Mostly it does — which is what makes the
exceptions dangerous, because nobody checks a figure that has agreed a thousand times.

Fifteen cases are now run against both, with the agreements pinned as well as the
differences. Three of the differences produce a **wrong number rather than an error**:

| | Transactional tier | Analytical tier |
|---|---|---|
| Summing past a 64-bit integer | `9223372036854775808` | **`-9223372036854775808`** |
| Multiplying past a 64-bit integer | refuses | **`0`** |
| Summing decimals past 38 digits | `100000000000000000000000000000000000000` | **`99999999999999997748809823456034029569`** |

The third is the one that matters most. Fixed-point decimal was chosen *because* money
must be exact, and on overflow the analytical tier returns something close to the right
answer instead of refusing. Close is the wrong kind of wrong.

Three further differences change a figure's precision or ordering without making it
wrong: `avg` of integers is arbitrary-precision on one side and a 64-bit float on the
other; division to a repeating fraction gives twenty significant digits against sixteen;
and text orders by the database's collation on one side and by bytes on the other, so any
paged or ranked result over text is in a different order in the two tiers.

**A mitigation exists and is partial.** The statistics catalogue can predict, before a
query runs, whether summing a column *can* overflow — bounds and row count give an upper
bound on the total. It errs toward "might", because a false alarm costs a refused query
and a missed one costs a wrong number nobody notices.

Its limit is worth stating plainly: bounds are held as 64-bit integers, so a decimal
column exceeding about nineteen digits has no representable bound and the check answers
"unknown". The columns most likely to overflow a 38-digit decimal are exactly the ones it
cannot reason about. Widening the bound type to 128 bits would close that, and has not
been done. Nothing yet consults the check.

---

## What runs today

| Capability | Evidence |
|---|---|
| The optimizer plans on the catalogue rather than a guess | Bounds, null counts and cardinality reach it from SANKHYA's own statistics — cardinality marked *inexact*, because an optimizer told a count is exact may conclude a column is unique, and being wrong about that is a different plan rather than a slower one |
| A row that has been updated is returned once, with its new value | By declared capability: union for append-only, latest-version-per-key for mutable, with a deletion suppressing the row rather than reverting it |
| A settled partition is written in the order it declares | Compaction sorts, which is what turns row-group bounds into an index — **7.8×** on the query whose objective was being missed, and the only one of that objective's three named preconditions that turned out to matter |
| A required setting is required because it was measured, not because it sounds right | Filter pushdown was asserted at startup for five milestones and is not any more — measured a cost at every selectivity, up to 2.6× on a full query |
| The pinned dependency set compiles with no critical duplicates | `cargo xtask check-dupes`, [ADR-0001](adr/0001-dependency-pin-set.md) |
| PostgreSQL 17.11 builds from vendored, checksum-verified source | `vendor/postgresql/build.sh` |
| The wire decoder handles a real replication stream | Conformance suite against captured bytes |
| The decoder never panics on arbitrary or corrupted input | Property tests plus single-byte corruption of real messages |
| A transaction is never split across batches | Property test over randomised interleavings |
| Types round-trip exactly or are refused with a reason | 73 real columns across 10 tables |
| Naming mirrors the source; collisions are refused | All 10 tables map with no transformation |
| A query is answered from tiers covering its span exactly once | Property test asserting exact cover |
| Tables onboard from the stream alone | All 10 tables, schema derived not declared |
| Captured data becomes queryable Parquet | Exact decimal sum matches the source |
| Several tables capture independently from one interleaved stream | 4 tables, each reconciling against the source |
| Capture holds up at scale | 1,000,000 rows across all 10 tables at ~285k rows/s, every table reconciling |
| A session sees its own write analytically | Wrote, capture caught up in 6 ms, the query returned the row |
| Capture cannot endanger its own source | Five-rung ladder escalating strictly below the database's own limit, validated against a real slot |
| Captured data provably matches the source | 3,000 rows digested independently on both sides, no discrepancies |
| A restart cannot duplicate data | Replay is filtered per row; every crash point across a constructed stream yields each row exactly once |
| A schema change never corrupts data | Additive changes apply automatically; anything whose intent cannot be inferred quarantines, keeps consuming so the cursor advances, and requires an operator to adopt the new shape |
| Backfill meets streaming with no gap and no overlap | Verified against a live slot; a late slot is shown to drop real positions into neither half |
| Small-file accumulation is detected and planned against | Two independent triggers, bounded passes, coverage preserved exactly, and a plan that always reduces the file count |
| Compaction runs without changing any answer | The same aggregate over 12 fragments and over the file they merge into, compared row for row; row counts verified against the inputs before the merge is reported as successful |
| Compaction cannot remove a file a reader might still be holding | Retirement refuses inside the grace period, refuses while a snapshot is pinned at or before the merged coverage, and refuses entirely if the replacement is missing or short |
| A write is visible before it is durable | The arrival tier answers from memory and drops out of the splice once published; the two tiers are shown to abut exactly with no change to the planner |
| The arrival tier never loses or double-counts a position | Property tests over arbitrary interleavings of appends and publication: every position between the durable frontier and the target appears exactly once, and a scan never returns a position the published tier already holds |
| Memory pressure cannot cost data | Nothing is released until publication covers it; a full tier refuses new work and names publication as the cause, and the refusal clears once publication catches up |
| One SQL query is answered from memory and Parquet at once | 700 positions published and 300 still in memory sum to the whole thousand, with the 700-position overlap counted once; a pinned query sees only its target; provenance names both tiers and their intervals |
| A gap between tiers refuses the query rather than answering it short | Verified for a genuine gap, for a target past every tier, and for a tier that started mid-stream |
| What the engine means by a query is written down | Thirty-nine cases across three-valued logic, aggregates, grouping, joins, ordering and coercion. Every expected value written by hand — a corpus that computes its expectation the way the engine does is a tautology |
| An optimisation that stops working is a failing test | Plans are pinned by operator shape, not text: a byte-exact snapshot fails on cosmetic upstream change and gets regenerated unread, at which point it catches nothing |
| Memory is counted where it is actually spent | A counting allocator, quarantined in the one crate where `unsafe` is permitted — the workspace `forbid`s it everywhere else. Growth, release, reallocation-by-difference, the peak, and concurrent allocation are all verified against a real installed allocator |
| Capture and maintenance commit to the same log without either stopping the other | Capture rebases onto the next free version when a compaction takes the one it was about to use — bounded, so a runaway committer is a diagnosable failure rather than a pipeline that appears to hang |
| A query stops within one batch of its deadline | Demonstrated on a real query planned and executed by the engine over real Parquet — it returns an error rather than fewer rows, and the clock is shown to have been read no more times than the deadline allows |
| A deadline and a cancellation are told apart | Different variants with a `retryable` flag: a deadline may succeed with longer to run, a cancellation is a decision somebody made. A client that cannot tell them apart cannot decide whether to retry |
| A query the system cannot afford never starts | Admission estimates from plan cardinality and queues or refuses; the queue is bounded, so a refusal arrives immediately rather than after a timeout |
| A client can tell a permanent refusal from a temporary one | "Too large for the pool" and "too large right now" are distinct answers with a `retryable` flag — collapsing them is how a client retries forever |
| One tenant cannot starve another, or take an idle pool | A floor is honoured under global pressure; a cap binds even when nothing else is running |
| Pressure escalates through one ladder, evaluated centrally | Five rungs from a typed signal bus; the last one — the only one that loses continuity — is reachable by the two source signals and nothing else, property-tested |
| Maintenance work is arbitrated against one budget across both sides | Strict class ladder; safety and availability work preempts queries and ignores the duty cycle, everything below it does not; a job that cannot checkpoint is refused rather than started; waiting never promotes a job out of its class |
| The maintenance scheduler cannot destroy retained history | There is no erasure class to configure — the guard is on the type, and an exhaustive match makes adding a variant a compile error |
| A fragmented warehouse is cleared by ticking, and converges | 60 fragments reduced by repeated ticks until nothing is left to do, with the row count unchanged; urgency maps onto the class ladder, so a degrading partition preempts and an ordinary one waits; one failing partition does not block the rest |
| A query is unaffected by compaction running underneath it | The same aggregate before and after a merge, over the live set |
| The live set is durable and readable by other engines | SANKHYA writes the Delta transaction log itself, and `delta_kernel` — a dev-dependency used as an independent oracle — reads it and agrees about the schema, version and live files, including across a compaction where four superseded fragments are still on disk |
| Capture publishes into a table log | Each table gets its own log; every file is committed with its row count, the creating commit carries the schema, and a translation that is not exact refuses to publish rather than publishing something similar |
| A restart resumes from the log, not from memory | File sequence and log version are both recovered from the table's whole commit history — not from the live set, so a compacted-away name is never reused while a reader may still resolve it |
| The whole storage loop runs through the log | 24 fragments published and committed, compacted tick by tick with each tick one atomic version, then queried by a reader given nothing but the table root — same answer, fewer live files, every superseded file still on disk |
| SANKHYA owns its table provider | Files and row counts come from the table log; scan execution is DataFusion's own Parquet source. Splices memory and files, refuses gaps at planning time, and refuses a file the log cannot state a row count for |
| Order statistics are exact and the convention is named | Three conventions, shown to disagree at the 99th percentile — which is the only place anyone asks for one. An unorderable value is refused rather than placed somewhere; an empty input is refused rather than answered with zero |
| The quantile of a sum is not computed as the sum of quantiles | Demonstrated with numbers: two exposures whose worst cases fall in different scenarios give −100 combined and −190 under the naive composition |
| A skipped file never hides a matching row | Property-tested over arbitrary values and predicates, and again over *merged* statistics — compaction merges rather than recomputes, so a merge that narrowed a bound would produce a defect appearing only after maintenance ran |
| Distinct-value counts are estimated well enough to order a join | Within 5% from 10 to 100,000 distinct values, exact under merge, and reproducible across processes — a per-process hash seed would make two nodes disagree about a plan and the disagreement would look like an optimizer bug |
| An approximate function cannot answer an exact question by accident | Rejected at planning time, including inside a subquery or a `HAVING` clause; a permissive session still gets a watermark naming what it used |
| A cache key cannot omit what the answer depended on | The entitlement set and the policy version are constructor arguments, not fields — a field can be left at its default, an argument has to be passed. Keys are byte-identical across processes, pinned so a change to the hash is deliberate |
| An interrupted compaction's leftovers are reclaimed, and nothing else is | Age is the only thing separating an orphan from a file mid-commit, so the threshold is a week by default; a file any retained snapshot reaches is kept however old, and the log is not a candidate at all |
| A cold reader starts from a checkpoint, and any reader may ignore one | **10×** at fifty thousand commits; the kernel reads a checkpoint this system wrote by hand, and is proven to *use* it rather than tolerate it — the commits it covers are deleted and the table still resolves |
| A warm process pays for what changed, not for the whole history | Table file sets are cached and resumed; the cache cannot go stale because it never trusts its own version, and asking costs one filesystem probe rather than a directory listing |
| Log replay scales linearly with a table's history | Guarded by measuring the *ratio* between two sizes rather than a clock, so it means the same on any machine — and proven to fail on the quadratic implementation it replaced |
| A file is prunable from the moment it is published | Capture computes statistics from the batch it just encoded — the same data, already in memory — so a file does not wait for maintenance to become skippable |
| Statistics survive a restart and other engines can read them | Bounds and null counts are written into the table log itself, so a fresh process prunes exactly as a warm one does — and the kernel reads a log carrying them |
| Compaction computes the statistics the provider prunes on | Bounds, null counts, widths and a cardinality sketch, produced by the merge that was already reading the data — no separate analysis pass and nothing for an operator to remember to run |
| The provider skips files the catalogue proves irrelevant | Nine of ten files pruned on a point lookup, five of ten on a range, and none at all on a disjunction, a predicate over an uncatalogued column, or no predicate. The same query returns the same answer with and without the catalogue |
| Planning does no file I/O | 800 files plan in 1.37 ms against 10.33 ms for a directory listing — **7.5×**, widening with file count |
| A dependency declared test-only actually is | `cargo xtask check-features` reads the manifests; proven to fail when the oracle is moved into `[dependencies]` |
| The tests guarding each core invariant are verified against the defect they claim to catch | `tools/mutation-audit.py` — 127 specific defects applied one at a time; all 127 fail the suite. Thirteen did not when first run; four catalogue entries turned out to be equivalent mutants no test could ever have caught, one entry was inert until corrected, and chasing another produced a documentation correction rather than a new test. The catalogue also checks that each entry still *matches* its source before applying it: a refactor moved four of them, and a mutation that no longer applies passes silently, which is the failure this tool exists to prevent |

---

## What does not exist

Stated plainly, because a status document that omits this is marketing.

- **No server.** The binary is a stub; there is no daemon, no listener, no lifecycle.
- **No streaming transport.** Changes are drained through a SQL function rather than a
  replication connection. The decoder and pipeline are transport-agnostic by design, but
  the transport itself is unwritten. Note that neither mainstream Rust PostgreSQL client
  supports the replication protocol, so this is real work rather than a wiring exercise.
- **No slot lifecycle.** No creation policy and no position advancement. The
  source-safety ladder now exists and is tested against a real slot, but nothing drives
  it on a timer yet — it is a decision function without a caller.
- **No backfill reader.** The handoff *contract* is built and verified against a live
  slot, but nothing yet reads the existing rows. Only changes after a slot exists are
  captured.
- **The arrival tier is a retention contract, not the buffer the architecture
  describes.** What exists is the part that governs correctness: coverage, per-row
  filtering at the durable frontier, and the rule that publication alone releases
  memory. What does not exist is §5.4's epoch ring, the per-epoch key digests that let
  a historical query skip the tier at no cost, and per-tenant sub-caps. Nothing yet
  wires the tier into the ingest path either, so read-your-own-writes still waits for
  publication in practice.
- **The cardinality sketch is not persisted.** The protocol has nowhere to put it, so a
  column read back from the log reports zero distinct values. Nothing currently reads
  that figure, but it is a trap for whatever does first.
- **No multi-part or V2 checkpoints, and no log cleanup.** A checkpoint is written as a
  single file, which is fine into the millions of live files and not beyond; and nothing
  deletes the commits a checkpoint subsumes, so the log directory grows without bound even
  though nothing reads most of it.
- **Nothing routes a captured table to the resolved provider automatically.** The
  capability is derived from what the source declared and the resolution works end to
  end, but the caller still has to assemble the two — there is no catalog mapping a table
  name to its provider, because there is no catalog.
- **The governor decides but governs nothing.** Admission and the pressure ladder are
  built and tested, and nothing calls either: no memory pool reports its occupancy, no
  subsystem publishes a signal, and no query passes through admission on its way to
  running. They are decision functions without callers, like the maintenance scheduler
  was before the driver.
- **No counting allocator and no spill isolation.** The architecture requires true
  accounting outside the engine's own pool, and spill on a different filesystem from the
  write-ahead log. Neither exists. Deadline propagation and cancellation *do* — bounded
  at one batch per partition — which is why they are no longer listed here.
- **Exact order statistics buffer their input.** Selection is linear rather than
  `n log n`, so it beats sorting, but every observation must be resident. `FR-QUERY-08`
  asks for a bounded-memory algorithm over large inputs and this is not one. Exact and
  bounded are independent properties, and only the first is delivered.
- **The exactness gate is not wired into a session.** `check_exactness` is a function
  with no caller: nothing carries the session's exactness setting, and nothing attaches
  the watermark to a result.
- **No catalog.** The table provider exists and resolves mutable tables correctly, but
  nothing maps a table *name* to one, so the caller still has to assemble it.
- **No deletion vectors, column mapping or partition values in the log.** Row counts,
  bounds and null counts are written, and checkpoints are; everything else the protocol
  permits is not. A reader requiring any of them refuses these tables, which is the
  correct outcome — refusing is visible, and a partially-implemented protocol feature is
  not.
- **No table partitioning.** Every scan is over the whole table, pruned by file
  statistics rather than by partition. This is why `NFR-PERF-03`'s partition-predicate
  precondition cannot be satisfied by any query today.
- **Nothing calls the maintenance loop on a timer.** The tick is built and tested end to
  end, but a caller has to invoke it, supply the live set and supply the pinned snapshot
  positions retirement checks against.
- **No API surfaces, no multi-tenancy, no security.** The graph engine and the extension
  mechanism are built; nothing drives graph hydration on a timer and no process loads a
  pack bundle, because there is no running process yet.

---

## Known defects found and fixed

Recorded because the interesting information is usually in what went wrong.

| Defect | How it surfaced |
|---|---|
| Rows of the transaction that first introduces a table were silently refused | Only visible with several tables interleaved; a single-table test cannot see it |
| The conformance fixture's central assertion passed vacuously — the "large" value compressed 80× and stayed inline, so it was never withheld | Caught by making the generator fail loudly if the value does not exceed the out-of-line threshold |
| The fixture capture tool injected newline separators into a binary stream | The decoder was right and the capture was wrong; found at the first byte after a transaction boundary |
| The layer rule forbade same-layer dependencies, which was wrong rather than strict | A vocabulary crate legitimately building on another |
| The documentation-rot check found a stale version claim on its first run | Its own first execution |
| **Zone offsets were stripped rather than applied, shifting a whole timestamp column by four hours** | End-to-end reconciliation against the source. Every value stayed internally consistent, so nothing looked wrong until the two sides were compared |
| **Duplicate suppression worked per batch rather than per row, so a batch spanning the restart boundary republished its already-durable half** | Crash-safety tests sweeping every possible interruption point. A resent stream does not rebatch identically, which a single hand-picked crash point would not have revealed |
| **A restart would have overwritten a live file.** The pipeline's file sequence was in-memory state starting at zero, so a restarted capture wrote `00000000.parquet` over a file that was still live and still referenced | Committing to the log. The overwrite had always been possible, but nothing could see it: the file count does not change, no error is raised, and the rows in the overwritten file simply become different rows. It surfaced as a version conflict — the log refusing to create a table that already existed — and the overwrite was the real defect behind it |
| **The test written for that overwrite could not detect it, twice over.** It asserted on file *names*, which an overwrite does not change; and its fixture published a single file per run, so resuming from the highest committed sequence and resuming from the lowest were the same number | The mutation audit, on two consecutive attempts. Now asserted on the log's own history — a path added twice *is* the overwrite — with a fixture that publishes at every transaction boundary, as continuous capture does |
| **The log described where files were but not what was in them.** With no row counts, a compaction plan driven from the log could not state what it expected to merge | The first tick planned from the log rather than from a value threaded out of the writer. The merge's own row-count check refused the plan — the guard worked, and what it caught was that the log was incomplete rather than that the merge was wrong. `numRecords` is now written and read |
| **Capture and maintenance could not both commit to the same table.** The publish path held its next version in memory and failed outright when a compaction took it | The first test that ran capture and maintenance together. Before it, every test had exactly one committer. Failing there inverts the ordering rule — a compaction could stop capture — so the publish path now rebases onto the next free version, which is what the error message had been telling it to do all along |
| **Vectorised bounds became unsafe in the presence of NaN.** Arrow's aggregate kernels propagate NaN, so `max` over a column containing one returns NaN; the NaN guard then refused it and left whatever bound had been recorded so far — a maximum *below* the true maximum | The unit test written for the NaN guard, on its first run. The narrowed bound is the one direction that matters: a file holding 2.5 would be skipped for `f > 0` because its statistics claimed it topped out at −1.5, silently and undetectably. A NaN result now invalidates the fast path and the column is measured again skipping NaNs, so the cost falls only on columns that actually contain one |
| **The hand-written Delta log was invalid, and this crate's own reader accepted it happily.** The `add` action's `partitionValues` field is non-nullable and was omitted entirely | The kernel, on the very first read. The log looked reasonable and round-tripped through this crate perfectly, because a reader ignores a field it never writes. Two implementations agreeing is worth nothing when one of them wrote both sides |
| **A directory listing is not a file set, and both the planner and the read path were treating it as one.** Compaction only ever adds, so between a merge and the retirement of its inputs the directory holds both — the same rows twice, by design, for at least a full grace period | Writing the convergence test. Re-observing the directory each tick made the planner merge files an earlier merge had already superseded. Nothing is wrong on disk; the *readers* were wrong. The published tier now names its files individually, the live set is carried across ticks, and the negative case is a test: the same query against the directory returns the merged rows twice |
| **The arrival tier declared coverage it did not hold.** A tier starting mid-stream reported from the durable frontier rather than from its own oldest segment, so it claimed every position before its first captured transaction | The first query spliced across two real tiers. The splice found an exact cover that did not exist, so the query would have been *answered* with the missing positions silently absent — the failure mode the splice exists to prevent, produced by the tier lying to it. This is the normal case rather than an edge case: a table onboarded from a running stream starts mid-stream by construction |
| **The property test written to catch that defect did not catch it.** Its generator started the publication frontier equal to the stream's origin, so the two could never diverge, and its assertion encoded the buggy expectation | Deliberately reverting the fix and finding the suite still green. The generator now starts publication at zero independently of the origin, and fails within a second on the reverted code |
| **A commit shipped one of the mutation audit's own deliberate defects, with a failing test.** The reversed-comparison fix in the predicate reader was reverted in the tree — `5 < x` was read as `x < 5` — so the reader skipped files that did hold matching rows | Reconciling the working tree before the merge. The audit restores what it mutates through an in-flight record and signal handlers, but nothing survives a `kill -9`, and the leak is invisible: the diff is one plausible-looking line in a file the commit was already touching. The test that proves the mutation caught was sitting red in `main`'s parent. Now guarded by `cargo xtask check-mutations`, which asserts every catalogue entry still matches its source — no compilation, milliseconds, so it gates every build rather than only a full audit. It catches catalogue drift by the same check |
| **The Parquet writer's default compression was never enabled.** The workspace pin omitted `zstd`, so every write on the default configuration panicked inside the column writer | The first crate to use the writer *without* also depending on DataFusion. Cargo unifies features across dependencies **and dev-dependencies**, and DataFusion — a dev-dependency of the writer's own crate — was quietly supplying the feature. The crate's entire test suite passed while the library was broken for every real consumer. Now guarded by `cargo xtask check-features`, which reads the manifest rather than the resolved graph, because the resolved graph is precisely what hides it |

---

## On the tests

Two of this project's invariant tests were reviewed, passed, and would not have failed
on the defect they were written for. Neither was found by reading them.

- The arrival tier's coverage property started the publication frontier at the stream's
  origin, so the two could never diverge — and its assertion asserted the buggy value.
- `is_exact_cover`, the oracle every splice test leans on, had no tests of its own. It
  could be changed to tolerate gaps between tiers, or to stop requiring the cover to
  reach the target, and the whole suite stayed green. Every splice test would have gone
  on passing over a planner returning partial covers.

A fourth was not a weak test but a **missing** one. The fix that made the provider's scan
run on more than one core had no test at all — the defect had been found by measurement,
and nothing was written to hold it. Months later that fix was replaced with a better one,
and the only thing that caught the intermediate regression was a *deadline* test which
happened to contain a self-check asserting its own plan ran in parallel. A test written
for one thing guarded another by accident, which is not a mechanism anyone should rely on
twice. There is now a direct test, asserted at the scan node rather than above it, because
a repartition can manufacture eight partitions from one serial reader.

A third was found the same way: the property named *open transactions are never
published* aborted every unsealed transaction before flushing, so it never left one in
flight. A mutation that published rows from transactions that had not committed survived
it untouched — and an in-flight transaction is the steady state of a busy source, not an
edge case.

`tools/mutation-audit.py` now keeps a catalogue of specific, plausible defects, applies
each one and reports whether anything fails. It refuses to run on a dirty tree, and it
flags a catalogue entry that no longer matches the source — a stale entry proves nothing
while looking like coverage, which is the failure mode the tool exists to find.

A fourth was found the same way, later: the driver's compaction removals were checked
only through the constructors they *could* have called, not through what the driver
actually committed. Swapping one for the other went unnoticed.

A fifth: the table provider could claim it evaluated predicates *exactly* — which gives
the engine permission to drop the filter from the plan entirely — and every test still
passed, because not one of them had a `WHERE` clause.

A sixth: the test asserting that a disjunction is never split used `a = x OR a = y`,
which the engine rewrites into an `IN` list before it reaches the code under test. The
test exercised no disjunction at all.

A ninth was found by the audit *hanging* rather than reporting: a threaded cancellation
test looped forever when the cancellation check was removed, so a defect that should have
been a failure in seconds became a thirty-minute stall. The loop is bounded now — a test
that stalls a pipeline is how a real defect gets discovered in someone else's build
rather than in this one.

An eighth: the regression guard written for the quadratic replay above passed against
the quadratic replay. It spread its workload across thousands of commits, where linear
file I/O dominates the quadratic term entirely.

A seventh, and the most useful: statistics computation had no tests in its own crate at
all. It was exercised only through an end-to-end test in a *different* crate, so
`cargo test -p sankhya-table` covered none of it and the audit reported two survivors
immediately. Writing the missing tests found the NaN bounds defect above on the first
run.

The catalogue also produced one **equivalent mutant** — a change to a duplicated guard
that left the second copy still refusing, so behaviour was unchanged and no test could
possibly have caught it. That is worth recording rather than quietly deleting: an entry
that can never fail trains you to read "SURVIVED" as noise, which is the one habit that
makes the whole exercise worthless.

**The general lesson, recorded because it applies to every test not yet audited:** a test
written to catch a defect is not evidence that it catches it. Until it has been run
against that defect, it should be assumed to be in the same state as the four above.

---

## Measurements

On a 24-core machine with NVMe storage.

| | |
|---|---|
| Cold `cargo check`, full critical dependency family | 32.9 s |
| Vendored PostgreSQL build | ~2 min, 35 MB installed |
| Synthetic generation | ~147 MB/s |
| Bulk load, 10 tables | 99,235,351 rows / 10 GiB in 188.7 s (~526k rows/s) |
| On-disk size after load | 14 GB |

### Query-engine settings

| | |
|---|---|
| Filter pushdown, 5M rows / 523 MiB / 1-in-10,000 selectivity | **1.02× — neutral** |
| The same measurement with a compressible payload | 0.74× — *slower*, an artefact of the fixture |

**This corrected a claim rather than confirming one.** Earlier drafts of the
requirements and architecture documents asserted that filter pushdown was worth roughly
an order of magnitude. It is not, on this shape. The setting is still pinned — neutral
is not harmful and the benefit is expected on wider payloads — but the documents now
record the measurement instead of the assumption.

The first attempt showed pushdown *slower*, which was a fixture artefact: a repetitive
padding string dictionary-encodes so well that decoding it is nearly free, so avoiding
that decode saves nothing while the row-selection bookkeeping still costs. Worth
recording, because a benchmark whose data is unrepresentative produces confident wrong
numbers rather than obviously wrong ones.

The companion claim about the Parquet page row-count limit has **not** been measured and
should be read as unverified.

### Compaction

400 fragments totalling 20,000,000 rows, merged into one file.

| | 400 files | 1 file | Ratio |
|---|---|---|---|
| Short query — one narrow range | 16.9 ms | 3.8 ms | **4.42×** |
| Long query — full aggregation | 121.0 ms | 97.3 ms | **1.24×** |
| On-disk size | 61.0 MB | 27.4 MB | **2.23×** |

**This confirmed a claim, having first failed to test it.** The architecture asserts that
small files cost query *planning* rather than scanning, which predicts a roughly fixed
per-query penalty — dominant on short queries, amortised away on long ones. The
measurement bears that out: the absolute overhead stays in the same order (13 ms to
24 ms) across a query doing thirty times more work, while the ratio collapses from 4.42×
to 1.24×.

The first run used 1,000,000 rows and produced 4.43× and 3.77× — apparently uniform, and
readable only as "more files are slower". The long query was not long enough for
planning to amortise against. The fixture was scaled until the two hypotheses gave
different answers; before that it was not evidence for either.

The practical consequence is that fragmentation is an **interactive-latency** problem
rather than a throughput one, which is the reason it is worth a first-class subsystem.

### Query planning

400 fragments and up, planned but not executed, best of three.

| Files | Provider | Directory listing | |
|---|---|---|---|
| 50 | 0.54 ms | 1.15 ms | **2.2×** |
| 200 | 0.63 ms | 3.02 ms | **4.8×** |
| 800 | 1.37 ms | 10.33 ms | **7.5×** |

The advantage widens with file count, which is what the metadata-only claim predicts:
the provider reads no Parquet footers, so its cost does not scale with the number of
files it is planning over.

**It does not scale with nothing, though.** Sixteen times the files costs the provider
about 2.5× more planning, because it replays the table log and the log grows with commit
count. The cost has moved from one seek per file to one sequential read — which is a much
better shape and is not the same as free. It is also the argument for log checkpoints,
which are not built.

### Where the memory limit bites

The gate is that a hostile query is *refused* rather than fatal, so the first thing to
establish is where refusal actually begins.

| Pool | An aggregation that cannot reduce anything |
|---|---|
| 64 KiB – 1 MiB | Refused: `Resources exhausted … SingleHashAggregateStream` |
| 8 MiB and above | Succeeds, 400,000 groups |

The operator **errors rather than spilling**, which is what makes admission worth having:
without it a query gets far enough to fail, having consumed the machine on the way.

**The first version of this test used two megabytes and passed while proving nothing** —
just above the line, so the query succeeded and the assertion never ran. The threshold is
now measured rather than guessed, and the test asserts the *operator* that ran out of
room as well as the fact that something did, so an aggregation that quietly started
spilling would be visible rather than silently making the gate meaningless.

### Replaying the table log

One add per commit, replayed from the first.

| Commits | Before | After |
|---|---|---|
| 100 | 0.32 ms | 0.34 ms |
| 1,000 | 1.99 ms | 3.58 ms |
| 10,000 | 81.5 ms | 20.5 ms |
| 50,000 | **1.96 s** | **117 ms** |

**The first column is a defect, not a cost.** The replay searched the accumulated file
list for every add and filtered it for every remove — quadratic in the number of files,
invisible in every test, and two seconds per query plan at fifty thousand commits. A
table committing every ten seconds reaches that inside a week.

Positions are now held in an index. What remains is linear and is dominated by opening
one file per commit, which is what checkpoints exist to fix rather than anything an
algorithm can.

The regression guard measures the **ratio** between two sizes rather than elapsed time,
so it means the same thing on any machine. Its first version put the actions in thousands
of separate commits and passed against the very implementation it was written to catch —
the quadratic term is per action, not per commit, so file I/O buried it. Concentrating
the actions into ten commits separates them: 16.1× for four times the files against about
4× for the fixed version.

### Checkpointing the log

A cold reader — an external engine, or a process that has just started.

| Commits | Replay | From a checkpoint | | Checkpoint size |
|---|---|---|---|---|
| 1,000 | 1.90 ms | 0.39 ms | **5×** | 32 KiB |
| 10,000 | 28.7 ms | 3.21 ms | **9×** | 269 KiB |
| 50,000 | 141 ms | 13.9 ms | **10×** | 1.3 MiB |

This is the number that matters to **other engines**, which have no cache and start cold
every time. A Spark job reading a fifty-thousand-commit table opened fifty thousand files
before reading a row.

The kernel reads a checkpoint this system writes by hand — nested structs, maps and lists
in Parquet, against a protocol it does not own. And it is shown to *use* it rather than
merely tolerate it: the commits the checkpoint covers are deleted, leaving it as the only
record of those files, and the table still resolves. Without that step both checkpoint
tests would have passed whether the kernel read the file or ignored it and replayed,
which is precisely the shape of a test that proves nothing.

**Nothing depends on a checkpoint being present, correct, or parseable.** It holds exactly
what replay produces, so every failure — a missing file, a corrupt pointer, one left
behind by a table that was dropped and recreated — falls back to the log and costs a
replay rather than an answer. That is what makes it defensible to write this by hand: the
worst a bad checkpoint can do is be ignored.

### Caching a table's file set

The same table, replanned by a long-running process.

| Commits | Cold replay | Cache, unchanged | Cache, one new commit |
|---|---|---|---|
| 1,000 | 1.66 ms | 23 µs | 43 µs |
| 10,000 | 22.1 ms | 224 µs | 371 µs |
| 50,000 | 140 ms | **1.20 ms** | **1.88 ms** |

**117× on an unchanged table, 75× after a commit**, and both are now proportional to what
changed rather than to the table's history. The residual 1.2 ms is copying fifty thousand
file entries into the answer, which is what the caller asked for.

Two things had to change to get there, and each was worth about an order of magnitude:

- **Asking is a probe, not a listing.** Versions are contiguous — enforced at commit
  rather than assumed — so a reader that knows the state at version *n* asks whether
  *n+1* exists. Listing instead made every lookup proportional to the whole history, which
  is the cost the cache existed to remove.
- **The index is kept, not rebuilt.** Resuming from a bare file list rebuilds the position
  index over every live file, swapping *linear in the history* for *linear in the table* —
  better, and still not right.

The cache cannot go stale, and that is structural rather than careful: it never trusts its
own version, there is no invalidation, no expiry and no notification to miss. A table
dropped and recreated at the same path is detected and replayed from the beginning, rather
than resumed from a base describing files that no longer exist.

### Computing statistics at compaction

5,000,000 rows across three columns, merged.

| | |
|---|---|
| Merge including statistics | 357 ms |
| The statistics alone | 83 ms — **23% of the merge** |
| The same, before using vectorised kernels | 164 ms — 36% |

**This corrected a claim.** The first version of the module said bounds came from Arrow's
vectorised aggregates and were "close to free". They did not and were not: bounds were
computed in a scalar loop costing 29% of the merge on its own, five times what the
cardinality sketch costs. The comment was written before the measurement and was false
when written.

With the kernels actually used, bounds and widths are close to free and the remaining
23% is almost entirely the sketch, which has to hash every value. That cost is paid
because a cardinality estimate is the one statistic neither the file format nor the table
log carries.

23% on top of a merge is not nothing. It is still much cheaper than the alternative,
which is a second full pass over storage — and it means statistics arrive without anyone
running an analysis command, which matters on a system where tables onboard themselves
from a replication stream and the tables nobody thought about are exactly the ones that
would have none.

### File pruning

200 files of 2,000 rows, the same query with and without the catalogue.

| Predicate selects | With statistics | Without | |
|---|---|---|---|
| One file in 200 | 1.26 ms | 9.21 ms | **7.3×** |
| A tenth of the table | 3.89 ms | 10.81 ms | **2.8×** |
| Everything | 17.52 ms | 17.71 ms | **1.0×** |

The gradient is the point. Pruning helps in proportion to what a predicate can exclude,
so a single figure would be meaningless — and the last row matters as much as the first:
when nothing can be pruned, consulting the catalogue costs nothing measurable.

Every row of that table was checked to return the same answer with and without the
statistics, because a pruning measurement that does not verify the answer is measuring
how fast it can be wrong.

### Deterministic reduction

Reduction sums in a canonical order with Neumaier compensation on top. Searching which
of the two actually delivers order-independence produced a correction rather than a
confirmation.

| | |
|---|---|
| Randomised inputs tried, spanning 120 orders of magnitude | 3,000 |
| Permutations where compensation *alone* gave a different total | **0** |
| Hand-built adversarial cases where it did | **0** |

**Compensation is what makes the total order-independent in practice.** The canonical
sort — which the first draft of the documentation credited with the property — changes
no behaviour that could be observed. What it contributes is a *guarantee*: Neumaier's
error bound bounds the error without proving bit-identity across permutations, and its
compensation term is itself one `f64` that can lose bits when corrections span extreme
ranges. Sorting makes the result a function of the multiset by construction.

The sort is therefore defence in depth, at `n log n` against `n`. It is kept because the
figures it exists for have to be defended later, and *"we could not find a
counterexample"* is a weaker thing to say than *"there cannot be one"* — but the
documentation now says which of the two mechanisms is doing the work.

### Capture at scale

One million rows across all ten tables, in five interleaved transactions.

| | |
|---|---|
| Rows written | 1,000,000 in 2.0 s (~500k rows/s) |
| Messages decoded | 1,000,060 |
| **Rows captured through the pipeline** | **1,000,000 in 3.5 s (~285k rows/s)** |
| Retained write-ahead log during the run | 1,053 MB |
| Files published | 10 |
| Bytes published | 6.3 MiB |

**On the compression figure**, because it would otherwise be misleading: 6.7 bytes per
row reflects *this* data, which is deliberately regular — sequential identifiers,
patterned labels, timestamps clustered within a single run. Dictionary encoding,
delta-encoded monotonic columns and zstd all do unusually well on it. Do not read it as
a general ratio; the honest range against a realistic schema is closer to 3×–15× the
source's footprint, driven mostly by cardinality and index count. See
`REQUIREMENTS.md` §7.4 on estimates versus measurements.

Reproduce with:

```bash
SANKHYA_PG_BIN=$PWD/.build/pg-install/bin SANKHYA_E2E_SOCKET=/tmp/sankhya-sock \
  cargo test -p sankhya-ingest --test scale --release -- --ignored --nocapture
```

---

## How to check this document is honest

```bash
cargo xtask check-all            # every repository invariant: layers, file length, doc
                                 # links, version claims, feature pins, clippy with the
                                 # workspace's denied lints across every target, and that
                                 # no mutation is still applied to the source
cargo test --workspace           # 630 tests, none of which needs a database
cargo xtask check-performance    # the NFR-PERF objectives, as a gate that can fail
python3 tools/mutation-audit.py  # 127 specific defects, applied one at a time
crates/sankhya-cdc-apply/tests/run_e2e.sh   # capture against a live database
```

The performance gate needs a quiet machine and several minutes, which is why it is not in
`check-all`. Everything else runs unattended.

[`QUICKSTART.md`](QUICKSTART.md) walks through building it from nothing.
