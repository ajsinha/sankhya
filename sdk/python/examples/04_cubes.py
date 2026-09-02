"""Declaring a cube, and navigating it --- roll up, slice.

    python3 sdk/python/examples/04_cubes.py

Creates and drops its own cube. Safe to re-run.

Needs a fact table with `region` and `amount` and a `regions` table to hang the dimension on,
which is what the quickstart warehouse has. It finds them rather than assuming their names, and
says plainly what is missing when they are not there.
"""

from _common import heading, open_warehouse
import sankhya


CUBE = "example_sales_by_region"


def _fact_and_dimension(db):
    """A fact table with `region` and `amount`, and the table its dimension hangs on.

    Prefers a separate `regions` table where there is one, and otherwise uses the fact table
    itself --- a *degenerate* dimension, where the member list is the distinct values already
    in the fact. Both are legitimate, and taking the second means this example runs against a
    warehouse that has not been given a dimension table yet.
    """
    tables = [table for table in db.tables() if table.schema != "sank"]
    names = {table.qualified for table in tables}
    best, most = (None, None), -1
    for table in tables:
        columns = {column.name for column in db.columns(table.qualified)}
        if not {"region", "amount"} <= columns:
            continue
        beside = f"{table.schema}.regions" if table.schema else "regions"
        if beside not in names:
            beside = table.qualified
        # The one with rows in it. A cube over an empty fact table declares cleanly and
        # publishes no cells, so every navigation below would refuse --- an example that
        # demonstrates the error path and calls it a demonstration.
        rows = int(db.scalar(f"SELECT count(*) FROM {table.qualified}") or 0)
        if rows > most:
            best, most = (table, beside), rows
    return best


def main():
    with open_warehouse() as db:
        fact, dimension = _fact_and_dimension(db)
        if fact is None:
            print("no table here has both `region` and `amount`; nothing to build a cube on.")
            print("Run this against the quickstart warehouse.")
            return

        print(f"  fact table: {fact.qualified}   dimension table: {dimension}")

        try:
            db.drop_cube(CUBE)
        except sankhya.Refusal:
            pass

        heading("declare one")
        # The cube language is the SERVER'S. A binding that built this from Python objects
        # would be a second definition of what a cube is.
        #
        # A measure must say HOW IT COMPOSES along each dimension. One that cannot be derived
        # from its parts --- a ratio, a percentile --- says so, and the server refuses to roll
        # it up rather than summing it into a wrong number nobody notices.
        db.create_cube(
            f"CREATE CUBE {CUBE} FROM {fact.qualified} "
            f"DIMENSION region FROM {dimension} ON region "
            "(LEVEL area = region) "
            "MEASURE amount (SUM ALONG region)"
        )

        heading("it is discoverable, so a client can offer a picker")
        for cube in db.cubes():
            print(f"  {cube.name} over {cube.fact_table}")
            print(f"    dimensions: {cube.dimensions}")
            print(f"    measures  : {cube.measures}")

        print("\n  dimensions:")
        for row in db.cube_dimensions(CUBE).rows:
            print("   ", row)
        print("  measures:")
        for row in db.cube_measures(CUBE).rows:
            print("   ", row)

        heading("roll up --- aggregate a dimension AWAY")
        # READ `completeness` AND `withheld`. A roll-up over a dimension with null members
        # leaves those rows out, and those two columns are how you learn that a third of the
        # value is missing from an otherwise entirely plausible total.
        result = db.rollup(CUBE, "amount", by="region")
        print("  columns:", result.columns)
        for row in result.rows:
            print("   ", row)

        heading("the grand total --- no dimension kept")
        for row in db.rollup(CUBE, "amount").rows:
            print("   ", row)

        heading("slice --- fix one member and look at the rest")
        # `where` is required. A slice with nothing fixed is a roll-up, and answering it as
        # one would give the right number to the wrong question.
        member = next(
            (row["region"] for row in db.rows(f"SELECT DISTINCT region FROM {fact.qualified} "
                                              "WHERE region IS NOT NULL LIMIT 1")),
            None,
        )
        if member:
            for row in db.slice(CUBE, "amount", where=f"region:{member}").rows:
                print("   ", row)

        heading("clean up")
        db.drop_cube(CUBE)
        print("  cubes now:", [cube.name for cube in db.cubes()])


if __name__ == "__main__":
    main()
