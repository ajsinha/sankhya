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
**Status:** Implementation — M0–M8, M10 and M13 complete; M8's scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14 in progress

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

## 4. Refusals arrive as data

```python
try:
    db.execute("DROP TABLE q3_frozen")
except sankhya.Refusal as refused:
    print(refused.sqlstate)     # what a generic driver branches on
    print(refused.message)      # what happened
    print(refused.detail)       # what to do about it
```

A refusal is **not** a sentence this package parses. [ADR-0017](../../docs/adr/0017-the-client-contract.md)
Decision 2 makes it structured on the wire so that a client dispatching on it never has to match
on prose — because a message a client parses becomes an API nobody may reword.

## 5. What this cannot do yet

Named rather than half-implemented, because a client that silently downgrades is worse than one
that says it cannot:

| Not yet | Why it matters |
|---|---|
| TLS | the connection is in the clear; fine on a loopback, not off it |
| Arrow / columnar results | large results come back as text rows, which is slower and larger |
| Streaming | `execute` collects; a result larger than memory will not fit |
| The extended query protocol | no parameter binding, so no server-side prepared statements |
| Ingest | M14; streaming ingest is M15 |

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
