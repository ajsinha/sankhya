"""An aggregation rule of your own, computed where the data is.

    python3 sdk/python/examples/12_your_own_aggregation.py

Read-only apart from declaring one function, which it drops again.

Requires a server that accepts user-supplied aggregations: `server.user_functions: true`,
or `SANKHYA_USER_FUNCTIONS=true`. It is off by default because declaring one runs code the
server did not write. Without it this script is refused with `42501`, by design.

The point of this file
----------------------
A cube's measures compose along each dimension by a **declared rule** --- sum, last, max --- and
that model exists because the alternative produces plausible wrong figures: a balance summed
across time, a rate summed across anything. What it cannot express is the rule that is *this*
firm's.

A weighted mean is the smallest honest example. `MEAN` is refused along any dimension, and
rightly: an average of averages is not an average unless every group is the same size, and
groups are never the same size. A weighted mean carries its weight in its state, so it **does**
compose --- and saying so is what a declared `merge` is for.

What the server does before it believes you
-------------------------------------------
It runs your function against the same numbers one way and several ways, merges the parts in two
different groupings, and compares **bit for bit**. A function that disagrees with itself is
refused at declaration, with both answers. See `ADR-0010`.

And it runs behind an operating-system boundary: no network, no filesystem, no subprocess,
bounded in time and memory (`ADR-0023`). Where that boundary cannot be built, declaring one is
refused naming the mechanism rather than run without it.
"""

from _common import heading, open_warehouse
import sankhya

TABLE = "sales.orders"

WEIGHTED_MEAN = """
def initial():
    return {'total': 0.0, 'weight': 0.0}

def accumulate(state, values):
    # `values` arrives interleaved, one tuple per row: value, weight, value, weight, ...
    # It is a memoryview of doubles, so nothing here builds a list.
    for i in range(0, len(values) - 1, 2):
        state['total'] += values[i] * values[i + 1]
        state['weight'] += values[i + 1]
    return state

def merge(a, b):
    # Its presence is the claim that partial results compose. The server checks the claim.
    return {'total': a['total'] + b['total'], 'weight': a['weight'] + b['weight']}

def finish(state):
    return state['total'] / state['weight'] if state['weight'] else 0.0
"""

BATCH_DEPENDENT = """
def initial():
    return {'total': 0.0, 'batches': 0}

def accumulate(state, values):
    for v in values:
        state['total'] += v
    state['batches'] += 1
    return state

def finish(state):
    return state['total'] / state['batches']
"""


def main():
    with open_warehouse() as db:
        if not db.exists(TABLE):
            print(f"this example needs `{TABLE}`. Run it against the quickstart warehouse.")
            return

        db.drop_aggregation("weighted_margin", if_exists=True)

        heading("declaring one")
        try:
            db.create_aggregation("weighted_margin", WEIGHTED_MEAN)
        except sankhya.Refusal as refused:
            # A machine without unprivileged user namespaces cannot host the boundary, and the
            # honest answer is to say which mechanism is missing rather than run without it.
            print(f"   this server cannot run a user's function: {refused.message}")
            return
        print("   declared, and exercised before it was trusted")

        for declared in db.aggregations():
            print(f"   {declared.name:20} composes: {'yes' if declared.composes else 'no'}")

        heading("using it")
        # An unweighted mean of margins and a mean weighted by order size are different numbers,
        # and the second is the one somebody actually wants.
        rows = db.rows(
            "SELECT region,"
            "       avg(margin_pct)                        AS plain,"
            "       weighted_margin(margin_pct, amount)    AS weighted"
            f"  FROM {TABLE} WHERE region IS NOT NULL GROUP BY region ORDER BY region"
        )
        for row in rows:
            print(
                f"   {row['region']:8} plain {float(row['plain']):.5f}   "
                f"weighted by order size {float(row['weighted']):.5f}"
            )

        heading("what it refuses, and why that matters")
        try:
            db.create_aggregation("mean_of_batches", BATCH_DEPENDENT)
            print("   ...it should not have accepted that")
        except sankhya.Refusal as refused:
            # Not split on a full stop: the message quotes two numbers, and a decimal point
            # is a full stop. Cut at the sentence's own end instead.
            first = refused.message.split(". A cuboid")[0]
            print(f"   {first}")
            print("   A cuboid is built batch by batch and a query reads them whole, so the")
            print("   two would disagree by however much the batching happened to differ ---")
            print("   and neither number would look wrong.")

        db.drop_aggregation("weighted_margin", if_exists=True)


if __name__ == "__main__":
    main()
