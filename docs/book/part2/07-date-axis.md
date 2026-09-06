# 7. The date axis

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> Every table in SANKHYA carries one column, `sank_data_date`, of type `DATE`, and is
> partitioned on it. This chapter argues that a single guaranteed time axis is what makes
> partitioning, retention, compaction and tiering write-once instead of per-table, and that the
> column's *meaning* must be declared per table rather than defaulted per row — because a
> per-row fallback produces a column whose semantics vary silently within one table, and no
> query can separate the two meanings afterwards. It also gives the arithmetic of partition
> fan-out, which is what the granularity setting exists to control, and names the one path
> that is still unpartitioned.

## 7.1 Three capabilities, one missing thing

Three requirements were blocked on the same absence, and each was individually solvable in a way
that would have been wrong.

| Blocked capability | Why it was blocked |
|---|---|
| Partitioning | A table with no agreed time column has no partition key, so every policy is written per table against whatever column that table happens to have |
| Time-based retention and purge | Retention must be a metadata operation rather than a bulk delete, which requires partitions aligned to time |
| Hot/cold tiering | The key-range dimension is, for almost every real table, a date |

Writing each per table means writing it many times, differently, and being wrong somewhere. A
single guaranteed axis makes the maintenance scheduler, compaction, purge and tiering
write-once. [ADR-0004](../../adr/0004-the-date-axis.md) is the decision.

## 7.2 The decision

**Every table carries `sank_data_date`, of type `DATE`, and is partitioned on it.** There is no
exemption for size or for purpose. `sank_` becomes a reserved column-name prefix, and a source
column already so named is a collision refused at onboarding rather than silently shadowed.

## 7.3 Why `DATE`, and not an encoded integer

The obvious alternatives are an `INT` holding `yyyymmdd` and a `BIGINT` epoch. The requirement
that external engines read these tables directly settles it, and it is worth seeing the
comparison in full because the integer form is a common choice and its costs are not obvious.

| Consideration | `DATE` (Arrow `Date32`) | `INT` as `yyyymmdd` | `BIGINT` epoch |
|---|---|---|---|
| Size | 4 bytes | 4 bytes | 8 bytes |
| Partition path | `sank_data_date=2024-03-01` — the Hive convention Spark and Trino parse as a date | `=20240301` — a string to them; every pruning query must know the encoding | `=1709251200` — unreadable |
| Arithmetic | `d - INTERVAL '7 days'` works | `20240301 - 7 = 20240294`, which is not a date, raises no error, and users will write it | works, but in seconds |
| Timezone | none, which is correct for a partition key | none | invites one |
| Precision it implies | a day, which is what it is | a day | a second, which it is not — and invites storing timestamps, silently destroying the partition granularity |

The row that decides it is the third. `20240301 - 7 = 20240294` is a plausible-looking
expression that produces a value which is not a date, raises no error anywhere in the stack, and
will be written by somebody. That is exactly the class of defect this system is organised
against: an answer that is wrong and looks right.

`Date32` also already round-trips exactly through the type mapping, so nothing new has to become
representable.

## 7.4 The value is declared per table, never defaulted per row

This is the part of the design that changed during its own review, and it is the part worth
understanding.

The obvious design is "use the supplied value, else today". **It is rejected.**

A default taken from write time makes the column's meaning vary per row without saying so. A
backfill of last year's data lands in today's partition — exactly wrong for retention and for
every time-bounded query. A replay partitions differently from the original run. Two nodes with
clock skew disagree.

None of those is the damaging consequence. The damaging consequence is that

```sql
WHERE sank_data_date = '2024-03-01'
```

then returns a **mixture** of rows meaning *this happened that day* and rows meaning *we
received this that day* — and no query can separate them afterwards, because the distinction was
never recorded.

So provenance is a property of the **table**, declared once:

| Declaration | Meaning | A null in the source |
|---|---|---|
| `sank.dataDate.source = <column>` (`dated_by("order_date")` in the publish API) | Every row's date comes from that column | **An error.** Not a fallback to today, which would reintroduce the mixture one row at a time |
| absent | Every row's date is the ingest date, **and the table records that it is** | not applicable |

A reader can therefore always discover what the column means for a given table, which is exactly
the property the rejected design lacked.

> **Key idea**
> The rule is not "record the date accurately". It is: *make the column's semantics a fact about
> the table, discoverable by anyone who reads it, and refuse the one operation — a per-row
> fallback — that would make the semantics vary within a column.* A mixture cannot be
> un-mixed, so the enforcement must be at write time or not at all.

The same rule reaches the declared feeds of Chapter 9, where `date:` is a required key: either
`ingest`, or `{ column: booked_on }` on a column that must be a date and must not be null.
There is no third option, and a feed configuration that omits it is refused at validation.

> **Pitfall**
> The pressure to relax this arrives from the loader, and it arrives reasonably: a backfill that
> does not know what date its data is about is refused, and somebody has to go and find out. That
> is a real imposition and it is the point. The alternative is silently misfiled data, and the
> cost is paid later by a person who cannot tell it happened.

## 7.5 Granularity, and the arithmetic of fan-out

A partition key with a fixed granularity is a small-file generator for low-volume tables. The
arithmetic is unforgiving.

A table partitioned daily produces at least one file per day it receives data. For a
low-volume table that is 365 files a year of a few kilobytes each — the small-file problem
compaction exists to fix, created on purpose by the partitioning scheme.

Worse, the fan-out is per *batch*, not per day:

| Batch | Days it spans | Files written | Rows per file |
|---|---|---|---|
| 200,000 rows | 90 | 90 | ~2,222 |
| 5,000 rows | 90 | 90 | ~55 |

A 5,000-row append spread over ninety days becomes ninety files of fifty-five rows. This is not
hypothetical: a measured run produced **32,279 live files across ten tables in four minutes,
averaging 37 KB each, against a compaction policy that targets 256 MB**. That is four orders of
magnitude below target, and it is the ordinary consequence of correct partitioning meeting a
wide batch.

Two mechanisms answer it.

**Declarable granularity.** `sank.dataDate.granularity` is `day` (the default), `month` or
`year`. A monthly table divides the file count by roughly thirty. An unrecognised value is
refused rather than defaulted, because a monthly table silently becoming daily is repartitioned
on its next write — a full rewrite for a typo.

**A fan-out guard on the writer.** A batch touching many partitions must not write one tiny file
per partition; the requirement is explicit, and the guard is what stops the ninety-files-of-
fifty-five-rows case from being the normal outcome. Compaction then converges what remains,
and Chapter 6 covers its planning and its budget.

## 7.6 Managed tables get the column; attached tables are not altered

The system supports attaching to a PostgreSQL cluster somebody else manages, and time
partitioning is scoped to tables *owned by* this system. `ALTER TABLE` on a customer's schema
breaks `INSERT` without column lists, changes `SELECT *`, touches their ORM mappings, and may
exceed the privileges granted.

So:

- **Managed tables** created by this system carry the column, with the partition.
- **Attached tables** are not altered. The column is derived during ingest and exists on the
  analytical side, which is where partitioning happens anyway.

An external publisher must supply the column or accept ingest-date semantics explicitly, and the
publishing library makes that unavoidable rather than documented.

## 7.7 A partition column must be in three places

This is the mechanical part, and it is where a partitioned table most often turns out not to be
one. Delta requires a partition column to appear in the **schema**, in the **path**, and in the
**add action's** partition values. A column present in only one of them reads as **null for
every row** in Spark and Trino, and prunes nothing.

That failure was real here. On 2026-08-27 every table produced by this system was malformed with
respect to Delta partitioning: the log declared `partitionColumns: ["sank_data_date"]`, the
files were written flat, `partitionValues` was `{}`, and the schema did not contain the column.
Each half was individually plausible. The composite was a table that an external engine reads as
having a null partition column for every row — which is precisely the open-storage claim
failing, silently, in the one place nobody was looking.

Two invariants now hold the property, and both are enforced rather than documented:

- A partition column is in the schema, the path, and the add action.
- A batch spanning partitions becomes several files in **one commit**, because a reader must
  never see half a batch.

> **Pitfall**
> The reason this survived so long is instructive and general: *the writer's own reader shared
> the misunderstanding*. A hand-written Delta log that omitted a non-nullable field was accepted
> happily by this crate's own reader. Two implementations agreeing is worth nothing when one of
> them wrote both sides. The claim is now exercised by the Delta kernel — a reader this project
> did not write — which is the only version of the test that means anything.

## 7.8 What the axis buys downstream

With one guaranteed, correctly-materialised time column, four subsystems become write-once
rather than per-table.

| Subsystem | What it gets |
|---|---|
| Pruning | A predicate on `sank_data_date` eliminates whole directories before any file statistics are consulted |
| Retention | Ageing data out is a partition detach — a metadata operation — rather than a bulk delete |
| Quarantine expiry | Built as partition detach rather than row deletion, so it stays reversible until retirement's grace period runs |
| Tiering | The key-range dimension for hot/cold placement already exists on every table |
| Cubes | A time dimension exists before anybody declares one |

The quarantine case is the clearest illustration that the axis is load-bearing rather than
tidy. A quarantine table's records must expire; expiry by row deletion would need a delete path
against published data, which the immutability argument forbids. Because the quarantine carries
`sank_data_date` like everything else, its expiry is a detach of old partitions, using
machinery that already exists and that stays reversible for a grace period.

## 7.9 What is not partitioned

The batch publish path is partitioned: every published table writes `sank_data_date=YYYY-MM-DD/`
directories and the log's add paths carry them.

**The streaming arrival path is not.** The ingest crate creates tables with no partition
columns and writes flat, which means the partitioning requirement is met on the batch publish
path and *not* on the path where most data lands during continuous capture. This is sized and
not started.

There is a second, sharper gap behind it: there is no timestamp on an ingested row to derive a
date from. A system column is declared on every ingested table and written as the literal `0`
for every row. Its comment says the value is recorded for human reading only, which it is not —
it is recorded for nothing.

Both are stated here rather than in a footnote because a reader building on continuous capture
will meet them, and because this book's rule is that a designed-but-unbuilt capability is named
where it is described.

## 7.10 Revisiting

The decision is revisited if a table needs **two** time axes — a business date and an arrival
date, both partitioned. The current answer is that the second is an ordinary column and only one
axis is the partition key. If that stops being enough, the declaration becomes a list rather than
a column, which is a schema change to the declaration rather than to the tables.
