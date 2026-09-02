# 3. The estate this replaces

> This chapter inventories the data estate SANKHYA is an argument against, and makes one
> claim about it: the reconciliation cost is **architectural, not careless**. Nobody chose to
> keep four copies of a trade. Each copy was the locally correct answer to a real question,
> taken by a competent team, and the estate is the sum of those answers rather than a failure
> of any of them. The chapter names the four forces that assemble it, does the arithmetic on
> what it costs to run, and then states — precisely — which of those costs consolidation
> removes and which it does not.

## 3.1 The inventory

The estate is not a diagram anybody drew. It is what accumulates, and its components arrive in
a predictable order over roughly a decade.

| Component | The question it answered | What it added |
|---|---|---|
| Operational RDBMS | Where does the transaction commit? | The system of record, and the first copy |
| Nightly ETL to a warehouse | Why is the reporting query killing the trading system? | A second copy, a schedule, and a schema translation |
| A columnar analytical cluster | Why does the month-end pivot take nine hours? | A third copy, a JVM fleet, and a second SQL dialect |
| A streaming bus and CDC connectors | Why is the warehouse a day behind? | A fourth copy in flight, a broker cluster, and an at-least-once delivery guarantee somebody must now deduplicate |
| A graph database | Why can nobody trace the ownership chain? | A fifth copy, a fifth query language, and a hydration job |
| A search index | Why can nobody find anything? | A sixth copy |
| A semantic layer / BI cache | Why do two dashboards disagree? | A seventh copy, described as the fix for the disagreement |
| Extract files | Because the model team could not get access | Copies nobody counts, on laptops |

Every row of that table is a good decision. That is the whole problem. There is no step at
which a reasonable engineer, presented with the state of the system and the question in front
of them, should have decided otherwise.

> **Key idea**
> An estate is not built by anyone choosing to build it. It is the fixed point of a sequence
> of locally optimal decisions, each taken under a real constraint, none of which is
> reversible by the team that would benefit from reversing it. That is what makes the cost
> architectural: it is a property of the sequence, not of the decisions.

## 3.2 The four forces

Four forces assemble the estate and hold it in place. Naming them matters because each one has
to be answered separately; a consolidation argument that answers only the first is the reason
consolidation has been attempted and abandoned before.

**Force 1 — Each store is genuinely better at its job.** A row store really is better at
`UPDATE ... WHERE pk = ?`. A columnar engine really is better at scanning a billion rows and
grouping them. A graph store really is better at a seven-hop traversal with a cycle in it. The
estate is not the result of ignorance about this; it is the result of *knowing* it. Any
consolidation argument that begins by denying the specialisation is arguing with the wrong
people.

The answer is not that specialisation is false. It is that specialisation is a claim about
**data structures and execution**, not about **processes and copies**. A single process can
hold a row-oriented transactional store, a vectorised columnar execution engine and an
adjacency structure at once, because those are three data structures, and nothing about them
requires three deployment units, three security models or three copies of the bytes.

**Force 2 — A connector is always cheaper than a migration.** When system A needs data from
system B, the two options are to move the data or to move the workload. Moving the data is a
two-week project owned by one team. Moving the workload is a two-year project owned by nobody.
The connector wins every time it is proposed, and it wins correctly, on the information
available to the person proposing it. The estate is the accumulated residue of that being
correct several dozen times.

**Force 3 — The security model follows the store.** Each engine has its own idea of what a
principal is, what a grant covers, and what an audit record contains. So each copy acquires a
policy translation, written once, maintained by whoever is left. The translations are where the
divergence lives, and they are invisible: a grant that is stricter in the warehouse than in the
source produces a *smaller answer*, not an error, and a smaller answer looks exactly like a
correct one.

**Force 4 — Nobody owns the seam.** Every component has an owner. The space between two
components has an owner only when an incident assigns one. So the seam is where the schema
drift, the time-zone assumption, the null convention and the rounding difference all live, and
each is discovered exactly once, in production, by somebody who cannot fix it.

## 3.3 The arithmetic of reconciliation

The reconciliation function is the estate's most visible cost and its least defensible one:
its entire output is an explanation of why two numbers that should be equal are not.

Take an illustrative institution, stated so the arithmetic can be checked rather than believed.

| Quantity | Value |
|---|---|
| Systems holding a copy of the position population | 5 |
| Pairs that must reconcile | 10 |
| Reconciliations run per business day | 10 |
| Business days per year | 250 |
| Reconciliation runs per year | 2,500 |
| Runs producing at least one break | ~15% |
| Breaks investigated per year | ~375 |
| Analyst-hours per break, median | 3 |
| Analyst-hours per year on breaks alone | 1,125 |

Eleven hundred hours is a little over half a full-time person, and it is the *floor* — it
excludes the engineering time to build and maintain the reconciliation jobs themselves, the
control framework that attests to them, the month-end escalations, and the standing meeting.
The pairwise term is the one that hurts: five copies is not five times the cost of one, it is
ten reconciliations, and a sixth copy adds five more.

$$\text{pairs} = \binom{n}{2} = \frac{n(n-1)}{2}$$

Six copies is fifteen pairs. Seven is twenty-one. The estate's reconciliation cost grows
quadratically in a quantity that grows monotonically because each addition was locally
justified.

> **Pitfall**
> The standard response to a break is to add a control, and the standard control is another
> reconciliation. This is the only cost structure in the enterprise where the remedy for the
> symptom increases the cause. A seventh copy — the semantic layer that exists to stop two
> dashboards disagreeing — is the purest instance: it is a copy introduced specifically to
> resolve the disagreement between copies.

## 3.4 The operational tax

The second cost is the estate itself, independently of whether it disagrees with itself.

| Line item | What it is |
|---|---|
| JVM analytical cluster | Nodes sized for a peak that occurs at month-end, idle otherwise; a garbage collector to tune; a heap to size |
| Streaming bus | Brokers, partitions, retention, a schema registry, and a consumer-lag alarm nobody can act on at 3 a.m. |
| Connector fleet | A process per source, each with its own offset store, each a place a pipeline can be silently stopped |
| Scheduler | A DAG, a calendar, and a dependency graph nobody has read end to end |
| Graph store | A hydration job, and the question of what happens when it is behind |
| Six upgrade cycles | Each independently versioned, each with its own compatibility matrix |

The tax is paid in headcount and in nights, and its distinguishing feature is that it is
**invisible in any per-component review**. Every component is correctly sized, correctly
operated and individually defensible. The cost is the cardinality.

## 3.5 The lag, stated as a chain

The third cost is time, and it is worth stating as a chain because that is what makes it
irreducible without removing hops rather than tuning them.

A position booked at 16:31 becomes visible to a risk aggregate after: the source commit
becomes durable; the WAL is flushed and decoded; the connector polls and publishes; the
consumer batches to a size worth writing; the warehouse commits; the semantic layer's cache
expires. Each stage has a window sized by a different team against a different objective.

Two properties of that chain matter more than its total.

**It is a sum, so the slowest stage does not dominate.** Four thirty-minute windows are two
hours regardless of how fast the engines are, and no amount of hardware shortens it. Only
removing hops does.

**Its first stage may be outside anybody's control.** [ADR-0002](../../adr/0002-async-commit-and-decoding-visibility.md)
records a genuine floor discovered in this system's own testing: under PostgreSQL's
`synchronous_commit = off`, a transaction returns to the client once its commit record reaches
the WAL *buffer*, while logical decoding reads *flushed* WAL only. There is therefore a window
in which a transaction is durable enough to be visible to ordinary queries on the primary and
entirely invisible to change capture. The measurement that settled it:

```
after insert:  pg_current_wal_lsn       = 4/1137C000
               pg_current_wal_flush_lsn = 4/11375CC8
```

The write position had advanced; the flush position had not. The consequence is the one worth
carrying: the source is healthy, the replication slot is active and valid, rows are queryable,
the pipeline reports no error — and it sees nothing. A freshness budget that does not name WAL
flush as its first stage is a budget with a term missing.

## 3.6 What consolidation removes, and what it does not

This is the part of the argument that is usually skipped, and skipping it is why consolidation
proposals lose to incumbents in the second meeting.

**Removed by construction.**

| Cost | Why it goes |
|---|---|
| Pairwise reconciliation between engines | There is one copy; there is no pair |
| The connector fleet and the broker | Capture is in-process, native, and reads the WAL directly |
| The second and third security model | One policy decision point; one audit chain |
| The representation conversion between engines | One Arrow buffer, shared |
| The graph/SQL disagreement | The graph holds no durable state; an edge exists because a row exists |
| Cache staleness as a failure mode | Every cache key embeds a snapshot identifier, so a new commit produces a miss rather than a stale hit — there is no invalidation protocol to get wrong |

**Not removed, and it is dishonest to claim otherwise.**

| Cost | Why it stays |
|---|---|
| Reconciliation against systems SANKHYA does not hold | An estate with an external general ledger still reconciles to it. Consolidation shrinks *n*, it does not set it to one |
| The upstream freshness floor | §3.5's first stage is the source's configuration, and no downstream design compensates for it |
| The declaration work | Aggregation rules, date provenance and feed shapes are work somebody must do, and Chapter 2 counts it as a cost |
| Single-node capacity limits | One node's memory and cores bound the largest query; Chapter 4 states this as a boundary rather than a roadmap item |
| Blast radius | Three processes fail independently; one does not |
| Migration itself | Force 2 does not stop being true because the target is better. A consolidation is still the two-year project owned by nobody, and this book does not pretend otherwise |

> **Key idea**
> The honest form of the consolidation argument is not "the estate is bad and this is good". It
> is: *the reconciliation cost is quadratic in the number of copies, the copies are added by
> forces that do not stop, and the technical reasons that made one copy impossible stopped being
> true.* Everything else — the operational tax, the lag, the fragmented control — follows from
> the copy count, and shrinking the copy count is the only lever that touches all of them at
> once.

## 3.7 The strongest counter-argument

The best objection to this chapter is not that the estate is fine. It is this:

> *The estate is expensive and the expense is bounded and understood. A consolidated engine is
> a single point of failure, a single vendor, a single scaling ceiling and a single code path
> whose bugs affect every workload simultaneously. You are proposing to trade a known cost for
> an unknown risk.*

That objection is correct in its premises, and the answer is not to deny them but to state what
the design does about each.

- **Single point of failure.** Accepted as real. The mitigations are architectural rather than
  rhetorical: bounded cancellation, a hostile aggregation refused rather than taking the
  process down, and extension code that holds no handle to a catalog, a connection, a file or
  a clock — so the absence of the capability is the enforcement, which is stronger than a check,
  because a check can be wrong. Chapters 5 and 21 give the mechanism.
- **Single vendor.** Answered by open storage, and it is answered mechanically rather than by
  promise. Analytical tables are ordinary Delta tables in a layout that mirrors operational
  naming, and the Delta kernel reads that log in the test suite. Another engine reads these
  tables with no SANKHYA process in the path. The exit from this system is `SELECT`, not an
  export. Chapter 6 gives the layout, including the one place where the property does not hold
  and why (Chapter 12).
- **Single scaling ceiling.** Accepted and stated, not deferred.
  [ADR-0015](../../adr/0015-the-shard-set-seam.md) refuses independently-committed shards for
  v1 *and* for the described v2, because the cheapest correct cross-shard commit is a lock over
  the shard set, which is the forbidden design reached from a different direction. The trigger
  to reopen it is a measured workload, not an anticipated one. Chapter 4 is the boundary.
- **A single code path whose bugs affect everything.** Correct, and it is the argument for the
  discipline in Part V rather than against the architecture. It is also why the four defects of
  2026-08-31 are recorded in this book rather than in a postmortem folder: a consolidated
  engine has to be able to say how it finds the defects its consolidation makes more expensive.

The estate's cost is bounded and understood in the sense that a mortgage is bounded and
understood. That is a reason to know the number, not a reason to keep paying it.
