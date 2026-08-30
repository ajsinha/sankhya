<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# ADR-0008 — Serving cubes under policy: the scope is part of the key

**Status:** Proposed · **Date:** 2026-08-28 · **Milestone:** M7
**Implements:** `FR-QUERY-13`, §11.5, §11.6 · **Builds on:** [ADR-0007](0007-the-cube-model.md)

## Context

The cube engine is built and a server cannot answer with it. Definitions are declared,
validated and loaded at startup; `sankhya-cube-sql` exposes roll-up and slice as table
functions; and nothing registers those functions into a session, because hydration has
nowhere correct to go.

Two obvious placements are both wrong, and it is worth being precise about why, because the
second one is wrong in a way that does not show up in any test that does not go looking.

**Hydrate per statement.** `session_for` builds a `SessionContext` per statement, so
hydrating there reads the entire fact table on every query. Correct, and it makes a cube
slower than the `GROUP BY` it was supposed to accelerate.

**Hydrate once at startup and share the cells.** Fast, and it hands every principal the same
totals whatever policy says. A user restricted to one region sees the global total, and the
result carries nothing to indicate it. This is the disclosure `FR-QUERY-13` and §11.5 exist to
prevent: *a total computed over rows the caller cannot see is a disclosure through arithmetic,
and it is invisible.*

## What other systems do

The question is not novel and the answers converge.

**Cube (cube.dev)** runs a `securityContext` through `queryRewrite`, which injects the
tenant's filter into every query. Its documentation states the rule this ADR is named for:
**anything that scopes the query must also scope the cache key.** Pre-aggregations are shared
infrastructure, and a pre-aggregation built under one security context and served to another
is a data leak — so the context participates in the cache identity, and `scheduledRefreshContexts`
exists to build them per context.

**SQL Server Analysis Services** solves it with `VisualTotals`. With it on, a role sees totals
recalculated over only the members it may see. Microsoft's own documentation is candid about
the trade: it is **off by default, for performance**, and being off *"could create a security
issue if a user can use the aggregated cell values to deduce values for attribute members to
which the user's database role does not have access."* SSAS also caches dimension security per
user at login, which becomes a queue when many users connect at once.

That is the industry's honest position: a correct answer costs an aggregation you cannot
share, so the default is the fast wrong one with a documented warning.

**AtScale** and the aggregate-aware semantic layers virtualise over the warehouse and rewrite
queries onto pre-computed rollups when the optimiser can prove the rollup answers the query.
Row-level security lives in the warehouse, which means a restricted query is one the rollup
may not be able to answer — and the fallback is the base table.

**Apache Kylin** builds cuboids from the whole fact table, so a row-level ACL applied at query
time is a filter the cuboid cannot satisfy, and the query falls back to base data.

The common shape, stated once: **a pre-aggregate is usable only when the scope it was built
under is at least as narrow as the scope the caller is entitled to.** Everything else is
bookkeeping.

## Decision

**Three tiers, chosen per query, with the scope in the key.**

### 1. Query-time aggregation over the already-secured session

The cube's table functions plan against the tables `session_for` has *already* registered, and
those are `SecuredTable`s — the same wrapper that filters a plain `SELECT`. A cube query is
therefore filtered by the same code as every other query.

This is the load-bearing decision. The alternative — teaching the cube engine about policy —
would be a **second implementation of the authorization rule**, and two implementations of one
rule disagree eventually. When they disagree the cube is the one that leaks, because it
answers with a number rather than with rows somebody might notice were missing.

Always correct, always available, and it is the fallback every other tier falls back to.

### 2. A hydration cache keyed by scope digest

`(definition version, snapshot, cuboid, scope digest)`.

The **scope digest** is a hash of the principal's *effective policy predicates* — not their
identity. Two analysts with the same entitlements share a cache entry; a thousand users across
six roles produce six entries, not a thousand. This is what makes the cache worth having, and
it is the piece SSAS's per-user security cache does not do.

A cache entry is legible: it can say which scope it was built under, so an operator can see why
two principals got different totals without inferring it.

### 3. Materialised cuboids, with a safety condition

§11.6's materialisation, usable when — and only when — the cuboid was built under a scope
digest equal to the caller's, **or** under the unrestricted scope while the caller is
unrestricted. Otherwise tier 1.

This is aggregate awareness with the condition Kylin and AtScale arrive at from the other
direction. The materialised cuboid key already includes *(definition version, snapshot,
cuboid)* per `FR-QUERY-20`; this adds the scope, which is the same key extension as tier 2.

### And completeness is what we have that they do not

SSAS makes the operator choose between a true total and a visible total. SANKHYA already
computes `Completeness` per `FR-QUERY-13`, so it does not have to choose: **every answer
carries how much of its input it saw.** A policy-filtered total is distinguishable from a
complete one by looking at it, rather than by knowing which role you were in.

That turns the trade-off the whole industry documents as a warning into a property of the
result. It should be stated in the product's own terms, because it is a genuine
differentiator and it comes from a decision already made.

## Consequences

**A cube is never faster than the policy allows.** A principal whose scope no aggregate was
built for pays for a real aggregation. That is the correct price, and it is visible rather
than paid in a silently wrong number.

**Cold cache is slow, and that is legible.** The first query for a new scope hydrates. The
scope digest makes it once per *entitlement set* rather than once per user.

**The scope digest must be derived from policy, never from the principal.** Deriving it from
the principal would make the cache useless; deriving it from anything that does not fully
determine visibility would make it wrong. It needs its own test: two principals with equal
entitlements must produce the same digest, and any difference in visibility must produce a
different one.

**Refusing to cache is always available.** If a scope cannot be digested — an unusual policy
shape, a predicate that is not a pure function of the principal — the right answer is tier 1,
not a guess. A cache that is sometimes wrong is worse than one that is sometimes absent.

## What this does not decide

Whether materialised cuboids are built per scope eagerly (Cube's `scheduledRefreshContexts`
shape) or only on demand. That needs the query log §11.6 already asks for, and building the
selector before the signal exists would repeat the error M7 already made once: eight exit
criteria that passed because every one supplied its own cells.

## Sources

- Cube — [Security context](https://cube.dev/docs/product/auth/context),
  [Multitenancy](https://cube.dev/docs/product/configuration/multitenancy)
- Microsoft — [Grant custom access to dimension data (Analysis Services)](https://learn.microsoft.com/en-us/analysis-services/multidimensional-models/grant-custom-access-to-dimension-data-analysis-services?view=asallproducts-allversions)
- AtScale, dbt and the aggregate-aware layers — [comparison of enterprise semantic layers](https://colrows.com/blogs/dbt-semantic-layer-vs-cube-vs-atscale/)
- Apache Kylin — [Aggregate Index](https://kylin.apache.org/docs/model/manual/aggregation_group/)
