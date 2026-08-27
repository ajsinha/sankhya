# ADR-0004 — `sank_data_date`: one date axis on every table

**Status:** Accepted · **Date:** 2026-08-27 · **Milestone:** M5
**Implements:** `FR-OLTP-12` (time partitioning), and unblocks `FR-STORE` partitioning,
retention and the tiering axis of §13
**Owner directive, 2026-08-27**

## Context

Three capabilities are unbuilt and all three are blocked on the same missing thing:

- **Partitioning.** A table with no agreed time column has no partition key, so every
  retention policy has to be written per table against whatever column that table happens
  to have.
- **Time-based retention and purge.** `FR-OLTP-12` requires retention to be a metadata
  operation rather than a bulk delete, which requires partitions aligned to time.
- **The hot/cold tiering axis** (§13), whose key-range dimension is, for almost every real
  table, a date.

Writing each of those per table means writing them many times, differently, and being wrong
somewhere. A single guaranteed axis makes the maintenance scheduler, compaction, purge and
tiering write-once.

## Decision

Every table carries **`sank_data_date`**, of type **`DATE`**.

### The type is `DATE`, not an integer

`CON-08` requires external engines to read these tables directly, and that settles it:

| Consideration | `DATE` (Arrow `Date32`) | `INT` as `yyyymmdd` | `BIGINT` epoch |
|---|---|---|---|
| Size | 4 bytes | 4 bytes | 8 bytes |
| Partition path | `sank_data_date=2024-03-01` — the Hive convention Spark and Trino parse as a date | `=20240301` — a string to them; every pruning query must know the encoding | `=1709251200` — unreadable |
| Arithmetic | `d - INTERVAL '7 days'` works | `20240301 - 7 = 20240294`, which is not a date, raises no error, and users will write it | works, but in seconds |
| Timezone | none, which is correct for a partition key | none | invites one |
| Precision it implies | a day, which is what it is | a day | a second, which it is not — and invites storing timestamps, silently destroying the partition granularity |

`Date32` also already round-trips exactly through this system's type mapping, so nothing
new has to be representable.

### The value is declared per table, never defaulted per row

The obvious design is "use the supplied value, else today". **This is rejected**, and it is
the part of the proposal that changed.

A default taken from write time makes the column's meaning vary per row without saying so.
A backfill of last year's data lands in today's partition — exactly wrong for retention and
for every time-bounded query. A replay partitions differently from the original run. Two
nodes with clock skew disagree.

The damaging consequence is not any of those individually. It is that
`WHERE sank_data_date = '2024-03-01'` then returns a **mixture** of rows meaning "this
happened that day" and rows meaning "we received this that day" — and no query can
separate them afterwards, because the distinction was never recorded.

That is precisely the failure this system is organised against: an answer that is wrong and
looks right.

So the provenance is a property of the **table**, declared once:

| Declaration | Meaning | A null in the source |
|---|---|---|
| `sank.dataDate.source = <column>` | Every row's date comes from that column | **An error.** Not a fallback to today, which would reintroduce the mixture one row at a time |
| absent | Every row's date is the ingest date, **and the table records that it is** | not applicable |

A reader can therefore always discover what the column means for a given table, which is
the property the rejected design lacked.

### Granularity is declarable, defaulting to daily

`FR-CDC-14` already warns that a batch touching many partitions must not write one tiny
file per partition. A low-volume table partitioned daily produces 365 small files a year —
the small-file problem compaction exists to fix, created on purpose.

`sank.dataDate.granularity` is `day` (default), `month` or `year`.

### Managed tables get the column; attached tables do not get altered

`FR-OLTP-12` already scopes time partitioning to tables *owned by* this system, and
`FR-OLTP-04` supports attaching to a PostgreSQL cluster somebody else manages.

`ALTER TABLE` on a customer's schema breaks `INSERT` without column lists, changes
`SELECT *`, touches their ORM mappings and may exceed the privileges granted. So:

- **Managed tables** created by this system carry the column, with the partition.
- **Attached tables** are not altered. The column is derived during ingest and exists on the
  analytical side, which is where partitioning happens anyway.

## Consequences

- Partitioning, retention and tiering acquire a key they can all rely on.
- A backfill must state what date its data is about, and will be refused if it does not.
  That is a real imposition on the loader and it is the point: the alternative is silently
  misfiled data.
- `sank_` becomes a reserved column-name prefix. A source column already so named is a
  collision, refused at onboarding rather than silently shadowed.
- An external publisher must supply the column or accept ingest-date semantics explicitly.
  The publishing library makes this unavoidable rather than documented.

## Revisit if

A table needs two time axes — a business date *and* an arrival date, both partitioned. The
current answer is that the second is an ordinary column and only one axis is the partition
key; if that stops being enough, this becomes a list rather than a column.
