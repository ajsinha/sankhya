<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# SANKHYA — a guide, by example

**Status:** Implementation — M0–M5 complete, M6 in progress

Every example here is **executed by a test**. `crates/sankhya-server/tests/guide.rs` runs the
SQL on this page and checks the answers, so an example that stops working breaks the build
rather than misleading a reader. Where an example needs something that does not exist yet,
it says so instead of pretending.

The [quickstart](QUICKSTART.md) gets a server running. This shows what to do with it.

---

## Contents

1. [Connecting](#1-connecting)
2. [Tables and where they come from](#2-tables-and-where-they-come-from)
3. [Publishing a table](#3-publishing-a-table)
4. [The date axis](#4-the-date-axis)
5. [Vectors and matrices](#5-vectors-and-matrices)
6. [Statistics and calculus](#6-statistics-and-calculus)
7. [Graph traversal from SQL](#7-graph-traversal-from-sql)
8. [Security, and what it refuses](#8-security-and-what-it-refuses)
9. [Verifying and repairing a table](#9-verifying-and-repairing-a-table)
10. [The diagnostic](#10-the-diagnostic)
11. [Metrics, and what a failure tells you](#11-metrics-and-what-a-failure-tells-you)
12. [Backups, and proving one](#12-backups-and-proving-one)
13. [What is not built](#13-what-is-not-built)

---

## 1. Connecting

SANKHYA speaks the PostgreSQL wire protocol, so anything that talks to PostgreSQL talks to
it. No driver, no shim.

```bash
SANKHYA_NO_PASSWORD=1 \
SANKHYA_WAREHOUSE=./warehouse \
SANKHYA_LISTEN=127.0.0.1:5433 \
  ./target/release/sankhya-server
```

```console
$ psql -h 127.0.0.1 -p 5433 -U you -d acme -c "SELECT version();"
                                  version
----------------------------------------------------------------------------
 PostgreSQL 17.0 (SANKHYA 0.1.0) on wire-protocol-compatible unified engine
```

The version string begins `PostgreSQL 17.0` because **every client parses the major version
out of it before it will proceed**, and then says what this actually is so the prefix does
not mislead anyone reading it.

`SANKHYA_NO_PASSWORD` is spelled as an opt-*out*, so the insecure choice has to be made
deliberately — and the startup line prints `NO AUTHENTICATION` in capitals when it is in
force, so an operator sees it rather than having to check.

### Catalogue queries

A reporting tool's first act after connecting is a burst of catalogue queries, and it makes
decisions from the answers before the user has typed anything. Those work:

```sql
SHOW server_version_num;                       -- 170000
SELECT schema_name FROM information_schema.schemata;
SELECT * FROM information_schema.tables;
\dt
\d sales.orders
```

Both spellings of the same question are recognised — `information_schema` (what JDBC uses)
and `pg_catalog` (what `psql`'s `\d` uses) — because matching only one works for the client
it was written against and fails for the next.

---

## 2. Tables and where they come from

A table is a directory of Parquet files with a `_delta_log`, under
`<warehouse>/<schema>/<table>/`. The server walks the warehouse at startup and reads each
table's schema **out of its own log** — not from a Parquet footer, because a table with no
files yet has no footer and one whose files predate a column would produce a schema missing
it.

```console
$ find warehouse -type f | head
warehouse/sales/orders/part-0000.parquet
warehouse/sales/orders/part-0001.parquet
warehouse/sales/orders/_delta_log/00000000000000000000.json
warehouse/sales/orders/_delta_log/00000000000000000001.json
```

```sql
SELECT region, count(*) AS n, round(sum(amount)) AS total
FROM orders
GROUP BY region
ORDER BY region;
```

```
 region |  n  | total
--------+-----+--------
 north  | 334 | 250250
 south  | 333 | 249251
        | 333 | 249750
```

The third row's region is genuinely **null**, not an empty string. That distinction survives
from the Parquet page, through the Arrow array, to the wire — where it becomes a length
of −1 rather than a length of 0.

### Two classes of table

| Class | System of record | Read modes |
|---|---|---|
| **Managed** | The transactional store | Strong, bounded-freshness, pinned |
| **External** | The published tier itself | Bounded-freshness, pinned |

A table published directly by an external writer has **no transactional tier**, so a
strongly-consistent read of one is refused by name rather than served from published data —
which would assert a currency the table cannot offer, with nothing in the result to say so.

The class lives in the table's own log, and **absence means external**. A directory somebody
dropped Parquet into is not managed by this system, and defaulting the other way would have
it claim a tier it does not have. See [ARCHITECTURE §5.6](ARCHITECTURE.md).

---

## 3. Publishing a table

External systems publish through **this system's library**, not by assembling the format
themselves. The format stays open and documented — external engines read it directly — but
the *supported* write path is the library, and the reason is asymmetry:

> A reader that misunderstands the format is wrong for itself, recoverably. A writer that
> misunderstands it corrupts the table for everyone, permanently, and undetectably — because
> the writer's own reader shares the misunderstanding.

This system has direct evidence. Writing its own format with the specification open, it
omitted a non-nullable field from every `add` action; its own reader accepted the result
happily and an independent implementation rejected it on the first read.

```rust
use sankhya_publish::publish::{publish_table, Publication};
use sankhya_schema::Granularity;

let publication = Publication::external(&root, "orders")
    .dated_by("order_date")                    // where each row's date comes from
    .partitioned_by(Granularity::Day)          // how coarsely it partitions
    .keyed_by(["order_id"]);                   // makes it mutable; omit for append-only

publish_table(&publication, &schema, &batches)?;
```

There is no way to call it that produces a file without statistics, a schema that does not
round-trip, or an action missing a field the format requires.

---

## 4. The date axis

Every table carries **`sank_data_date`**, of type `DATE`, and is partitioned on it. That one
guaranteed column is what makes partitioning, time-based retention and hot/cold tiering
possible to write once rather than per table.

**The type is `DATE` and not an encoded integer.** Partition paths are
`sank_data_date=2024-03-01`, which Spark and Trino parse as a date natively; an integer is a
string they must be told about. And `20240301 - 7 = 20240294` is not a date, raises no
error, and is a thing people write.

**The value is declared per table, never defaulted per row.** This is the part worth
understanding:

| Declaration | Meaning | A null in the source |
|---|---|---|
| `dated_by("order_date")` | Every row's date comes from that column | **An error** |
| omitted | Every row uses the ingest date, **and the table records that it does** | — |

A per-row fallback to "today" would make the column mean *when it happened* in some rows and
*when we received it* in others, in the same table, with nothing recording which. Then
`WHERE sank_data_date = '2024-03-01'` returns a mixture that no query can separate
afterwards. See [ADR-0004](adr/0004-the-date-axis.md).

Granularity is `day`, `month` or `year`. An unrecognised value is refused rather than
defaulted — a monthly table silently becoming daily is repartitioned on its next write,
which is a full rewrite for a typo.

---

## 5. Vectors and matrices

A column can hold a vector per row — an embedding, a factor vector, a window of readings —
stored as `FixedSizeList<Float64, N>`.

### Building them in SQL

```sql
SELECT vec_of(1.0, 2.0, 3.0);                       -- a 3-element vector
SELECT mat_of(2, 2, 1.0, 2.0, 3.0, 4.0);            -- a 2x2 matrix, row-major
SELECT mat_identity(3);                             -- the 3x3 identity
```

A matrix's shape is part of its **type**, so `mat_of`'s dimensions must be literals, and a
wrong element count is refused **when the query is planned** — not partway through a scan,
after work has been done.

```sql
SELECT mat_of(2, 3, 1.0, 2.0, 3.0, 4.0, 5.0);
-- ERROR: mat_of(2, 3, …) needs 6 values and was given 5.
--        Refusing at planning time rather than partway through the scan
```

### Vector maths

```sql
SELECT vec_dot(a, b),
       vec_euclidean(a, b),
       vec_cosine_similarity(a, b),
       vec_norm_l2(a)
FROM pairs;
```

A similarity search is an ordinary `ORDER BY`:

```sql
SELECT title, vec_cosine_similarity(embedding, vec_of(0.1, 0.4, 0.9)) AS score
FROM documents
ORDER BY score DESC
LIMIT 10;
```

### Linear algebra

```sql
SELECT mat_determinant(covariance),
       mat_trace(covariance)
FROM portfolios;

SELECT mat_solve(coefficients, observations) FROM systems;
SELECT mat_multiply(a, b) FROM pairs;
```

Matrix-returning functions carry their own shape, so these **compose**:

```sql
SELECT mat_determinant(mat_multiply(a, b)) FROM pairs;
```

### The property that matters

**Every reducing kernel is bit-deterministic.** A dot product is a floating-point sum, and a
sum whose order depends on how the query was partitioned returns a different number when the
machine is busier or has more cores. The difference is small — *too small to notice and too
large to reconcile*.

So these go through the same compensated, order-fixed summation the rest of the system uses.
Two runs of the same ranking produce the same order, not merely a similar one. That is also
why this does not delegate to a numeric library: reordering freely for speed is what a good
one does, and it is exactly what cannot be permitted here. See
[ADR-0005](adr/0005-array-columns-and-numeric-kernels.md).

### Two costs, stated as costs

- **An array column cannot be pruned.** A minimum and maximum of a vector prune nothing, so a
  table of embeddings prunes on `sank_data_date` and its scalar columns only.
- **An array cannot be a key column.** Array equality as row identity is refused rather than
  supported badly.

---

## 6. Statistics and calculus

These describe the series **inside one row** — a window of readings, a term structure, a
factor path. That is distinct from SQL's `stddev(x)`, which describes a *column* across rows.

```sql
SELECT vec_mean(readings),
       vec_stddev(readings),
       vec_median(readings),
       vec_skewness(readings),
       vec_kurtosis(readings)
FROM sensors;

SELECT vec_correlation(a, b) FROM series;
SELECT vec_integral(curve) FROM samples;      -- trapezoidal, unit spacing
```

Two behaviours worth knowing:

**Variance is computed in two passes.** The textbook one-pass identity `E[x²] − E[x]²` is
algebraically correct and numerically disastrous: for values with a large mean and small
spread it subtracts two nearly equal large numbers, and cancellation can produce a
**negative variance** — which every downstream square root turns into a NaN.

**Kurtosis is excess**, so a normal distribution reads zero. Reporting raw kurtosis is a
common and confusing choice: a reader seeing 3.0 cannot tell whether it means "normal" or
"quite heavy-tailed" without knowing the convention, and both are plausible.

**A correlation against a constant series is refused**, not reported as zero. Zero would say
*unrelated*; the truth is *undefined*, and a ranked correlation table would show a constant
column as genuinely uncorrelated rather than as unanswerable.

---

## 7. Graph traversal from SQL

The graph tier holds **no durable state**. An epoch is built by scanning published tables,
carries the snapshot it came from, and is dropped on shutdown. There is no graph write path,
so the graph cannot disagree with SQL: an edge exists because a row exists.

```sql
SELECT p.label, r.depth
FROM graph_reachable('payments', 'acct-1', 'max_depth=3') AS r
JOIN parties AS p ON p.key = r.vertex
WHERE NOT r.truncated;
```

Five functions: `graph_reachable`, `graph_time_respecting`, `graph_shortest_path`,
`graph_cycles`, `graph_influence`.

### Time-respecting traversal is a separate function, not a flag

Static reachability over a temporal graph **over-reports** — it finds routes that time
forbids — and always in that direction. A flag defaulting to off would hand the optimistic
answer to everyone who forgot it, and the optimistic answer looks exactly like the correct
one.

```sql
-- Only routes that could have been used in order, with value conservation and dwell bounds
SELECT * FROM graph_time_respecting('payments', 'acct-1',
    'max_depth=6, min_conservation=0.9, max_dwell=86400000000');
```

`min_conservation=0.9` requires each onward edge to carry at least nine tenths of the one
before. Without it, a large edge chains onto a negligible one and the result is called a
route. `max_dwell` bounds how long a path may pause at a vertex — without an upper bound,
two unrelated events years apart join into one path.

### Every row carries its provenance

`epoch`, `snapshot`, `truncated` and `truncation_reason` are **columns**, not query metadata.
A flag beside the result gets dropped by the first projection that does not mention it, and a
short list looks exactly like a short answer.

### Named arguments are an options string

`name => value` is rejected outright by the SQL planner for table functions, and
`name = value` is resolved as a *column* against an empty schema. Only literals reach a
table function, so bounds arrive as `'max_depth=3, min_conservation=0.9'` — with every key
checked against a known set, so a misspelled bound is refused rather than silently taking
its default.

---

## 7a. Arrow Flight SQL — the bulk plane

The wire protocol is a **row** protocol: the last step of every query takes columnar batches
apart one value at a time. For an interactive query that costs nothing worth measuring; for
a bulk extract it is the whole cost. Flight SQL does not do that — the client's Arrow buffers
are the same shape as the server's.

```rust
let info = client.get_flight_info(descriptor_for("SELECT id, label FROM orders")).await?;
let ticket = info.endpoint[0].ticket.clone().expect("a ticket");
let batches: Vec<RecordBatch> = FlightRecordBatchStream::new_from_flight_data(
    client.do_get(ticket).await?.into_inner().map_err(FlightError::from)
).try_collect().await?;
```

**Nothing is materialised.** A batch is encoded as it is produced and its memory released as
soon as it is sent, which `FR-API-07` requires. The consequence is a real behaviour change:
**an error can arrive mid-stream.** A row protocol sends its error before the first row or
not at all; this one may have sent a gigabyte first. It reports the failure on the stream
rather than closing quietly, because a truncated stream that ends cleanly is
indistinguishable from a complete one.

**The ticket is a security boundary.** Flight splits a query into planning and redemption,
and the principal redeeming is not necessarily the one who requested. So the decision is made
once, at `GetFlightInfo`, and the ticket carries its outcome — redeeming does not re-plan and
does not re-authorize. It checks only that the presenter is the tenant it was issued to, and
the refusal does not say whose ticket it is.

Tickets expire after five minutes: a ticket names a snapshot, and a snapshot's files are
eventually retired, so an unbounded one is a lease nobody granted.

**Deliberately absent**: `DoPut`, prepared statements, transactions, `DoExchange`. Each
returns `UNIMPLEMENTED` with a reason rather than working differently than it should — and
`DoPut` names which write path to use instead, because a third one with its own semantics
would be a way for the other two to disagree.

See [ADR-0006](adr/0006-flight-sql.md).

---

## 8. Security, and what it refuses

A statement is authorised **before** a table is registered in the session. A table the caller
may not read is therefore never present, so naming it fails to resolve:

```sql
SELECT * FROM salaries;
-- ERROR: table 'datafusion.public.salaries' not found
```

That is deliberate and not an accident. "You may not read that" would **confirm the table
exists**, and the difference between it and "no such table" is a working enumeration oracle.

A policy row predicate is enforced where **no provider can decline it**:

```sql
-- With a policy of  region = 'north'  on this table:
SELECT count(*) FROM orders WHERE region = 'south' OR 1 = 1;
-- returns only the northern rows
```

The first implementation handed the predicate to the provider as a pushdown filter, and
`MemTable` *declines* filters — so every row came back and the table was secured in name
only. No error. Correctness now never depends on the provider cooperating.

### The audit records what was seen

Not "a query happened and here is who ran it". The row filter and column masks that were
applied, and the table snapshot **and graph epoch** that answered — because the same query
returns different rows a day later, and a record without a version reproduces nothing.

The chain is hash-linked with SHA-256 and detects alteration, reordering and insertion. It
does **not** detect truncation of the tail — an attacker who removes the tail leaves a chain
that verifies perfectly. Only publishing the head somewhere append-only makes the true length
knowable, and that limitation is asserted by a test rather than left to be discovered.

---

## 9. Verifying and repairing a table

```console
$ sankhya-publish verify ./warehouse/sales/orders
external table, 4 file(s), all with statistics — nothing to report
```

Verification does **not** assume the publishing library was used, because making it the
supported path is a recommendation and a recommendation is not an invariant. It reports
*what* is wrong rather than *whether*:

```console
$ sankhya-publish verify ./warehouse/sales/broken
external table, 4 file(s), 0 with statistics — 4 finding(s), 0 affecting correctness
  [slow] part-0000.parquet cannot be pruned, so every query reads it. …
```

Findings that make queries **slow** and findings that make them **wrong** are distinguished,
and the exit code follows: `0` clean, `1` slow, `2` wrong. A build gate can fail on one and
not the other, because a table that is merely slow can wait until Monday.

### Repair derives; it never guesses

```console
$ sankhya-publish repair ./warehouse/sales/broken            # shows a plan, changes nothing
4 action(s) can be derived, 0 finding(s) need a person
  would recompute statistics for part-0000.parquet by reading it

$ sankhya-publish repair ./warehouse/sales/broken --apply
wrote version 2, repaired 4 file(s), 0 failed — afterwards: nothing to report
```

| Finding | Repairable | Why |
|---|---|---|
| No statistics | **Yes** | Read the file — the file *is* the truth |
| No row count | **Yes** | The Parquet footer records it |
| No schema | **No** | A table with a column added after its files were written would infer a schema missing it |
| Key column absent | **No** | Only a person knows whether it was renamed or was a typo |

A tool that guesses is worse than no tool: it writes a plausible invented value into the
table permanently, with an operator's confidence attached, because a tool said it was fixed.
Nobody re-checks a table a tool reported as repaired.

Three properties make it safe to point at production: **it never deletes**, it **repairs by
appending** so the broken commit stays readable and the repair is revertible, and it **does
nothing by default** — the commonest way to run a repair tool is by accident, on the wrong
directory, at three in the morning.

---

## 10. The diagnostic

```
sankhya-server doctor
```

It reads the warehouse directly and does **not** start the server. That is deliberate: the
day you want a diagnostic is frequently the day the server will not start, and a diagnostic
that needs a healthy server to report an unhealthy one is decoration.

### What it prints

```
SANKHYA doctor 0.1.0
  warehouse /srv/sankhya/warehouse
  1 table(s)

  [warning] table sales.orders — 900 live files; at the current rate, about 1 day.
         Compact it: `sankhya maintenance compact --table sales.orders`. If this recurs,
         the maintenance duty cycle is too low for this table's write rate — raising it is
         the durable fix and compacting by hand is not.

1 check(s) clean, 1 finding(s) of which 1 have a date, 0 check(s) could not run
```

Two things in that line are the whole design.

**"about 1 day", not "900 files."** `FR-OPS-17` asks for the time until a problem becomes
user-visible rather than its current value, on the grounds that *"compaction debt is 400 GB"*
is far less actionable than *"query latency on this table will double in about nine days"*.

**"of which 1 have a date."** Which brings us to the part that surprises people.

### The first run gives you no dates, and says so

A time cannot be computed from one sample. "900 files" and "growing by 100 files a day" are
different kinds of fact and only the second yields a date. So the first run of `doctor` on a
new installation looks like this:

```
  [note] table sales.orders — 990 live files; no projection is possible from 1
         observation(s): a time needs a rate, and a rate needs at least 2.
```

It reports the value, refuses the date, and names what is missing. The alternative — a
projection invented from one sample — is a number with a date attached, and a date is
exactly what gets believed and scheduled around.

**Run it on a schedule.** Hourly from cron is what makes the projections real:

```cron
17 * * * * SANKHYA_WAREHOUSE=/srv/sankhya/warehouse /usr/local/bin/sankhya-server doctor
```

Observations are appended to `.sankhya/diagnostic-history.tsv` beside the warehouse — plain
tab-separated text, so `tail` answers "what did it see last night?" without any tooling. It
is bounded, and it is deliberately *not* a table in the system being diagnosed.

### The four answers, and why there are four

| Answer | Meaning |
|---|---|
| **Already** | Past the threshold now. An incident, not a warning |
| **Crossing** | A date, with a confidence. Two observations give `Weak` and say so in the text; five or more give `Firm` |
| **Receding** | Moving away from the threshold, or flat. **Not reported** — a large number that is shrinking needs no attention, and reporting it teaches an operator to skim |
| **Beyond / Unknown** | It will not say. See below |

It refuses to give a date in four distinct situations, and each refusal names itself:

- **Too few observations.** Fewer than two. The first run, always.
- **Not linear.** The measurements do not follow a line closely enough. A sawtooth — debt
  accumulating and being compacted away — fits a line badly *by construction*, and a date
  drawn through one reports where in the cycle the samples happened to fall.
- **Beyond the horizon.** It crosses on this trend, but further out than the observation
  window supports. Four days of samples projecting six months ahead is arithmetic, not
  evidence. The horizon is three times the observed span.
- **No elapsed time.** Every observation shares an instant.

A measure that is *near* the threshold still speaks up without a date, at `note` severity —
silence at 990 of 1,000 files reads as health, and it is not.

### Findings are ordered by *when*, not by *how bad*

A `note` that becomes an outage tomorrow is printed above a `critical` that has been stable
for a month. Severity orders a list by how loudly each item shouts; time orders it by which
one has to be dealt with first. Reading top-down should be reading a schedule.

### "Could not run" is its own section, and its own exit status

```
Could not run:
  [compaction-debt] table sales.archive: log version 7 is malformed
```

A table nobody could look at and a table that is fine both produce no findings. If they land
in the same empty list, the report says "all clear" about something it never examined.

| Exit | Meaning |
|---|---|
| `0` | Clean |
| `1` | Findings |
| `2` | At least one check could not run |

The third status exists so a monitoring system cannot treat "I could not look" as "nothing
found".

### What it checks today

| Check | Threshold | Status |
|---|---|---|
| `compaction-debt` | 1,000 live files per table | Built |
| `storage-headroom` | free space reaching zero | Built as a check; nothing feeds it observations yet, because reading free space needs a platform call this workspace's `forbid(unsafe_code)` will not permit. The caller passes the number in |
| `replication-lag` | a freshness objective the caller supplies | Built as a check; not yet wired, because nothing in this process advances a replication position |

`FR-OPS-16` lists more — conformance, replica identity, archival consistency. Those are not
built, and [`STATUS.md`](STATUS.md) is the authoritative list.

---

## 11. Metrics, and what a failure tells you

### The scrape endpoint

```bash
curl -s http://127.0.0.1:9464/metrics
```

Its own port (`SANKHYA_METRICS_LISTEN`, default `127.0.0.1:9464`), one route, exact match.
Loopback by default, because a metrics endpoint on every interface is a small permanent
disclosure of the deployment's shape and the safe choice should be the one you get by not
deciding.

```
# HELP sankhya_queries_total Statements that reached execution, by how they ended.
# TYPE sankhya_queries_total counter
sankhya_queries_total{outcome="ok"} 412
sankhya_queries_total{outcome="refused"} 3
sankhya_table_live_files{table="sales.orders"} 87
```

Every metric appears even at zero, so a dashboard can tell **"no events" from "not wired
up"**. The full list is [`METRICS.md`](METRICS.md), which is generated from the declarations
and checked against them on every build.

### `refused` is not `error`

A quota held and a permission enforced are the system working. Counting them alongside
genuine failures makes a healthy system under load look like a broken one — which is how an
error-rate alert comes to fire on correct behaviour. Four outcomes: `ok`, `error`, `refused`,
`cancelled`.

### Labels cannot carry your data

A label is one of exactly two kinds:

- **Closed** — a named set of permitted values. `outcome` is one of four strings; anything
  else is refused and counted. Such a label cannot be handed a customer's name however the
  call site is written.
- **Identifier** — a deployment-scoped name like a table, under a cap. Past the cap new
  series are refused and `sankhya_metrics_rejected_total{reason="over_cap"}` rises. The
  metric goes **incomplete and says so**, rather than growing without bound.

There is no third kind, so a label that varies per row has no way to be declared. That is
`ARCHITECTURE.md` §17.1's tenant-data prohibition made structural rather than left as a
review item.

Watch `sankhya_metrics_rejected_total`. Non-zero means a call site disagrees with the
catalogue, or something has outgrown its cap.

### What a failed query tells you

```
psql> SELECT * FROM sales.ordres;
ERROR:  [SNK-C0001] Error during planning: table 'sales.ordres' not found
DETAIL:  Correct the statement. The detail names the offending element.
```

Three things, and each is doing a job:

| | |
|---|---|
| **`SNK-C0001`** | A permanent code. It is what a support conversation is conducted in and what a runbook is indexed by. Codes never change meaning and are never renumbered |
| **The message** | What happened |
| **`DETAIL`** | What to do about it — the catalogue's own remediation, so the client and [`ERRORS.md`](ERRORS.md) cannot say different things |

The SQLSTATE comes from the error's **class**, not from its wording. Every driver in this
ecosystem branches on those five characters, and a plausible message with the wrong ones
produces a client that connects, appears to work, and mishandles every failure.

The letter after `SNK-` is the class: `C` the caller's request, `R` a limit, `F` a conflict,
`T` transient, `X` cancelled, `S` a fault that pages. Every `S` code has a runbook in
[`runbooks/`](runbooks/).

### A table that does not exist and one you may not read are the same error

Deliberately. Saying "you may not read that" confirms it exists, and existence is frequently
the secret. Only the tables a principal may read are registered, so the engine says "not
found" either way — the same code, the same state, the same words.

### Writes are refused, not accepted and discarded

```
psql> CREATE TABLE public.staging (id BIGINT);
ERROR:  [SNK-C0006] data definition is not served over this connection; this server is
        a read path over a published warehouse
DETAIL:  Write to the transactional store and let capture publish it, or publish an
         external table with `sankhya-publish`. See GUIDE.md §3.
```

This once returned `CREATE TABLE` and did nothing durable — the table existed for the rest of
that connection and vanished on reconnect. The refusal names the supported route, because a
refusal that only says no sends somebody looking for a flag to turn it on, and there is no
flag.

### Exit statuses

`sankhya-server doctor` exits `0` clean, `1` findings, `2` a check could not run. The third
exists so a monitoring system cannot read "I could not look" as "nothing found".

---

## 12. Backups, and proving one

```bash
sankhya-server backup     # record a manifest
sankhya-server drill      # prove it restores
```

### What a backup actually is here

A **manifest**, not an archive. This system does not copy your data somewhere; it binds three
artefacts — the transactional backup you took, the table versions in the warehouse, and the
key generation — to one point, and protects the files so they stay readable.

```
SANKHYA backup 0.1.0
  sales.orders at version 1, 1000 row(s)

  backup:01a04442-936a-73a1-bfd1-964c8cd66330
  queryable at 4821
  manifest /srv/sankhya/.sankhya/backup-manifest.json

This backup is unproven until it has been drilled: `sankhya-server drill`.
```

### Two positions, and they are not the same number

| | |
|---|---|
| `source_restores_to` | Where the transactional store lands |
| `queryable_at` | The highest position at which **every** table is complete |

The second is the minimum over the tables' coverage, because a query joining two tables can
only be answered where both of them reach. Tables publish at their own cadence, so these are
rarely equal, and the gap between them is **how much re-capture a restore implies** before a
cross-table query can reach the source's position.

A backup binds to the second. Recording only the first and calling it "the consistent point"
is the commonest way this goes wrong.

### The manifest refuses to exist rather than record a disagreement

**No table may cover a position past where the source restores to.** If one does, the backup
is refused:

```
refusing to record a backup whose analytical tier is ahead of its source. After restoring
it, 1 table(s) would hold rows the transactional store no longer has; capture would resume
behind them and republish that range at different positions. Not detectable afterwards from
either side alone: sales.items covers to 900 and the source restores to 800
```

Every offending table is named, not just the first — fixing them one at a time means learning
about the next only after another full backup.

### A drill reads the data back

```
SANKHYA restore drill 0.1.0
  backup:01a04442-936a-73a1-bfd1-964c8cd66330
  sales.orders: verified, 1000 row(s)

Proven. 1 table(s) read back and digested.
```

Not a file-presence check. **A presence check passes on a truncated Parquet**, on a file whose
bytes were replaced with another table's, and on essentially every failure that actually
happens — because what goes wrong with a backup is almost never that a file is missing. A
missing file is loud. What goes wrong is that a file is there and wrong.

So the drill recomputes the digest. It is expensive and it is the only version of this that
means anything. And the failure it reports distinguishes two situations that need different
investigations:

| | |
|---|---|
| `expected 1000 row(s) and found 940` | Rows were **lost or duplicated** |
| `the row count matches at 1000 and the data does not` | Rows were **altered** — every file present, right length, wrong contents |

| Exit | Meaning |
|---|---|
| `0` | Proven |
| `1` | A table did not verify |
| `2` | The drill could not run |

**Alert on `2` as well as `1`.** A monitor treating "could not run" as "nothing wrong"
reports a backup as proven when nothing looked at it.

### The evidence keeps the failures

`<data-dir>/restore-drills.jsonl`, append-only:

```
{"at": 1787851608943991, "backup": "backup:01a0…", "verdict": "pass", "tables": 1}
{"at": 1787851616975478, "backup": "backup:01a0…", "verdict": "FAIL", "tables": 1,
 "failures": "sales.orders: could not be read (…part-0000.parquet: Parquet file too small)"}
```

A drill history with no failures in three years describes either a very good system or a
drill that does not really run, and nothing in the history says which. So failures are
written with the same ceremony as passes, and a drill that could not *start* is recorded
distinctly from one that ran and passed.

`doctor` reports the last **pass**, never the last attempt — an operator asking "when did we
last prove we could restore" must not be answered with the time of a failure.

### Deleting a backup does not immediately release its files

Seven days of grace between expiry and removal. The failure it prevents: a backup deleted by
mistake, its files swept before anybody notices, and no way back even if the manifest is
recovered five minutes later.

### What this does not cover

**The transactional half.** This system binds itself to a PostgreSQL backup somebody else
took. It records the location and digest; it does not take one and does not verify one. A
passing drill means the analytical tier restores and says nothing about the source. Proving
that is a separate drill against your database backup tooling.

Full detail in [`runbooks/restore-drill.md`](runbooks/restore-drill.md).

---

## 13. What is not built

Stated explicitly, because a guide that implies more than exists is worse than one that
admits less. [`STATUS.md`](STATUS.md) is the authoritative version.

| | |
|---|---|
| **The gRPC control plane and REST gateway** | Not built. The `/metrics` endpoint is not the beginning of one: one route, no authentication, and nothing that returns rows |
| **Backing up the transactional store** | Not built, and deliberately not planned as this system's job. The manifest binds to a PostgreSQL backup taken by your own tooling |
| **Distributed tracing** | Not built. Metrics and the error catalogue exist; spans do not |
| **Ingest on a timer** | Not built. Capture, apply and publication all work and none of them is driven by a running process, so everything the server serves is already published |
| **Graph hydration on a timer** | Not built. An epoch is built when something builds it |
| **The pack loader in the server** | Not built. Packs load into a registry; nothing in the running process does that |
| **Partitioning, bloom filters, the result cache** | Not built. The date axis and its declaration exist; nothing yet writes partitioned directories |
| **Most of `FR-OPS-16`'s checks** | Not built. `doctor` covers compaction debt end to end; storage headroom and replication lag exist as checks with nothing feeding them |
| **QR, SVD, eigendecomposition** | Deliberately absent. They are where an in-house implementation is worse than none — a subtly wrong SVD produces plausible singular values |

---

## Where to go next

- [`QUICKSTART.md`](QUICKSTART.md) — build it and get a server running
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — why it is shaped this way
- [`STATUS.md`](STATUS.md) — what is built, what is measured, and the defects found along the way
- [`adr/`](adr/) — the decisions, with what each one costs
