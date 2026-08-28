<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# ADR-0012 — Open capabilities: what a standing artefact must declare

**Status:** Proposed · **Date:** 2026-08-28 · **Milestone:** M7 and beyond
**Builds on:** [ADR-0008](0008-serving-cubes-under-policy.md), [ADR-0009](0009-the-cube-lifecycle.md), [ADR-0011](0011-sdaf-declared-dependencies.md)

## Context

The direction, in the owner's words:

> *A user may create a cuboid and then mark it maintained; when that is done the system starts
> maintaining it even when the user is not logged in. At some point we will need an
> architecture so that it doesn't cause explosion in cuboid population. A query supplied by
> anyone can construct and maintain a cube, using a builtin or custom aggregation. Let us
> build Sankhya more open in terms of capabilities.*

Three capabilities, one shape: **something a user declares outlives the session that declared
it, and the system does work on its behalf afterwards.** A maintained cuboid, a cube built
from somebody's query, an aggregation somebody wrote.

Openness of this kind fails in three specific ways, and each has a name.

## The three failures, and what this system already does about them

### 1. A standing artefact carries authority — *"whose rows is it made of?"*

The moment something is maintained without its author present, it is being computed **on
behalf of somebody**. SQL has had this argument for thirty years and calls it
definer's rights versus invoker's rights: a `SECURITY DEFINER` procedure runs with its
author's authority, and the classic failure is that the author leaves, or their entitlements
narrow, and the artefact keeps handing out data under permissions nobody holds any more.

**This system gets a better answer for free, and it is worth noticing why.**
`Guard::scope_digest` hashes what a guard *permits* — tenant, table, action, row filter, column
masks — and deliberately **not** who is asking. A materialised cuboid is keyed by that digest.

So a cuboid is tied to an **entitlement set**, not to a person. If the policy changes, the
digest changes, the key no longer matches, and the old cuboid is simply never found. There is
no stale-permission window to close because there is no lookup that can succeed.

That is the property `SECURITY DEFINER` lacks, and it arrived by accident of a decision made
for caching reasons. It should now be treated as **load-bearing**: any future change that
makes the digest coarser — hashing a role name instead of the predicates, say — silently
reopens the hole.

Today's refresher sidesteps the question entirely by building only the unrestricted cuboid,
which may serve only an unrestricted caller. That is safe and limited. Pre-building a
restricted scope is the moment the standing-grant question becomes live, and the digest is the
answer to it.

### 2. Population — *"how many of these are there?"*

The count multiplies:

```
cuboids  =  cubes  ×  cuboids per cube  ×  scopes  ×  live snapshots
```

Every factor is something a user can increase without meaning to. A hundred cubes, ten cuboids
each, six entitlement scopes and five retained snapshots is thirty thousand tables — and each
one was individually reasonable.

Three bounds, and the third does not exist yet:

- **Cuboids per cube** is already bounded: §11.6's greedy selection spends an operator budget.
- **Scopes** are bounded by the digest being over entitlement *sets* rather than people: a
  thousand analysts across six roles produce six, not a thousand. This is why the digest
  excludes the subject, and the second reason that decision is load-bearing.
- **Snapshots are not bounded at all.** A cuboid at an old snapshot is never read — its key
  cannot match — so it is garbage the moment the table advances. And nothing collects it: the
  orphan sweep finds unreferenced *files within a table*, and a superseded cuboid is a whole
  table that no log mentions. **This is a leak, and it is the same shape as the one that filled
  a disk in the soak: a thing that produces garbage and nothing that reclaims it.**

  The fix belongs with the machinery that already does this work — retire a cuboid table whose
  snapshot is further behind than any live query could ask for, on the maintenance tick, after
  a grace period, exactly as compaction inputs are retired.

### 3. Cost — *"what does this cost, and who agreed to it?"*

A user declaring a cube commits an operator to storage and to maintenance time, and the
declaration is cheap while the consequence is not. This is why
[ADR-0009](0009-the-cube-lifecycle.md) makes **Ephemeral the default** and persisting the
deliberate act, and why a Declared cube materialises nothing.

Marking a cube maintained should be a **grant**, not a property a user can set on their own
behalf — for the same reason nobody can grant themselves storage quota. The mechanism is not
decided here; that it is a grant is.

## Decision

**A capability may be open in proportion to what it declares.** Everything a standing artefact
needs from the system is stated up front, and the statement is what the system checks, keys,
bounds and revokes.

That is the single rule behind decisions already taken, and it is worth naming so the next
capability is designed the same way rather than argued from scratch:

| Capability | What it declares | What the declaration buys |
|---|---|---|
| A cube | fact table, dimensions, measures, rules | validation at startup, refusal of illegal roll-ups |
| A maintained cube | `target_lag` | a staleness bound that is checked, not estimated |
| An SDAF | its dependencies, its `merge` | a cache key, a policy check, a composability answer |
| A cube from a query | the query and its dependencies | the same, one level up |

An artefact that cannot say what it needs cannot be cached correctly, cannot be checked against
policy, and cannot be bounded — so it cannot be maintained on somebody's behalf. **Openness
comes from a well-specified contract, not from the absence of one.** An open `execute`, an
ambient session, an undeclared dependency: each of these is the *absence* of a contract, and
each makes the artefact unmaintainable in the precise sense that the system can no longer
reason about it.

### A cube from a query

The generalisation the owner asked for is small under this rule. A cube's `fact_table` becomes
a **declared query** rather than a name, and everything else is unchanged:

- Its dependencies are the tables the query reads, resolved through the caller's guard, exactly
  as an SDAF's are ([ADR-0011](0011-sdaf-declared-dependencies.md)).
- Its snapshot is the newest of its dependencies' snapshots, so the materialisation key keeps
  working with no new invalidation protocol.
- It must be **deterministic** — no `now()`, no `random()`, no ordering-dependent limit —
  because a cuboid built from a non-deterministic query is a cache of one arbitrary answer.

The refusals write themselves, and they are the same refusals as everywhere else.

## Consequences

**The scope digest is now three things at once**: a cache key, the population bound, and the
answer to the standing-grant problem. It deserves the scrutiny of a security control, because
it is one. Any change that makes it coarser must be argued, not merged.

**Cuboid retirement has to exist before named scopes do.** Multiplying an uncollected
population by the number of pre-built scopes is how a leak becomes an outage. That ordering is
a decision, not a preference.

**Marking a cube maintained becomes an authorization question**, and the answer will annoy
somebody. That is preferable to a warehouse where any user can commit the operator to
unbounded background work.

## What this does not decide

The grant mechanism for `maintained`; where a cube-from-query's SQL is stored and versioned;
and whether an operator budget is per-cube, per-tenant or global. Each wants a measurement or
an owner decision, and none of them changes the rule above.
