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

### Virtual by default; materialisation is a decision somebody makes

A cube holds no data. It is a declaration over published tables, and a cell exists because
rows exist — exactly the rule the graph engine follows, and for the same reason.

The alternative is the classical one: precompute the consolidation, store it, serve queries
from the store. It is faster and it reintroduces the thing this system exists to remove. A
materialised aggregate is **a second copy that can disagree with its source**, and the
disagreement is not detectable from the copy. Every product in this category has a "the cube
is stale" failure mode, and the reason is architectural rather than incidental.

So materialisation is available, per level, **declared explicitly**, carrying a staleness
contract, and never automatic. `FR-QUERY-27` already sets the rule that makes incremental
refresh safe — only aggregates forming a commutative monoid may declare it — and that rule
applies here unchanged.

This is a real performance trade and it is taken knowingly. It is the same trade as
deterministic summation: slower, and reconcilable.

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

## Consequences, stated as costs

- **A virtual cube is slower than a materialised one**, sometimes by a lot. That is the trade,
  and the escape hatch is explicit per-level materialisation rather than a default.
- **Declaring aggregation rules is work**, and it is work at definition time on somebody who
  would rather be querying. It is the price of not shipping plausible wrong numbers.
- **Two principals seeing different totals will be reported as a bug** at least once. The
  documentation has to say why it is not, and the completeness measure has to make it visible
  rather than mysterious.
- **Write-back is a separate overlay**, versioned, never touching published data, and a query
  states whether one was applied. Planning and what-if analysis need it; the published facts
  are not the place for it.

## Revisit if

A measured workload shows virtual consolidation is the bottleneck for a large fraction of
queries rather than a few. The answer then is broader default materialisation with the
staleness contract made prominent — not silent precomputation.
