<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# ADR-0020 — A wide catalogue of built-in functions

**Status:** Accepted · **Date:** 2026-09-02 · **Milestone:** M21 — the design gate, before any implementation
**Builds on:** [ADR-0010](0010-external-aggregations.md), [ADR-0017](0017-the-client-contract.md), [ARCHITECTURE](../ARCHITECTURE.md) §5.7

## Context

**Owner directive, 2026-09-02:** the built-in catalogue must be *very wide and very expansive* —
linear algebra, matrices, calculus, statistics, mathematics, vector mathematics, Excel-style
functions, graph functions — reachable from **both** the SQL surface and every SDK, and
**vectorized** for performance.

The reasoning is a product one and worth stating, because it decides what "enough" means. A user
who has to leave the warehouse to do arithmetic has left the warehouse: they pull the rows into
pandas, and from that moment the warehouse is a file server. Every function that exists here is
a reason for the computation to happen where the data already is, which is the only place it is
cheap.

### What exists today

Twenty-six functions, in `sankhya-olap`: `vec_of`, `vec_sum`, `vec_dot`, three norms, four
distances, seven descriptive statistics, one integral, and nine matrix operations. Plus
DataFusion's own scalar and aggregate library, and five `graph_*` table functions.

Three things are wrong with that as a foundation.

**It is not vectorized, despite looking as though it is.** `invoke_with_args` takes arrays and
returns an array — columnar at the boundary — and then loops row by row, and *inside that loop*
`vector_at` performs an `Arc` allocation and a `Vec<f64>` copy for every vector of every
argument. A ten-million-row cosine similarity does twenty million allocations to compute
something that is a contiguous dot product. For a `FixedSizeList` the child values are already
laid out end to end, so row *i* is `&values[i·n .. (i+1)·n]` — no allocation, and a loop the
compiler can autovectorize.

**It lives in the wrong crate.** `sankhya-olap` also holds the session and the exactness policy.
A catalogue meant to grow tenfold cannot share a crate's 1500-line budget with unrelated
concerns.

**None of it is reachable from a binding.** The Python SDK exposes cubes, clones, feeds and the
graph, and not one vector or matrix function. A capability nobody can call from the client they
use is a capability that does not exist for them.

## Decision 1 — One implementation, in Rust, in this process

Built-ins are compiled Rust registered as DataFusion UDFs, UDAFs and UDTFs. Not the
out-of-process Arrow IPC path of [ADR-0010](0010-external-aggregations.md): that exists for
**user-supplied** functions in other languages, where isolating somebody else's code is the
whole point. These are ours, and paying an IPC hop to call our own dot product would be paying
for a guarantee we do not need.

## Decision 2 — Every function works on every query, and the router adapts rather than the user

**The invariant, first, because it is the requirement and everything below is only the
mechanism:**

> A user does not know whether their query was answered by the transactional tier or the
> analytical one, and **must never need to know**. Every built-in works on every query. There
> is no statement a user can write that is refused, or answered differently, because of a
> routing decision they cannot see and did not make.

This is not a nicety. [ARCHITECTURE](../ARCHITECTURE.md) §5.7.3 already refuses to put the tier
into a table name, on the grounds that a name encoding *where a row lives* is a name whose
meaning changes underneath the query. A function whose availability depends on the tier is the
same mistake wearing different clothes: the user would have to know the publication lag and the
router's shape rules to know whether `vec_cosine_similarity` is going to work this morning.

Now the mechanism, which is where it gets awkward. §5.7 routes by query shape: a point lookup by
key goes to the transactional store, because a b-tree probe against the authoritative copy is
simultaneously fastest and freshest. That store is PostgreSQL, and **a Rust UDF registered in
DataFusion does not exist there.**

Three ways to hold the invariant, and only one survives.

**Implement each function twice** — once as a Rust UDF and once as a PostgreSQL extension —
is refused. Two implementations of one function agree until they do not, and the day they
disagree the answer depends on which tier served it: the same statement, the same data, a
different number, and nothing in the result says which path it took. That is the worst failure
this system can produce, and it is worse than not having the function.

**Refuse a built-in on the transactional path** breaks the invariant outright, and is refused.
It hands a physical fact to the user as a logical one: to know whether their function works they
would have to know how their query routes.

**A statement calling a built-in routes to the analytical path.** Accepted, and it is the only
one of the three that holds the invariant. The router already decides by shape; *"this statement
calls a built-in"* is one more shape, and one that is knowable from the parsed statement before
anything is read. The user writes the function and it works. They are never told which tier
answered, because it does not change the answer.

Freshness is not lost: a fresh read splices the transactional buffer, so the rows are the same
rows the OLTP path would have returned. What is lost is the b-tree probe's *latency*, on the
narrow case of a point lookup that also calls a built-in — a cost that is visible in a query
plan rather than hidden in a wrong number, and one the user pays only when they asked for
something the fast path could never have done anyway.

> Nothing routes to PostgreSQL today: `sankhya-oltp-pg` is built and unwired. This is decided
> **now**, before the router exists, because a rule of this kind is unaffordable to retrofit —
> by the time both tiers answer queries, the second implementation is already written.

## Decision 3 — Speed comes from fixed point, not from SIMD, and the reason is not obvious

The directive asks for vectorization. The first design here said *"vectorized means the inner
loop"*: borrow contiguous slices, run lane-parallel accumulators, let the compiler emit SIMD.
That design is **wrong for this system**, and measuring it is what showed why. The reasoning is
recorded because anybody who reads the profile will propose it again.

### What is actually slow

`vec_dot(a, b)` allocates a `products` vector, then `deterministic_sum` allocates a second,
**sorts it by magnitude**, and runs a sequential Neumaier compensation — and above that,
`vector_at` performs an `Arc` allocation and a `Vec<f64>` copy for each vector of each argument,
*per row*. A ten-million-row cosine similarity over 512-dimensional embeddings therefore does
tens of millions of allocations and ten million sorts to compute a fused multiply-add loop.

The sort is not incidental. `deterministic_sum` exists because **floating-point addition is not
associative**, so a total whose order depends on how work was partitioned returns a different
number when the machine is busier — a difference too small to notice and too large to
reconcile.

### Why lane-parallel accumulation cannot be the answer

Eight fixed lanes with per-lane compensation is **10 to 15 times faster** and, on well-behaved
data, bit-identical. On badly-conditioned data it is neither. For a vector of `1e16, 1, -1e16, 1`
repeated nine times, whose exact total is `18`:

| | Result |
|---|---|
| `deterministic_sum` | **18**, and 18 again when the input is reversed |
| Eight-lane compensated | **5**, and **0** when the input is reversed |
| Naive `f64` sum | 1 |

A threshold that fast-paths only "safe" inputs was tried and is also refused. Across 200,000
randomized vectors it never once fell back, and still **differed from the exact sum in 57% of
cases** and changed under permutation in 65% — worst relative error `5e-11`. Small. Small is the
problem: that is exactly the figure that will not tie out and nobody can explain.

### What the answer is

`sankhya-math`'s own documentation names it: *"the answer is not to drop the sort — it is to use
fixed-point arithmetic, where addition **is** associative and the question does not arise."*

Reductions accumulate into a **fixed-point integer accumulator** scaled from the largest
magnitude in the input. Integer addition is associative and commutative, so the total depends
only on the multiset — order-independence holds **by construction** rather than by sorting, and
the `n log n` disappears along with both allocations.

Exactness has a proof rather than a measurement behind it. The scale places the accumulator's
least significant bit **100 binary places below the largest term**, and a `f64` result carries
53 significant bits — so the accumulator holds strictly more precision than the answer can
express, and truncating a term below that point cannot change the correctly-rounded result.

Measured against the shipping implementation:

| | Result |
|---|---|
| Agreement on 200,000 randomized vectors | **bit-identical, zero differences** |
| Change under permutation | **none**, asserted on every case |
| The `1e16, 1, -1e16, 1` case | **18**, correct |
| Speed of the reduction alone, 64 / 512 / 4096 dimensions | **1.5× / 2.0× / 2.7×** |

And end to end through the shipping kernels, with every result bit-identical to what the same
call returned before:

| Kernel | 64 dims | 512 dims | 4096 dims |
|---|---|---|---|
| `vector::dot` | 1.33× | 2.75× | 2.36× |
| `vector::cosine_similarity` | 1.99× | 2.35× | **3.62×** |
| `vector::euclidean` | 1.86× | 1.95× | 2.48× |

Less than the 15× a lane-parallel loop offers, and the whole of that 15× was never available:
the fast version was computing a different, worse number.

The change is made **inside `deterministic_sum`**, which takes the fixed-point route and falls
back to the sorted one. Every caller — vector, statistics, calculus, quantile — gets this
without a call-site edit, and none of them can get a different answer, which is why it was safe
to do in one place rather than twenty.

### So the requirements are

1. **Reductions go through the fixed-point accumulator**, and fall back to the sorted path only
   where no common scale exists — never to an approximation.
2. **No per-row allocation.** A `FixedSizeList` argument is borrowed as a contiguous `&[f64]`;
   the general `List` case takes a slice of the child array, still without copying.
3. **Element-wise kernels take slices**, where the compiler's autovectorization is free and
   changes no result — element-wise operations reassociate nothing.
4. **A null is a null**, never a zero. A cosine similarity of zero says *orthogonal*, and a
   missing vector is not orthogonal to anything.
5. **Every claim carries its number.** *"We vectorized it"* with no measurement is a claim, and
   this repository does not ship claims.

## Decision 4 — One catalogue, one crate, one registration point

A new crate, `sankhya-functions`, holds every built-in and one `register` entry point. The
existing `vec_*` and `mat_*` move into it unchanged in behaviour, and `sankhya-olap` keeps the
session and exactness concerns that are actually its own.

**Every function is discoverable from SQL**, through a `functions()` table function reporting
name, category, arity, argument types, return type and a one-line description — for the same
reason `cubes()` exists: a client that cannot enumerate a capability cannot offer it, and a
catalogue nobody can list is a reference manual nobody reads.

## Decision 5 — A function is not delivered until a binding can call it

Two obligations, not one. A function shipped on the SQL surface and absent from the SDKs is
half-shipped, and the half that is missing is the half most users have.

The binding stays thin — [ADR-0017](0017-the-client-contract.md) Decision 1 is not weakened
here. A method builds a statement and passes back what came out; it validates nothing, because
a binding that checked arity would be a second definition of the function's signature and the
two would drift. What the bindings add is **enumeration and naming**, which is discovery rather
than logic.

Every category ships with runnable examples in each SDK, gated as tests.

## Decision 6 — Excel compatibility is by name and by result, or it is not claimed

Excel-style functions are the ones with the most users and the least tolerance for
approximation, because the person checking has the spreadsheet open beside them.

So a function named after an Excel one must **agree with Excel**, including on the cases where
Excel is arguably wrong — `IRR`'s iteration and starting guess, `NPV` discounting from period
one rather than zero, the 1900 leap-year bug in date serials. A function that is *nearly* `XIRR`
and is called `XIRR` is worse than one called something else, because the disagreement is found
by somebody reconciling to four decimal places at a month-end.

Where agreement is not achievable, the function takes a different name and says why in its
description. Named differently is honest; named identically and subtly different is not.

## Decision 7 — Designed for thousands, because that is the stated target

**Added 2026-09-02 by owner directive**: the catalogue is meant to grow until it *rivals
MATLAB*. That is a different engineering problem from the one the first six decisions solved,
and saying so now is cheaper than discovering it at four hundred functions.

### What breaks at scale, and what each costs

**One file per category stops working at about fifteen functions each.** `check-loc` caps a
file at 1500 lines and does so for a reason. The answer is one module per *family* — normal,
Student's t, the incomplete gamma — not one per broad category, and a crate per domain once a
domain outgrows a directory.

**One crate becomes a compile bottleneck.** `sankhya-functions` is already the crate every
build must finish before the server links. Splitting it by domain — one crate for statistics,
one for linear algebra, one for finance — buys parallel compilation and lets a contributor
rebuild one domain. It costs a registration point per crate, which the catalogue already
handles: `describe::register` takes the entries rather than owning them, exactly so that the
list can come from several places.

(Those crates do not exist. They are named as a shape rather than as a plan, and the gate
refuses a document that references a crate the workspace does not have — which is how this
paragraph came to be phrased without them.)

**Hand-written registration stops scaling before the functions do.** Sixty-nine
`ScalarUDF::from(Numeric::new(...))` lines are readable; two thousand are not, and the
catalogue entry beside each is a second copy that drifts. The next structural step is one table
per family carrying name, arity, shape, description **and** kernel together, with registration
and catalogue both generated from it — so the two cannot disagree because there is one list.

The drift test written for the current shape (`every_registered_function_is_in_the_catalogue_
and_the_reverse`) is what makes that migration safe: it fails the moment the two lists diverge,
whichever way they are built.

### Performance is a property that is measured

The wrappers here were written row-at-a-time, allocating a `Vec` per argument per row — the
same defect that made `vec_dot` slow before the fixed-point sum replaced the sorted one,
written again in four new places three weeks later. A `FixedSizeList` stores its rows end to
end, so a row is a borrowed slice with a known stride and no allocation at all.

Measured, summing a column on this machine:

| Width | Copying per row | Borrowing | |
|---|---|---|---|
| 8 | 21.15 ms | 0.85 ms | **24.9×** |
| 64 | 4.57 ms | 0.63 ms | **7.2×** |
| 512 | 3.08 ms | 1.46 ms | **2.1×** |

The narrow case wins most, and narrow is what a series column usually is: a window of readings,
a term structure, a short curve. Results are identical; only the allocation changed.

So the rule for anything added here:

1. **A row is borrowed, not copied**, wherever the layout allows it. Where it does not — a
   variable-length `List` — the buffer is reused across rows, so the cost is one allocation per
   column rather than one per row.
2. **A reduction goes through `sankhya-math`**, which is where the order-independence argument
   lives. A kernel that sums its own way is a kernel whose answer moves when the machine is
   busier.
3. **Every claim about speed carries its number.** The table above is the form; *"we optimised
   it"* with no measurement is a claim, and this repository does not ship claims.

### What is deliberately not promised

Not that every function will be fast. A Cholesky is `O(n³)` and no amount of care changes that.
What is promised is that the **wrapper** costs nothing measurable next to the kernel, so a slow
function is slow because the mathematics is, and a caller reading a profile sees the arithmetic
rather than the plumbing.

## What this does not decide

- **Which functions, exactly.** The catalogue is a living list in the milestone, not an ADR
  clause; enumerating it here would make every addition an ADR amendment.
- **User-defined functions in SQL** (`CREATE FUNCTION`). A different question with a different
  security answer, and [ADR-0010](0010-external-aggregations.md) already owns the out-of-process
  half of it.
- **GPU or explicit SIMD intrinsics.** Autovectorized slice kernels first, with the benchmark
  from Decision 3 to say whether anything beyond them is worth its portability cost.
- **Pushing a built-in into the storage layer** as a pruning predicate. Real, and it needs the
  statistics work to land first.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
