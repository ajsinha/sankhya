<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# ADR-0014 — Materialized views, and whether they are a cube lifetime

**Status:** Proposed · **Date:** 2026-08-28 · **Milestone:** M8 or later
**Builds on:** [ADR-0009](0009-the-cube-lifecycle.md), [ADR-0012](0012-open-capabilities.md)

## Context

A review of all 55 crates on 2026-08-28 found ten holding a single line of source. Nine were
resolved directly — six adopted with a dated milestone, three deleted. `sankhya-mv`
*("Materialized view definition and incremental refresh")* is the one that could not be
resolved that way, and the reason is worth recording rather than deciding by reflex.

**The tempting answer is that M7 already built this.** It is nearly right, which is what makes
it dangerous.

## What M7 actually built

A maintained cube is a materialized view in every respect that costs engineering effort:

| Machinery | Where it lives |
|---|---|
| A declared definition, persisted and versioned | `cube::catalogue` |
| Materialisation keyed by *(definition, snapshot, scope, shape)* | `cube::materialise::Key` |
| A staleness target that is checked rather than estimated | `maintenance::cuboid::{lag, within_target}` |
| Refresh without a caller, on the maintenance tick | `server::refresh_maintained_cubes` |
| Reclamation of superseded results | `maintenance::cuboid::retire_superseded` |
| Serving under policy, keyed by entitlement rather than by person | `Guard::scope_digest` |
| Completeness carried from the filter to the presentation | `cube::complete` |

None of that is cube-specific. It is the machinery of *any* declared, maintained, derived
result — and [ADR-0012](0012-open-capabilities.md) already generalised the input side: a cube's
`fact_table` becomes a **declared query** rather than a name, with its dependencies resolved
through the caller's guard and its snapshot the newest of theirs.

## Why that does not settle it

**A materialized view is not necessarily an aggregate.** A cube is a grid: dimensions, measures,
and reduction rules that say how a measure may compose along each axis. An MV can be a join, a
filter, a projection, a window — a result with no measures and no grain, for which
`Rule::composes`, the cuboid lattice, roll-up and drill-down are all meaningless.

So an MV is **not** a cube with a query for a fact table. The relationship is the other way
round: a maintained cube is one *shape* of maintained query, and the cube's extra structure —
the lattice, additivity, ancestor answerability — is what a general MV does not have.

Three consequences follow, and each is a real design question rather than a detail:

1. **Selection has nothing to select.** §11.6's greedy selection spends a budget across a
   lattice of cuboids. A general MV has one shape, so there is nothing to choose between and
   the query log has nothing to weigh.
2. **Incremental refresh is a different problem.** A cube refreshes by rehydrating from its
   fact table at the new snapshot. Incremental maintenance of an arbitrary view — the classic
   delta rules for joins and aggregates — is a body of work M7 did not do and does not need.
   The crate's own one-line description says *"incremental refresh"*, which is precisely the
   part that is not built.
3. **Answering from an ancestor has no analogue.** Reading a coarser cuboid and rolling up is
   sound only because additivity says it is. There is no equivalent for a filtered join.

## Options

**A. Delete `sankhya-mv`; MVs are a lifetime of a declared query.** Generalise ADR-0012's
declared-query cube so that a definition with no dimensions and no measures is simply a
maintained query. Reuses everything above. Incremental refresh stays out of scope — such a view
is recomputed at each snapshot, which the keying already makes correct.

**B. Keep `sankhya-mv` for the part that is genuinely different**: incremental maintenance,
delta rules, and the algebra that decides which views can be maintained incrementally at all.
It would depend on the shared machinery rather than restate it.

**C. Delete it and record MVs as out of scope.** SANKHYA has cubes and it has tables; a user
wanting a maintained join writes a maintained cube over a declared query, or a table.

## Recommendation

**Option A, with B revisited only when incremental refresh is actually wanted.**

The reason is the pattern this repository keeps finding. Every candidate design for `mv` reuses
the catalogue, the key, the target lag, the tick and the reclamation — and the failure mode of
building it in a separate crate is not that it will not work. It is that it will grow a second
refresh loop, a second staleness rule and a second reclamation path, and the two will drift.
This warehouse has already paid for that twice: a soak with its own writer produced flat
warehouses that reported `PASS` against a layout the product does not produce, and an ingest
pipeline with its own writer produced tables with no partition columns. **A second
implementation of maintained-derived-data is the same defect one level up.**

Incremental refresh is the only genuinely separate body of work, and it is not wanted yet.
Building the crate now to hold it would be reserving a name for a capability nobody has asked
for — which is what the other nine empty crates were.

## Consequences

Deciding this needs one thing that did not exist when this was written: **the declared-query
cube from ADR-0012**. Until a cube could be defined over a query rather than a table name,
option A could not be built and option B had nothing to depend on. So this ADR stayed
*Proposed*, and `sankhya-mv` stayed empty and **listed with a reason** rather than deleted on a
guess.

> **Built 2026-09-02.** `CREATE CUBE <name> FROM ( <query> )` exists, and the rule that makes it
> safe is ADR-0012's: the query is planned once under the caller's own guard, and the tables it
> is found to read are recorded with the definition. Those tables are what the cube is
> authorized against, what its cache is keyed on, and what makes it stale — so nothing else
> about a cube changed. A query that cannot be planned, and a query whose answer can move on its
> own, are both refused at declaration.
>
> The prerequisite is therefore met and option A is now buildable. What remains of it is the
> smaller half: a definition with **no dimensions and no measures** — a maintained query rather
> than a maintained cube — which today `validate` refuses by name, because a cube with neither
> is a table. That refusal has to become a second lifetime rather than an error, and until it
> does, this ADR stays *Proposed* for the part that is genuinely undecided.

That is the deliberate difference from the nine crates resolved beside it. Those had no design
question left — six had a milestone that was simply unwritten, three duplicated something that
already existed. This one has a real question, and the honest state for it is open.

## What this does not decide

Whether incremental view maintenance is ever in scope, and if so under which algebra. Whether a
maintained query is authorized like a maintained cube — [ADR-0012](0012-open-capabilities.md)
says marking a cube maintained should be a grant, and a maintained arbitrary query is a larger
commitment of an operator's storage, not a smaller one.
