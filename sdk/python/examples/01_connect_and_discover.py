"""Connect, and ask the warehouse what it holds.

    python3 sdk/python/examples/01_connect_and_discover.py

Read-only. Safe to run against anything.
"""

from _common import a_table, heading, open_warehouse


def main():
    with open_warehouse() as db:
        heading("what is on the other end of this socket")
        # A binding that assumed it was talking to SANKHYA would misreport a PostgreSQL as one.
        # `contract` is `None` for anything else speaking the same wire protocol, and that
        # distinction is the client's only defence against a confident wrong answer.
        print("server version :", db.version())
        print("contract       :", db.connection.contract)

        heading("schemas")
        for schema in db.schemas():
            print(" ", schema)

        heading("tables")
        for table in db.tables():
            print(" ", table.qualified)

        table = a_table(db)
        heading(f"columns of {table}")
        for column in db.columns(table):
            null = "null" if column.nullable else "not null"
            print(f"  {column.name:<16} {column.type_name:<12} {null}")

        heading("a name that is not there")
        # `exists` is a question, not a refusal. Asking is how a script decides what to do;
        # discovering by failure is how a script decides what to crash on.
        print(f"  {table:<16}", db.exists(table))
        print(f"  {'no_such_table':<16}", db.exists("no_such_table"))


if __name__ == "__main__":
    main()
