"""Value-at-risk from Python, computed where the data is.

    python3 sdk/python/examples/10_value_at_risk.py

Read-only. Needs a table of profit-and-loss vectors; it says so and stops otherwise.

The point of this file
----------------------
Every other example calls a function on a literal carried from Python. That shows the surface
and hides the reason for it: **a user who ships their data to a client to do arithmetic has left
the warehouse**, and the warehouse is where the arithmetic is cheap.

So nothing here moves a P&L vector. Twelve positions, sixty-four simulated outcomes each, and
every number below is computed by the server over the stored column --- the client sends a
statement and receives twelve rows.
"""

from _common import heading, open_warehouse
import sankhya
from sankhya import col


def _numbers(rendered):
    """A `float8[]` as it arrives on the wire, back to numbers.

    `{1,2.5,3}` is PostgreSQL's own array syntax, which every driver decodes --- see
    `ADR-0021` Decision 3. Written out here rather than reached for, because this example is
    about what crosses the wire and the reader should see the shape of it.
    """
    inner = rendered.strip().strip("{}")
    return [part for part in inner.split(",") if part]


TABLE = "risk.positions"


def main():
    with open_warehouse() as db:
        if not db.exists(TABLE):
            print(f"this example needs `{TABLE}`, a table with a `pnl` vector column.")
            print("Run it against the quickstart warehouse.")
            return

        columns = {c.name: c.type_name for c in db.columns(TABLE)}
        heading("the table")
        for name, kind in columns.items():
            print(f"  {name:14} {kind}")
        print()
        print("  `pnl` is one row's simulated outcomes --- a vector per position, not a")
        print("  column per outcome. Sixty-four numbers in one value, stored contiguously,")
        print("  which is what lets a kernel read them without copying.")

        heading("value at risk, one position per row")
        # The 5% quantile of each position's outcome distribution. Computed by the server, over
        # the stored column: the vectors never cross the wire.
        rows = db.fn.vec_quantile(
            col("pnl"),
            [0.05],
            frm=TABLE,
            keep=["position_id", "book"],
        )
        for row in rows[:6]:
            print(f"  position {row['position_id']:>2}  {row['book']:<7} VaR(95%) {row['result']:>9.4f}")
        print(f"  ... {len(rows)} position(s) in total")

        heading("the same thing as one statement, which is what it becomes")
        # `db.fn.<name>(..., frm=...)` builds this. Written out because a reader should be able
        # to see what was sent, and because anything more elaborate than a column reference is
        # a statement rather than a method call.
        for row in db.rows(
            f"""
            SELECT position_id,
                   book,
                   vec_quantile(pnl, vec_of(0.05)) AS var_95,
                   vec_quantile(pnl, vec_of(0.01)) AS var_99,
                   vec_mean(pnl)                   AS expected,
                   vec_stddev(pnl)                 AS volatility,
                   vec_min(pnl)                    AS worst
            FROM {TABLE}
            ORDER BY var_95
            LIMIT 5
            """
        ):
            print(
                f"  position {row['position_id']:>2}  VaR95 {float(row['var_95']):>8.3f}"
                f"  VaR99 {float(row['var_99']):>8.3f}"
                f"  sigma {float(row['volatility']):>7.3f}"
                f"  worst {float(row['worst']):>8.3f}"
            )

        heading("expected shortfall, from the quantile the server already computed")
        # The mean of the outcomes at or below the VaR. Written as one statement because it is
        # one question --- and because sending the vectors here to average them is exactly the
        # thing this example exists to avoid.
        for row in db.rows(
            f"""
            SELECT position_id,
                   vec_quantile(pnl, vec_of(0.05)) AS var_95,
                   vec_mean(pnl)                   AS expected
            FROM {TABLE}
            WHERE vec_quantile(pnl, vec_of(0.05)) < -2.0
            ORDER BY var_95
            """
        ):
            print(
                f"  position {row['position_id']:>2}  VaR95 {float(row['var_95']):>8.3f}"
                f"  mean {float(row['expected']):>8.3f}"
            )

        heading("a book's risk, by rolling the positions up")
        for row in db.rows(
            f"""
            SELECT book,
                   count(*)                          AS positions,
                   round(min(vec_min(pnl))::numeric, 3)    AS worst_outcome,
                   round(avg(vec_stddev(pnl))::numeric, 3) AS mean_volatility
            FROM {TABLE}
            GROUP BY book
            ORDER BY book
            """
        ):
            print(
                f"  {row['book']:<8} {row['positions']:>2} position(s)"
                f"  worst {row['worst_outcome']:>9}"
                f"  mean sigma {row['mean_volatility']:>7}"
            )

        heading("a covariance matrix, and whether it could have come from data")
        # Two positions' outcomes, correlated. `vec_covariance` reads both columns of one row;
        # here the two vectors come from two rows, which is what a self-join is for.
        for row in db.rows(
            f"""
            SELECT a.position_id AS one,
                   b.position_id AS other,
                   round(vec_correlation(a.pnl, b.pnl)::numeric, 6) AS correlation
            FROM {TABLE} a JOIN {TABLE} b ON a.position_id < b.position_id
            WHERE a.position_id <= 2 AND b.position_id <= 3
            ORDER BY 1, 2
            """
        ):
            print(f"  {row['one']} against {row['other']}: correlation {row['correlation']}")
        print()
        print("  These positions share a shape, so they are perfectly correlated --- which is")
        print("  a property of the fixture and is worth saying, because a correlation of one")
        print("  read as a finding is how a fixture becomes a conclusion.")

        heading("what it costs to compute here, and what it costs to compute there")
        # The claim this whole catalogue rests on is that computing in the warehouse means only
        # the *answers* cross the wire. That is a claim until somebody measures it, so here it
        # is measured: the same twelve VaR numbers, reached two ways, with the bytes counted.

        # `list(...)` because `rows` is lazy: measuring around an iterator that has not been
        # consumed measures nothing, which is what the first version of this did --- both
        # numbers came out zero and the comparison read as a pass.
        before = db.connection.bytes_received
        server_side = list(
            db.rows(
                f"SELECT position_id, vec_quantile(pnl, vec_of(0.05)) AS var_95 FROM {TABLE}"
            )
        )
        in_warehouse = db.connection.bytes_received - before

        before = db.connection.bytes_received
        # The same answer, computed in Python. Every outcome has to arrive for this to work.
        fetched = list(db.rows(f"SELECT position_id, pnl FROM {TABLE}"))
        client_side = db.connection.bytes_received - before

        def quantile(values, probability):
            """The same linear interpolation the server uses, so the answers can be compared."""
            ordered = sorted(values)
            position = (len(ordered) - 1) * probability
            low = int(position)
            high = min(low + 1, len(ordered) - 1)
            return ordered[low] + (ordered[high] - ordered[low]) * (position - low)

        computed = [
            (row["position_id"], quantile([float(v) for v in _numbers(row["pnl"])], 0.05))
            for row in fetched
        ]

        # And they agree, which is what makes the comparison a comparison of cost rather than
        # of two different answers.
        agree = all(
            abs(float(server["var_95"]) - client[1]) < 1e-9
            for server, client in zip(server_side, computed)
        )
        assert agree, "the two routes gave different answers, so this compares nothing"
        print(f"  the two agree: {agree}")
        print()
        print(f"  computed in the warehouse : {in_warehouse:>7} bytes received")
        print(f"  computed in the client    : {client_side:>7} bytes received")
        if in_warehouse:
            print(f"  ratio                     : {client_side / in_warehouse:>7.1f}x")
        print()
        print("  Twelve positions of sixty-four outcomes. A real book has thousands of")
        print("  positions and tens of thousands of outcomes, and the ratio does not improve")
        print("  with size --- it is the whole distribution against one number per row.")
        print()
        print("  That is the point of the catalogue: a user who has to leave the warehouse to")
        print("  do arithmetic has left the warehouse.")

        heading("and a refusal, which travels the same way")
        try:
            db.fn.vec_quantile(col("pnl"), [1.5], frm=TABLE, limit=1)
        except sankhya.Refusal as refused:
            print(f"  {refused.sqlstate}  {refused.message[:92]}")


if __name__ == "__main__":
    main()
