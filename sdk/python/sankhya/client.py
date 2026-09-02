"""Everything a SANKHYA does, as methods.

Why these are thin
------------------
``ADR-0017`` Decision 1: **a binding may contain no logic the server does not enforce.** The
test is that deleting this package changes nothing about what the system permits, refuses or
audits. Anything failing that test is server work wearing a client's clothes.

So every method here builds a statement and hands back what came out. None validates a cube,
decides whether a clone is allowed, or invents a refusal. Where a method *appears* to know a
rule --- that a clone stays in its origin's schema, say --- it does not: the server refuses and
this passes the refusal on.

That is why a wrong method here produces a confusing error and never a wrong answer. The
alternative, a binding that checks first, gives the Python user one product and the Java user
another, and makes the second binding a documented way around a correctness rule.

Why the surface is wide anyway
------------------------------
Thin is not the same as sparse. A binding that omits half the server's capabilities forces its
users into raw SQL for the other half, and a user who has to drop to SQL for cloning will drop
to SQL for everything. So this covers the whole surface --- discovery, querying, cloning and
lineage, cubes and their navigations, feeds and quarantine, the graph functions, and the
analytical function catalogue --- while adding nothing to any of them.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Iterator

from .wire import Connection, Refusal, Result

# --- what the server sends back, named -------------------------------------


@dataclass(frozen=True)
class Table:
    """A table in the catalogue."""

    schema: str
    name: str

    @property
    def qualified(self) -> str:
        """``schema.name`` --- the form that keeps meaning one table.

        A bare name resolves only while one schema claims it. Anything written down, or run
        again next quarter, should use this.
        """
        return f"{self.schema}.{self.name}" if self.schema else self.name

    def __str__(self) -> str:
        return self.qualified


@dataclass(frozen=True)
class Column:
    """A column of a table."""

    name: str
    type_name: str
    nullable: bool


@dataclass(frozen=True)
class Ancestor:
    """One step of a clone's lineage."""

    #: 1 for what this was cloned from, 2 for what *that* was cloned from, and so on.
    step: int
    #: The qualified name of the ancestor.
    origin: str
    #: The origin version this step reads, or ``None`` if unrecorded.
    origin_version: int | None
    #: When the clone was made, microseconds from the epoch, or ``None``.
    cloned_at: int | None


@dataclass(frozen=True)
class Dependent:
    """Something that still reads a table."""

    #: The qualified name of the table that reads it.
    name: str
    #: ``"direct"`` if it is a clone of this table; ``"indirect"`` if it reads through another.
    relation: str
    #: The version it reads, or ``None``.
    reads_version: int | None

    @property
    def is_direct(self) -> bool:
        """Whether dropping *this* table is what breaks it.

        An indirect reader breaks when the table between them goes --- a different problem with
        a different fix, and collapsing the two tells somebody the wrong thing about which
        table to deal with.
        """
        return self.relation == "direct"


@dataclass(frozen=True)
class Cube:
    """A declared cube."""

    name: str
    fact_table: str
    definition_version: str
    dimensions: str
    measures: str
    hydrated_measures: str


@dataclass(frozen=True)
class SnapshotInfo:
    """A named instant, and what it holds."""

    name: str
    #: ``"live"`` or ``"expired"``.
    state: str
    #: Who took it. A snapshot holds storage on somebody's behalf, and a cost with no owner is
    #: one nobody reclaims.
    taken_by: str
    taken_at: int | None
    #: The day it stops being honoured, as days from the epoch.
    expires_on: int | None
    #: How many tables it pins.
    tables: int
    #: Their qualified names.
    pins: list

    @property
    def is_live(self) -> bool:
        return self.state == "live"


@dataclass(frozen=True)
class Change:
    """One commit in a table's log, as :meth:`Sankhya.history` reports it.

    A *summary*, not a diff. The log records file-level adds and removes: it knows a file
    arrived and another left, and it cannot know which rows differ.
    """

    version: int
    #: ``created``, ``appended``, ``removed``, ``compacted``, ``rewritten`` or ``metadata``.
    what: str
    #: When, in milliseconds from the epoch, or ``None`` for a commit that touched no file and
    #: so recorded no time. Reporting zero here would put the event in 1970 and present it as
    #: a fact --- so the two stay different, as they do everywhere else in this binding.
    at: int | None
    files_added: int
    files_removed: int
    bytes_added: int
    #: Whether the commit changed **data**, as the writer declared it --- not a guess from the
    #: file counts. ``False`` for a compaction, which rewrites files and changes not one row.
    changed_data: bool
    #: The snapshot or clone holding this version alive, or ``None`` for a version nothing is
    #: keeping. That ``None`` is the important part: retirement deletes the files a merge
    #: replaced, so the commit outlives its data.
    kept_by: str | None

    @property
    def is_readable(self) -> bool:
        """Whether something is keeping this version's files alive.

        A version with no keeper may still be readable --- nothing has swept it *yet* --- so
        this is a guarantee, not a prediction. Only ``True`` is worth relying on.
        """
        return self.kept_by is not None


@dataclass(frozen=True)
class Feed:
    """A declared feed, and what it is doing."""

    name: str
    #: ``"running"`` or ``"halted"``.
    state: str
    halted_since: int | None
    reason: str | None
    runs: int
    published: int
    quarantined: int
    #: Sources arriving behind the position's high-water mark, counted and not re-ingested.
    skipped: int
    #: How many times it has halted --- kept across a resume, because a feed that halted twice
    #: for the same reason is not in the situation a feed that halted once is in.
    halts: int

    @property
    def is_halted(self) -> bool:
        return self.state == "halted"


def _int(value: str | None) -> int | None:
    """A count the server sent, or ``None`` if it sent nothing.

    ``None`` and ``0`` are different answers --- *"not recorded"* against *"recorded as none"*
    --- and collapsing them would report a clone as reading version 0 of its origin.
    """
    if value is None or value == "":
        return None
    try:
        return int(value)
    except ValueError:
        return None


def _quote(value: str) -> str:
    """Escape a single-quoted SQL literal.

    Minimal on purpose. The simple-query protocol has no parameter binding, so a literal is the
    only way to pass a value, and this is the one place this package handles one. When the
    extended query protocol lands, values bind and this goes away.
    """
    return value.replace("'", "''")


class Sankhya:
    """A SANKHYA, with its capabilities as methods.

    Wraps a :class:`~sankhya.wire.Connection`. Make one with :func:`sankhya.open`.
    """

    def __init__(self, connection: Connection) -> None:
        # Read once, on first use. A server's function list does not change while a connection
        # is open, and asking per call would put a round trip in front of every arithmetic
        # operation.
        self._functions = None
        self._connection = connection

    # -- the raw door -------------------------------------------------------

    @property
    def connection(self) -> Connection:
        """The connection underneath, for statements this class does not name.

        Deliberately public. A binding that hides the wire forces its author to anticipate
        every statement anybody will ever want, and the ones they did not anticipate become
        impossible rather than merely unnamed.
        """
        return self._connection

    def sql(self, statement: str) -> Result:
        """Run any statement and return its rows. Everything below is this, with the statement
        built for you."""
        return self._connection.execute(statement)

    def scalar(self, statement: str):
        """The single value a statement returns, or ``None`` if it returned no rows."""
        result = self.sql(statement)
        if not result.rows or not result.rows[0]:
            return None
        return result.rows[0][0]

    def one(self, statement: str) -> list | None:
        """The single row a statement returns, or ``None``."""
        result = self.sql(statement)
        return result.rows[0] if result.rows else None

    def rows(self, statement: str) -> Iterator:
        """Iterate the rows a statement returns, as dictionaries keyed by column name."""
        result = self.sql(statement)
        for row in result.rows:
            yield dict(zip(result.columns, row))

    def close(self) -> None:
        self._connection.close()

    def __enter__(self) -> "Sankhya":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    # -- discovery ----------------------------------------------------------

    def version(self) -> str:
        """What the server says it is."""
        return self.scalar("SELECT version()") or ""

    def settings(self) -> dict:
        """What the server announced about itself when this connection opened."""
        return self._connection.parameters

    def schemas(self) -> list:
        """Every schema this connection may see."""
        seen = []
        for table in self.tables():
            if table.schema and table.schema not in seen:
                seen.append(table.schema)
        return seen

    def tables(self, schema: str | None = None) -> list:
        """Every table this connection may see, as :class:`Table`.

        Filtered by policy server-side: a catalogue listing tables the caller cannot read would
        disclose their existence, which is the leak the policy component refuses everywhere
        else, arriving through a schema browser.
        """
        statement = "SELECT table_schema, table_name FROM information_schema.tables"
        if schema is not None:
            statement += f" WHERE table_schema = '{_quote(schema)}'"
        return [
            Table(schema=row.get("table_schema") or "", name=row.get("table_name") or "")
            for row in self.rows(statement)
        ]

    def columns(self, table: str) -> list:
        """The columns of a table, as :class:`Column`."""
        # Qualified when it can be. `orders` may exist in several schemas, and asking about
        # the unqualified name would return every one of their columns interleaved --- which is
        # a wrong answer, not a wide one.
        schema, _, bare = table.rpartition(".")
        where = f"table_name = '{_quote(bare)}'"
        if schema:
            where = f"table_schema = '{_quote(schema)}' AND " + where
        # Read **by column name**, not by position. The catalogue answers with its own
        # projection rather than the one asked for, so a client reading `row[0]` gets whatever
        # happened to be first --- which is how this method first returned table names where
        # column names belong.
        return [
            Column(
                name=row.get("column_name") or "",
                type_name=row.get("data_type") or "",
                nullable=(row.get("is_nullable") or "").upper() == "YES",
            )
            for row in self.rows(
                "SELECT column_name, data_type, is_nullable FROM information_schema.columns "
                f"WHERE {where}"
            )
        ]

    def exists(self, table: str) -> bool:
        """Whether a table of this name is visible to this connection.

        ``False`` for a table that exists and may not be read, deliberately: the server makes
        the two indistinguishable, because saying *"you may not read that"* confirms it is
        there.
        """
        if "." in table:
            schema, name = table.split(".", 1)
            return any(t.schema == schema and t.name == name for t in self.tables())
        return any(t.name == table for t in self.tables())

    # -- zero-copy cloning --------------------------------------------------

    def clone(self, table: str, origin: str, at_version: int | None = None) -> None:
        """Create ``table`` as a zero-copy clone of ``origin``.

        A clone is a **reference** to its origin's files, not a copy
        ([ADR-0016]), so it costs the same whether the origin holds a thousand rows or a
        billion, and it adds no files of its own.

        ``at_version`` pins the origin version the clone reads. Omitted, it takes the origin as
        it stands **when the clone is made** --- resolved then, not when this was written.

        The clone lands in its origin's schema; naming another schema is refused. A clone is
        authorized through its origin, so one placed elsewhere would have its name governed by
        one policy and its data by another.

        Raises :class:`~sankhya.wire.Refusal` when the origin does not exist, is not readable,
        names more than one table, the new name is taken, or the pinned version's files are
        gone.
        """
        statement = f"CREATE TABLE {table} CLONE {origin}"
        if at_version is not None:
            statement += f" AT VERSION {at_version}"
        self.sql(statement)

    def drop(self, table: str, if_exists: bool = False) -> None:
        """Drop a clone.

        Refused if another clone still reads it, and the refusal names them. Ask
        :meth:`dependents_of` first: a refusal that names what would break is no use to
        somebody who had no way to ask beforehand.
        """
        self.sql("DROP TABLE " + ("IF EXISTS " if if_exists else "") + table)

    def lineage_of(self, table: str) -> list:
        """What ``table`` is a clone of, nearest first, as :class:`Ancestor`.

        Empty for a table that is not a clone --- which is an answer, and a different one from
        *"I could not tell you"*. The first entry answers *"what was this cloned from?"*; the
        last answers *"what is it ultimately a snapshot of?"*, which one step cannot.
        """
        return [
            Ancestor(
                step=_int(row[0]) or 0,
                origin=row[1] or "",
                origin_version=_int(row[2]),
                cloned_at=_int(row[3]),
            )
            for row in self.sql(f"SHOW LINEAGE OF {table}").rows
        ]

    def dependents_of(self, table: str) -> list:
        """What still reads ``table``, as :class:`Dependent`.

        Ask this **before** dropping anything. What it returns is what the drop refuses on,
        from the same records --- a list that disagreed with the refusal would be worse than no
        list, because somebody would act on it.
        """
        return [
            Dependent(
                name=row[0] or "",
                relation=row[1] or "",
                reads_version=_int(row[2]),
            )
            for row in self.sql(f"SHOW DEPENDENTS OF {table}").rows
        ]

    def is_clone(self, table: str) -> bool:
        """Whether ``table`` is a clone of something."""
        return bool(self.lineage_of(table))

    # -- cubes --------------------------------------------------------------

    def create_cube(self, definition: str) -> None:
        """Declare a cube from a ``CREATE CUBE`` statement, passed through verbatim.

        The cube language is the server's. A binding that built the statement from Python
        objects would be a second definition of what a cube is --- exactly the divergence
        Decision 1 exists to prevent.

        The shape::

            CREATE CUBE sales FROM orders
              DIMENSION region FROM regions ON region (LEVEL area = region)
              MEASURE amount (SUM ALONG region)

        A measure must say how it composes along each dimension. One that cannot be derived
        from its parts --- a ratio, a percentile --- says so, and the server refuses to roll it
        up rather than summing it into a wrong number nobody notices.
        """
        self.sql(definition)

    def cubes(self) -> list:
        """Every cube declared on this server, as :class:`Cube`.

        Empty on a warehouse with no cubes, rather than failing --- *"none yet"* and *"this
        server does not do cubes"* are opposite facts with opposite responses.
        """
        return [
            Cube(
                name=row[0] or "",
                fact_table=row[1] or "",
                definition_version=row[2] or "",
                dimensions=row[3] or "",
                measures=row[4] or "",
                hydrated_measures=row[5] or "",
            )
            for row in self.sql("SELECT * FROM cubes()").rows
        ]

    def cube_dimensions(self, cube: str) -> Result:
        """The dimensions of a cube, with their levels and member tables."""
        return self.sql(f"SELECT * FROM cube_dimensions('{_quote(cube)}')")

    def cube_measures(self, cube: str) -> Result:
        """The measures of a cube and how each composes along each dimension."""
        return self.sql(f"SELECT * FROM cube_measures('{_quote(cube)}')")

    def rollup(self, cube: str, measure: str, by: str | None = None, **options) -> Result:
        """Roll a cube up --- aggregate a dimension **away**.

        ``measure`` is which measure to report; ``by`` is the dimension to keep, and omitting
        it gives the grand total. The two are different arguments because a cube holds many
        measures and cells hold one measure's values each --- naming a dimension where the
        measure goes asks for cells that do not exist, which is what this binding did until
        the argument was named.

        ``**options`` are passed as the server spells them, ``key=value``, unaltered.

        The result carries `completeness` and `withheld` columns. **Read them.** A roll-up over
        a dimension with null members leaves those rows out, and those two columns are how you
        learn that a third of the value is missing from an otherwise plausible total.
        """
        return self.sql(_cube_call("cube_rollup", cube, measure, by, options))

    def slice(self, cube: str, measure: str, where: str, **options) -> Result:
        """Slice a cube --- fix one dimension's member and look at the rest.

        ``where`` is the member to slice to, spelled ``dimension:member`` as the server spells
        it --- ``"region:north"``. It is required, and deliberately: a slice with nothing fixed
        is a roll-up, and answering it as one would give the right number to the wrong question.
        """
        return self.sql(
            _cube_call("cube_slice", cube, measure, None, {"where": where, **options})
        )

    def drop_cube(self, name: str) -> None:
        """Remove a cube, and the cuboids materialised for it."""
        self.sql(f"DROP CUBE {name}")

    # -- snapshots ----------------------------------------------------------

    def take_snapshot(self, name: str, expire_after_days: int) -> None:
        """Take a named snapshot of every table you may read.

        A snapshot is a **position**, not a table: a clone freezes a *thing*, a snapshot freezes
        a *moment*. It records the version each table stood at and pins those files, so a run
        that reads four tables reads them as of one instant --- otherwise the reconciliation
        problem this system exists to remove reappears inside a single query.

        ``expire_after_days`` is **required** and there is no unbounded form. A snapshot pins
        files, so one that never expired would hold a whole warehouse's versions alive and the
        cost would fall on somebody who did not ask for it.

        Raises :class:`~sankhya.wire.Refusal` when the name is taken, or when the lifetime is
        zero or longer than this server will hold storage for.
        """
        self.sql(f"CREATE SNAPSHOT {name} EXPIRE AFTER {int(expire_after_days)} DAYS")

    def snapshots(self) -> list:
        """Every snapshot this warehouse holds, as :class:`SnapshotInfo`."""
        return [
            SnapshotInfo(
                name=row.get("snapshot") or "",
                state=row.get("state") or "",
                taken_by=row.get("taken_by") or "",
                taken_at=_int(row.get("taken_at")),
                expires_on=_int(row.get("expires_on")),
                tables=_int(row.get("tables")) or 0,
                pins=(row.get("pins") or "").split(),
            )
            for row in self.rows("SHOW SNAPSHOTS")
        ]

    def drop_snapshot(self, name: str, if_exists: bool = False) -> None:
        """Remove a snapshot, releasing the files it pinned."""
        self.sql("DROP SNAPSHOT " + ("IF EXISTS " if if_exists else "") + name)

    def read_as_of(self, name: str) -> None:
        """Read as of a named snapshot, for the rest of this connection.

        A **session** setting, because a run reads one instant across many statements. Another
        connection is unaffected.

        A table created *after* the snapshot is not there, and naming it fails to resolve
        exactly as a table that does not exist does. It is deliberately not answered as empty:
        a table that did not exist is not a table that was empty, and a join against one returns
        a confident zero.

        Refused **here** rather than at the next query when the snapshot does not exist or has
        expired.
        """
        self.sql(f"SET SNAPSHOT = '{_quote(name)}'")

    def read_the_present(self) -> None:
        """Stop reading as of a snapshot."""
        self.sql("RESET SNAPSHOT")

    # -- history and versions ------------------------------------------------

    def history(self, table: str) -> list:
        """Every commit a table's log holds, oldest first, as :class:`Change`.

        A snapshot is a *tag*; this is the log underneath it. Two fields carry the meaning:
        ``changed_data``, which is the writer's own declaration and is ``False`` for a
        compaction; and ``kept_by``, which is ``None`` for a version nothing is keeping alive.

        **History is readable only where something is keeping it alive.** Retirement deletes
        the files a merge replaced. The commit stays in the log forever; its data does not.

        This is not version control. There is no diff between two versions and no way to
        restore one, because the log records files rather than rows --- a compaction replaces
        every file and changes nothing, so a file-level diff would report a maintenance job as
        a total rewrite.
        """
        return [
            Change(
                version=_int(row.get("version")) or 0,
                what=row.get("what") or "",
                at=_int(row.get("at")),
                files_added=_int(row.get("files_added")) or 0,
                files_removed=_int(row.get("files_removed")) or 0,
                bytes_added=_int(row.get("bytes_added")) or 0,
                changed_data=(row.get("changed_data") or "") == "yes",
                kept_by=row.get("kept_by") or None,
            )
            for row in self.rows(f"SHOW HISTORY OF {table}")
        ]

    def read_version(self, table: str, version: int) -> None:
        """Read one table at one version, for the rest of this connection.

        Per table and per session, and independent of :meth:`read_as_of` --- this answers
        *"what did this table look like then"*, where a snapshot answers *"what did everything
        look like then"*. Use a snapshot when more than one table has to agree.

        Raises :class:`~sankhya.wire.Refusal` **here** rather than at the next query:

        * ``42704`` when the table has no such version. The refusal names the newest it does
          have. Replaying a log stops at its end, so this once handed back the newest version
          --- one nobody has, served as though they had it.
        * ``42704`` when the version is in the log and its data is not, because retirement
          took the files. Answering would return whichever rows happened to survive: a
          historical query silently missing whatever was compacted.
        * ``42P01`` when there is no such table.
        """
        self.sql(f"SET VERSION OF {table} = {int(version)}")

    def read_the_present_of(self, table: str) -> None:
        """Stop reading one table at a version."""
        self.sql(f"RESET VERSION OF {table}")

    # -- the built-in function catalogue -------------------------------------

    @property
    def fn(self):
        """Every built-in this server offers, as a method.

        ::

            db.fn.norm_inv(0.975)
            db.fn.vec_cosine_similarity([0.1, 0.9], [0.2, 0.8])
            db.fn.mat_cholesky([4, 12, -16, 12, 37, -43, -16, -43, 98])

        Driven by the server's own ``functions()`` catalogue rather than by a stub per
        function. A hundred and twenty-eight hand-written stubs would be two thousand lines
        whose only job is to agree with the server, and they would stop agreeing the first
        time one was added and a stub was not --- silently, because a missing method is not an
        error until somebody calls it.
        """
        if self._functions is None:
            from .functions import Catalogue

            self._functions = Catalogue(self)
        return self._functions

    def functions(self, category: str | None = None) -> list:
        """Every function this server offers, as :class:`~sankhya.functions.Function`.

        ``category`` narrows it: ``"distribution"``, ``"linear algebra"``, ``"inference"``,
        ``"regression"``, ``"vector"``, ``"calculus"``, ``"statistics"``, ``"graph"``,
        ``"cube"``, ``"special"``.

        This exists for the reason ``cubes()`` does: **a capability nobody can enumerate is a
        reference manual nobody reads**, and a client that cannot list the catalogue cannot
        offer it in a picker.
        """
        from .functions import Function

        where = ""
        if category is not None:
            where = f" WHERE category = '{_quote(category)}'"
        return [
            Function(
                name=row["function"] or "",
                category=row["category"] or "",
                arity=int(row["arity"] or 0),
                takes=row["takes"] or "",
                gives=row["gives"] or "",
                about=row["about"] or "",
            )
            for row in self.rows(
                "SELECT function, category, arity, takes, gives, about "
                f"FROM functions(){where} ORDER BY category, function"
            )
        ]

    # -- feeds and quarantine -----------------------------------------------

    def feeds(self) -> list:
        """Every declared feed and what it is doing, as :class:`Feed`.

        A feed that has never managed to run is listed too --- the case an operator most needs
        to see, and the one a *"list of running feeds"* would omit.
        """
        return [
            Feed(
                name=row[0] or "",
                state=row[1] or "",
                halted_since=_int(row[2]),
                reason=row[3],
                runs=_int(row[4]) or 0,
                published=_int(row[5]) or 0,
                quarantined=_int(row[6]) or 0,
                skipped=_int(row[7]) or 0,
                halts=_int(row[8]) or 0,
            )
            for row in self.sql("SHOW FEEDS").rows
        ]

    def resume_feed(self, name: str) -> None:
        """Set a halted feed running again, from the next tick.

        A statement rather than a restart: restarting the server to resume one feed takes an
        outage on every other feed and every open connection. Resuming does not forget --- the
        halt count survives it.
        """
        self.sql(f"RESUME FEED {name}")

    def quarantine(self, feed: str | None = None, limit: int = 100) -> Result:
        """Records a feed refused, whole, exactly as they arrived.

        A record that does not fit is quarantined rather than coerced or dropped
        ([ADR-0018]), with the reason, a stable code, the position it arrived at, and a
        fingerprint of the declaration that refused it. Whole, because a record reduced to an
        error message cannot be replayed, and replay is the only actual remedy.
        """
        statement = (
            "SELECT feed, source, position, reason_code, reason, payload FROM sank_quarantine"
        )
        if feed is not None:
            statement += f" WHERE feed = '{_quote(feed)}'"
        return self.sql(statement + f" LIMIT {int(limit)}")

    # -- the temporal graph -------------------------------------------------

    def reachable(self, graph: str, start: str, **options) -> Result:
        """What is reachable from a node.

        ``**options`` are the server's own, as ``key=value``: ``max_depth``, ``max_results``,
        and the edge mask.
        """
        return self.sql(_graph_call("graph_reachable", graph, [start], options))

    def shortest_path(self, graph: str, start: str, to: str, **options) -> Result:
        """The cheapest route from one node to another.

        ``to`` is a **positional** argument of the server's function, not an option --- it was
        once passed as a keyword whose name was discarded, so it worked only while it happened
        to be the first keyword given and any other option silently took its place.
        """
        return self.sql(_graph_call("graph_shortest_path", graph, [start, to], options))

    def cycles(self, graph: str, **options) -> Result:
        """Cycles in a graph."""
        return self.sql(_graph_call("graph_cycles", graph, [], options))

    def influence(self, graph: str, start: str, **options) -> Result:
        """Influence from a node."""
        return self.sql(_graph_call("graph_influence", graph, [start], options))

    def time_respecting(self, graph: str, start: str, **options) -> Result:
        """Paths that respect the order edges appeared in.

        The property a plain reachability query cannot express: a route through a graph is only
        a route if its edges existed in the order you traverse them.
        """
        return self.sql(_graph_call("graph_time_respecting", graph, [start], options))


def _cube_call(
    function: str, cube: str, measure: str, by: str | None, options: dict
) -> str:
    """Build a cube navigation call: ``function(cube, measure, 'by=…', 'key=value'…)``.

    Named arguments are passed as the server spells them, unaltered --- including ``by``,
    which is one of them. A binding that translated them would be inventing a second name for
    each, and the two would drift.
    """
    arguments = [f"'{_quote(cube)}'", f"'{_quote(measure)}'"]
    if by is not None:
        arguments.append(f"'by={_quote(by)}'")
    for key, value in options.items():
        arguments.append(f"'{_quote(key)}={_quote(str(value))}'")
    return f"SELECT * FROM {function}({', '.join(arguments)})"


def _graph_call(
    function: str, graph: str, positional: list, options: dict
) -> str:
    """Build a graph call: ``function(graph, <positional…>, 'key=value'…)``.

    Options are `key=value` strings, which is how the server reads them --- an earlier version
    passed only the *values* and discarded every name, so `max_depth=3` reached the server as a
    bare `3` in whatever position it happened to fall. A parameter that accepted anything and
    meant nothing.
    """
    arguments = [f"'{_quote(graph)}'"]
    arguments.extend(f"'{_quote(str(value))}'" for value in positional if value is not None)
    for key, value in options.items():
        arguments.append(f"'{_quote(key)}={_quote(str(value))}'")
    return f"SELECT * FROM {function}({', '.join(arguments)})"


def open(  # noqa: A001 -- `open` is the natural verb, and this is a module-qualified name.
    host: str = "127.0.0.1",
    port: int = 5432,
    user: str = "sankhya",
    database: str = "sankhya",
    password: str | None = None,
    timeout: float = 30.0,
) -> Sankhya:
    """Open a SANKHYA, with its capabilities as methods.

    ``sankhya.connect`` gives you the raw wire connection instead; this is the one to use.
    """
    return Sankhya(
        Connection(
            host=host,
            port=port,
            user=user,
            database=database,
            password=password,
            timeout=timeout,
        )
    )


__all__ = [
    "Ancestor",
    "Column",
    "Cube",
    "Dependent",
    "Feed",
    "Refusal",
    "Sankhya",
    "SnapshotInfo",
    "Table",
    "open",
]
