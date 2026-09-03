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
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from sankhya.functions import matrix  # noqa: E402


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
    "lognorm_inv": [2],
    "poisson_pmf": [1], "poisson_cdf": [1],
    "gamma_cdf": [0, 1, 2], "beta_cdf": [1, 2], "gamma_p": [0, 1], "gamma_q": [0, 1],
    "beta_i": [0, 1], "gammaln": [0],
    # Written here rather than corrected after the loop, where they were: a correction after
    # the fact is invisible to anything else that asks what a position takes, and the column
    # probes ask exactly that.
    "chisq_cdf": [0], "chisq_sf": [0], "f_cdf": [0], "f_sf": [0],
    "expon_cdf": [0, 1], "lognorm_cdf": [0, 2],
}

#: A probability at these positions, for the discrete families.
_PROBABILITY_AT = {"binom_pmf": [2], "binom_cdf": [2]}

#: Positions taking an `x` on the unit interval --- which is not a probability, though it has
#: the same range: it is where a cumulative beta is evaluated.
_UNIT_AT = {"beta_i": [2], "beta_cdf": [0]}

#: Positions whose value is pinned rather than drawn, because it cannot be chosen one position
#: at a time. A uniform distribution's bounds must bracket its argument, and Welch's formula
#: takes two variances and two counts in an order no per-position rule expresses.
_FIXED_AT = {
    "uniform_cdf": {0: 5.0, 1: 0.0, 2: 10.0},
    "ttest_df_welch": {0: 4.0, 1: 12.0, 2: 9.0, 3: 15.0},
}


def domain_at(name: str, at: int):
    """What kind of number belongs at one argument position, and a value of that kind.

    **The single place the soak decides what a probability is.** Both halves ask it --- the
    literal generator below and the column probes in ``columns.py`` --- because two tables of
    domains drift, and a drifted domain does not fail: it makes every path refuse identically,
    which the soak counts as agreement and reports as a pass.
    """
    if at in _FIXED_AT.get(name, {}):
        return ("fixed", _FIXED_AT[name][at])
    if (at == 0 and name in _PROBABILITY_FIRST) or at in _PROBABILITY_AT.get(name, []):
        return ("probability", 0.5)
    if at in _UNIT_AT.get(name, []):
        return ("unit", 0.5)
    if at in _WHOLE_AT.get(name, []):
        # A count, and for the binomial the second must not be below the first.
        return ("count", 3.0 if at == 0 else 10.0)
    if at in _FREEDOM_AT.get(name, []):
        return ("freedom", 9.0)
    if at in _POSITIVE_AT.get(name, []):
        return ("positive", 1.5)
    return ("real", 0.25)

#: Functions this generator cannot produce arguments for, with the reason.
#:
#: Named rather than skipped by falling through, so the soak can report its own coverage
#: honestly — see ``unreachable`` below.
CANNOT = {
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


def _prices(seeded: Seeded, length: int = 16) -> list:
    """A price path, strictly positive at every point.

    Positive because a logarithmic return needs both ends of every step to be above zero, and a
    drawdown measured against a negative peak is a number with no meaning. Given the generator's
    ordinary series, every one of the time-series functions refused --- on all three paths, so
    the soak counted it as agreement and compared no answers at all.
    """
    price = 100.0
    out = []
    for _ in range(length):
        price *= 1.0 + seeded.signed(0.05)
        out.append(price)
    return out


def _returns(seeded: Seeded, length: int = 24) -> list:
    """Period returns, some of them losses.

    Some of them losses on purpose: a Sortino ratio over a series that never fell is refused,
    and rightly --- there is no downside deviation to divide by.
    """
    return [seeded.signed(0.04) for _ in range(length)]


def _flows(seeded: Seeded, length: int = 8) -> list:
    """A cash flow with one outlay and several receipts, so an internal rate exists."""
    return [-(100.0 + seeded.unit() * 20.0)] + [
        20.0 + seeded.unit() * 10.0 for _ in range(length - 1)
    ]


def _option(seeded: Seeded) -> list:
    """A spot, a strike, a rate, a volatility and a time to expiry, each in its own domain."""
    return [
        100.0 + seeded.signed(10.0),
        95.0 + seeded.signed(10.0),
        0.01 + seeded.unit() * 0.04,
        0.1 + seeded.unit() * 0.4,
        0.25 + seeded.unit() * 2.0,
    ]


def _series(seeded: Seeded, length: int, positive: bool = False) -> list:
    """A series with genuine variation, because a constant series has no statistics."""
    out = []
    for i in range(length):
        value = 1.0 + i * 0.5 + seeded.signed(0.3)
        out.append(abs(value) + 0.1 if positive else value)
    return out


#: Functions whose arguments are a **shape** rather than a domain per position, written out.
#:
#: A per-position rule cannot say that a cash flow needs a sign change, that a matrix's right
#: hand side is as long as its order, or that a depreciation period lies inside the asset's
#: life. Guessing produced twenty-six functions that refused on every path --- which the soak
#: counts as agreement, and which means their *answers* were never compared once.
#:
#: The whole numbers here are `int`, and that matters: a window, a lag, a life and a matrix's
#: order are counts, and every function taking one refuses a `5.0` by name.
_SHAPED = {
    "irr": lambda s: [_flows(s)],
    "npv": lambda s: [0.05 + s.unit() * 0.1, _flows(s)],
    "npv_from_now": lambda s: [0.05 + s.unit() * 0.1, _flows(s)],
    "var_historical": lambda s: [_returns(s, 32), 0.01 + s.unit() * 0.1],
    "expected_shortfall": lambda s: [_returns(s, 32), 0.01 + s.unit() * 0.1],
    "sharpe": lambda s: [_returns(s), s.unit() * 0.01],
    "sortino": lambda s: [_returns(s), s.unit() * 0.01],
    "ts_returns": lambda s: [_prices(s)],
    "ts_log_returns": lambda s: [_prices(s)],
    "ts_drawdown": lambda s: [_prices(s)],
    "ts_max_drawdown": lambda s: [_prices(s)],
    "ts_cumulative_return": lambda s: [_returns(s, 16)],
    "ts_rolling_mean": lambda s: [_prices(s, 20), 5],
    "ts_rolling_std": lambda s: [_prices(s, 20), 5],
    "ts_rolling_min": lambda s: [_prices(s, 20), 5],
    "ts_rolling_max": lambda s: [_prices(s, 20), 5],
    "ts_ewma": lambda s: [_prices(s, 20), 0.1 + s.unit() * 0.8],
    "ts_autocorrelation": lambda s: [_returns(s, 32), 1],
    "pv": lambda s: [0.02 + s.unit() * 0.08, 10, -100.0 - s.unit() * 50.0, 0.0],
    "fv": lambda s: [0.02 + s.unit() * 0.08, 10, -100.0 - s.unit() * 50.0, 0.0],
    "pmt": lambda s: [0.02 + s.unit() * 0.08, 10, 1000.0 + s.unit() * 500.0, 0.0],
    "sln": lambda s: [10000.0 + s.unit() * 1000.0, 1000.0, 5],
    "syd": lambda s: [10000.0 + s.unit() * 1000.0, 1000.0, 10, 3],
    "black_scholes_call": _option,
    "black_scholes_put": _option,
    "greeks_delta": _option,
    "greeks_vega": _option,
    # The matrix family, which needs a *declared* shape and refuses a plain array by name:
    # six values are a 2x3 or a 3x2, and transposing the wrong one answers with numbers built
    # from values that were never in the same row.
    "mat_identity": lambda s: [2 + int(s.next() % 4)],
    # The values are arguments of `mat_of`, not one array argument: it counts them and checks
    # the count against the shape while it is still planning.
    "mat_of": lambda s: [2, 3, *_series(s, 6)],
    "mat_transpose": lambda s: [matrix(2, 3, _series(s, 6))],
    "mat_multiply": lambda s: [matrix(2, 3, _series(s, 6)), matrix(3, 2, _series(s, 6))],
    "mat_vec": lambda s: [matrix(3, 3, _positive_definite(s, 3)), _series(s, 3)],
    "mat_solve": lambda s: [matrix(3, 3, _positive_definite(s, 3)), _series(s, 3)],
}


def for_function(entry, seeded: Seeded):
    """Arguments for one function, or ``None`` if this generator cannot make them.

    ``entry`` is a :class:`sankhya.functions.Function` from the server's own catalogue, so a
    function added to the server is covered here without an edit.
    """
    name, arity, takes = entry.name, entry.arity, entry.takes
    if name in CANNOT:
        return None
    if name in _SHAPED:
        return _SHAPED[name](seeded)

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
        values = []
        for at in range(arity):
            kind, fixed = domain_at(name, at)
            if kind == "probability":
                values.append(0.25 + seeded.unit() * 0.5)
            elif kind == "unit":
                values.append(0.2 + seeded.unit() * 0.6)
            elif kind == "positive":
                values.append(0.5 + seeded.unit() * 3.0)
            elif kind == "freedom":
                values.append(float(5 + (seeded.next() % 20)))
            else:
                # A count, a pinned bound, or an ordinary real. The first two are what
                # `domain_at` says they are; only the last is drawn.
                values.append(fixed if kind in ("count", "fixed") else seeded.signed(1.5))
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
