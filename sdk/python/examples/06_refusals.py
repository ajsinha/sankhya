"""Refusals arrive as data, with a code, a message and what they are about.

    python3 sdk/python/examples/06_refusals.py

Read-only: every statement here is meant to fail.
"""

from _common import heading, open_warehouse
import sankhya


def show(db, sql, why):
    print(f"\n  {sql}")
    print(f"    ({why})")
    try:
        db.sql(sql)
        print("    ...was accepted")
    except sankhya.Refusal as refused:
        print(f"    {refused.sqlstate}  {refused.message}")
        if refused.subjects:
            # What the refusal is ABOUT, as a list rather than as prose a client has to parse.
            # A client that had to regex the message for a table name would break the first
            # time the wording improved.
            print(f"    about: {', '.join(refused.subjects)}")


def main():
    with open_warehouse() as db:
        heading("a table that is not there")
        show(db, "SELECT * FROM no_such_table", "42P01, undefined_table")

        heading("a column that is not there")
        show(db, "SELECT no_such_column FROM sales.orders", "42703, undefined_column")

        heading("syntax")
        show(db, "SELEKT 1", "42601, syntax_error")

        heading("things that are refused rather than quietly ignored")
        # The class that matters most. Each of these WOULD have produced an answer --- a
        # plausible one, with rows in it --- and the answer would have been to a question
        # nobody asked.
        show(
            db,
            "SELECT * FROM sales.orders TABLESAMPLE BERNOULLI (10)",
            "a sample silently ignored is a full scan reported as a sample",
        )
        show(
            db,
            "CREATE SNAPSHOT example_no_expiry",
            "there is no default lifetime, and saying so is the point",
        )
        show(
            db,
            "SET SNAPSHOT = 'no_such_snapshot'",
            "refused at the SET, not at the next query",
        )

        heading("and the connection is still usable")
        # A refusal is an answer, not a failure of the connection. A client that reconnected
        # after every refusal would lose its session settings without noticing.
        print("  SELECT 1 ->", db.scalar("SELECT 1"))


if __name__ == "__main__":
    main()
