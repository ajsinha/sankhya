# ADR-0005 — Array columns, and kernels that are deterministic

**Status:** Accepted · **Date:** 2026-08-27 · **Milestone:** M6
**Implements:** array-valued columns and the vector kernel set
**Owner directive, 2026-08-27**

## Context

Analytical work in several domains is vector-shaped per row rather than scalar: factor
vectors, embeddings, time-series windows, sensor readings, yield curves. Today none of that
can be stored. `LogicalType` has fifteen scalar variants and no list, the table format's
type mapping returns nothing for anything nested, and its reader refuses a nested type by
name.

That refusal was deliberate — the rule was to refuse anything that does not round-trip
exactly rather than publish something merely similar — but it means data of this shape has
to live somewhere else. Which reintroduces the split this system exists to remove: export to
compute, compute elsewhere, import the answer, reconcile the two copies.

Storage alone is not enough. A column that can be stored and not operated on has moved the
export one step later, not removed it.

## The constraint that decides the design

**Every useful vector kernel ends in a floating-point sum.** A dot product, a norm, a mean,
a cosine distance — each is a reduction, and `REQUIREMENTS` §432 already records what that
costs:

> Parallel floating-point reduction is non-deterministic by default. Reduction order varies
> with partition completion order, so the same aggregate query returns different values run
> to run.

`sankhya-numeric` exists because of that, and its own module comment states the cost
precisely: *"The difference is small. That is what makes it expensive: it is too small to
notice and too large to reconcile, so it surfaces as a figure that will not tie out and
nobody can explain."*

A kernel set that bypassed that machinery would silently undo a decision this system has
already made and paid for — and it would undo it in the numerically worst place, because a
dot product over widely-scaled factors is exactly where non-associativity bites hardest.

So: **every reducing kernel goes through `deterministic_sum`.** That is not a preference. It
is the property that makes the rest of the system's determinism claim true rather than
partially true.

## Decision

### Storage: `FixedSizeList<Float64, N>` for known dimension

Fixed rather than variable, wherever the dimension is known:

- No offsets buffer. The child buffer *is* a flat `&[f64]`, which is the layout a numeric
  kernel wants anyway.
- The length becomes a **schema-level guarantee** rather than a per-row fact, so a
  wrong-length row is refused at write instead of discovered during a computation.
- Cheaper to decode: no repetition levels to walk.

`List<Float64>` remains for the genuinely ragged case, and costs what raggedness costs.

For matrices there is no matrix type in Arrow and inventing one would be a mistake. The
canonical answer is Arrow's **`arrow.fixed_shape_tensor`** extension: a `FixedSizeList` of
`rows × cols` with the shape in field metadata. Nesting `FixedSizeList<FixedSizeList<…>>`
also works and is worse — two offset layers, awkward kernels, and no shape metadata for an
external reader to find.

### Kernels: pure Rust in `sankhya-numeric`, not a library

**Not Polars.** Polars is a dataframe *engine*, not a kernel library: it carries its own
Arrow memory layer, its own expression system and its own execution engine. ADR-0001 already
settled what that costs — *"Two Arrow majors cannot coexist in one process; identically-named
types become distinct and incompatible, and the trait-identity problem is worse."* Polars is
a substitute for DataFusion, not a companion to it.

**Not C BLAS or LAPACK.** Three independent blockers, any one decisive:

| Blocker | Why |
|---|---|
| A `links` key | ADR-0001: a duplicate `links` key is a hard build failure, not a cost. `parquet` already carries native compression |
| `forbid(unsafe_code)` | Workspace-wide and mechanically enforced. Every FFI call needs `unsafe`, and a dot product is not a reason to punch a hole in it |
| Air-gapped packaging | `FR-OLTP-10` disqualifies runtime downloads. A native BLAS means a toolchain and a shared library at the target |

And the constraint above: BLAS offers no ordering guarantee at all. Reordering freely for
speed is its entire design.

**So: pure Rust over Arrow's own buffers.** Elementwise operations are exact and
autovectorise; the reduction stays ordered and compensated. That costs something real
against BLAS and buys reproducibility, which for a system whose outputs feed reconciliations
is the right side of the trade.

### Scope: vectors now, matrices deferred

| Built | Deferred, deliberately |
|---|---|
| Elementwise: add, subtract, multiply, divide, scale | **Matrix multiply** — where BLAS genuinely wins, and where an optional native feature has a real case |
| Reductions: dot, L1 and L2 norm, sum, mean | Decompositions (LU, QR, SVD) — a different discipline, done badly in-house |
| Distances: euclidean, cosine | Sparse vectors — a real need and a separate representation decision |
| `matvec` | |

Deferring matmul is a judgement that the vector cases are both commoner and more
determinism-critical, and that the right moment to weigh a native dependency is when a
measured workload needs it — not now, on the assumption that it might.

## Consequences, stated as costs

- **Statistics and pruning do not work on an array column.** A minimum and maximum of a
  vector prune nothing. A table of embeddings prunes on `sank_data_date` and its scalar
  columns, and on nothing else. This is a property of the shape, not a gap to be closed.
- **An array cannot be a key column.** Mutable-table merge resolves latest-version-per-key,
  and array equality as row identity is a bad idea. It is refused rather than supported
  badly.
- **A deterministic dot product is slower than an undeterministic one.** The elementwise
  multiply vectorises; the sum does not. That is the price of the guarantee and it is paid
  knowingly.
- `sankhya-ext::Value` gains an array variant, so pack functions can compute on these
  columns — which is where domain mathematics belongs.

## Revisit if

A measured workload is dominated by matrix multiplication. Then the case for an **optional**
native backend behind a feature flag becomes real, with the packaging cost stated — rather
than baked into the baseline on speculation.
