"""Every built-in, as a method, without a method written for each.

Why this is generated from the catalogue rather than hand-written
----------------------------------------------------------------
There are a hundred and twenty-eight of them. Hand-writing a stub each would be two thousand
lines whose only job is to agree with the server, and they would stop agreeing the first time a
function was added and the stub was not --- silently, because a missing method is not an error
until somebody calls it.

So the server's own ``functions()`` catalogue is the source of truth. ``db.fn`` asks for it once
per connection and offers exactly what came back. A function added to the server appears here
with no change to this file; a function removed disappears, rather than lingering as a method
that raises a planning error.

Why this is still a thin binding
--------------------------------
``ADR-0017`` Decision 1: no logic the server does not enforce. This validates nothing --- not
the argument count, not the types, not the domains. It builds a statement and passes back what
came out, and a wrong call produces the server's own refusal, which is the one that knows why.

What it *does* add is **encoding**: a Python list has to become a SQL array, and a Python float
has to keep its digits. That is not a rule about what is permitted; it is how a value crosses
the wire, which is exactly what a binding is for.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class Function:
    """One entry of the server's catalogue."""

    name: str
    category: str
    arity: int
    #: What the arguments are: ``numbers``, ``a series``, ``a matrix``, and so on.
    takes: str
    #: What comes back: ``a number`` or ``a series``.
    gives: str
    about: str

    @property
    def returns_series(self) -> bool:
        return self.gives == "a series"


def _literal(value: Any) -> str:
    """One argument, as SQL.

    A list becomes ``vec_of(...)``, which is how an array is written down here. A number keeps
    every digit it had --- ``repr`` rather than ``str``, because a float rendered to six places
    and sent to a kernel is a different number from the one the caller passed.
    """
    if isinstance(value, (list, tuple)):
        inner = ", ".join(_literal(item) for item in value)
        return f"vec_of({inner})"
    if isinstance(value, bool):
        # Before the numeric branch: `bool` is a subclass of `int` in Python, and `TRUE`
        # reaching a kernel as `1` would be a coercion this binding is not entitled to make.
        return "TRUE" if value else "FALSE"
    if isinstance(value, (int, float)):
        return repr(float(value))
    if value is None:
        return "NULL"
    if isinstance(value, str):
        escaped = value.replace("'", "''")
        return f"'{escaped}'"
    raise TypeError(
        f"a function argument must be a number, a list of numbers, a string or None, "
        f"and this is {type(value).__name__}"
    )


def _parse(text: str | None, series: bool):
    """One answer, back from its rendering.

    An array crosses the wire in PostgreSQL's own syntax --- ``{1,2.5,3}`` --- because
    ``ADR-0021`` Decision 3 sends it under the ``float8[]`` type every driver already decodes.
    Parsed here into a list, so a caller gets numbers rather than a string to pick apart.
    """
    if text is None or text == "":
        return None
    if not series:
        try:
            return float(text)
        except ValueError:
            return text
    inner = text.strip()
    if inner.startswith("{") and inner.endswith("}"):
        inner = inner[1:-1]
    if not inner:
        return []
    out = []
    for part in inner.split(","):
        part = part.strip()
        # A bare `NULL` is how PostgreSQL writes a null element, and it is not the string
        # "NULL" --- which is why the element is not quoted and why this comparison is exact.
        out.append(None if part == "NULL" else float(part))
    return out


class Catalogue:
    """The server's functions, callable as methods.

    Reached as ``db.fn``. Each attribute is one function from the server's own catalogue::

        db.fn.norm_inv(0.975)
        db.fn.vec_cosine_similarity([0.1, 0.9], [0.2, 0.8])
        db.fn.mat_cholesky([4, 12, -16, 12, 37, -43, -16, -43, 98])

    A name the server does not have raises :class:`AttributeError` naming the nearest matches,
    rather than sending a statement that will be refused --- because a typo in a function name
    is the binding's to catch and a wrong *argument* is the server's.
    """

    def __init__(self, client) -> None:
        self._client = client
        self._entries: dict | None = None

    def _catalogue(self) -> dict:
        """The catalogue, read once per connection.

        Once, because a server's function list does not change while a connection is open ---
        and asking per call would put a round trip in front of every arithmetic operation.
        """
        if self._entries is None:
            self._entries = {
                row["function"]: Function(
                    name=row["function"] or "",
                    category=row["category"] or "",
                    arity=int(row["arity"] or 0),
                    takes=row["takes"] or "",
                    gives=row["gives"] or "",
                    about=row["about"] or "",
                )
                for row in self._client.rows(
                    "SELECT function, category, arity, takes, gives, about FROM functions()"
                )
            }
        return self._entries

    def __getattr__(self, name: str):
        if name.startswith("_"):
            raise AttributeError(name)
        entries = self._catalogue()
        if name not in entries:
            near = [known for known in entries if name in known or known in name]
            hint = f" Did you mean {near[:4]}?" if near else ""
            raise AttributeError(
                f"this server has no function called `{name}`.{hint} "
                f"`db.functions()` lists all {len(entries)} of them"
            )
        entry = entries[name]

        def call(*arguments):
            # Deliberately **not** checked against `entry.arity` here. A binding that
            # validated would be a second definition of the signature, and the two would
            # drift; the server's refusal names the counts and is the one that knows.
            rendered = ", ".join(_literal(argument) for argument in arguments)
            result = self._client.sql(f"SELECT {name}({rendered})")
            if not result.rows or not result.rows[0]:
                return None
            return _parse(result.rows[0][0], entry.returns_series)

        call.__name__ = name
        call.__doc__ = f"{entry.about}.\n\nTakes {entry.takes}; gives {entry.gives}."
        return call

    def __dir__(self):
        return sorted(self._catalogue())

    def __len__(self) -> int:
        return len(self._catalogue())

    def __contains__(self, name: str) -> bool:
        return name in self._catalogue()
