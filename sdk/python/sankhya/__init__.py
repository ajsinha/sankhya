"""The SANKHYA Python binding.

``ADR-0017`` governs what may be in here: **no logic the server does not enforce**. The test is
that deleting this package changes nothing about what the system permits, refuses or audits.
Anything failing that test is server work wearing a client's clothes.

Two ways in:

    import sankhya

    with sankhya.open(port=5432, user="you") as db:   # the capabilities, as methods
        for row in db.rows("SELECT region, sum(amount) FROM sales.orders GROUP BY region"):
            print(row)

    with sankhya.connect(port=5432, user="you") as wire:   # the raw wire, for anything else
        print(wire.execute("SELECT 1").rows)
"""

from .client import (
    Ancestor,
    Column,
    Cube,
    Dependent,
    Feed,
    Sankhya,
    SnapshotInfo,
    Change,
    Table,
    open,
)
from .wire import Connection, Refusal, Result, WireError, connect

__all__ = [
    "Ancestor",
    "Column",
    "Connection",
    "Cube",
    "Dependent",
    "Feed",
    "Refusal",
    "Result",
    "Sankhya",
    "SnapshotInfo",
    "Change",
    "Table",
    "WireError",
    "connect",
    "open",
]
__version__ = "0.1.0"
