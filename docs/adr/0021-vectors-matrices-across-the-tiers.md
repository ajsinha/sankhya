<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# ADR-0021 — Vectors and matrices, on both sides of the tier boundary

**Status:** Accepted · **Date:** 2026-09-02 · **Milestone:** M21 — the design gate, before any implementation
**Builds on:** [ADR-0020](0020-the-built-in-function-catalogue.md), [ADR-0017](0017-the-client-contract.md), [ARCHITECTURE](../ARCHITECTURE.md) §5.7

## Context

A wide function catalogue needs types to operate on. `vec_cosine_similarity` is only useful if a
**column** can hold a vector — and a column lives in two places at once here: the transactional
store, which is PostgreSQL and is the writer of record, and the published warehouse, which is
Arrow and Parquet.

[ADR-0020](0020-the-built-in-function-catalogue.md) Decision 2 settled where a *function* runs:
the router adapts, so a user never learns which tier answered. A **type** cannot be settled the
same way. A row is inserted into the transactional store and read from either, so the type must
exist, mean the same thing, and survive the trip — on both sides, from the beginning.

### What is true today

| | State |
|---|---|
| Analytical storage | Arrow `FixedSizeList<Float64, n>` and `List<Float64>`, with `tensor_metadata` carrying a matrix's shape |
| The wire | **A vector is sent as text.** `SELECT vec_of(1.0, 2.0)` returns the string `'[1.0, 2.0]'` under no array type at all |
| Transactional storage | **Nothing.** `sankhya-oltp-pg` is built and unwired, and no type mapping exists |
| The bindings | Receive that text and would have to parse it |

The middle two are the ones that will be expensive to change later, and the first of them is
wrong now rather than merely absent.

## Decision 1 — One logical type, two representations, and the mapping is written once

A column declared `VECTOR(n)` is:

| Where | As |
|---|---|
| PostgreSQL | `float8[]`, or `vector(n)` where the `pgvector` extension is present |
| Arrow and Parquet | `FixedSizeList<Float64, n>` |
| The wire | `float8[]`, OID `1022` — see Decision 3 |

The mapping lives in **one** table in one crate. Not in capture, and separately in the wire
encoder, and separately again in each binding: three copies of a type mapping is three chances
to disagree about what a null element means, and the disagreement surfaces as a number rather
than as an error.

### What this means on each side, concretely

| | PostgreSQL, the writer of record | Arrow and Parquet, the published copy |
|---|---|---|
| `ARRAY(t)` | `t[]` — **native, no extension** | `List<t>` |
| `VECTOR(n)` | `float8[]` with a generated `CHECK (array_length(c, 1) = n)`, or `vector(n)` where `pgvector` is present | `FixedSizeList<Float64, n>` |
| `MATRIX(r, c)` | `float8[]` with a `CHECK` on `r · c`, plus the shape recorded in the catalogue | `FixedSizeList<Float64, r·c>` with the shape in field metadata |

Arrays need nothing added to PostgreSQL — `float8[]` has been there for decades. The two things
that need building are the *width* and the *shape*, because PostgreSQL's array type carries
neither: `ARRAY[1,2,3]` and `ARRAY[1,2]` have the same type.

**The width is enforced by a constraint SANKHYA generates**, not by convention, so a row that
does not fit is refused by the store itself rather than by whatever reads it next. Where
`pgvector` is present the width moves into the type and the constraint is unnecessary.

**The shape of a matrix is recorded in SANKHYA's own catalogue**, not in a column comment. A
comment is documentation, and this is a fact the read path must have in order to reshape a flat
array — putting it somewhere a `COMMENT ON COLUMN` can silently change would make the shape
editable by anybody with DDL rights and no way to notice.

## Decision 2 — The width is part of the type, and a row that does not fit is refused

PostgreSQL's `float8[]` carries no width: `ARRAY[1,2,3]` and `ARRAY[1,2]` share a type. Arrow's
`FixedSizeList` carries one, and **that width is what makes the whole performance argument
work** — a fixed-size list stores its values in one contiguous child buffer, so row *i* is
`&values[i·n .. (i+1)·n]`, borrowed rather than copied.

So a `VECTOR(n)` column declares `n`, and capture **refuses** a row whose array is a different
length. It does not widen the column to a variable-length `List`.

That refusal is the decision. Widening would be the accommodating choice and it is wrong twice
over: the column silently loses the contiguity that made it fast, with no symptom but a slope on
a latency chart; and a vector of the wrong width in a similarity search is not a near miss, it
is a different question — a 384-dimensional embedding and a 512-dimensional one do not have a
meaningful cosine between them, and the honest answer to being handed one is not a number.

## Decision 3 — A vector crosses the wire as an array, not as text

Today `SELECT vec_of(1.0, 2.0)` sends the eight characters `[1.0, 2.0]` as `text`. A generic
PostgreSQL driver — which is every driver this door exists to serve — sees a string.

That is refused, and the fix is to send `float8[]` under OID `1022`, which every PostgreSQL
driver already decodes. The reasons are the ones this system applies everywhere else:

- **A client that parses a rendering will get it wrong**, and not at first. It will get it wrong
  on a null element, on a locale that renders a decimal comma, on an empty array against a null
  array — the cases that are rare enough to reach production.
- **`NULL` and the empty vector are different values.** As text they are `''` and `'[]'`, one
  keystroke apart and both plausible; as an array they are structurally distinct.
- **It costs a binding nothing.** The decoding already exists in every driver, so this is the
  one case where doing the correct thing also deletes code from every client.

The matrix case follows Decision 4: the array plus its shape.

## Decision 4 — A matrix is a vector and a shape, not a third storage type

PostgreSQL has multidimensional arrays; Arrow does not, and `pgvector` has no matrix at all.
Rather than invent a representation that only one side can hold, a `MATRIX(r, c)` column is
stored exactly as `VECTOR(r·c)` — one flat array, row-major — with the shape in the **column's
metadata**, which is where `tensor_metadata` already puts it on the Arrow side.

One physical representation on each side, a lossless round trip, and no new storage type to
teach capture, the read path, the backup checker and the retention sweeper about.

The alternative — PostgreSQL's `float8[][]` — was rejected because its dimensionality is a
property of the *value* rather than the type, so a column could hold a 2×3 in one row and a
4×4 in the next, and the shape would have to be re-read per row to know what was there.

## Decision 5 — An index may rank candidates; only a kernel may report a distance

`pgvector` brings HNSW and IVFFlat indexes, and they are the reason to want it: a nearest-
neighbour probe against the authoritative copy is the transactional tier doing what it is for.

But an approximate index that *ranks* and a function that *scores* are two implementations of
one notion of distance, and [ADR-0020](0020-the-built-in-function-catalogue.md) Decision 2
refused exactly that pairing. So:

> **An index narrows the candidates. The distance a user is shown is always computed by
> `sankhya-math`.**

A query using the index retrieves *k* candidates and rescores them with the kernel. The index is
allowed to be approximate — that is what makes it fast — and the number reported is never the
index's opinion. Ranking and scoring stay one answer, and a caller comparing a returned distance
against a threshold is comparing against the same arithmetic they would get from a full scan.

## Decision 6 — Every new type declares its null, its equality and its ordering, or it is not added

A type is not a storage format. Before a `VECTOR` exists in the catalogue, three questions have
answers, because each of them is a silent wrong answer if left to a default:

- **What is a null vector?** A null column value. *Not* an empty vector, and not a vector of
  zeros: a cosine similarity of zero says *orthogonal*, and a missing vector is not orthogonal
  to anything. This rule already holds inside the kernels and now holds at the type.
- **When are two vectors equal?** Element-wise, and only at the same width. Two vectors of
  different widths are not unequal — the comparison is refused, as it is between a date and a
  number.
- **How do vectors order?** They do not. There is no `ORDER BY embedding`, because there is no
  ordering of vectors that anybody means. What people mean is `ORDER BY vec_cosine_distance(
  embedding, :query)`, which orders *numbers*, and is available. A lexicographic ordering
  would be defensible, implementable, and would silently answer a question nobody asked.

## What this does not decide

- **Whether `pgvector` is vendored**, and at which version. It is the obvious candidate for
  Decision 1's second representation and Decision 5's index, and pinning a third-party
  extension is [ADR-0001](0001-dependency-pin-set.md)'s question, not this one.
- **Sparse vectors.** A different storage problem with a different access pattern, and nothing
  has asked for one.
- **A vector type in the graph epoch.** Vertices carry properties; whether one of those may be
  a vector is `M4`'s question.
- **Quantised or reduced-precision vectors** (`float4`, `int8` embeddings). Real, and a
  compression decision rather than a type decision — it belongs with the storage work that
  owns encodings.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
