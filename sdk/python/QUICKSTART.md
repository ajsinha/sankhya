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
refusals are being built in M14. Section 5 lists what it cannot do yet, by name.

This assumes a SANKHYA is already running. To start one, see the repository's
[`QUICKSTART.md`](../../docs/QUICKSTART.md) — this document begins where that one ends.

---

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

## 5. Reading one instant across many tables

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

## 6. Refusals arrive as data

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

## 7. What this cannot do yet

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
