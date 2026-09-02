# 11. The graph engine

> The graph tier holds no durable state. An epoch is built by scanning published tables, carries
> the snapshot it was built from, and is dropped on shutdown — so **an edge exists because a row
> exists**, and the graph cannot disagree with SQL. This chapter argues that the absence of a
> write path is the feature rather than the limitation; that time-respecting traversal must be a
> separate function rather than a flag, because static reachability over-reports and always in
> that direction; and that every bound a traversal ran under has to travel on the row, because a
> qualification outside the rows is dropped by the first projection that omits it.

## 11.1 A derived projection, not a second store

There is no graph write path. An epoch is hydrated by scanning published tables, and it records
the snapshot it was built from.

Four consequences follow, and each is the answer to a question a graph database has to keep
answering:

| Question a graph store must answer | Here |
|---|---|
| What happens when the graph is behind the tables? | It cannot be, in a way the snapshot does not record. Every traversal result carries its epoch and its snapshot |
| What if the graph and SQL disagree about an entity? | They cannot. An edge exists because a row exists |
| What is the graph's durability contract? | It has none, and needs none. It is rebuildable from published tables, with a published recovery time |
| What happens on restart? | It is rebuilt. Derived state never blocks shutdown |

The cost is real and is stated in Chapter 4: no graph writes, no persistent graph-native indexes,
and a rebuild after restart. The trade is that the class of defect a graph store most often
produces — the relational half seeing an entity the graph half does not — is **unrepresentable**
rather than defended against.

> **Key idea**
> This is the same argument as the cube's tier 1 (Chapter 10, §10.8) in a different subsystem:
> the graph resolves through the same catalog as everything else, so an unauthorised edge is never
> materialised for that tenant, and there is no second implementation of the authorization rule to
> disagree with the first.

## 11.2 Three crates, and the one with no dependencies

The engine is three crates, and the split has earned itself.

| Crate | Layer | Holds | Depends on |
|---|---|---|---|
| `graph-algo` | 1 | The compressed adjacency, traversal, paths, components, centrality, communities, multiplicative influence, and the bounded-result wrapper | **Nothing** |
| `graph` | 3 | Epoch construction, hydration from published tables, the overlay, the specification | Read path, authz |
| `graph-sql` | 4 | Five table functions and their argument parsing | The query engine |

`graph-algo` having **zero dependencies at all** is what makes its property tests fast enough to
run thousands of cases on every build, and what keeps storage concerns out of the algorithms.
Nothing in it allocates a graph: every algorithm takes an adjacency by reference. The cube engine
was given the same three-crate shape for the same reason (Chapter 10).

## 11.3 The adjacency

Typed vertices and typed edges, with per-edge-type adjacency in **both** directions, and
half-open validity intervals on edges stored sorted by source and by time. The consequence is the
one that makes temporal traversal affordable: *"the edges of this vertex as of time t"* is a
binary search plus a contiguous slice, not a filter over everything.

The structure exposes the temporal predicates directly — edges live at an instant, edges starting
by an instant, edges starting from an instant — and the typed accessors take an edge-type mask, so
a multi-relational graph is one structure rather than several.

**The graph carries one number per edge**, and that is a deliberate limit rather than an
omission. It may carry an ownership percentage or a haircut, not both. Amounts belong in the
tables and are joined to the traversal result, not carried on it — which is what keeps the
traversal a traversal and the arithmetic in the engine that has completeness and additivity rules.

## 11.4 Time-respecting traversal is a separate function, not a flag

This is the design decision most likely to be argued with, and the argument for it is asymmetric
rather than aesthetic.

> **Key idea**
> Static reachability over a temporal graph **over-reports** — it finds routes that time forbids —
> and *always in that direction*. A flag defaulting to off would hand the optimistic answer to
> everyone who forgot it, and **the optimistic answer looks exactly like the correct one.**

So it is `graph_time_respecting`, a function with its own name, and two further bounds that a
naive temporal traversal omits:

```sql
SELECT * FROM graph_time_respecting('payments', 'acct-1',
    'max_depth=6, min_conservation=0.9, max_dwell=86400000000');
```

`min_conservation=0.9` requires each onward edge to carry at least nine tenths of the one before.
Without it, a large edge chains onto a negligible one and the result is called a route: a
₹10,000,000 transfer followed by a ₹40 transfer is not a laundering chain, and a traversal with no
conservation bound will report it as one.

`max_dwell` bounds how long a path may pause at a vertex — here 86,400,000,000 microseconds, one
day. Without an upper bound, **two unrelated events years apart join into one path**, and the
resulting "chain" is an artefact of the data set's length rather than of anything that happened.

There is a `min_dwell` too, for the opposite hypothesis: a chain that moves faster than any human
process could is evidence of automation rather than of a person.

## 11.5 Every bound travels on the row

`epoch`, `snapshot`, `truncated` and `truncation_reason` are **columns**, not query metadata.

> **Key idea**
> A flag beside the result gets dropped by the first projection that does not mention it, **and a
> short list looks exactly like a short answer.** A traversal that stopped at its budget and a
> traversal that found four things are indistinguishable from four rows.

Every algorithm is bounded by construction and reports its own truncation through a common
wrapper that carries the result, whether it is complete, and — if not — which bound stopped it.
That wrapper is why "bounded" is a property of the crate rather than a convention each function
observes. The cube's completeness columns were built on this precedent, with the argument
strengthened, because a partial *total* is worse than a partial *list* (Chapter 10, §10.8).

## 11.6 The SQL surface

Five table functions, registered as one call so a session either has the whole surface or none of
it — a partially registered catalogue means a query works on one node and fails on another.

| Function | What it answers |
|---|---|
| `graph_reachable` | What can be reached from here, within bounds |
| `graph_time_respecting` | What can be reached *in order*, under conservation and dwell bounds |
| `graph_shortest_path` | The cheapest route, and the k cheapest loopless routes |
| `graph_cycles` | Simple cycles |
| `graph_influence` | Multiplicative influence — weighted transitive closure with a pruning floor |

```sql
SELECT p.label, r.depth
FROM graph_reachable('payments', 'acct-1', 'max_depth=3') AS r
JOIN parties AS p ON p.key = r.vertex
WHERE NOT r.truncated;
```

The join is the point. A traversal that cannot be joined against a table is a separate product
with its own query language; here the result is a relation and the ordinary tables are relations.

### Why the bounds arrive as a string

This looks like a wart and is a forced move. `name => value` is rejected outright by the SQL
planner for table functions, and `name = value` is resolved as a *column* against an empty schema.
Only literals reach a table function, so bounds arrive as `'max_depth=3, min_conservation=0.9'`.

The mitigation is that **every key is checked against a known set**. The full set is fourteen:
`max_depth`, `max_results`, `max_visits`, `max_degree`, `edge_types`, `from`, `until`, `start_at`,
`max_dwell`, `min_dwell`, `min_conservation`, `damping`, `floor`, `k`. The reason for checking is
stated in the source, and it is the general form of the argument in this book: *a query asking for
`max_dpeth => 3` and getting the default six is wrong in a way its own text does not reveal, and
nobody reviewing it would catch that.*

`max_depth` defaults to six. `edge_types` is the multi-relational filter; `from`, `until` and
`start_at` are the temporal window; `damping` and `floor` belong to influence; `k` to k-shortest
paths.

## 11.7 The algorithm set

| Family | What is there |
|---|---|
| Traversal | Reachability, and time-respecting reachability |
| Paths | Shortest path, k-shortest **loopless** paths, simple cycles |
| Components | Weakly and strongly connected, with group and largest-component accessors |
| Centrality | Degree, PageRank-style rank (hence `damping`), and **betweenness by estimate** |
| Community | Detection, and modularity |
| Influence | Multiplicative influence between two vertices, and from one — hence `min_conservation` and `floor` |

Betweenness is an **estimate** and is named as one in the API. Exact betweenness and closeness
centrality at scale are computationally infeasible at the target sizes and are explicitly not
planned (Chapter 4); presenting an approximation as exact would be the same defect this book keeps
naming, in a centrality measure.

Two capabilities elsewhere in the system are graph capabilities wearing other names.
**Cube consolidation runs on the graph engine** — a consolidation path *is* a traversal, and a
ragged parent-child hierarchy with alternate roll-ups is exactly the shape `graph-algo` already
handles with bounded traversal and reported truncation (Chapter 10, §10.2). And weighted
transitive closure with multiplicative edge weights, cycle tolerance and a pruning threshold is
the general form of ultimate-beneficial-ownership tracing (Chapter 1, §1.3).

## 11.8 Tenancy

Graphs are hydrated **per tenant**. A traversal leaving the tenant's identifier space is an
invariant violation, not a filtered result.

> **Key idea**
> Traversing a shared graph and filtering afterwards is **not offered, even as an optimisation.**
> It leaks existence and topology through timing and through path structure even when payloads are
> hidden — a traversal that takes longer because it walked through vertices you may not see has
> told you they are there.

Audit records carry the table snapshot **and the graph epoch** that answered, so a traversal
result is reconstructible years later rather than merely attributable.

## 11.9 What the tests found

Three defects are worth carrying, because each is a different shape and each is the kind that
survives a suite.

**A negative-weight guard fired by luck.** The shortest-path implementation refused a
negative-weight edge, and the refusal was reached by an accident of ordering rather than by the
check being correct. A guard that happens to work is a guard that stops working when something
unrelated is reordered.

**Community detection collapsed two clusters into one.** Label propagation was replaced with
modularity optimisation. The output of the broken version was a plausible community structure —
one community where there were two — which is the characteristic failure of a clustering
algorithm: nothing is missing and nothing is null.

**A whole source file was never compiled.** It was not declared in the crate root, and it
contained a match arm naming a struct field that does not exist. Nothing failed, because nothing
built it.

> **Pitfall**
> The third one is the reason the reachability gate widened from *"crates that register SQL
> functions"* to plain reachability. **A capability nothing reaches is indistinguishable from one
> that was never built**, and the narrower check had let about 2,600 lines through, including
> Arrow Flight SQL — documented, tested, and served by nothing.

## 11.10 What is not built

| Not built | Note |
|---|---|
| **A measured benchmark against a public suite** | This is **the one M4 exit criterion carried forward as unmet**, rather than reinterpreted. The primitives are correct against brute force and bounded by construction; they have not been *timed at scale*. Memory per vertex and per edge is published, and is measured from a real epoch rather than estimated from type sizes — but throughput is not |
| A timer driving hydration | Nothing rebuilds an epoch on a schedule in a running server |
| Per-tenant epochs through the front door | Built and tested; nothing wires them to a door |
| The pack bundle loader in a running server | The declarative pack tier is built; the loader that reads a bundle directory into a running process was never finished. M4's remainder |
| The structured graph API on the control plane | Named by the requirement; not served |
| A durable graph database | Not a gap — a non-goal. §11.1 |
| A bespoke graph query language | Not planned. A structured API and SQL functions now; the ISO standard later, implemented as a rewrite onto those functions rather than a second engine |
| Exact betweenness and closeness at scale | Infeasible at the target sizes; approximate variants provided and named as such |

The first row deserves the emphasis it is given. It would have been easy to reinterpret the
criterion — *"correct and bounded"* is a defensible thing to have demonstrated — and the milestone
would have closed clean. It is carried as unmet instead, which is what an exit criterion is for.
