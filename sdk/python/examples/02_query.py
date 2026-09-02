"""Query, four ways, and what each is for.

    python3 sdk/python/examples/02_query.py

Read-only.
"""

from _common import a_table, heading, open_warehouse


def main():
    with open_warehouse() as db:
        table = a_table(db)
        columns = [column.name for column in db.columns(table)]

        heading("a single value")
        print("rows:", db.scalar(f"SELECT count(*) FROM {table}"))

        heading("a single row, or None")
        # `None` means no row came back. A method that returned an empty list here would make
        # "there is no such row" and "there is a row of nothing" the same answer.
        print(db.one(f"SELECT * FROM {table} LIMIT 1"))
        print(db.one(f"SELECT * FROM {table} WHERE 1 = 0"))

        heading("rows, as dictionaries, streamed")
        for index, row in enumerate(db.rows(f"SELECT * FROM {table} LIMIT 3")):
            print(f"  {index}: {row}")

        heading("the whole result, with its columns")
        result = db.sql(f"SELECT * FROM {table} LIMIT 2")
        print("columns:", result.columns)
        for row in result.rows:
            print("   ", row)

        heading("NULL is not the empty string")
        # They arrive as `None` and `''` and stay different. Conflating them is a wrong answer
        # dressed as a formatting choice --- and the wrong answer is silent.
        nullable = next((c.name for c in db.columns(table) if c.nullable), None)
        if nullable:
            values = {
                row[nullable]
                for row in db.rows(f"SELECT {nullable} FROM {table} LIMIT 40")
            }
            print(f"  distinct {nullable} values seen:", sorted(values, key=str))
            print("  None present:", None in values)

        if "region" in columns and "amount" in columns:
            heading("an aggregate")
            for row in db.rows(
                f"SELECT region, round(sum(amount)) AS total FROM {table} "
                "GROUP BY region ORDER BY region"
            ):
                print(f"  {str(row['region']):<8} {row['total']}")


if __name__ == "__main__":
    main()
