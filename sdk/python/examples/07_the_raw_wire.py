"""Underneath the methods: the connection itself.

    python3 sdk/python/examples/07_the_raw_wire.py

Read-only.

The binding is thin by decision --- `ADR-0017` Decision 1, *no logic the server does not
enforce* --- so anything it does not have a method for, you can still send. That is the point
of a thin client rather than a limitation of this one.
"""

from _common import a_table, heading, open_warehouse
import sankhya


def main():
    heading("the raw wire")
    with sankhya.connect(host="127.0.0.1", port=_port(), user=_user()) as wire:
        # What the server told us at startup, before any query was sent.
        print("  startup parameters:")
        for name, value in sorted(wire.parameters.items()):
            print(f"    {name:<24} {value}")
        print("  contract:", wire.contract)
        print()
        print("  `contract` is `None` for anything else speaking this protocol --- a real")
        print("  PostgreSQL, say. A binding that assumed otherwise would misreport one as a")
        print("  SANKHYA, which is the one thing a thin client must never do.")

    with open_warehouse() as db:
        table = a_table(db)

        heading("the same connection, reached through the methods")
        print("  server version:", db.version())
        print("  settings the server reports:")
        for name, value in sorted(db.settings().items()):
            print(f"    {name:<24} {value}")

        heading("streaming a large result")
        # `stream` yields batches as they arrive rather than accumulating the whole answer,
        # which is what makes a result larger than memory a slow answer rather than a crash.
        seen = 0
        for batch in db.connection.stream(f"SELECT * FROM {table}"):
            seen += len(batch.rows)
        print(f"  {seen} rows, in batches")

        heading("anything without a method is still one statement away")
        print("  SELECT 2 + 2 ->", db.scalar("SELECT 2 + 2"))


def _port():
    import os
    return int(os.environ.get("SANKHYA_PORT", "5432"))


def _user():
    import os
    return os.environ.get("SANKHYA_USER", "quickstart")


if __name__ == "__main__":
    main()
