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
| Maintenance work is arbitrated against one budget across both sides | Strict class ladder; safety and availability work preempts queries and ignores the duty cycle, everything below it does not; a job that cannot checkpoint is refused rather than started; waiting never promotes a job out of its class |
| The maintenance scheduler cannot destroy retained history | There is no erasure class to configure — the guard is on the type, and an exhaustive match makes adding a variant a compile error |
| A fragmented warehouse is cleared by ticking, and converges | 60 fragments reduced by repeated ticks until nothing is left to do, with the row count unchanged; urgency maps onto the class ladder, so a degrading partition preempts and an ordinary one waits; one failing partition does not block the rest |
| A query is unaffected by compaction running underneath it | The same aggregate before and after a merge, over the live set |
| The tests guarding each core invariant are verified against the defect they claim to catch | `tools/mutation-audit.py` — 20 specific defects applied one at a time; all 20 fail the suite. Three did not when the catalogue was first run |

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
- **The spliced read is a view, not a table provider.** It registers the selected tiers
  and unions them in SQL, which is correct and demonstrably exact-once, but it is not
  the SANKHYA-owned `TableProvider` of DEC-06 — there is no statistics catalogue, no
  pruning from SANKHYA's own metadata, and no merge strategy beyond union.
- **No catalog and no table provider.**
- **No table log.** The live set is carried in memory by the caller across maintenance
  ticks, which is correct but not durable: a restart loses it, and there is nothing for
  an external engine to read. This is the concrete gap where Delta or Iceberg belongs —
  see the finding below.
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

**The general lesson, recorded because it applies to every test not yet audited:** a test
written to catch a defect is not evidence that it catches it. Until it has been run
against that defect, it should be assumed to be in the same state as the three above.

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
