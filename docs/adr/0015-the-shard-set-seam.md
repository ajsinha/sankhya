<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# ADR-0015 — What a table reference resolves to, and the shard-set seam

**Status:** Accepted · **Date:** 2026-08-30 · **Milestone:** M8 — design only, no implementation
**Builds on:** [`DEC-14`](../REQUIREMENTS.md), [ADR-0013](0013-concurrency-and-data-safety.md)

## Context

Two documents carry the same sentence. `IMPLEMENTATION_PLAN.md` §12.2: *"One seam remains
**designed** here and built later, near-free now and an expensive retrofit: allowing a table
reference to resolve to a shard set."* `DEC-14` says it in the same words. Its sibling seam —
keeping the commit path per-table rather than globally serialized — was promoted to an M8 exit
criterion, measured against a control, and met on 2026-08-29. This one was never designed.

On 2026-08-30 the owner moved §12.2 in its entirety to M12, because criteria 7 and 8 need a
second machine and this project has one. Parking *build* work is safe. Parking an *undesigned
seam* is the precise thing both sentences warn about, so the seam is decided here, before the
parking rather than after it.

## What the sentence assumes, and what the code does

The sentence implies that a table reference resolves to a single thing today, and that teaching
it to resolve to several is the retrofit. Neither half is true.

| | Where | What it already does |
|---|---|---|
| One reference → several sources | `sankhya-plan::plan_splice` | Selects a set of tiers and **proves they cover the query's span exactly once**, refusing on a gap or an overlap rather than answering short |
| Provenance across those sources | `Splice::provenance` | Returns which source answered over which interval, on every result rather than on request |
| Per-file partitioning | `sankhya-table-delta::AddFile::partition` | *"Required, and empty for an unpartitioned table — not absent."* Every file records the partition each column places it in |

A table reference already resolves to a partitioned, multi-source set carrying a coverage proof.
Whatever the retrofit is, it is not teaching resolution to return more than one thing.

## The sentence describes two different features

Once the code is read rather than the plan, "resolve to a shard set" separates into two
readings whose costs differ by an order of magnitude.

**Reading A — a shard is a group of files beneath one log.** Partitioning by key range, hash or
tenant, with every shard's files listed in the same transaction log and claimed by the same
version. DataFusion consumes file groups as partitions natively; `AddFile.partition` already
carries the values that define them; the splice proof is untouched because shards live *inside*
a tier rather than beside one. **This is built.** It costs nothing because it was never absent.

**Reading B — a shard is an independently committed log.** `log::commits(table_root)` reads one
log per table root, and its versions are *"monotone, gapless, and starting at zero."* N shards
under Reading B means N version sequences, and a write spanning shards must claim a version in
each **atomically or not at all**.

That is a distributed commit protocol, and it lands directly on the criterion M8 spent its
budget proving. Exit criterion 1: *"N writers racing for one commit version: exactly one wins
and every loser is told, with no lost commit under sustained contention."* ADR-0013 establishes
that against a **single atomic claim**. Two claims that must both succeed or both fail is a
different theorem with a different proof, and none of that proof exists.

## Decision

1. **A table reference resolves to a set of file groups beneath exactly one log.** Sharding is a
   physical partitioning *below* the commit boundary and never above it.
2. **Reading B is refused, not deferred** — for v1 and for the v2 that `DEC-14` describes.
3. **No code changes now.** That is the finding rather than the convenient answer: under
   Reading A a shard never reaches the splice planner, so `TierRef` does not need a runtime
   identity, `Splice` does not need a second coverage axis, and there is nothing to make
   near-free because nothing is being deferred.

## Why refusing is not deferring the hard part

**`DEC-14`'s own chosen v2 path contradicts Reading B.** It names `datafusion-distributed` as
preferred *"which expresses distribution as exchange operators inside otherwise-normal plans,
preserving the single-node code path."* Exchange operators distribute **execution** across file
groups. They do not ask the catalog for N logs. The seam, as written, would have prepared for a
v2 the same decision rules out.

**`NG-01` already answers the product question.** *"SANKHYA is not a distributed OLTP database;
sharding the customer's ledger is their architectural decision, not ours."*

**Reading B does not lift the ceiling `DEC-14` accepts.** *"The largest single query is bounded
by one node's memory and cores"* is a statement about execution. It is equally true of a table
with one log and a table with eight.

**Reading B costs S1 to buy nothing.** The cheapest correct cross-shard commit is a lock over
the shard set — which is exit criterion 4's forbidden design, re-arrived at from a different
direction. A shard set is a smaller blast radius than a warehouse, and that is a difference of
degree in the one property the milestone was spent establishing is not negotiable.

## What would reopen this

A **measured** workload in which a single table's *write* throughput exceeds one node's commit
path. Read throughput does not qualify: routing with cache affinity already scales it, which is
what §12.2 was for. `DEC-14` requires the trigger to be measured rather than anticipated, and
that requirement carries here unchanged.

## Consequences

- **Parking §12.2 strands nothing.** This was the one item in it that could not be safely
  deferred by ordinary means, and it is now decided rather than postponed.
- **M12 inherits no retrofit** from this decision.
- **The cost is where it should be visible.** Anyone who later wants Reading B is not blocked by
  a resolution layer that failed to anticipate them; they are blocked by exit criterion 1, which
  is the part that is actually expensive.
- **The seam was mislabelled**, and that is the useful output. It was recorded as a *resolution*
  seam in two documents for months. Resolution was never the cost — the commit protocol is —
  and a near-free change to the resolution layer would have bought no part of it. Recording the
  mislabelling matters more than recording the decision, because the next seam described in one
  sentence in two places deserves the same reading of the code before it is budgeted.
