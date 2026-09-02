# Runnable examples

Eight scripts, one per capability, each runnable on its own against a live SANKHYA.

```
pip install -e sdk/python
python3 sdk/python/examples/01_connect_and_discover.py
```

`SANKHYA_HOST`, `SANKHYA_PORT` and `SANKHYA_USER` override the defaults
(`127.0.0.1`, `5432`, `quickstart`).

| File | What it shows |
|---|---|
| [`01_connect_and_discover.py`](01_connect_and_discover.py) | connecting, the contract, schemas, tables, columns |
| [`02_query.py`](02_query.py) | the four ways to read a result, and why `NULL` is not `''` |
| [`03_cloning.py`](03_cloning.py) | zero-copy cloning, a clone of a clone, lineage, dependents |
| [`04_cubes.py`](04_cubes.py) | declaring a cube, rolling up, slicing, and reading `completeness` |
| [`05_feeds_and_quarantine.py`](05_feeds_and_quarantine.py) | what the feeds are doing, and what they refused |
| [`06_refusals.py`](06_refusals.py) | refusals as data — code, message, subjects |
| [`07_the_raw_wire.py`](07_the_raw_wire.py) | the connection underneath the methods, and streaming |
| [`08_snapshots_and_history.py`](08_snapshots_and_history.py) | naming an instant, reading it back, and the log underneath |

`_common.py` is imported by the rest and is not a script.

## These are tests

Every one of them runs in CI against a real server, from
`crates/sankhya-server/tests/sdk_examples.rs`. Each must exit zero and write nothing to
stderr.

That is not ceremony. **An example that does not run is documentation that lies**, and it lies
most convincingly right after the code it describes has changed — nothing about a stale example
looks stale. Writing these found four defects that unit tests, mutation tests and a green gate
had all missed, every one of them on the front door:

- `CREATE CUBE` could not name a table in a schema **at all** — the qualified form failed to
  parse, and the bare form resolved only while one schema claimed the name.
- A leading `--` comment made the server fail to recognise its own statements. Every script
  here comments its statements, so the feature worked only for somebody who did not write down
  what they were doing.
- `SHOW HISTORY OF`'s `kept_by` column said the word `snapshot` rather than naming one, and
  never mentioned a clone — though a clone keeps a version alive in exactly the same way.
- This binding's `rollup` and `slice` passed the dimension where the **measure** goes, and
  discarded the names of their keyword options.

Each was reachable only by somebody trying to use the product.

## They adapt to your warehouse

Nothing here hard-codes a table. Each script asks the catalogue what is there and picks
something, so the same file runs against the test fixture and against yours. An example that
only runs against the author's data is an example that only the author has run.
