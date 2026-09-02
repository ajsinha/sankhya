<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# ADR-0019 — A name for one instant across many tables

**Status:** Accepted · **Date:** 2026-09-02 · **Milestone:** M17 — the design gate, before any implementation
**Builds on:** [ADR-0016](0016-zero-copy-cloning.md), [ADR-0008](0008-serving-cubes-under-policy.md), [ADR-0017](0017-the-client-contract.md), [`RSK-35`](../IMPLEMENTATION_PLAN.md)

## Context

A clone pins **one table at one version**. That is the right shape for freezing a table, and the
wrong shape for the thing people actually do with a warehouse.

A market-risk run reads the trade population, the FX rates, the curves and the legal-entity
hierarchy. It must read **all of them as of one instant**, or the reconciliation problem this
system exists to remove reappears *inside a single query*: four tables, four moments, one number
that reconciles to nothing. The same is true of a regulatory submission, a month-end close, and
any two reports that must agree with each other.

The machinery is already here and has no surface. The read path takes a target position and
splices tiers against it; `read_as_of` is that position, read once at startup and never named.
What is missing is a way to **name** a position, keep the files it references alive, and hand
the name to a query.

It is nearly free, and that is the point: nothing is copied. A snapshot is a label on a
consistent point plus a rule that keeps its files alive — the same reclamation machinery a clone
already uses, applied to a set of tables rather than to one.

Three questions have no obvious answer, and this decides them.

## Decision 1 — A snapshot is a position, not a table

A clone is a **table**: it has a name in the catalogue, it is queried by that name, and dropping
it is a schema change. A snapshot is a **position**: it has a name in its own namespace, it is
quoted *by* a query, and dropping it changes no schema.

Concretely, a snapshot records a version per table — `sales.orders@412`, `ref.fx@77` — and
nothing else. It holds no rows, no schema and no files of its own.

Keeping them distinct matters because they answer different questions and would otherwise be
confused into one feature that does neither well:

| | Clone | Snapshot |
|---|---|---|
| Pins | one table, one version | many tables, one consistent position |
| Appears as | a table in the catalogue | a name a query quotes |
| Read by | its own name | any table's name, *as of* it |
| Cost | one log with no files | one small document |
| Dropping it | a schema change | not |

> **Key idea** — A clone freezes a thing. A snapshot freezes a *moment*. Somebody who wants to
> compare two quarters wants clones; somebody who wants four tables to agree wants a snapshot.

## Decision 2 — A table created after the snapshot is refused, never answered as empty

The question with no obvious answer. A snapshot taken on Monday names the tables that existed on
Monday. A query as of it that names a table created on Tuesday could be answered three ways:
with no rows, with the table's current contents, or with a refusal.

**It is refused, naming the table and the snapshot.**

*"No rows"* is the dangerous one and it is the one most systems choose, because it is the easiest
to implement and it never fails. It is wrong because **a table that did not exist is not a table
that was empty**. A query joining orders to a rates table created after the snapshot would return
the rows that survive an inner join with nothing — that is to say, none — and report success. The
answer is not merely incomplete; it is confidently zero.

*"Current contents"* is worse: it silently mixes one instant with another, which is the exact
defect a snapshot exists to prevent, arriving through the mechanism meant to prevent it.

So the read refuses, and says which table and which snapshot, because the person reading the
refusal has a real choice to make: take a newer snapshot, or exclude the table.

> **Pitfall** — This means a snapshot ages: a query that worked in March may refuse in June
> because the schema grew a table. That is a feature. The alternative is a query whose meaning
> quietly changes as the warehouse does.

## Decision 3 — A snapshot has a mandatory expiry

A snapshot pins files. That is what makes it useful and what makes it dangerous: an unexpiring
snapshot holds a whole warehouse's worth of versions alive forever, and `RSK-35` — the
accumulation nobody is responsible for — arrives at warehouse scale rather than at table scale.

So an expiry is **required at the moment of taking**, not defaulted:

```sql
CREATE SNAPSHOT eod_2026_09_02 EXPIRE AFTER 90 DAYS
```

There is no unbounded form. `EXPIRE NEVER` is not spelled, because a snapshot that never expires
is a decision nobody will revisit and the storage cost falls on somebody who did not make it.

This is the third mechanism in this system to carry a mandatory lifetime, after the quarantine
([ADR-0018](0018-a-record-that-does-not-fit.md) Decision 3) and the ephemeral cube. The pattern
is deliberate: **anything that keeps data alive on somebody's behalf must say for how long.**

### A read after expiry refuses, naming the date

Never a short answer, and never a silent fall back to *now*. The refusal says the snapshot
expired and when — which is what turns *"my report changed"* into *"my snapshot expired on
Tuesday"*.

Expiry detaches the pins; it does not delete anything. Retirement reclaims the files afterwards
on its own grace period, exactly as quarantine expiry works, so an expiry that should not have
happened is reversible for as long as that period lasts.

## Decision 4 — A snapshot is not a permission cache

A snapshot records the tables the taker could read at the moment of taking. It does **not** grant
anything at read time.

Every read as of a snapshot is authorized *then*, against the reader's own entitlements — so a
snapshot taken by an administrator does not become a way for anybody else to read what the
administrator could. This is [ADR-0017](0017-the-client-contract.md) Decision 6 (*the session is
a permission's context, never its cache*) and [ADR-0008](0008-serving-cubes-under-policy.md)'s
rule (*serve under policy, keyed by entitlement rather than by person*) applied to a stored
position.

It follows that two people reading the same snapshot may legitimately get different answers, and
that is correct rather than a bug: they are reading the same instant through different
entitlements.

### What a snapshot may name

Only tables the taker could read. A snapshot that recorded a table its taker could not read would
disclose that table's **existence** to everyone who can list snapshots — the leak this system
refuses everywhere else, arriving through a bookkeeping document.

## Decision 5 — Where a snapshot lives, and how the sweeper learns of it

A document per snapshot under the warehouse's `_snapshots/` bookkeeping schema, beside `_cubes/`.
Durable, because a snapshot that died with the process would be useless for the overnight run it
exists for; and in the warehouse rather than beside it, because a backup that copied the tables
and not the snapshots would restore a warehouse whose reports cannot be reproduced.

The sweeper already asks `Lineages::pinned_versions` which versions a clone still reads.
Snapshots become a second source of pins, answered by the same question and unioned with the
first. A file is reclaimable when **no** clone and **no** live snapshot names it.

> **Key idea** — Reclamation already has exactly one question — *"does anything still read
> this?"* — and this adds a second thing that can answer yes. It does not add a second
> reclamation rule, and it must not: two rules disagree eventually, and the one that loses
> deletes a file somebody is reading.

## Decision 6 — Quoting a snapshot is a session setting, and it may not be ignored

A run reads as of one instant across *many statements*, not one. So the primary form is a
session setting:

```sql
SET SNAPSHOT = 'eod_2026_09_02';
-- every statement from here reads as of that instant
SHOW SNAPSHOT;
RESET SNAPSHOT;
```

A per-statement form (`... AS OF SNAPSHOT x`) is deliberately **not** decided here. It is
plausible and it is not needed by the case that motivated this, and a second way to say the same
thing is a second thing to keep consistent.

### The trap this must not fall into

`SET` is currently accepted and ignored — deliberately, because nothing reads a session setting
and a driver sends several. **`SET SNAPSHOT` is the first setting that changes an answer**, and
accepting it as a no-op would be the worst defect this system can have: a caller who asked for
one instant and was silently served *now*, with no symptom.

So the implementation must treat this as a special case of the rule already written down: a
statement whose meaning is not implemented is refused, never confirmed and discarded (`DEC-47`).
Until `SET SNAPSHOT` is honoured it must be **refused**; the day it is honoured, the generic
`SET` no-op must exclude it explicitly rather than by luck of ordering.

## Consequences, stated as costs

1. **Storage is held by a name somebody typed.** A snapshot with a 90-day expiry holds 90 days
   of versions of every table it names. That is the price of reproducibility and it should be
   visible: `SHOW SNAPSHOTS` reports what each one pins, so the cost has an owner.
2. **A query as of an old snapshot can refuse for a reason unrelated to the query** — a table
   appeared. That is Decision 2 working, and it will surprise somebody.
3. **The sweeper does more work per pass**, reading the snapshot documents as well as the
   lineages. Both are small and both are already read per maintenance tick.
4. **Two readers of one snapshot may see different rows.** Correct, and it will be reported as a
   bug at least once.

## Decision 7 — A version is readable and comparable, but only where something kept it

**Added 2026-09-02 by owner directive**, after the question *"is this a git-like view of history?"*
--- to which the honest answer is: a snapshot is a **tag**, not a log.

Two surfaces follow from that, and one deliberately does not.

**`SHOW HISTORY OF <table>`** lists the commits a table's log holds: version, when, and what
each added or removed. Nearly free --- the log already keeps every commit as its own file --- and
it is what makes pinning legible. Without it a person cannot see which versions exist in order
to reason about which to keep.

Two of its columns are decisions rather than data.

`changed_data` reports the **writer's own declaration** --- `dataChange` on every add and remove
--- and not an inference from the file counts. A compaction rewrites files and changes not one
row. Building this found that the two halves of a compaction disagreed: the removals declared
`dataChange: false`, correctly and since they were written, and the *addition* declared `true`.
So a compaction was a data change in one direction and not the other, invisible until something
printed it. A column that reports maintenance as a change is worse than no column, because it
trains a reader to ignore it.

`kept_by` names **the snapshots and clones** keeping each version alive, by name and all of
them. Not the *kind* --- a first implementation printed the word `snapshot`, which is true and
useless: the person reading this column is deciding what to drop to release the storage, and on
a warehouse with a dozen snapshots that answer sends them elsewhere to find out which. It also
omitted clones entirely, though a clone keeps a version alive in exactly the same way, and
reclamation asks one question that both answer yes to.

**`SET VERSION OF <table> = <n>`** reads a table at a version directly, without a snapshot ---
per table and per session, spelled as a setting for the same reason `SET SNAPSHOT` is.
Mechanically it is the same resolution a snapshot uses.

**It must refuse rather than answer short, in two directions.**

The log surviving is not the same as the data surviving: retirement deletes the files a merge
replaced once nothing references them, so an *untagged* old version resolves to a file list
naming files that are gone. That read is refused, and the refusal says the version's files were
reclaimed and that only pinned points survive --- because a partial answer here would be a
historical query silently missing whatever had been compacted. It is the wrong answer that looks
most like a right one, because it has rows in it.

And a version the log does not contain is refused too, which was not obvious until it was
wrong. Replaying a log stops at its end, so asking for version 9999 of a five-version table
resolved to version 5 and answered --- a version nobody has, served as though they had it. The
refusal names the newest the table does have.

**A diff between two versions is not decided here.** *(Owner directive 2026-09-02: deferred.)* It
is a genuine design question rather than an implementation detail --- the log records file-level
adds and removes, so what a *row-level* difference means over it needs an answer before it needs
code. It has its own milestone.

> The rule this states plainly, and the one a user must understand: **history is readable only
> where something is keeping it alive.** A snapshot and a clone are the two things that do.

## What this does not decide

- **A per-statement `AS OF SNAPSHOT`.** Plausible; not needed yet.
- **Snapshots of a graph epoch or a cube's materialised cuboids.** A cuboid is keyed by snapshot
  already, so the mechanism may extend cleanly — but *may* is not a decision.
- **Taking a snapshot automatically**, on a schedule. It is the obvious next request and it needs
  its own answer about who owns the expiry of something nobody typed.
- **Whether a snapshot can be exported**, so that two deployments read the same instant. That is
  a distributed-consistency question and `M12` owns those.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
