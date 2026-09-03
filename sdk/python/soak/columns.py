"""The same functions, called on a **stored column** rather than on literals.

Why this is a separate half of the soak
---------------------------------------
A function reached with a literal and the same function reached over a column take genuinely
different paths through the server. A literal is a scalar broadcast; a column is an Arrow array
read by stride, borrowed out of a contiguous child buffer. The second is the path that matters —
it is what a real query does — and until this file existed the soak exercised only the first.

The comparison it makes is stronger than either path on its own:

    f(literal built from row N's values)  ==  f(column) evaluated at row N

Same values, same function, two entirely different routes into the kernel. A disagreement is a
defect in the marshalling, which is exactly where one would be: a stride read from the wrong
offset, a null mask ignored, a buffer carried from one row to the next.

Why one column can serve every shape
------------------------------------
The fixture's ``risk.positions.pnl`` holds sixty-four simulated outcomes per position. Sixty-four
is a square number, so the same column is a valid 8×8 matrix — which means one column exercises
the series shape, the series-and-number shape, the two-series shape *and* the matrix shape,
without four fixtures that could drift apart.
"""

from __future__ import annotations

#: The table and column the column-based probes read.
TABLE = "risk.positions"
COLUMN = "pnl"
KEY = "position_id"


def _call_over_column(entry, extra: list) -> str | None:
    """How this function is written against the column, or ``None`` if it cannot be.

    ``extra`` supplies whatever is not the column: a window, a probability, a lag.
    """
    name, takes, arity = entry.name, entry.takes, entry.arity

    if takes in ("a series", "a matrix") and arity == 1:
        return f"{name}({COLUMN})"

    if takes == "a series and a number" and arity == 2:
        return f"{name}({COLUMN}, {extra[0]})"

    if takes == "several arrays":
        if arity == 2:
            # A second array argument becomes the column again. Legitimate for every one of
            # them --- a correlation of a series with itself, a paired test against itself, a
            # regression of a series on itself --- and it exercises the two-argument marshalling
            # with two genuinely separate reads of the same column.
            if _needs_number_second(name):
                return f"{name}({COLUMN}, {extra[0]})"
            return f"{name}({COLUMN}, {COLUMN})"
        return None

    return None


#: Two-argument functions whose second argument is a number rather than a series.
#:
#: The catalogue calls both shapes "several arrays", because the wrapper takes both — so this
#: distinction lives here rather than being read off the entry. Listing it is honest; guessing
#: from the name would work until a function was named unlike its neighbours.
_NUMBER_SECOND = {
    "ts_rolling_mean", "ts_rolling_std", "ts_rolling_min", "ts_rolling_max", "ts_ewma",
    "ts_autocorrelation", "var_historical", "expected_shortfall", "sharpe", "sortino",
    "npv", "npv_from_now", "vec_quantile", "vec_scale",
}


def _needs_number_second(name: str) -> bool:
    return name in _NUMBER_SECOND


#: What to pass where a number is wanted, per function.
#:
#: Domain-aware for the same reason the literal generator is: feeding every function the same
#: number would spend the soak confirming that refusals refuse.
def _extra_for(name: str) -> list:
    if name in ("ts_rolling_mean", "ts_rolling_std", "ts_rolling_min", "ts_rolling_max"):
        return [8]
    if name == "ts_ewma":
        return [0.5]
    if name == "ts_autocorrelation":
        return [1]
    if name in ("var_historical", "expected_shortfall"):
        return [0.05]
    if name in ("sharpe", "sortino"):
        return [0.0]
    if name in ("npv", "npv_from_now"):
        return [0.1]
    if name == "vec_quantile":
        return ["vec_of(0.5)"]
    if name == "vec_scale":
        return [2.0]
    if name.startswith("ttest_1samp"):
        return [0.0]
    return [1.0]


def probes(catalogue) -> list:
    """One column-based probe per function that can be written against a column."""
    out = []
    for entry in catalogue:
        extra = _extra_for(entry.name)
        call = _call_over_column(entry, extra)
        if call is not None:
            out.append((entry, call))
    return out


def literal_of(values: list) -> str:
    """One row's values, written as a literal the same function can be called with."""
    inner = ", ".join(repr(float(v)) for v in values)
    return f"vec_of({inner})"
