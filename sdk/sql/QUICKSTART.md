<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../../docs/assets/wordmark-dice-dark.png">
    <img src="../../docs/assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA from SQL — quickstart

**Document ID:** SNK-SDK-SQL-001
**Version:** 0.1.0
**Status:** Implementation — M0–M8, M10 and M13 complete; M8's scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

Everything SANKHYA does, from a SQL prompt. **No client library, no language runtime, no
driver** — the wire-protocol door speaks the PostgreSQL protocol, so `psql` and anything that
uses it work directly.

This is a peer of [`sdk/python/`](../python/QUICKSTART.md), not a lesser version of it. The
Python binding contains no logic the server does not enforce
([ADR-0017](../../docs/adr/0017-the-client-contract.md) Decision 1), which means **everything
it can do, a SQL prompt can do**. The binding saves you typing; it does not unlock anything.

---

## 1. Connect

```
psql -h 127.0.0.1 -p 5432 -U you -d sankhya
```

No password on a development server. Anything that speaks the PostgreSQL wire protocol
connects, so DBeaver, DataGrip, Metabase and the `psql` in your package manager all work.

> **Use the simple query protocol.** The extended protocol — what most *drivers* use by
> default — is not yet served, and `psql` uses the simple one unless you ask for `\bind`. This
> is being built in M14; it is named here rather than left to be discovered.

## 2. The examples

Each file under [`examples/`](examples/) is runnable as it stands:

```
psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f examples/01-connect-and-discover.sql
```

| File | What it shows |
|---|---|
| [`01-connect-and-discover.sql`](examples/01-connect-and-discover.sql) | what is on the server, and how tables are named |
| [`02-query.sql`](examples/02-query.sql) | selection, aggregation, joins, nulls, the date axis |
| [`03-cloning.sql`](examples/03-cloning.sql) | zero-copy clones, lineage, dependents, and the drop that refuses |
| [`04-cubes.sql`](examples/04-cubes.sql) | declaring a cube and the five navigations |
| [`05-feeds.sql`](examples/05-feeds.sql) | declared ingest, quarantine, and resuming a halted feed |
| [`06-refusals.sql`](examples/06-refusals.sql) | one refusal per path, with what each one tells you |
| [`07-analytics.sql`](examples/07-analytics.sql) | the vector, matrix and statistical surface |
| [`08-snapshots.sql`](examples/08-snapshots.sql) | naming one instant across many tables, reading as of it, and the log underneath |

The examples assume the sample warehouse from the repository's
[`QUICKSTART.md`](../../docs/QUICKSTART.md). Where one needs a table it creates itself, it
creates and drops it.

## 3. Naming a table

Both forms work:

```sql
SELECT id FROM sales.orders;   -- always resolves
SELECT id FROM orders;         -- while only one schema holds an `orders`
```

The bare name stops resolving the day a second schema grows a table of that name, and the
refusal then **names both candidates** rather than choosing one. Anything written down should
qualify.

## 4. What SQL cannot do that a binding can

Nothing, by design — with two mechanical exceptions:

| | Why |
|---|---|
| Stream a result larger than memory | `psql` collects. A binding can iterate. |
| Get a refusal as structured fields | The wire carries them; `psql` renders them as text. |

Both are properties of the *client*, not of the server.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>

## These are tests

Every file here runs in CI against a live server, statement by statement, from
`crates/sankhya-server/tests/sql_examples.rs`. A statement preceded by a `-- REFUSES` line must
fail; every other statement must succeed. **Both** directions are checked, because a
demonstration of a refusal that quietly starts succeeding is a rule that has been removed and a
document that still claims it.

This was not always true, and the cost of that shows: two of these files shipped statements that
could never have run — a cube navigation with the dimension in the measure's place, and a set of
vector functions under names that do not exist. Both had been reviewed. A `psql` script with
`ON_ERROR_STOP off` prints its errors and keeps going, so a wall of output reads as success;
only something that reads the exit of each statement can tell.
