<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# ADR-0011 — SDAF: an extension declares what it needs, and is given it

**Status:** Proposed · **Date:** 2026-08-28 · **Milestone:** M7 or later
**Builds on:** [ADR-0010](0010-external-aggregations.md), [ADR-0008](0008-serving-cubes-under-policy.md)

## Context

[ADR-0010](0010-external-aggregations.md) settled the *shape* of an external aggregation: a
state, an `accumulate`, a `merge` that declares composability, a `finish`. It assumed a pure
function over values.

The requirement is larger:

> *I want our SDAF to be more generic and extensible … typically it will be written in Python
> … and when an SDAF runs it can access resources in Sankhya as well.*

That is a reasonable thing to want and it is most of what makes such a function useful. A
currency translation needs a rate table. A risk roll-up needs a factor matrix. A regulatory
measure needs a mapping nobody wants to pass as a literal. An aggregate that can only see the
numbers in front of it is a calculator; one that can reach its warehouse is a model.

It also changes what an SDAF *is*, and the change is not small.

## What the major engines do, and why

**Snowflake does not pass a session to a UDF or a UDTF.** They cannot query Snowflake objects
at all. This is documented as a deliberate security restriction rather than a gap: a function
is a sandbox, and a stored procedure — which *does* receive a session — is the thing you reach
for when you need to query. **Spark** is the same in a different vocabulary: a UDF runs on an
executor and there is no `SparkContext` there to use.

The reason is not squeamishness. A function that may issue queries is re-entrant, so it can
recurse; its answer depends on data nobody declared, so it cannot be cached correctly; and it
runs somewhere the engine has already decided the security context, so it becomes the easiest
place to escape one.

**So the precedent says: do not hand an aggregate a connection.** It does not say the
capability is wrong. It says the mechanism is.

## Decision

**An SDAF declares its dependencies; the runtime resolves them and hands them in.** Injection,
not connection. Everything below follows from that one choice.

```python
@sdaf(
    name="fx_translated_sum",
    version="3",
    # What this needs. Declared, so the planner, the cache key and the policy engine
    # can all see it before a single row is read.
    needs=[
        table("reference.fx_rates", columns=["ccy", "as_of", "rate"]),
        parameter("target_ccy", default="USD"),
    ],
    # Along which dimension this rule applies, and whether it merges there.
    merges=True,
)
class FxTranslatedSum:
    def __init__(self, ctx):
        # `ctx.table(...)` returns an Arrow table, already resolved, already filtered
        # by whatever the caller may read. There is no query to issue and no connection
        # to hold.
        self.rates = ctx.table("reference.fx_rates")
        self.target = ctx.parameter("target_ccy")
        self.total = 0.0

    def accumulate(self, batch):        # Arrow, not rows
        ...

    def merge(self, other): ...          # its presence is the composability claim
    def state(self): ...                 # what a materialised cuboid stores
    def finish(self): ...
```

### 1. Resolved through the caller's guard, always

An aggregate is computed over the rows a principal may read
([ADR-0008](0008-serving-cubes-under-policy.md)). A declared table is resolved through **that
principal's guard** — same row filters, same column masks, same tenant.

This is the load-bearing rule. An SDAF that could read a table its caller cannot is a
privilege escalation with a friendly name, and its output is disclosure through arithmetic
with an extra step: the caller never sees the forbidden rows, only a number derived from them,
and a number carries no evidence of where it came from.

A caller who may not read a declared dependency does not get a slow answer or a partial one.
The measure is **not available to them**, said at planning time.

### 2. Resolved once, before accumulation

Dependencies are resolved when the aggregation starts, not inside `accumulate`. A lookup per
cell is a query per cell, and a cube has millions.

This also removes re-entrancy by construction: there is no moment during accumulation at which
a query can be issued, so an SDAF cannot recurse into the engine that is running it.

### 3. Every dependency's snapshot joins the key

The materialisation and hydration key is already *(definition version, snapshot, cuboid,
scope)*. An SDAF adds two more: **the function's version**, and **the snapshot of each
declared dependency**.

Without them a cached aggregate outlives the data it was computed from. A cube materialised
against yesterday's FX rates, served today, is exactly the stale-answer failure `FR-QUERY-20`
was written to make impossible — and it would be invisible, because the number looks fine.

### 4. Cycles are refused at declaration

Dependencies are declared, so the graph is known before anything runs. An SDAF that depends on
a cube whose measure uses that SDAF is refused when it is registered, naming the cycle.

An engine that discovers this at query time discovers it as a stack overflow or a hang.

### 5. Capabilities, not a connection — which is what makes it extensible

`ctx` exposes narrowly-typed accessors for *declared* things: `ctx.table`, `ctx.parameter`,
`ctx.members`, `ctx.cube`. It is not a session and has no `execute`.

That is the extensibility mechanism rather than a limit on it. A new capability is a new
declaration kind, and adding one forces the question that matters: *what does this do to the
cache key, and what does it do to the policy check?* An open `execute` lets a capability be
added by accident, with nobody to ask.

### 6. The manifest is versioned and open

`needs` is a list of declarations, not a fixed signature, so a later version can add kinds
without breaking a single existing SDAF. The same contract shape serves scalar and table
extensions later; an aggregate is the first instance, not the whole of it.

### 7. Language is not part of the contract

Python is the ecosystem people have. The contract is a shape — a manifest, four methods, Arrow
in and out — and WebAssembly satisfies it too, with isolation nobody has to trust rather than a
restricted interpreter. Worth keeping the contract language-neutral now, so choosing later is
a decision rather than a rewrite.

## Consequences

**An SDAF is a data dependency, and the system treats it as one.** It appears in lineage, in
the cache key, and in the answer to "why did this number change". That is more machinery than a
callable, and it is the machinery that makes a callable safe to cache.

**Declaring is more work than connecting.** An author must say what they need instead of
reaching for it. That cost buys the planner a dependency graph, the cache a correct key, the
policy engine something to check, and the operator an answer to what a measure reads.

**Some things become impossible, on purpose.** An SDAF cannot decide mid-aggregation to read
something it did not declare. A function needing that is asking to be a stored procedure or a
pipeline stage, and both are better places for it than the inside of an aggregate.

**A dependency that cannot be resolved is a planning error, not an empty result.** An SDAF
silently seeing an empty rate table returns numbers that are wrong by a factor nobody notices.

## What this does not decide

Where the interpreter runs — embedded (`pyo3`, fast, sharing an address space with the audit
chain) or out-of-process behind Arrow IPC (isolated, killable, slower per call). The contract
is identical either way, which is the point of settling it first; the choice wants a
measurement rather than a preference.

## Sources

- Snowflake — [User-defined functions overview](https://docs.snowflake.com/en/developer-guide/udf/udf-overview), and [Python UDTFs cannot query Snowflake objects](https://interworks.com/blog/2022/11/15/an-introduction-to-python-udtfs-in-snowflake/)
- Gray, Chaudhuri, Bosworth *et al.* — [Data Cube](https://web.stanford.edu/class/cs345d-01/rl/olap.pdf) (ICDE 1996), for the composability classification [ADR-0010](0010-external-aggregations.md) rests on
