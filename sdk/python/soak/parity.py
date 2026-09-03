"""Every function, through every path, compared bit for bit.

The property this exists for
----------------------------
**A user does not care whether an answer came from OLTP, OLAP or the SDK, and the experience
must not depend on it.** That is the requirement, and it is not what a unit test checks: a unit
test asks whether one path is right, and three paths can each be right about a different thing.

So this asks the only question that captures it — *do the paths agree?* — and asks it of every
function in the server's own catalogue, with arguments generated from what the catalogue says
each one takes. A disagreement is a defect wherever it is, and the paths are exactly where
encoding defects live: a float rendered to six places on the way out, an array parsed with the
wrong separator, a null that became a zero.

The paths
---------
==========================  ==================================================================
``wire``                    A statement over the PostgreSQL wire protocol, as ``psql`` sends it
``sdk_sql``                 The same statement through the binding's ``sql()``
``sdk_method``              ``db.fn.<name>(...)`` — the binding's own encoding and decoding
``oltp``                    Reported as unavailable; see below
==========================  ==================================================================

The transactional path is **reported, never skipped silently**. Nothing routes to PostgreSQL
yet, so the honest result is a named gap in the run rather than a pass over three paths and a
shrug about the fourth.

What counts as agreement
------------------------
Bit-for-bit, after each path's own decoding. Not "close enough": a difference of ``1e-16``
between two paths is the same difference that makes a figure fail to tie out, and it is exactly
what a rendering that loses a digit produces. Where the paths return different *types* — a
string from the wire, a float from the binding — the comparison is made on the value the binding
would give a user, because that is the answer they act on.
"""

from __future__ import annotations

import os
import sys
import time
import traceback
from dataclasses import dataclass, field

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import sankhya  # noqa: E402
from soak.arguments import Seeded, for_function, unreachable  # noqa: E402
from soak import columns  # noqa: E402
from sankhya.functions import _literal, _parse  # noqa: E402


@dataclass
class Disagreement:
    """Two paths that answered differently."""

    function: str
    arguments: str
    left: str
    left_value: str
    right: str
    right_value: str

    def __str__(self) -> str:
        return (
            f"{self.function}({self.arguments}): "
            f"{self.left} gave {self.left_value!r}, {self.right} gave {self.right_value!r}"
        )


@dataclass
class Findings:
    """What one pass found."""

    called: int = 0
    compared: int = 0
    refused_consistently: int = 0
    #: Function name to the refusal every path gave, so a generator that gets a domain wrong
    #: is visible rather than counted as agreement.
    refusals: dict = field(default_factory=dict)
    disagreements: list = field(default_factory=list)
    #: Function to the first answer seen, for the drift check across passes.
    first_answer: dict = field(default_factory=dict)
    drifted: list = field(default_factory=list)
    #: Functions where one path refused and another answered.
    inconsistent_refusals: list = field(default_factory=list)
    #: Column-based probes run, and those whose answer differed from the literal one.
    over_columns: int = 0
    column_mismatches: list = field(default_factory=list)
    column_refusals: dict = field(default_factory=dict)


def _statement(name: str, arguments: list) -> str:
    """The statement all three paths run, so a difference is not a difference of statement."""
    return f"SELECT {name}({', '.join(_literal(value) for value in arguments)})"


def _normalise(value, series: bool):
    """One answer in the form a user would act on.

    The wire returns text; the binding returns a float or a list. Compared as the binding's
    form, because that is what a caller receives --- and a soak that compared the *renderings*
    would pass while the binding's parsing was wrong, which is the defect most likely to exist.
    """
    if value is None:
        return None
    if isinstance(value, str):
        return _parse(value, series)
    return value


def _same(left, right) -> bool:
    """Whether two answers are the same, bit for bit.

    NaN is equal to NaN here. A function that legitimately returns NaN --- a correlation of a
    constant series --- must do so on every path, and ``NaN != NaN`` would report that
    agreement as a disagreement on every single pass.
    """
    if left is None or right is None:
        return left is None and right is None
    if isinstance(left, list) != isinstance(right, list):
        return False
    if isinstance(left, list):
        if len(left) != len(right):
            return False
        return all(_same(a, b) for a, b in zip(left, right))
    if isinstance(left, float) and isinstance(right, float):
        if left != left and right != right:
            return True
        return left == right
    return left == right


def _ask(kind, db, entry, arguments, statement):
    """One path's answer, or the refusal it gave.

    Returns ``("value", answer)`` or ``("refused", message)``, so a refusal on one path and an
    answer on another is visible as a disagreement rather than as an exception that ends the
    run.
    """
    try:
        if kind == "wire":
            result = db.connection.execute(statement)
            raw = result.rows[0][0] if result.rows and result.rows[0] else None
            return ("value", _normalise(raw, entry.returns_series))
        if kind == "sdk_sql":
            result = db.sql(statement)
            raw = result.rows[0][0] if result.rows and result.rows[0] else None
            return ("value", _normalise(raw, entry.returns_series))
        if kind == "sdk_method":
            return ("value", getattr(db.fn, entry.name)(*arguments))
        raise ValueError(kind)
    except sankhya.Refusal as refused:
        return ("refused", refused.message)


#: The paths that exist today. `oltp` is absent by fact, and reported as such.
PATHS = ("wire", "sdk_sql", "sdk_method")


def one_pass(db, catalogue, seeded: Seeded, findings: Findings, fixed_seed: int) -> None:
    """Call every function on every path, and compare.

    Twice per function, and the two probes answer different questions.

    **Varying** arguments give breadth: a new draw each pass, so over an hour a function meets
    thousands of inputs rather than the one its author thought of.

    **Fixed** arguments give the drift check, and it needs them. Comparing a varying call
    against its own history compares nothing, because the arguments differ --- which is what
    the first version of this did, so the drift check was a check that could not fire. The
    fixed probe uses the same seed on every pass, so the same statement over the same values
    must return the same bits an hour later.
    """
    for entry in catalogue:
        # The drift probe first, from a seed that does not move.
        steady = for_function(entry, Seeded(fixed_seed))
        if steady is not None:
            _probe(db, entry, steady, findings, drift=True)

        arguments = for_function(entry, seeded)
        if arguments is None:
            continue

        _probe(db, entry, arguments, findings, drift=False)


def _probe(db, entry, arguments, findings: Findings, drift: bool) -> None:
    """One function, one set of arguments, every path."""
    statement = _statement(entry.name, arguments)
    rendered = ", ".join(repr(a) for a in arguments)
    answers = {path: _ask(path, db, entry, arguments, statement) for path in PATHS}
    findings.called += 1

    kinds = {kind for kind, _ in answers.values()}
    if kinds == {"refused"}:
        # Every path refused. That is agreement, and for a domain the generator got wrong it is
        # the right outcome --- so it is counted and named rather than treated as a failure.
        findings.refused_consistently += 1
        findings.refusals.setdefault(entry.name, str(answers[PATHS[0]][1])[:100])
        return
    if len(kinds) > 1:
        # One path answered and another refused. Always a defect: a refusal is a rule, and a
        # rule that one path enforces and another does not is not a rule.
        detail = {path: f"{kind}: {value}" for path, (kind, value) in answers.items()}
        findings.inconsistent_refusals.append(f"{entry.name}({rendered}) -> {detail}")
        return

    reference_path = PATHS[0]
    reference = answers[reference_path][1]
    for path in PATHS[1:]:
        findings.compared += 1
        if not _same(reference, answers[path][1]):
            findings.disagreements.append(
                Disagreement(
                    function=entry.name,
                    arguments=rendered,
                    left=reference_path,
                    left_value=repr(reference),
                    right=path,
                    right_value=repr(answers[path][1]),
                )
            )

    # And the same answer on every pass --- but only for the **fixed** probe, whose arguments
    # do not move. Comparing a varying call against its own history compares nothing, which is
    # what the first version did, so the drift check was a check that could not fire.
    if not drift:
        return
    seen = findings.first_answer.get(entry.name)
    if seen is None:
        findings.first_answer[entry.name] = (rendered, repr(reference))
    elif seen[1] != repr(reference):
        findings.drifted.append(
            f"{entry.name}({rendered}): was {seen[1]}, now {repr(reference)}"
        )


def over_columns(db, catalogue, findings: Findings) -> None:
    """Call every function that can be written against a column, and check it against the
    literal route.

    The comparison is the point: `f(literal built from row N)` must equal `f(column)` at row N.
    Same values, same function, two entirely different routes into the kernel --- a scalar
    broadcast against an Arrow array read by stride. A disagreement is a marshalling defect,
    which is where one would be.
    """
    table, key = columns.TABLE, columns.KEY
    if not db.exists(table):
        print(f"   SKIPPED  `{table}` is not on this warehouse, so nothing was read from a column")
        return

    # One row's values, to build the literals from. The first by key, so the run is repeatable.
    first = None
    reads = ", ".join(columns.READS)
    for row in db.rows(f"SELECT {reads} FROM {table} ORDER BY {key} LIMIT 1"):
        first = row
    if first is None:
        print(f"   SKIPPED  `{table}` is empty")
        return
    values = {}
    for read in columns.READS:
        raw = first[read]
        values[read] = (
            [float(v) for v in str(raw).strip("{}").split(",") if v]
            if read in columns.VECTORS
            else float(raw)
        )
    identifier = int(values[key])

    for entry, over_column, over_literal in columns.probes(catalogue, values):
        findings.over_columns += 1
        # Over the columns, restricted to the row the literals came from.
        column_sql = f"SELECT {over_column} AS result FROM {table} WHERE {key} = {identifier}"
        literal_sql = f"SELECT {over_literal} AS result"

        outcomes = {}
        for label, sql in (("column", column_sql), ("literal", literal_sql)):
            try:
                result = db.sql(sql)
                raw = result.rows[0][0] if result.rows and result.rows[0] else None
                outcomes[label] = ("value", _normalise(raw, entry.returns_series))
            except sankhya.Refusal as refused:
                outcomes[label] = ("refused", refused.message)

        kinds = {kind for kind, _ in outcomes.values()}
        if kinds == {"refused"}:
            findings.column_refusals.setdefault(
                entry.name, str(outcomes["column"][1])[:100]
            )
            continue
        if len(kinds) > 1:
            findings.column_mismatches.append(
                f"{entry.name}: one route answered and the other refused --- {outcomes}"
            )
            continue
        if not _same(outcomes["column"][1], outcomes["literal"][1]):
            findings.column_mismatches.append(
                f"{entry.name}: over the column gave {outcomes['column'][1]!r}, "
                f"over a literal of the same values gave {outcomes['literal'][1]!r}"
            )


def run(minutes: float, seed: int, port: int, user: str) -> int:
    """Run for a duration and report. Returns a process exit code."""
    deadline = time.monotonic() + minutes * 60.0
    findings = Findings()
    passes = 0

    with sankhya.open(port=port, user=user) as db:
        catalogue = db.functions()
        print(f"catalogue: {len(catalogue)} function(s)")

        cannot = unreachable(catalogue)
        covered = len(catalogue) - len(cannot)
        print(f"coverage : {covered} of {len(catalogue)} callable by this generator")
        if cannot:
            print("not called, by name:")
            for name, why in sorted(cannot.items()):
                print(f"  {name:24} {why}")

        # The column half runs once: it compares two routes to the same answer, and repeating
        # it measures nothing the literal passes below do not already measure.
        print("\n== over a stored column ==")
        over_columns(db, catalogue, findings)
        print(f"   {findings.over_columns} function(s) called on a column and checked against "
              f"a literal of the same values")
        if findings.column_refusals:
            print(f"   {len(findings.column_refusals)} refused on both routes:")
            for name, why in sorted(findings.column_refusals.items()):
                print(f"     {name:26} {why}")

        seeded = Seeded(seed)
        while time.monotonic() < deadline:
            one_pass(db, catalogue, seeded, findings, fixed_seed=seed ^ 0x9e37)
            passes += 1
            if passes == 1:
                print(f"\nfirst pass: {findings.called} call(s), {findings.compared} comparison(s)")

    print(f"\n== {passes} pass(es) ==")
    print(f"   {findings.called} call(s) across {len(PATHS)} path(s)")
    print(f"   {findings.compared} comparison(s) between paths")
    print(f"   {findings.refused_consistently} call(s) refused by every path, which is agreement")
    print(
        f"   {len(findings.first_answer)} function(s) held to a fixed argument set across "
        f"{passes} pass(es), which is the drift check"
    )
    if findings.refusals:
        # Agreement, and still worth naming: a function refused on every pass is one whose
        # *answers* were never compared, so the coverage number above overstates what was
        # tested. A generator that gets a domain wrong hides here.
        print(f"\n   {len(findings.refusals)} function(s) refused on every path:")
        for name, why in sorted(findings.refusals.items()):
            print(f"     {name:26} {why}")

    print("\n== the transactional path ==")
    print(
        "   UNAVAILABLE  nothing routes to PostgreSQL yet, so the transactional path was not\n"
        "                compared. `sankhya-oltp-pg` supervises a cluster and nothing wires it\n"
        "                into the server (M8 §12.2). Reported rather than skipped: a run that\n"
        "                covers three of four paths and prints PASS is a claim nobody checked."
    )

    failed = False
    if findings.disagreements:
        failed = True
        print(f"\n== {len(findings.disagreements)} DISAGREEMENT(S) ==")
        for item in findings.disagreements[:40]:
            print(f"   {item}")
    if findings.inconsistent_refusals:
        failed = True
        print(f"\n== {len(findings.inconsistent_refusals)} INCONSISTENT REFUSAL(S) ==")
        for item in findings.inconsistent_refusals[:40]:
            print(f"   {item}")
    if findings.column_mismatches:
        failed = True
        print(f"\n== {len(findings.column_mismatches)} COLUMN MISMATCH(ES) ==")
        for item in findings.column_mismatches[:40]:
            print(f"   {item}")
    if findings.drifted:
        failed = True
        print(f"\n== {len(findings.drifted)} DRIFT(S) ==")
        for item in findings.drifted[:40]:
            print(f"   {item}")

    if not failed:
        print(
            "\n   every path agreed on every call, on every pass, and every function called "
            "over a column\n   agreed with the same function called over a literal of the same "
            "values"
        )
    return 1 if failed else 0


def main() -> int:
    minutes = float(os.environ.get("SANKHYA_PARITY_MINUTES", "0.5"))
    seed = int(os.environ.get("SANKHYA_PARITY_SEED", "20260902"))
    port = int(os.environ.get("SANKHYA_PORT", "5432"))
    user = os.environ.get("SANKHYA_USER", "quickstart")
    try:
        return run(minutes, seed, port, user)
    except Exception:  # noqa: BLE001 -- a soak reports rather than raises
        traceback.print_exc()
        return 2


if __name__ == "__main__":
    sys.exit(main())
