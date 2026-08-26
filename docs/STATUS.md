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
| **M3** Query engine and storage performance | 28–34 ew | **Most of the work, none of the exit gates.** Built: the table provider, statistics (computed, persisted, used for pruning), the arrival tier, compaction end to end, the maintenance scheduler, the governor, exact order statistics, the metadata cache and log checkpoints. **Not met:** no benchmark numbers against a named public suite, no cancellation or deadlines, no counting allocator or spill isolation, no SQL-semantics corpus, no cross-engine difference list, and compaction is not demonstrated under *continuous* ingest. See below |
| **M4**–**M8** | — | Not started |

---

## What M3 still owes

Recorded separately from the list below because the work items are substantially built
and it would be easy to read that as the milestone being finished. It is not, and these
are its own stated exit criteria:

| Gate | State |
|---|---|
| Performance objectives met in the pipeline, against named public-suite queries | **Numbers exist; the objectives are not met.** Four TPC-H queries at scale factor 1, at the concurrency the requirements name — **two of the four exceed the closest stated objective**, and the preconditions those objectives assume are not built. See below |
| Cancellation demonstrated within its bound, including inside user code | **Half.** Deadlines and cancellation exist and are demonstrated inside a real query, bounded at one batch. There is no sandboxed user code to propagate into yet |
| A hostile aggregation under a constrained memory limit is rejected rather than terminating the process | **Met.** An aggregation that cannot reduce anything is refused under a one-megabyte pool, by name — and the process runs the same query to completion afterwards. Repeated five times, so a refusal that leaked its reservation would show up. Spill isolation onto a separate filesystem is still not built |
| Plan snapshots stable; the SQL-semantics corpus green | **Met.** Thirty-nine semantics cases pinned by hand from the standard's rules; plan *shapes* pinned rather than plan text, plus assertions on the optimisations that fail silently — projection pushdown, two-phase aggregation, the read-position filter inside the plan, and file pruning naming which file survives rather than counting |
| Cross-engine semantic differences enumerated in a tested list | **Met, and it found three ways the analytical tier returns a wrong number.** Fifteen cases run against both engines; agreements are pinned too, so a *new* divergence fails the test rather than being discovered later. See below |
| Compaction holds file counts within policy under continuous ingest | **Met.** Forty ticks of capture with maintenance sweeping every third one; the live count never passes the urgent threshold and no row is lost. The fixture asserts it actually created a backlog, or it would prove nothing |

Smaller items inside the work breakdown that are also absent: delete resolution into
plan-time row selections, file ordering by statistics for early termination, bloom
filters, per-column encoding chosen from measured statistics, quantile sketches, the
footer and byte-range and decoded-batch caches, and leader election.

The *result* cache does not exist either — but its **key** does, because what makes a
result-cache key correct is a security property and the right time to fix it is before
anything is caching. A key that omits the entitlement set does not return a stale answer;
it returns someone else's, correctly and quickly.

---

## TPC-H, and two objectives that are not met

Every other measurement in this document isolates one mechanism. These are queries
somebody else wrote, so the numbers can be compared to something other than themselves.

Data generated at scale factor 1 — 8,661,245 rows, 261 MiB of Parquet — written through
this system's own write path. Best of three for single-query latency; five rounds at the
stated concurrency for the rest, after two warm-up executions.

| | Clients | Median | p95 | Nearest objective | |
|---|---|---|---|---|---|
| **Q1** pricing summary — full scan, eight aggregates | 4 | 518 ms | **577 ms** | `NFR-PERF-04` wide scan, < 3 s | **inside** |
| **Q6** forecasting revenue — narrow selective filter | 8 | 332 ms | **360 ms** | `NFR-PERF-02` selective lookup, < 250 ms | **over** — but see clustering below, which brings the same query to 234 ms |
| **Q3** shipping priority — three-way join, top-N | 8 | 757 ms | **776 ms** | `NFR-PERF-03` pivot, < 1 s | **inside** |
| **Q5** local supplier volume — six-way join | 8 | 1161 ms | **1187 ms** | `NFR-PERF-03` pivot, < 1 s | **over** |

**Two of the four are over.** Both deserve qualification, and neither qualification makes
them met:

- Q6 is a range scan over roughly a seventh of the table, not the "selective needle
  lookup" `NFR-PERF-02` describes — and that objective explicitly assumes bloom filters
  and late materialization, neither of which is built.
- Q5 is a six-way join, which is a harder shape than the "multi-dimensional pivot"
  `NFR-PERF-03` describes.

So the mapping from these queries to those objectives is mine rather than the
requirements', and it is approximate. What can be said without qualification is that
there are now numbers against recognisable queries at a stated concurrency, and that two
of them are the wrong side of the closest thing to a target this project has written down.

**The first run of this measured the wrong thing**, and finding out why produced the
correction below. It used a bare engine rather than this system's configured session, so
the numbers described the engine's defaults. Running it properly made three of four
queries *slower* — Q6 by 2.6× — which is how a setting that had been asserted as required
since M3 began turned out to be a cost.

**What this is not:** an audited TPC-H result. Scale factor 1 on a development machine,
single node, four of twenty-two queries, no substitution rules, no refresh streams. Using
the name for anything more would be a misuse of it.

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

**Parity on scans, 12% and 39% behind on joins.** Two causes were found and fixed on the
way here, and one remains unexplained.

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

**What remains is not explained.** The join gap is smaller than it was and it is still
there. Recording it as unexplained is the honest state; the alternative is a plausible
story nobody checked.

---

## Clustering, which closes the objective pushdown could not

`NFR-PERF-02` names bloom filters and late materialization as the preconditions for its
250 ms. Neither is the lever. Bloom filters do not apply to Q6, which has no equality
predicate; late materialization costs rather than saves. **Sorting does.**

Q6 selects one year in seven of `l_shipdate`. Written in arrival order every row group
holds the whole date range, so the bounds exclude nothing and the query reads the entire
table. Written in date order most row groups fall outside the year and are skipped on
their statistics, before any decoding.

| Layout | Single query | p95 at 8 clients |
|---|---|---|
| Generation order | 222 ms | 1819 ms |
| Sorted by ship date | **31 ms** | **234 ms** |

**7.8× at the stated concurrency, and 234 ms is inside the 250 ms objective.** Compaction
now does this: a settled partition is written in the order the policy declares.

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
| The tests guarding each core invariant are verified against the defect they claim to catch | `tools/mutation-audit.py` — 121 specific defects applied one at a time; all 121 fail the suite. Thirteen did not when first run; four catalogue entries turned out to be equivalent mutants no test could ever have caught, one entry was inert until corrected, and chasing another produced a documentation correction rather than a new test |

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
- **The provider is slower than the engine's own file listing on joins**, by 12% on a
  three-way and 39% on a six-way, at parity on scans. The cause is not established.
  Partitioning the scan and skipping the unnecessary position filter each closed part of
  the gap; what remains has not been explained, and guessing at it here would be worse
  than saying so.
- **No merge strategy beyond union.** Latest-version-per-key, which mutable tables need,
  is not implemented; the provider unions its tiers.
- **The governor decides but governs nothing.** Admission and the pressure ladder are
  built and tested, and nothing calls either: no memory pool reports its occupancy, no
  subsystem publishes a signal, and no query passes through admission on its way to
  running. They are decision functions without callers, like the maintenance scheduler
  was before the driver.
- **No counting allocator, no spill isolation, no deadline propagation.** The
  architecture requires all three around admission: true accounting outside the engine's
  own pool, spill on a different filesystem from the write-ahead log, and cancellation
  that takes effect within a bounded time. None exists.
- **Exact order statistics buffer their input.** Selection is linear rather than
  `n log n`, so it beats sorting, but every observation must be resident. `FR-QUERY-08`
  asks for a bounded-memory algorithm over large inputs and this is not one. Exact and
  bounded are independent properties, and only the first is delivered.
- **The exactness gate is not wired into a session.** `check_exactness` is a function
  with no caller: nothing carries the session's exactness setting, and nothing attaches
  the watermark to a result.
- **No catalog and no table provider.**
- **No log checkpoints.** Replay reads every commit, so startup cost grows linearly with
  a table's commit count. Fine at the scale tested; not fine at a year of continuous
  capture.
- **No checkpoints, deletion vectors, column mapping or partition values in the log.**
  Row counts, bounds and null counts are written; everything else the protocol permits is
  not, and a reader requiring any of it refuses these tables. A reader requiring any of them refuses these tables, which is the correct
  outcome — refusing is visible, and a partially-implemented protocol feature is not.
- **Nothing calls the maintenance loop on a timer.** The tick is built and tested end to
  end, but a caller has to invoke it, supply the live set and supply the pinned snapshot
  positions retirement checks against.
- **No graph engine, no API surfaces, no multi-tenancy, no security.**

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
cargo xtask check-all          # every repository invariant, including doc links and version claims
cargo test --workspace         # everything that needs no database
crates/sankhya-cdc-apply/tests/run_e2e.sh   # capture against a live database
```

[`QUICKSTART.md`](QUICKSTART.md) walks through building it from nothing.
