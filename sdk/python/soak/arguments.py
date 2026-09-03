"""Arguments a function will accept, generated from what the catalogue says it takes.

Why the arguments are generated rather than listed
--------------------------------------------------
There are a hundred and twenty-eight functions. A hand-written call for each is a hundred and
twenty-eight chances to write the *easy* call — the one whose arguments the author already knew
were fine — and the parity soak's whole value is in the calls nobody thought about.

Generating them from ``takes``, ``arity`` and the function's own name means a function added to
the server is exercised the next time this runs, with no edit here. A function whose arguments
cannot be generated is **reported**, not skipped silently: a soak that quietly covers a hundred
of a hundred and twenty-eight and prints PASS is a claim nobody checked.

Why the domains are respected
-----------------------------
``norm_inv(5.0)`` is refused, and rightly — a probability is not five. A soak that fed every
function the same numbers would spend its time confirming that refusals refuse, and would never
once compare two paths' *answers*.

So the generator knows what a probability is, what degrees of freedom are, and that a covariance
matrix must factor. That knowledge is about the mathematics, not about the implementation, which
is why it can live in the test.
"""

from __future__ import annotations

import math


class Seeded:
    """A deterministic generator, so a failing soak is reproducible from its seed."""

    def __init__(self, seed: int) -> None:
        self.state = seed | 1

    def next(self) -> int:
        x = self.state
        x ^= (x << 13) & 0xFFFFFFFFFFFFFFFF
        x ^= x >> 7
        x ^= (x << 17) & 0xFFFFFFFFFFFFFFFF
        self.state = x
        return x

    def unit(self) -> float:
        """In ``[0, 1)``."""
        return (self.next() >> 11) / (1 << 53)

    def signed(self, scale: float = 1.0) -> float:
        """In ``[-scale, scale)``."""
        return (self.unit() * 2.0 - 1.0) * scale


#: Functions whose first argument is a probability, and must be in ``(0, 1)``.
_PROBABILITY_FIRST = {
    "norm_inv", "normal_inv", "lognorm_inv", "t_inv", "chisq_inv", "f_inv",
}

#: Functions taking degrees of freedom, and where.
_FREEDOM_AT = {
    "t_pdf": [1], "t_cdf": [1], "t_sf": [1], "t_inv": [1], "t_two_sided": [1],
    "chisq_cdf": [1], "chisq_sf": [1], "chisq_inv": [1],
    "f_cdf": [1, 2], "f_sf": [1, 2], "f_inv": [1, 2],
}

#: Functions whose arguments are counts and must be whole.
_WHOLE_AT = {
    "binom_pmf": [0, 1], "binom_cdf": [0, 1], "poisson_pmf": [0], "poisson_cdf": [0],
}

#: Functions taking a scale or shape that must be positive.
_POSITIVE_AT = {
    "normal_pdf": [2], "normal_cdf": [2], "normal_inv": [2],
    "lognorm_cdf": [2], "lognorm_inv": [2],
    "expon_cdf": [1], "poisson_pmf": [1], "poisson_cdf": [1],
    "gamma_cdf": [1, 2], "beta_cdf": [1, 2], "gamma_p": [0], "gamma_q": [0],
    "beta_i": [0, 1], "gammaln": [0],
}

#: A probability at these positions, for the discrete families.
_PROBABILITY_AT = {"binom_pmf": [2], "binom_cdf": [2]}

#: Functions this generator cannot produce arguments for, with the reason.
#:
#: Named rather than skipped by falling through, so the soak can report its own coverage
#: honestly — see ``unreachable`` below.
CANNOT = {
    "vec_of": "builds a vector from its arguments, so it is how every other call is written",
    "mat_of": "takes a row count and a column count before its values, which no shape describes",
    "mat_identity": "takes an order rather than data",
    "functions": "a table function; it returns rows rather than a value",
    "cubes": "a table function over cubes this soak does not declare",
    "cube_dimensions": "needs a declared cube",
    "cube_measures": "needs a declared cube",
    "cube_rollup": "needs a declared cube with published cells",
    "cube_slice": "needs a declared cube with published cells",
    "graph_reachable": "needs a declared graph",
    "graph_shortest_path": "needs a declared graph",
    "graph_time_respecting": "needs a declared graph",
    "graph_cycles": "needs a declared graph",
    "graph_influence": "needs a declared graph",
    "mat_solve": "takes a matrix and a right-hand side whose length is the matrix's order",
    "mat_multiply": "takes two matrices whose inner dimensions must agree",
    "mat_transpose": (
        "is defined on a rectangular matrix, so it needs a shape declared by `mat_of` --- "
        "sixteen values are a 4x4 or a 2x8, and transposing the wrong one produces numbers "
        "from values that were never in the same row. This generator writes plain arrays"
    ),
    "mat_vec": "takes a matrix and a vector of its order",
}


def _positive_definite(seeded: Seeded, order: int) -> list:
    """A covariance matrix, flat and row-major, that factors.

    Built as ``L·Lᵀ + εI`` so it is positive definite by construction — which is what makes a
    Cholesky failure in the soak a real finding rather than a property of the fixture.
    """
    lower = [[0.0] * order for _ in range(order)]
    for row in range(order):
        for column in range(row + 1):
            lower[row][column] = (
                abs(seeded.signed()) + 0.5 if row == column else seeded.signed()
            )
    flat = []
    for i in range(order):
        for j in range(order):
            total = sum(lower[i][k] * lower[j][k] for k in range(order))
            flat.append(total + (1e-6 if i == j else 0.0))
    return flat


def _series(seeded: Seeded, length: int, positive: bool = False) -> list:
    """A series with genuine variation, because a constant series has no statistics."""
    out = []
    for i in range(length):
        value = 1.0 + i * 0.5 + seeded.signed(0.3)
        out.append(abs(value) + 0.1 if positive else value)
    return out


def for_function(entry, seeded: Seeded):
    """Arguments for one function, or ``None`` if this generator cannot make them.

    ``entry`` is a :class:`sankhya.functions.Function` from the server's own catalogue, so a
    function added to the server is covered here without an edit.
    """
    name, arity, takes = entry.name, entry.arity, entry.takes
    if name in CANNOT:
        return None

    if takes == "a matrix":
        return [_positive_definite(seeded, 4)]

    if takes == "a series":
        # Simpson's rule needs an **even** number of intervals, so an odd number of points.
        # Thirteen rather than twelve, because twelve made `vec_integral_simpson` refuse on
        # every path --- consistent, and a comparison of nothing.
        return [_series(seeded, 13 if name == "vec_integral_simpson" else 12)]

    if takes == "a series and a number":
        if name.startswith("ttest_1samp"):
            # Tested against a value near the sample's own mean, so the statistic is
            # interesting rather than enormous.
            series = _series(seeded, 12)
            return [series, sum(series) / len(series) + 0.4]
        if name == "vec_scale":
            return [_series(seeded, 8), 2.5]
        return [_series(seeded, 12), 1.0]

    if takes == "several arrays":
        if name.startswith("regress_multiple") or name == "regress_ridge_norm":
            # A flat design matrix, its targets, the predictor count, and for ridge a penalty.
            # An intercept column is supplied deliberately: `least_squares` does not add one,
            # so the model fitted is the model written down.
            rows = 14
            design = []
            targets = []
            for i in range(rows):
                t = float(i + 1)
                design.extend([1.0, t, t * t * 0.05])
                targets.append(2.0 + 1.5 * t + seeded.signed(0.3))
            if name == "regress_ridge_norm":
                return [design, targets, 3.0, 1.0]
            return [design, targets, 3.0]
        if name == "ttest_df_welch":
            # Two variances and two counts, for a caller who has summary statistics rather
            # than samples --- which is what an aggregate query leaves them with.
            return [4.0, 12.0, 9.0, 15.0]
        if name.startswith("chisq_test"):
            # Observed and expected counts, both strictly positive: an expected count of zero
            # is refused, and rightly.
            observed = [abs(seeded.signed(5.0)) + 5.0 for _ in range(6)]
            expected = [10.0] * 6
            return [observed, expected]
        if name.startswith("ttest_paired"):
            left = _series(seeded, 10)
            # Paired, and genuinely different, so the statistic is not zero.
            right = [value - 0.7 - seeded.signed(0.1) for value in left]
            return [left, right]
        if name.startswith(("regress_", "vec_regression_")):
            x = [float(i + 1) for i in range(12)]
            y = [3.0 + 2.0 * value + seeded.signed(0.4) for value in x]
            return [x, y]
        if name.startswith(("f_test", "ttest_2samp")):
            return [_series(seeded, 10), [value + 3.0 for value in _series(seeded, 10)]]
        if name == "vec_quantile":
            # The second argument is a probability, not a series. Given two ordinary series,
            # every path refused identically --- which is agreement, and told the soak nothing
            # about the answers.
            return [_series(seeded, 9), [0.25 + seeded.unit() * 0.5]]
        if arity == 2:
            return [_series(seeded, 8), _series(seeded, 8)]
        return None

    if takes == "numbers":
        if name == "ttest_df_welch":
            # Two variances and two counts. Handled here rather than with the two-sample tests
            # because the catalogue says it takes *numbers*: a caller who has summary
            # statistics rather than samples, which is what an aggregate query leaves them
            # with. Each count must be at least two, or the formula divides by zero.
            return [4.0, 12.0, 9.0, 15.0]
        values = []
        for at in range(arity):
            if at == 0 and name in _PROBABILITY_FIRST:
                values.append(0.25 + seeded.unit() * 0.5)
            elif at in _PROBABILITY_AT.get(name, []):
                values.append(0.3 + seeded.unit() * 0.4)
            elif at in _WHOLE_AT.get(name, []):
                # A count, and for the binomial the second must not be below the first.
                values.append(3.0 if at == 0 else 10.0)
            elif at in _FREEDOM_AT.get(name, []):
                values.append(float(5 + (seeded.next() % 20)))
            elif at in _POSITIVE_AT.get(name, []):
                values.append(0.5 + seeded.unit() * 3.0)
            else:
                values.append(seeded.signed(1.5))
        # `beta_i` and `beta_cdf` take an `x` on the unit interval, in a position the tables
        # above cannot express because it differs between the two.
        if name == "beta_i":
            values[2] = 0.2 + seeded.unit() * 0.6
        if name == "beta_cdf":
            values[0] = 0.2 + seeded.unit() * 0.6
        if name == "uniform_cdf":
            values = [seeded.unit() * 10.0, 0.0, 10.0]
        if name in ("gamma_p", "gamma_q"):
            values[1] = abs(values[1]) + 0.1
        if name == "gamma_cdf":
            values[0] = abs(values[0]) + 0.1
        if name in ("chisq_cdf", "chisq_sf"):
            values[0] = abs(values[0]) + 0.1
        if name in ("f_cdf", "f_sf"):
            values[0] = abs(values[0]) + 0.1
        if name == "expon_cdf":
            values[0] = abs(values[0]) + 0.1
        if name in ("lognorm_cdf",):
            values[0] = abs(values[0]) + 0.1
        if name == "gammaln":
            values[0] = abs(values[0]) + 0.5
        return values

    return None


def unreachable(catalogue) -> dict:
    """The functions this generator cannot call, and why.

    Returned so the soak can print its own coverage. A soak that covers a hundred of a hundred
    and twenty-eight and says PASS has made a claim about twenty-eight functions it never ran.
    """
    return {
        entry.name: CANNOT.get(entry.name, f"no rule for `{entry.takes}`")
        for entry in catalogue
        if for_function(entry, Seeded(1)) is None
    }
