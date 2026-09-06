# 20. The client contract and the SDKs

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> This chapter specifies what a SANKHYA client may assume and what it may never decide. Its central
> claim is one sentence: **a binding contains no logic the server does not also enforce**, and the
> test of it is that deleting the package changes nothing about what the system permits, refuses or
> audits. Three bindings are coming — Python, then Java and Rust — so the *contract* matters more
> than any of them. This chapter gives the contract, walks the Python binding with real output,
> shows the SQL peer that does everything the binding does, and names by measurement the three
> places where the shipped code does not yet meet the contract it is written against.

---

## 20.1 The rule, and the failure it prevents

The temptation with three SDKs is to write the good one first and port it. That produces three
clients that each decided for themselves what to validate, and the divergence surfaces as *"it worked
in Python"* — a sentence somebody then has to debug across two languages and a wire.

> **Key idea** — An SDK contains no logic the server does not also enforce. A client may *anticipate*
> a refusal to give a better message, and it may never *be* the refusal. If the Python binding
> rejects a cube whose measure declares no rule and the Java binding does not, then the rule lives in
> Python, the server is not enforcing it, and the second binding is a documented way around a
> correctness rule.

The check belongs at the choke point (Chapter 13, *Security, tenancy and policy*, §13.1) like every
other one, and a client's copy is a courtesy that must fail the same way or not exist. This is the
same argument Chapter 21, *Extensions and packs*, makes about the extension API: a capability that is
only enforced in one caller is not enforced.

The Python package states the rule in its own module docstring and then obeys it. Every method builds
a statement and hands back what came out; none validates a cube, decides whether a clone is allowed,
or invents a refusal. Where a method *appears* to know a rule — that a clone stays in its origin's
schema, say — it does not: the server refuses, and the binding passes the refusal on. The consequence
is that a wrong method call produces a confusing error and never a wrong answer.

**Thin is not the same as sparse.** A binding that omits half the server's capabilities forces its
users into raw SQL for the other half, and a user who has to drop to SQL for cloning will drop to SQL
for everything. So the surface is wide — discovery, querying, cloning and lineage, cubes and their
navigations, feeds and quarantine, the graph functions — while adding nothing to any of them.

## 20.2 Which door, and why not a third

Door | Binding uses it | Why
---|---|---
Arrow Flight SQL | **not yet** | Arrow-native end to end, streams by construction, carries a result's schema without a second description of it
PostgreSQL wire protocol | **yes** | The floor a client can always stand on: no dependencies at all
REST/JSON | **never** | A row-oriented JSON surface converts twice and loses the type distinctions storage spent effort preserving

The Python binding is **pure Python by decision, not by accident.** `pyarrow` is already what an Arrow
result *is* on the Python side and carries a Flight client; a Rust core behind `pyo3` would add a
per-platform wheel matrix, a build toolchain for anyone on an unlisted platform, and an ABI to keep in
step with three interpreter versions — all to accelerate a layer that must contain no logic.

> *If the client is thin, its language does not matter. If its language matters, it is not thin
> enough.*

The wire-protocol module is 251 lines and imports `socket` and `struct`. A laptop with a stock
interpreter and no build toolchain can connect.

## 20.3 The Python binding, walked

Two ways in, and the difference is deliberate: `sankhya.connect` gives you the raw wire connection,
`sankhya.open` gives you the capabilities as methods.

```python
import sankhya

with sankhya.open(host="127.0.0.1", port=55432, user="you", database="sankhya") as db:
    print(db.version())
    print([t.qualified for t in db.tables(schema="common")])
    for column in db.columns("common.orders"):
        print(" ", column.name, column.type_name, "null" if column.nullable else "not null")
    for row in db.rows("SELECT region, count(*) AS n FROM common.orders GROUP BY region ORDER BY region"):
        print(" ", row)
```

Run against a live server:

```
PostgreSQL 17.0 (SANKHYA 0.1.0) on wire-protocol-compatible unified engine
['common.empty', 'common.orders', 'common.regions']
  id int8 not null
  region text null
  period text null
  amount float8 not null
  note text null
  sank_data_date date not null
  {'region': 'north', 'n': '334'}
  {'region': 'south', 'n': '333'}
  {'region': None, 'n': '333'}
```

Values arrive as **strings**, and `None` is SQL `NULL`. The empty string and `NULL` are different
values and stay different — conflating them is a wrong answer, not a formatting choice.

The surface, by area:

Area | Methods
---|---
Raw | `connection`, `sql`, `scalar`, `one`, `rows`
Discovery | `version`, `settings`, `schemas`, `tables`, `columns`, `exists`
Cloning | `clone`, `drop`, `lineage_of`, `dependents_of`, `is_clone`
Cubes | `create_cube`, `cubes`, `cube_dimensions`, `cube_measures`, `rollup`, `slice`, `drop_cube`
Feeds | `feeds`, `resume_feed`, `quarantine`
Graph | `reachable`, `shortest_path(graph, from, to)`, `cycles`, `influence`, `time_respecting`

Two design choices in there are worth defending. **`connection` is deliberately public**: a binding
that hides the wire forces its author to anticipate every statement anybody will ever want, and the
ones they did not anticipate become impossible rather than merely unnamed. And **`create_cube` takes
the `CREATE CUBE` statement verbatim** rather than building it from Python objects, because a builder
would be a second definition of what a cube is — exactly the divergence the rule exists to prevent.

Verified against the same server, in a session where `probe_e.frozen` was a clone of
`probe_e.scratch` and `probe_e.audit` a clone of that:

```python
>>> db.lineage_of("probe_e.audit")
[Ancestor(step=1, origin='probe_e.frozen', origin_version=0, cloned_at=1788314179153747),
 Ancestor(step=2, origin='probe_e.scratch', origin_version=1, cloned_at=1788314179110932)]

>>> db.dependents_of("probe_e.scratch")
[Dependent(name='probe_e.audit', relation='indirect', reads_version=0),
 Dependent(name='probe_e.frozen', relation='direct', reads_version=1)]

>>> db.feeds()
[]
```

`Dependent.is_direct` is the one piece of interpretation in the package, and it is a rename rather
than a rule: an indirect reader breaks when the table *between* them goes, which is a different
problem with a different fix, and collapsing the two tells somebody the wrong thing about which table
to deal with.

Counts distinguish `None` from `0` throughout. *"Not recorded"* and *"recorded as none"* are different
answers, and collapsing them would report a clone as reading version 0 of its origin.

The cube navigations take the measure first and the dimension second, exactly as the server's own
functions do:

```python
db.rollup('sales', 'amount')                       # the grand total
db.rollup('sales', 'amount', by='region')          # a breakdown by region
db.slice('sales', 'amount', where='region:north')  # one member fixed
db.rollup('sales', 'amount', by='region', min_completeness=0.5)
```

> **How this was wrong, and how it was found** — both methods once took `(cube, by=…)` and passed
> `by` into the **measure** position, so `db.rollup('c', 'region')` was refused with *"cube 'c' has
> no published cells for measure 'region'"* and there was no way to say `by=` at all. Keyword
> options had their **names discarded**, so `opts='by=region'` worked and so did any other spelling
> — a parameter that accepted anything and meant nothing.
>
> Nothing caught it: the methods were covered, the gate was green, and the docstring described the
> opposite of what the code did. It was found by *writing a runnable example*, which is why those
> examples are now a test (`crates/sankhya-server/tests/sdk_examples.rs`). An example that does not
> run is documentation that lies, and it lies most convincingly right after the code has changed.
>
> The graph methods had the same defect and it had gone unnoticed for the same reason: keyword
> options reached the server as bare values with their names stripped, so `max_depth=3` arrived as
> `3` in whatever position it happened to fall, and `shortest_path`'s destination — which is a
> **positional** argument of the server's function — worked only while it happened to be the first
> keyword given. Both builders now have unit tests of their own, under `sdk/python/tests/`, run
> from the same gate. They are the one place this package has logic that nothing downstream
> enforces, which is exactly why both of them were wrong.

## 20.4 A refusal must cross the wire as data

This system spends real effort on refusals that say what to do — *"drop it first"*, *"materialise
them first"*, *"the archive is the copy and the way back is a rehydration"*. A binding that renders
those as a string has thrown away the half that matters. So the contract carries four things:

Field | Why it cannot be folded into the message
---|---
`code` | The stable identity a client dispatches on
`sqlstate` | What a generic driver on the other door understands
`remediation` | The half that says what to do
`subjects` | The **names** a refusal cites — clones, partitions, dimensions — as a list

`subjects` is the one that is easy to omit and expensive to add later. Without it, a client that wants
to show *"three clones read this table"* must parse the message, and the message becomes an API nobody
meant to publish and nobody may reword.

**What actually crosses today** is the PostgreSQL error envelope, and it carries two of the four:

```python
>>> try: db.sql("SELECT * FROM common.ordres")
... except sankhya.Refusal as e: e.fields
{'S': 'ERROR', 'V': 'ERROR', 'C': '42P01',
 'M': "[SNK-C0001] Error during planning: table 'datafusion.common.ordres' not found",
 'D': 'Correct the statement. The detail names the offending element.'}
```

`sqlstate` is a field. `remediation` arrives as `D` and the binding exposes it as `.detail`. **The
stable code is inside the message text**, so a client that wants to dispatch on `SNK-C0001` must
extract it with a regular expression, and `Refusal` has no `code` attribute. **`subjects` does not
cross at all.** And on the statement paths added in `M10` and `M13` — clone, lineage, feed — the
envelope carries neither a code nor a remediation:

```python
{'S': 'ERROR', 'V': 'ERROR', 'C': '22000',
 'M': '`probe_e.tmp_a` is still read by probe_e.tmp_b. Removing it is the deletion cloning is gated
       on, arriving through the front door --- materialise them first, or drop them'}
```

That message names the clone that would break. The name is in prose, so the only way a client can show
it is by parsing the sentence — which is precisely the outcome the contract forbids. It is stated here,
in the chapter that defines the contract, because a contract whose gaps are only in the server's
changelog is a contract nobody can plan against. Chapter 14, *Observability*, §14.5 has the server-side
view.

## 20.5 A result larger than the client

`MAX_RESULT_ROWS` bounds what a statement returns. A client asking for a hundred million rows must
**stream**, and a binding that collects before yielding converts a working query into an
out-of-memory kill on the user's laptop — with the server having done nothing wrong.

So the contract makes the streaming call the **primitive**, and every convenience — a dataframe, a
list of rows — a named opt-in on a result the caller has decided is small. Back-pressure belongs to
the transport: a slow consumer slows the scan rather than buffering it into the client.

**The Python binding does not do this yet, and says so.** `Connection.stream` yields one `Result` per
*statement answer* — the simple query protocol allows several, and a client returning only the first
would silently drop the rest — but it accumulates every row of a statement before yielding it. A
result larger than memory will not fit. This is named in `sdk/python/QUICKSTART.md` rather than
half-implemented, which is the right disposition: **a client that silently downgrades is worse than
one that says it cannot.**

## 20.6 A long operation is a commit, not a job handle

Materialising a cuboid, cloning a large table, ingesting a file and taking a backup can each outlast a
sensible timeout. The obvious design is a job registry: return a handle, poll it. **Refused, for now.**

A registry is a second durable state machine — entries that outlive their operation, needing their own
reclamation, their own authorization, and their own answer to *"what happens when the server restarts
mid-job?"* This system already has exactly one durable record of what happened, and it is the log.

So **every long operation's effect is a commit whose name the client already knows**, and a
disconnected client does not ask the server what it was doing; it asks the warehouse what is there:

Question | Answer
---|---
Did my clone happen? | The table exists, or it does not
Did the cuboid materialise? | It is present at *(definition version, snapshot, scope, cuboid)*, or the next query recomputes
Did the ingest land? | The table's version moved, or it did not

Two obligations are the price of not having a registry: **every long operation is idempotent under
re-issue**, or refuses naming the object it found — never a partial second attempt; and **no operation
leaves a state only the disconnected client could describe.**

This is why *"retry a non-idempotent operation"* is on the forbidden list below. A clone or an ingest
retried after a timeout is a second one.

## 20.7 What an SDK must never do

Collected because each is a plausible convenience that costs a property this system has spent
milestones establishing.

Never | Because
---|---
Cache an authorization decision | A grant revoked between two calls must take effect on the second
Validate what the server validates | The check moves into one binding and out of the other two
Retry a non-idempotent operation | A clone or an ingest retried after a timeout is a second one
Materialise a stream to make an API tidy | It turns a working query into a client-side kill
Reconstruct a refusal from its message text | The message becomes an API nobody meant to publish
Reach the filesystem the server uses | There is one write path, and a client is not it

The first is the one that looks most like good engineering. A session here is a permission's
*context*, never its cache: the connection carries a `Principal`, and every statement is authorized at
the choke point exactly as a local one is.

**Version skew** belongs on this list by implication. A client is installed independently of the server
and the two will differ; the contract carries a version, exchanged at connection, and a mismatch is
refused **there** — naming both versions — rather than surfacing eleven calls later when a field turns
out to be missing. Same reasoning as the artefact versioning that makes a newer on-disk format say
*"upgrade the binary"* rather than failing as a parse error somewhere in the middle. **This is not
implemented**: the Python startup exchange sends the protocol version and a user, and negotiates no
contract version at all.

## 20.8 The SQL peer

`sdk/sql/` is a peer of `sdk/python/`, not a lesser version of it. Because a binding contains no logic
the server does not enforce, **everything the binding can do, a SQL prompt can do.** The binding saves
typing; it does not unlock anything.

Exactly two things are properties of the client rather than the server:

| | Why
---|---
Streaming a result larger than memory | `psql` collects; a binding can iterate
Getting a refusal as structured fields | The wire carries them; `psql` renders them as text

Eight example files ship under `sdk/sql/examples/`, one per capability, and eight more under
`sdk/python/examples/`. Both sets are **gated artefacts**: they run in CI against a live server, from
`crates/sankhya-server/tests/sql_examples.rs` and `tests/sdk_examples.rs`. *An example that does not
run is documentation that lies, and it lies to the person least able to tell.*

The SQL gate reads each statement's outcome, not the script's. A statement preceded by a `-- REFUSES`
line must fail; every other statement must succeed. Both directions are checked, because a
demonstration of a refusal that quietly starts succeeding is a rule that has been removed and a
document that still claims it.

> **What ungated cost** — `07-analytics.sql` once called `vec`, `vec_add`, `vec_norm` and `mat`, none
> of which exist, and `04-cubes.sql` passed the dimension where the measure goes. Both had been
> reviewed; one carried a written note claiming it had been verified against a live server. A `psql`
> script with `ON_ERROR_STOP off` prints its errors and keeps going, so a wall of output reads as
> success. Only something that reads the exit of each statement can tell — which is what these gates
> now are, and what found the four front-door defects listed in `sdk/python/examples/README.md`.

## 20.9 What is not built, by name

Named rather than half-implemented, because a client that silently downgrades is worse than one that
says it cannot.

Not yet | What it costs you
---|---
**TLS** | The connection is in the clear. Fine on a loopback; **credential exposure off it** — and the server has offered TLS on both doors since `M14`'s gate
**Arrow / columnar results** | Large results come back as text rows, which is slower and larger
**Streaming** | `execute` collects; a result larger than memory will not fit
**Parameter binding** | The binding speaks the simple query protocol only, so values are interpolated as literals — the package has exactly one quoting helper and says it goes away when the extended protocol lands
**Structured refusal fields** | §20.4: no `code`, no `subjects`
**Version negotiation** | §20.7
**Ingest** | `M14`; streaming ingest is `M15`
**Java and Rust bindings** | `M16` — named now so the contract is written for three bindings rather than retrofitted to them
**Federated identity** | Chapter 13 §13.7. A `Principal` is a fixed tenant; which identity providers are supported, and how a token's claims map, waits for a deployment with an opinion

> **Key idea** — The gaps above are almost all *server-side work a client merely surfaces*: lineage,
> dependents, capability discovery and structured refusals are things the server must grow before a
> binding can expose them. That is what the rule in §20.1 forces, and it is why `M14` is not *"write a
> Python package"*.

One last property, and it is the one that makes the whole chapter checkable. **No client can be
honestly tested against a mock.** A binding whose tests mock the server tests its author's belief
about the server. The gate runs it against a real one, which makes the client's test suite slower and
worth having — and which is how the divergence in §20.8 becomes visible at all.
