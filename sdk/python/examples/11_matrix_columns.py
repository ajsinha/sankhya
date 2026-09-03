"""A matrix that is stored, and the arithmetic done where it sits.

    python3 sdk/python/examples/11_matrix_columns.py

Read-only. Needs a table with a declared matrix column; it says so and stops otherwise.

The point of this file
----------------------
A vector column is a run of numbers. A **matrix** column is a run of numbers *and a shape*, and
the shape is the whole difference: sixteen values are a 4x4 or a 2x8, and transposing the wrong
one answers with numbers that were never in the same row.

So the shape is declared once, in the column's metadata, and it survives being written and read
back --- which it did not until 2026-09-02, when the writer was found to be dropping it. Nothing
below carries a matrix into Python to work on it: the client sends a statement, the server
factors and multiplies where the data is, and twelve answers come back.

`ADR-0021` Decisions 4, 4a and 4b.
"""

from _common import heading, open_warehouse
from sankhya import matrix

TABLE = "risk.positions"
COLUMN = "covariance"
ORDER = 4


def main():
    with open_warehouse() as db:
        if not db.exists(TABLE):
            print(f"this example needs `{TABLE}`, a table with a `{COLUMN}` matrix column.")
            print("Run it against the quickstart warehouse.")
            return

        heading("the column")
        for column in db.columns(TABLE):
            if column.name == COLUMN:
                print(f"   {column.name:12} {column.type_name}")
                print(f"   {'':12} a flat array of {ORDER * ORDER}, read as a {ORDER}x{ORDER}")

        # Every one of these is computed in the warehouse. The matrix never crosses the wire;
        # a determinant is one number and a Cholesky factor is sixteen, whatever the row count.
        heading("read where it sits")
        rows = db.rows(
            f"SELECT position_id,"
            f"       mat_determinant({COLUMN}) AS determinant,"
            f"       mat_trace({COLUMN})       AS trace,"
            f"       mat_is_positive_definite({COLUMN}) AS factors"
            f"  FROM {TABLE} ORDER BY position_id LIMIT 4"
        )
        for row in rows:
            print(
                f"   position {row['position_id']:>3}  "
                f"determinant {float(row['determinant']):>10.4f}  "
                f"trace {float(row['trace']):>8.4f}  "
                f"positive definite: {row['factors']}"
            )

        # The functions that need a *declared* shape. These are the ones that refuse a bare
        # array --- and the ones that could not be reached from a column at all until the shape
        # survived being stored.
        heading("shape-dependent, over the stored column")
        transposed = db.rows(
            f"SELECT mat_transpose({COLUMN}) AS t FROM {TABLE} ORDER BY position_id LIMIT 1"
        )
        for row in transposed:
            print(f"   transpose  {row['t']}")

        # And the same thing from a literal, where the shape is written down instead. `matrix()`
        # renders as `mat_of(rows, columns, ...)`, which is how a shape is spelled in SQL.
        heading("the same function, over a literal that declares its shape")
        values = [float(v) for v in range(1, ORDER * ORDER + 1)]
        print(f"   transpose  {db.fn.mat_transpose(matrix(ORDER, ORDER, values))}")
        print(f"   trace      {db.fn.mat_trace(values)}")

        heading("what crossed the wire")
        print("   the statements above, and the answers. No matrix was carried into Python:")
        print(f"   twelve stored {ORDER}x{ORDER} matrices stayed where they were written.")


if __name__ == "__main__":
    main()
