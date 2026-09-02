"""The one place this binding has logic: the statements it builds.

Why these exist
---------------
`ADR-0017` Decision 1 says a binding may contain no logic the server does not enforce, and this
package holds to that --- except for the two functions that turn a method call into a statement.
Those are string building, they are not enforced anywhere, and **both of them were wrong**:

* `_cube_call` put the dimension where the measure goes, so `rollup(cube, 'region')` asked for
  cells that do not exist, and there was no way to say `by=` at all.
* `_graph_call` passed only the *values* of keyword options and discarded every name, so
  `max_depth=3` reached the server as a bare `3` in whatever position it fell --- a parameter
  that accepted anything and meant nothing.

Neither was caught by anything, because a wrong statement is still a statement: it compiles, it
sends, and the refusal comes back from the server looking like the user's mistake.

Run with `python3 -m unittest discover sdk/python/tests`, which is what the gate does.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from sankhya.client import _cube_call, _graph_call, _quote  # noqa: E402


class CubeStatements(unittest.TestCase):
    def test_the_measure_comes_before_the_dimension(self):
        # A cube holds many measures and a cell holds one measure's values, so the two are
        # different arguments and their order is the server's, not this binding's.
        self.assertEqual(
            _cube_call("cube_rollup", "sales", "amount", None, {}),
            "SELECT * FROM cube_rollup('sales', 'amount')",
        )
        self.assertEqual(
            _cube_call("cube_rollup", "sales", "amount", "region", {}),
            "SELECT * FROM cube_rollup('sales', 'amount', 'by=region')",
        )

    def test_an_option_keeps_its_name(self):
        # The whole defect in one assertion. `min_completeness=0.5` once reached the server as
        # `0.5`, which is not an option --- it is an argument in a position that means
        # something else.
        self.assertEqual(
            _cube_call("cube_rollup", "sales", "amount", "region", {"min_completeness": 0.5}),
            "SELECT * FROM cube_rollup('sales', 'amount', 'by=region', 'min_completeness=0.5')",
        )

    def test_options_keep_the_order_they_were_written_in(self):
        # Keyword order is meaningful to a reader comparing a call with its statement, and
        # Python preserves it. A builder that sorted them would make every diff noisy.
        built = _cube_call("cube_slice", "s", "amount", None, {"where": "region:north", "z": 1})
        self.assertEqual(
            built, "SELECT * FROM cube_slice('s', 'amount', 'where=region:north', 'z=1')"
        )


class GraphStatements(unittest.TestCase):
    def test_positional_arguments_stay_positional(self):
        # `graph_shortest_path(graph, from, to)` takes its destination by POSITION. Passed as
        # a keyword whose name was discarded, it worked only while it happened to be the first
        # keyword given, and any other option silently took its place.
        self.assertEqual(
            _graph_call("graph_shortest_path", "supply", ["acme", "zenith"], {}),
            "SELECT * FROM graph_shortest_path('supply', 'acme', 'zenith')",
        )

    def test_an_option_keeps_its_name(self):
        self.assertEqual(
            _graph_call("graph_reachable", "supply", ["acme"], {"max_depth": 3}),
            "SELECT * FROM graph_reachable('supply', 'acme', 'max_depth=3')",
        )

    def test_a_function_with_no_start_takes_none(self):
        self.assertEqual(
            _graph_call("graph_cycles", "supply", [], {}),
            "SELECT * FROM graph_cycles('supply')",
        )


class Quoting(unittest.TestCase):
    def test_a_quote_in_a_value_is_escaped(self):
        # The simple-query protocol has no parameter binding, so a literal is the only way to
        # pass a value. This is the one place this package handles one, and a name holding an
        # apostrophe must not end the literal.
        self.assertEqual(_quote("o'brien"), "o''brien")
        self.assertIn(
            "'o''brien'",
            _cube_call("cube_rollup", "o'brien", "amount", None, {}),
        )


if __name__ == "__main__":
    unittest.main()
