# 18. Getting started

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> This chapter takes you from a running SANKHYA to a query, a clone, a cube and a refusal, in that
> order, using nothing but `psql` and — at the end — five lines of Python. Its central claim is that
> the whole product is reachable from a SQL prompt: there is no client library you must install to
> do anything, because a binding contains no logic the server does not enforce. Every transcript
> below was produced by running the statement shown against a live server while this chapter was
> written. Where a step was **not** run here, the text says so and says why.

---

## 18.1 What was run, and what was not

Two honesty notes before anything else, because this chapter is the one a reader is least equipped
to check.

**Everything from §18.3 onwards is real output.** Each block was produced by executing the statement
above it against a SANKHYA 0.1.0 serving a warehouse of twelve tables. Nothing was retyped from a
design document, and where an example produced something surprising the surprise is in the text.

**§18.2 was not re-executed here.** Building the workspace takes minutes and tens of gigabytes, and
the machine this was written on was serving the very server the rest of the chapter queries. Those
steps are the repository's own quickstart, which is itself executed by a test on every build —
`crates/sankhya-server/tests/five_minutes.rs` generates a warehouse, starts the real binary, connects
over the real wire protocol, queries, runs the diagnostic, takes a backup and proves it. It is a test
so it cannot rot, and it is the reason those steps can be quoted with a straight face.

## 18.2 Getting a server

You need Rust 1.97.1 --- pinned by `rust-toolchain.toml`, so `rustup` will fetch it --- a C toolchain (`gcc`, `make`, `bison`, `flex`, `perl`, `pkg-config`),
the `readline`, `zlib`, `openssl` and `icu` development headers, about 25 GB of disk if you intend to
run the full acceptance dataset, and 8 GB of memory.

```bash
git clone https://github.com/ajsinha/sankhya.git
cd sankhya
cargo build --release -p sankhya-server
```

Then a warehouse to point it at. One exists for exactly this purpose:

```bash
SANKHYA_WAREHOUSE=./warehouse \
  cargo test -p sankhya-server --test make_warehouse -- --ignored
```

That writes one table of a thousand rows across several Parquet files, so the read path has something
to prune and to parallelise over. Start the server on it:

```bash
SANKHYA_NO_PASSWORD=1 \
SANKHYA_WAREHOUSE=./warehouse \
SANKHYA_LISTEN=127.0.0.1:5433 \
  ./target/release/sankhya-server
```

`SANKHYA_WAREHOUSE` is a directory of `<schema>/<table>/`, each table holding Parquet files under
`sank_data_date=YYYY-MM-DD/` and a `_delta_log`. The server walks it at startup and reads each
table's schema **out of its own log** — not from a Parquet footer, because a table with no files yet
has no footer, and one whose files predate a column would produce a schema missing it.

The startup line is worth reading rather than scrolling past. It names the tenant, the authentication
posture in capitals when there is none, the transport posture in words, the audit chain head, both
doors and the maintenance settings. Chapter 17, *Packaging and deployment*, §17.5 explains why each
line is there.

> **Fixed** — This chapter previously warned that the shipped `config/application.yaml` carried a
> `warehouse.read_as_of` the loader refused as out of range, and told you to write your own file
> instead. It did: `18446744073709551615` is `u64::MAX`, and a configuration file is read through a
> signed integer. Because the configuration was loaded *before* the subcommand was dispatched, the
> refusal reached `doctor`, `backup`, `drill`, `attest` and `--version` as well --- every entry point
> the binary has. The setting is now left unset, which means everything published, and
> `crates/sankhya-server/tests/configured.rs` runs `doctor` against the file this repository actually
> ships so it cannot drift again.

The rest of this chapter is written against a server on port `55432` whose warehouse holds a schema
called `common`. Substitute your own port and names.

## 18.3 Connect

SANKHYA speaks the PostgreSQL wire protocol, so anything that talks to PostgreSQL talks to it. No
driver, no shim.

```console
$ psql -h 127.0.0.1 -p 55432 -U you -d sankhya -c "SELECT version();"
                                  version
----------------------------------------------------------------------------
 PostgreSQL 17.0 (SANKHYA 0.1.0) on wire-protocol-compatible unified engine
(1 row)
```

The string begins `PostgreSQL 17.0` because **every client parses the major version out of it before
it will proceed**, and then says what this actually is so the prefix does not mislead anyone reading
it.

## 18.4 Find out what is there

```console
$ psql … -c "SELECT table_schema, table_name FROM information_schema.tables WHERE table_schema = 'common';"
 table_schema | table_name | table_type
--------------+------------+------------
 common       | empty      | BASE TABLE
 common       | orders     | BASE TABLE
 common       | regions    | BASE TABLE
(3 rows)
```

That listing is filtered by policy **server-side**. A catalogue that returned everything and left the
client to filter would disclose the existence of tables the caller may not read, which is the leak
Chapter 13, *Security, tenancy and policy*, refuses everywhere else — arriving through a schema
browser.

Columns:

```console
$ psql … -c "SELECT column_name, data_type, is_nullable FROM information_schema.columns
             WHERE table_schema='common' AND table_name='orders';"
 table_schema | table_name |  column_name   | ordinal_position | data_type | is_nullable
--------------+------------+----------------+------------------+-----------+-------------
 common       | orders     | id             |                1 | int8      | NO
 common       | orders     | region         |                2 | text      | YES
 common       | orders     | period         |                3 | text      | YES
 common       | orders     | amount         |                4 | float8    | NO
 common       | orders     | note           |                5 | text      | YES
 common       | orders     | sank_data_date |                6 | date      | NO
(6 rows)
```

Two things to notice. `sank_data_date` is on **every** table, of type `DATE`, and is what the table is
partitioned on — Chapter 7, *The date axis*, argues why one guaranteed column is what makes
partitioning, retention and tiering writable once rather than per table. And the catalogue answered
with **its own projection** rather than the three columns asked for. That is a wart, and it matters
to anyone writing a client: read the result by column name, never by position. Chapter 19,
*The SQL surface*, §19.2 records it.

`\dt` and `\dn` work. `\d <table>` does **not**, in this build — `psql` issues a `pg_class` query
whose answer it cannot use and fails with *"column number 3 is out of range 0..2"*. Use
`information_schema.columns` instead.

## 18.5 Your first query

```console
$ psql … -c "SELECT id, region, period, amount FROM common.orders ORDER BY id LIMIT 5;"
 id | region | period | amount
----+--------+--------+--------
  0 | north  | q1     |      0
  1 | south  | q2     |    1.5
  2 |        | q1     |      3
  3 | north  | q2     |    4.5
  4 | south  | q1     |      6
(5 rows)
```

Row 2's region is genuinely **null**, not an empty string. That distinction survives from the Parquet
page, through the Arrow array, to the wire — where it becomes a length of −1 rather than a length of
0. Conflating them is a wrong answer, not a formatting choice, and it is the first thing to check in
any system that claims to be columnar end to end.

## 18.6 Two names for a table, and the day one stops working

```sql
SELECT count(*) FROM common.orders;   -- always resolves
SELECT count(*) FROM orders;          -- while only one schema holds an `orders`
```

The bare name resolves *while* only one schema holds a table of that name, and stops resolving the
day a second one does. On this warehouse a second one already does:

```console
$ psql … -c "SELECT count(*) FROM orders;"
ERROR:  [SNK-C0001] Error during planning: table 'datafusion.public.orders' not found
DETAIL:  `orders` names more than one table in this warehouse: common.orders, probe_a.orders. It is
         registered under neither, because answering with one of them would hand back a table you had
         no way to identify. Qualify it with its schema.
```

> **Key idea** — That is the only behaviour that cannot be wrong. A name meaning two things has no
> right answer, and picking one hands back a table the caller had no way to identify. Anything
> written down — a script, a dashboard, a saved query — should qualify.

## 18.7 Aggregate, and read the null row

```console
$ psql … -c "SELECT region, count(*) AS n, round(sum(amount)) AS total
             FROM common.orders GROUP BY region ORDER BY region;"
 region |  n  | total
--------+-----+--------
 north  | 334 | 250250
 south  | 333 | 249251
        | 333 | 249750
(3 rows)
```

Hold on to that third row. A third of this table has no region, and §18.9 is about a system that
tells you so without being asked.

## 18.8 A clone, and what it costs

A clone is a **reference** to its origin's files at a version, not a copy of them. It costs the same
whether the origin holds a thousand rows or a billion, and it adds no files of its own.

```console
$ psql … -c "CREATE TABLE q3_frozen CLONE probe_e.scratch;"
$ psql … -c "SELECT (SELECT count(*) FROM probe_e.scratch) AS origin,
                    (SELECT count(*) FROM probe_e.q3_frozen) AS clone;"
 origin | clone
--------+-------
    250 |   250
(1 row)
```

Note the name in the statement is unqualified and the table landed in `probe_e`. **A clone stays in
its origin's schema**, and naming another one is refused:

```console
$ psql … -c "CREATE TABLE probe_a.q3 CLONE probe_e.scratch;"
ERROR:  a clone stays in its origin's schema, and `probe_e` is not `probe_a`. A clone is a reference
        to its origin's files and is authorized through them, so one placed under another schema would
        have its name governed by one policy and its data by another
```

Ask where it came from, and ask what still reads a table **before** you try to remove it:

```console
$ psql … -c "SHOW LINEAGE OF probe_e.q3_frozen;"
 step |     origin      | origin_version |    cloned_at
------+-----------------+----------------+------------------
    1 | probe_e.scratch |              1 | 1788315306943759
(1 row)

$ psql … -c "SHOW DEPENDENTS OF probe_e.scratch;"
     dependent     | relation | reads_version
-------------------+----------+---------------
 probe_e.q3_frozen | direct   |             1
(1 row)
```

`SHOW DEPENDENTS` exists because a refusal that names what would break is no use to somebody who had
no way to ask first. Chapter 12, *Cloning and lineage*, has the rest.

## 18.9 A cube, and a total that says how much it saw

A `GROUP BY` knows the column names you typed. A **cube** knows a model: which columns are dimensions,
which are measures, and — the part that decides whether an answer is correct — how each measure may
be combined along each dimension.

```console
$ psql … -c "CREATE CUBE regional FROM \"common.orders\"
               DIMENSION region FROM \"common.orders\" ON region (LEVEL area = region)
               DIMENSION period FROM \"common.orders\" ON period (LEVEL quarter = period)
               MEASURE amount (SUM ALONG region, SUM ALONG period);"
```

The quoting is not decoration and is not documented anywhere else: **`CREATE CUBE` does not parse a
schema-qualified name unless it is quoted.** Written bare, `FROM common.orders` fails at the dot.
Chapter 19 §19.5 records it.

It is discoverable, so a client can offer a picker instead of hardcoding a model that will drift:

```console
$ psql … -c "SELECT cube, fact_table, dimensions, measures FROM cubes();"
   cube   |  fact_table   | dimensions | measures
----------+---------------+------------+----------
 regional | common.orders |          2 |        1

$ psql … -c "SELECT measure, dimension, rule, composes FROM cube_measures('regional');"
 measure | dimension | rule | composes
---------+-----------+------+----------
 amount  | region    | sum  | t
 amount  | period    | sum  | t
```

Roll a dimension **away**:

```console
$ psql … -c "SELECT region, amount, completeness, withheld, materialised
             FROM cube_rollup('regional','amount','by=region');"
 region |  amount  | completeness | withheld | materialised
--------+----------+--------------+----------+--------------
 north  | 250249.5 |        0.667 |      333 | f
 south  | 249250.5 |        0.667 |      333 | f
(2 rows)
```

**Read `completeness` and `withheld`.** They are the 333 rows from §18.7 whose region is null: 667 of
1,000 rows reached the cube, and the answer says so on every row rather than in metadata a projection
would drop. Check it against plain SQL:

```console
$ psql … -c "SELECT amount, completeness, withheld FROM cube_rollup('regional','amount');"
 amount | completeness | withheld
--------+--------------+----------
 499500 |        0.667 |      333

$ psql … -c "SELECT sum(amount) FROM common.orders WHERE region IS NOT NULL;"
 sum(common.orders.amount)
---------------------------
                    499500
```

Exactly equal. That is the property to take away: a cube total is a total *over the rows it could
place and the caller may read*, and the fraction is published rather than inferred.

An option it does not recognise is refused while the query is planned, rather than quietly taking its
default:

```console
$ psql … -c "SELECT * FROM cube_rollup('regional','amount','by=region, materialise=maybe');"
ERROR:  [SNK-C0001] Error during planning: the 'materialise' option must be true, false or pinned,
        and 'maybe' is not. It narrows what this query will use: 'false' computes from the base data,
        'pinned' uses only cuboids the definition pins. There is no value that widens it --- a session
        that could spend more of an operator's storage would be a storage grant to anybody who can
        open one
```

## 18.10 When it refuses

Refusals are a feature here, and one that does not say what to do is treated as a defect. Three worth
meeting on your first day.

**A write, against a read path:**

```console
$ psql … -c "INSERT INTO common.orders (id) VALUES (1);"
ERROR:  [SNK-C0006] the statement uses a feature this build does not implement: data modification is
        not served over this connection; this server is a read path over a published warehouse
DETAIL:  Write to the transactional store and let capture publish it, or publish an external table
         with `sankhya-publish`. See GUIDE.md §3.
```

The refusal names the supported route, because a refusal that only says no sends somebody looking for
a flag to turn it on, and there is no flag. Note also what it did *not* do: it did not accept the
statement and discard it. This once returned a success tag and did nothing durable, which is worse
than failing — Chapter 14, *Observability*, §14.5.

**A typo:**

```console
$ psql … -c "SELECT * FROM common.ordres;"
ERROR:  [SNK-C0001] Error during planning: table 'datafusion.common.ordres' not found
DETAIL:  Correct the statement. The detail names the offending element.
```

`SNK-C0001` is permanent. It is what a support conversation is conducted in and what a runbook is
indexed by, and `DETAIL` is the error catalogue's own remediation — so the client and
[`ERRORS.md`](../../ERRORS.md) cannot say different things. A table you may not read produces
**exactly this error**, deliberately: saying *"you may not read that"* confirms it exists.

**A drop that would strand a clone:**

```console
$ psql … -c "CREATE TABLE q3_audit CLONE probe_e.q3_frozen;"
$ psql … -c "DROP TABLE probe_e.q3_frozen;"
ERROR:  `probe_e.q3_frozen` is still read by probe_e.q3_audit. Removing it is the deletion cloning is
        gated on, arriving through the front door --- materialise them first, or drop them
```

Drop the leaf first and the origin goes:

```console
$ psql … -c "DROP TABLE probe_e.q3_audit;"
$ psql … -c "DROP TABLE probe_e.q3_frozen;"
```

## 18.11 Watch what it is doing

Metrics live on their own port, one route, loopback by default:

```console
$ curl -s http://127.0.0.1:55433/metrics | grep -E '^sankhya_(queries|rows|audit|connections)'
sankhya_queries_total{outcome="error"} 14
sankhya_queries_total{outcome="ok"} 42
sankhya_rows_returned_total 42
sankhya_connections_active 0
sankhya_audit_records_total 56
```

Two things before you build a dashboard on it. **`refused` is not `error`** — a quota held is the
system working, and an error-rate alert that counts them together fires on correct behaviour. And
**watch `sankhya_metrics_rejected_total`**: non-zero means a call site disagrees with the catalogue, or
a label has outgrown its cap and that metric is now incomplete. Chapter 14 covers both.

Then the diagnostic, which reads the warehouse directly and deliberately does **not** start the
server:

```console
$ sankhya-server doctor
SANKHYA doctor 0.1.0
  warehouse .../warehouse
  12 table(s)

  [critical] backup — no restore drill has ever passed; this threshold has already been crossed.
         Run a restore drill: `sankhya-server drill`. If it fails, the backup is not a backup and this
         is an incident rather than a maintenance task. See docs/runbooks/restore-drill.md.

12 check(s) clean, 1 finding(s) of which 1 have a date, 0 check(s) could not run
$ echo $?
1
```

Do what it says:

```console
$ sankhya-server backup
  …
  backup:01a05fd9-53ee-7723-a129-85da1719e9bd
  queryable at 0
This backup is unproven until it has been drilled: `sankhya-server drill`.

$ sankhya-server drill
  common.orders: verified, 1000 row(s)
  …
Proven. 12 table(s) read back and digested.
```

That drill **read the data back and recomputed its digest**. A file-presence check would have passed
on a truncated Parquet, on a file whose bytes were replaced with another table's, and on essentially
every failure that actually happens. Chapter 16, *Backup, restore and disaster*, is the whole
argument.

## 18.12 The same thing, in Python

The binding is pure Python — no compiled extension, no wheel matrix. `pip install -e sdk/python`, and
then:

```python
import sankhya

with sankhya.open(host="127.0.0.1", port=55432, user="you", database="sankhya") as db:
    print(db.version())
    print([t.qualified for t in db.tables(schema="common")])
    for column in db.columns("common.orders"):
        print(" ", column.name, column.type_name, "null" if column.nullable else "not null")
    for row in db.rows("SELECT region, count(*) AS n FROM common.orders GROUP BY region ORDER BY region"):
        print(" ", row)
    try:
        db.sql("SELECT * FROM common.ordres")
    except sankhya.Refusal as refused:
        print("sqlstate:", refused.sqlstate)
        print("detail  :", refused.detail)
```

Run against the same server:

```
PostgreSQL 17.0 (SANKHYA 0.1.0) on wire-protocol-compatible unified engine
['common.empty', 'common.orders', 'common.regions']
  id int8 not null
  region text null
  …
  sank_data_date date not null
  {'region': 'north', 'n': '334'}
  {'region': 'south', 'n': '333'}
  {'region': None, 'n': '333'}
sqlstate: 42P01
detail  : Correct the statement. The detail names the offending element.
```

Values arrive as strings, and `None` is SQL `NULL` — the same distinction §18.5 made on the wire,
preserved into Python. Chapter 20, *The client contract and the SDKs*, explains what the binding may
and may not do, and lists by name what it cannot do yet — including TLS, which it does not use even
though the server offers it.

## 18.13 What you have not seen

Chapter 19, *The SQL surface*, is the complete statement and function catalogue: vectors, matrices,
statistics, graph traversal, feeds, and every clause of `CREATE CUBE`. Chapter 21, *Extensions and
packs*, covers writing something the engine did not ship with.

Three things you should not go looking for, because they are not there:

- **Ingest on a timer from a database.** Capture, apply and publication all work and no running
  process drives them, so everything this server serves is already published. Declared file feeds
  (`config/feeds/*.yaml`) *are* driven by the server.
- **A graph to traverse.** The five `graph_*` functions exist and refuse by name — *"no graph named
  'payments' is registered; known graphs are []"* — because nothing hydrates an epoch on a timer.
- **A second node.** Leader election, executor scale-out and failover are `M12` and need a second
  machine.

Chapter 26, *Roadmap and status*, is the authoritative list, and the repository's
[`STATUS.md`](../../STATUS.md) is the continuously updated one.
