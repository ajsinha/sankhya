# 10. Multidimensional analysis

> A `GROUP BY` knows the column names you typed. A cube knows a *model*: which columns are
> dimensions, which are measures, and — the part that decides whether an answer is correct — how
> each measure may be combined along each dimension. This chapter argues that declaring that rule
> per dimension, and refusing a measure that lacks one, is the difference between a cube and a
> convenience over `GROUP BY`; that snapshot keying makes a materialised aggregate structurally
> incapable of being stale; and that the bit-identical property those two buy is what makes
> automatic materialisation safe. It also gives the one-ULP defect that nearly broke the claim.

## 10.1 What is missing from `GROUP BY`

SQL's `GROUP BY CUBE` and `ROLLUP` are *grouping constructs*: they enumerate combinations of the
columns you name in the statement. There is no dimension, no hierarchy, no declared measure, no
consolidation rule and no notion of a member. Writing `GROUP BY ROLLUP(year, quarter, month)`
does not make a time dimension; it makes one query that knows three column names.

The gap matters because the operations people actually perform — take this slice, dice it by
those two dimensions, roll it up that hierarchy, drill into the outlier — are not a sequence of
unrelated statements. They are **navigation of one structure**, and a system that cannot represent
the structure makes the user reconstruct it in the client, which is how the work ends up in a
spreadsheet nobody can reconcile.

## 10.2 Why this system, specifically

Three properties SANKHYA already had are the three a cube engine most needs, and the combination
is not available in one process anywhere else.

| Property | Why a cube needs it |
|---|---|
| Parent-child hierarchies are graphs | A ragged organisation tree, a chart of accounts with alternate roll-ups, a legal-entity structure where a subsidiary appears under two parents — these are what the graph engine already traverses, with bounded traversal and reported truncation. **A consolidation path *is* a traversal.** Every other cube engine reimplements hierarchy walking as a special case; here it is the general case that already exists |
| Consolidation is a large floating-point reduction | A roll-up is that reduction at its worst — many partials, widely varying magnitudes, summed across a hierarchy. Whether two runs of the same consolidation tie out is the first question a finance function asks and the one most products answer with a shrug |
| Every table already carries a date axis | A time dimension exists before anybody declares one (Chapter 7) |

## 10.3 The rule that decides whether an answer is correct

> **A measure with no declared aggregation rule is refused at definition time. Not defaulted to
> `SUM`.**

This is the single most important decision in the cube model, because the default is wrong for an
entire class of measures and wrong *invisibly*.

**Worked example.** A closing balance, twelve monthly cells, in thousands:

| Month | Jan | Feb | Mar | Apr | May | Jun | Jul | Aug | Sep | Oct | Nov | Dec |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Closing balance | 412 | 430 | 408 | 455 | 471 | 462 | 480 | 495 | 488 | 502 | 517 | 524 |

Summed across time: **5,644**. The year's closing balance: **524**. The first figure has the right
magnitude for a balance-sheet line, the right sign, four significant figures, and no meaning
whatsoever. It reconciles against nothing, because nobody reconciles a subtotal. A rate averaged
across regions without weighting is wrong the same way: nothing is missing, nothing is null, and
no test catches it.

Three kinds of measure:

| Kind | Behaviour | Example |
|---|---|---|
| **Additive** | Sums over every dimension | Transaction amount, quantity |
| **Semi-additive** | Sums over some dimensions, something else over others | A balance: sum across accounts, **last** across time |
| **Non-additive** | Cannot be derived from its own children at all | A ratio, a distinct count, a percentile |

### Additive is not the same as composable

An earlier version of this design said a semi-additive measure was *worse* than a non-additive
one — "correct along most dimensions and wrong along exactly one". That is a description of what a
naive implementation does, not of the rule, and taking it literally cost a set of tests that
refused *valid* roll-ups, which would have sent every balance query to base data for no reason.

The precise statement separates two things the loose one ran together:

- **Additive** is about the *operator*: may this measure be **summed** along this axis?
- **Composable** is about *permission*: can the whole be built from partial aggregates along this
  axis at all?

A closing balance is not additive over time and it **is** composable over time, because
`last(last(a, b), c) == last(a, b, c)` given an order. Rolling it up across time is perfectly
valid — *provided the executor applies `last` and not `sum`.*

| Rule | Combines by | Composes further? |
|---|---|---|
| `SUM` | adding | yes |
| `MIN`, `MAX` | the extreme | yes |
| `FIRST`, `LAST` | position in the dimension's order | yes |
| `MEAN` | the arithmetic mean | **no** — an average of averages is an average only when every group is the same size, and groups are never the same size |
| `NONE` | it cannot be derived from parts at all | **no** — a ratio, a distinct count |

> **Key idea**
> The danger of a semi-additive measure is **the operator, not the axis**, and it lives in the
> executor. So the operator is the *measure's*, never the caller's: a roll-up reduces eagerly,
> under the rule the measure declares for the dimension being rolled away, and each cell of the
> result holds one value. There is nothing left for a later call to reduce differently. A first
> version of this module merged the contributing cells and left the caller to choose — which is
> exactly how a closing balance gets summed, with every part of the answer real.

The consequence is stronger than the requirement asked for. The exit criterion asks that summing
a semi-additive measure across time be *rejected at planning time*. **It is not rejected — it is
not expressible.**

### `FIRST` and `LAST` need to know the order

This is the trap underneath the previous one, and it is worth a paragraph because it survives
testing.

A semi-additive measure names a *position* — the closing balance, the opening headcount — which
is meaningless over an unordered bag of contributions. The obvious implementation takes them in
whatever order the cells were visited, which for a sorted address map is **lexicographic by
member name**.

`"feb" < "jan"`, so the closing balance of the first quarter is January's.

ISO-8601 dates sort correctly, so a system tested with `2026-01`, `2026-02` never exhibits it,
and the first wrong number appears against member names somebody chose for a report. So the order
is a **parameter**, and a `FIRST` or `LAST` roll-up without one is refused rather than guessed at.

## 10.4 The five navigations

The requirement asks for slice, dice, roll-up, drill-down and pivot, **expressible from SQL with
no separate build step**. Every operation is a table function with a fixed output schema, callable
in a `FROM` clause and joinable against ordinary tables:

```sql
SELECT r.region, r.amount, p.manager
FROM cube_rollup('figures', 'amount', 'by=region') AS r
JOIN people AS p ON p.region = r.region
WHERE r.completeness = 1.0 AND r.overlay IS NULL
```

A cube that cannot be joined against a table is a separate product with its own query language,
and the point of putting multidimensional analysis in the same engine is that it is not one.

**Roll-up is the one that can be wrong.** Slicing, dicing and pivoting only ever remove or
rearrange cells — they cannot produce a number that was not already there, so the worst they do
is return nothing. Roll-up *computes*: it takes cells at one grain and produces cells at a coarser
one, and whether that is legitimate depends on the measure.

The SQL surface as it stands today registers **two navigation functions and three discovery
functions**:

| Function | What it does |
|---|---|
| `cube_rollup(cube, measure, options)` | Rolls a dimension away, under the measure's declared rule |
| `cube_slice(cube, measure, options)` | Narrows to a member; a restriction on the question, not a loss of data |
| `cubes()` | Every cube: name, fact table, dimensions, measures |
| `cube_dimensions(cube)` | Dimension, level, **depth**, column |
| `cube_measures(cube)` | Measure, dimension, rule, **composes** |

Dicing is expressed as restrictions on those two — the `where=` option takes a list — and
drill-down as a `by=` at a finer level. `depth` is a column rather than the row order because the
order is a fact about the model: sort the result without it and you draw a list where there is a
hierarchy. `composes` says whether a measure can be rolled up **at all**, because a interface
offering "roll up by time" on a ratio offers a button that cannot work.

The full option set is checked against a known list, so a misspelling is refused: `by`, `where`,
`order`, `overlay`, `allocate`, `materialise`, `min_completeness`. The reason is stated in the
source and is the general form of the argument in this chapter: *`by=regoin` silently ignored
gives a grand total labelled as a breakdown, and nobody reviewing the SQL would catch it.*

Pivot and hierarchy drill exist in the library layer and are **not separately exposed as SQL table
functions**; they are named here because a reader comparing this list against the requirement will
notice. MDX is deliberately not planned (Chapter 4).

### Declaring a cube

```sql
CREATE CUBE quarterly FROM orders
  DIMENSION region FROM orders ON region (LEVEL area = region)
  DIMENSION period FROM orders ON period (LEVEL quarter = period)
  MEASURE amount (SUM ALONG region, SUM ALONG period);
```

Read it as: the facts are in `orders`; `region` takes its members from `orders` itself, joined on
the fact table's `region` column; and `amount` adds along both dimensions.

Both table positions — the fact table and each dimension's member table — accept a **qualified**
name, `sales.orders`. Nowhere else in the statement does: a dimension name, a level name and a
column name are not qualified, and accepting a dot in one of those would parse a typo into a name
nothing resolves, giving a definition that saves cleanly and hydrates to nothing.

> **This did not work until 2026-09-02.** The statement's lexer stops an unquoted word at a `.`,
> so `sales.orders` arrived as three tokens and the parser failed with *"expected at least one
> `DIMENSION`"* — a message about the wrong half of the statement, which is how it survived. The
> bare form was no better: it resolved only while a single schema claimed the name, so on any
> warehouse where two schemas both held an `orders`, a cube could not be declared at all. Unit
> tests, mutation tests and a green gate all passed throughout. It was found by writing a
> *runnable example* — see §20.8. Levels are declared
coarse to fine, which is the order a drill-down walks. A parent-child hierarchy is
`PARENT employee_id TO manager_id`; an alternate roll-up is `ROLLUP emea TO world`; and two
optional clauses follow — `MAINTAINED WITHIN 5 VERSIONS` and `PINNED (geography)`.

There is deliberately **no `CREATE OR REPLACE CUBE`**. Replacing a cube retires everything it
materialised, and that must not happen because somebody re-ran a script. Drop it and create it, so
the expensive half is written down.

`DROP CUBE` also reclaims every cuboid the cube materialised — and that matters more than it
sounds. The ordinary cuboid sweep deliberately *keeps* anything belonging to a cube it cannot find
a current version for, because deleting on a guess is how a cache becomes a data loss. **The drop
is the only moment at which that storage can be released.** Nothing else will ever reclaim it.

## 10.5 Materialisation cannot be stale

The classical objection to precomputing an aggregate is that it can go stale, and that the
staleness is not detectable from the copy — which is why every product in this category has a
"the cube is stale" failure mode.

That objection dissolves here, and the reason is a property the storage layer already has:

> Because files are immutable and every key embeds a snapshot identifier, **a new commit cannot
> produce a stale hit** — the key simply misses. No invalidation protocol is required.

A materialised cuboid keyed by *(definition version, snapshot, cuboid, scope)* **cannot be
stale.** It either matches the snapshot the query is reading at, or it is a miss and the answer is
computed. There is no invalidation protocol to get wrong, no time-to-live to tune, and no window
during which a stale answer is served.

> **Key idea**
> That changes what materialisation *is*. It is not a second copy of the truth. It is a **cache**,
> and being wrong about what to cache costs latency rather than correctness. Which is precisely
> what makes automatic selection safe: *what may be automatic is anything whose being wrong costs
> latency; what may not is anything whose being wrong changes an answer.*

Automatic: which cuboids to materialise, when, in what order, how long to keep them, and whether
to answer a query from a materialised ancestor. **Never automatic:** aggregation rules, hierarchy
definitions, what a measure means, and the completeness contract.

### The definition half of the key needed the same protection

The guarantee has a precondition the first version of the design did not state: **the definition
version must actually move when the definition does.** A version somebody edits by hand does not.
It is a field, a review that has to catch it when the field is missed, and then a cuboid built
under the old meaning of a measure being served against the new one — a real number, computed
correctly, from a definition that no longer exists.

The snapshot half is derived from the log and cannot be forgotten. So the definition half is
derived too: the version is a **fingerprint of the validated content**, computed at validation and
available no other way. Change a measure's rule and the key changes; reformat the file and it does
not. There is no field to forget.

Two properties of that fingerprint are load-bearing rather than incidental:

- Fields are **length-prefixed**, or a dimension named `ab` on column `c` hashes identically to one
  named `a` on column `bc`, and two different cubes share a materialisation key.
- **Level order is preserved while roll-up edges are sorted**, because levels are coarse-to-fine
  and reordering them reorders a drill-down, whereas a hierarchy is a *set* of edges — and
  rebuilding every cuboid because somebody sorted a config file is a cost against no risk.

The hash is FNV-1a, which defends against accident and not against a constructed collision. That
is written down in the module too: **a hash in a cache key invites the cryptographic assumption,
and here the assumption is wrong.**

## 10.6 The arithmetic that nearly broke it

The exit criterion asks that every query return **bit-identical** results with materialisation on
and off. It did not, and the reason was not a defect in any one place — it was arithmetic.

A cube rolls up in stages: sum by month, then sum the months. Every stage rounds, and

$$\mathrm{round}\big(\mathrm{round}(a+b) + \mathrm{round}(c+d)\big) \;\neq\; \mathrm{round}(a+b+c+d)$$

Fixing the *order* of summation makes one reduction reproducible; it does nothing about
**associativity**, and a materialised cuboid is precisely a re-association of the same addition.

**The measured difference was one ULP** — which is the worst possible size: large enough that two
reports disagree by a penny, small enough that nobody can point at a defect.

So a materialised aggregate **stores its value unrounded**, as a Shewchuk expansion: a list of
non-overlapping doubles whose sum is exact. Adding is exact, combining two expansions is exact,
and rounding happens once, when somebody reads the number. Any grouping of the same values then
gives the identical `f64`.

The cost is a few doubles per cell and a pass over them per addition. It buys the one property a
cache must have: not changing the answer.

> **Key idea**
> **A cube that is faster and different is not a faster cube.** The bit-identical property is
> load-bearing — it is what makes materialisation a cache rather than a second source of truth,
> and therefore what makes automatic selection safe. If some measure or some path cannot be made
> to agree bit for bit, that measure is excluded from materialisation entirely rather than the
> property being weakened to "agrees closely".

Session control makes the property checkable by a person rather than only by a test:

```sql
SELECT region, amount, materialised
FROM cube_rollup('sales', 'amount', 'by=region, materialise=false');
```

`materialise=false` computes from base data. **That is the reproducibility check**: a figure that
differs between it and the default is a defect, not a tuning question. `materialise=pinned` uses
only shapes the definition names, and not a selection bought from another user's queries.

## 10.7 Answering from an ancestor is where the wrong answers live

A query for a cuboid that is not materialised can be answered by further aggregating a **finer**
cuboid that is — but only when the measure permits every roll-up between the cuboid and the query.

This is the single most dangerous operation in the engine. A non-additive measure answered from an
ancestor produces a number that is wrong, plausible, and derived from real data — no null, no
error, nothing missing.

> **Pitfall**
> This is also the criterion that passed **vacuously**. The exit criterion — *a non-additive
> measure is never answered from a materialised ancestor* — was green while nothing answered from
> an ancestor **at all**. Ancestor answering was designed and unreachable, so the property test
> was proving something about an empty set. A criterion whose subject does not exist cannot fail,
> and a green criterion is exactly as green either way.

The same milestone was declared complete once and retracted the same day, for the same class of
reason: the hydration path did not exist, and **every criterion passed on cells its own fixture
supplied.**

## 10.8 Serving a cube under policy

An aggregate is computed only over rows the principal may read. The consequence is that **two
principals querying the same cell may legitimately see different totals**, and that is correct
rather than a bug to design around.

The alternative — computing the total over everything and returning it to whoever asks — is a
disclosure through arithmetic. It leaks no row and it answers a question the caller was not
entitled to ask, which for a regional total over a region they cannot see is precisely the number
they wanted. It is invisible: nothing is redacted, no error is raised, and the figure looks like
every other figure.

The industry's honest position is worth stating, because SANKHYA's differs from it. SQL Server
Analysis Services solves this with recalculated visual totals — **off by default, for
performance**, with Microsoft's own documentation noting that being off *"could create a security
issue if a user can use the aggregated cell values to deduce values for attribute members to
which the user's database role does not have access."* A correct answer costs an aggregation you
cannot share, so the default is the fast wrong one with a documented warning.

Three tiers, chosen per query, with the scope in the key:

| Tier | What it is | When |
|---|---|---|
| 1 | Query-time aggregation over the already-secured session | Always correct, always available; the fallback every other tier falls back to |
| 2 | A hydration cache keyed by *(definition version, snapshot, cuboid, **scope digest**)* | When a digest can be derived |
| 3 | Materialised cuboids | Only when the cuboid's scope digest equals the caller's, or it was built unrestricted and the caller is unrestricted |

Tier 1 is the load-bearing decision. The cube's table functions plan against the tables the
session has *already* registered, and those are secured tables — the same wrapper that filters a
plain `SELECT`. **There is no cube-specific authorization code, so there is no second
implementation of the rule to disagree with the first.** Two implementations of one rule disagree
eventually, and when they disagree the cube is the one that leaks, because it answers with a
number rather than with rows somebody might notice were missing.

The **scope digest** is a hash of the principal's *effective policy predicates*, not their
identity. Two analysts with the same entitlements share a cache entry; a thousand users across six
roles produce six entries, not a thousand. It must be derived from policy and never from the
principal: deriving it from the principal would make the cache useless, and deriving it from
anything that does not fully determine visibility would make it wrong. And a cuboid is itself a
published table, so **two scopes are two tables** — separate files with separate names. A bug in
the lookup cannot serve one principal's rows to another, because those rows are not in the file
being read.

Refusing to cache is always available: if a scope cannot be digested, the right answer is tier 1,
not a guess. **A cache that is sometimes wrong is worse than one that is sometimes absent.**

### Completeness is what makes the trade-off visible

Because a filtered total is a different number from a complete one, every cube answer carries how
much of its input it saw. Five columns qualify every row:

| Column | What it tells you |
|---|---|
| `snapshot` | The table version this was computed at, so a cube figure reconciles with a relational one taken at another moment |
| `completeness` | What fraction of the input reached the cube |
| `withheld` | How many rows did not — from policy, or because they could not be placed |
| `materialised` | Whether the answer came from a stored cuboid or from base data |
| `from_cuboid` | Which one, when it did |

They are **columns, not query metadata**, and the reason is inherited from the graph engine's
truncation columns with more force: *a qualification that lives outside the rows is dropped by the
first `SELECT` that does not mention it.* For a graph that costs a truncated result read as a
complete one. For a cube the same mistake produces **a partial total read as a total**, or a
what-if figure read as fact — numbers that reconcile against nothing, in a report, with no way to
tell from the value what went wrong.

A threshold is expressible: `min_completeness=0.5`.

> **Key idea**
> **Completeness cannot be computed from what survived.** A withheld row leaves no trace in the
> result. The count comes from the filter, at the point of enforcement, or it does not exist. That
> is why a row which cannot be placed — a null key, a null measure — is *counted*, never dropped.

`materialised` is visible for a different reason: not for correctness, since the answer is
identical either way, but because *"why was this fast?"* and *"why was this slow?"* are the same
question, and an operator cannot answer either from a result that carries only numbers.

## 10.9 Ragged hierarchies

The usual workaround for a parent-child hierarchy of varying depth is to pad every branch to a
fixed number of levels by repeating the leaf. It makes the storage rectangular and it **invents
members that do not exist**, which then appear in results, in member counts and in drill-downs. A
user asked to explain why a division appears at four levels of the tree has been handed an
implementation detail as their problem.

So hierarchies are ragged natively. Two properties hold them together:

- The hierarchy is validated **acyclic at definition time**, with the cycle reported. A cycle found
  during consolidation is an unbounded traversal, and the symptom is a query that never returns
  rather than an error anybody can act on.
- **A member reachable by two paths contributes once.** Double-counting through an alternate
  roll-up is the classic silent cube defect: the total is larger than it should be, every
  constituent is correct, and the discrepancy is a plausible size.

## 10.10 Three lifetimes

Two things people want from a cube are in tension. Exploration — shape a cube, look at it, discard
it — where making it durable is paperwork nobody asked for. And a dashboard — the same cube, every
morning, fast, and demonstrably not stale — where making it ephemeral means recomputing it for
every viewer, which is the cost the cube existed to remove.

Three lifetimes, differing only in what is persisted and what maintains it, sharing one definition
model so a cube is *promoted* rather than rebuilt.

| | Definition | Materialised | Maintained by | Dies when |
|---|---|---|---|---|
| **Ephemeral** | session only | never | nothing | the session ends |
| **Declared** | persisted | never | nothing | it is dropped |
| **Maintained** | persisted + `target_lag` | yes | the maintenance thread | it is dropped |

`target_lag` is a **staleness target, not a schedule**. `MAINTAINED WITHIN 5 VERSIONS` says the
cells may be at most five commits behind, not *rebuild every five commits* — and the difference
matters in both directions, because a schedule refreshes when nothing has changed and fails to
refresh when a build takes longer than its interval.

Freshness here is **exact rather than estimated**: staleness is the distance between the cuboid's
snapshot and the table's current version, not a wall-clock guess about when a job last ran. And a
cuboid past its lag is never served as though it were fresh — the answer falls back to live
aggregation, correct and slower, with `materialised = false` saying so. The alternative, serving a
stale figure because it is quick, is how a dashboard comes to disagree with the table it is drawn
from and nobody can say by how much.

Refresh reuses the existing maintenance tick, governed by the same budget, so refreshing a cube
cannot starve compaction — nothing new schedules anything. And **refresh never takes the cube
away**: a rebuilt cuboid is a new published table at a new snapshot, the previous one stays live
until the new one commits, and is then unreferenced and reclaimed after its grace period. That is
build-then-switch, using the mechanism this warehouse already has rather than a second one shaped
like it.

> **Pitfall**
> **Ephemeral is the intended default and is not what `CREATE CUBE` does today.** A plain
> `CREATE CUBE` persists a definition, visible to every other connection. There is no syntax yet
> for asking for an ephemeral one, so on a shared server a reader who believes the design
> publishes their exploration to everybody. The reasoning stands and the mechanism does not exist;
> M14 builds the ephemeral lifetime with the mandatory expiry that an unbounded accumulation
> requires. Until then, **drop what you declare.**

## 10.11 Selection, and its budget

A cube with *n* dimensions has a lattice of cuboids — one per combination of levels, so
∏(levels + 1) of them, which passes a thousand at six dimensions with three levels each.
Materialising all is impossible; materialising none leaves an enormous amount of performance on
the floor. Choosing well is a known problem with a known answer: greedy selection under a space
budget, within a constant factor of optimal, informed here by the query log rather than by a guess.

**A human cannot do this job.** Nobody can look at a thousand-node lattice and pick the forty that
pay for themselves, and the attempt produces a cube tuned for the queries somebody imagined rather
than the ones being run.

The budget is a real setting somebody has to choose, in rows, and setting it to zero buys nothing
from automatic selection — the base cuboid and any pinned shape are still built, because neither
is bought from the budget.

The query log records a **shape**, and nothing else: which cube, and which dimensions were grouped
by. There is nowhere in it to put a member, a predicate or a principal, which is deliberate — *a
query log is the kind of thing that quietly becomes a record of who asked what about whom, and
this one cannot.*

## 10.12 What is not there

| Gap | Note |
|---|---|
| **Cuboids are pre-built only for the unrestricted scope** | A maintained cube is built with nobody logged in, so the refresher has no principal. Background refresh therefore helps dashboards and service accounts and **does nothing for a restricted analyst**, whose cuboids are built by their own queries. Pre-building named scopes is a decision nobody has made and is not taken by implication |
| The ephemeral lifetime | M14 — see §10.10 |
| A definition as a row in a system table | Definitions live as JSON files under the warehouse's `_cubes/`. The catalogue-backed form waits on the catalogue proper |
| The hydration trigger is crude | The statement text is scanned for cube function names to decide what to hydrate. Deliberately crude: a false positive costs a cache lookup, a false negative costs a query that fails to resolve a cube it named |
| Pivot and hierarchy drill as SQL functions | Present in the library layer; not registered as table functions |
| MDX | Deliberately not planned. [ADR-0007](../../adr/0007-the-cube-model.md) |
| Automatic rewriting of arbitrary queries onto cuboids | Explicit addressing only. Revisited after 1.0 |

Two soak results bound what is known about behaviour over time. A 44-minute judged run — the first
soak to exercise a cube — passed with all seven measures steady over 157 rounds and 2.41 billion
rows scanned, with resident memory settling at 2.2 GB against a 779 MB baseline without cubes;
roughly 1.5 GB of that plateau is the cell structure retaining every raw contribution. A later
59-minute run, **the first in which a cube was served from storage**, passed with 196 rounds, 3.01
billion rows read, the cube answered 49 times, 11.66 GB reclaimed over 1,651 ticks, and resident
memory closing at 2.0 GB — *more work, a longer run, and less memory.*

## 10.13 What it costs

An honest model states its bill, and this one has five line items.

- **A virtual cube is slower than a materialised one**, sometimes by a lot. Adaptive selection
  narrows the gap and does not close it, and a query for a cuboid nobody has asked for before pays
  full price.
- **Adaptive materialisation spends resources on its own**, in the background, on work no user
  asked for. It is bounded by an operator's budget, and the budget is a real number somebody has to
  choose.
- **Declaring aggregation rules is work**, at definition time, on somebody who would rather be
  querying. It is the price of not shipping plausible wrong numbers.
- **Two principals seeing different totals will be reported as a bug** at least once. The
  documentation has to say why it is not, and the completeness measure has to make it visible
  rather than mysterious.
- **A cube is never faster than the policy allows.** A principal whose scope no aggregate was built
  for pays for a real aggregation. That is the correct price, and it is visible rather than paid in
  a silently wrong number.
