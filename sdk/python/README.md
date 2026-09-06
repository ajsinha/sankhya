# SANKHYA — Python client

```python
import sankhya

with sankhya.open(host="127.0.0.1", port=5433, user="quickstart") as db:
    for row in db.sql("SELECT region, sum(amount) FROM orders GROUP BY region"):
        print(row)
```

`sankhya.open` returns a `Sankhya` — the client with `schemas()`, `tables()`, `clone()`,
`lineage_of()` and `sql()`. `sankhya.connect` returns a lower-level `Connection`, which has
`execute()` and not those methods; mixing the two is the mistake this paragraph exists to
prevent.

**The port is 5433**, not 5432. 5432 is where your own PostgreSQL is, so getting this wrong
fails by connecting to the wrong database and returning plausible answers.

Runnable examples are in [`examples/`](examples/), and every one of them is executed against a
live server by `crates/sankhya-server/tests/sdk_examples.rs` — an example that does not run is
documentation that lies.

The full client reference is [§14 of the guide](../../docs/GUIDE.md).

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
