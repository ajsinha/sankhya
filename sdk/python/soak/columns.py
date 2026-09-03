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

Why one table can serve every shape
-----------------------------------
``risk.positions.pnl`` holds sixty-four simulated outcomes per position. Sixty-four is a square
number, so the same column is a valid 8×8 matrix — which means one column exercises the series
shape, the series-and-number shape, the two-series shape *and* the matrix shape, without four
fixtures that could drift apart.

The scalar functions needed something the vector column could not give. A distribution reached
with a literal is a broadcast of one value; reached over a column it is a ``Float64Array`` read
per row, which is a different piece of marshalling and the one a real query uses. So the table
carries two scalar columns whose **domains are stated by their names** — ``confidence`` lies in
``(0, 1)`` and ``exposure`` is a positive real — because a probability column and a scale column
are not interchangeable, and feeding a distribution the wrong one would spend the soak comparing
refusals.
"""

from __future__ import annotations

import re

from soak.arguments import domain_at

#: The table the column-based probes read.
TABLE = "risk.positions"
#: The vector column: sixty-four outcomes per position.
COLUMN = "pnl"
KEY = "position_id"

#: A **stored** matrix: a 4x4 whose column declares its shape in the field's metadata.
#:
#: A separate column rather than `pnl` read as an 8x8, because the shape has to be *declared*.
#: `mat_transpose` refuses an argument that carries no shape --- sixteen values are a 4x4 or a
#: 2x8, and transposing the wrong one answers with numbers that were never in the same row ---
#: and a `FixedSizeList` column with no metadata declares nothing. This is the column that shows
#: `ADR-0021` Decision 2 actually works: the shape survives being stored and read back.
MATRIX = "covariance"
#: Its order, which the literal route has to restate because a literal carries no metadata.
MATRIX_ORDER = 4

#: A scalar column per domain, so an argument can be filled from stored data rather than from a
#: number written here. A position whose domain has no column stays a literal, and the literal
#: route substitutes the same value — so the comparison is still like for like.
BY_DOMAIN = {
    "probability": "confidence",
    "positive": "exposure",
    "real": "exposure",
}

#: Every column the probes read, so the caller fetches one row and has all of them.
READS = (KEY, COLUMN, MATRIX, "exposure", "confidence")

#: The vector-valued columns, whose text arrives as `{1,2,3}` rather than as a number.
VECTORS = (COLUMN, MATRIX)

#: Functions whose arguments must be literals, so they cannot be written over a column at all.
#: Both take a *shape*: how large the matrix is, not what is in it, and a shape that arrived per
#: row would not be a shape --- the result's type would depend on the data.
LITERAL_ONLY = {"mat_of", "mat_identity"}


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

#: Functions taking a declared matrix and a vector of its order. `pnl` is an 8x8, so the right
#: hand side has eight entries rather than sixty-four.
_MATRIX_THEN_VECTOR = {"mat_solve": MATRIX_ORDER, "mat_vec": MATRIX_ORDER}


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


def _vector_probe(entry, extra: list):
    """How a function over the vector column is written, or ``None``.

    Returns the call twice — once naming the column, once with a placeholder the caller
    replaces by a literal of that column's values.
    """
    name, takes, arity = entry.name, entry.takes, entry.arity

    if takes == "a matrix" and arity == 1:
        return f"{name}({MATRIX})"

    if takes == "a series" and arity == 1:
        return f"{name}({COLUMN})"

    if takes == "a series and a number" and arity == 2:
        return f"{name}({COLUMN}, {extra[0]})"

    if takes == "several arrays" and arity == 2:
        if name in _MATRIX_THEN_VECTOR:
            inner = ", ".join(repr(float(i + 1)) for i in range(_MATRIX_THEN_VECTOR[name]))
            return f"{name}({MATRIX}, vec_of({inner}))"
        if name == "mat_multiply":
            return f"{name}({MATRIX}, {MATRIX})"
        if name in _NUMBER_SECOND:
            return f"{name}({COLUMN}, {extra[0]})"
        # A second array argument becomes the column again. Legitimate for every one of them
        # --- a correlation of a series with itself, a paired test against itself --- and it
        # exercises the two-argument marshalling with two separate reads of the same column.
        return f"{name}({COLUMN}, {COLUMN})"

    return None


def _scalar_probe(entry, row):
    """A scalar function written over the scalar columns, and the same call over literals.

    Every argument position is asked what its domain is — the same tables the literal generator
    uses, so the two halves of the soak cannot disagree about what a probability is — and filled
    from the column that holds that domain. A position whose domain has no column (a count, a
    degrees of freedom) is written as the literal the generator would use, on **both** routes,
    so it is held fixed rather than compared.
    """
    if entry.takes != "numbers":
        return None
    over_columns, over_literals = [], []
    for at in range(entry.arity):
        kind, fallback = domain_at(entry.name, at)
        column = BY_DOMAIN.get(kind)
        if column is None:
            over_columns.append(repr(float(fallback)))
            over_literals.append(repr(float(fallback)))
        else:
            over_columns.append(column)
            over_literals.append(repr(float(row[column])))
    return (
        f"{entry.name}({', '.join(over_columns)})",
        f"{entry.name}({', '.join(over_literals)})",
    )


def _as_literals(call: str, row) -> str:
    """The same call with every column replaced by a literal of that row's values.

    The matrix column becomes `mat_of`, not `vec_of`: it is the only spelling that carries a
    shape, and without one the very functions this exists to reach would refuse.
    """
    for name, literal in (
        (MATRIX, literal_matrix_of(row[MATRIX])),
        (COLUMN, literal_of(row[COLUMN])),
    ):
        # Whole identifiers only. A plain `str.replace` rewrote `vec_covariance` into
        # `vec_mat_of(...)` --- a column name is a substring of several function names, and
        # the resulting refusal looked like a disagreement between the two routes.
        call = re.sub(rf"(?<![A-Za-z0-9_]){name}(?![A-Za-z0-9_])", literal, call)
    return call


def probes(catalogue, row) -> list:
    """One probe per function that can be written against a column.

    Each is ``(entry, the call over columns, the call over literals of the same row)``.
    """
    out = []
    for entry in catalogue:
        if entry.name in LITERAL_ONLY:
            continue
        call = _vector_probe(entry, _extra_for(entry.name))
        if call is not None:
            out.append((entry, call, _as_literals(call, row)))
            continue
        scalar = _scalar_probe(entry, row)
        if scalar is not None:
            out.append((entry, scalar[0], scalar[1]))
    return out


def literal_of(values: list) -> str:
    """One row's values, written as a literal the same function can be called with."""
    inner = ", ".join(repr(float(v)) for v in values)
    return f"vec_of({inner})"


def literal_matrix_of(values: list) -> str:
    """One row's matrix, written as a literal that declares the same shape."""
    inner = ", ".join(repr(float(v)) for v in values)
    return f"mat_of({MATRIX_ORDER}, {MATRIX_ORDER}, {inner})"
