# 19. The SQL surface

> This chapter is the complete catalogue of what you can say to a SANKHYA over a socket: which
> protocol, which catalogue queries, the cube statements and their five navigations, the clone
> statements, the feed commands, and every vector, matrix, statistical and graph function. Its
> central claim is that **the SQL surface is the whole product** — a client binding saves typing and
> unlocks nothing — so this chapter is also the specification a second binding would be written
> against. Every function and statement below was executed against a live server, and the three that
> behave differently from the way the repository's documentation describes are named where they sit.

---

## 19.1 Two doors, one protocol each

Door | Protocol | What it is for
---|---|---
Wire protocol | PostgreSQL 3.0, default port 5433 | Tools nobody wrote for this system: `psql`, a notebook's driver, a BI product
Arrow Flight SQL | gRPC, default port 5434 | The bulk plane — Arrow-native end to end, streamed by construction

A REST/JSON API is deliberately **not** a third door. The engine is columnar and typed; a
row-oriented JSON surface converts twice, loses the type distinctions the storage layer spent effort
preserving — a `Decimal(38,9)` becomes a double or a string, and both are wrong in different ways —
and would need its own pagination, its own error shape and its own authorization path. That is a
second product surface maintained forever to avoid a dependency the client already has.

Both **simple** and **extended** query protocols are served. The extended one — what most *drivers*
use by default — was implemented recently and is exercised here through `psql`:

```console
$ psql … -c "SELECT count(*) FROM common.orders WHERE region = \$1" -- fails: no value bound
ERROR:  [SNK-C0007] Execution error: Placeholder '$1' was not provided a value for execution.

$ echo "SELECT count(*) FROM common.orders WHERE region = \$1 \bind north \g" | psql …
 count(*)
----------
      334
(1 row)
```

> **Pitfall** — That path works and **no third-party driver has been tested against it**. JDBC,
> psycopg, pgx, npgsql and ODBC all use the extended protocol by default, and until `M6` this server
> acknowledged `Parse`, acknowledged `Bind`, answered `Describe` with `NoData` and had no `Execute`
> arm at all — three cheerful acknowledgements and then a dead socket. The arm exists now. The
> compatibility matrix does not. One difference is known and deliberate: a statement runs at
> `Describe` time, earlier than PostgreSQL would run it, because `Describe` needs the column names
> and `Execute` needs the rows, and running twice would answer from two different snapshots.

## 19.2 The catalogue

Both spellings of the same question are recognised — `information_schema`, which is what JDBC uses,
and `pg_catalog`, which is what `psql`'s backslash commands use — because matching only one works for
the client it was written against and fails for the next.

```sql
SHOW server_version_num;                                  -- 170000
SELECT schema_name FROM information_schema.schemata;
SELECT table_schema, table_name FROM information_schema.tables;
SELECT column_name, data_type, is_nullable FROM information_schema.columns
  WHERE table_schema = 'common' AND table_name = 'orders';
```

`psql` metacommands, as they behave in this build:

Command | Behaviour
---|---
`\dt`, `\dn` | Work
`\conninfo` | Works
`\d <table>` | **Fails** — `psql` issues a `pg_class` query whose answer it cannot use: *"column number 3 is out of range 0..2"*
`\l`, `\du` | **Fail** — `pg_catalog.pg_database` and `pg_catalog.pg_roles` are not served
`\df` | Answers, with the schema list rather than a function list
`\dv` | Answers, with every base table rather than the views

> **Pitfall** — The catalogue relations answer with **their own projection**, not the one you asked
> for. `SELECT column_name, data_type, is_nullable FROM information_schema.columns` returns six
> columns beginning with `table_schema`. Read a catalogue result **by column name, never by
> position**: this is exactly how the Python binding once returned table names where column names
> belong, and it is the first thing to get right in any new client.

## 19.3 Queries, and what a read path refuses

Ordinary `SELECT` is ordinary: projection, predicates, `GROUP BY`, `ORDER BY`, `LIMIT`, joins,
scalar subqueries, `CASE`. Nulls are nulls and are distinct from the empty string all the way to the
wire. A projection over an empty table returns no rows rather than failing, and an aggregate over one
returns `count(*) = 0` with a null `sum`.

Data definition and data modification are **refused, never accepted and discarded**:

```console
$ psql … -c "INSERT INTO common.orders (id) VALUES (1);"
ERROR:  [SNK-C0006] the statement uses a feature this build does not implement: data modification is
        not served over this connection; this server is a read path over a published warehouse
DETAIL:  Write to the transactional store and let capture publish it, or publish an external table
         with `sankhya-publish`. See GUIDE.md §3.
```

Two exceptions to that rule are statements, not writes to rows: `CREATE TABLE … CLONE` (§19.7) and
`CREATE CUBE` / `DROP CUBE` (§19.5). Both change catalogue state and neither writes a row.

Table naming follows one rule: **the qualified name always resolves; the bare name resolves while
only one schema holds a table of that name**, and stops the day a second one does, naming both
candidates rather than answering from whichever registered first. Chapter 18, *Getting started*,
§18.6 has the transcript.

Every table carries `sank_data_date`, of type `DATE`, and is partitioned on it:

```sql
SELECT sank_data_date, count(*) FROM common.orders GROUP BY sank_data_date ORDER BY 1;
```

The type is `DATE` and not an encoded integer, because partition paths are `sank_data_date=2024-03-01`
which Spark and Trino parse natively — and because `20240301 - 7 = 20240294` is not a date, raises no
error, and is a thing people write. Chapter 7, *The date axis*, has the rest.

## 19.4 Refusals you will meet, with their codes

Every error that reaches a client should carry a permanent `SNK-` code, a SQLSTATE derived from its
class, and the catalogue's own remediation as `DETAIL`. Verified on the running server:

Statement | SQLSTATE | Code | `DETAIL`
---|---|---|---
`SELECT * FROM common.no_such_table` | `42P01` | `SNK-C0001` | present
`SELECT no_such_column FROM common.orders` | `42703` | `SNK-C0001` | present
`INSERT INTO common.orders …` | `0A000` | `SNK-C0006` | present
`SELECT 1/0` | `22012` | `SNK-C0007` | present
`SELECT mat_of(2,3,1.0,2.0,3.0,4.0,5.0)` | `42601` | `SNK-C0001` | present
`DROP TABLE <clone origin>` | `22000` | **none** | **empty**
`RESUME FEED no_such_feed` | `42704` | **none** | **empty**
`SHOW LINEAGE OF nothing_here` | `42P01` | **none** | **empty**

The bottom three rows are the honest finding of this chapter. The *messages* on those paths are among
the best in the system — they name the clones that would break, they point at `SHOW FEEDS` — and the
*envelope* is not built: no stable code to dispatch on, no remediation field, and a generic SQLSTATE.
A client can only get at those names by parsing prose, which turns the message into an API nobody
meant to publish. Chapter 14, *Observability*, §14.5 places it beside the identical defect the wire
path had before `M6`.

One cosmetic wart in the same family: the `RESUME FEED` refusal arrives with a run of literal spaces
inside it — ``no feed called `x` is declared on this server. `SHOW FEEDS`                lists them``
— the signature of a Rust string continuation that kept its indentation.

## 19.5 Cubes

### Declaring one

```sql
CREATE CUBE <name> FROM { <fact table> | ( <query> ) }
  DIMENSION <dim> FROM <member table> ON <join column> (
      LEVEL <level> = <column>[, LEVEL … ]
    [ , PARENT <child column> TO <parent column> ]
    [ , ROLLUP <member> TO <member> ]
  )
  [ DIMENSION … ]
  MEASURE <measure> ( <RULE> ALONG <dim> [, <RULE> ALONG <dim> ] )
  [ MEASURE … ]
  [ MAINTAINED WITHIN <n> VERSIONS ]
  [ PINNED ( <dim>[, …] ) ]
```

All of the optional clauses parse and were exercised. Levels run coarse to fine, which is the order a
drill-down walks; a ragged hierarchy — an org chart, where depth varies — is declared as
`PARENT … TO …` rather than as levels, because flattening it forces padding.

A schema-qualified fact table is written plainly — `FROM common.orders`. It used to fail at the
dot, which made a cube undeclarable on exactly the warehouses where qualification exists.

**The facts may be a query instead of a table**, parenthesised:

```sql
CREATE CUBE joined FROM (
    SELECT o.amount, r.area FROM sales.orders o JOIN sales.regions r ON o.region = r.region
)
  DIMENSION area FROM sales.regions ON area (LEVEL area = area)
  MEASURE amount (SUM ALONG area);
```

Nothing else about the cube changes, and that is the point:
[ADR-0012](../../adr/0012-open-capabilities.md) makes the generalisation small under one rule —
*an artefact that cannot say what it needs cannot be cached correctly, checked against policy, or
bounded.* A query's **text** does not say what it needs, so the query is planned once, under the
caller's own guard, and the tables it turns out to read are recorded with the definition. Those
tables are then what the cube is authorized against, what its cache is keyed on, and what makes
it stale. `cubes()` shows them in its `reads` column.

Two refusals follow from the same rule, and both land at declaration rather than later:

- **A query that cannot be planned is not a fact source.** No plan, no dependency list; a cube
  with no dependency list would be checked against nothing and invalidated by nothing.
- **A query whose answer can move on its own is refused**, naming what it found — `now()`,
  `random()`, `current_date`. A cuboid built from such a query is a cache of one arbitrary
  answer, and every later read serves it as though it were *the* answer.

**Every measure needs a rule for every dimension, and there is no default:**

```console
$ psql … -c "CREATE CUBE bad FROM \"probe_e.scratch\"
               DIMENSION region … DIMENSION period …
               MEASURE amount (SUM ALONG region);"
ERROR:  the cube `bad` was not created: measure 'amount' declares no aggregation rule along period.
        It is refused rather than summed: summing a balance across time, or a rate across anything,
        produces a figure that looks right and is not
```

Rule | Combines by | Composes further?
---|---|---
`SUM` | adding | yes
`MIN`, `MAX` | the extreme | yes
`FIRST`, `LAST` | position in the dimension's order | yes
`MEAN` | the arithmetic mean | **no** — an average of averages is not an average
`NONE` | it cannot be derived from parts at all | **no** — a ratio, a distinct count

There is deliberately no `CREATE OR REPLACE CUBE`; a duplicate name is refused, because replacing a
cube retires everything it materialised and that should not happen because somebody re-ran a script.
`DROP CUBE <name>` and `DROP CUBE IF EXISTS <name>` remove it, and the drop is the **only** moment at
which its cuboids can be reclaimed (Chapter 15, *Maintenance, tiering and the data lifecycle*, §15.4).

### Discovering one

```sql
SELECT cube, fact_table, reads, definition_version, dimensions, measures, hydrated_measures FROM cubes();
SELECT dimension, level, depth, column, joins_on, member_table, parent_child
  FROM cube_dimensions('regional');
SELECT measure, dimension, rule, composes FROM cube_measures('regional');
```

`depth` is a column rather than the row order, because the order is a fact about the model — sort the
result without it and you draw a list where there is a hierarchy. `composes` says whether a measure
can be rolled up **at all**; offering *"roll up by period"* on something that cannot is offering a
button that does not work.

### Navigating one

```sql
SELECT * FROM cube_rollup('<cube>', '<measure>' [, '<options>']);
SELECT * FROM cube_slice ('<cube>', '<measure>' [, '<options>']);
```

Options are a single string of `key=value` pairs, because only literals reach a table function and
`name => value` is rejected outright by the SQL planner: `'by=region, min_completeness=0.5,
materialise=false'`. Every key is checked against a known set, so a misspelled bound is refused rather
than silently taking its default.

Option | Meaning
---|---
`by=<dim>` | The dimension to keep. Omitted, the answer is the grand total
`where=<dim>:<member>` | Fix one member — a slice
`min_completeness=<f>` | Refuse an answer that saw less than this fraction
`materialise=true\|false\|pinned` | Narrow what this query may use. **Nothing widens it**

Every answer carries its provenance as columns:

Column | What it tells you
---|---
`definition_version` | Derived from the definition's content, never declared
`snapshot` | The table version this was computed at
`completeness` | What fraction of the input reached the cube
`withheld` | How many rows did not — policy, or an unplaceable member
`materialised` | Whether a stored cuboid answered
`from_cuboid` | Which one, when it did

Worked, with a control (Chapter 18 §18.9 has the full transcript): a `SUM` cube over a 1,000-row table
whose region is null on 333 rows reports `completeness 0.667`, `withheld 333`, and a grand total of
`499500` — exactly `SELECT sum(amount) … WHERE region IS NOT NULL`.

`materialise=false` computes from the base data and is the **reproducibility check**: a figure that
differs between it and the default is a defect, not a tuning question, because materialisation is a
cache and a cache that changes the answer is not one.

### A defect you must know about before you declare a non-`SUM` measure

Verified against the running server. A measure declared `MEAN ALONG region` or `MAX ALONG region`
returns the **sum**:

```console
$ psql … -c "CREATE CUBE m FROM \"probe_e.scratch\"
               DIMENSION region … MEASURE amount (MAX ALONG region);"
$ psql … -c "SELECT region, amount FROM cube_rollup('m','amount','by=region');"
 region | amount
--------+--------
 north  |  15687
 south  |  15438

$ psql … -c "SELECT region, max(amount) FROM probe_e.scratch WHERE region IS NOT NULL GROUP BY region;"
 region | max(probe_e.scratch.amount)
--------+-----------------------------
 north  |                       373.5
 south  |                       370.5
```

The same holds for `MEAN`: the cube reports `15687` where the mean is `186.75`. The composability half
of the rule is enforced correctly — rolling a `MEAN` or `NONE` measure *away* is refused with the
right sentence — but the cell is read with a hardcoded summation
(`crates/sankhya-cube-sql/src/functions.rs`), so the declared rule never reaches the value. Until it
does, **only `SUM` measures return the number they claim.** That is the exact failure the cube model
exists to prevent, arriving one layer below where the model checks for it.

## 19.5a An aggregation of your own

A cube's measures compose along each dimension by a **declared rule**, and that model exists
because the alternative produces plausible wrong figures. What it cannot express is the rule that
is *this* firm's — a weighted average with their weighting, an exposure netted their way.

```sql
CREATE AGGREGATION weighted_mean LANGUAGE PYTHON AS $$
def initial():
    return {'total': 0.0, 'weight': 0.0}

def accumulate(state, values):
    # `values` arrives interleaved, one tuple per row, as a memoryview of doubles.
    for i in range(0, len(values) - 1, 2):
        state['total'] += values[i] * values[i + 1]
        state['weight'] += values[i + 1]
    return state

def merge(a, b):
    return {'total': a['total'] + b['total'], 'weight': a['weight'] + b['weight']}

def finish(state):
    return state['total'] / state['weight'] if state['weight'] else 0.0
$$;

SELECT region, weighted_mean(margin_pct, amount) FROM sales.orders GROUP BY region;
```

`SHOW AGGREGATIONS` lists them with their source; `DROP AGGREGATION [IF EXISTS] <name>` removes
one. The four methods are [ADR-0010](../../adr/0010-external-aggregations.md)'s contract:
`initial` and `merge` are optional, `accumulate` and `finish` are not, and **a declared `merge`
is the claim that partial results compose** — it is what lets the aggregate be split across
partitions and, later, rolled up from a materialised cuboid.

Three things happen before the server believes you, and each is a refusal you will meet if it
does not hold:

- **It is exercised.** The same values are accumulated in one batch and in several, and the
  parts are merged in two different groupings, and the answers are compared **bit for bit**. A
  function whose answer depends on how the rows happened to be batched is refused at declaration
  with both numbers, rather than found later as two reports differing by a penny.
- **Its state must be storable.** The state is JSON. One that is not — a `set`, an object — is
  refused by name, because `ADR-0010` says a measure whose state is not serialisable is usable
  and *not materialisable*, and that has to be said at declaration rather than discovered when a
  cuboid fails to write.
- **It runs behind an operating-system boundary.** No network, no filesystem, no subprocess,
  bounded in time and memory ([ADR-0023](../../adr/0023-the-sandbox-a-user-function-runs-in.md)).
  On a machine where that boundary cannot be built — a kernel with unprivileged user namespaces
  disabled — `CREATE AGGREGATION` is refused naming the mechanism. It does not fall back.

> **It costs a process boundary per batch**, which is two to three orders of magnitude against a
> compiled built-in ([ADR-0022](../../adr/0022-user-defined-functions.md) Decision 5). That is the
> reason the built-in catalogue is worth its size, and it is still enormously faster than
> fetching a million rows to a client to do the same arithmetic — which is the comparison that
> decides whether the feature earns its place.

## 19.6 Clones and lineage

```sql
CREATE TABLE <name> CLONE <schema>.<table> [ AT VERSION <n> ];
DROP TABLE [ IF EXISTS ] <schema>.<table>;
SHOW LINEAGE OF <table>;
SHOW DEPENDENTS OF <table>;
```

A clone lands in its **origin's** schema and naming another one is refused, because a clone is
authorized through its origin: one placed elsewhere would have its name governed by one policy and its
data by another. `AT VERSION` pins the origin version the clone reads; omitted, it takes the origin as
it stands when the clone is made.

`SHOW LINEAGE OF` returns `step, origin, origin_version, cloned_at`, nearest first — the first row
answers *"what was this cloned from?"* and the last answers *"what is it ultimately a snapshot of?"*,
which one step cannot. An empty result means *not a clone*, which is an answer and a different one
from *"I could not tell you"*.

`SHOW DEPENDENTS OF` returns `dependent, relation, reads_version`, where `relation` is `direct` or
`indirect`. Ask it **before** a drop: the refusal names what would break, which is no use to somebody
who had no way to ask first.

Chapter 18 §18.8 and §18.10 have the transcripts, including the drop refusal and the leaf-first order
that satisfies it.

## 19.7 Feeds and quarantine

Declaring a feed is a file under `config/feeds/`, not a statement. What a *client* can do is see them
and resume them:

```sql
SHOW FEEDS;
RESUME FEED <name>;

SELECT feed, source, position, arrived_at, reason_code, reason, declaration, payload
  FROM sank.sank_quarantine
  WHERE feed = 'orders';
```

`SHOW FEEDS` returns one row per declared feed: `feed, state, halted_since, reason, runs, published,
quarantined, skipped, halts`. A feed that has never managed to run is listed too — the case an operator
most needs to see, and the one a *"list of running feeds"* would omit. **The halt count survives a
resume**, because a feed that halted, was resumed and halted again for the same reason is not in the
situation a feed that halted once is in.

Resuming is a statement rather than a restart, and it takes effect on the next tick: restarting the
server to resume one feed takes an outage on every other feed and every open connection.

`sank_quarantine` is a **table**, not a directory of rejected files, and it holds the record *whole,
exactly as it arrived*, because a record reduced to an error message cannot be replayed and replay is
the only actual remedy. Chapter 9, *Capture and ingest*, covers what gets quarantined; Chapter 15
§15.5 covers how it expires.

On a server with no feeds declared, `SHOW FEEDS` returns no rows and `sank_quarantine` is empty —
which is the correct answer rather than an error.

## 19.7a Snapshots

A **snapshot** names one instant across many tables. A clone freezes a *thing*; a snapshot
freezes a *moment*.

```sql
CREATE SNAPSHOT eod_2026_09_02 EXPIRE AFTER 90 DAYS;
SHOW SNAPSHOTS;

SET SNAPSHOT = 'eod_2026_09_02';
SELECT region, sum(amount) FROM sales.orders GROUP BY region;   -- as of that instant
RESET SNAPSHOT;

DROP SNAPSHOT eod_2026_09_02;
```

`SHOW SNAPSHOTS` reports the name, whether it is `live` or `expired`, who took it, when, the day
it expires, how many tables it pins and their qualified names.

> **Key idea** — A calculation that reads a population of records, a set of rates, a set of
> curves and the hierarchy they roll up through must read all four **as of one instant**.
> Otherwise the reconciliation problem this system exists to remove reappears *inside a single
> query*: four tables, four moments, one number that reconciles to nothing.

Nothing is copied. A snapshot records a version per table and pins those files against
reclamation, which is the same machinery a clone uses.

**The expiry is required and there is no `EXPIRE NEVER`.** A snapshot pins files; one that never
expired would hold a whole warehouse's versions alive, and `RSK-35` — the accumulation nobody is
responsible for — would arrive at warehouse scale rather than table scale. The upper bound is
730 days, and the refusal says the limit is not technical: it bounds how far ahead one person
may commit storage somebody else will pay for.

> **Pitfall** — A table created *after* a snapshot is **not there** when you read as of it, and
> naming it fails to resolve exactly as a table that does not exist does. It is deliberately not
> answered as empty: a table that did not exist is not a table that was empty, and a join
> against one returns the rows surviving an inner join with nothing — a confident zero, reported
> as success. This means a query that worked in March may refuse in June because the schema
> grew. That is the feature; the alternative is a query whose meaning quietly changes.

`SET SNAPSHOT` is a **session** setting, because a run reads one instant across many statements.
Another connection is unaffected. Naming a snapshot that does not exist, or one that has
expired, is refused at the `SET` rather than at the next query — failing where a person can act
beats failing where the consequence happens to be noticed.

See [ADR-0019](../../adr/0019-named-snapshots.md) for the reasoning, and Chapter 12 for how this
differs from cloning.

## 19.7b History, and one table at one version

A snapshot is a **tag**. `SHOW HISTORY OF` is the log underneath it.

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
rather than zero for a commit that touched no file: a commit that only declared a schema has
nothing to take a time from, and a date in 1970 presented as a fact is worse than a blank.

`what` is one of `created`, `appended`, `removed`, `compacted`, `rewritten` and `metadata` —
derived from the commit rather than stored in it, because the log records facts and this is a
*reading* of them, and a reading that lived in the log would be one more thing a writer could
get wrong.

Two columns carry most of the meaning.

**`changed_data`** is the writer's own declaration — `dataChange` on every add and remove — not
an inference from the file counts. A compaction rewrites files and changes not one row, so it
reports `f`. This was wrong when the column was first written: the *removals* declared
`dataChange: false` and the *addition* declared `true`, so a compaction was a data change in one
direction and not the other. Invisible until something printed it. A column that reports
maintenance as a change is worse than no column, because it trains a reader to ignore it.

**`kept_by`** names the snapshots and clones holding that version alive --- **by name**, and
all of them, because somebody reading this column is deciding what to drop to release the
storage and a column that said only `"snapshot"` would send them to `SHOW SNAPSHOTS` to work
out which one. It is **empty** for a version nothing is keeping. That emptiness is the important half:

> **The rule.** *History is readable only where something is keeping it alive.* Retirement
> deletes the files a merge replaced once nothing references them. The commit stays in the log
> forever; its data does not. A version with an empty `kept_by` may still be readable — nothing
> has swept it *yet* — so only a non-empty `kept_by` is a guarantee.

### Reading one table at a version

```sql
SET VERSION OF sales.orders = 2;
SELECT count(*) FROM sales.orders;
RESET VERSION OF sales.orders;
```

Per table and per session, and independent of `SET SNAPSHOT`. This answers *"what did **this**
table look like then"*; a snapshot answers *"what did **everything** look like then"*. Reach for
this to check one table against yesterday, and for a snapshot when more than one table has to
agree.

Three refusals, each of which was a wrong answer before it was a refusal:

| What you asked | Code | Why it is refused |
|---|---|---|
| A version the table does not have | `42704` | Replaying a log stops at its end, so version 9999 of a five-version table resolved to version 5 — a version nobody has, served as though they had it. The refusal names the newest it does have. |
| A version whose files retirement has taken | `42704` | The commit is in the log and its data is not. Answering it would return whichever rows happened to survive: a historical query silently missing whatever was compacted. |
| A table that does not exist | `42P01` | At the `SET`, not at the next query. |

The middle one is the whole reason the feature is shaped this way. It is the wrong answer that
looks most like a right one, because it has rows in it — and nothing about it is distinguishable
from a correct historical read except the number at the bottom.

> **What this is not.** Not version control. There is no diff between two versions and no way to
> restore one, and neither is an oversight. The log records **files**, not rows: a compaction
> replaces every file and changes nothing, so a file-level diff would report a maintenance job as
> a total rewrite — a diff that is worse than no diff, because it looks like an answer. A
> row-level difference needs a decision before it needs code, and that decision is `M20`'s.
>
> What you have is closer to a **tag** than a branch: name a moment, read it back, and know that
> the naming is what keeps it readable.

## 19.8 Vectors

A column can hold a vector per row — an embedding, a factor vector, a window of readings — stored as
`FixedSizeList<Float64, N>`. Constructors let one be built and operated on without ever being stored.

Function | Arity | Verified
---|---|---
`vec_of(a, b, …)` | n ≥ 1 | `vec_of(1.0,2.0,3.0)` → `[1.0, 2.0, 3.0]`
`vec_dot(a, b)` | 2 | `vec_dot([1,2,3],[4,5,6])` → `32`
`vec_euclidean(a, b)` | 2 | `vec_euclidean([0,0],[3,4])` → `5`
`vec_cosine_similarity(a, b)` | 2 | orthogonal → `0`
`vec_cosine_distance(a, b)` | 2 | orthogonal → `1`
`vec_norm_l1(a)` | 1 | `[1,2,3,4,10]` → `20`
`vec_norm_l2(a)` | 1 | `[3,4]` → `5`
`vec_sum(a)` | 1 | `[1,2,3,4,10]` → `20`
`vec_mean(a)` | 1 | → `4`

A similarity search is an ordinary `ORDER BY`:

```sql
SELECT title, vec_cosine_similarity(embedding, vec_of(0.1, 0.4, 0.9)) AS score
FROM documents
ORDER BY score DESC
LIMIT 10;
```

Two costs, stated as costs. **An array column cannot be pruned** — a minimum and maximum of a vector
prune nothing — so a table of embeddings prunes on `sank_data_date` and its scalar columns only. And
**an array cannot be a key column**: array equality as row identity is refused rather than supported
badly.

## 19.9 Statistics and calculus, within one row

These describe the series **inside one row** — a window of readings, a term structure, a factor path.
That is distinct from SQL's `stddev(x)`, which describes a *column* across rows. Verified against
`vec_of(1.0, 2.0, 3.0, 4.0, 10.0)`:

Function | Arity | Result
---|---|---
`vec_variance(a)` | 1 | `12.5` (sample)
`vec_stddev(a)` | 1 | `3.5355339059327378`
`vec_median(a)` | 1 | `3`
`vec_skewness(a)` | 1 | `1.697056274847714`
`vec_kurtosis(a)` | 1 | `3.1519999999999992` — **excess**, so a normal distribution reads zero
`vec_covariance(a, b)` | 2 | `12.5` against itself
`vec_correlation(a, b)` | 2 | `0.9999999999999999` against itself
`vec_integral(a)` | 1 | `14.5` — trapezoidal, unit spacing

Two behaviours worth knowing. **Variance is computed in two passes**: the textbook one-pass identity
`E[x²] − E[x]²` is algebraically correct and numerically disastrous, since for values with a large mean
and small spread it subtracts two nearly equal large numbers and cancellation can produce a *negative
variance*, which every downstream square root turns into a NaN. And **a correlation against a constant
series is refused**, not reported as zero:

```console
$ psql … -c "SELECT vec_correlation(vec_of(1.0,1.0,1.0), vec_of(1.0,2.0,3.0));"
ERROR:  [SNK-C0007] Execution error: vec_correlation: cosine similarity is undefined against a vector
        of zero magnitude: the angle to the origin is not zero and not one, and returning either places
        the vector at a definite similarity to everything
```

Zero would say *unrelated*; the truth is *undefined*, and a ranked correlation table would show a
constant column as genuinely uncorrelated rather than as unanswerable.

## 19.10 Matrices

A matrix is stored flat and its shape comes from **field metadata**, using Arrow's canonical
`arrow.fixed_shape_tensor` extension. A column with no declared shape is refused rather than assumed
square, because that guess is wrong for every rectangular matrix and produces numbers from values that
were never in the same row.

Function | Arity | Verified
---|---|---
`mat_of(rows, cols, …)` | 2 + rows·cols | `mat_of(2,2,1.0,2.0,3.0,4.0)` → `[1.0, 2.0, 3.0, 4.0]`, row-major
`mat_identity(n)` | 1 | `mat_trace(mat_identity(3))` → `3`
`mat_transpose(m)` | 1 | → `[1.0, 3.0, 2.0, 4.0]`
`mat_multiply(a, b)` | 2 | composes: see below
`mat_determinant(m)` | 1 | `[[1,2],[3,4]]` → `-2`
`mat_trace(m)` | 1 | `mat_identity(3)` → `3`
`mat_inverse(m)` | 1 | `[[4,7],[2,6]]` → `[0.6000000000000001, -0.7000000000000001, -0.2, 0.4]`
`mat_solve(m, v)` | 2 | `[[2,0],[0,4]] x = [2,8]` → `[1.0, 2.0]`
`mat_vec(m, v)` | 2 | `[[1,2],[3,4]] · [1,1]` → `[3.0, 7.0]`

Because matrix-returning functions carry their own shape, these compose:

```console
$ psql … -c "SELECT mat_determinant(mat_multiply(mat_of(2,2,1.0,2.0,3.0,4.0),
                                                 mat_of(2,2,5.0,6.0,7.0,8.0)));"
 4.000000000000007
```

The exact answer is `4`; the result is `4.000000000000007`. **Deterministic is not the same as exact**,
and this is the distinction to hold on to: the promise is that two runs of the same expression on
different machines with different core counts return the *same* bits, not that those bits are the
infinitely precise answer.

A matrix's shape is part of its **type**, so `mat_of`'s dimensions must be literals and a wrong element
count is refused **when the query is planned**:

```console
$ psql … -c "SELECT mat_of(2, 3, 1.0, 2.0, 3.0, 4.0, 5.0);"
ERROR:  [SNK-C0001] Error during planning: mat_of(2, 3, …) needs 6 values and was given 5. Refusing at
        planning time rather than partway through the scan
```

> **Key idea** — Every reducing kernel here is **bit-deterministic**. A dot product is a floating-point
> sum, and a sum whose order depends on how the query was partitioned returns a different number when
> the machine is busier or has more cores — a difference *too small to notice and too large to
> reconcile*. These go through the same compensated, order-fixed summation the rest of the system uses,
> which is also why this does not delegate to a numeric library: reordering freely for speed is what a
> good one does, and it is exactly what cannot be permitted here.

**QR, SVD and eigendecomposition are deliberately absent.** They are where an in-house implementation
is genuinely worse than none: a subtly wrong SVD produces plausible singular values.

## 19.11 The graph

Five table functions, joinable against ordinary tables:

```sql
SELECT p.label, r.depth
FROM graph_reachable('payments', 'acct-1', 'max_depth=3') AS r
JOIN parties AS p ON p.key = r.vertex
WHERE NOT r.truncated;
```

`graph_reachable` · `graph_time_respecting` · `graph_shortest_path` · `graph_cycles` ·
`graph_influence`

**Time-respecting traversal is a separate function, not a flag.** Static reachability over a temporal
graph *over-reports* — it finds routes that time forbids — and always in that direction. A flag
defaulting to off would hand the optimistic answer to everyone who forgot it, and the optimistic answer
looks exactly like the correct one.

```sql
SELECT * FROM graph_time_respecting('payments', 'acct-1',
    'max_depth=6, min_conservation=0.9, max_dwell=86400000000');
```

Every row carries `epoch`, `snapshot`, `truncated` and `truncation_reason` as **columns**, because a
flag beside the result gets dropped by the first projection that does not mention it, and a short list
looks exactly like a short answer.

**Nothing hydrates an epoch on a timer.** On a server with no epoch built, every one of these refuses
by name rather than returning nothing:

```console
$ psql … -c "SELECT * FROM graph_reachable('payments','acct-1','max_depth=3');"
ERROR:  [SNK-C0001] Error during planning: no graph named 'payments' is registered; known graphs are
        []. Refusing rather than returning no rows: an empty traversal over a graph that does not exist
        reads exactly like one that found nothing
```

That refusal is the right behaviour and it is also, today, the only behaviour a fresh server exhibits.
Chapter 11, *The graph engine*, covers the algorithms; the population path is `M4`'s carried remainder.

## 19.12 What is not on this surface

- **MDX**, deliberately. The cube surface is SQL table functions.
- **`DoPut`, prepared statements, transactions and `DoExchange` on Flight SQL.** Each returns
  `UNIMPLEMENTED` with a reason, and `DoPut` names which write path to use instead — a third write path
  with its own semantics would be a way for the other two to disagree.
- **A REST/JSON surface.** §19.1.
- **`current_user`, `pg_database`, `pg_roles`.** §19.2.
- **Anything that writes rows.** §19.3.

And one caution that belongs at the end of a surface catalogue rather than in a footnote. A table that
was present when the server started and is dropped afterwards **remains listed and remains queryable**
for the life of that process, while `SHOW LINEAGE OF` it correctly refuses. If you are scripting
against `information_schema.tables`, do not treat its presence as proof the table is there. Chapter 14,
*Observability*, §14.7 has the transcript.
