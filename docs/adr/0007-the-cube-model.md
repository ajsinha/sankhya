<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# ADR-0007 — Cubes are declared views, not a second store

**Status:** Accepted · **Date:** 2026-08-27 · **Milestone:** M7
**Implements:** `FR-CUBE-01` … `FR-CUBE-19`
**Owner directive, 2026-08-27:** *"we need to build native capability of data cubing slice
and dice on demand rollup and consolidation … that is a key space where sankhya will make
difference"*

## Context

Multidimensional analysis is missing. `FR-QUERY-14` requires SQL's `GROUP BY CUBE` and
`ROLLUP`, and those are grouping constructs: they enumerate combinations of the columns you
name in the statement. There is no dimension, no hierarchy, no declared measure, no
consolidation rule and no notion of a member. Writing `GROUP BY ROLLUP(year, quarter, month)`
does not make a time dimension; it makes one query that knows three column names.

The gap matters because the operations people actually perform — take this slice, dice it by
those two dimensions, roll it up that hierarchy, drill into the outlier — are not a sequence
of unrelated `GROUP BY` statements. They are navigation of one structure, and a system that
cannot represent the structure makes the user reconstruct it in the client, which is how the
work ends up in a spreadsheet that nobody can reconcile.

## Why this system, specifically

Three properties SANKHYA already has are the three a cube engine most needs, and the
combination is not available in one process anywhere else.

**Parent-child hierarchies are graphs.** A ragged organisation tree, a chart of accounts with
alternate roll-ups, a legal-entity structure where a subsidiary appears under two parents —
these are what `sankhya-graph-algo` already traverses, with bounded traversal and reported
truncation. A consolidation path *is* a traversal. Every other cube engine reimplements
hierarchy walking as a special case; here it is the general case that already exists.

**Consolidation is a large floating-point reduction.** `deterministic_sum` exists because
`REQUIREMENTS` §432 records what non-associative parallel reduction costs: *"the same
aggregate query returns different values run to run"*. A roll-up is that reduction at its
worst — many partials, widely varying magnitudes, summed across a hierarchy. Whether two runs
of the same consolidation tie out is the first question a finance function asks and the one
most products answer with a shrug.

**Every table already carries a date axis.** ADR-0004 put `sank_data_date` on everything, so
a time dimension exists before anybody declares one.

## Decision

### Both modes are first class, because snapshot identity removes the reason to fear one

*Revised 2026-08-27, same day, on owner direction: "cubes can be materialized and on demand
both all controlled via config and preferences". The first version of this section said
materialisation must be **"never automatic or implicit"**, on the grounds that a precomputed
aggregate is a second copy that can disagree with its source. That reasoning is right about
every other product in this category and wrong about this one, for a reason the first version
did not notice.*

A cube holds no data of its own: it is a declaration over published tables, and a cell exists
because rows exist. That much is unchanged.

What changed is the status of precomputation. The classical objection is that a materialised
aggregate can go stale, and that the staleness is not detectable from the copy — which is why
every product here has a "the cube is stale" failure mode. But `FR-QUERY-20` already records
the property that dissolves it:

> Because files are immutable and every key embeds a snapshot identifier, **a new commit
> cannot produce a stale hit** — the key simply misses. No invalidation protocol is required.

A materialised cuboid keyed by *(cube definition version, snapshot identifier, cuboid
specification)* **cannot be stale**. It either matches the snapshot the query is reading at,
or it is a miss and the answer is computed. There is no invalidation protocol to get wrong,
no time-to-live to tune, and no window during which a stale answer is served.

That changes what materialisation *is*. It is not a second copy of the truth. It is a
**cache**, and being wrong about what to cache costs latency rather than correctness.

**Amended 2026-08-27, while building `sankhya-cube`.** That guarantee has a precondition the
first version of this section did not state: the definition version must actually *move* when
the definition does. A version somebody edits by hand does not. It is a field, a review that
has to catch it when the field is missed, and then a cuboid built under the old meaning of a
measure being served against the new one — a real number, computed correctly, from a
definition that no longer exists. The snapshot half of the key is derived from the log and
cannot be forgotten; the definition half had no such protection.

So it is derived too. `Cube::version` is a fingerprint of the validated content, computed at
validation and available no other way. Change a measure's rule and the key changes; reformat
the file and it does not. There is no field to forget.

Two properties of that fingerprint are load-bearing rather than incidental. Fields are
**length-prefixed**, or a dimension named `ab` on column `c` hashes identically to one named
`a` on column `bc` and two different cubes share a materialisation key. And **level order is
preserved while roll-up edges are sorted**, because levels are coarse-to-fine and reordering
them reorders a drill-down, whereas a hierarchy is a set of edges and rebuilding every cuboid
because somebody sorted a config file is a cost against no risk.

The fingerprint is `FNV-1a`, which defends against accident and not against a constructed
collision. That is written down in the module too: a hash in a cache key invites the
cryptographic assumption, and here the assumption is wrong.

**So the mode is a per-cuboid decision, and it is configurable at three levels:**

| Level | Who sets it | What it controls |
|---|---|---|
| **Cube definition** | Whoever models the cube | Cuboids *pinned* materialised — a judgement that some roll-up is always worth having |
| **Server configuration** | The operator | The budget: space, refresh concurrency, whether adaptive selection runs at all |
| **Session preference** | The caller | Whether this query may spend time materialising, and whether it may read materialised cuboids |

The last one deserves a note, because with snapshot keying it is not a correctness control —
a materialised answer and a computed one are the same answer. It is a cost control, with one
genuine exception: turning materialisation **off** for a session is how you *prove* the two
agree. An audit run that reads only base data and reconciles against the ordinary path is a
real capability, and it exists because the preference exists.

### Selection may be automatic; semantics may not

A cube with *n* dimensions has a lattice of cuboids — one per combination of levels, so
∏(levels + 1) of them, which passes a thousand at six dimensions with three levels each.
Materialising all is impossible. Materialising none leaves an enormous amount of performance
on the floor. Choosing well is a known problem with a known answer: greedy selection under a
space budget, which is within a constant factor of optimal, informed here by the query log
rather than by a guess about what people will ask.

**A human cannot do this job.** Nobody can look at a thousand-node lattice and pick the
forty that pay for themselves, and the attempt produces a cube tuned for the queries somebody
imagined rather than the ones being run.

So the line is not "automatic versus declared". It is:

> **What may be automatic: anything whose being wrong costs latency.**
> **What may not: anything whose being wrong changes an answer.**

Automatic: which cuboids to materialise, when, in what order, how long to keep them, and
whether to answer a given query from a materialised ancestor.

Never automatic: aggregation rules, hierarchy definitions, what a measure means, and the
completeness contract. Those change answers, and a system that infers them produces numbers
nobody declared.

### The property that makes all of it safe, and that has to be tested

**A cube returns bit-identical answers whether or not anything is materialised.**

That is the claim, it is stronger than it looks, and almost nothing else in this category can
make it — because it requires the underlying reduction to be deterministic, which
`FR-QUERY-10` and `deterministic_sum` already provide. Without it, "materialised" and
"computed" are two answers that are *close*, and close is the state this whole system is
built to avoid: too small to notice and too large to reconcile.

With it, materialisation has **zero semantic content**. It is purely a performance decision,
which is precisely what makes it safe to automate.

It is an exit criterion for `M7`, tested by running every query both ways and comparing bits.

### Answering from an ancestor is where the wrong answers live

A query for a cuboid that is not materialised can be answered by further aggregating a
**finer** cuboid that is — but only when the measure is additive along *every dimension being
further rolled up*.

This is the single most dangerous operation in the engine. A non-additive measure answered
from an ancestor produces a number that is wrong, plausible, and derived from real data — no
null, no error, nothing missing. It reconciles against nothing, because nobody reconciles a
subtotal.

**Corrected 2026-08-27, while building `sankhya-cube-algo`.** An earlier version of this
paragraph said a semi-additive measure was *worse*, "correct along most dimensions and wrong
along exactly one". That is a description of what a naive implementation does, not of the
rule, and taking it literally cost a set of tests that refused valid roll-ups — which would
have sent every balance query to base data for no reason at all.

The precise statement separates two things the loose one ran together:

- **Additive** is about the *operator*: may this measure be **summed** along this axis?
- **Composable** is about *permission*: can the whole be built from partial aggregates along
  this axis at all?

A closing balance is not additive over time and it **is** composable over time, because
`last(last(a, b), c) == last(a, b, c)` given an order. Rolling it up across time is perfectly
valid — *provided the executor applies `last` and not `sum`*. `Sum`, `Min`, `Max`, `First`
and `Last` compose; `Mean` does not, because an average of averages is an average only when
every group is the same size and groups are never the same size; and a distinct count or a
ratio composes along nothing.

So the danger of a semi-additive measure is **the operator, not the axis** — and it lives in
the executor, which must honour the declared rule per dimension rather than summing. The
planner's question is only whether the roll-up is possible.

This is why additivity is **declared rather than inferred**, and why the declaration is
per dimension rather than per measure: at query time the planner asks "may this measure be
combined along *this* axis, and with what?" and needs an answer a person committed to, not
one derived from a column's type or name.

### A materialised cuboid is an ordinary published table

Not a proprietary cube file. It is written through the same writer, into the same warehouse,
with the same log, and it is readable by Spark like anything else. The open-storage
commitment does not get an exception for the fast path — and a cache that external tools can
read is a cache an operator can inspect when they do not believe it.

### Every measure declares its aggregation rule, per dimension

**A measure with no declared rule is refused at definition time.** Not defaulted to `SUM`.

This is the single most important decision in the document, because the default is wrong for
an entire class of measures and wrong invisibly. A closing balance summed across time gives
you the sum of twelve month-end balances, which is not a number that means anything and which
looks exactly like a number that does. A rate averaged across regions without weighting is
wrong in a way no test catches, because nothing is missing and nothing is null.

Three kinds:

| Kind | Behaviour | Example |
|---|---|---|
| **Additive** | Sums over every dimension | Transaction amount, quantity |
| **Semi-additive** | Sums over some dimensions, something else over others | A balance: sum across accounts, **last** across time |
| **Non-additive** | Cannot be derived from its own children at all | A ratio, a distinct count, a percentile |

A plan applying an additive roll-up to a non-additive measure is **rejected at planning
time**, naming the measure and the dimension. `FR-QUERY-12` already states this rule for
precomputed measures; a cube is simply where it is violated most often.

### Ragged hierarchies are supported natively, not padded

The usual workaround for a parent-child hierarchy of varying depth is to pad every branch to
a fixed number of levels by repeating the leaf. It makes the storage rectangular and it
**invents members that do not exist**, which then appear in results, in member counts and in
drill-downs. A user asked to explain why a division appears at four levels of the tree has
been handed our implementation detail as their problem.

The hierarchy is validated acyclic at definition time, with the cycle reported. A cycle found
during consolidation is an unbounded traversal, and the symptom is a query that never returns
rather than an error anybody can act on.

**A member reachable by two paths contributes once.** Double-counting through an alternate
roll-up is the classic silent cube defect: the total is larger than it should be, every
constituent is correct, and the discrepancy is a plausible size.

### An aggregate is computed only over rows the principal may read

The consequence is that **two principals querying the same cell may legitimately see
different totals**, and that is correct rather than a bug to be designed around.

The alternative — computing the total over everything and returning it to whoever asks — is a
disclosure through arithmetic. It leaks no row and it answers a question the caller was not
entitled to ask, which for a regional total over a region they cannot see is precisely the
number they wanted. It is invisible: nothing is redacted, no error is raised, and the figure
looks like every other figure.

Because a filtered total is a different number from a complete one, it carries a completeness
measure per `FR-QUERY-13`. A total the policy reduced must be distinguishable from a total it
did not.

### SQL, and deliberately not MDX

MDX is the traditional language for this and it is not planned. It is a large language with
subtle semantics — the implicit current member, the `.CurrentMember` context, solve orders —
and implementing it adequately is a multi-month project. It buys compatibility with a client
population that is small and shrinking, and which this system does not target.

Cubes are addressed from SQL: the system's primary surface, the one `FR-API-02` calls the
highest-adoption-value surface in the product, and the one every tool already speaks.

### Three crates, mirroring the graph

The graph engine is three crates and the split has earned itself: `sankhya-graph-algo` has
**zero dependencies**, which is what makes its property tests fast enough to run thousands of
cases on every build, and what keeps storage concerns out of the algorithms.

Cubing has the same shape and gets the same treatment. `sankhya-cube-algo` exists as of
2026-08-27 and holds the two decisions above; the other two are planned, named here so the
layering was decided before the first line was written rather than discovered afterwards:

| Crate (planned) | Layer | What is in it | Depends on |
|---|---|---|---|
| `sankhya-cube-algo` | 1 | The lattice, the additivity algebra, the "may C be answered from D?" predicate, cuboid selection under a budget, cell addressing, consolidation ordering | **Nothing** |
| **sankhya-cube** | 3 | Resolving a definition against published tables, member sets, execution over Arrow, reading and writing materialised cuboids, the budget manager | read path, math, graph, authz |
| **sankhya-cube-sql** | 4 | The SQL surface | DataFusion |

The layer-1 crate is the one that matters. The additivity algebra and the ancestor-answering
predicate are where wrong answers come from, they are pure functions of a declaration, and
with no dependencies they can be exhausted by property test rather than sampled by example.

**Materialisation storage gets no crate**, because a materialised cuboid is a published table
and that machinery exists.

## Consequences, stated as costs

- **A virtual cube is slower than a materialised one**, sometimes by a lot. Adaptive selection
  narrows the gap and does not close it, and a query for a cuboid nobody has asked for before
  pays full price.
- **Adaptive materialisation spends resources on its own**, in the background, on work no
  user asked for. It is bounded by an operator's budget and the budget is a real setting
  somebody has to choose, not a number that can be right by default.
- **Declaring aggregation rules is work**, and it is work at definition time on somebody who
  would rather be querying. It is the price of not shipping plausible wrong numbers.
- **Two principals seeing different totals will be reported as a bug** at least once. The
  documentation has to say why it is not, and the completeness measure has to make it visible
  rather than mysterious.
- **Write-back is a separate overlay**, versioned, never touching published data, and a query
  states whether one was applied. Planning and what-if analysis need it; the published facts
  are not the place for it.

## Revisit if

**The bit-identical property cannot be held.** It is load-bearing: it is what makes
materialisation a cache rather than a second source of truth, and therefore what makes
automatic selection safe. If some measure or some path cannot be made to agree bit for bit
between the materialised and computed routes, that measure must be excluded from
materialisation entirely rather than the property being weakened to "agrees closely".

**Snapshot keying stops being sufficient.** The argument above rests on `FR-QUERY-20`: every
key embeds a snapshot identifier, so a stale hit is impossible. Anything that introduces a
mutable key into the materialisation path — a cuboid keyed by "latest" rather than by a
snapshot — reintroduces the staleness problem in full, and the first version of this ADR is
right again.
