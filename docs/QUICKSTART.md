<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Quickstart

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

Forty minutes, most of it compiling. At the end you will have a server running, a client
connected to it, a cube answering with its own completeness, a clone that costs nothing, a
backup you have *proved* restores, and a clear idea of which third of this product exists.

**Every transcript below was produced by running the command above it**, against a warehouse
this document tells you how to generate, on one machine with no container runtime, no message
broker, no object store and no cloud credentials. Where a step was not run, it says so.

> **One warehouse, one recipe.** Everything here runs against the fixture at
> `crates/sankhya-server/tests/make_warehouse.rs`, which is **the same warehouse every gate in
> this repository runs against**. That was not true until 2026-09-03: this document used to
> describe building one warehouse and then demonstrate against a different one, so the gates
> were green against a warehouse no reader could produce and a reader following along got
> `no cube named 'sales'` on the first statement. Two sources for one fact are two sources that
> will one day disagree. There is now one.

---

## What you need

| | |
|---|---|
| Rust | 1.97.1, pinned by `rust-toolchain.toml` (`rustup` recommended) |
| A PostgreSQL client | Any one. `psql` is used below; the server speaks the wire protocol, so anything that talks to PostgreSQL talks to it |
| C toolchain | `gcc`, `make`, `bison`, `flex`, `perl`, `pkg-config` — only if you build the vendored database in §2 |
| Disk | ~10 GB for the build; ~25 GB more only if you run the full acceptance dataset |
| Memory | 8 GB is enough |

On Debian or Ubuntu:

```bash
sudo apt install build-essential bison flex libreadline-dev zlib1g-dev \
                 libssl-dev libicu-dev pkg-config
```

---

## 1. Build

```bash
git clone https://github.com/ajsinha/sankhya.git
cd sankhya
cargo build --release -p sankhya-server -p sankhya-publish
```

The first build compiles a large dependency graph — expect several minutes. The enforcement
tooling builds in seconds and is worth running first:

```bash
cargo xtask check-all
```

That runs every repository invariant: the layer graph, the file-length ceiling, the
domain-vocabulary prohibition, the duplicate-dependency gate, the documentation checks, the
feature pins, clippy under the workspace's denied lints, and the concurrency measurements.
[`DEVELOPING.md`](DEVELOPING.md) explains each gate and [`TESTING.md`](TESTING.md) explains
what is and is not verified.

Two gates are **not** in `check-all`, deliberately, and are worth knowing about:

```bash
cargo xtask check-performance    # the NFR-PERF objectives, as a gate that can fail
python3 tools/mutation-audit.py  # 912 deliberate defects, applied one at a time
```

`check-performance` needs a TPC-H dataset and takes long enough that putting it in the default
gate would make people stop running the default gate — which also means **the performance
budgets do not run in CI**, since CI runs `check-all`. The mutation audit is the answer to
*"the tests pass, but do they test anything?"*: it applies 912 specific defects one at a time
and requires the suite to fail on each. It edits your source files as it goes, restoring each
one after, so run it on a clean tree.

---

## 2. Build the vendored database — optional

You need this only for a `psql` binary, and only if you do not already have one. **The server
does not use it**: nothing in this quickstart starts a database, because nothing in this
product currently does.

```bash
vendor/postgresql/build.sh          # builds PostgreSQL 17.11 from checksum-verified source
export PATH=$PWD/.build/pg-install/bin:$PATH
```

> **Not built: the transactional tier.** `crates/sankhya-oltp-pg/src/lib.rs` supervises
> PostgreSQL as a child process whose whole lifecycle SANKHYA owns, and it is tested against
> this vendored build. It is a **dev-dependency** of the server, `Settings` has no transactional
> configuration, and **the server never starts a database**. If you skip this section and use
> any `psql` you already have, nothing below changes.

---

## 3. Make a warehouse

```bash
SANKHYA_WAREHOUSE=./warehouse \
  cargo test -p sankhya-server --test make_warehouse -- --ignored
```

```
wrote the sample warehouse to ./warehouse --- sales.orders, sales.regions,
sank.risk, the quarantine, and the `sales` cube
```

It is `--ignored` because it writes to a path you name in the environment, and a test that
writes outside its own temporary directory is one that surprises somebody.

What you now have:

| Table | What it is |
|---|---|
| `sales.orders` | 1,000 rows across four Parquet files, so the read path has something to prune and to parallelise over. Columns `id`, `region`, `period`, `amount`, `margin_pct` |
| `sales.regions` | The dimension table a roll-up joins to |
| `risk.positions` | Twelve positions, each carrying a profit-and-loss **vector** of 64 outcomes and a stored 4×4 covariance **matrix** — so the function catalogue can be exercised on columns rather than on literals |
| `sank.sank_quarantine` | The feed quarantine, empty, so the example that reads it runs |
| the `sales` cube | Two dimensions and two measures, one of which deliberately cannot be rolled up |

Look at the layout before you start the server, because it is the product:

```console
$ find warehouse/sales/orders -type f | sort
warehouse/sales/orders/_delta_log/00000000000000000000.json
warehouse/sales/orders/_delta_log/00000000000000000001.json
warehouse/sales/orders/_delta_log/00000000000000000002.json
warehouse/sales/orders/_delta_log/00000000000000000003.json
warehouse/sales/orders/_delta_log/00000000000000000004.json
warehouse/sales/orders/sank_data_date=2026-09-06/part-0000-v0000001-2d10910.parquet
warehouse/sales/orders/sank_data_date=2026-09-06/part-0001-v0000002-2d10911.parquet
warehouse/sales/orders/sank_data_date=2026-09-06/part-0002-v0000003-2d10912.parquet
warehouse/sales/orders/sank_data_date=2026-09-06/part-0003-v0000004-2d10913.parquet
```

Open storage, in a layout that mirrors an operational schema: `<schema>/<table>/`, one
self-contained folder per table, a Delta transaction log, and Hive-style
`sank_data_date=YYYY-MM-DD/` directories. The date is the **business** date of a record, not
the moment it arrived — [`GLOSSARY.md`](GLOSSARY.md) explains why conflating those two is a
wrong answer that looks like a right one.

---

## 4. Start the server, and connect

```bash
SANKHYA_NO_PASSWORD=1 \
SANKHYA_WAREHOUSE=./warehouse \
SANKHYA_LISTEN=127.0.0.1:5433 \
  ./target/release/sankhya-server
```

```
SANKHYA 0.1.0
  tenant tenant:00000000-0000-0000-0000-000000000001, NO AUTHENTICATION — every connection
  is accepted, NO POLICY CONFIGURED — every authenticated user may read every one of the
  4 table(s) below, 4 table(s) known
  listening on 127.0.0.1:5433
  wire protocol unencrypted — passwords cross the network in plain text
  Arrow Flight SQL on 127.0.0.1:5434
  maintaining 4 table(s) every 30s, compacting every 1 tick(s), sweeping every 120
  audit chain head 0000000000000000000000000000000000000000000000000000000000000000 (0 record(s))
  1 cube(s): sales
  connect with: psql -h 127.0.0.1 -p 5433 -U <user>
  metrics on http://127.0.0.1:9464/metrics
```

**Read that banner.** It names its security posture in capitals when it has none, says the
transport is unencrypted in words, prints the audit chain head, and names both doors and the
metrics port. Every port above is a default you can override — `SANKHYA_LISTEN`,
`SANKHYA_FLIGHT_LISTEN`, `SANKHYA_METRICS_LISTEN` — and none of them is derived from another
by a rule. `sankhya-server --help` lists every `SANKHYA_*` variable; that is the complete list,
written out rather than generated, because the first thing a stranger types when a binary
refuses is `--help`.

`SANKHYA_NO_PASSWORD` is spelled as an opt-**out** so the insecure choice has to be made
deliberately.

> **A password is verified only if you have written one down.** Leaving `SANKHYA_NO_PASSWORD`
> unset makes this server *demand* a password; whether it *checks* one depends on
> `server.credentials`. With that list empty there is nothing to check against, so any password
> from any user connects, and the startup line says `PASSWORD UNVERIFIED` in capitals for
> exactly that reason. Write one with `sankhya-server hash-password` and paste the line under
> `server.credentials.<user>`. Naming one user makes the list the list: a user absent from it
> is refused.
>
> Until 2026-09-04 there was no credential store at all. That is `SEC-01`, the first finding of
> the security audit, and this paragraph said so before the repair rather than after it.

In another shell:

```console
$ psql -h 127.0.0.1 -p 5433 -U you -d sankhya -c "SELECT version();"
                                  version
----------------------------------------------------------------------------
 PostgreSQL 17.0 (SANKHYA 0.1.0) on wire-protocol-compatible unified engine
(1 row)
```

The string begins `PostgreSQL 17.0` because every client parses the major version out of it
before it will proceed, and then says what this actually is so the prefix does not mislead
anyone reading it.

```console
$ psql -h 127.0.0.1 -p 5433 -U you -d sankhya -c "\dt"
              List of relations
 table_schema |   table_name    | table_type
--------------+-----------------+------------
 risk         | positions       | BASE TABLE
 sales        | orders          | BASE TABLE
 sales        | regions         | BASE TABLE
 sank         | sank_quarantine | BASE TABLE
(4 rows)
```

That listing is filtered by policy **server-side**. A catalogue that returned everything and
left the client to filter would disclose the existence of tables the caller may not read —
which is the leak this system refuses everywhere else, arriving through a schema browser.

`\dt` and `\dn` work. `\d <table>` does **not** in this build: `psql` issues a `pg_class` query
whose answer it cannot use. Use `information_schema.columns` instead.

---

## 5. Query it

```console
$ psql … -c "SELECT id, region, period, amount FROM sales.orders ORDER BY id LIMIT 5;"
 id | region | period | amount
----+--------+--------+--------
  0 | north  | q1     |      0
  1 | south  | q2     |    1.5
  2 |        | q1     |      3
  3 | north  | q2     |    4.5
  4 | south  | q1     |      6
(5 rows)
```

Row 2's region is genuinely **null**, not an empty string. That distinction survives from the
Parquet page, through the Arrow array, to the wire — where it becomes a length of −1 rather
than a length of 0. Conflating them is a wrong answer, not a formatting choice, and it is the
first thing to check in anything claiming to be columnar end to end.

```console
$ psql … -c "SELECT region, count(*) AS n, round(sum(amount)) AS total
             FROM sales.orders GROUP BY region ORDER BY region;"
 region |  n  | total
--------+-----+--------
 north  | 334 | 250250
 south  | 333 | 249251
        | 333 | 249750
(3 rows)
```

Hold on to that third row: a third of this table has no region, and §6 is about a system that
tells you so without being asked.

**A bare name resolves while exactly one schema holds it.**

```console
$ psql … -c "SELECT count(*) FROM orders;"
 count(*)
----------
     1000
(1 row)
```

That works here because only `sales` has an `orders`. The day a second schema gains one, this
statement stops resolving — deliberately, because a name meaning two things has no right
answer, and picking one hands back a table the caller had no way to identify. **This fixture
cannot demonstrate the refusal**, which is a gap in the fixture rather than in the product; see
*Two things this fixture cannot show you* at the end. Anything written down — a script, a
dashboard, a saved query — should qualify.

Columnar throughout: Parquet on disk, Arrow in memory, and the read path plans from the table
log alone — no directory listing and no footer reads — pruning files by the statistics the log
records. **What the query path enforces:** a table the principal may not read is never
registered in the session, so naming it fails to resolve, indistinguishable from naming a table
that does not exist. That is deliberate — saying *"you may not read that"* confirms it exists.
A policy row predicate is conjoined where no provider can decline it, so a tautology in the
query cannot widen it. Both are tested end to end.

---

## 6. A cube, and the refusal that justifies it

A `GROUP BY` knows the column names you typed. A **cube** knows a model: which columns are
dimensions, which are measures, and — the part that decides whether an answer is correct — how
each measure may be combined along each dimension. The fixture declared one:

```console
$ psql … -c "SELECT cube, fact_table, dimensions, measures FROM cubes();"
 cube  | fact_table | dimensions | measures
-------+------------+------------+----------
 sales | orders     |          2 |        2
(1 row)
```

Roll a dimension **away**:

```console
$ psql … -c "SELECT region, amount, completeness, withheld, materialised
             FROM cube_rollup('sales','amount','by=region');"
 region |  amount  | completeness | withheld | materialised
--------+----------+--------------+----------+--------------
 north  | 250249.5 |        0.667 |      333 | f
 south  | 249250.5 |        0.667 |      333 | f
(2 rows)
```

**Read `completeness` and `withheld`.** They are the 333 rows from §5 whose region is null: 667
of 1,000 rows reached the cube, and the answer says so **on every row**, rather than in
metadata that a projection would drop. Check it against plain SQL:

```console
$ psql … -c "SELECT amount, completeness, withheld FROM cube_rollup('sales','amount');"
 amount | completeness | withheld
--------+--------------+----------
 499500 |        0.667 |      333
(1 row)
```

`499500` is exactly `sum(amount) WHERE region IS NOT NULL`. A cube total is a total *over the
rows it could place and the caller may read*, and the fraction is published rather than
inferred.

Now the measure that matters more:

```console
$ psql … -c "SELECT region, margin_pct FROM cube_rollup('sales','margin_pct','by=region');"
ERROR:  [SNK-C0001] Error during planning: measure 'margin_pct' cannot be rolled up along
        'period': its value at the coarser grain is not derivable from its values at the finer
        one, so any figure produced here would be plausible and wrong
DETAIL:  Correct the statement. The detail names the offending element.
```

**That refusal is the whole reason the cube model exists.** `margin_pct` is a ratio. There is
no operation over the parts that yields the whole, so it is declared as composing along
nothing, and a roll-up that would need it to is refused **while the query is planned** — not
answered with a number of the right magnitude, the right sign and no meaning. Summing a closing
balance across twelve months has the same shape and the same wrongness.

---

## 7. Time, versions and snapshots

The server has been maintaining this warehouse since it started. After the first tick, look at
what it did:

```console
$ psql … -c "SHOW HISTORY OF sales.orders;"
 version |   what    |      at       | files_added | files_removed | bytes_added | changed_data |  kept_by
---------+-----------+---------------+-------------+---------------+-------------+--------------+-----------
       0 | created   |               |           0 |             0 |           0 | no           |
       1 | appended  |             0 |           1 |             0 |        3526 | yes          |
       2 | appended  |             0 |           1 |             0 |        3421 | yes          |
       3 | appended  |             0 |           1 |             0 |        3405 | yes          |
       4 | appended  |             0 |           1 |             0 |        3395 | yes          |
       5 | compacted | 1788712921185 |           1 |             4 |        7593 | no           |
(6 rows)
```

Version 5 is the maintenance thread merging the four files into one, on its own, thirty seconds
after startup. `changed_data` is **no** for it, which is the distinction that matters to
anything reading downstream: compaction moved bytes and changed no answer. Getting that wrong
in one direction only — a compaction that added files declaring `dataChange: true` while
removing them with `false` — was one of five defects found while building this view.

Read one table at one version. `SET` is session state, so this needs one session rather than
two `-c` flags:

```console
$ psql -h 127.0.0.1 -p 5433 -U you -d sankhya <<'SQL'
SET VERSION OF sales.orders = 1;
SELECT count(*) FROM sales.orders;
SQL
 count(*)
----------
      250
(1 row)
```

Version 1 was the first of four appends, so 250 rows is the whole of it. A version beyond the
log is **refused** rather than resolving to the newest — serving a version nobody has was
another of those five defects:

```console
$ psql … -c "SET VERSION OF sales.orders = 99;"
ERROR:  `sales.orders` has no version 99. Its newest is 5. `SHOW HISTORY OF sales.orders`
        lists every version it has, and which are pinned
```

Note that `SET` used to be accepted as a **no-op**, so `SET VERSION OF` and `SET SNAPSHOT` both
silently served the present to a caller who had asked for one instant. Settings that would
change an answer are now refused by name if they are not understood.

A **snapshot** is a different thing: one consistent position across *many* tables.

```console
$ psql … -c "CREATE SNAPSHOT month_end EXPIRE AFTER 7 DAYS;"
$ psql … -c "SHOW SNAPSHOTS;"
 snapshot  | state | taken_by |     taken_at     | expires_on | tables |                              pins
-----------+-------+----------+------------------+------------+--------+----------------------------------------------------------------
 month_end | live  | you      | 1788712987851540 |      20709 |      4 | risk.positions sales.orders sales.regions sank.sank_quarantine
(1 row)
```

Re-run `SHOW HISTORY OF sales.orders` now and version 5's `kept_by` column reads `month_end` —
the snapshot is keeping those files alive, which is the mechanism, made visible.

A clone freezes a *thing*; a snapshot freezes a *moment*. A market-risk run reads trades, rates,
curves and hierarchy and must read all of them as of one instant, or the reconciliation problem
this system exists to remove reappears **inside a single query**. The expiry is mandatory, and
a table created after the snapshot is refused rather than answered as empty — a table that did
not exist is not a table that was empty. See [ADR-0019](adr/0019-named-snapshots.md).

```console
$ psql … -c "DROP SNAPSHOT month_end;"
```

---

## 8. A clone, and what it costs

A clone is a **reference** to its origin's files at a version, not a copy. It costs the same
whether the origin holds two rows or a billion, and adds no files of its own.

```console
$ psql … -c "CREATE TABLE regions_frozen CLONE sales.regions;"
$ psql … -c "SELECT (SELECT count(*) FROM sales.regions)        AS origin,
                    (SELECT count(*) FROM sales.regions_frozen) AS clone;"
 origin | clone
--------+-------
      2 |     2
(1 row)
```

The name in the statement was unqualified and the table landed in `sales`. **A clone stays in
its origin's schema**, and naming another one is refused:

```console
$ psql … -c "CREATE TABLE probe.copy CLONE sales.regions;"
ERROR:  a clone stays in its origin's schema, and `sales` is not `probe`. A clone is a
        reference to its origin's files and is authorized through them, so one placed under
        another schema would have its name governed by one policy and its data by another
```

Ask where it came from, and ask what still reads a table **before** a drop refuses:

```console
$ psql … -c "SHOW LINEAGE OF sales.regions_frozen;"
 step |    origin     | origin_version |    cloned_at
------+---------------+----------------+------------------
    1 | sales.regions |              1 | 1788712953618226
(1 row)

$ psql … -c "SHOW DEPENDENTS OF sales.regions;"
      dependent       | relation | reads_version
----------------------+----------+---------------
 sales.regions_frozen | direct   |             1
(1 row)
```

Clone the clone, then try to remove the middle of the chain:

```console
$ psql … -c "CREATE TABLE regions_audit CLONE sales.regions_frozen;"
$ psql … -c "DROP TABLE sales.regions_frozen;"
ERROR:  `sales.regions_frozen` is still read by sales.regions_audit. Removing it is the
        deletion cloning is gated on, arriving through the front door --- materialise them
        first, or drop them
DETAIL:  Drop what still reads it first, or ask `SHOW DEPENDENTS OF` before dropping anything.
         Every name is in the `subjects` of this refusal.
HINT:  sales.regions_audit
```

`SHOW DEPENDENTS` exists because a refusal that names what would break is no use to somebody
who had no way to ask first. Drop the leaf, then the origin:

```console
$ psql … -c "DROP TABLE sales.regions_audit;"
$ psql … -c "DROP TABLE sales.regions_frozen;"
```

Note what you **cannot** drop:

```console
$ psql … -c "DROP TABLE sales.regions;"
ERROR:  [SNK-C0006] the statement uses a feature this build does not implement: data definition
        is not served over this connection; this server is a read path over a published warehouse
DETAIL:  Write to the transactional store and let capture publish it, or publish an external
         table with `sankhya-publish`. See GUIDE.md §3.
```

A clone is metadata this server owns, so it can remove one. A base table is somebody else's
published data, so it cannot. The same refusal answers a write:

```console
$ psql … -c "INSERT INTO sales.orders (id) VALUES (1);"
ERROR:  [SNK-C0006] the statement uses a feature this build does not implement: data
        modification is not served over this connection; this server is a read path over a
        published warehouse
```

The refusal names the supported route, because one that only says no sends somebody looking for
a flag to turn it on, and there is no flag. Note also what it did *not* do: it did not accept
the statement and discard it. That once returned a success tag and did nothing durable, which
is worse than failing.

---

## 9. Vectors and matrices, on columns

`risk.positions` carries a 64-outcome P&L vector and a 4×4 covariance matrix per row, because
the point of a function catalogue is that arithmetic happens **where the data is**:

```console
$ psql … -c "SELECT position_id, book, round(vec_norm_l2(pnl)::numeric,3) AS l2
             FROM risk.positions ORDER BY position_id LIMIT 4;"
 position_id |  book  |   l2
-------------+--------+--------
           1 | rates  | 18.522
           2 | credit | 25.931
           3 | equity | 33.339
           4 | rates  | 40.748
(4 rows)
```

`vec_dot`, `vec_euclidean`, `vec_cosine_similarity`, `vec_cosine_distance`, `vec_norm_l1`,
`vec_norm_l2`, `vec_sum`, `vec_mean`. Linear algebra over matrix columns: `mat_multiply`,
`mat_transpose`, `mat_inverse`, `mat_solve`, `mat_vec`, `mat_determinant`, `mat_trace`.

A matrix is stored flat and its shape comes from **field metadata**, using Arrow's canonical
`arrow.fixed_shape_tensor` extension — a column with no declared shape is refused rather than
assumed square, because that guess is wrong for every rectangular matrix and produces numbers
from values that were never in the same row. Vectors and matrices can also be built in SQL, and
the constructors emit their own shape:

```sql
SELECT mat_determinant(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0));   -- -2
SELECT vec_norm_l2(vec_of(3.0, 4.0));                        -- 5
SELECT mat_trace(mat_identity(4));                           -- 4
```

Shape is part of a matrix's *type*, so `mat_of`'s dimensions must be literals and a wrong
element count is refused **when the query is planned**, not partway through a scan.

**Every reducing kernel is bit-deterministic.** A dot product is a floating-point sum, and a sum
whose order depends on how the query was partitioned returns a different number when the machine
is busier. These use fixed-point accumulation, where integer addition *is* associative, so
order-independence holds by construction rather than by sorting — and it is 1.3× to 3.6× faster
than the sorted sum it replaced. Lane-parallel SIMD accumulation was rejected for this: it is
10–15× faster and computes a different, worse number. On `1e16, 1, -1e16, 1` repeated, whose
exact total is 18, it returns 5 — and 0 when the input is reversed.

Two costs worth knowing: **an array column cannot be pruned** — a minimum and maximum of a
vector prune nothing — and **an array cannot be a key column**.

> **Correction.** QR, SVD and eigendecomposition **ship**, and this
sentence used to say they were deliberately absent. The refusal was real when written — an
in-house SVD that is subtly wrong produces plausible singular values, which is worse than none
— and it was lifted rather than forgotten: `crates/sankhya-math/src/decompose.rs` implements
them by Jacobi rotation on symmetric input, refusing a non-symmetric matrix rather than
symmetrising it, and they are registered as `mat_qr_q`, `mat_qr_r`, `mat_singular_values`,
`mat_cholesky`, `mat_eigenvalues` and `mat_eigenvectors`. What was not done was retracting the
refusal in the eight places that stated it. **A stated refusal silently reversed is the worst
class of claim in this repository**, because a refusal is the one thing a reader is entitled to
treat as permanent.

---

## 10. Check the health of the warehouse

```console
$ SANKHYA_WAREHOUSE=./warehouse ./target/release/sankhya-server doctor
SANKHYA doctor 0.1.0
  warehouse ./warehouse
  4 table(s)

  [critical] backup — no restore drill has ever passed; this threshold has already been crossed.
         Run a restore drill: `sankhya-server drill`. If it fails, the backup is not a backup
         and this is an incident rather than a maintenance task.
         See docs/runbooks/restore-drill.md.

5 check(s) clean, 1 finding(s) of which 1 have a date, 0 check(s) could not run
$ echo $?
1
```

It reads the warehouse directly and does **not** start the server, because the day you want a
diagnostic is often the day the server will not start. `FR-OPS-17` asks for the time until a
problem bites rather than its current value, so findings are ordered by **when**, not by how
bad — a warning that becomes an outage tomorrow outranks an error that has been stable for a
month. Where it cannot compute a date it says so rather than inventing one: a time needs a
rate, and a rate needs at least two observations.

Exit status is `0` clean, `1` findings, `2` a check could not run. The third exists so a
monitoring system cannot read *"I could not look"* as *"nothing found"*.

> **Every remediation names a command that exists.** This is worth stating because it was not
> true. The compaction-debt finding — the remediation on the only alert that can page — used to
> say *"run `sankhya maintenance compact --table sales.orders`"*, and **there is no `sankhya`
> binary**: the CLI is a stub that prints "not built yet" and exits 2. The code was fixed and
> this document's sample output was not, so the false command survived here after it had been
> removed from the product. It now reads:
>
> > Raise the maintenance duty cycle: lower `maintenance.compact_every` (or
> > `maintenance.interval`) in the configuration and send the server SIGHUP, which takes effect
> > on the next tick without a restart. **There is deliberately no command that compacts by
> > hand**: the server is the only maintainer of a warehouse it holds the lock on, and a second
> > writer is the failure `cargo xtask check-writers` exists to stop.
>
> That is `OPS-26`. A remediation naming a command that does not exist is worse than none: it
> costs the person reading it at three in the morning the time it takes to find out, and it is
> the moment they stop trusting the rest of the runbook.

Put it in cron — hourly is enough — and the projections become real:

```cron
17 * * * * SANKHYA_WAREHOUSE=/srv/sankhya/warehouse /usr/local/bin/sankhya-server doctor
```

---

## 11. Prove the backup

Do what the doctor said.

```console
$ SANKHYA_WAREHOUSE=./warehouse ./target/release/sankhya-server backup
SANKHYA backup 0.1.0
  risk.positions at version 1, 12 row(s)
  sales.orders at version 5, 1000 row(s)
  sales.regions at version 1, 2 row(s)
  sank.sank_quarantine at version 0, 0 row(s)

  backup:01a0779a-0e75-7340-94c6-96728dfc456c
  queryable at 0
  manifest ./.sankhya/backup-manifest.json

This backup is unproven until it has been drilled: `sankhya-server drill`.
```

A backup here is a **manifest**, not an archive: it binds the transactional backup you took,
the table versions in the warehouse and the key generation to one point, and protects those
files so they stay readable. It refuses to record an inconsistency.

```console
$ SANKHYA_WAREHOUSE=./warehouse ./target/release/sankhya-server drill
SANKHYA restore drill 0.1.0
  backup:01a0779a-0e75-7340-94c6-96728dfc456c
  risk.positions: verified, 12 row(s)
  sales.orders: verified, 1000 row(s)
  sales.regions: verified, 2 row(s)
  sank.sank_quarantine: verified, 0 row(s)

Proven. 4 table(s) read back and digested.
```

**It read the data back and recomputed its digest.** A file-presence check would have passed on
a truncated Parquet, on a file whose bytes were replaced with another table's, and on
essentially every failure that actually happens — because a *missing* file is loud, and what
goes wrong is that a file is there and wrong. Try it: corrupt a file under
`warehouse/sales/orders/sank_data_date=*/` and drill again. It reports which table could not be
read and exits `1`.

Exit `0` proven, `1` a table did not verify, `2` could not run. **Alert on `2` as well**: a
monitor treating "could not look" as "nothing wrong" reports a backup as proven when nothing
examined it. Both runs are kept in `<data-dir>/restore-drills.jsonl`, append-only and including
the failures — a drill history with no failures describes either a very good system or a drill
that does not really run, and nothing in the history says which. `doctor` reads the last **pass**
from it, never the last attempt.

Now ask again:

```console
$ SANKHYA_WAREHOUSE=./warehouse ./target/release/sankhya-server doctor

Nothing to report.

6 check(s) clean, 0 finding(s) of which 0 have a date, 0 check(s) could not run
```

**Backup covers the analytical half only.** Backing up a transactional store is your own
tooling's job; the manifest binds to it and does not take it.

---

## 12. Watch what it is doing

```console
$ curl -s http://127.0.0.1:9464/metrics | grep -E '^sankhya_(queries|rows|audit|connections)'
sankhya_queries_total{outcome="error"} 6
sankhya_queries_total{outcome="ok"} 16
sankhya_rows_returned_total 20
sankhya_connections_active 0
sankhya_audit_records_total 20
```

Its own port, one route, loopback by default. The full list is [`METRICS.md`](METRICS.md),
which is **generated from the declarations** — recording a metric requires passing its
declaration, so an undeclared metric cannot be typed, and a declared one that nothing records
fails the build.

Two things before you build a dashboard:

- **`refused` is not `error`.** A quota held is the system working. An error-rate alert that counts them together fires on correct behaviour.
- **Watch `sankhya_metrics_rejected_total`.** Non-zero means a call site disagrees with the catalogue, or a label has outgrown its cap and that metric is now incomplete.

Every error a client sees carries a permanent code, and `DETAIL` is the catalogue's own
remediation — so the client and [`ERRORS.md`](ERRORS.md) cannot say different things. The
codes are what a support conversation is conducted in and what a [runbook](runbooks/) is
indexed by. Note that **thirteen codes in that catalogue are marked "not produced by this
build"**: they exist and no path raises them.

---

## 13. Publishing a table from outside

An external system publishes through **this system's library**, not by assembling the format
itself:

```console
$ ./target/release/sankhya-publish verify ./warehouse/sales/orders
```

The format is open and documented, and the library is in this repository under the same licence
for anyone who wants to see exactly what it does. What it provides is not secrecy but
**correctness by construction**: there is no way to call it that produces a file without
statistics, a schema that does not round-trip, or an action missing a field the format requires.

The reason is asymmetry. A *reader* that misunderstands the format is wrong for itself,
recoverably. A *writer* that misunderstands it corrupts the table for everyone, permanently,
and undetectably — because the writer's own reader shares the misunderstanding. This system made
exactly that mistake once, writing its own format with the specification open.

`verify` does not assume the library was used, because a recommendation is not an invariant. It
reports *what* is wrong rather than *whether*, and distinguishes a finding that makes queries
**slow** from one that makes them **wrong** — exit 0 clean, 1 slow, 2 wrong, so a build gate can
fail on one and not the other. If a table is deficient it can be repaired:

```bash
./target/release/sankhya-publish repair ./warehouse/sales/orders          # shows a plan
./target/release/sankhya-publish repair ./warehouse/sales/orders --apply  # carries it out
```

Repair **derives, never guesses**. Statistics are recomputed by reading the file, because the
file is the truth. Anything needing a guess — a missing schema, a key column that does not
exist — is refused with what a person has to decide, because a tool that invents a plausible
value writes it into the table permanently with an operator's confidence attached. It never
deletes, and it appends a new version rather than rewriting a committed one, so the repair is
auditable and revertible and time travel to before it still works.

---

## 14. Tidy up

```bash
# stop the server with Ctrl-C; it drains on shutdown
rm -rf warehouse .sankhya
```

The whole `.build/` directory is scratch and safe to delete; `vendor/postgresql/build.sh`
recreates what it needs.

---

## Two things this fixture cannot show you

Recorded here rather than worked around, because a walkthrough that quietly avoids what it
cannot demonstrate is how the last version of this document came apart.

1. **The ambiguous bare name.** §5 shows `SELECT count(*) FROM orders` resolving, and says it stops the day a second schema holds an `orders`. That refusal is real and well-tested, and **this fixture cannot produce it**, because only `sales` has an `orders`. Demonstrating it needs a second table of that name in another schema — a two-line addition to `crates/sankhya-server/tests/make_warehouse.rs`.
2. **A clone target that is not load-bearing.** §8 clones `sales.regions`, which is the dimension table the `sales` cube joins to. That works, and it means the walkthrough mutates a table the rest of the document depends on — you must drop the clones afterwards, in order, or `SHOW DEPENDENTS OF sales.regions` keeps reporting one. A small throwaway table existing only to be cloned would remove the ordering constraint.

Neither is a defect in the product. Both are the fixture being one table short of the document,
which is the exact class of gap that made the previous walkthrough unfollowable, so they are
named rather than left for the next reader to discover.

---

## Appendix: watching capture work

> **Not built: there is no change-capture runtime.** Everything in this appendix runs in a
> **test harness**, not in a server you started. The section it replaces was titled *"Watch
> capture work"* and ran exactly this harness, which invited a reader to conclude that the
> server they had just started was capturing something. It is not. `sankhya-cdc-pg`,
> `sankhya-cdc-apply`, `sankhya-cdc-model` and `sankhya-ingest` are not dependencies of
> `sankhya-server` at all — the audit records this as `ING-00`.
>
> What is real is everything *except* the driver: the `pgoutput` wire decoder validated against
> a real PostgreSQL 17.11 stream, an apply path whose transaction invariant is property-tested,
> lossless type mapping, reconciliation against an independent count taken from the source, and
> a crash at any point yielding each row exactly once. Running the harness is the only way to
> see that today, and it is worth seeing.

Start a throwaway cluster configured for logical replication:

```bash
export PGBIN=$PWD/.build/pg-install/bin
export PGSOCK=/tmp/sankhya-sock
mkdir -p "$PGSOCK"

$PGBIN/initdb -D .build/pg -U sankhya --auth=trust -E UTF8 --no-sync

cat >> .build/pg/postgresql.conf <<'CONF'
wal_level = logical
max_replication_slots = 8
max_wal_senders = 8
listen_addresses = ''
unix_socket_directories = '/tmp/sankhya-sock'
max_slot_wal_keep_size = 8GB
CONF

$PGBIN/pg_ctl -D .build/pg -l .build/pg.log start -w
```

Create the fixture schema, a publication, and some data:

```bash
cargo run --release -p sankhya-datagen --bin sankhya-datagen -- ddl \
  | $PGBIN/psql -h $PGSOCK -U sankhya -d postgres -q

$PGBIN/psql -h $PGSOCK -U sankhya -d postgres -c \
  "CREATE PUBLICATION sankhya_all FOR ALL TABLES;"

cargo run --release -p sankhya-datagen --bin sankhya-datagen -- copy --gb 1 \
  | $PGBIN/psql -h $PGSOCK -U sankhya -d postgres -q -v ON_ERROR_STOP=1
```

The full acceptance dataset is `--gb 10`: **99.2 million rows across ten tables**, about three
minutes and 14 GB on disk. The ten schemas are deliberately drawn from logistics, telemetry,
retail, media, energy and civic domains. None is financial — the general-purpose claim is tested
rather than asserted, and fixtures from a single industry would let a core quietly shaped around
that industry pass every test.

Then run the harness:

```bash
export SANKHYA_PG_BIN=$PWD/.build/pg-install/bin
export SANKHYA_E2E_SOCKET=$PGSOCK
crates/sankhya-cdc-apply/tests/run_e2e.sh
```

It issues a workload, captures the resulting replication stream, decodes it, applies it, and
**reconciles the result against an independent count taken from the source** — not against a
second pass of our own decoder, which would let a shared defect cancel out and pass.

```
e2e: 933 messages decoded, 922 mutations across 4 transactions, reconciled against source
```

**One setting that will otherwise confuse you.** If capture appears to see nothing while rows
are plainly present, check `SHOW synchronous_commit;`. Under `synchronous_commit = off` a
transaction returns before its WAL reaches disk, and **logical decoding reads flushed WAL only**.
The change is durable enough to query and entirely invisible to capture, and every symptom that
normally indicates a problem looks healthy: the source is up, the slot is valid, the rows are
there, no errors are reported. See
[`adr/0002-async-commit-and-decoding-visibility.md`](adr/0002-async-commit-and-decoding-visibility.md).

Clean up:

```bash
$PGBIN/pg_ctl -D .build/pg stop -m fast
rm -rf .build/pg /tmp/sankhya-sock
```

---

## What does not work yet

**The single canonical inventory is [`STATUS.md`](STATUS.md)**, which opens with *what works
today* and then lists what is not built with the evidence for each entry. This document
deliberately does not keep its own copy: it used to, and so did a dozen other documents, and
each was partly stale in a different way.

The one-sentence version, so you can decide whether to read further: **the correctness
contracts are built and tested, and for the ingest half the machinery that would run them
continuously is not**. What you started above is a real single-node analytical warehouse over an
open format, with cubes, clones, snapshots, maintenance, backup and an audited query path. What
it is not yet is the three-engine system the architecture describes — there is no change-capture
runtime, no transactional tier wired into the server, no graph hydration on a timer, and no
second node.

Next: [`GUIDE.md`](GUIDE.md) is what to *do* with a running server, worked through by example,
every example executed by a test. [`TUTORIALS`](TUTORIALS.md) are hands-on and in order.
[`GLOSSARY.md`](GLOSSARY.md) defines the terms this document used before explaining them.
[`ROADMAP.md`](ROADMAP.md) says what each release is for.
