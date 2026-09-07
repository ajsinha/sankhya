<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — the guide

**Document ID:** SNK-GUIDE-001
**Version:** 0.1.0
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

This is the reference document for everything a person does with a running SANKHYA: every
statement, every function, both client surfaces, and — beside each one — what it refuses and
what is not built behind it.

It is a *user's* reference. Running a server is somebody else's job and has its own documents:
[`OPERATIONS.md`](OPERATIONS.md) for configuration, metrics, the diagnostic, maintenance,
backup and the restore drill, and [`SECURITY.md`](SECURITY.md) for the choke point, the policy
vocabulary, authentication and the audit chain. Neither is restated here. Where this document
needs one of their facts it names the section rather than copying it — a copy is a thing that
drifts, and ending that is the whole point of the exercise this rewrite belongs to.

Three more companions. [`QUICKSTART.md`](QUICKSTART.md) builds the binaries and gets a server
running. [`TUTORIALS.md`](TUTORIALS.md) is this material walked step by step, starting from an
empty prompt. [`STATUS.md`](STATUS.md) is the authoritative record of what is built, what is
measured, and what was got wrong on the way.

## How this document is gated, and what that does not cover

`crates/sankhya-server/tests/guide.rs` extracts the fenced `sql` blocks from this page — this
page, not a copy of it — starts a real server against the fixture warehouse and runs what can
run. The rest query tables you would bring yourself, and each is listed in that test with the
reason it cannot run here. A block that is neither executed nor listed fails the build, so an
example cannot quietly become neither. `crates/sankhya-server/tests/book_sql.rs` reads every
markdown file under `docs/` a second time and holds a statement shown as working to a narrower
rule: it may fail only because the object it names is absent.

Two things this paragraph has already got wrong, kept because they are the argument for the
test. It once claimed every example ran, and the file it named did not exist. It then quoted
how many did run, and the figure went stale the next time an example was added — nothing checks
a number in prose, so no number is quoted now.

And the gate does not check English. Every defect this rewrite repaired was a *sentence* that
had stopped being true while its code block still ran: a determinism claim covering kernels
that do not have the property, a refusal that had been reversed, a count of cube navigations
that was never right. Those are named where they sit, in the section a reader meets the feature
in, rather than collected on a page at the end where the person who needs them will not be.

---

## Contents

1. [Connecting, and how a server is configured](#1-connecting-and-how-a-server-is-configured)
2. [Tables, and where they come from](#2-tables-and-where-they-come-from)
3. [Naming a table, cloning one, and lineage](#3-naming-a-table-cloning-one-and-lineage)
4. [The date axis](#4-the-date-axis)
5. [Publishing a table, and declaring a feed](#5-publishing-a-table-and-declaring-a-feed)
6. [Snapshots, history and versions](#6-snapshots-history-and-versions)
7. [Cubes — declare, roll up, slice](#7-cubes--declare-roll-up-slice)
8. [Vectors and matrices](#8-vectors-and-matrices)
9. [Determinism: bit-reproducible is not compensated](#9-determinism-bit-reproducible-is-not-compensated)
10. [The function catalogue](#10-the-function-catalogue)
11. [Graph traversal from SQL](#11-graph-traversal-from-sql)
12. [Security at the query surface](#12-security-at-the-query-surface)
13. [Arrow Flight SQL — the bulk plane](#13-arrow-flight-sql--the-bulk-plane)
14. [The clients: a SQL prompt and the Python binding](#14-the-clients-a-sql-prompt-and-the-python-binding)
15. [Extensions, packs, and an aggregation of your own](#15-extensions-packs-and-an-aggregation-of-your-own)
16. [Verifying and repairing a table](#16-verifying-and-repairing-a-table)
17. [What a failure tells you](#17-what-a-failure-tells-you)
18. [What is not built](#18-what-is-not-built)

Operating a server is [`OPERATIONS.md`](OPERATIONS.md) — configuration, metrics, the
diagnostic, maintenance, backup and the restore drill. Its security half is
[`SECURITY.md`](SECURITY.md). Neither is restated here; where this document needs one of their
facts it names the section rather than copying it, because a copy is a thing that drifts.

---

## 1. Connecting, and how a server is configured

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

**The default port is 5433**, not 5432 — `crates/sankhya-server/src/main.rs:159` reads
`server.listen` with `127.0.0.1:5433` as its fallback, and `config/application.yaml` says the
same. That is deliberate: a SANKHYA and the PostgreSQL it captures from are frequently on one
host, and a default that collided with the source's would make the two indistinguishable in a
connection string. It is worth stating loudly here because the Python binding's own default is
`5432` (`sdk/python/sankhya/client.py:837`), which is a defect and is named again in §14.

### Two doors

| Door | Protocol | What it is for |
|---|---|---|
| Wire protocol | PostgreSQL 3.0, default `127.0.0.1:5433` | Tools nobody wrote for this system: `psql`, a notebook's driver, a BI product |
| Arrow Flight SQL | gRPC, default `127.0.0.1:5434` | The bulk plane — Arrow-native end to end, streamed by construction. See §13 |

A REST/JSON API is deliberately **not** a third door. The engine is columnar and typed; a
row-oriented JSON surface converts twice, loses the type distinctions the storage layer spent
effort preserving — a `Decimal(38,9)` becomes a double or a string, and both are wrong in
different ways — and would need its own pagination, its own error shape and its own
authorization path. That is a second product surface maintained forever to avoid a dependency
the client already has.

Both the **simple** and the **extended** query protocols are served. The extended one — what
most *drivers* use by default — was implemented late, and **no third-party driver has been
tested against it**. JDBC, psycopg, pgx, npgsql and ODBC all use it, and until M6 this server
acknowledged `Parse`, acknowledged `Bind`, answered `Describe` with `NoData` and had no
`Execute` arm at all: three cheerful acknowledgements and then a dead socket. The arm exists
now; the compatibility matrix does not. One difference is known and deliberate — a statement
runs at `Describe` time, earlier than PostgreSQL would run it, because `Describe` needs the
column names and `Execute` needs the rows, and running twice would answer from two different
snapshots.

### Configuring one

Configuration is an operator's document rather than a user's, and it has one:
[`OPERATIONS.md`](OPERATIONS.md) §5 carries the complete `SANKHYA_*` table with each
variable's configuration key, its default and the ways each one surprises people — including
the two that are read directly rather than through a configuration key, and the one whose YAML
spelling is silently ignored. It is deliberately the only copy. A second table in this document
would be a table that drifts, which is the failure the `docs/book/` deletion exists to end.

What a *user* needs from it is the address and the posture, and both are printed once, in
words, on every start:

```
SANKHYA 0.1.0
  tenant tenant:0000…0001, NO AUTHENTICATION — every connection is accepted,
  10 policy rule(s), 10 table(s) known
  listening on 127.0.0.1:5433
  wire protocol unencrypted — passwords cross the network in plain text
  connect with: psql -h 127.0.0.1 -p 5433 -U <user>
  metrics on http://127.0.0.1:9464/metrics
  Arrow Flight SQL on 127.0.0.1:5434
```

**`NO AUTHENTICATION` is in capitals** because `SANKHYA_NO_PASSWORD` is an opt-out and an
operator should see the consequence rather than have to check. **The TLS posture is named in
words**, every time, so a half-configured server cannot be mistaken for an encrypted one. And
**the bound address is printed, not the configured one** — told to bind port 0, this once
printed `:0`, so the line whose only job is to say where to connect said nothing.

A table the server cannot open is named on stderr rather than omitted. A server that starts
with three tables of four and says nothing produces an outage that looks, to whoever queries
it, like a table nobody ever created.

### Over TLS

Both doors present one certificate, configured by file — there is no `SANKHYA_*` variable for
one, so a deployment configured purely by environment runs in the clear.

```console
$ psql "host=127.0.0.1 port=5433 user=you dbname=acme sslmode=require" -c "SELECT 1;"
```

The four postures are `unencrypted`, `TLS offered` (encrypted for clients that ask, plain for
the rest — a state to migrate *through* rather than sit in), `TLS required`, and `TLS required,
and a client certificate with it`. Requiring is the default once a certificate is configured:
an operator who went to the trouble did not do it so a client could decline to use it. A
certificate configured without its key **stops startup**, because a server that fell back to
plain text there would be one whose operator believes it is encrypted.

[`SECURITY.md`](SECURITY.md) is where the postures, the credential store and the identity gap
are set out in full, and it is the copy to trust. The one consequence a *user* cannot route
around: **a connection's principal is a fixed tenant.** Mutual TLS puts a client certificate
where the door can see it and nothing yet derives an identity from it, so nothing you do at a
SQL prompt distinguishes you from anyone else connecting to the same server.

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

Both spellings of the same question are recognised — `information_schema` (what JDBC uses) and
`pg_catalog` (what `psql`'s `\d` uses) — because matching only one works for the client it was
written against and fails for the next.

`psql`'s metacommands, as they behave in this build:

| Command | Behaviour |
|---|---|
| `\dt`, `\dn`, `\conninfo` | Work |
| `\d <table>` | **Fails** — `psql` issues a `pg_class` query whose answer it cannot use: *"column number 3 is out of range 0..2"* |
| `\l`, `\du` | **Fail** — `pg_catalog.pg_database` and `pg_catalog.pg_roles` are not served |
| `\df` | Answers, with the schema list rather than a function list |
| `\dv` | Answers, with every base table rather than the views |

> **Read a catalogue result by column name, never by position.** The catalogue relations answer
> with **their own projection**, not the one you asked for: `SELECT column_name, data_type,
> is_nullable FROM information_schema.columns` returns six columns beginning with
> `table_schema`. This is exactly how the Python binding once returned table names where column
> names belong, and it is the first thing to get right in any new client.

---

## 2. Tables, and where they come from

A table is a directory of Parquet files with a `_delta_log`, under
`<warehouse>/<schema>/<table>/`. The server walks the warehouse at startup and reads each
table's schema **out of its own log** — not from a Parquet footer, because a table with no
files yet has no footer and one whose files predate a column would produce a schema missing it.

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
from the Parquet page, through the Arrow array, to the wire — where it becomes a length of −1
rather than a length of 0.

Ordinary `SELECT` is ordinary: projection, predicates, `GROUP BY`, `ORDER BY`, `LIMIT`, joins,
scalar subqueries, `CASE`. A projection over an empty table returns no rows rather than
failing, and an aggregate over one returns `count(*) = 0` with a null `sum`.

### Two classes of table

| Class | System of record | Read modes |
|---|---|---|
| **Managed** | The transactional store | Strong, bounded-freshness, pinned |
| **External** | The published tier itself | Bounded-freshness, pinned |

A table published directly by an external writer has **no transactional tier**, so a
strongly-consistent read of one is refused by name rather than served from published data —
which would assert a currency the table cannot offer, with nothing in the result to say so.

The class lives in the table's own log, and **absence means external**. A directory somebody
dropped Parquet into is not managed by this system, and defaulting the other way would have it
claim a tier it does not have. See [ARCHITECTURE §5.6](ARCHITECTURE.md).

### Data definition and modification are refused, never accepted and discarded

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

That `§3` is quoted exactly as the server emits it
(`crates/sankhya-server/src/execute.rs:661`) and it now points at the wrong section: publishing
is §5 of this rewrite, and §3 is naming. A section number compiled into a binary is a
cross-reference nothing checks, so it is recorded here rather than repaired by renumbering a
document around a string literal. Moving it is a code change.

Two statements look like exceptions and are not writes to rows: `CREATE TABLE … CLONE` (§3)
and `CREATE CUBE` / `DROP CUBE` (§7). Both change catalogue state and neither writes a row.

> **A table dropped after startup stays listed.** A table that was present when the server
> started and is dropped afterwards **remains listed and remains queryable** for the life of
> that process, while `SHOW LINEAGE OF` it correctly refuses. If you script against
> `information_schema.tables`, do not treat its presence as proof the table is there.

---

## 3. Naming a table, cloning one, and lineage

A warehouse is `<schema>/<table>/`, and a table has two names that both work:

```sql
SELECT id FROM sales.orders;
SELECT id FROM orders;
```

The qualified name always resolves. The bare one resolves **while only one schema holds a table
of that name** — and stops resolving the day a second one does, naming both candidates rather
than quietly answering from whichever was registered first. That is the only behaviour that
cannot be wrong: a name that means two things has no right answer, and picking one would hand
back a table the caller had no way to identify.

So a script that will outlive today's warehouse should qualify. `information_schema.tables`
prints both parts:

```sql
SELECT table_schema, table_name FROM information_schema.tables;
```

### A clone stays in its origin's schema

```sql
CREATE TABLE q3_frozen CLONE sales.orders;
```

It lands in `sales`, beside what it was cloned from. Saying so explicitly —
`CREATE TABLE sales.q3_frozen CLONE sales.orders` — is the same statement. Naming any other
schema, `CREATE TABLE archive.q3_frozen CLONE sales.orders`, is **refused**.

Not a convention. A clone is a **reference** to its origin's files rather than a copy of them
([ADR-0016](adr/0016-zero-copy-cloning.md)), and the right to read it derives from the right to
read what it references — which is why authorization resolves a clone through its root. A clone
under another schema would have its *name* governed by one policy and its *data* by another,
and nobody could say which rule applied to it.

`AT VERSION <n>` pins the origin version the clone reads; omitted, it takes the origin as it
stands when the clone is made. Nothing is copied: a clone's log names none of its origin's
files, and a read splices the origin's live set *at the cloned version* with the clone's own
log. The cost is one log with no files in it, whatever the table's size.

Lineage records the qualified name for the same reason a script should use it: a lineage
outlives the moment a bare name was unambiguous.

```sql
SHOW LINEAGE OF q3_frozen;      -- what it is a clone of, nearest first
SHOW DEPENDENTS OF sales.orders; -- what still reads it, before you try to drop anything
```

`SHOW LINEAGE OF` returns `step, origin, origin_version, cloned_at`, nearest first — the first
row answers *"what was this cloned from?"* and the last answers *"what is it ultimately a
snapshot of?"*, which one step cannot. An empty result means *not a clone*, which is an answer
and a different one from *"I could not tell you"*.

`SHOW DEPENDENTS OF` returns `dependent, relation, reads_version`, where `relation` is `direct`
or `indirect`. It answers the question a refusal used to answer too late: dropping a table a
clone still reads is refused and names the clones, which is no use to somebody who had no way
to ask first. An indirect reader breaks when the table *between* them goes, which is a
different problem with a different fix, so the two are not collapsed.

---

## 4. The date axis

Every table carries **`sank_data_date`**, of type `DATE`, and is partitioned on it. That one
guaranteed column is what makes partitioning, time-based retention and hot/cold tiering
possible to write once rather than per table.

```sql
SELECT sank_data_date, count(*) FROM sales.orders GROUP BY sank_data_date ORDER BY 1;
```

**The type is `DATE` and not an encoded integer.** Partition paths are
`sank_data_date=2024-03-01`, which Spark and Trino parse as a date natively; an integer is a
string they must be told about. And `20240301 - 7 = 20240294` is not a date, raises no error,
and is a thing people write.

**The value is declared per table, never defaulted per row.** This is the part worth
understanding:

| Declaration | Meaning | A null in the source |
|---|---|---|
| `dated_by("order_date")` | Every row's date comes from that column | **An error** |
| omitted | Every row uses the ingest date, **and the table records that it does** | — |

A per-row fallback to "today" would make the column mean *when it happened* in some rows and
*when we received it* in others, in the same table, with nothing recording which. Then
`WHERE sank_data_date = '2024-03-01'` returns a mixture that no query can separate afterwards.
See [ADR-0004](adr/0004-the-date-axis.md).

Granularity is `day`, `month` or `year`. An unrecognised value is refused rather than defaulted
— a monthly table silently becoming daily is repartitioned on its next write, which is a full
rewrite for a typo.

---

## 5. Publishing a table, and declaring a feed

External systems publish through **this system's library**, not by assembling the format
themselves. The format stays open and documented — external engines read it directly — but the
*supported* write path is the library, and the reason is asymmetry:

> A reader that misunderstands the format is wrong for itself, recoverably. A writer that
> misunderstands it corrupts the table for everyone, permanently, and undetectably — because
> the writer's own reader shares the misunderstanding.

This system has direct evidence. Writing its own format with the specification open, it omitted
a non-nullable field from every `add` action; its own reader accepted the result happily and an
independent implementation rejected it on the first read.

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

### Declaring a feed

Publishing a table by writing Rust is the M2 way in. A **feed** is the declared one: a file
says where documents arrive, what shape they are, and where they land, and the server does the
rest on a cadence.

One file per feed, under `config/feeds/`:

```yaml
name: orders
from: /var/spool/sankhya/orders   # newline-delimited JSON, one dictionary per line
schema: sales
table: orders                     # must already exist — a feed never creates a table
date: ingest                      # or { column: booked_on }, which must be a date and not null
columns:
  - name: id
    type: int64
  - name: amount
    from: total                   # the key in the document, when it differs
    type: decimal(18,2)           # decimals arrive as *strings*
```

`config/feeds/README.md` is the annotated version, with every setting and its default.

Each of these refusals turns a defect at the source into published data that looks fine, which
is the failure nobody notices at the time:

| It refuses | Rather than |
|---|---|
| `"42"` into an `int64` | parsing it, and hiding the day the source sends `"forty-two"` |
| `3.0` into an `int32` | converting it, and then having to decide about `3.5` |
| a JSON *number* into a `decimal` | accepting it, when `0.1` is not `0.1` in binary floating point |
| a missing key | inventing a value indistinguishable from a measurement |
| a key no column claims | discarding a field the source just grew |
| `"31/08/2026"` as a date | guessing between day-first and month-first |

### What happens to a record that does not fit

It is **quarantined**: written whole, exactly as it arrived, into `sank.sank_quarantine` — a
table, not a directory of rejected files — alongside the reason, a stable code, the position it
arrived at, and a fingerprint of the declaration that refused it. Whole, because a record
reduced to an error message cannot be replayed, and replay is the only actual remedy.

```sql
SELECT source, position, reason_code, reason, payload
FROM sank_quarantine
WHERE feed = 'orders';
```

**One bad record is an incident; a run of them is an outage.** Above `stop_above` of a recent
window, or on a source that produced nothing usable at all, the feed **stops and waits for a
person**. It does not retry on a timer: a source whose shape has changed produces all-bad
records for as long as it runs, and a feed that keeps going leaves every dashboard green while
nothing arrives.

### Seeing a feed, and starting one again

A feed that stopped is a **state**, not a log line somebody had to be watching for:

```sql
SHOW FEEDS;
```

One row per declared feed — `feed, state, halted_since, reason, runs, published, quarantined,
skipped, halts`. A feed that has never managed to run is listed too, which is the case an
operator most needs to see and the one a *"list of running feeds"* would omit.

Resuming is a statement, not a restart:

```
RESUME FEED orders
```

It takes effect on the next tick. Restarting the server would resume it as well, and take an
outage on every other feed and every open connection to do it.

Resuming does not forget. The halt count survives it, because a feed that halted, was resumed
and halted again for the same reason is not in the situation a feed that halted once is in, and
the row is how anybody tells them apart.

A feed records how far it has got as a property of the table it writes to, **in the same commit
as the rows**. Either both are visible or neither is, so a restart can neither duplicate nor
skip. Sources are read in name order and the position is the last one finished — so a file
appearing *behind* that mark is named rather than ingested, because a producer writing out of
order and somebody replaying an old file want opposite responses and only a person can tell
which happened. See [ADR-0018](adr/0018-a-record-that-does-not-fit.md).

**What is not built:** nothing drives ingest in a running server on a timer, so everything a
server serves is already published.

---

## 6. Snapshots, history and versions

A **snapshot** names one instant across many tables. A clone freezes a *thing*; a snapshot
freezes a *moment*.

```sql
CREATE SNAPSHOT eod_2026_09_02 EXPIRE AFTER 90 DAYS;
```

```sql
SHOW SNAPSHOTS;
```

It records the version every table you may read stood at, and pins those files so they survive
reclamation. Nothing is copied.

**The expiry is required and there is no `EXPIRE NEVER`.** A snapshot pins files, so one that
never expired would hold a whole warehouse's versions alive, and the storage cost would fall on
somebody who did not ask for it. `SHOW SNAPSHOTS` reports what each one pins and who took it,
because a cost with no visible owner is one nobody reclaims.

Why a run needs this: a market-risk calculation reads the trade population, the FX rates, the
curves and the hierarchy. If those four are read at four moments, the reconciliation problem
this system exists to remove reappears *inside a single query*.

### Reading as of one

```sql
SET SNAPSHOT = 'eod_2026_09_02';
```

```sql
SELECT region, round(sum(amount)) AS total FROM orders GROUP BY region ORDER BY region;
```

```sql
RESET SNAPSHOT;
```

```sql
DROP SNAPSHOT eod_2026_09_02;
```

It is a **session** setting, because a run reads one instant across many statements rather than
one. Another connection is unaffected.

**A table created after the snapshot is not there**, and a statement naming it fails to resolve
exactly as a table that does not exist does. It is not answered as empty: a table that did not
exist is not a table that was empty, and a join against one returns the rows surviving an inner
join with nothing — a confident zero, reported as success.

`SET SNAPSHOT` to a name that does not exist, or to one that has expired, is refused **at the
`SET`** rather than at the next query. Failing where a person can act beats failing where the
consequence happens to be noticed.

### Reading one table's history

A snapshot is a *tag*. `SHOW HISTORY OF` is the log underneath it.

```sql
SHOW HISTORY OF sales.orders;
```

```
 version |   what    |      at       | files_added | files_removed | bytes_added | changed_data |         kept_by
---------+-----------+---------------+-------------+---------------+-------------+--------------+-------------------------
       0 | created   |               |           0 |             0 |           0 | no           |
       1 | appended  | 1756545242000 |           4 |             0 |     8912344 | yes          |
       2 | appended  | 1756631575000 |           4 |             0 |     9014112 | yes          | eod_2026_08_31
       3 | compacted | 1756609211000 |           1 |             8 |    17800004 | no           | eod_2026_08_31, q3_frozen
```

`at` is milliseconds from the epoch, as every timestamp on this surface is, and is **empty**
rather than zero for a commit that touched no file and so recorded no time. A commit that only
declared a schema has nothing to take a time from, and 1970 presented as a fact is worse than a
blank.

Two columns carry most of the meaning.

**`changed_data`** is the writer's own declaration, not a guess from the file counts. A
compaction rewrites files and changes not one row, so it reports `no` — and a column that
called that a change would be telling you your table moved every time maintenance ran, which is
both false and the fastest way to make you stop reading the column.

**`kept_by`** names the snapshots and clones holding that version alive — by name, because
somebody reading this column is deciding what to drop to release the storage — and is empty for
versions nothing is keeping. That emptiness is the important part: **history is readable only
where something is keeping it alive.** Retirement deletes the files a merge replaced. The
commit stays in the log forever; its data does not.

### Reading one table at a version

```sql
SET VERSION OF sales.orders = 2;
SELECT count(*) FROM sales.orders;
RESET VERSION OF sales.orders;
```

Per table, per session, and independent of `SET SNAPSHOT` — this answers "what did *this* table
look like then", where a snapshot answers "what did *everything* look like then". Use it to
check one table against yesterday; use a snapshot when more than one table has to agree.

Three refusals, each of which was a wrong answer before it was a refusal:

- **A version the table does not have** is `42704` and names the newest it does have. Replaying
  a log stops at its end, so asking for version 9999 of a five-version table used to hand back
  version 5 — a version nobody has, served as though they had it.
- **A version whose files retirement has taken** is `42704` and says so in those words: the
  commit is in the log and its data is not. Answering it would return the rows that happen to
  survive, which is a historical query silently missing whatever was compacted — the wrong
  answer that looks most like a right one, because it has rows in it.
- **A table that does not exist** is `42P01`, at the `SET`.

### What changed between two versions

```sql
SHOW CHANGES BETWEEN 1 AND 4 FOR sales.orders;
```

```
 commits | rows_added | rows_removed | files_added | files_removed | compactions
---------+------------+--------------+-------------+---------------+-------------
       3 |        750 |            0 |           3 |             0 |           0
```

The question people ask between two reporting runs — *did anything change, and how much* —
answered from the log alone. Nothing here opens a Parquet file, so it is answerable on a table
nobody would consider scanning.

[ADR-0024](adr/0024-what-a-difference-between-two-versions-is.md) settles what a difference
*is*, and one line decides the rest: **a difference is a change to rows, and a compaction is
not one.** The log already knows — every add and remove carries `dataChange`, and a compaction
writes `false` on both sides. That is the writer's own statement about what it did, not a
heuristic.

- **The range excludes the earlier version and includes the later.** `BETWEEN 4 AND 7` is *what
  happened after 4, up to and including 7*. The other reading is defensible, and the two differ
  by exactly one commit.
- **`compactions` is counted and never folded in.** Filtering it out of the arithmetic is
  correct; staying silent about it would leave a reader wondering why the storage looks nothing
  like it did.
- **There is no `rows_changed`, deliberately.** An update here is a remove and an add, and
  nothing in the log says the two are the same row. Reporting a changed count would mean
  guessing which removal pairs with which addition — right often enough to be trusted, and
  wrong exactly when a key was rewritten, which is the case somebody is diffing to find.

**What this is not.** It is not version control. There is no way to restore a version, because
the log records *files*, not rows. What you have is closer to a tag than a branch: name a
moment, read it back, and know that the naming is what keeps it readable. A row-level
difference needs a decision before it needs code, and that decision is `M20`'s.

### How long a statement may run

A statement is stopped after **thirty minutes** and answers `57014`, *query_canceled*.

Not a performance target. A statement that outlives the client that asked for it is pure cost —
nobody will read the answer, and it holds a worker until it finishes. One is waste; enough of
them are a denial of service that any connected client can cause by typing a short query and
hanging up. `SANKHYA_STATEMENT_TIMEOUT_SECONDS` overrides it, and `0` means no limit — a real
choice for a batch deployment with no untrusted clients, said out loud rather than arrived at
by having no limit at all.

---

## 7. Cubes — declare, roll up, slice

A `GROUP BY` knows the column names you typed. A **cube** knows a *model*: which columns are
dimensions, which are measures, and — the part that decides whether an answer is correct —
**how each measure may be combined along each dimension**.

That last one is why this is not a convenience over `GROUP BY`. Summing a closing balance
across twelve months gives a number of the right magnitude, the right sign, and no meaning. A
cube refuses it.

### Two navigations, not four or five

`cube_rollup` and `cube_slice` are the whole navigation surface, registered at
`crates/sankhya-cube-sql/src/functions.rs:56-60`. `FR-CUBE-14` asks for slice, dice, roll-up,
drill-down and pivot, and `crates/sankhya-cube-sql/src/lib.rs:5` says so. `dice` and `pivot`
exist as kernels — `crates/sankhya-cube/src/lib.rs:71` exports them — and `dice` is reachable
only from *inside* `cube_slice`'s implementation
(`crates/sankhya-cube-sql/src/functions.rs:237`). **`cube_pivot` and a drill-down have no SQL
registration anywhere in `crates/`.**

That correction is the reason this heading exists. Several documents said four navigations and
one said five; the README said two and was right. A count is the easiest kind of claim to write
without checking and the hardest to notice is wrong, because it reads as a summary rather than
as an assertion.

Two further table functions describe a cube rather than navigate it — `cube_dimensions` and
`cube_measures`, at `crates/sankhya-cube-sql/src/describe.rs:35-36` — alongside `cubes()` and
`derived()`.

### Declaring one

```sql
CREATE CUBE quarterly FROM orders
  DIMENSION region FROM orders ON region (LEVEL area = region)
  DIMENSION period FROM orders ON period (LEVEL quarter = period)
  MEASURE amount (SUM ALONG region, SUM ALONG period);
```

Read it as: the facts are in `orders`; `region` takes its members from `orders` itself, joined
on the fact table's `region` column; and `amount` adds along both dimensions.

A dimension usually has its own table — `DIMENSION geography FROM regions ON region_id` — and
several levels, coarse to fine, which is the order a drill-down walks:

```text
  DIMENSION geography FROM regions ON region_id (
      LEVEL country = country_code,
      LEVEL region  = region_code
  )
```

The rule is the part that decides whether an answer is correct. `SUM ALONG period` says a
measure adds over time; `LAST ALONG period` says it does not, which is what a balance needs,
because a December balance is not the sum of twelve month-end balances.

**Every measure needs a rule for every dimension, and there is no default.** A missing rule is
refused when the cube is declared, naming the measure and the dimension. That is the whole
design: the alternative is an implicit `SUM` that produces a plausible wrong number.

| Rule | Combines by | Composes further? |
|---|---|---|
| `SUM` | adding | yes |
| `MIN`, `MAX` | the extreme | yes |
| `FIRST`, `LAST` | position in the dimension's order | yes |
| `MEAN` | the arithmetic mean | **no** — an average of averages is not an average |
| `NONE` | it cannot be derived from parts at all | **no** — a ratio, a distinct count |

`MEAN` and `NONE` are declarable and are refused where they would be composed, rather than
quietly producing a figure. That refusal is the feature.

**Optional clauses**, in this order:

```text
  MAINTAINED WITHIN 5 VERSIONS      -- materialise, and tolerate five commits' drift
  PINNED (geography)                -- always materialise this shape
```

A ragged hierarchy — an organisation chart, where depth varies — is declared as parent-child
rather than as levels, because flattening it forces padding:

```text
  DIMENSION people FROM employees ON employee_id (
      LEVEL person = employee_id,
      PARENT employee_id TO manager_id
  )
```

And an alternate roll-up that lives in the definition rather than in the data:

```text
      ROLLUP emea TO world
```

> **A defect that lived here, and what leaving it documented cost.** Until 2026-09-01 a measure
> declared `MEAN ALONG region` or `MAX ALONG region` returned the **sum**: 15,687 where the
> maximum was 373.5. The composability half of the rule was enforced correctly — rolling a
> `MEAN` or `NONE` measure *away* was refused with the right sentence — and the cell was then
> read with a hardcoded summation one layer below, so the declared rule never reached the
> number. That is the exact failure the cube model exists to prevent, arriving beneath where
> the model checks for it.
>
> It is fixed, and `crates/sankhya-server/tests/cube_rules.rs` pins it: a cube declaring `MAX`,
> `MEAN` and `MIN` is compared against `max()`, `avg()` and `min()` over the same rows, over
> the wire, on every build. The two days between the fix and this paragraph are the shape of
> rot this document is otherwise careful about — the defect was fixed and nothing pinned the
> fix, so the text went on describing it as live. **A defect that is fixed and unpinned is a
> defect that comes back**, and until it does, its obituary is the thing that is wrong.

### Removing one

```sql
DROP CUBE quarterly;
DROP CUBE IF EXISTS quarterly;
```

**Dropping a cube also reclaims every cuboid it materialised.** That matters more than it
sounds: the ordinary cuboid sweep deliberately *keeps* anything belonging to a cube it cannot
find a current version for — deleting on a guess is how a cache becomes a data loss — so a drop
is the only moment at which that storage can be released. Nothing else will ever reclaim it.

**There is no `CREATE OR REPLACE CUBE`**, deliberately. Replacing a cube retires everything it
materialised, and that should not happen because somebody re-ran a script. Drop it and create
it, so the expensive half is written down.

A cube named in a `CREATE` whose fact table or dimension tables you cannot read is refused with
the same sentence as one whose tables do not exist. A refusal that distinguished them would
tell you the table is there.

### Finding out what exists

A cube is discoverable, so a client offers a picker instead of hardcoding a model that will
drift from it.

```sql
SELECT cube, fact_table, dimensions, measures FROM cubes();
```

```sql
SELECT dimension, level, depth, column FROM cube_dimensions('sales');
```

`depth` is the level's position from coarse to fine. It is a column rather than the row order
because the order is a fact about the model — sort the result without it and you draw a list
where there is a hierarchy.

```sql
SELECT measure, dimension, rule, composes FROM cube_measures('sales');
```

`composes` says whether a measure can be rolled up **at all**. Offering "roll up by period" on
something that cannot is offering a button that does not work, and finding out when the query
fails is worse than never offering it.

### Rolling up

```sql
SELECT region, amount FROM cube_rollup('sales', 'amount', 'by=region');
```

Roll-up means rolling a dimension **away**. The sample cube has `region` and `period`, so
asking `by=region` combines every period into one figure per region.

### Slicing

```sql
SELECT region, amount FROM cube_slice('sales', 'amount', 'where=period:q1');
```

A slice narrows to one member. It is a restriction on the question, not a loss of data — which
is why it does not change the completeness reported below.

### The options string

Options are a single string of `key=value` pairs, because only literals reach a table function
and `name => value` is rejected outright by the SQL planner. Every key is checked against a
known set, so a misspelled bound is refused rather than silently taking its default.

| Option | Meaning |
|---|---|
| `by=<dim>` | The dimension to keep. Omitted, the answer is the grand total |
| `by=<dim>\|<dim>` | Several dimensions. The list separator is a pipe because the options string is itself comma-separated |
| `where=<dim>:<member>` | Fix one member — a slice |
| `min_completeness=<f>` | Refuse an answer that saw less than this fraction |
| `materialise=true\|false\|pinned` | Narrow what this query may use. **Nothing widens it** |

### Every answer says what it is

```sql
SELECT region, amount, snapshot, completeness, withheld, materialised
FROM cube_rollup('sales', 'amount', 'by=region');
```

| Column | What it tells you |
|---|---|
| `definition_version` | Derived from the definition's content, never declared |
| `snapshot` | the table version this was computed at, so a cube figure can be reconciled with a relational one taken at another moment |
| `completeness` | what fraction of the input reached the cube |
| `withheld` | how many rows did not, whether from policy or because they could not be placed |
| `materialised` | whether the answer came from a stored cuboid or from the base data |
| `from_cuboid` | which one, when it did |

**`completeness` is the one to understand.** Two people with different permissions ask the same
question and correctly get different totals, because an aggregate is computed over the rows the
caller may read. Most systems make an operator choose between a true total and a visible one;
here every answer states how much of its input it saw, so a filtered total is distinguishable
by looking at it rather than by knowing which role you were in.

A query may insist:

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region, min_completeness=0.5');
```

Worked, with a control: a `SUM` cube over a 1,000-row table whose region is null on 333 rows
reports `completeness 0.667`, `withheld 333`, and a grand total of `499500` — exactly
`SELECT sum(amount) … WHERE region IS NOT NULL`.

### What a cube refuses, and why that is the point

```sql
-- ERROR: a ratio cannot be derived from its parts
SELECT region, margin_pct FROM cube_rollup('sales', 'margin_pct', 'by=region');
```

`margin_pct` is a ratio. There is no operation over the parts that yields the whole — the
margin of two regions is not the sum, the mean, or anything else derivable from the two
margins. So it is declared as composing along nothing, and the refusal happens **while the
query is planned**, not after a plausible number has been computed.

Averaging is refused for the same reason. An average of averages is an average only when every
group is the same size, and groups are never the same size.

### Three lifetimes

| | Persisted | Materialised | Maintained by | Ends when |
|---|---|---|---|---|
| **Ephemeral** | no | no | nothing | the session ends |
| **Declared** | yes | no | nothing | it is dropped |
| **Maintained** | yes | yes | the warehouse | it is dropped |

**Ephemeral is the intended default**, and it is **not what a plain `CREATE CUBE` does today**:
that persists a definition under the warehouse's `_cubes/`, visible to every other connection.
There is no syntax yet for asking for an ephemeral one, so on a shared server a reader who
believes this paragraph publishes their exploration to everybody.

The reasoning stands and the mechanism does not exist. `M14` builds the ephemeral lifetime with
the **mandatory expiry** that `RSK-35` requires; until then, drop what you declare.

A **Declared** cube costs one small file and computes on demand. It is the right choice for a
cube asked about occasionally, and for any cube whose readers have different permissions — a
stored aggregate is only usable by callers entitled to exactly the rows it was built from, so
materialising a cube read by twenty differently-restricted analysts mostly produces cells
nobody may use.

A **Maintained** cube adds `target_lag`, and the warehouse keeps it within that lag whether or
not anybody is logged in. It is a **staleness target, not a schedule**: `target_lag = 5` means
*the cells may be at most five commits behind*, not *rebuild every five commits*. A schedule
rebuilds when nothing has changed and fails to rebuild when a build takes longer than its
interval; a target says what you actually want.

Staleness here is exact rather than estimated, because a stored cuboid records the version it
was computed at. **A cuboid past its target is never served as though it were fresh** — the
answer falls back to live aggregation, which is slower and right, and says `materialised =
false` so you can see which you got.

### Deciding what gets materialised

The lattice of possible cuboids is exponential in the dimension count, so *everything* is not a
plan — it is a way to fill a disk. Three controls decide, and they belong to three different
people.

| Level | Who sets it | What it says |
|---|---|---|
| **Definition** | whoever models the cube | shapes **pinned** — always worth holding |
| **Configuration** | the operator | the row **budget** automatic selection may spend |
| **Session** | the caller | whether *this* query uses materialisation at all |

**The definition pins.** Selection spends the operator's budget on evidence — what people have
actually asked for. A pin is the statement that a shape is worth holding *before* any evidence
exists: the month-end roll-up nobody runs until the day it has to be instant.

**The operator budgets.** It is their storage being spent on their behalf by a selection
reading somebody else's query log, so it is bounded by a number they set:

```toml
[cubes]
budget_rows = 10000000
```

Set it to `0` and automatic selection buys nothing. The base cuboid and any pinned shape are
still built — neither is bought from the budget.

**The caller may ask for less, and only less.** There is deliberately no value that widens
anything: a session that could raise the budget would be an unbounded storage grant to anybody
who can open a connection.

```sql
SELECT region, amount, materialised
FROM cube_rollup('sales', 'amount', 'by=region, materialise=false');
```

`materialise=false` computes from the base data. That is the **reproducibility check**: a
figure that differs between it and the default is a defect, not a tuning question —
materialisation is a cache, and a cache that changes the answer is not one.
`materialise=pinned` uses only shapes the definition names.

An unrecognised value is refused while the query is planned:

```sql
-- ERROR: 'materialise' must be true, false or pinned
SELECT region, amount FROM cube_rollup('sales', 'amount', 'by=region, materialise=maybe');
```

### What is left when nobody is watching

Automatic materialisation is driven by a **query log**: a bounded record, per cube, of which
dimensions people grouped by. The repetition is the weighting — a shape asked ten times counts
ten times — and old entries are overwritten, so a dashboard nobody has opened in a week stops
pinning storage without anybody deciding it should.

It records a *shape*, and there is nowhere in it to put a member, a predicate, or who was
asking. That is worth stating plainly, because a query log is the kind of thing that quietly
becomes a record of who asked what about whom. This one cannot.

**A cube nobody has queried gets its base cuboid and nothing else.** That is the honest answer
rather than a guess: there is no evidence about what would help, and spending an operator's
storage on a guess is worse than spending none.

### Who a stored cuboid may serve

A background refresh has no principal — nobody is logged in at four in the morning — so it
builds the **unrestricted** cuboid: an aggregate over every row.

That cuboid may serve only a caller whose own permissions withhold nothing. Serving it to
somebody a row policy filters would be a disclosure through arithmetic, and an invisible one:
the number is real, it is simply computed over rows they may not read. There is no error to
notice and nothing in a log to find.

The consequence is worth knowing rather than discovering. **Background refresh helps dashboards
and service accounts, and does nothing for a restricted analyst** — their cuboids can only be
built by their own queries.

### A derived result

A query given a name, selected from like a table:

```sql
CREATE DERIVED regional FROM (
    SELECT r.area, o.amount FROM sales.orders o JOIN sales.regions r ON o.region = r.region
);
```

```sql
SELECT area, sum(amount) FROM regional GROUP BY area;
```

```sql
DROP DERIVED regional;
```

[ADR-0014](adr/0014-materialized-views-and-the-cube-lifetime.md) settles what this is: **a
definition with no dimensions and no measures is simply a maintained query.** It is the same
definition a cube is, so it gets the same declared query, the same dependency list resolved by
planning under your own guard, the same snapshot key and the same fingerprint. A second
implementation of maintained-derived-data would grow a second refresh loop and a second
staleness rule, and the two would drift.

Three refusals: a derived result over a bare table name (it would be that table with a second
name); a query whose answer can move on its own — `now()`, `random()`; and `MAINTAINED WITHIN n
VERSIONS`, because nothing materialises a derived result yet. That last is refused rather than
accepted and ignored, which would tell an operator their staleness bound was being honoured
when it was not.

### What is not here

MDX, deliberately — see [ADR-0007](adr/0007-the-cube-model.md). The navigation vocabulary is
SQL table functions instead, so a cube is reachable from any client that speaks the PostgreSQL
wire protocol, with no second query language to learn or to secure.

---

## 8. Vectors and matrices

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

### QR, SVD and eigendecomposition are shipped

**This document said the opposite until this rewrite, and so did seven other places.** They are
built in `crates/sankhya-math/src/decompose.rs` — `qr` at line 139, `eigen_symmetric` at 224,
`eigenvalues_symmetric` at 343, `singular_values` at 358, `cholesky` at 84 — and registered as
SQL functions in `crates/sankhya-functions/src/linalg.rs:49-88`:

| Function | What it gives |
|---|---|
| `mat_cholesky` | The factor `L` with `L·Lᵀ = A`, flat and lower-triangular |
| `mat_eigenvalues` · `mat_eigenvectors` | Symmetric eigendecomposition, eigenvalues descending |
| `mat_singular_values` | Singular values, descending |
| `mat_qr_q` · `mat_qr_r` | The QR factors, each returned flat |
| `mat_is_symmetric` · `mat_is_positive_definite` · `mat_is_square` | Questions answered `1` or `0` |

The names are `mat_qr_q`, `mat_qr_r` and `mat_singular_values`. `mat_qr` and `mat_svd` do not
exist and never did, though a planning list in the old function catalogue offered both.

This is worth being blunt about, because **a stated refusal silently reversed is the worst
class of falsehood a document of this kind can carry.** A missing feature is a gap a reader
routes around. A refusal is different in kind: it is read as a decision, it is quoted in
architecture reviews, and somebody plans a year of work around it. Reversing one without saying
so does not merely mislead — it tells a reader that the refusals *are not load-bearing*, which
is the property this entire document is trying to establish. The old sentence was *"they are
where an in-house implementation is worse than none — a subtly wrong SVD produces plausible
singular values."* The implementation now exists; the risk the sentence names has not gone
away, and §9 is where it lives.

Jacobi is used for the eigendecomposition rather than the faster algorithms. A shifted QR
iteration is several times faster and picks its pivots from the current iterate, so a matrix
perturbed in its last bit can converge to eigenvalues differing in their last several. An
eigenvalue is a figure, and a figure that moves when the machine is busier is the thing this
system is arranged against. [historical: a property of the algorithm rather than a measurement of this build --- no shifted-QR implementation exists here to time against]

A Cholesky failure is the useful part. It succeeds exactly on the positive-definite matrices,
so a covariance matrix that will not factor is not a numerical accident — it is one no data
could have produced, usually a correlation somebody interpolated by hand.

### A declared shape does not always win

The old function catalogue said *"a declared shape always wins where there is one"*, restating
[ADR-0021](adr/0021-vectors-matrices-across-the-tiers.md) Decision 4a. That is true of one
family and false of the other, and the difference decides whether a rectangular matrix produces
an answer or a wrong answer.

**The OLAP family honours the declaration.** `crates/sankhya-olap/src/matrices.rs:311` computes
`declared.or(deduced)`: the shape written into the column's field metadata wins, and the
`sqrt(len)` deduction is reached only for the operations that can mean nothing else —
determinant, trace, inverse, solve (`matrices.rs:168-170`). `mat_multiply`, `mat_transpose` and
`mat_vec` refuse rather than guess (`matrices.rs:312-320`).

**The linear-algebra family does not read the metadata at all.** All six functions registered
at `linalg.rs:49-88` call one helper, `order()` at `linalg.rs:26-37`, which takes the square
root of the array's length and refuses anything that is not a perfect square. It never consults
the tensor shape. Two of the six are worse than square-only, because they force a genuinely
rectangular algorithm into a square shape: `linalg.rs:74` calls
`decompose::singular_values(values, size, size)` and `linalg.rs:82` and `:88` call
`decompose::qr(values, size, size)`. So a 3×12 matrix — thirty-six values, a shape it may well
have declared — is read as a 6×6 and answered.

That is a **code** defect, not a documentation one, and it is recorded here rather than fixed
here. Until it is fixed, use `mat_qr_q`, `mat_qr_r` and `mat_singular_values` on square
matrices only, and treat an answer over a rectangular one as arithmetic performed on values
that were never in the same row.

### Vectors and matrices as column *types*

[ADR-0021](adr/0021-vectors-matrices-across-the-tiers.md) decides that a vector is a declarable
column type — `FixedSizeList<Float64, n>` analytically, `float8[]` or `pgvector`'s `vector(n)`
transactionally — that the width is part of the type, that a matrix is that array plus a shape
in the column's metadata, that equality is element-wise and only at the same width, and that
there is no ordering of vectors so no `ORDER BY embedding`.

**Of that, what exists today is the storage and the wire form.** A vector crosses the wire as
`float8[]`, rendered `{1,2.5,3}` — not as the text `[1.0, 2.5, 3.0]`, which is what it was
until 2026-09-02. Every PostgreSQL driver already decodes `float8[]`, so this is the rare case
where doing the correct thing deletes code from every client. And a matrix column's shape now
survives being stored: the shape lives in the column's field metadata, and the Delta writer
used to keep only its own key and discard every other one, so **a matrix that was stored came
back not being a matrix**. The table read perfectly, every function that can deduce a square
order still answered, and only the three that need a declared shape refused — which reads as
three functions being awkward rather than as a storage defect. Found by calling every function
over a stored column and comparing it against the same function over a literal of the same
values.

What does **not** exist, and was written in the present indicative until this rewrite:

- **No DDL, no parser, no logical type.** There is no `VECTOR(n)` a column can be declared as.
- **The width is enforced on the publish path only**, not on capture.
- **There is no refusal for cross-width equality, and none for `ORDER BY embedding`.** Both are
  Decision 6, and both are unimplemented — so the wrong thing does not currently refuse.
- **No vector index exists.** Decision 5's rule — an index may rank candidates, only a kernel
  may report a distance — governs an index nothing has built.

### Two costs, stated as costs

- **An array column cannot be pruned.** A minimum and maximum of a vector prune nothing, so a
  table of embeddings prunes on `sank_data_date` and its scalar columns only.
- **An array cannot be a key column.** Array equality as row identity is refused rather than
  supported badly.

---

## 9. Determinism: bit-reproducible is not compensated

This section replaces one sentence that was doing two jobs and getting one of them wrong.

The old claim, in this document and in the book's SQL-surface chapter, was: *"Every reducing
kernel here is bit-deterministic … These go through the same compensated, order-fixed summation
the rest of the system uses."* The first half is a **reproducibility** property. The second
half is an **accuracy** property. They are different guarantees with different mechanisms, and
conflating them let the sentence extend an accuracy claim over a family of functions that does
not have it — the LU family, the decompositions, and the special functions. Which is precisely
the family a risk calculation reaches for.

### What every kernel has: bit-reproducibility

Every kernel here computes in a fixed order that does not depend on how the query was
partitioned, how many cores the machine has, or how busy it is. Two runs of the same expression
over the same values return the *same bits*. That is why this system does not delegate to a
numeric library: reordering freely for speed is what a good one does, and it is exactly what
cannot be permitted here. See [ADR-0005](adr/0005-array-columns-and-numeric-kernels.md).

Reproducible is not exact. `mat_determinant(mat_multiply(mat_of(2,2,1.0,2.0,3.0,4.0),
mat_of(2,2,5.0,6.0,7.0,8.0)))` answers `4.000000000000007` where the exact answer is `4`. The
promise is that two machines return the same bits, not that those bits are the infinitely
precise answer.

### What a shorter list has: compensated summation

`deterministic_sum` — `crates/sankhya-math/src/reduce.rs:85`, re-exported at `lib.rs:68` — sums
in a canonical order, ascending by magnitude, with Neumaier compensation on top. Its own module
documentation is precise about which mechanism does the work, and it is worth repeating because
the obvious story is wrong: **compensation is what makes the result order-independent in
practice**, searched over 3,000 randomised inputs spanning 120 orders of magnitude with no
counterexample found; **the canonical order is what makes it a guarantee rather than an
observation**, because Neumaier's bound bounds the error without proving bit-identity across
permutations. The sort is defence in depth for a property the compensation already delivers,
and it costs `n log n` against `n`. `exact_sum` accumulates into a fixed-point integer scaled
from the largest magnitude in the input, which buys the same proof without the sort — integer
addition is associative by construction.

These route through it:

| Family | Evidence |
|---|---|
| `vec_dot`, `vec_sum`, `vec_mean`, `vec_norm_l1`, `vec_norm_l2`, `vec_euclidean` | `crates/sankhya-math/src/vector.rs:165, 171, 185, 192, 202, 209` |
| `vec_cosine_similarity`, `vec_cosine_distance` | `vector.rs:221, 234`, through `dot` and `norm_l2` |
| `mat_vec` | `vector.rs:274` |
| `mat_multiply` | `crates/sankhya-math/src/matrix.rs:189`, through `dot` |
| `mat_trace` | `matrix.rs:220` |
| The statistics, calculus, regression, time-series and finance kernels | `stats.rs:75, 97, 176, 201`; `calculus.rs:116, 161, 172`; `regression.rs:111, 133, 136, 142, 144, 169`; `timeseries.rs:61, 90, 92, 315, 319, 331`; `finance.rs:53, 83, 381, 399, 401, 432, 443`; `quantile.rs:202` |
| A cube's own cells | `crates/sankhya-cube/src/cells.rs:148, 156` |

### These do not

They accumulate with a bare `+=`. They are order-fixed and reproducible — the property above
holds — and they are **uncompensated**:

| Family | Evidence |
|---|---|
| The LU family: `mat_determinant`, `mat_solve`, `mat_inverse` | `matrix.rs:284`, `matrix.rs:309`, `matrix.rs:339-341` and `:352-353`; `inverse` delegates to `solve` at `matrix.rs:382` |
| `mat_cholesky` | `crates/sankhya-math/src/decompose.rs:100` |
| The QR family: `mat_qr_q`, `mat_qr_r` | `decompose.rs:155, 172, 181, 192` — Householder, with bare accumulation throughout |
| The eigen family: `mat_eigenvalues`, `mat_eigenvectors` | `decompose.rs:252`, and the rotation loops beneath it; `eigenvalues_symmetric` delegates at `decompose.rs:344` |
| `mat_singular_values` | `decompose.rs:370-373` — the Gram matrix is built with a bare running sum |
| `gamma_p`, `gamma_q`, `beta_i`, `gammaln` | `crates/sankhya-math/src/special.rs:198` and `:219` (the gamma series and its Lentz recurrence), `special.rs:271` (the beta continued fraction), `special.rs:133` |

`decompose.rs` does not import `reduce` at all: it has zero `deterministic_sum` call sites.

### What to do with that

Two runs of `mat_eigenvalues` over the same covariance matrix agree bit for bit. That is the
property a reconciliation needs and it holds. What does *not* hold is the accuracy claim: a
Cholesky of an ill-conditioned covariance matrix, or a determinant of a large one, carries the
error a naive accumulation carries, and no compensation is subtracting it off.

So: **use the vector kernels and `mat_multiply`/`mat_trace` where a figure has to be defended
to the last bit, and treat the decompositions and the incomplete gamma and beta functions as
reproducible numerical results rather than as compensated ones.** A regression's coefficients
go through QR — which is the right choice, because forming the normal equations squares the
condition number, and a design at `1e8` becomes `1e16`, which a double cannot resolve — and the
summations *around* the solve are compensated while the solve itself is not.

Closing the gap is a code change: routing `decompose.rs` and `special.rs` through
`deterministic_sum` or `exact_sum`. It is recorded here rather than done here, because a
document that quietly widened its claim to cover the fix would be repeating the mistake this
section exists to correct.

---

## 10. The function catalogue

`SELECT * FROM functions()` is the catalogue, served by the server itself
(`crates/sankhya-functions/src/describe.rs:34`). It carries name, category, arity, argument
types, return type and a one-line description.

It exists for the reason `cubes()` exists: **a capability nobody can enumerate is a reference
manual nobody reads.** Both the Python binding's method surface and the tier-routing rule read
from it rather than from a list somebody maintains by hand — a binding cannot generate what it
cannot enumerate, and a router cannot classify a statement without a list of what the built-ins
are. A function added to the server appears in the binding with no change to the binding.

`functions()` was listed as *coming* in the old function catalogue's plan section while being
described as the wiring's single source of truth two pages earlier in the same file. It ships.

Two rules govern the tables below.

**A function is not delivered until a binding can call it.** A function on the SQL surface and
absent from the SDKs is half-shipped, and the missing half is the one most users have.

**A function works on every query, whoever runs it.** A user does not know whether the
transactional tier or the analytical one answered, and never needs to. The transactional tier
has no query path yet, so nothing routes there; what is built is `tier_for(sql, catalogue)`,
which reports that a statement naming a built-in is an analytical statement. That is built now,
before the router, because it is unaffordable to retrofit — by the time both tiers answer
queries, the second implementation of every function is already written, and the day two
implementations disagree the answer depends on a routing decision nobody can see. The match is
deliberately **eager**: a string literal that reads like a call routes analytically, and
nothing about the answer changes. The other direction is not survivable, because missing a call
means a refusal for a function the catalogue says exists.

### Vectors and per-row series

A vector is one row's series. These describe **one row**, where SQL's aggregates describe a
column: `stddev(x)` is the spread of a column; `vec_stddev(v)` is the spread inside a single
row's vector.

| Function | What it gives |
|---|---|
| `vec_of(...)` | Build a vector from scalars |
| `vec_sum` · `vec_mean` · `vec_median` | Total, mean, median of one row's series |
| `vec_min` · `vec_max` · `vec_range` | The ends, and the spread between them |
| `vec_variance` · `vec_stddev` | Sample forms (divide by *n−1*) |
| `vec_variance_pop` · `vec_stddev_pop` | Population forms (divide by *n*) |
| `vec_skewness` · `vec_kurtosis` | Third and fourth moments; kurtosis is excess |
| `vec_covariance` · `vec_covariance_pop` · `vec_correlation` | Between two vectors |
| `vec_regression_slope` · `vec_regression_intercept` · `vec_regression_r2` | Least-squares fit of one vector on another |
| `vec_dot` | Dot product |
| `vec_norm_l1` · `vec_norm_l2` | Manhattan and Euclidean norms |
| `vec_euclidean` | Distance between two vectors |
| `vec_cosine_similarity` · `vec_cosine_distance` | Angle-based similarity, for embeddings |
| `vec_integral` · `vec_integral_simpson` | Area under a sampled curve, trapezoid and Simpson |
| `vec_quantile` | Any quantile of one row's series, by linear interpolation |
| `vec_add` · `vec_subtract` · `vec_multiply` · `vec_divide` · `vec_scale` | Element-wise, between two series or a series and a number |
| `vec_differences` · `vec_derivative` · `vec_second_derivative` | Rate of change across a series |
| `vec_cumulative_sum` · `vec_cumulative_integral` | Running total, running area |
| `vec_standardise` | Centre and scale to unit variance, so two series in different units compare |

```sql
SELECT vec_mean(readings),
       vec_stddev(readings),
       vec_median(readings),
       vec_skewness(readings),
       vec_kurtosis(readings)
FROM sensors;
```

> **Why both a sample and a population form.** Which divisor a variance uses is a statement
> about what the data *is*, not a preference. A sample variance of a complete population
> overstates the spread, and the difference is invisible in the number — so both are named and
> you choose the one you mean.

> **Why Simpson sits beside the trapezoid rather than replacing it.** Simpson's rule is exact
> for a cubic where the trapezoid is exact only for a line, but it needs an even number of
> intervals and refuses otherwise. A function that silently changed rule to accommodate its
> input would return two different approximations under one name.

**Variance is computed in two passes.** The textbook one-pass identity is algebraically correct
and numerically disastrous: for values with a large mean and small spread it subtracts two
nearly equal large numbers, and cancellation can produce a **negative variance** — which every
downstream square root turns into a NaN.

**Kurtosis is excess**, so a normal distribution reads zero. Reporting raw kurtosis is a common
and confusing choice: a reader seeing 3.0 cannot tell whether it means "normal" or "quite
heavy-tailed" without knowing the convention, and both are plausible.

**A correlation against a constant series is refused**, not reported as zero. Zero would say
*unrelated*; the truth is *undefined*, and a ranked correlation table would show a constant
column as genuinely uncorrelated rather than as unanswerable.

The eleven series-returning kernels — `vec_differences` through `vec_standardise` — return a
`List<Float64>` rather than a `FixedSizeList`, because they change the width: a first
difference of *n* values has *n−1*, and a fixed width would make `vec_differences` of a
384-dimensional embedding a different function from `vec_differences` of a 3-dimensional one.
Twelve of these kernels existed in `sankhya-math`, unit-tested and mutation-tested, with **no
SQL name at all** until 2026-09-02. Every check in the repository passed the whole time,
because every check looked at the code rather than at the surface. `check-kernels` now fails
the build for the next one.

### Matrices

| Function | What it gives |
|---|---|
| `mat_of(rows, cols, ...)` | Build a matrix |
| `mat_identity(n)` | The identity |
| `mat_multiply` · `mat_vec` | Matrix–matrix and matrix–vector products |
| `mat_transpose` · `mat_trace` | Transpose, and the sum of the diagonal |
| `mat_determinant` · `mat_inverse` | Determinant and inverse |
| `mat_solve` | Solve *Ax = b* |

The decomposition family is in §8. Which of these is compensated and which is not is §9.

### Distributions and special functions

Four suffixes, the same four everywhere, so a caller who has met one family can guess the rest.

| Family | Functions |
|---|---|
| Normal | `norm_pdf` · `norm_cdf` · `norm_sf` · `norm_inv` · `normal_pdf` · `normal_cdf` · `normal_inv` |
| Lognormal | `lognorm_cdf` · `lognorm_inv` |
| Student's *t* | `t_pdf` · `t_cdf` · `t_sf` · `t_inv` · `t_two_sided` |
| Chi-squared | `chisq_cdf` · `chisq_sf` · `chisq_inv` |
| *F* | `f_cdf` · `f_sf` · `f_inv` |
| Binomial, Poisson | `binom_pmf` · `binom_cdf` · `poisson_pmf` · `poisson_cdf` |
| Exponential, gamma, beta, uniform | `expon_cdf` · `gamma_cdf` · `beta_cdf` · `uniform_cdf` |
| Special functions | `erf` · `erfc` · `gammaln` · `gamma_p` · `gamma_q` · `beta_i` |

> **`sf` is not `1 - cdf`.** A p-value of `1e-20` subtracted from one is zero, and a test
> reporting `p = 0` where the truth is `1e-20` has thrown away the only digits anybody was
> going to read. Every family with a tail worth asking for offers it directly.

> **A count must be whole.** `binom_pmf(2.7, 10, 0.5)` is refused rather than rounded —
> rounding answers a question about a different number of events, silently.

The cumulatives iterate to `3e-16`, so they are good to the last few bits. `erf` and `erfc`
come from the incomplete gamma rather than a rational fit, because the usual fit gives
`erf(0) = -5.9e-8` — a standard normal whose median is not zero. Note that `gamma_p`,
`gamma_q` and `beta_i` are on §9's uncompensated list.

### Inference and regression

| Function | What it gives |
|---|---|
| `ttest_1samp_t` · `_p` · `_df` | One sample against a hypothesised mean |
| `ttest_2samp_t` · `_p` · `_df` | Two samples, by **Welch's** test |
| `ttest_paired_t` · `_p` | Paired observations |
| `chisq_test_t` · `_p` | Pearson goodness of fit |
| `f_test_t` · `_p` | Two variances compared, two-sided |
| `jarque_bera_t` · `_p` | Normality, from skewness and kurtosis |
| `regress_slope` · `_intercept` · `_stderr` · `_tstat` · `_pvalue` · `_r2` · `_adj_r2` · `_residual_error` | Simple regression, one part per name |
| `regress_multiple_r2` · `_f_p` | Multiple least squares over a flat design matrix |
| `regress_ridge_norm` | Ridge, whose coefficients carry **no** *t*-statistic |
| `ttest_df_welch` | Welch's degrees of freedom from summary statistics |

> **Welch's, not Student's pooled test.** The pooled form assumes the two populations share a
> variance, and when they do not it rejects too often — it finds differences that are not
> there. Welch is correct either way, so there is no case where the pooled form is the better
> default and a caller has to know which they have.

> **A slope with no standard error is a number nobody can act on.** Every regression names its
> uncertainty beside its estimate. Ridge is the exception and deliberately so: the penalty
> invalidates the unpenalised standard errors, so offering a significance test beside a shrunk
> coefficient would invite one the arithmetic does not support.

> **Adjusted `R²` is reported beside `R²`** because plain `R²` never falls when a predictor is
> added — including a predictor of pure noise — so comparing two models by it always prefers
> the larger one.

### Time series, finance and risk

| Function | What it gives |
|---|---|
| `ts_rolling_mean` · `_std` · `_min` · `_max` | Rolling statistics over a window |
| `ts_ewma` | Exponentially weighted moving average |
| `ts_returns` · `ts_log_returns` | Simple and logarithmic period returns |
| `ts_drawdown` · `ts_max_drawdown` | Fall from the running peak, and the worst of them |
| `ts_cumulative_return` | Period returns **compounded**, which is not their sum |
| `ts_autocorrelation` | Correlation of a series with itself at a lag |
| `npv` · `npv_from_now` · `irr` | Net present value under either convention, and the rate that zeroes it |
| `pv` · `fv` · `pmt` | Annuity present value, future value and level payment |
| `sln` · `syd` | Straight-line and sum-of-years depreciation |
| `var_historical` · `expected_shortfall` | Value-at-risk, and the mean of what lies beyond it |
| `sharpe` · `sortino` | Excess return per unit of total, and of downside, deviation |
| `black_scholes_call` · `_put` · `greeks_delta` · `greeks_vega` | European option prices and two sensitivities |

> **A rolling window reports nothing where it does not reach.** The leading positions come back
> as **nulls inside the array**, not zeros. Zero is a number somebody acts on; the series mean
> pretends to information that is not there; repeating the first value makes a flat start that
> reads as low volatility.

> **A value-at-risk is negative for a loss**, because the outcomes are. It is not flipped to a
> positive "amount at risk" — one quoted positive gets added to a profit somewhere, and the
> sign is the only thing between a report and a number twice as wrong as it looks.

> **Both discounting conventions are named**, rather than selected by a flag. `npv` discounts
> from period one as a spreadsheet does; `npv_from_now` leaves the first flow undiscounted. A
> boolean deciding which of two definitions applies is a boolean somebody passes wrongly, and
> the result is plausible.

> **An internal rate of return refuses more than it converges.** A cash flow of one sign has no
> rate at which its value is zero, and one with several sign changes has several — all correct,
> none of them *the* answer.

### From the query engine

**Aggregates:** `sum` · `avg` · `mean` · `count` · `min` · `max` · `median` · `any_value` ·
`first_value` · `last_value` · `nth_value` · `array_agg` · `string_agg` · `grouping` ·
`stddev` · `stddev_pop` · `stddev_samp` · `var` · `var_pop` · `var_samp` · `var_population` ·
`var_sample` · `covar` · `covar_pop` · `covar_samp` · `corr` · `regr_slope` ·
`regr_intercept` · `regr_r2` · `regr_count` · `regr_avgx` · `regr_avgy` · `regr_sxx` ·
`regr_syy` · `regr_sxy` · `percentile_cont` · `quantile_cont` · `approx_median` ·
`approx_percentile_cont` · `approx_percentile_cont_with_weight` · `approx_distinct` ·
`bool_and` · `bool_or` · `bit_and` · `bit_or` · `bit_xor`

> Every `approx_*` name is a promise about *exactness*, not speed. Approximation here is
> declared and visible, never chosen for you.

**Window:** `row_number` · `rank` · `dense_rank` · `percent_rank` · `cume_dist` · `ntile` ·
`lag` · `lead` · `first_value` · `last_value` · `nth_value`

**Scalar mathematics:** `abs` · `ceil` · `floor` · `round` · `trunc` · `signum` · `factorial` ·
`gcd` · `lcm` · `pow` · `power` · `sqrt` · `cbrt` · `exp` · `ln` · `log` · `log2` · `log10` ·
`pi` · `nanvl` · `isnan` · `iszero` · `random` · `rand` · `sin` · `cos` · `tan` · `cot` ·
`asin` · `acos` · `atan` · `atan2` · `sinh` · `cosh` · `tanh` · `asinh` · `acosh` · `atanh` ·
`degrees` · `radians`

**Text:** `length` · `char_length` · `character_length` · `bit_length` · `octet_length` ·
`lower` · `upper` · `initcap` · `trim` · `btrim` · `ltrim` · `rtrim` · `lpad` · `rpad` ·
`left` · `right` · `substr` · `substring` · `substr_index` · `substring_index` · `split_part` ·
`concat` · `concat_ws` · `repeat` · `replace` · `translate` · `overlay` · `reverse` ·
`starts_with` · `ends_with` · `contains` · `position` · `strpos` · `instr` · `find_in_set` ·
`levenshtein` · `ascii` · `chr` · `to_hex` · `encode` · `decode` · `uuid` · `regexp_match` ·
`regexp_like` · `regexp_replace` · `regexp_count` · `regexp_instr`

**Dates and times:** `now` · `today` · `current_date` · `current_time` · `current_timestamp` ·
`make_date` · `make_time` · `date_part` · `datepart` · `date_trunc` · `datetrunc` ·
`date_bin` · `date_format` · `to_date` · `to_time` · `to_char` · `to_timestamp` ·
`to_timestamp_seconds` · `to_timestamp_millis` · `to_timestamp_micros` ·
`to_timestamp_nanos` · `from_unixtime` · `to_unixtime` · `to_local_time`

**Conditionals, structs and types:** `coalesce` · `nullif` · `ifnull` · `nvl` · `nvl2` ·
`greatest` · `least` · `named_struct` · `struct` · `row` · `get_field` · `union_extract` ·
`union_tag` · `arrow_cast` · `arrow_try_cast` · `arrow_typeof` · `arrow_field` ·
`arrow_metadata` · `with_metadata` · `cast_to_type` · `try_cast_to_type` · `version` ·
`input_file_name` · `file_row_index`

### How the catalogue is kept honest

`sdk/python/soak/parity.py` runs on each build against the shipping server. It reads the
server's own catalogue, generates arguments from what each entry says it takes, and calls every
function three ways — over the wire as `psql` sends it, through the binding's `sql()`, and
through `db.fn.<name>()` — comparing the answers **bit for bit**. A disagreement of `1e-16`
between two paths is the difference that makes a figure fail to tie out.

Two things it reports that a green tick would hide. **Its own coverage** — the functions the
generator cannot call are named in the run with the reason: the graph and cube functions need a
declared graph or cube, and `functions()` and `cubes()` are table functions, which return rows
rather than a value. A soak that covers most of a catalogue and prints PASS has made a claim
about the rest. And **every function that refused on all three paths** — that is agreement, and
it means those *answers* were never compared, so the coverage figure would overstate what ran.
Getting that number to zero is what turned up the arguments the generator had wrong: a Sortino
ratio over a series that never fell, a logarithmic return over prices that went negative, a
depreciation period outside the asset's life.

The second half calls every function it can **over a stored column** and checks it against the
same function over a literal built from that row's values — a scalar broadcast against an Arrow
array read by stride. That is the half that found the matrix column losing its shape.

The catalogue is designed to grow to thousands of entries, and
[ADR-0020](adr/0020-the-built-in-function-catalogue.md) governs what may be added and under
what name. The rule worth repeating here is the one that constrains any Excel-compatible
family: **a function named after an Excel one must agree with Excel, including where Excel is
arguably wrong** — the 1900 leap-year bug in date serials, `NPV` discounting from period one.
Where agreement is not achievable the function takes a different name and says why. Named
differently is honest; named identically and subtly different is not, because the disagreement
is found by somebody reconciling to four decimal places at a month-end.

---

## 11. Graph traversal from SQL

**The graph cannot answer on any server you can start.** That is the first thing to say about
it, and it used to be said nine hundred lines later.

`crates/sankhya-server/src/execute.rs:303-306` registers the five graph table functions against
an `Arc<GraphCatalog>` constructed **inline as a temporary argument**:

```rust
sankhya_graph_sql::functions::register(
    &context,
    Arc::new(sankhya_graph_sql::catalog::GraphCatalog::new()),
);
```

No binding, field or handle to that catalogue survives the call, so nothing outside
`session_reaching` can reach it to hydrate an epoch. `session_reaching` runs per session, so
every session gets a brand-new empty map. The only two mentions of `GraphCatalog` in the entire
server crate are those two lines — no publish site, no hydration path, no shared `Arc` on the
wiring struct, in deliberate contrast to the cube path, which threads a real
`Arc<CubeCatalog>` from `crates/sankhya-server/src/wiring.rs:1170` through `wiring.rs:1311`.

So every traversal resolves at planning and fails at execution:

```console
$ psql … -c "SELECT * FROM graph_reachable('payments','acct-1','max_depth=3');"
ERROR:  [SNK-C0001] Error during planning: no graph named 'payments' is registered; known
        graphs are []. Refusing rather than returning no rows: an empty traversal over a graph
        that does not exist reads exactly like one that found nothing
```

That refusal is the right behaviour, and the reason for registering an empty surface rather
than leaving the names unresolvable is sound: `Invalid function 'graph_reachable'` points at a
function this document describes and implies it does not exist, where the message above points
at the real gap. But the surface is **non-functional by construction, not merely unhydrated** —
`GraphCatalog`'s sibling `NotHydrated` state
(`crates/sankhya-graph-sql/src/catalog.rs:105-110`) is unreachable here, because no slot is
ever created.

This disclosure sits at the top of the section because of where it used to sit. The old text
taught traversal with worked SQL for forty-odd lines and disclosed the gap in a table nine
hundred lines further down. A reader following a worked example is not reading the appendix;
they are typing. **A caveat that arrives after the example is a caveat the reader meets as a
failure**, and the failure is the one thing this system spends its effort on not being.

The engine underneath is built and tested — traversal, weighted and *k*-shortest loopless
paths, simple cycles, components, centrality, communities and multiplicative influence, each
bounded and each reporting its own truncation. The population path is `M4`'s carried remainder.

With that said, this is the surface, and it is what a hydration path would light up.

The graph tier holds **no durable state**. An epoch is built by scanning published tables,
carries the snapshot it came from, and is dropped on shutdown. There is no graph write path, so
the graph cannot disagree with SQL: an edge exists because a row exists.

```sql
SELECT p.label, r.depth
FROM graph_reachable('payments', 'acct-1', 'max_depth=3') AS r
JOIN parties AS p ON p.key = r.vertex
WHERE NOT r.truncated;
```

Five functions, registered at `crates/sankhya-graph-sql/src/functions.rs:37-47`:

| Function | What it gives |
|---|---|
| `graph_reachable(graph, from, …)` | What is reachable |
| `graph_shortest_path(graph, from, to, …)` | The cheapest route, and the *k* cheapest loopless ones |
| `graph_time_respecting(graph, from, …)` | Routes whose edges existed **in the order you traverse them** |
| `graph_cycles(graph, …)` | Cycles |
| `graph_influence(graph, from, …)` | Influence from a node |

### Time-respecting traversal is a separate function, not a flag

Static reachability over a temporal graph **over-reports** — it finds routes that time forbids
— and always in that direction. A flag defaulting to off would hand the optimistic answer to
everyone who forgot it, and the optimistic answer looks exactly like the correct one.

```sql
-- Only routes that could have been used in order, with value conservation and dwell bounds
SELECT * FROM graph_time_respecting('payments', 'acct-1',
    'max_depth=6, min_conservation=0.9, max_dwell=86400000000');
```

`min_conservation=0.9` requires each onward edge to carry at least nine tenths of the one
before. Without it, a large edge chains onto a negligible one and the result is called a route.
`max_dwell` bounds how long a path may pause at a vertex — without an upper bound, two
unrelated events years apart join into one path.

### Every row carries its provenance

`epoch`, `snapshot`, `truncated` and `truncation_reason` are **columns**, not query metadata. A
flag beside the result gets dropped by the first projection that does not mention it, and a
short list looks exactly like a short answer.

### Named arguments are an options string

`name => value` is rejected outright by the SQL planner for table functions, and `name = value`
is resolved as a *column* against an empty schema. Only literals reach a table function, so
bounds arrive as `'max_depth=3, min_conservation=0.9'` — with every key checked against a known
set, so a misspelled bound is refused rather than silently taking its default.

---

## 12. Security at the query surface

[`SECURITY.md`](SECURITY.md) is the reference: the choke point, the policy vocabulary,
authentication, the audit chain, the transport postures, and the findings a reviewer needs
together. This section is only the part a person meets *while typing SQL*, because two of its
behaviours look like defects until you know why they are not.

**A table you may not read does not exist.** A statement is authorised before a table is
registered in the session, so one you have no right to is never present and naming it fails to
resolve:

```sql
SELECT * FROM salaries;
-- ERROR: table 'datafusion.public.salaries' not found
```

That is deliberate. "You may not read that" would **confirm the table exists**, and the
difference between it and "no such table" is a working enumeration oracle. The consequence for
you is that a typo and a permission produce the same sentence, and only an administrator can
tell you which you have.

**A row policy cannot be argued out of.** The predicate is conjoined where no provider can
decline it, so a tautology in your query does not widen it:

```sql
-- With a policy of  region = 'north'  on this table:
SELECT count(*) FROM orders WHERE region = 'south' OR 1 = 1;
-- returns only the northern rows
```

The first implementation handed the predicate to the provider as a pushdown filter, and
`MemTable` *declines* filters — so every row came back and the table was secured in name only,
with no error anywhere. Correctness now never depends on the provider cooperating.

The visible consequence of both is the `completeness` column of §7: **your total is the total
of what you may read, and it says so.** That is the whole reason the column exists rather than
being metadata somebody could drop.

---

## 13. Arrow Flight SQL — the bulk plane

> **Served since 2026-08-29**, on its own port. `server.flight_listen` defaults to
> `127.0.0.1:5434`; set it to nothing to turn the bulk plane off.
>
> It is worth recording that this section described a working, tested protocol that **nothing
> served** for the whole of M6 and M7 — a client had nowhere to send a `GetFlightInfo`. It went
> unnoticed because `check-surfaces` looked for crates registering *SQL functions*, which
> Flight does not; widening that check to plain reachability found it in a minute.
>
> Identify yourself with the **sankhya-user** metadata key. A request that does not is refused
> rather than defaulted, for the same reason the wire protocol refuses a connection with no
> user: an unattributable request cannot be audited.

The wire protocol is a **row** protocol: the last step of every query takes columnar batches
apart one value at a time. For an interactive query that costs nothing worth measuring; for a
bulk extract it is the whole cost. Flight SQL does not do that — the client's Arrow buffers are
the same shape as the server's.

```rust
let info = client.get_flight_info(descriptor_for("SELECT id, label FROM orders")).await?;
let ticket = info.endpoint[0].ticket.clone().expect("a ticket");
let batches: Vec<RecordBatch> = FlightRecordBatchStream::new_from_flight_data(
    client.do_get(ticket).await?.into_inner().map_err(FlightError::from)
).try_collect().await?;
```

**Nothing is materialised.** A batch is encoded as it is produced and its memory released as
soon as it is sent, which `FR-API-07` requires. The consequence is a real behaviour change:
**an error can arrive mid-stream.** A row protocol sends its error before the first row or not
at all; this one may have sent a gigabyte first. It reports the failure on the stream rather
than closing quietly, because a truncated stream that ends cleanly is indistinguishable from a
complete one.

**The ticket is a security boundary.** Flight splits a query into planning and redemption, and
the principal redeeming is not necessarily the one who requested. So the decision is made once,
at `GetFlightInfo`, and the ticket carries its outcome — redeeming does not re-plan and does
not re-authorize. It checks only that the presenter is the tenant it was issued to, and the
refusal does not say whose ticket it is.

Tickets expire after five minutes: a ticket names a snapshot, and a snapshot's files are
eventually retired, so an unbounded one is a lease nobody granted.

**Deliberately absent**: `DoPut`, prepared statements, transactions, `DoExchange`. Each returns
`UNIMPLEMENTED` with a reason rather than working differently than it should — and `DoPut`
names which write path to use instead, because a third one with its own semantics would be a
way for the other two to disagree.

See [ADR-0006](adr/0006-flight-sql.md).

---

## 14. The clients: a SQL prompt and the Python binding

### The rule that shapes both

> **An SDK contains no logic the server does not also enforce.** A client may *anticipate* a
> refusal to give a better message, and it may never *be* the refusal.

If the Python binding rejects a cube whose measure declares no rule and a Java binding does
not, then the rule lives in Python, the server is not enforcing it, and the second binding is a
documented way around a correctness rule. The check belongs at the choke point, and a client's
copy is a courtesy that must fail the same way or not exist.

The consequence is that **everything a binding can do, a SQL prompt can do.** `sdk/sql/` is a
peer of `sdk/python/`, not a lesser version of it. Exactly two things are properties of the
client rather than the server:

| | Why |
|---|---|
| Streaming a result larger than memory | `psql` collects; a binding can iterate |
| Getting a refusal as structured fields | The wire carries them; `psql` renders them as text |

**Thin is not the same as sparse.** A binding that omits half the server's capabilities forces
its users into raw SQL for the other half, and a user who has to drop to SQL for cloning will
drop to SQL for everything. So the surface is wide while adding nothing to any of it.

### From a SQL prompt

```
psql -h 127.0.0.1 -p 5433 -U you -d sankhya
```

No password on a development server. Anything that speaks the PostgreSQL wire protocol
connects, so DBeaver, DataGrip, Metabase and the `psql` in your package manager all work.

Eight example files ship under `sdk/sql/examples/`, one per capability, runnable as they stand:

```
psql -h 127.0.0.1 -p 5433 -U you -d sankhya -f sdk/sql/examples/01-connect-and-discover.sql
```

| File | What it shows |
|---|---|
| `01-connect-and-discover.sql` | what is on the server, and how tables are named |
| `02-query.sql` | selection, aggregation, joins, nulls, the date axis |
| `03-cloning.sql` | zero-copy clones, lineage, dependents, and the drop that refuses |
| `04-cubes.sql` | declaring a cube and navigating it |
| `05-feeds.sql` | declared ingest, quarantine, and resuming a halted feed |
| `06-refusals.sql` | one refusal per path, with what each one tells you |
| `07-analytics.sql` | the vector, matrix and statistical surface |
| `08-snapshots.sql` | naming one instant across many tables, reading as of it, and the log underneath |

They assume the fixture warehouse; where one needs a table it creates itself, it creates and
drops it.

**They are gated.** `crates/sankhya-server/tests/sql_examples.rs` runs each file against a live
server statement by statement: a statement preceded by a `-- REFUSES` line must fail, and every
other statement must succeed. Both directions are checked, because a demonstration of a refusal
that quietly starts succeeding is a rule that has been removed and a document that still claims
it.

> **What ungated cost.** `07-analytics.sql` once called `vec`, `vec_add`, `vec_norm` and `mat`,
> none of which existed under those names, and `04-cubes.sql` passed the dimension where the
> measure goes. Both had been reviewed; one carried a written note claiming it had been
> verified against a live server. A `psql` script with `ON_ERROR_STOP off` prints its errors
> and keeps going, so a wall of output reads as success.

### From Python

```
pip install -e sdk/python
```

There is nothing to compile. The binding is **pure Python** by decision, not by accident:
[ADR-0017](adr/0017-the-client-contract.md) Decision 7 refuses a compiled extension, because a
per-platform wheel matrix and an ABI across three interpreter versions is a large price for
accelerating a layer that is required to contain no logic. The wire-protocol module imports
`socket` and `struct`; a laptop with a stock interpreter and no build toolchain can connect.

> *If the client is thin, its language does not matter. If its language matters, it is not thin
> enough.*

**Two ways in, and choosing the wrong one is the mistake this section exists to prevent.**

| Call | Returns | Has |
|---|---|---|
| `sankhya.open(...)` | `Sankhya` (`sdk/python/sankhya/client.py:835`) | Every capability as a method — the one to use |
| `sankhya.connect(...)` | `Connection` (`sdk/python/sankhya/wire.py:418`) | `execute()` and the raw wire, and nothing else |

`schemas()`, `tables()`, `columns()`, `clone()` and the rest live on `Sankhya`
(`sdk/python/sankhya/client.py:305`, `:313`, `:367`). They are **not** on `Connection`.
`sdk/python/QUICKSTART.md` opened a `connect(...)` in its §2, named the result `db` — the name
every other section uses for a `Sankhya` — and then called `db.schemas()` in its §5, so a
reader copying both sections in order got an `AttributeError` from a document whose examples
were otherwise gated. Nothing caught it because no test extracts that file's code blocks.

`connection` is deliberately public on `Sankhya`: a binding that hides the wire forces its
author to anticipate every statement anybody will ever want, and the ones they did not
anticipate become impossible rather than merely unnamed.

```python
import sankhya

with sankhya.open(host="127.0.0.1", port=5433, user="you", database="sankhya") as db:
    print(db.version())
    print([t.qualified for t in db.tables(schema="sales")])
    for column in db.columns("sales.orders"):
        print(" ", column.name, column.type_name, "null" if column.nullable else "not null")
    for row in db.rows("SELECT region, count(*) AS n FROM sales.orders GROUP BY region"):
        print(" ", row)
```

**Use port 5433.** The binding's own default is `5432` (`sdk/python/sankhya/client.py:837`,
`sdk/python/sankhya/wire.py:140`), as is `sdk/python/examples/_common.py:29`, and the server's
default is `5433` (`crates/sankhya-server/src/main.rs:159`). The examples pass their gate
because `crates/sankhya-server/tests/sdk_examples.rs` injects `SANKHYA_PORT` explicitly, so the
tests are green and the documented default is wrong. That is a **code** change — the binding's
default should move to 5433 — and it is named here rather than papered over, because a wrong
default that CI hides is exactly the defect a reader hits on their first line and a maintainer
never does.

Values arrive as **strings**, and `None` is SQL `NULL`. The empty string and `NULL` are
different values and stay different — conflating them is a wrong answer, not a formatting
choice. Counts distinguish `None` from `0` throughout: *"not recorded"* and *"recorded as
none"* are different answers, and collapsing them would report a clone as reading version 0 of
its origin.

The surface, by area:

| Area | Methods |
|---|---|
| Raw | `connection`, `sql`, `scalar`, `one`, `rows`, `execute` |
| Discovery | `version`, `settings`, `schemas`, `tables`, `columns`, `exists`, `functions` |
| Functions | `fn.<name>(...)` — every catalogue entry, generated from `functions()` |
| Cloning | `clone`, `drop`, `lineage_of`, `dependents_of`, `is_clone` |
| Cubes | `create_cube`, `cubes`, `cube_dimensions`, `cube_measures`, `rollup`, `slice`, `drop_cube` |
| Snapshots and history | `take_snapshot`, `snapshots`, `read_as_of`, `read_the_present`, `drop_snapshot`, `history`, `read_version`, `read_the_present_of` |
| Feeds | `feeds`, `resume_feed`, `quarantine` |
| Graph | `reachable`, `shortest_path`, `cycles`, `influence`, `time_respecting` |

```python
db.fn.norm_inv(0.975)                                   # 1.9599639845400367
db.fn.mat_cholesky([4, 12, -16, 12, 37, -43, -16, -43, 98])
db.fn.regress_stderr(x, y)
db.functions(category="linear algebra")                 # the catalogue, as records
```

No stub is written per function. Hundreds of them would be thousands of lines whose only job is
to agree with the server, and they would stop agreeing the first time one was added and a stub
was not — silently, because a missing method is not an error until somebody calls it. The
binding reads the catalogue once per connection and offers exactly what came back. It validates
nothing — not the argument count, not the domains — so a wrong call produces the server's own
refusal, which is the one that knows why. What it adds is *encoding*: a Python list becomes a
SQL array going out and a list coming back. A **typo in a name** is the binding's to catch,
because that one it can answer without a round trip.

`create_cube` takes the `CREATE CUBE` statement verbatim rather than building it from Python
objects, because a builder would be a second definition of what a cube is — exactly the
divergence the rule exists to prevent. The cube navigations take the measure first and the
dimension second, exactly as the server's own functions do:

```python
db.rollup('sales', 'amount')                       # the grand total
db.rollup('sales', 'amount', by='region')          # a breakdown by region
db.slice('sales', 'amount', where='region:north')  # one member fixed
db.rollup('sales', 'amount', by='region', min_completeness=0.5)
```

> **How this was wrong, and how it was found.** Both methods once took `(cube, by=…)` and
> passed `by` into the **measure** position, so `db.rollup('c', 'region')` was refused with
> *"cube 'c' has no published cells for measure 'region'"* and there was no way to say `by=` at
> all. Keyword options had their **names discarded**, so `opts='by=region'` worked and so did
> any other spelling — a parameter that accepted anything and meant nothing. Nothing caught it:
> the methods were covered, the gate was green, and the docstring described the opposite of
> what the code did. It was found by *writing a runnable example*, which is why those examples
> are now a test. The graph methods had the same defect for the same reason, so
> `shortest_path`'s destination — a **positional** argument of the server's function — worked
> only while it happened to be the first keyword given.

Twelve example scripts ship under `sdk/python/examples/`, each runnable on its own and each
gated by `crates/sankhya-server/tests/sdk_examples.rs`: every one must exit zero and say
nothing on stderr. `sdk/python/examples/README.md` names the four defects writing them found.

### A refusal must cross the wire as data

The contract asks for four things, because each is expensive to add later:

| Field | Why it cannot be folded into the message |
|---|---|
| `code` | The stable identity a client dispatches on |
| `sqlstate` | What a generic driver on the other door understands |
| `remediation` | The half that says what to do |
| `subjects` | The **names** a refusal cites — clones, partitions, dimensions — as a list |

**What actually crosses today is the PostgreSQL error envelope, and it carries two of the
four:**

```python
>>> try: db.sql("SELECT * FROM sales.ordres")
... except sankhya.Refusal as e: e.fields
{'S': 'ERROR', 'V': 'ERROR', 'C': '42P01',
 'M': "[SNK-C0001] Error during planning: table 'datafusion.sales.ordres' not found",
 'D': 'Correct the statement. The detail names the offending element.'}
```

`sqlstate` is a field and `remediation` arrives as `D`, exposed as `.detail`. **The stable code
is inside the message text**, so a client that wants to dispatch on `SNK-C0001` must extract it
with a regular expression, and `Refusal` has no `code` attribute. **`subjects` does not cross
at all.** And on the statement paths added in M10 and M13 — clone, lineage, feed — the envelope
carries neither a code nor a remediation, only prose that names the clone that would break. The
only way a client can show that name is by parsing the sentence, which is precisely the outcome
the contract forbids.

### A long operation is a commit, not a job handle

Materialising a cuboid, cloning a large table, ingesting a file and taking a backup can each
outlast a sensible timeout. The obvious design is a job registry: return a handle, poll it.
**Refused, for now** — a registry is a second durable state machine, with its own reclamation,
its own authorization and its own answer to *"what happens when the server restarts mid-job?"*
This system already has exactly one durable record of what happened, and it is the log.

| Question | Answer |
|---|---|
| Did my clone happen? | The table exists, or it does not |
| Did the cuboid materialise? | It is present at *(definition version, snapshot, scope, cuboid)*, or the next query recomputes |
| Did the ingest land? | The table's version moved, or it did not |

Two obligations are the price: **every long operation is idempotent under re-issue**, or
refuses naming the object it found; and **no operation leaves a state only the disconnected
client could describe.**

### What a client must never do

| Never | Because |
|---|---|
| Cache an authorization decision | A grant revoked between two calls must take effect on the second |
| Validate what the server validates | The check moves into one binding and out of the others |
| Retry a non-idempotent operation | A clone or an ingest retried after a timeout is a second one |
| Materialise a stream to make an API tidy | It turns a working query into a client-side kill |
| Reconstruct a refusal from its message text | The message becomes an API nobody meant to publish |
| Reach the filesystem the server uses | There is one write path, and a client is not it |

### What the binding cannot do yet, by name

Named rather than half-implemented, because a client that silently downgrades is worse than one
that says it cannot.

| Not yet | What it costs you |
|---|---|
| **Arrow / columnar results** | Large results come back as text rows, which is slower and larger |
| **Streaming** | `execute` collects. `Connection.stream` yields one `Result` per *statement answer* — the simple query protocol allows several, and a client returning only the first would silently drop the rest — but it accumulates every row of a statement before yielding it. A result larger than memory will not fit |
| **Parameter binding** | The binding speaks the simple query protocol only, so values are interpolated as literals. The server has served the extended protocol since 2026-09-02 and the binding does not use it; the package has exactly one quoting helper and says it goes away when it does |
| **Structured refusal fields** | No `code`, no `subjects` |
| **Version negotiation** | The contract carries a version and a mismatch should be refused at connection, naming both. The Python startup exchange sends the protocol version and a user and negotiates no contract version at all |
| **Ingest from the client** | `M14`; streaming ingest is `M15` |
| **Java and Rust bindings** | `M16` — named now so the contract is written for three bindings rather than retrofitted to them |
| **Federated identity** | A `Principal` is a fixed tenant; which identity providers are supported, and how a token's claims map, waits for a deployment with an opinion |

TLS is built: `sslmode=` takes PostgreSQL's own vocabulary, `db.connection.encrypted` says what
happened, and `require` refuses a server that declines.

Almost every gap above is *server-side work a client merely surfaces*. That is what the rule at
the top of this section forces, and it is why M14 is not "write a Python package".

> **No client can be honestly tested against a mock.** A binding whose tests mock the server
> tests its author's belief about the server. The gate runs it against a real one, which makes
> the client's test suite slower and worth having.

---

## 15. Extensions, packs, and an aggregation of your own

### The boundary

```text
   packs/    risk · financial-crime · telemetry · logistics
             (and anything a third party writes)
                        |  may depend ONLY on:
                        v
   sankhya-ext   the published extension API
                 - SANKHYA's OWN function traits
                 - a curated, pinned Arrow subset
                 - the logical-type registry
                        v
   the core - knows nothing about any domain
```

The extension API defines **its own** function traits and re-exports only a curated Arrow
subset. Re-exporting the query engine's traits directly would break every pack in existence on
every engine upgrade, several times a year. `sankhya-ext` is the only crate in this workspace
carrying a stable-version commitment while everything else is pre-1.0, and it is under a
thousand lines. Both facts are deliberate and the second protects the first.

A pack may depend on `sankhya-ext`, `sankhya-types` and `sankhya-error`, and on nothing else.
That is enforced by `check-layers`, which refuses any pack dependency outside those three and
any core dependency on a pack in the other direction. Packs **self-register**: no core crate
contains a dispatch on pack identity, so the rule cannot be quietly circumvented by
"temporarily" adding a branch.

A pack may contribute table and schema definitions, logical types, scalar/aggregate/window
functions, graph algorithms, view and materialized-view definitions, rules and detectors, named
parameterized endpoints, and policy vocabulary. Nothing outside that set.

The **logical-type registry** resolves an otherwise intractable tension. The core forbids bare
primitives in public signatures, but a pack must be able to define its own types, which the
core cannot name. The resolution is that the core moves Arrow arrays paired with an **opaque
logical-type identifier**, and the pack owns validation, coercion and formatting.

An extension API rots by accretion rather than by breaking, so the mechanisms against that are
structural: a hard size budget; the **two-domain rule**, under which nothing enters until two
packs *from different domains* need it; no escape hatches, so type-erased downcasting,
free-form document values and open-ended string maps are prohibited; a restricted dependency
allowance, where a pack legitimately needing more makes the build fail and that failure *is*
the signal the API has a gap; compiling examples on every public item, so bloat acquires a
visible recurring cost; use it or lose it; and mechanical breaking-change detection rather than
a reviewed diff. The two-domain rule does the most work and is the hardest to hold, because the
request always arrives as *"just this one accessor"*.

### Three tiers

| Tier | Form | Sandboxed | Hot-reload | Build cost |
|---|---|---|---|---|
| **Declarative** | Signed bundle: schemas, views, SQL functions, rules, policy vocabulary, endpoints. **No code** | It is data | Yes | None |
| **Sandboxed module** | Compiled to a portable sandboxed target | Full: fuel metering, memory cap, deadline interruption, no ambient authority | Yes | None to the server |
| **Compiled** | Built into the binary behind a feature | **None** — pack code is core code | No | The only tier that adds build time |

The declarative tier's expression language has **no loop, no recursion, no call and no I/O**,
so a declarative function *cannot* be the one that hangs a query. An expression language with
loops is a programming language, and one loaded from a configuration file is a remote code
execution feature with extra steps.

Reloading a bundle set is **atomic or it does not happen**. A single bad file leaves the
previous set entirely in place, because a reload that half-applies means some queries see the
new definitions and some the old, with which one depending on timing.

**Dynamically-loaded native extensions are rejected**, and the reasons are recorded so the
decision is not relitigated annually: no stable binary interface; a version mismatch is
undefined behaviour rather than an error; a fault kills the process with no isolation; the
entire Arrow type surface would have to be projected across the boundary; and every extension
would need a per-compiler-version build matrix.

A pack function in a deliberate infinite loop that *ignores* the cancellation flag is stopped,
and the query fails naming the pack rather than hanging. The call runs on its own thread and is
**abandoned** when the bound passes: Rust has no safe way to kill a thread and this repository
has no unsafe code, so a genuinely non-terminating function leaks one thread until the process
ends. `Sandbox::abandoned()` counts them, so a pack that does this is **visible rather than
suspected**. The alternatives are worse — hanging the query for ever, or killing a thread
mid-allocation and corrupting the allocator for everything else.

### An aggregation of your own

The catalogue is what a hundred people need. The rule that is *this* firm's — a weighted
average with their weighting, an exposure netted their way — will never be in it.

> **Off by default.** `CREATE AGGREGATION` runs code the caller supplied, so a server refuses
> it unless an operator sets `server.user_functions: true` (or `SANKHYA_USER_FUNCTIONS=true`).
> The refusal carries `42501` and names the setting. `SHOW AGGREGATIONS` answers either way.

```
CREATE AGGREGATION weighted_mean LANGUAGE PYTHON AS $$
def accumulate(state, values): ...
def merge(a, b): ...          # optional; its presence is the claim that partials compose
def finish(state): ...
$$;
```

The contract is the literature's — Gray, Chaudhuri, Bosworth *et al.*, *Data Cube* (ICDE 1996):

| Gray's term | Definition | This system's rule |
|---|---|---|
| **Distributive** | Computable from partitions by applying the function to each and combining | `Sum`, `Min`, `Max`, `First`, `Last` |
| **Algebraic** | Computable from a **bounded** intermediate of *M* distributive aggregates | `Mean` — sum and count |
| **Holistic** | No constant bound on the intermediate exists | `None` — ratios, distinct counts, percentiles |

A bare callable cannot answer the question the cube model asks. Given a function of a list of
values, the system cannot know whether combining its answers over two partitions equals its
answer over the union. It has two choices and both are bad: assume it composes, and produce
plausible wrong numbers from materialised ancestors; or assume it does not, and give up
roll-up, the lattice and materialisation for that measure. So **`merge` earns the lattice** —
its presence is the composability declaration, and no `merge` means `Rule::None`. **The state
is what materialises, not the number**, because a float cannot be rolled up further without the
rounding bit-identity forbids. And **determinism is exercised, not trusted**: the same input,
accumulated in one batch and in several, merged in two groupings, compared by bits, with a
failure refused at declaration and the two answers shown side by side.

Adopting the contract also closes a gap in the built-in rules. `composes()` is currently true
for exactly Gray's distributive set and false for `Mean`, so the model treats algebraic and
holistic the same and sends both to base data. But `Mean` is algebraic — it composes perfectly
well given the intermediate *(sum, count)*, which is precisely what a state plus a merge is.
With the contract in place the refusal narrows from *"not distributive"* to *"genuinely
holistic"*, which is where it belongs.

An aggregation occupies a **rule slot, not a measure**. A measure declares a rule *per
dimension* — a closing balance is `Last` along time and `Sum` along entity — so there is no
single "does this compose" answer. This is the one place the borrowed contract does not fit
unchanged.

Decided 2026-08-31: it runs **out of process, behind Arrow IPC**. A user's aggregation is the
one part of a query this system did not write; in-process it shares an address space with the
audit chain and with every other tenant's data, a panic is a server, and an infinite loop is an
outage. A sidecar that panics is a sidecar that dies, and the query fails with a typed error
naming the aggregation. Three obligations follow, none optional: **a registered aggregation
names its sidecar**, and a query planned against it fails closed when that sidecar is absent;
**a sidecar that dies mid-query fails the query**, because partial state is not an answer; and
**the determinism exercise runs before the aggregation is trusted**, not on first use in anger.

It also runs inside the trust boundary of the data it sees — an aggregate is computed over the
rows a principal may read, so a function that can open a socket is an exfiltration path with a
legitimate-looking name. No network, no filesystem, no subprocess; a declared, pinned set of
importable modules; a wall-clock and memory bound per call; and the function's version
participates in the cache and materialisation keys, because a changed function is a changed
answer and serving a cuboid computed by the previous version is the same defect as serving one
from the previous snapshot. `accumulate` takes a batch rather than a row, because per-row calls
across millions of cells spend their time in the interpreter boundary and the data is already
Arrow on both sides.

It costs a process boundary per batch — two to three orders of magnitude against a compiled
built-in, which is exactly why the built-in catalogue in §10 is worth its size.
[ADR-0010](adr/0010-external-aggregations.md),
[ADR-0022](adr/0022-user-defined-functions.md) and
[ADR-0023](adr/0023-the-sandbox-a-user-function-runs-in.md) hold the decisions.

### Proving the core is actually general

Four mechanisms of increasing strength: a **naming lint** rejecting domain vocabulary in core
identifiers, filenames and documentation; a **pack-free build** of the full core test suite; two
**reference packs, deliberately opposite** — `pack-ref-telemetry`, high-volume and
time-series-shaped, and `pack-ref-logistics`, entity-heavy with a physical network graph,
neither financial, and the change that adds one must touch **zero core files**; and an
**adversarial pack** attempting what packs must not be able to do — read another tenant's data,
escape its sandbox, register a non-terminating or panicking function, exceed its budget, shadow
a core name, claim an API version the engine does not offer. Seven attempts, seven named
refusals.

> The lint catches leakage; the reference packs catch shape. A core can be immaculately neutral
> in its naming and still be structurally bent toward one domain — which is exactly what
> happened to the graph model during review, and exactly what no lint would have caught.

What ships for trust is **digest pinning**, not signing: an operator pins the digests of
bundles they have reviewed and anything else is refused, including everything when nothing is
pinned, because a trust policy that defaults to trusting is not a policy. **A digest proves the
bytes are the bytes you pinned; it proves nothing about who wrote them.** The verifier is a
trait, so adding public-key signing later changes no caller.

### What is not built

**The loader is not wired into the server.** Packs load into a registry; nothing in a running
process does that. The consequence is directly observable:

```console
$ psql … -c "SELECT logistics_check_digit('abc');"
ERROR:  [SNK-C0001] Error during planning: Invalid function 'logistics_check_digit'.
Did you mean 'to_timestamp_seconds'?
```

That function exists, is tested, and is not reachable from any door.

**Out-of-process aggregations are `M14` and are not built.** The contract is decided, the
sidecar is not; there is no `register` call to make and no Arrow IPC channel to make it over.

> Two things here are easy to conflate and are a milestone apart. **A pack** contributes
> definitions and functions and is loaded from a bundle; that mechanism is built and unwired.
> **An external aggregation** contributes a rule slot in a cube and runs in a sidecar; that
> contract is decided and unbuilt. Neither is reachable from a running server today, and the
> reasons they are not are different reasons.

---

## 16. Verifying and repairing a table

```console
$ sankhya-publish verify ./warehouse/sales/orders
external table, 4 file(s), all with statistics — nothing to report
```

Verification does **not** assume the publishing library was used, because making it the
supported path is a recommendation and a recommendation is not an invariant. It reports *what*
is wrong rather than *whether*:

```console
$ sankhya-publish verify ./warehouse/sales/broken
external table, 4 file(s), 0 with statistics — 4 finding(s), 0 affecting correctness
  [slow] part-0000.parquet cannot be pruned, so every query reads it. …
```

Findings that make queries **slow** and findings that make them **wrong** are distinguished,
and the exit code follows: `0` clean, `1` slow, `2` wrong. A build gate can fail on one and not
the other, because a table that is merely slow can wait until Monday.

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

A tool that guesses is worse than no tool: it writes a plausible invented value into the table
permanently, with an operator's confidence attached, because a tool said it was fixed. Nobody
re-checks a table a tool reported as repaired.

Three properties make it safe to point at production: **it never deletes**, it **repairs by
appending** so the broken commit stays readable and the repair is revertible, and it **does
nothing by default** — the commonest way to run a repair tool is by accident, on the wrong
directory, at three in the morning.

---

## 17. What a failure tells you

Everything else an operator needs — the diagnostic, the metrics endpoint and its label rules,
maintenance, backups and the restore drill, the archive attestation — is
[`OPERATIONS.md`](OPERATIONS.md), and that is the only copy. This section is the half a person
meets at a prompt: the shape of a refusal, and what to do with each part of it.

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

Refusals you will meet, verified on a running server:

| Statement | SQLSTATE | Code | `DETAIL` |
|---|---|---|---|
| a table that does not exist | `42P01` | `SNK-C0001` | present |
| a column that does not exist | `42703` | `SNK-C0001` | present |
| `INSERT INTO …` | `0A000` | `SNK-C0006` | present |
| `SELECT 1/0` | `22012` | `SNK-C0007` | present |
| `mat_of` with a wrong element count | `42601` | `SNK-C0001` | present |
| `DROP TABLE` on a clone's origin | `22000` | **none** | **empty** |
| `RESUME FEED` on a feed nobody declared | `42704` | **none** | **empty** |
| `SHOW LINEAGE OF` a table that is not there | `42P01` | **none** | **empty** |

The bottom three rows are the honest finding. The *messages* on those paths are among the best
in the system — they name the clones that would break, they point at `SHOW FEEDS` — and the
*envelope* is not built: no stable code to dispatch on, no remediation field, and a generic
SQLSTATE. A client can only get at those names by parsing prose, which turns the message into
an API nobody meant to publish.

One cosmetic wart in the same family: the `RESUME FEED` refusal arrives with a run of literal
spaces inside it, the signature of a Rust string continuation that kept its indentation.

And one refusal that is not a defect and reads like one: **a table that does not exist and a
table you may not read are the same error** — the same code, the same state, the same words.
§12 says why.

---

## 18. What is not built

Stated explicitly, because a guide that implies more than exists is worse than one that admits
less. [`STATUS.md`](STATUS.md) is the authoritative version.

| | |
|---|---|
| **The graph's population path** | Not built, and the surface is unreachable rather than merely unhydrated — §11 has the two lines of code that make it so |
| **The gRPC transport, and every write path on the control plane** | Not built. The gateway's route table and the size decision `FR-API-06` turns on both exist and are tested; wiring them to tonic and to an audited write path is the remainder. Jobs and archive operations are absent on purpose — with no scheduler, a jobs endpoint would list nothing forever and a client could not tell that from a system with nothing to list |
| **A REST/JSON surface** | Refused by design rather than pending. §1 |
| **`cube_dice`, `cube_pivot`, a drill-down** | Not registered. `dice` and `pivot` exist as kernels; only `cube_rollup` and `cube_slice` reach SQL. §7 |
| **A vector or matrix column *type*** | Not built. No DDL, no parser, no logical type; the width is enforced on the publish path only; there is no refusal for cross-width equality or for `ORDER BY embedding`; no vector index exists. [ADR-0021](adr/0021-vectors-matrices-across-the-tiers.md) decides all of it and §8 says which parts are code |
| **Compensated accumulation in the decompositions and the special functions** | Not built. They are reproducible and uncompensated; §9 is the list and the evidence |
| **A declared shape reaching the linear-algebra family** | Not built. Six functions take the square root of the array's length unconditionally, and two of them force a rectangular algorithm into a square shape. §8 |
| **Backing up the transactional store** | Not built, and deliberately not planned as this system's job. The manifest binds to a PostgreSQL backup taken by your own tooling |
| **Distributed tracing** | Not built. Metrics and the error catalogue exist; spans do not |
| **A multi-day soak** | Not run. The harness exists, is proven to detect a leak, and runs short on every build — a forty-five-minute run at twenty gigabytes passes with resident memory flat; see [`SOAK.md`](SOAK.md). The scheduled run moved to M12 with the rest of the scale-out work |
| **Container images and signing** | Not built. The platform baseline and the manifests' termination grace are checked; the artifacts a release pipeline produces are not |
| **Ingest on a timer** | Not built. Capture, apply and publication all work and none of them is driven by a running process, so everything the server serves is already published |
| **The pack loader in the server** | Not built. Packs load into a registry; nothing in the running process does that. §15 |
| **Out-of-process aggregations** | `M14`. The contract is decided and the sidecar is not. §15 |
| **An ephemeral cube lifetime** | `M14`. Ephemeral is the *intended* default and is not what `CREATE CUBE` does today. §7 |
| **Federated identity** | `M14`. A `Principal` is a fixed tenant; mutual TLS puts a client certificate where a door can see it without anything deriving an identity from it |
| **Bloom filters, the result cache** | Not built. **Partitioning is** — every published table writes `sank_data_date=YYYY-MM-DD/` directories and the log's `add` paths carry them. This row claimed otherwise until an adversarial review checked it on 2026-09-01, contradicting §4 of this same document |
| **Most of `FR-OPS-16`'s checks** | Not built. [`OPERATIONS.md`](OPERATIONS.md) §9 lists the four the diagnostic runs, and names the one that is exported and called by nothing |
| **Lifecycle tiering's destructive half** | Partly built, and **gated**. `sankhya-tiering` holds 16 modules and about 5,100 lines of source (8,581 including its tests): the policy model, eligibility, canonical encoding, verification, quarantine, rehydration and the attestation drill. What is not built is purge from the source. **Destructive purge stays disabled until reconciliation has run clean in production**, which is a separate milestone. Building the purge path and arming it are two decisions |
| **Multi-node: leader election, executor scale-out, failover, replication** | Not built, and not reachable here. All of it moved to M12 on 2026-08-30, because proving it needs a second machine and a recovery objective measured on one host would exclude the failures the criterion exists to price |

The honest summary, and it applies to the whole document: **the correctness contracts are built
and tested, and the machinery that runs them continuously is not.** Every capability above is
exercised by the test suite; several are not yet exercised by a process you can start.

---

## Where to go next

| | |
|---|---|
| [`TUTORIALS.md`](TUTORIALS.md) | The same ground, walked step by step from an empty prompt |
| [`QUICKSTART.md`](QUICKSTART.md) | Build it, load ten gigabytes, watch capture reconcile |
| [`OPERATIONS.md`](OPERATIONS.md) | Running one: configuration, metrics, the diagnostic, maintenance, backup and the drill |
| [`SECURITY.md`](SECURITY.md) | The choke point, the policy vocabulary, authentication, audit, and the gaps by name |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Why it is shaped this way, and the measurements behind the choices |
| [`STATUS.md`](STATUS.md) | What is built, what is measured, and the defects found along the way |
| [`GLOSSARY.md`](GLOSSARY.md) | The coined terms, the milestone names and the finding codes |
| [`ERRORS.md`](ERRORS.md) · [`METRICS.md`](METRICS.md) · [`INVARIANTS.md`](TESTING.md) | The catalogues, generated and checked against the code |
| [`adr/`](adr/) | The decisions, each with the question that prompted it and what it costs |

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
