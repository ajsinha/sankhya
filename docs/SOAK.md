<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — The soak: method, results, and what four attempts taught

**Status:** Implementation — M0–M8 and M10 complete; M8's scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11
**Milestone:** M6 §10.7 · **Exit criterion 4**

---

## The run of 2026-08-28, and the two defects it found

`PASS` over 44 judged minutes against a two-hour horizon, all seven measures steady. 157
rounds, 1,570 files published, **29.7 GB / 2.41 billion rows** scanned, the cube answered 39
times, and maintenance reclaimed 11.42 GB across 1,234 ticks.

Getting there took three runs, and the first two are the reason this page exists.

| Run | Resident memory | What was in it |
|---|---|---|
| Before cube coverage | 779 MB, steady | no cube path at all |
| First with cubes | **5,876 MB, climbing** | `collect()` on the whole fact table, plus a tokio runtime per navigation |
| Second | 1,821 MB, climbing | streaming hydration |
| Third | **2,246 MB, steady** | one runtime for the run |

**Neither defect was visible to any unit test**, and both were introduced the same day the
cube path was added to this harness.

The first was in the product: `publish_from_fact_table` called `frame.collect()`, materialising
every batch of the fact table before absorbing any of it, when `absorb` takes one batch at a
time. Decompressed Arrow runs two to four times the Parquet on disk, so a one-gigabyte table
was two to four gigabytes resident.

The second was in this harness: `navigate_the_cube` built a fresh runtime and `SessionContext`
on every call. Creating and dropping those forty times leaves a high-water mark the allocator
does not return, and from outside it reads exactly like a leak in the product. A test doing
infrastructure work, which is what the golden rule exists to catch.

**And the judge was right where reading the numbers was not.** Watching the series interval by
interval, the growth looked linear and alarming — 173, 118, 81, 55, 61, 72, 26, 40, 39, 32, 20,
20, 45, 3, 17, 32, 30, 1 MB. Fitted across the whole series it projects no crossing of the 8 GB
limit within two hours, and the final reading *fell* by 128 MB. A leak does not give memory
back. Eyeballing a noisy series is how a plateau gets reported as a leak, and the opposite.

## What the run exercises

Writes through `sankhya-publish`, reads through the read path, maintenance on the warehouse's
own thread — and, since 2026-08-28, **the cube path**: a cube is declared over the first
table, hydrated from it and rolled up every fourth round.

That last one was absent, and its absence was the kind that reads as success. A soak covering
M6's surface and reported as covering M7 is the same overstatement M6 closed on: a criterion
met by something adjacent to what it asked for.

The cube's cost lands in measures that already exist rather than a new one. Cells are held in
this process, so a leak in them shows in `resident_bytes`; cuboids are written under the
warehouse, so their population shows in `warehouse_bytes`. A measure nothing distinguishes
would be one more thing every soak has to supply for no judgement.

Hydration runs every fourth round rather than every round. It reads the whole fact table, so
doing it each time would make the soak a measurement of hydration instead of of the system —
often enough that a leak accumulates visibly over forty-five minutes, rare enough that the
write and compaction paths still dominate.

The run **asserts** it navigated the cube at least once. A zero would be invisible in a report
full of healthy measures.

## 1. What a soak is for

Not *"it did not crash"*. That is what a soak reports and it is the one thing nobody doubted.

A soak exists for a specific class of failure: the kind invisible in any single sample and
obvious across a week. Memory that grows a megabyte an hour. Descriptors that are not
returned. A cache with no eviction. Compaction that never quite catches up, so each cycle
starts a little further behind than the last.

Every one of those is something this system has a **bound** for. A soak is where the bound is
found not to work.

## 2. The criterion, and why "clean" was not one

`§10.7` originally read, in its entirety: *"A multi-day soak."* That is not something anyone
can fail. It is now:

> **A soak passes when no bounded measure has a projection that crosses its threshold within
> the observation horizon.**

A measure trending upward with a crossing three weeks out is a **failure**, not a curiosity.
It is precisely the failure that ships and gets diagnosed six months later by somebody else.

**Inconclusive is a failure too.** A run whose sampling broke must not report the same green
as one that ran properly, because the green is the thing everybody reads. This is the same
distinction the diagnostic draws between a check that was clean and one that could not run,
arrived at from the other direction.

## 3. Three kinds of bounded

The naive soak watches every number and complains when one rises. Half of them are supposed
to — queries served, audit records, bytes written — and complaining about those trains
everybody to ignore the report. That is the failure mode of every monitoring system ever
switched off.

So a measure declares **what kind of bounded it is**:

| Kind | The question | Example |
|---|---|---|
| **Steady** | Is the slope positive? | Resident memory, open descriptors, distinct metric series |
| **Per unit of work** | Is the *ratio* drifting? | Audit records per query |
| **Sawtooth** | Are the **peaks** climbing? | Live files per table |

The second and third are where the interesting failures live.

**Per unit of work** catches what a total never can. Audit records are supposed to grow, one
per query. What must not grow is records *per query* — if that drifts upward something is
recorded twice, and if it drifts down something is not recorded at all, which is the worse
direction. Watching the total passes this every single time.

**Sawtooth** is a distinction a point-in-time diagnostic cannot draw. Writes add files;
compaction removes them. Two such series can look identical at any moment: one returns to the
same floor every cycle, the other starts a little higher each time. The first is a system
keeping up. The second is a system falling behind, and the difference is only in the peaks.

## 4. The results

### The ten-gigabyte run

Ten tables, one gigabyte each, generated in 221 seconds. Writes, log replays and a compaction
duty cycle running together; a reading every fifteen seconds; judged and written to disk every
two minutes, so a run killed at hour nine leaves hour eight's verdict behind.

```
[21:46:08Z]  t+  362s  round 25  published 250  queries 250  live_files 490  rss  9 MB  PASS
[21:50:10Z]  t+  604s  round 41  published 410  queries 410  live_files 492  rss 13 MB  PASS
[21:54:12Z]  t+  846s  round 57  published 570  queries 570  live_files 494  rss 12 MB  PASS
[21:58:15Z]  t+ 1088s  round 73  published 730  queries 730  live_files 496  rss 12 MB  PASS
```

Resident memory oscillates between 9 and 14 MB and does not trend. Live files hold near the
floor the duty cycle returns them to. Audit records track queries one for one.

**The first two reports say `watching`, not `PASS`, and that is the harness working.** Ten
samples are discarded as warm-up and ten more are needed before a rate means anything, so
nothing is judgeable for the first five minutes. It says so rather than guessing.

**The horizon grows with the run** — 633s at six minutes, 2811s at eighteen. A run may speak
about three times what it observed and no further, which is why the four-hour run in progress
will entitle claims about twelve hours and **not** about three weeks, whatever it shows.

### The short run, on every build

The same harness against the real server through the query path, sixty rounds in eight
seconds. It proves the measurements come from a running system and the judgement runs end to
end; it establishes nothing about duration, and does not claim to.

### The harness is proven to notice

`crates/sankhya-diagnostic/tests/leak.rs` injects one failure of each shape and requires the run to
fail on it:

| Injected | Caught as |
|---|---|
| 20 MB/minute retained | `GROWING` — reaches its limit inside the horizon |
| 4 descriptors/minute not returned | `GROWING` |
| A sawtooth whose peaks climb | Fails, while a level sawtooth passes |
| The same sawtooth ending on a trough | Still fails — peaks, not last readings |
| Audit drifting from 1 to 2 records per query | Fails, **while its total looks healthy** |
| Nothing sampled at all | `COULD NOT JUDGE` — and that is a failure |

Without those, a green soak would be green because nothing was capable of turning it red.

## 5. The result nobody expects: the harness was the hard part

**Every defect this soak has found so far has been in the measuring apparatus, not in the
system it measures.** Four in the judgement, four more in the runner. The system under test
has not yet produced a single finding.

That is worth stating rather than quietly enjoying, because it generalises. A soak is a
measuring instrument, and an instrument that has never been shown to be wrong is an
instrument nobody has looked at hard enough. The four runner defects are the sharpest
evidence:

| Defect | What the report said while it was wrong |
|---|---|
| Commit versions from a global counter, refused as non-contiguous, error swallowed by `.ok()` | Healthy. Three minutes of writing, **nothing published**, live files unchanged |
| Compaction that removed every live file and replaced it with one small batch | Would have been healthy — while ten gigabytes stopped being live at round 8 and the remaining 3h58m soaked an empty warehouse |
| A per-table limit judged against a **sum across ten tables** | `BREACHED — 4900 past the limit`, when every table held 490 |
| Compaction that never re-compacted its own output | Healthy for hours, then a slow climb that would have been flagged near the end of the run — correctly, and about the harness |

**Three of the four produced a green report while measuring nothing, and the fourth produced
a red one about nothing.** None would have appeared in a summary at the end. All four
surfaced because the run prints as it goes — which is the argument for reporting *during* a
soak rather than at the end of one, and it is now the strongest thing this document has to
say.

The obvious inference is uncomfortable and probably right: **a soak that has never found a
defect in itself has not been read closely enough to be trusted about anything else.**

## 6. What four attempts at the judgement taught

Every one of these was found by running the thing, not by reading it.

### The diagnostic's linearity gate is wrong here

The first version reused `Trend::time_until`, which refuses to project through a series that
does not fit a line. That is right for a diagnostic: a confident date drawn through a sawtooth
reports where in the cycle the samples fell.

It is wrong for a soak, and wrong in the worst direction. **A healthy measure is noisy and
flat**, which has an r² near zero — there is no trend to explain. So the baseline run reported
memory, descriptors and the history file as *unjudgeable* while nothing whatsoever was wrong
with any of them.

It is the same trap as r² on a constant series, which had to be corrected in `sankhya-math`
earlier for exactly the same reason: **the statistic is undefined where there is nothing to
explain, and "undefined" is not "bad"**.

The soak asks a different question of the same numbers — *is this drifting upward over hours*
— and the answer is the slope. Noise averages out across a run's worth of samples.

### A half-second run reported a memory leak

The first real run against the server flagged resident memory `GROWING — reaching its limit
in about 6 minutes`.

It was not a leak. Sixty rounds had completed in **0.35 seconds**, and a process allocates as
it starts: session contexts, decoded batches, caches filling for the first time. Across the
opening of a run every one of those looks exactly like a linear climb, because over a short
enough window it *is* one.

Two things were wrong, and only fixing both was enough.

**A warm-up prefix is discarded** — a fixed, declared ten samples, stated in the report.
Fixed and declared is what keeps it honest: discarding *until the series looks flat* would
hide every leak by construction, because a leak is precisely a series that does not go flat.

**And the horizon is bounded by what the run observed.** Discarding warm-up alone did not fix
it and could not have — the whole run was inside the ramp. Half a second of samples was being
extrapolated to three weeks, a factor of three and a half million.

`sankhya-diagnostic` already carries this guard, and applying it there and not here was the
omission. A trend measured over an interval can speak about a few multiples of that interval
and no further; past that the arithmetic still works and stops being evidence.

**The consequence is stated rather than hidden: a short run cannot pass a long horizon.** It
reports that it was too short, and the answer is to run for longer rather than to widen the
limit — which is the tempting wrong answer and is why the message says so.

### Every caller got the warm-up arithmetic wrong

Both of them, on the first attempt: they derived a horizon from the raw run span while the
judgement used the span *after* the prefix, overshooting by exactly the warm-up and turning
every fixture inconclusive.

That is a good sign a calculation does not belong at the call site.
`report::supported_horizon` now answers it once — and it is what a scheduled runner needs
anyway, being the answer to *"how long must this run to say anything about three weeks?"*

### A summary spans less than what it summarises

The sawtooth path trends the **peaks**, and a peak sits inside its window rather than at the
edge — so the peak series always spans less than the run it came from. Deriving the run's
entitlement from the peaks shrank it by an amount depending on where the peaks happened to
fall. The span is a property of the run, so it is taken from the run.

## 7. What the word "query" meant here, and no longer does

Found on 2026-08-27, by looking at the warehouse directory rather than by any test failing.

The loop counted a **log replay** as a query:

```rust
// Queries: replay every table's log, which is what planning actually costs.
for root in &roots {
    if live_files(root).is_ok() { queries += 1; }
}
```

That is a real cost and a real leak surface, and it is not a read. The ten gigabytes sat in
`part-*.parquet` files that were written once at seed time and **read by nothing, anywhere in
the binary**. So a run reporting "10 GB, 5,130 queries, PASS" measured the append and log
paths, and named its workload after work it did not do.

The figures it produced were true of what it measured. The label was not, which is worse than
measuring less: a soak nobody can trust the scope of is a soak nobody can act on.

**What changed.** `scan_parquet` decodes rows and returns a row count, byte count and a
checksum. The checksum is not for integrity — Parquet has its own — it is there so that *the
scan read nothing* is distinguishable from *the scan read zeros*, and so a decode cannot be
elided into an empty loop that reports throughput. The loop now scans a **bounded, rotating**
window per round: bounded because scanning ten gigabytes per round makes a round take minutes
and the sampling useless, rotating because scanning the same slice exercises the page cache
rather than the read path. A round that reads nothing prints `UNREAD` rather than being
averaged away.

The counter that used to be called `queries` is now `planned`, because that is what it counts.

## 7b. The sixty-minute run of 2026-08-29, with materialisation load-bearing

`PASS` over 59 judged minutes against a **two-hour** horizon, first ten samples discarded as
warm-up, and all seven measures steady:

| | | | |
|---|---|---|---|
| `resident_bytes` | steady | `open_files` | steady |
| `metric_series` | steady | `history_bytes` | steady |
| `audit_records` | steady | `warehouse_bytes` | steady |
| `live_files` | steady | | |

| | |
|---|---|
| Rounds | 196 — 1,960 writes published, 1,960 queries planned |
| Read | **37.2 GB across 3.01 billion rows** |
| Cube | answered 49 times |
| Maintenance | 1,651 ticks, reclaiming **11.66 GB** |
| Resident memory | 2,017 MB at close |
| Warehouse | 9 GB throughout, 90 live files throughout |

### Why this run is not a repeat of the last one

The 44-minute run of 2026-08-28 exercised a cube. This one is the first to exercise a cube
**served from storage**, because until M7 closed, `materialised` was dead code: the refresher
built cuboids on a timer and every query still went to the fact table. The cuboid read path,
the completeness columns and the scope match all ran here for the first time under load.

Two numbers are worth putting side by side, because the expectation would be the opposite:

| | 2026-08-28 | 2026-08-29 |
|---|---|---|
| Resident memory | 2.2 GB | **2.0 GB** |
| Judged minutes | 44 | 59 |
| Rows read | 2.41 bn | 3.01 bn |

**More work, longer run, less memory.** The cuboid path replaces fact-table hydration rather
than adding to it, so serving from a cuboid reads less than the query it replaces. That was the
argument for materialising in the first place, and this is the first measurement of it rather
than the first assertion.

`warehouse_bytes` steady at 9 GB across 1,960 publications is the other number to keep:
reclamation is keeping pace exactly, and the 11.66 GB reclaimed is more than the warehouse
holds. Superseded cuboids are part of what it collected --- a path that did not exist a day ago.

## 8. What has not been done

**The multi-day run at the acceptance scale, with the reading workload.** That is `M6` exit
criterion 4. The four-hour run of 2026-08-26 exercised the append, log-replay and compaction
paths at ten gigabytes and passed; it did not read the data, so it discharges the criterion
only for the paths it touched. Nothing here claims otherwise.

**The scale itself doubled on 2026-08-29**, by owner decision: `SANKHYA_SOAK_GB` now defaults
to **twenty**, not ten. The reason is that the figure means something different than it did
when it was chosen. Ten gigabytes was picked when the soak did not read its data at all --- it
was a number about how much got written. Now that a run reads 3.01 billion rows, the dataset
is a working set, and the property that matters is that it does not fit in page cache. A
bigger one is a harder test of the same machinery.

The cost is linear and lands almost entirely in the fill: roughly forty-five seconds per
gigabyte, so the preamble moves from about seven minutes to about fifteen. The judged window
is unaffected, because it is counted from when measurement starts rather than from launch.

### The doubling landed in one line, and three thresholds stayed where they were

**Found 2026-08-30, when the first run at the new scale aborted inside ten minutes.** The
verdict read *"the warehouse holds 33.2 GB against a budget of 32.0 GB"*, and the report called
it a reclamation failure. It was not one. `measure.rs` said so in its own comment: *"Thirty-two
gigabytes against a **ten**-gigabyte target."* The budget was a flat constant sized at 3.2× the
old figure, the doubling moved only the harness's default, and headroom fell from 22 GB to 12 GB
without anything recording that it had.

Two more were stranded the same way, and one of them was already breaching in silence:

| | Was | Why it was that | Now |
|---|---|---|---|
| `warehouse_bytes` | 32 GB flat | 3.2× a ten-gigabyte target | `3.2 × scale.gb` |
| `live_files` | 1,000 flat | a two-GB-per-table run measured 1,080 in its worst table | `1,000 × per-table GB` |
| `USAGE` text | "default 10" | restated the harness's own default | deleted — it could only go stale again |

**The fix is structural rather than three new numbers.** A `Scale` type now lives in the library
beside the measures, the harness reads the target *from* it instead of parsing its own copy, and
both limits are arithmetic on it. A scale change now moves everything that follows from it,
which is the property that was missing — the numbers were only the symptom.

**It made a passing unit test scale-dependent, which is worth recording because it nearly
shipped.** `a_sawtooth_is_judged_from_its_peaks_even_when_it_ends_in_a_trough` built its fixture
from values tuned to sit under a flat 1,000. Once the limit derived from the scale, the fixture
was measuring the environment. It now asks the declaration what the limit is and states the
fixture as fractions of it, so it proves the same thing at any scale.

### The run of 2026-08-30 — the first `PASS` at twenty gigabytes

The prior green run was at ten, so **nothing had ever judged this scale**. Forty-five judged
minutes, every measure `PASS`.

| | 2026-08-28, 10 GB | 2026-08-30, 20 GB |
|---|---|---|
| Judged minutes | 44 | 45 |
| Rounds | 157 | 77 |
| Files published | 1,570 | 770 of 770 planned |
| Scanned | 29.7 GB / 2.41 bn rows | 14.5 GB / 1.16 bn rows |
| Resident memory | 2,246 MB steady | **2,271 MB** |
| Reclaimed | 11.66 GB | **21.50 GB** over 816 maintenance ticks |
| `warehouse_bytes` peak | 9 GB | **41.58 GB** |

**Both stranded thresholds were exercised, which is what makes the fix evidence rather than
argument.** The warehouse peaked at 41.58 GB — over the old flat budget by more than nine
gigabytes, and comfortably inside the derived one. `live_files` went **1,080 → 90** across a
reclamation cycle, a sawtooth doing exactly what `Bound::Sawtooth` exists to permit and exactly
what the flat limit of 1,000 would have called a breach. Neither of those is a number that
needed changing; both are the same machinery working, judged against a scale it was told about.

**Fewer rounds at more data is expected and is not a regression.** A round publishes to every
table, and each table now holds twice as much, so a round costs proportionally more. The rows
scanned per second is the comparable figure and it is unchanged.

**Resident memory is the one worth watching.** It rose across the run — 1,223 MB at t+478s to
2,458 MB at t+2671s, settling at 2,271 MB — and passed its bound throughout. That it lands
within 25 MB of the ten-gigabyte run's steady figure, at double the data, is the statement
worth keeping: the working set is bounded by the machinery rather than by the dataset. A future
run that ends materially above this is a finding.

What exists is the harness, driven against the real server on every build, proven to detect
each shape of failure it claims to detect. **The scheduled run is a change of duration and
scale rather than a first attempt at the whole thing** — which is the difference between
having a soak and having somewhere to start one.

Two numbers change: the round count, and the horizon that follows from it. At the acceptance
scale the run needs to observe for **at least a week** to speak about three weeks, and
`report::supported_horizon` is what says so rather than somebody's arithmetic.

**Retained evidence.** The run prints its report; it does not yet append to a durable record
the way restore drills do. A soak history is the thing that shows a slow drift across
releases, and it is the obvious next piece.

---

## Where to go next

- [`STATUS.md`](STATUS.md) — what is built, and what is not
- [`GUIDE.md` §10](GUIDE.md#10-the-diagnostic) — the projection machinery this reuses
- [`runbooks/`](runbooks/) — one per alert that can page
