<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# ADR-0016 — Who may delete a file that more than one table names

**Status:** Accepted · **Date:** 2026-08-31 · **Milestone:** M10 — the design gate, before any implementation
**Builds on:** [ADR-0013](0013-concurrency-and-data-safety.md), [`DEC-15`](../REQUIREMENTS.md), [`DEC-24`](../REQUIREMENTS.md)

## Context

`CREATE TABLE ... CLONE source AT VERSION n` produces a table that reads exactly what the source
read at that version, in constant time and constant space, by **referencing the same Parquet
files rather than copying them**. Writes to either side then diverge: each commits to its own
log, and neither observes the other. At schema scope the same mechanism gives a branch of a
whole warehouse, which is the shape most of the demand takes.

`IMPLEMENTATION_PLAN.md` §13a gates this on an accepted ADR, and says why in one sentence: *"the
failure mode is not a failed query. It is **silent data loss in a table nobody was touching**,
discovered when somebody reads a clone months later."*

That sentence is the whole reason this document exists before any code.

## The premise every reclamation path rests on

Three mechanisms in this warehouse decide that a file may be removed. Each consults **one
table's log**, and each is correct today for the same reason:

| Mechanism | What it removes | Why it is safe today |
|---|---|---|
| Retirement | an input a merge replaced | its grace period ran and no lease of *that table* still holds it |
| Orphan collection | a file *that table's* log has never named | nothing else can name a file under that table's root |
| Purge (`M9`) | source data after verification | the archive is *that table's* archive |

**A file belongs to exactly one table.** Under cloning that premise is false, and each mechanism
becomes a way to delete data a clone is the only remaining reader of.

## Where the loss actually happens, concretely

Orphan collection is the most dangerous of the three, and it is worth walking because the
current code makes it vivid. `MaintenanceService::sweep_orphans_once` lists the files under
**one table's root**, builds `named` from that table's live set, and passes `reachable` as an
**empty set** with a comment saying the age threshold is what protects a file until leases reach
maintenance.

Now clone `entries` at version 40. The clone's log names the origin's files; the files stay
where they are, under the origin's root. A week later the origin's sweeper runs:

- the file is on disk under the origin's root — listed;
- the origin's live set no longer names it, because the origin has compacted past it — not
  `named`;
- `reachable` is empty — not protected;
- it is older than the seven-day threshold — **removed**.

Nothing failed. No query errored. The clone is now missing rows, and the first evidence arrives
whenever somebody next reads that range of it. **From the origin's point of view a file only the
clone still names is indistinguishable from debris**, which is precisely what the plan warns
about.

## Decision 1 — reachability at sweep time, scoped by a lineage record

`reachable` becomes the union of the live sets of every table in the **clone family**: the
transitive closure of the origin and everything cloned from it, walked through a lineage record
each clone writes at creation.

### Why not reference counting

It is exact, and it is exact in the way that matters least. A count is derived state that must
be maintained across clone, drop, compaction, retirement and crash — and derived state that
disagrees with reality is the failure this project keeps finding elsewhere. The asymmetry
settles it:

| The count drifts | Consequence |
|---|---|
| high | a file is never reclaimed — **disk is lost** |
| low | a file a clone still reads is deleted — **data is lost, silently, in a table nobody was touching** |

The second is the exact sentence the gate exists to prevent, and reference counting is the only
one of the three candidates that can produce it. It would also make every clone and every drop a
crash-safe write on a path that maintenance contends with, which `ADR-0013`'s C1 spent a
milestone keeping per-table.

### Why not copy-on-maintenance

Clones never share a file that maintenance wants to touch, so nothing can be deleted from under
one. It is simple, it is safe, and the plan already notes that it *"quietly gives up the constant
space property that motivated the feature"*.

The word doing the work there is **quietly**. A clone's cost would depend on maintenance activity
its owner cannot see: clone a quiet table and pay nothing, clone one that compacts tonight and
pay for the whole table by morning. A feature whose headline property is *constant space* and
whose actual space depends on somebody else's compaction schedule is a feature that will surprise
its users at the worst moment. Refusing it outright would be more honest than that, and we are
not refusing it.

### Why reachability is affordable here, which is usually the objection

The usual objection is that a sweep's cost grows with the number of tables rather than the size
of one. Three things bound it:

1. **The scan is over the clone family, not the warehouse.** A table nobody has cloned has a
   family of one, its reachable set stays empty, and the sweep does exactly what it does
   today — the change is a **no-op for every table that has never been cloned**, which is every
   table that exists.
2. **Reading a log is what the sweep already does.** Reading `n` logs is that same operation `n`
   times, and `n` is the size of one clone family — a handful, not a warehouse.
3. **`DEC-14` puts execution on one node.** There is no distributed agreement to reach; every
   log in the family is local.

### And it fails in the safe direction

A lineage record that is stale or unreadable makes the reachable set *larger* than it needs to
be, so a file is kept that could have been reclaimed. Keeping a file costs disk. This is the
same choice the quarantine reaper makes — *"keeping one is never an error"* — and the same one
the orphan sweeper's age threshold already makes.

## Decision 1a — a clone's log does not name the origin's files at all

Building Decision 1 surfaced a question it had not answered: **how does a clone's log name a file
it did not write?** `AddFile.path` is documented in this repository as *"relative to the table
root, as the protocol requires"*, and the sweeper compares names it produced by stripping a table
root. A clone naming an origin's file has to say so somehow, and the obvious ways are both worse
than they look.

| How | Why not |
|---|---|
| An absolute URI, which the Delta specification does permit | it embeds a filesystem path, so **restore into a different directory silently produces a table whose files are all missing** — and restore-to-a-different-path is an operation this system has |
| A `../`-relative path | the specification says *relative to the root of the table*; where a reader resolves `..` is undefined, so this is a bet on every reader agreeing |

So the clone's log names **none** of them. It records its origin and version as properties
(Decision 1) and contains only the files the clone itself writes afterwards. A read of the clone
splices the origin's live set *at that version* with the clone's own log — which is not a new
mechanism: [ADR-0015](0015-the-shard-set-seam.md) found that `plan_splice` already resolves one
table reference to several sources and proves exact coverage.

**This makes the lifetime question simpler rather than harder.** The origin's sweeper does not
have to normalise another table's paths into its own naming; it asks *"which versions of me does
a clone still read?"* and keeps the live set of each. That is a question about its own log, which
it already reads.

### What this costs, stated plainly

A foreign Delta reader pointed at a clone's directory sees only the files the clone wrote, not
the rows it inherited. **The clone is not independently readable by the kernel**, and the
open-storage claim holds for ordinary tables and not for clones.

**And it costs a read path, which this section failed to say when it was written.** If a clone's
log named the origin's files, the existing read path would have served a clone with no changes at
all. Deciding that it names none of them means *this* engine must splice too --- the origin's
live set at the cloned version, unioned with the clone's own log --- and until that exists a
clone is a table that reads as **empty**.

That omission is worth recording rather than quietly fixing. The decision was argued on
portability and on the lifetime question, both of which it wins; the cost it did not name is a
piece of work, and a decision whose costs are listed incompletely is one somebody re-reads and
mis-weighs. `M10`'s work list gained the item when the gap was found, which was while planning
the soak that would have exercised it.

That is a real cost and it is the right one. The alternative buys kernel-readable clones with
absolute paths that break the moment a warehouse is restored somewhere else — trading a
correctness property for an interoperability one. A clone that must be readable elsewhere is
**materialised**, which is the explicit copy the ADR already requires for a clone that must
travel without its origin, and which produces an ordinary self-contained table.

## Decision 2 — what each maintenance path does about it

**Orphan collection** takes as `reachable` the union of the origin's own live sets at every
version a clone still reads, which by Decision 1a is a question about its own log. The seam is
already there: `plan_orphan_cleanup` accepts `reachable` as a parameter and the service passes an
empty set. The change is at the call site, not in the decision.

**Retirement** already asks whether every lease that could name an input has drained. A clone is
a reader that outlives every lease, so a clone's live set is consulted alongside them: an input
any family member still names is not due, whatever its grace period says.

**Purge** refuses. `M9` detaches a partition and drops it after quarantine; a clone naming those
files would be reading data the registry says was archived and the source says is gone. So **a
table any clone still references is ineligible for purge**, and the refusal names the clones, in
the shape `M9`'s other refusals already use. The alternative — purging anyway and leaving the
clone reading files nothing owns — would break the immutability argument `DEC-24` rests on.

## Decision 3 — what a clone means for the rest of the system

**Backup and restore.** A backup of a clone alone is incomplete by construction: it references
files the origin owns. Backups are therefore taken at warehouse scope and record the lineage, and
**restoring a clone without its origin is refused** rather than restored into a table with
missing files. A clone that must travel alone is materialised first, which is an explicit
operation that copies.

**Tiering.** Covered by Decision 2: a clone is a reader, and purge refuses while one exists.

**The audit chain.** `sankhya-audit` records the data versions an authorization decision saw. For
a clone that is not enough to reconstruct what was read years later, so the lineage — origin and
version — is part of what the chain records. Without it, two clones of the same table at
different versions are indistinguishable in the record.

**Time travel.** A clone's history **begins at its creation**. Asking a clone for a version
before that is refused rather than resolved into the origin's history: answering it would make
the clone appear to have a past it never had, and the origin remains directly queryable for
anybody who wants the real one. The origin's own time travel is unaffected by a clone existing —
which is Decision 1's job to keep true.

## Decision 4 — what is refused

The plan names four. Building the list found three more, and each is a way the same silent loss
arrives from a different direction.

| Refused | Because |
|---|---|
| A clone whose origin has a purge in flight | the files it would name are being detached as it is created |
| A clone across tenants | a clone is a reference, so it is a way to read another tenant's bytes without a grant |
| A clone of a table mid-schema-evolution | the clone would name files under two schemas and belong to neither |
| **A clone at a version the origin no longer retains** | there is nothing to reference; the files were retired |
| **Dropping an origin a clone still references** | the drop is the deletion this ADR exists to prevent, arriving through the front door |
| **Purging a table a clone references** | Decision 2 |
| **Time travel on a clone before its creation** | Decision 3 |

Every one is a refusal rather than a warning, for the reason `M9` settled: a warning defers the
decision to whoever reads the log.

## What would reopen this

- **A clone family large enough that the sweep's log reads are measurable.** The fix is bounded
  and known — cache each member's live set between sweeps, invalidated by its log version — and
  it is not worth building before there is a number.
- **Cross-node execution.** `DEC-14` puts execution on one node; if that changes, "every log in
  the family is local" stops being true and the lifetime question is a distributed one.
- **A clone that must outlive its origin.** Today the answer is materialise-then-drop. If that
  becomes a common request rather than a rare one, copy-on-drop for the shared subset is the
  next design to consider.

## Consequences

- Cloning may be built. The lifetime question has an answer that does not depend on bookkeeping
  being right.
- Every table that has never been cloned is unaffected, in behaviour and in cost.
- The clone family is a new concept the maintenance service must know about, and it is the only
  one: three mechanisms consult it, and none of them gains a second notion of who owns a file.
- Seven refusals must each be built and shown to fail closed, which is what `M10`'s exit criteria
  already ask for.
