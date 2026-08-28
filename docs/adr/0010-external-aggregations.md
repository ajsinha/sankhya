<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# ADR-0010 — External aggregations: a measure may bring its own rule, if it can merge

**Status:** Proposed · **Date:** 2026-08-28 · **Milestone:** M7 or later
**Builds on:** [ADR-0007](0007-the-cube-model.md), [ADR-0008](0008-serving-cubes-under-policy.md)

## Context

> *When we form a cube or do a roll-up, can we supply an external aggregation function written
> in Python? This can be incredibly powerful.*

It can, and the reason it is powerful is the reason it is dangerous, which is worth stating
before any design: **the aggregation rule is the one thing in a cube that decides whether an
answer is correct.**

`Rule` is not a formatting choice. It decides which roll-ups are legal, whether a materialised
ancestor may answer a query, and whether a total means anything. `Rule::Mean` exists so that
averaging averages can be *refused*; `Rule::None` exists for ratios and distinct counts, where
there is no operation over the parts that yields the whole. A cube's correctness rests on
these being declared honestly.

A bare Python callable cannot answer the question the model actually asks. Given
`def f(values): ...`, the system cannot know whether `f(f(a) + f(b))` is `f(a + b)`. It has
two choices and both are bad: assume it composes, and produce plausible wrong numbers from
materialised ancestors; or assume it does not, and give up roll-up, the lattice and
materialisation entirely for that measure.

## What other systems do

**Snowflake's Python UDAFs** require a class with `__init__`, `accumulate`, `merge`,
`aggregate_state` and `finish`. `merge` is not optional and not a convenience: it is what lets
the engine split the work across threads, aggregate partially, and combine the pieces. The
state is explicit — `aggregate_state` hands it out — so partial results are first-class rather
than an internal detail.

**Spark's `Aggregator`** is the same shape: `zero`, `reduce`, `merge`, `finish`.

That shape is not an accident of either engine. It is what a distributed or cached aggregation
*requires*, and it is exactly the question a cube needs answered.

## Decision

**A measure may declare an external aggregation, and the declaration is a contract, not a
function.** The contract is the industry's, because the industry converged on it for our
reason:

| Method | What it means here |
|---|---|
| `accumulate(state, batch)` | Fold a batch of values into the state. Batch, not row — see below. |
| `merge(a, b)` | Combine two partial states. **Its presence is the composability declaration.** |
| `finish(state)` | The state as a number. |
| `state()` | The partial state, serialisable — what a materialised cuboid stores. |

### `merge` is how a measure earns the lattice

A declared `merge` means the measure composes: it may be rolled up, answered from a
materialised ancestor, and combined across partitions. **No `merge` means `Rule::None`** —
usable, computed from base data every time, never from an ancestor.

That is not a restriction invented for Python. It is the rule already applied to `Mean` and to
ratios, stated in a form an external author can satisfy.

### The state is what gets materialised, not the number

A Python function returns a float, and a float cannot be rolled up further without the
rounding that exit criterion 3a forbids — that is precisely what
[`store.rs`](../../crates/sankhya-cube/src/store.rs) exists to avoid, and why a cell is
stored as an unrounded Shewchuk expansion rather than a total.

So an external measure materialises its **state**, exactly as Snowflake's `aggregate_state`
does, and `finish` runs once when the answer is read. This generalises what is already there:
a cell's contributions are today either raw values or an exact expansion, and this adds a
third case — an opaque state with a declared merge.

A measure whose state is not serialisable is usable and not materialisable. Said plainly at
declaration time, not discovered when a cuboid fails to write.

### Determinism is checked, not trusted

Criterion 3a requires materialisation on and off to agree **by bits**. An external function
can break that without anybody lying: Python set iteration order, dictionary ordering before
3.7, accumulated floating point in a different order, a stray `random` seed.

So a declared aggregation is **exercised** before it is trusted: the same input, accumulated
in one batch and in several, merged in two different groupings, compared by bits. A function
that fails is refused at declaration with the two answers side by side. This is cheap, it runs
once, and it catches the class of defect that would otherwise appear as two reports disagreeing
by a penny.

### It runs inside the trust boundary of the data it sees

An aggregate is computed over the rows a principal may read
([ADR-0008](0008-serving-cubes-under-policy.md)), so an external function is handed
policy-filtered data. A function that can open a socket is an exfiltration path with a
legitimate-looking name.

Therefore: no network, no filesystem, no subprocess; a declared, pinned set of importable
modules; a wall-clock and memory bound per call, because an aggregation that never returns is
an outage rather than an error. And the function's **version participates in the cache and
materialisation keys** — a changed function is a changed answer, and serving a cuboid computed
by the previous version is the same defect as serving one from the previous snapshot.

### Arrow in, Arrow out

`accumulate` takes a batch rather than a row. Per-row calls across millions of cells spend
their time in the interpreter boundary and the GIL, and the data is already Arrow on both
sides — so it crosses as Arrow, zero-copy, and a competent implementation does its work in
NumPy or Polars rather than in a Python loop.

## Consequences

**The powerful case works.** A weighted average, an exponential moving average, a percentile
sketch, a domain-specific risk rollup, a HyperLogLog — all of these have a natural `merge`,
and all of them get roll-up, the lattice and materialisation for free by saying so.

**The genuinely non-composable case still works, slowly and correctly.** No `merge` means base
data every time. That is the honest answer rather than a fast wrong one.

**A cube's correctness now depends on code the warehouse did not write**, which is a real
change in posture and the reason for the checks above. The mitigation is that the *declaration*
is what is trusted, the declaration is machine-checked, and a function that cannot satisfy it
is refused rather than accommodated.

**Python is not the boundary.** The contract is a shape, not a language, and the same one
admits WebAssembly — which would answer the sandboxing question far more convincingly than a
restricted interpreter. Python is the ecosystem people have; WASM is the isolation nobody has
to trust. Worth planning the contract so that both can satisfy it.

## What this does not decide

Whether the interpreter is embedded (`pyo3`, in-process, fast, and sharing an address space
with the audit chain) or out-of-process behind Arrow IPC (slower per call, isolated, killable).
The second is the safer default and the first is what people will ask for; that is a decision
to make with a measurement rather than a preference, and the contract above is the same either
way.

## Sources

- Snowflake — [Python user-defined aggregate functions](https://docs.snowflake.com/en/developer-guide/udf/python/udf-python-aggregate-functions)
- Snowflake — [Creating UDAFs for DataFrames in Python](https://docs.snowflake.com/en/developer-guide/snowpark/python/creating-udafs)
- Felipe Hoffa — [Uncovering the new Snowflake UDAFs with Apache DataSketches](https://hoffa.medium.com/uncovering-the-new-snowflake-udafs-with-apache-datasketches-ceeca5d22985)
