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
| **M3** Query engine and storage performance | 28–34 ew | A vertical slice, asserted engine settings, compaction (policy, execution and retirement, with the small-file penalty measured), the arrival tier's retention contract, a query spliced across both tiers, and the maintenance scheduler's arbitration. No table provider, statistics catalogue or caching |
| **M4**–**M8** | — | Not started |

---

## What runs today

| Capability | Evidence |
|---|---|
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
| A cold reader starts from a checkpoint, and any reader may ignore one | **10×** at fifty thousand commits; the kernel reads a checkpoint this system wrote by hand, and is proven to *use* it rather than tolerate it — the commits it covers are deleted and the table still resolves |
| A warm process pays for what changed, not for the whole history | Table file sets are cached and resumed; the cache cannot go stale because it never trusts its own version, and asking costs one filesystem probe rather than a directory listing |
| Log replay scales linearly with a table's history | Guarded by measuring the *ratio* between two sizes rather than a clock, so it means the same on any machine — and proven to fail on the quadratic implementation it replaced |
| A file is prunable from the moment it is published | Capture computes statistics from the batch it just encoded — the same data, already in memory — so a file does not wait for maintenance to become skippable |
| Statistics survive a restart and other engines can read them | Bounds and null counts are written into the table log itself, so a fresh process prunes exactly as a warm one does — and the kernel reads a log carrying them |
| Compaction computes the statistics the provider prunes on | Bounds, null counts, widths and a cardinality sketch, produced by the merge that was already reading the data — no separate analysis pass and nothing for an operator to remember to run |
| The provider skips files the catalogue proves irrelevant | Nine of ten files pruned on a point lookup, five of ten on a range, and none at all on a disjunction, a predicate over an uncatalogued column, or no predicate. The same query returns the same answer with and without the catalogue |
| Planning does no file I/O | 800 files plan in 1.37 ms against 10.33 ms for a directory listing — **7.5×**, widening with file count |
| A dependency declared test-only actually is | `cargo xtask check-features` reads the manifests; proven to fail when the oracle is moved into `[dependencies]` |
| The tests guarding each core invariant are verified against the defect they claim to catch | `tools/mutation-audit.py` — 86 specific defects applied one at a time; all 86 fail the suite. Thirteen did not when first run; four catalogue entries turned out to be equivalent mutants no test could ever have caught, one entry was inert until corrected, and chasing another produced a documentation correction rather than a new test |

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
