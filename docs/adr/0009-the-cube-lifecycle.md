<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# ADR-0009 — The cube lifecycle: three lifetimes, one model

**Status:** Proposed · **Date:** 2026-08-28 · **Milestone:** M7
**Builds on:** [ADR-0007](0007-the-cube-model.md), [ADR-0008](0008-serving-cubes-under-policy.md)

## Context

Two things people want from a cube are in tension, and a design that serves only one of them
is only half a product.

> *Sometimes a user may ask for an on-the-fly cube and may not need to persist it, while at
> other times he may want a persisted cube with an SLA on maintaining its freshness in an
> automated way.*

The first is exploration: shape a cube, look at it, discard it. Making that durable is
paperwork nobody asked for, and a warehouse accumulating a definition per abandoned question
is a catalogue nobody can read.

The second is a dashboard: the same cube, every morning, fast, and demonstrably not stale.
Making *that* ephemeral means recomputing it for every viewer, which is the cost the cube
existed to remove.

## What other systems do

**Snowflake dynamic tables** take a `TARGET_LAG` and the documentation is careful about what
it means: *a staleness target, not a refresh interval*. `TARGET_LAG = '10 minutes'` says the
data should be no more than ten minutes old — not that a job runs every ten minutes. The
difference matters in both directions: a schedule refreshes when nothing has changed, and
fails to refresh when a build takes longer than its interval.

**BigQuery materialized views** express the same idea as `max_staleness`.

**SSAS proactive caching** answers "how do you refresh without an outage": on a change
notification it builds a **new** MOLAP cache while serving the old one, and switches queries
over once the new one is complete. It also has a *silence interval* — wait a little to see
whether more changes are coming, rather than rebuilding once per arriving row.

**Kylin** separates the operations explicitly: build a segment, refresh a segment, merge
segments, and `PURGE` to clear a cube's segments without dropping the underlying storage.

## Decision

**Three lifetimes, differing only in what is persisted and what maintains it.** They share one
definition model, so a cube is promoted or demoted rather than rebuilt.

| | Definition | Materialised | Maintained by | Dies when |
|---|---|---|---|---|
| **Ephemeral** | session only | never | nothing | the session ends |
| **Declared** | `_cubes/*.json` | never | nothing | it is dropped |
| **Maintained** | `_cubes/*.json` + `target_lag` | yes | the maintenance thread | it is dropped |

### Ephemeral — the on-the-fly cube

Declared against a session and never written. It is a name, a validated definition, and
whatever the hydration cache holds for it under the caller's scope. When the session ends, all
of it goes.

**This is the default.** A user exploring should not have to decide whether their question
deserves to be durable, and a warehouse should not accumulate a definition per abandoned
question. Persisting is the deliberate act, which is the right way round: the reversible
option is the one you get without asking.

### Declared — persisted, and computed when asked

The definition is written to the catalogue and outlives the process. Nothing is
pre-computed, so it costs one JSON file, and a query pays for hydration under its own scope
per ADR-0008.

The right choice for a cube that is asked about occasionally, and for **every cube whose
callers have different entitlements** — because a materialised cuboid built under one scope
cannot serve another, so materialising a cube read by twenty differently-restricted analysts
mostly produces cuboids nobody may use.

### Maintained — persisted, materialised, and held to a stated lag

`target_lag` is a **staleness target, not a schedule**, following Snowflake's framing because
the alternative is worse in both directions.

**Freshness here is exact rather than estimated.** A materialised cuboid is already keyed by
*(definition version, snapshot, cuboid)* per `FR-QUERY-20`. Staleness is the distance between
that snapshot and the table's current version — not a wall-clock guess about when a job last
ran. The same quantity is already in the provenance columns every cube answer carries, so a
caller can see the lag of the number they were given.

**Refresh reuses the maintenance thread.** It has a duty cycle, configurable cadences and live
reconfiguration, and a cube refresh is another job in the tick — governed by the same budget,
so refreshing a cube cannot starve compaction. Nothing new schedules anything.

**Refresh never takes the cube away.** A rebuilt cuboid is a new published table at a new
snapshot; the previous one stays live until the new one commits, and is then unreferenced and
reclaimed by retirement after its grace period. This is SSAS's build-then-switch, and it is
the mechanism this warehouse already has rather than a second one shaped like it.

**A cuboid past its lag is not served as though it were fresh.** The answer falls back to live
aggregation — correct and slower — with `materialised = false` in the provenance saying so.
The alternative, serving a stale figure because it is quick, is how a dashboard comes to
disagree with the table it is drawn from and nobody can say by how much.

### Retirement

Dropping a cube deletes its definition and leaves its materialised cuboids unreferenced.
They are ordinary published tables, so **retirement and the orphan sweep reclaim them** — after
the grace period, which is exactly the protection a reader mid-query needs.

Kylin's separation is worth keeping: *purge* a cube's materialisations while keeping the
definition (stop paying for it, keep the model) is a different operation from *dropping* it.
Purging is demotion from Maintained to Declared, which the shared model makes a one-field
change rather than a rebuild.

## Promotion and demotion

Because all three share one definition, the transitions are small:

- **Ephemeral → Declared:** write the definition. Nothing recomputes.
- **Declared → Maintained:** set `target_lag`. The next maintenance tick sees a cuboid that
  does not exist and builds it.
- **Maintained → Declared (purge):** clear `target_lag`. Existing cuboids become unreferenced
  and are reclaimed on the usual grace period.
- **Any → dropped:** remove the definition; the same reclamation follows.

## Consequences

**Scope decides whether materialisation is worth anything.** Per ADR-0008 a cuboid built under
one scope cannot serve another, so a Maintained cube is most valuable where callers share
entitlements — a dashboard read by a service account, or a tenant-scoped cube. Where they do
not, Declared is the honest choice, and the system should say so rather than let somebody
materialise something no query may use.

**A lag that cannot be met must be visible.** If a cube's build takes longer than its
`target_lag`, the target is not achievable and saying nothing turns an SLA into a decoration.
That is a reportable condition, in the same shape as the fan-out alarm: state the measured
build time, state the target, and name the thing to change.

**The silence interval is worth stealing.** Rebuilding once per commit on a table under
continuous ingest would spend the entire maintenance budget on one cube. A cube should wait
for quiet, and `target_lag` is the budget for that waiting.

## What this does not decide

Which cuboids a Maintained cube materialises. §11.6's greedy selection under an operator
budget needs the recorded query log it already asks for, and choosing before that signal
exists is the error M7 made once already.
