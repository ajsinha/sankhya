<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# SANKHYA — The soak: method, results, and what four attempts taught

**Status:** Implementation — M0–M5 complete, M6 closing, M7 in progress
**Milestone:** M6 §10.7 · **Exit criterion 4**

---

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

## 8. What has not been done

**The multi-day run at the ten-gigabyte scale, with the reading workload.** That is `M6` exit
criterion 4. The four-hour run of 2026-08-26 exercised the append, log-replay and compaction
paths at that scale and passed; it did not read the data, so it discharges the criterion only
for the paths it touched. Nothing here claims otherwise.

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
