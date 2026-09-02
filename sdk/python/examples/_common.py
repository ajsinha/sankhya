"""What every example needs: a connection, and a table to point at.

Why this file exists
--------------------
The examples are **gated like tests**. An example that does not run is documentation that
lies, and it lies most convincingly right after the code it describes has changed.

That gate runs them against a fixture warehouse; a reader runs them against theirs. So nothing
here hard-codes a table: each example asks the catalogue what is there and picks one. An
example that only runs against the author's data is an example that only the author has run.
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import sankhya  # noqa: E402


def open_warehouse():
    """Connect the way the environment says to.

    ``SANKHYA_PORT``, ``SANKHYA_HOST`` and ``SANKHYA_USER`` override the defaults, which is how
    the gate points these at the server it started.
    """
    return sankhya.open(
        host=os.environ.get("SANKHYA_HOST", "127.0.0.1"),
        port=int(os.environ.get("SANKHYA_PORT", "5432")),
        user=os.environ.get("SANKHYA_USER", "quickstart"),
    )


def a_table(db, prefer="sales.orders"):
    """A qualified table name that exists on *this* warehouse and has rows in it.

    Prefers the one the examples were written against and otherwise picks the **largest**
    table there is. Not the first: the first alphabetically was an empty one, and an example
    that runs against no rows prints nothing and passes --- which is the shape of a test that
    cannot fail, arriving in the documentation.
    """
    if db.exists(prefer) and db.scalar(f"SELECT count(*) FROM {prefer}"):
        return prefer

    best, most = None, -1
    for table in db.tables():
        # `sank` holds the system's own tables. Reading one would work and would teach the
        # wrong thing, so it is never chosen.
        if table.schema == "sank":
            continue
        rows = db.scalar(f"SELECT count(*) FROM {table.qualified}") or 0
        if int(rows) > most:
            best, most = table.qualified, int(rows)

    if best is None:
        raise SystemExit("this warehouse has no tables to read; nothing to demonstrate")
    if most == 0:
        print(f"  (every table here is empty; using {best}, which will show little)")
    return best


def heading(text):
    print()
    print(f"== {text} ==")
