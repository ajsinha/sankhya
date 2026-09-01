<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# ADR-0018 — A record that does not fit, and the pipeline that must not stop for it

**Status:** Accepted · **Date:** 2026-08-31 · **Milestone:** M13 — the design gate, before any implementation
**Builds on:** [ADR-0004](0004-the-date-axis.md), [`FR-TIER-08`](../REQUIREMENTS.md), [`RSK-35`](../IMPLEMENTATION_PLAN.md)

## Context

Everything this system does refuses rather than coerces. A type that cannot round-trip makes a
table ineligible. A clone whose lineage is unreadable is a refusal rather than an absence. A
statement the server will not honour is refused rather than confirmed and discarded (`DEC-47`).

**A stream cannot refuse the way a statement can.** There is nobody to tell: the producer wrote
the record and moved on, the connection that carried it is closed, and the caller who would read
an error does not exist. And the obvious alternative is worse --- stopping the pipeline for one
malformed document turns one bad record into an outage, which is how ingest systems come to be
run with every validation switched off.

So this decides what happens to a record that does not fit, and --- the harder half --- when a
*run* of them means something different from one of them.

## Decision 1 — Quarantined, whole, with the reason attached

A record that does not fit is neither dropped nor coerced. It is written to a **quarantine**,
and what is written is the record **exactly as it arrived**, alongside:

| Field | Why |
|---|---|
| the payload, verbatim | a record reduced to an error message cannot be replayed, and replay is the only actual remedy |
| the reason, as a code | so a client can count kinds rather than parse sentences (`DEC-44`'s argument) |
| the reason, as a sentence | so a person can act on one without a lookup table |
| the config version that refused it | a config changes; *"why did this fail in March"* is otherwise unanswerable |
| when it arrived, and from where | which file, which offset --- the coordinates a replay needs |

Dropping is refused because a pipeline that discards what it cannot parse is one whose
correctness claim is *"everything I kept was fine"*, which is not a claim about the data.

## Decision 2 — The quarantine is a table

Not a directory of rejected files beside the warehouse.

A side directory is outside everything this system has built: nothing sweeps it, nothing backs
it up, no policy governs who can read it --- and it holds **source data**, which is the most
sensitive thing here. It would be a second store with none of the first one's properties, which
is exactly the shape `check-writers` exists to refuse.

As a table it inherits durability, backup, retention, tiering and policy for free. Its schema is
fixed and independent of any pipeline's, because the whole point is that these records do not fit
the pipeline's schema. It carries `sank_data_date` like everything else ([ADR-0004](0004-the-date-axis.md)).

## Decision 3 — A quarantine has a mandatory expiry

A quarantine that only grows is `RSK-35` in another costume: an accumulation nobody is
responsible for, each record individually reasonable, with no day on which anybody could have
decided otherwise. Worse than rehydrated backups, because it holds exactly the records nobody
looked at.

So **a pipeline configuration without a quarantine retention is refused at validation**, in the
same way a rehydration without an expiry is refused. Expiry is enforced by the maintenance
thread that already retires files --- nothing new schedules anything.

## Decision 4 — One bad record is an incident; a run of them is an outage

These need different answers, and giving them the same one is the defect this decision exists to
prevent.

**The control is a rate over a recent window, not a total.** A total accumulates over the life of
a pipeline and eventually trips for reasons that are historical; a rate says what is happening
now. When more than a configured fraction of a recent window is quarantined, the pipeline
**stops**.

Stopping means: it halts, says why, names the position of the last record it accepted, and
**waits for a person**. It does not retry on a timer. A source whose shape changed produces
all-bad records, and a pipeline that sidelines them one at a time turns a schema change into a
silent data outage --- everything green, nothing arriving. Auto-resume is how the same outage is
rediscovered every five minutes and acted on by nobody.

The threshold is configured, and it has a default, because a control an operator must invent a
number for is one that ships switched off.

## Decision 5 — What a configuration may not do

Each of these turns a defect at the source into published data that looks fine, which is the
failure mode that is never noticed at the time.

| Refused | Because |
|---|---|
| Silently widening a type | an `Int32` column fed an `Int64` is a config that will one day be right about a number that is wrong |
| Inventing a value for a missing key | a default that is indistinguishable from a measurement is a measurement nobody made. A missing key is a refusal unless the column is nullable **and** the config says so by name |
| Accepting keys it has never seen | a source that grew a field is news. `unknown: ignore` may be written down; it may not be the default |
| Coercing a string to a number, or a number to a date | the conversions that always look reasonable in a test and are ambiguous in production data |

Everything above is a **validation** failure --- refused when the config is loaded, naming
**every** rule that failed rather than the first, for the reason the cube's measure validation
gives: fixing them one build at a time is how a person gives up.

## Decision 6 — A file's position is committed with its rows, in one commit

A microbatch pipeline restarts. If where-we-got-to is recorded separately from what-was-
published, a crash between the two produces either duplicated rows or a silent gap, and which one
depends on the order somebody chose.

**So the position is part of the same commit as the rows.** Either both are visible or neither
is, which makes a restart a question with an answer rather than a reconciliation exercise. This
is `FR-TIER-08`'s argument for the purge journal --- commit the transition before the action, and
make every phase resumable --- applied to ingest.

It also settles re-ingest: a source already recorded as complete is **not read again**. A file
redelivered under the same name is a real event with two possible meanings and the system cannot
tell which, so it does not guess.

### Amendment, 2026-09-01 --- what a position can actually record

Implementation found a claim in the paragraph above that the design cannot support, and the
soak found it in under a minute.

The position is a **high-water mark**: the last source finished, in name order. That is what
keeps it O(1) --- the alternative, a set of every source ever read, grows for as long as the
feed runs and is held in a table property rewritten on every commit.

A mark records *where a feed got to*, not *which files it read*. So a source sorting below the
mark is **indistinguishable** from one finished last week: both are "behind where we are". The
first implementation reported everything below the mark as having arrived late, and in a spool
directory --- which keeps its files --- that meant every previously-finished source was
announced as a late arrival on every run after the third.

**The decision is which error to prefer, and it is: never re-ingest.** A source that genuinely
arrives out of order is skipped rather than read. Duplication is silent and permanent --- every
row twice, in a table somebody reconciles against, with nothing in the result saying so --- where
a skipped source is a file still sitting in a directory, findable and replayable.

What replaces the refusal is a **count**. Every run reports how many sources it skipped as
already read. The number is steady for a spool that accumulates and grows when sources start
arriving behind the mark, which is what turns an undetectable event into one an operator can
notice. Refusing individually is not available at this cost; noticing is.

Distinguishing them properly requires remembering which sources were read, and no bounded
structure does it: a *recent* set answers "in the set" but cannot tell "long since done" from
"never seen" for anything that has fallen out of it.

## Consequences

**Quarantine is a first-class surface, not a debug feature.** It is queryable, governed by
policy, counted in metrics, and expires. Somebody has to be able to ask *"what did not fit
yesterday, and why?"* in SQL, or the mechanism is a hole records fall into.

**The stop is a state, and states need an interface.** A halted pipeline must be visible, and
resuming must be an act somebody performs. That is a command surface this milestone has to build,
not a flag in a file.

**Ingest is a producer, not a storage path.** Everything above ends in `sankhya-publish`. A
config that reached storage another way would be the second writer `check-writers` refuses, and
this ADR does not create an exception to that.

## What this does not decide

**Ordering guarantees across files.** Whether two files ingested concurrently may interleave in
the published order is a question `M15` will have to answer for partitions anyway, and answering
it now for files alone would probably be answered differently there.

**Schema evolution.** What happens when a source legitimately grows a column is a real question
with a real answer, and it is not *"quarantine everything from now on"*. It waits until there is
a deployment whose source has actually changed, because the alternative is designing a migration
path against an imagined one.
