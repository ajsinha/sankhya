<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# SANKHYA — The soak: method, results, and what four attempts taught

**Status:** Implementation — M0–M5 complete, M6 in progress
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

A short run against the real server, on every build — writes, queries and maintenance
concurrent. This is `crates/sankhya-server/tests/soak.rs`.

```
soak: PASS over less than a minute, judged against a less than a minute horizon,
      first 10 sample(s) discarded as warm-up

  resident_bytes   steady
  open_files       steady
  metric_series    steady
  history_bytes    steady
  audit_records    steady
  live_files       steady
```

Sixty rounds, five queries each, a file published every round, compaction every fifth round.
Resident memory settles around **86 MB**; live files oscillate between roughly 1 and 6 and
return to the floor at every compaction; audit records track queries one for one.

**And the harness is proven to notice.** `crates/sankhya-soak/tests/leak.rs` injects one
failure of each shape and requires the run to fail on it:

| Injected | Caught as |
|---|---|
| 20 MB/minute retained | `GROWING` — reaches its limit inside the horizon |
| 4 descriptors/minute not returned | `GROWING` |
| A sawtooth whose peaks climb | Fails, while a level sawtooth passes |
| Audit drifting from 1 to 2 records per query | Fails, **while its total looks healthy** |
| Nothing sampled at all | `COULD NOT JUDGE` — and that is a failure |

Without those, a green soak would be green because nothing was capable of turning it red —
an untested backup by another name.

## 5. What four attempts taught

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

## 6. What has not been done

**The multi-day run at the ten-gigabyte scale.** That is `M6` exit criterion 4 and it is a
scheduled pipeline, not a `cargo test`. Nothing here claims otherwise.

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
