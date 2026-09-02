<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../../docs/assets/wordmark-dice-dark.png">
    <img src="../../docs/assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA for Python — quickstart

**Document ID:** SNK-SDK-PY-001
**Version:** 0.1.0
**Status:** Implementation — M0–M8, M10 and M13 complete; M8's scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14 and M17 in progress

This binding is early: the wire-protocol client works, and the columnar path, TLS and typed
refusals are being built in M14. Section 8 lists what it cannot do yet, by name.

This assumes a SANKHYA is already running. To start one, see the repository's
[`QUICKSTART.md`](../../docs/QUICKSTART.md) — this document begins where that one ends.

---

## 0. Eight runnable examples

Before any of this: [`examples/`](examples/) holds one script per capability, each runnable on
its own.

```
pip install -e sdk/python
python3 sdk/python/examples/01_connect_and_discover.py
```

They are **gated like tests** — every one runs in CI against a live server, and must exit zero
and say nothing on stderr. An example that does not run is documentation that lies, and writing
these found four defects a green gate had missed. [`examples/README.md`](examples/README.md)
names them.

## 1. Install

```
pip install -e sdk/python
```

There is nothing to compile. The binding is **pure Python** by decision, not by accident:
[ADR-0017](../../docs/adr/0017-the-client-contract.md) Decision 7 refuses a compiled extension,
because a per-platform wheel matrix and an ABI across three interpreter versions is a large
price for accelerating a layer that is required to contain no logic.

> *If the client is thin, its language does not matter. If its language matters, it is not thin
> enough.*

## 2. Connect and query

```python
import sankhya

with sankhya.connect(host="127.0.0.1", port=5432, user="you") as db:
    result = db.execute("SELECT region, sum(amount) FROM sales.orders GROUP BY region")
    print(result.columns)
    for row in result.rows:
        print(row)
```

`row` values are strings, or `None` for SQL `NULL`. The empty string and `NULL` are different
values and stay different — conflating them is a wrong answer, not a formatting choice.

## 3. Naming a table

Both forms work:

```python
db.execute("SELECT id FROM sales.orders")   # always resolves
db.execute("SELECT id FROM orders")         # while only one schema holds an `orders`
```

The bare name stops resolving the day a second schema grows a table of that name, and the
refusal then **names both candidates** rather than choosing one. Scripts that will outlive
today's warehouse should qualify.

## 4. The contract, checked at connection

This binding declares which **client contract** it speaks, and a server speaking a different
one refuses at connection, naming both:

```python
>>> sankhya.open(port=5432, user="you")
sankhya.Refusal: [08004] this client speaks contract 99 and this server speaks contract 1.
                 Refused here rather than eleven calls from now, when a field turns out to be
                 missing
```

The refusal carries both versions as data — `refused.subjects == ['client=99', 'server=1']` —
and a remediation naming which side to move.

A binding is installed independently of the server: a package index, a container image and a
deployment each move at their own pace, so the two *will* disagree. The only question is
whether it surfaces where somebody can act, or eleven calls later as a missing field.

The server's own contract is readable before you ask it anything:

```python
db.connection.contract     # 1, or None if this is not a SANKHYA
```

`None` means something else is speaking the PostgreSQL wire protocol — which this binding can
talk to, and should not pretend otherwise.

## 5. Discovery, cloning and cubes

Everything the server does is a method. The binding adds no rule of its own — `ADR-0017`
Decision 1: *no logic the server does not enforce* — so a wrong call here produces a confusing
error and never a wrong answer.

### What is there

```python
db.schemas()                    # ['sales', 'reference', ...]
db.tables()                     # [Table(schema='sales', name='orders'), ...]
db.tables(schema="sales")
db.columns("sales.orders")      # [Column(name='id', type_name='int8', nullable=False), ...]
db.exists("sales.orders")       # a question, not a refusal
db.settings()                   # what the server reported at startup
```

`Table.qualified` is `schema.name` — the form that keeps meaning one table. A bare name resolves
only while one schema claims it, so anything written down, or run again next quarter, should use
the qualified one.

### Zero-copy cloning

```python
db.clone("sales.q3_frozen", "sales.orders")           # the present
db.clone("sales.q3_frozen", "sales.orders", at_version=412)

db.is_clone("sales.q3_frozen")
db.lineage_of("sales.q3_frozen")     # [Ancestor(step=1, origin='sales.orders', ...)]
db.dependents_of("sales.orders")     # who breaks if this goes
db.drop("sales.q3_frozen", if_exists=True)
```

**Nothing is copied.** A clone's log names none of its origin's files; a read splices the
origin's live set *at the cloned version* with the clone's own log. The cost is one log with no
files in it, whatever the table's size.

Three rules the server enforces and this binding only relays:

- **A clone stays in its origin's schema.** `db.clone("archive.q3", "sales.orders")` is refused.
  A clone is a reference to its origin's files and is authorized through them, so one placed
  under another schema would have its name governed by one policy and its data by another.
- **A clone of a clone works**, and each step is recorded — `lineage_of` walks the whole chain,
  so nothing has to guess where the files are.
- **Dropping an origin something still reads is refused**, and the refusal names what would
  break in its `subjects`. Ask `dependents_of` *before* dropping; a refusal after the attempt is
  no use to somebody who had no way to ask first.

A clone freezes a **thing**. A snapshot (§6) freezes a **moment**.

### Cubes

```python
db.create_cube(
    "CREATE CUBE sales_by_region FROM sales.orders "
    "DIMENSION region FROM sales.regions ON region (LEVEL area = region) "
    "MEASURE amount (SUM ALONG region)"
)

db.cubes()                       # [Cube(name='sales_by_region', ...)]
db.cube_dimensions("sales_by_region")
db.cube_measures("sales_by_region")

db.rollup("sales_by_region", "amount")                        # the grand total
db.rollup("sales_by_region", "amount", by="region")           # a breakdown
db.slice("sales_by_region", "amount", where="region:north")   # one member fixed
db.rollup("sales_by_region", "amount", by="region", min_completeness=0.5)

db.drop_cube("sales_by_region")
```

The cube language is the **server's**, passed through verbatim. A binding that built the
statement from Python objects would be a second definition of what a cube is, and the two would
drift.

Measure first, dimension second — the order the server's own functions take. A cube holds many
measures and a cell holds one measure's values, so naming a dimension where the measure goes
asks for cells that do not exist.

> **Read `completeness` and `withheld`.** A roll-up over a dimension with null members leaves
> those rows out, and those two columns are how you learn that a third of the value is missing
> from an otherwise entirely plausible total. `min_completeness` turns that into a refusal
> instead of a footnote.

A measure must declare **how it composes** along each dimension. One that cannot be derived from
its parts — a ratio, a percentile — says so, and the server then refuses to roll it up rather
than summing it into a wrong number nobody notices.

### Feeds and the graph

```python
for feed in db.feeds():          # including feeds that have never run
    print(feed.name, feed.state, feed.reason)
db.resume_feed("orders_nightly")
db.quarantine(limit=20)          # what was refused, and why

db.reachable("supply", "acme")
db.shortest_path("supply", "acme", to="zenith")
db.cycles("supply")
db.influence("supply", "acme")
```

## 6. Reading one instant across many tables

```python
db.take_snapshot("eod_2026_09_02", expire_after_days=90)

db.read_as_of("eod_2026_09_02")     # a session setting: this connection, from here on
db.sql("SELECT region, sum(amount) FROM sales.orders GROUP BY region")
db.read_the_present()               # back to now

for held in db.snapshots():
    print(held.name, held.state, held.tables, held.taken_by)
db.drop_snapshot("eod_2026_09_02")
```

A clone freezes a *thing*; a snapshot freezes a *moment*. A calculation reading a population of
records, a set of rates, a set of curves and the hierarchy they roll up through must read all
four **as of one instant**, or the reconciliation problem this system exists to remove reappears
inside a single query.

`expire_after_days` is **required** and there is no unbounded form — a snapshot pins files, so
one that never expired would hold a whole warehouse's versions alive and the cost would fall on
somebody who did not ask for it.

A table created *after* the snapshot is **not there**, and naming it fails to resolve exactly as
a table that does not exist does. It is deliberately not answered as empty: a table that did not
exist is not a table that was empty, and a join against one returns a confident zero.

### The log underneath the tag

```python
for change in db.history("sales.orders"):
    print(change.version, change.what, change.at, change.changed_data, change.kept_by)

db.read_version("sales.orders", 2)     # this table, this session
db.sql("SELECT count(*) FROM sales.orders")
db.read_the_present_of("sales.orders")
```

`read_version` is per table and independent of `read_as_of` — it answers *"what did this table
look like then"*, where a snapshot answers *"what did everything look like then"*. Use a
snapshot when more than one table has to agree.

Two fields of a `Change` carry most of the meaning:

- **`changed_data`** is the writer's own declaration, not a guess from the file counts. A
  compaction rewrites files and changes not one row, so it is `False`. A field that called that
  a change would say your table moved every time maintenance ran.
- **`kept_by`** names the snapshot or clone holding that version alive, and is `None` for a
  version nothing is keeping. **History is readable only where something is keeping it alive** —
  retirement deletes the files a merge replaced, so the commit outlives its data. `change.is_readable`
  is the same fact as a boolean, and only its `True` is worth relying on.

Three refusals, each of which was a wrong answer before it was a refusal:

```python
db.read_version("sales.orders", 9999)   # 42704 — and names the newest it does have
db.read_version("sales.orders", 1)      # 42704 — in the log, and its data is not
db.read_version("sales.nowhere", 1)     # 42P01 — at the SET, not at the next query
```

Replaying a log stops at its end, so the first of those used to hand back the *newest* version:
one nobody has, served as though they had it. The second would have returned whichever rows
happened to survive — a historical query silently missing whatever was compacted, which is the
wrong answer that looks most like a right one, because it has rows in it.

**This is not version control.** There is no diff between two versions and no way to restore
one, because the log records *files* rather than rows: a compaction replaces every file and
changes nothing, so a file-level diff would report a maintenance job as a total rewrite. What
you have is closer to a tag than a branch — name a moment, read it back, and know that the
naming is what keeps it readable.

## 7. Refusals arrive as data

```python
try:
    db.execute("DROP TABLE q3_frozen")
except sankhya.Refusal as refused:
    print(refused.sqlstate)     # what a generic driver branches on
    print(refused.message)      # what happened
    print(refused.detail)       # what to do about it
    print(refused.subjects)     # the NAMES it cites: ['sales.q3_audit']
```

A refusal is **not** a sentence this package parses. [ADR-0017](../../docs/adr/0017-the-client-contract.md)
Decision 2 makes it structured on the wire so that a client dispatching on it never has to match
on prose — because a message a client parses becomes an API nobody may reword.

`subjects` is the one that matters most: the **names** a refusal cites — the clones that would
break, the two tables an ambiguous name could mean. Without it, showing *"three clones read
this table"* means parsing the sentence.

## 8. What this cannot do yet

Named rather than half-implemented, because a client that silently downgrades is worse than one
that says it cannot:

| Not yet | Why it matters |
|---|---|
| TLS | the connection is in the clear; fine on a loopback, not off it |
| Arrow / columnar results | large results come back as text rows, which is slower and larger |
| Streaming | `execute` collects; a result larger than memory will not fit |
| Ingest from the client | M14; streaming ingest is M15 |

The **extended query protocol** — parameter binding, prepared statements — is served by the
server as of 2026-09-02, and this binding does not use it yet. It sends the simple query flow,
so values are quoted into the statement rather than bound. That is stated rather than hidden:
it is the one place this package handles a value, and it goes away when the binding moves to
the extended flow.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
