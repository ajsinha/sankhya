<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — the tutorials

**Document ID:** SNK-TUT-001
**Version:** 0.1.0
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

Five tutorials, in order, about an hour in total. They assume a running server —
[`QUICKSTART.md`](QUICKSTART.md) gets you one, including the fixture warehouse every example
here is written against. [`GUIDE.md`](GUIDE.md) is the reference; this is the walk.

**Every SQL block below is executed by a test.**
`crates/sankhya-server/tests/book_sql.rs` walks every markdown file under `docs/` — this file,
not a copy of it — extracts the fenced `sql` blocks and runs each statement against a real
server, holding one shown as working to a single rule: it may fail only because the object it
names is absent. A function renamed, a clause that no longer parses, a refusal the system has
stopped making — all of those break the build.

**And one thing here is not yet true.** The stricter harness,
`crates/sankhya-server/tests/guide.rs`, runs its documents in order against one server and
insists that every block either succeeds, is marked `-- ERROR` and fails, or is listed with a
reason. Its list still names the four files under `docs/tutorials/` that this document
replaces (`guide.rs:84-90`), and pointing it at `docs/TUTORIALS.md` is a code change nobody
has made yet. Until it is made, this page is gated by the lenient harness and not the strict
one. Recorded here rather than left implied, because *"every example is executed by a test"* is
precisely the sentence a reader stops checking behind.

That matters more in a tutorial than anywhere else. You are following it step by step with no
independent way to tell a stale instruction from a current one, so an untested tutorial rots in
the worst possible place. It has happened here: the recipe that built the sample warehouse once
wrote a three-column `sales.orders` and no cube, while the fixture the gates ran against had
five columns and a cube — so every gate was green against a warehouse no reader could produce,
and a reader following Tutorial 2 got `no cube named 'sales'` on its first statement. **Three
of twenty-one blocks ran.** The recipe now calls the fixture's own writer.

## Why this is one file

There used to be four tutorials and every one of them was about cubes. A newcomer told "start
here" landed in a track about a single feature, having never connected to the server, never
seen `sank_data_date`, and never read a refusal — the three things they were going to meet in
their first ten minutes. Tutorial 1 below is the hour that was missing. The four that existed
follow it, unchanged in what they teach and in every statement they run.

| | Tutorial | What you get |
|---|---|---|
| 1 | [The first hour](#tutorial-1--the-first-hour) | Point a server at a warehouse, query it, understand the date axis, read a refusal, read a completeness column |
| 2 | [Your first cube](#tutorial-2--your-first-cube) | Declare, roll up, slice, and read what an answer says about itself |
| 3 | [Making a cube fast](#tutorial-3--making-a-cube-fast) | Lifetimes, staleness targets, and the three controls over what gets stored |
| 4 | [Completeness and policy](#tutorial-4--completeness-and-why-two-people-get-two-different-totals) | Why two people correctly get two different totals |
| 5 | [When a cube refuses](#tutorial-5--when-a-cube-refuses-and-why-that-is-the-feature) | Every refusal, what it means, and what to do instead |

---

# Tutorial 1 — The first hour

**Time:** about fifteen minutes · **You need:** the binaries and the fixture warehouse from
[`QUICKSTART.md`](QUICKSTART.md)

## Step 1 — Point a server at a warehouse

A SANKHYA does not have a data directory it owns and hides. It has a **warehouse**: a
directory of `<schema>/<table>/`, each table holding Parquet files and a `_delta_log`. You
point the server at one and it walks it.

```bash
SANKHYA_NO_PASSWORD=1 \
SANKHYA_WAREHOUSE=./warehouse \
SANKHYA_LISTEN=127.0.0.1:5433 \
  ./target/release/sankhya-server
```

Read the startup line before you type anything, because it is the deployment describing
itself:

```
SANKHYA 0.1.0
  tenant tenant:0000…0001, NO AUTHENTICATION — every connection is accepted,
  10 policy rule(s), 10 table(s) known
  listening on 127.0.0.1:5433
  wire protocol unencrypted — passwords cross the network in plain text
  connect with: psql -h 127.0.0.1 -p 5433 -U <user>
```

`NO AUTHENTICATION` is in capitals on purpose. `SANKHYA_NO_PASSWORD` is spelled as an
opt-*out*, so the insecure choice has to be made deliberately — and having made it, you should
be able to see that you did without checking anything.

**The port is 5433, not 5432.** A SANKHYA and the PostgreSQL it captures from are frequently on
one host, and a default that collided with the source's would make the two indistinguishable in
a connection string. (The Python binding's own default is still 5432, which is a defect;
[`GUIDE.md`](GUIDE.md) §14 names it.)

Also read what the server says it *could not* open. A table it cannot read is named on stderr
rather than omitted, because a server that starts with three tables of four and says nothing
produces an outage that looks, to whoever queries it, like a table nobody ever created.

## Step 2 — Connect with anything that speaks PostgreSQL

```bash
psql -h 127.0.0.1 -p 5433 -U you -d acme
```

No driver, no shim: the wire protocol is PostgreSQL's, so `psql`, DBeaver, DataGrip, Metabase
and your notebook's driver all connect. The version string it reports begins
`PostgreSQL 17.0` — every client parses a major version out of it before it will proceed — and
then says what this actually is, so the prefix does not mislead anybody reading it.

Ask what is there. This is also the first thing every reporting tool does, before you have
typed anything:

```sql
SELECT table_schema, table_name FROM information_schema.tables;
```

Two names work for every table, and the difference matters more than it looks:

```sql
SELECT id FROM sales.orders;
SELECT id FROM orders;
```

The qualified name always resolves. The bare one resolves **while only one schema holds a table
of that name**, and stops resolving the day a second one does — naming both candidates rather
than quietly answering from whichever was registered first. A name that means two things has no
right answer, and picking one would hand you a table you had no way to identify. So anything
written down, or run again next quarter, should qualify.

## Step 3 — Ask it something

```sql
SELECT region, count(*) AS n, round(sum(amount)) AS total
FROM orders
GROUP BY region
ORDER BY region;
```

```
 region |  n  | total
--------+-----+--------
 north  | 334 | 250250
 south  | 333 | 249251
        | 333 | 249750
```

Look at the third row. That region is genuinely **null**, not an empty string. The distinction
survives from the Parquet page, through the Arrow array, to the wire — where it becomes a
length of −1 rather than a length of 0. It is worth noticing on your first query because it is
the first of many places this system refuses to collapse *absent* into *empty*, and the third
row is going to come back in Tutorial 4 as a third of a total.

Nothing here needed a load step, a build step or an index. The read path plans from the table
log alone — no directory listing, no Parquet footer reads — and prunes files by the statistics
the log records.

## Step 4 — Understand `sank_data_date`

Every table carries one guaranteed column, `sank_data_date`, of type `DATE`, and every table is
partitioned on it:

```sql
SELECT sank_data_date, count(*) FROM sales.orders GROUP BY sank_data_date ORDER BY 1;
```

This is the single most load-bearing convention in the system, and there are two decisions
inside it worth ten seconds each.

**The type is `DATE`, not an encoded integer.** Partition paths are
`sank_data_date=2024-03-01`, which Spark and Trino parse as a date natively where an integer is
a string they have to be told about. And `20240301 - 7 = 20240294` is not a date, raises no
error, and is a thing people write.

**The value is declared per table, never defaulted per row.** A table either takes each row's
date from a named column — in which case a null there is an *error* — or uses the ingest date
for every row *and records in its own log that it does*. There is deliberately no per-row
fallback to "today", because that would make the column mean *when it happened* in some rows
and *when we received it* in others, in the same table, with nothing recording which. Then
`WHERE sank_data_date = '2024-03-01'` returns a mixture no query can separate afterwards.

That is the shape of almost every decision you will meet here: **the wrong answer that looks
right is the one being designed against**, and the price is usually that you have to say what
you meant.

## Step 5 — Read a refusal

Type a table name wrong:

```sql
-- ERROR: the table does not exist, and the error says so with a code you can dispatch on
SELECT * FROM sales.ordres;
```

```
ERROR:  [SNK-C0001] Error during planning: table 'sales.ordres' not found
DETAIL:  Correct the statement. The detail names the offending element.
```

Three things are in that, and each is doing a job.

| | |
|---|---|
| **`SNK-C0001`** | A permanent code. It is what a support conversation is conducted in and what a runbook is indexed by. Codes never change meaning and are never renumbered |
| **The message** | What happened |
| **`DETAIL`** | What to do about it — the catalogue's own remediation, so the client and [`ERRORS.md`](ERRORS.md) cannot say different things |

The five-character SQLSTATE beside them comes from the error's **class**, not from its wording,
because every driver in this ecosystem branches on those five characters and a plausible
message with the wrong ones produces a client that connects, appears to work, and mishandles
every failure.

The letter after `SNK-` is that class: `C` your request, `R` a limit, `F` a conflict, `T`
transient, `X` cancelled, `S` a fault that pages somebody. Every `S` code has a runbook.

Now try to write:

```
psql> CREATE TABLE public.staging (id BIGINT);
ERROR:  [SNK-C0006] data definition is not served over this connection; this server is
        a read path over a published warehouse
DETAIL:  Write to the transactional store and let capture publish it, or publish an
         external table with `sankhya-publish`. See GUIDE.md §3.
```

**Refused, not accepted and discarded.** This once returned `CREATE TABLE` and did nothing
durable — the table existed for the rest of that connection and vanished on reconnect. Notice
that the refusal names the supported route: a refusal that only says no sends somebody looking
for a flag to turn it on, and there is no flag. (Its `§3` is the server's own text and points
at the wrong section of a rewritten guide — publishing is §5 there now. Nothing checks a
section number compiled into a binary, which is the point of noticing it here.)

One refusal you should meet now rather than later. Ask the graph anything:

```
psql> SELECT * FROM graph_reachable('payments','acct-1','max_depth=3');
ERROR:  [SNK-C0001] Error during planning: no graph named 'payments' is registered; known
        graphs are []. Refusing rather than returning no rows
```

That is not your mistake. **No server you can start can answer a graph query today** — the
traversal engine is built and tested, and nothing hydrates an epoch into a catalogue anything
can reach. [`GUIDE.md`](GUIDE.md) §11 has the two lines of code that make it so. It is here in
the first hour because the alternative is finding out in the third, halfway through a worked
example.

## Step 6 — Read a completeness column

The last thing to learn in the first hour is the habit this system is built around: **read what
an answer says about itself.**

```sql
SELECT region, amount, completeness, withheld
FROM cube_rollup('sales', 'amount', 'by=region');
```

`completeness` is the fraction of the intended input that reached these cells; `withheld` is
how many rows did not. A completeness of `1.0` means every intended row contributed. Anything
less means something did not.

Two things can withhold a row: a **policy** that filters what you may read, and a row that
**cannot be placed** on the grid — a null dimension key, a null measure. Remember the third row
of Step 3, whose region was null. It is a third of the table, and it cannot be placed on a
`region` axis.

Why this is a column and not query metadata: metadata is tidier and is lost by the first
`SELECT` that does not mention it, and the moment it is lost, **a partial total looks exactly
like a complete one** — same type, same plausible magnitude, no error anywhere. Somebody puts
it in a report and nobody can tell.

That is the whole hour, in one habit. The rest of these tutorials are one feature — the cube —
worked through in depth, because it is where the habit pays.

## What you have learned

- A server is pointed at a warehouse of Parquet and reads each table's schema out of its own log.
- The startup line describes the deployment, in words, including the parts you would rather not
  see.
- Qualified names always resolve; bare ones resolve only while they are unambiguous.
- `sank_data_date` is declared per table and never defaulted per row.
- A refusal carries a permanent code, a class-derived SQLSTATE and a remediation.
- Every cube answer states how much of its input it saw, as columns you can select.

---

# Tutorial 2 — Your first cube

**Time:** about ten minutes · **You need:** a running server and a table

## What a cube is here, and what it is not

A cube in SANKHYA is a **declared view over a published table**. It is not a second copy of
your data, not a separate store, and not something you build before you can query it.

That matters for what you are about to do. There is no load step, no build step and no wait.
You declare which columns are dimensions and which are measures, and the cube answers from the
table you already have.

The trade is that a cube is a *contract*: you say up front how each measure is allowed to
combine. That is the part most systems leave implicit, and it is why they can hand you a
wrong number without noticing.

## Step 1 — See what is there

Connect with any PostgreSQL client. Start by asking what the warehouse has:

```sql
SELECT * FROM cubes();
```

Then ask what one cube is made of. You do not have to know the model in advance, and neither
does a dashboard built against it:

```sql
SELECT * FROM cube_dimensions('sales');
```

```sql
SELECT * FROM cube_measures('sales');
```

`cube_measures` is the more interesting of the two. It tells you each measure's **rule** along
each dimension — how it is allowed to combine — and that is the thing that decides which
questions the cube will answer.

## Step 2 — Roll up

Rolling *up* means rolling a dimension **away**. The `sales` cube has two dimensions, `region`
and `period`. Group by one and the other is aggregated out:

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region');
```

Group by the other instead:

```sql
SELECT period, amount
FROM cube_rollup('sales', 'amount', 'by=period');
```

Group by both and you have the base grain — nothing is rolled away. Note the `|`: the options
string is itself comma-separated, so a list uses a different separator. Nesting one inside the
other is how a list silently truncates at its first element.

```sql
SELECT region, period, amount
FROM cube_rollup('sales', 'amount', 'by=region|period');
```

Notice what did *not* happen: no build step ran between these three queries. Each one is
answered from the fact table, or from a stored cuboid if one happens to fit. You did not have
to know which, and the next tutorial shows how to find out.

## Step 3 — Slice

Slicing fixes a member on one axis and drops that axis from the result. It is not the same as
filtering — the dimension is *gone*, not merely narrowed:

```sql
SELECT period, amount
FROM cube_slice('sales', 'amount', 'where=region:north');
```

There is no `region` column in that result, because you have already said which region. A cube
that returned it anyway would be inviting you to group by a column with one value in it.

**Roll-up and slice are the whole navigation surface.** Not four, and not five: `cube_rollup`
and `cube_slice` are what is registered, at
`crates/sankhya-cube-sql/src/functions.rs:56-60`. A `dice` and a `pivot` exist as kernels and
`dice` is reachable only from inside `cube_slice`'s own implementation; there is no
`cube_pivot` and no drill-down anywhere in `crates/`. Several documents said otherwise, and one
said five. A count is the easiest kind of claim to write without checking.

## Step 4 — Read what the answer says about itself

This is the step people skip, and it is the one worth learning first.

```sql
SELECT region, amount, snapshot, completeness, withheld, materialised
FROM cube_rollup('sales', 'amount', 'by=region');
```

Every cube answer carries these columns:

| Column | The question it answers |
|---|---|
| `snapshot` | *"As of when?"* — the table version this was computed at |
| `completeness` | *"How much of the input did this see?"* |
| `withheld` | *"How many rows did not reach it?"* |
| `materialised` | *"Did this come from stored cells or from the table?"* |
| `from_cuboid` | *"Which stored cells, when it did?"* |

They are columns rather than query metadata on purpose. Metadata is tidier and is lost by the
first `SELECT` that does not mention it — and the moment it is lost, a filtered total looks
exactly like a complete one.

**`snapshot` is what lets you reconcile.** A cube figure and a relational figure taken a minute
apart will differ, and without a version stamp the only available explanation is "one of them
is wrong". With it, the explanation is arithmetic.

## Step 5 — Meet a refusal

Ask for something the cube cannot honestly answer:

```sql
-- ERROR: a ratio cannot be derived from its parts
SELECT region, margin_pct FROM cube_rollup('sales', 'margin_pct', 'by=region');
```

`margin_pct` is a ratio. The margin of two regions together is not the sum of their margins,
nor the mean, nor anything else you can compute from the two margins alone — you need the
underlying numerators and denominators, which a rolled-up cell no longer has.

So `margin_pct` is declared as composing along nothing, and the query is refused **while it is
being planned**. Not after a plausible number has been produced and put on a slide.

This is the trade named at the top. You told the system how the measure combines, so it can
tell you when a question does not have an answer — instead of answering anyway.

## Step 6 — Declare one of your own

Everything so far used a cube somebody else declared. Making one is a statement:

```sql
CREATE CUBE quarterly FROM orders
  DIMENSION region FROM orders ON region (LEVEL area = region)
  DIMENSION period FROM orders ON period (LEVEL quarter = period)
  MEASURE amount (SUM ALONG region, SUM ALONG period);
```

Read it as: the facts are in `orders`; `region` takes its members from `orders` itself, joined
on the fact table's `region` column; and `amount` adds along both dimensions.

**The `MEASURE` line is the contract from the top of this page, written down.** Every measure
needs a rule for every dimension and there is no default — leave one out and the cube is
refused when you declare it, naming the measure and the dimension. That refusal is the whole
point: an implicit `SUM` is exactly how a system hands you a wrong number without noticing.

It is available immediately, to this connection and every other:

```sql
SELECT cube FROM cubes();
```

That last clause is not a convenience, it is a warning. `CREATE CUBE` **persists** a definition
under the warehouse, visible to every other connection. An *ephemeral* lifetime — one that ends
with your session — is the intended default and does not exist yet, so on a shared server your
exploration is published to everybody until you remove it. Which you do by saying so:

```sql
DROP CUBE quarterly;
```

**A drop also reclaims anything the cube materialised**, which nothing else would: the
background sweep deliberately keeps cuboids belonging to a cube it cannot find, because
deleting on a guess is how a cache becomes a data loss. The drop is the only moment anything
knows the cube is *gone* rather than merely unrecognised.

There is no `CREATE OR REPLACE CUBE`. Replacing a cube retires everything it materialised, and
that should not happen because you re-ran a script — so drop it and create it, and the
expensive half is written down.

## What you have learned

- A cube is a declared view over a published table: no build step, no second store.
- `CREATE CUBE` declares one and `DROP CUBE` removes it, along with anything it materialised.
- `cube_dimensions` and `cube_measures` let a client discover the model instead of hardcoding it.
- Roll up rolls a dimension *away*; slice *removes* an axis rather than filtering it.
- Roll up and slice are the two navigations that exist.
- Every answer states its snapshot, its completeness and where it came from.
- A measure that cannot compose is refused rather than approximated, and *where* it is refused
  differs by kind: a measure declared with no rule is refused when the cube is declared, a
  ratio or a mean when the roll-up is planned, and a semi-additive measure across time is not
  refused at all because it cannot be written --- the reduction operator belongs to the
  measure and is never the caller's. Tutorial 4 works through all four.

---

# Tutorial 3 — Making a cube fast

**Time:** about fifteen minutes · **Before this:** Tutorial 2

## The thing to understand before any of the controls

A materialised cuboid is a **cache**, and it is a cache in a stricter sense than usual: it
cannot change the answer.

The reason is the key. A stored cuboid is keyed by *(definition version, snapshot, scope,
shape)*. Every one of those is part of the lookup, so a new commit produces a **miss**, not a
stale hit. There is no invalidation protocol, no time-to-live to tune, and no window in which
something old is served as though it were current.

That is what makes automatic materialisation safe. Being wrong about what to cache costs you
latency; it cannot cost you correctness. If it could, none of the rest of this tutorial would
be a good idea.

## Step 1 — Find out where an answer came from

Before tuning anything, learn to read what is happening:

```sql
SELECT region, amount, materialised, snapshot
FROM cube_rollup('sales', 'amount', 'by=region');
```

`materialised` is `false` when the answer was computed from the fact table and `true` when it
came from stored cells. It reports **what happened** — not what you asked for.

That distinction is not pedantry. This column previously echoed an argument the caller passed,
so an operator asking "why was this fast?" was told whatever their own query had typed. A
diagnostic that reports its input is worse than no diagnostic, because it looks like evidence.

## Step 2 — Choose a lifetime

| | Persisted | Materialised | Maintained by | Ends when |
|---|---|---|---|---|
| **Ephemeral** | no | no | nothing | the session ends |
| **Declared** | yes | no | nothing | it is dropped |
| **Maintained** | yes | yes | the warehouse | it is dropped |

**Ephemeral is the intended default** — exploring should not require deciding whether a
question deserves to be durable, and a warehouse should not accumulate a definition per
abandoned question. It is also **not what a plain `CREATE CUBE` does today**, as Tutorial 2's
Step 6 said: there is no syntax for asking for one yet. `M14` builds the lifetime with the
mandatory expiry that `RSK-35` requires. Until then, drop what you declare.

**Declared** costs one small file and computes on demand. It is right for a cube asked about
occasionally — and for any cube whose readers have *different* permissions, for a reason
covered in Tutorial 4: a stored aggregate is only usable by callers entitled to exactly the
rows it was built from, so materialising a cube read by twenty differently-restricted analysts
mostly produces cells nobody may use.

**Maintained** adds a `target_lag`, and the warehouse keeps the cube within it whether or not
anybody is logged in.

## Step 3 — Understand `target_lag`

`target_lag` is a **staleness target, not a schedule**. `target_lag = 5` means *these cells may
be at most five commits behind* — not *rebuild every five commits*.

A schedule is wrong in both directions at once. It rebuilds when nothing has changed, and it
fails to rebuild when a build takes longer than its interval. A target says the thing you
actually care about.

And here the target is **checkable rather than estimated**, because a stored cuboid records the
version it was computed at. Staleness is the distance from the table's current version: an
integer, known without reading a clock. A duration would have to be inferred from commit rates,
and an inferred SLA is right while the system behaves and wrong exactly when it does not.

**A cuboid past its target is never served as though it were fresh.** The answer falls back to
live aggregation — slower and correct — and says `materialised = false`, so you can see which
one you got.

## Step 4 — The three controls

The lattice of possible cuboids is exponential in the dimension count, so "store everything" is
not a plan. Three controls decide what actually gets stored, and they belong to three different
people on purpose.

| Level | Who | What it says |
|---|---|---|
| **Definition** | whoever models the cube | shapes **pinned** — always worth holding |
| **Configuration** | the operator | the row **budget** automatic selection may spend |
| **Session** | the caller | whether *this* query uses materialisation at all |

### The definition pins

Automatic selection spends the operator's budget on **evidence**: the shapes people have
actually asked for. A pin is the statement that a shape is worth holding *before* any evidence
exists — the month-end roll-up nobody runs until the day it has to be instant.

So pinned shapes are not put through selection. A pin that had to win against a query log would
not be a control at all.

### The operator budgets

```toml
[cubes]
budget_rows = 10000000
```

It is the operator's storage, being spent on their behalf by a selection reading somebody
else's queries — so it is bounded by a number they set rather than by whatever the lattice
happens to contain.

Set it to `0` and automatic selection buys nothing. The base cuboid and any pinned shape are
still built: neither is bought from the budget.

### The caller may ask for less — and only less

```sql
SELECT region, amount, materialised
FROM cube_rollup('sales', 'amount', 'by=region, materialise=false');
```

There is deliberately **no value that widens anything**. A session that could raise the budget
would be an unbounded storage grant to anybody who can open a connection — a resource
exhaustion with a polite interface.

| Value | Effect |
|---|---|
| *(omitted)* | use whatever the definition and configuration provide |
| `materialise=pinned` | use only shapes the definition names, not ones bought from a query log |
| `materialise=false` | compute from the base data |

An unrecognised value is refused while the query is planned, rather than quietly taking its
default:

```sql
-- ERROR: 'materialise' must be true, false or pinned
SELECT region, amount FROM cube_rollup('sales', 'amount', 'by=region, materialise=maybe');
```

That refusal exists because a misspelled option that silently defaults produces a result which
is wrong in a way the query text does not reveal.

## Step 5 — Use `materialise=false` as a check, not a workaround

This is the most useful habit in the tutorial.

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region, materialise=false');
```

Run the same question with and without it. **The two answers must be bit-identical.** Not close,
not within a tolerance — identical.

If they ever differ, you have found a defect, not a tuning question. Materialisation is a cache
and a cache that changes the answer is not one.

Getting that property was not free, and it is worth knowing why. A cube rolls up in stages and
every stage rounds, so `round(round(a+b) + round(c+d))` is not `round(a+b+c+d)`. Fixing the
*order* of summation makes one reduction reproducible and does nothing about **associativity** —
and a stored cuboid is precisely a re-association of the same addition. The discrepancy measured
at one unit in the last place: large enough for two reports to disagree by a penny, small enough
that nobody can point at a defect. So a stored aggregate keeps its value **unrounded**, as the
components of an exact expansion, and rounds once when read.

That is also the narrow sense in which "deterministic" is used across this system, and it is
worth carrying into any figure you defend. Every kernel is **bit-reproducible**: two runs over
the same values return the same bits, whatever the machine's core count or load. A shorter list
is additionally **compensated** — the vector kernels, `mat_multiply`, `mat_trace`, and the
statistics, finance and cube reductions all route through `deterministic_sum`. The
decompositions and the incomplete gamma and beta functions do not. [`GUIDE.md`](GUIDE.md) §9 is
the list with its evidence; the distinction matters the first time somebody asks you how
accurate a number is rather than how repeatable.

## Step 6 — Know what gets chosen, and why

Automatic selection reads a **query log**: a bounded record, per cube, of which dimensions
people grouped by.

The repetition is the weighting — a shape asked ten times counts ten times — and old entries are
overwritten, so a dashboard nobody has opened in a week stops pinning storage without anybody
deciding it should. The log is bounded because a structure that grows once per query and is
never trimmed is a leak with a business justification.

It records a *shape*: which cube, which dimensions. There is nowhere in it to put a member, a
predicate, or who was asking. Worth stating plainly, because a query log is the kind of thing
that quietly becomes a record of who asked what about whom.

**A cube nobody has queried gets its base cuboid and nothing else.** That is the honest answer
rather than a guess — there is no evidence about what would help, and spending an operator's
storage on a guess is worse than spending none.

## Step 7 — Know who a stored cuboid can serve

A background refresh has no principal — nobody is logged in at four in the morning — so it
builds the **unrestricted** cuboid: an aggregate over every row.

That cuboid may serve only a caller whose own permissions withhold nothing. Serving it to
somebody a row policy filters would be a disclosure through arithmetic, and an invisible one:
the number is real, it is simply computed over rows they may not read. No error, nothing in a
log to find.

So: **background refresh helps dashboards and service accounts, and does nothing for a
restricted analyst.** Their cuboids can only be built by their own queries. Know this before you
measure, or you will conclude materialisation is broken when it is behaving exactly as designed.

## A checklist

1. Is the cube read often enough to be worth storage? If not, leave it **Declared**.
2. Do its readers share permissions? If they are all differently restricted, materialising helps
   almost nobody.
3. Set `target_lag` to the staleness you can actually tolerate, not to a rebuild frequency.
4. Pin the shapes you know matter. Let the budget buy the rest from evidence.
5. Check `materialised` to see what is happening, and `materialise=false` to check the number.

---

# Tutorial 4 — Completeness, and why two people get two different totals

**Time:** about ten minutes · **Before this:** Tutorial 2

## The problem this solves

Two analysts run the same query. One may see every region, the other only the north. They get
different totals.

**Both totals are correct.** An aggregate is computed over the rows the caller may read, so a
restricted caller's total is genuinely the total of what they may see.

The danger is not the difference. It is that a number arrives with no indication that it is
partial. A total over half the data looks exactly like a total over all of it — same type, same
plausible magnitude, no error anywhere. Somebody puts it in a report and nobody can tell.

Most systems make an operator choose: enforce the policy and hand out silently-partial
aggregates, or bypass it and leak. Neither is acceptable, and the choice is a false one.

## Step 1 — Every answer states what it saw

```sql
SELECT region, amount, completeness, withheld
FROM cube_rollup('sales', 'amount', 'by=region');
```

| Column | Meaning |
|---|---|
| `completeness` | the fraction of the intended input that reached these cells |
| `withheld` | how many rows did not |

A completeness of `1.0` means every intended row contributed. Anything less means something did
not, and `withheld` says how much.

So a filtered total is **distinguishable by looking at it**, rather than by knowing which role
you happened to be connected as.

Against the fixture warehouse, the arithmetic is checkable by hand: a `SUM` cube over a
1,000-row table whose region is null on 333 rows reports `completeness 0.667`, `withheld 333`,
and a grand total of `499500` — exactly `SELECT sum(amount) … WHERE region IS NOT NULL`. That
is the null third row you met in Tutorial 1, arriving as a third of the value.

## Step 2 — Understand where the number comes from

This is the part worth being precise about, because the obvious implementation is wrong.

Completeness **cannot be computed from the result**. A withheld row leaves no trace: it is not
in the cells, not in a null, not anywhere. An aggregate that counts what arrived and divides by
what arrived reports itself complete however much policy removed — always, and with total
confidence.

So the withheld count comes from **the filter that did the withholding**, or it does not exist.
It is carried from the point of enforcement to the point of presentation, and it is a required
field the whole way. A stored cuboid carries it too, so a cube served from storage still says
how much of the fact table it saw rather than assuming the answer.

## Step 3 — Rows that could not be placed

Policy is not the only reason a row fails to reach a cube. A row with a null dimension key, or a
null measure, cannot be placed on the grid.

Those rows are **counted, never dropped**. Skipping them leaves the total quietly short — which
is the same failure as a policy-filtered total presented as complete, so it gets the same
machinery and shows up the same way in `withheld`.

## Step 4 — Insist on a threshold

Reading the column is good. Sometimes you want the query to refuse rather than hand you
something you have to remember to check:

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region, min_completeness=0.5');
```

If less than half the input reached the cube, the query fails instead of returning a number.

Use this on anything automated. A human might notice a completeness of `0.3`; a nightly job
writing to a dashboard will not.

## Step 5 — Know the empty case

An aggregate over **no rows** has *no* completeness. It is not complete.

That sounds like hair-splitting and is not. If "nothing at all" reported completeness `1.0`,
then an empty result would sail through every `min_completeness` threshold you set — the
strictest possible check would pass on the emptiest possible answer. So the fraction is *absent*
rather than `1.0`, and a threshold refuses it.

It is the same absent-versus-zero distinction the cube keeps everywhere: a cell that does not
exist is not a cell containing zero.

## Step 6 — What this means for materialising

A stored cuboid is keyed by the **scope** it was computed under — a digest of what the guard
*permits*: tenant, table, action, row filter, column masks.

Deliberately **not** who was asking. That single decision does three jobs:

1. **It bounds the population.** A thousand analysts across six roles produce six scopes, not a
   thousand.
2. **It answers the standing-grant problem.** A cuboid is tied to an entitlement *set*, not a
   person. Change the policy and the digest changes, the key no longer matches, and the old
   cells are simply never found. There is no stale-permission window to close, because there is
   no lookup that can succeed.
3. **It decides who may be served.** The background refresher builds the *unrestricted* cuboid.
   It may serve only a caller whose guard withholds nothing — checked by asking the guard, not
   by comparing digests.

The practical consequence, again: **materialising helps callers who share an entitlement set.**
A cube read by twenty differently-restricted analysts mostly produces cells nobody else may use.
That is a reason to leave such a cube Declared, not a defect.

## What you have learned

- Two principals correctly get different totals; the difference is the point, not the bug.
- Completeness is carried from the filter, never derived from the result — a withheld row leaves
  no trace.
- Unplaceable rows are counted, not dropped.
- `min_completeness` turns a thing you must remember to check into a thing the query enforces.
- An empty aggregate has no completeness, so it cannot pass a threshold.
- Stored cells are keyed by entitlement set, which is what makes them safe to reuse.

---

# Tutorial 5 — When a cube refuses, and why that is the feature

**Time:** about ten minutes · **Before this:** Tutorial 2

## The idea

Most of what follows is a list of things this system will not do. That is not a limitations
page. Each refusal replaces a **wrong number** that another system would have handed you
without comment.

A wrong aggregate is the worst kind of defect available. It has the right type, a plausible
magnitude, and no error attached. It gets copied into a report, and by the time anybody
disagrees with it there is no way to reconstruct which of the two figures was wrong.

So the design rule is: **if a question does not have an answer, say so while the query is being
planned.** Not afterwards, and not approximately.

## Refusal 1 — A ratio has no roll-up

```sql
-- ERROR: a ratio cannot be derived from its parts
SELECT region, margin_pct FROM cube_rollup('sales', 'margin_pct', 'by=region');
```

**Why.** The margin of two regions together is not the sum of their margins. It is not the mean
either — that is only right when the regions are the same size, and regions are never the same
size. It is not anything you can compute from the two margins alone: you need the underlying
numerators and denominators, and a rolled-up cell no longer has them.

So `margin_pct` is declared as composing along nothing.

**What to do.** Roll up the *parts* and divide at the end. Store margin as its numerator and
denominator — both of which are additive — and compute the ratio in the outer query, after the
aggregation, where the inputs still exist.

This is the general shape of the fix, and it works because the thing that does not compose is
the division, not the data.

## Refusal 2 — Averaging is the same mistake wearing a friendlier name

An average of averages is an average only when every group is the same size.

It reads as harmless because `avg` is a function every database offers, and because the answer
looks fine. It is the single most common wrong aggregate in analytics, and it is wrong by the
identical argument as the ratio above — a mean is a ratio with the denominator hidden.

**What to do.** The same thing: roll up the sum and the count separately, and divide once at the
end.

## Refusal 3 — A semi-additive measure across the wrong axis

A stock level, a headcount, an account balance: these add across *regions* and do not add across
*time*. Adding January's closing balance to February's gives a number that means nothing.

Here that is not merely rejected — it is **not expressible**. The reduction operator belongs to
the measure, declared per dimension, and is never the caller's to choose. There is no way to
write the query that would sum a balance across time, because there is nowhere in the syntax to
put the wrong operator.

That is a stronger guarantee than a check. A check has to be reached; a thing that cannot be
said has nothing to reach.

**What to do.** Declare the measure with the rule it actually has — `Last` across time,
`Sum` across everything else — and ask for it. The cube applies the declared rule and the
question answers itself.

> **This rule was, for a while, enforced in only one direction.** Until 2026-09-01 a measure
> declared `MEAN ALONG region` or `MAX ALONG region` returned the **sum**: 15,687 where the
> maximum was 373.5. Composability was checked correctly — rolling a `MEAN` or `NONE` measure
> away was refused with the right sentence — and the cell was then read with a hardcoded
> summation one layer below, so the declared rule never reached the number. That is the exact
> failure this tutorial is about, arriving beneath where the model checks for it. It is fixed,
> and `crates/sankhya-server/tests/cube_rules.rs` compares a cube declaring `MAX`, `MEAN` and
> `MIN` against `max()`, `avg()` and `min()` over the same rows, over the wire, on every build.
> A defect that is fixed and unpinned is a defect that comes back.

## Refusal 4 — A measure with no rule at all

A measure declared without an aggregation rule is refused **when the cube is declared**, naming
the measure.

**Why not default to summation?** Because summation is the wrong answer for balances, for
ratios, for averages, for prices and for rates — and it is *silently* wrong for every one of
them. A default here converts a modelling oversight into a wrong number in production, and the
oversight is invisible from that point on.

A cube whose measures nobody has thought about should not become a cube.

**What to do.** Declare a rule per dimension. It takes a minute and it is the minute that makes
every later answer trustworthy.

## Refusal 5 — An option that does not exist

```sql
-- ERROR: 'materialise' must be true, false or pinned
SELECT region, amount FROM cube_rollup('sales', 'amount', 'by=region, materialise=maybe');
```

Unknown option names and unusable option values are both refused rather than ignored.

**Why.** An option that is quietly ignored takes its default, and the result is wrong in a way
the query text does not reveal. Somebody reads the statement, sees the option they meant, and
has no way to know it did nothing. `materialise=fasle` should not silently become
`materialise=true`.

## Refusal 6 — A member reachable by two paths

In a hierarchy with alternate roll-ups, a member can be reachable more than one way. It
contributes **once**.

This is not a refusal you will see as an error — it is a wrong number that does not happen. It
is worth knowing about because double-counting in a ragged or alternate hierarchy is
approximately undetectable by inspection: the total is too large by an amount that looks like
growth.

Ragged hierarchies are handled **natively, never padded**. Padding invents members that do not
exist, and invented members show up in results as real ones.

## Refusal 7 — A threshold you set, enforced

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region, min_completeness=0.5');
```

If less than half the input reached the cube, this fails rather than returning a number. See
Tutorial 4.

An empty result carries *no* completeness rather than `1.0`, so it cannot pass a threshold
either — otherwise the strictest check you can write would pass on the emptiest possible answer.

## What is deliberately not here

**MDX.** Not planned — see [ADR-0007](adr/0007-the-cube-model.md). The navigation vocabulary is
SQL table functions instead, so a cube is reachable from any client that speaks the PostgreSQL
wire protocol, with no second query language to learn or to secure.

**A cube-build step.** There is none, and its absence is what Tutorial 2 Step 2 is
demonstrating.

*(This section used to say `CREATE CUBE` was not a statement — "a cube is registered against a
warehouse rather than written in SQL. That is a gap rather than a decision." It has been a
statement since M7, and Tutorial 2 of this same set has taught it ever since, so the two
contradicted each other inside one document set. Nothing checks English, which is why it
survived four tutorials' worth of review. A document that describes an intention in the present
tense lies to the person least able to tell.)*

## The shape of all of it

Every refusal above is the same move: **make the contract explicit, then enforce it while the
query is being planned.**

The cost is that you have to say how a measure combines before you can use it. The return is
that no cube in this system can hand you a confidently wrong total — and confidently wrong
totals are what analytical systems are actually for, most of the time, by accident.

---

## Where to go next

| | |
|---|---|
| [`GUIDE.md`](GUIDE.md) | The reference: every statement, every function, both clients, and what each one refuses |
| [`QUICKSTART.md`](QUICKSTART.md) | Build it, load ten gigabytes, watch capture reconcile |
| [`OPERATIONS.md`](OPERATIONS.md) | Running a server: configuration, metrics, the diagnostic, backup and the drill |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | How it is put together, and the measurements behind the choices |
| [`GLOSSARY.md`](GLOSSARY.md) | The coined terms, the milestone names and the finding codes |
| [`STATUS.md`](STATUS.md) | What is built, what is not, and what was got wrong on the way |
| [`adr/`](adr/) | The decisions, each with the question that prompted it |

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
