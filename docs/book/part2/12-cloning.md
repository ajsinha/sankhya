# 12. Cloning, lineage and dependents

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> A clone reads exactly what its origin read at a version, in constant time and constant space,
> by referencing the same files rather than copying them. This chapter shows why that one
> sentence invalidates a premise three separate reclamation paths rest on — *a file belongs to
> exactly one table* — and why the failure mode is not a failed query but **silent data loss in a
> table nobody was touching**, discovered when somebody reads a clone months later. It gives the
> mechanism chosen, the two mechanisms refused and why, the seven refusals the feature ships
> with, and the defect that thirteen passing tests could not see.

## 12.1 What a clone is

```sql
CREATE TABLE q3_frozen CLONE sales.orders;
```

The result is a table that reads exactly what `sales.orders` read at the cloned version. Writes to
either side then diverge: each commits to its own log, and neither observes the other. At schema
scope the same mechanism gives a branch of a whole warehouse, which is the shape most of the
demand takes.

It lands in `sales`, beside what it was cloned from. Naming any other schema is **refused**, and
the reason is not a convention: a clone is a *reference* to its origin's files, and the right to
read it derives from the right to read what it references — which is why authorization resolves a
clone through its root. A clone under another schema would have its *name* governed by one policy
and its *data* by another, and nobody could say which rule applied to it.

Two statements answer the questions a clone creates:

```sql
SHOW LINEAGE OF q3_frozen;        -- what it is a clone of, nearest first
SHOW DEPENDENTS OF sales.orders;  -- what still reads it, before you try to drop anything
```

`SHOW DEPENDENTS` answers a question a refusal used to answer too late. Dropping a table that a
clone still reads is refused and names the clones — which is no use to somebody who had no way to
ask first.

## 12.2 The premise that cloning breaks

Three mechanisms in this warehouse decide that a file may be removed. **Each consults one table's
log**, and each is correct today for the same reason.

| Mechanism | What it removes | Why it is safe without clones |
|---|---|---|
| Retirement | An input a merge replaced | Its grace period ran and no lease of *that table* still holds it |
| Orphan collection | A file *that table's* log has never named | Nothing else can name a file under that table's root |
| Purge | Source data after verification | The archive is *that table's* archive |

> **Key idea**
> **A file belongs to exactly one table.** Under cloning that premise is false, and each of the
> three mechanisms becomes a way to delete data a clone is the only remaining reader of.

### Where the loss actually happens

Orphan collection is the most dangerous of the three, and it is worth walking because the code
makes it vivid. The sweeper lists the files under **one table's root**, builds its `named` set from
that table's live set, and passes `reachable` as an **empty set** — with a comment saying the age
threshold is what protects a file until leases reach maintenance.

Now clone `entries` at version 40. The clone's log names the origin's files; the files stay where
they are, under the origin's root. A week later the origin's sweeper runs:

1. The file is on disk under the origin's root — **listed**.
2. The origin's live set no longer names it, because the origin has compacted past it — **not
   `named`**.
3. `reachable` is empty — **not protected**.
4. It is older than the seven-day threshold — **removed**.

Nothing failed. No query errored. The clone is now missing rows, and the first evidence arrives
whenever somebody next reads that range of it.

> **Pitfall**
> **From the origin's point of view, a file only the clone still names is indistinguishable from
> debris.** That is the whole hazard in one sentence, and it is why this feature was design-gated:
> no code was written before [ADR-0016](../../adr/0016-zero-copy-cloning.md) was accepted.

## 12.3 The mechanism, and the two that were refused

### What was chosen

`reachable` becomes the union of the live sets of every table in the **clone family**: the
transitive closure of the origin and everything cloned from it, walked through a lineage record
each clone writes at creation.

### Why not reference counting

It is exact, and it is exact in the way that matters least. A count is derived state that must be
maintained across clone, drop, compaction, retirement and crash — and derived state that disagrees
with reality is the failure this project keeps finding elsewhere. **The asymmetry settles it:**

| The count drifts | Consequence |
|---|---|
| High | A file is never reclaimed — **disk is lost** |
| Low | A file a clone still reads is deleted — **data is lost, silently, in a table nobody was touching** |

The second is the exact sentence the design gate exists to prevent, and reference counting is the
only one of the three candidates that can produce it. It would also make every clone and every
drop a crash-safe write on a path that maintenance contends with, which the concurrency milestone
spent its budget keeping per-table.

### Why not copy-on-maintenance

Under copy-on-maintenance, clones never share a file that maintenance wants to touch, so nothing
can be deleted from under one. It is simple, it is safe, and it *quietly* gives up the
constant-space property that motivated the feature.

**The word doing the work is *quietly*.** A clone's cost would depend on maintenance activity its
owner cannot see: clone a quiet table and pay nothing; clone one that compacts tonight and pay for
the whole table by morning. A feature whose headline property is *constant space*, and whose actual
space depends on somebody else's compaction schedule, will surprise its users at the worst moment.
Refusing the feature outright would be more honest than that.

### Why reachability is affordable, which is the usual objection

The objection is that a sweep's cost grows with the number of tables rather than the size of one.
Three things bound it:

1. **The scan is over the clone family, not the warehouse.** A table nobody has cloned has a family
   of one, its reachable set stays empty, and the sweep does exactly what it does today — the
   change is a **no-op for every table that has never been cloned**, which is every table that
   exists.
2. **Reading a log is what the sweep already does.** Reading *n* logs is that same operation *n*
   times, and *n* is the size of one clone family — a handful, not a warehouse.
3. **Execution is on one node.** There is no distributed agreement to reach; every log in the
   family is local.

And it fails in the safe direction: a lineage record that is stale or unreadable makes the
reachable set *larger* than it needs to be, so a file is kept that could have been reclaimed.
Keeping a file costs disk. That is the same choice the quarantine reaper makes — *keeping one is
never an error* — and the same one the orphan sweeper's age threshold already makes.

## 12.4 A clone's log names none of the origin's files

Building the mechanism surfaced a question it had not answered: **how does a clone's log name a
file it did not write?** File paths in the log are relative to the table root, as the protocol
requires, and the sweeper compares names it produced by stripping a table root. The two obvious
ways out are both worse than they look.

| How | Why not |
|---|---|
| An absolute URI, which the Delta specification does permit | It embeds a filesystem path, so **restore into a different directory silently produces a table whose files are all missing** — and restore-to-a-different-path is an operation this system has |
| A `../`-relative path | The specification says *relative to the root of the table*; where a reader resolves `..` is undefined, so this is a bet on every reader agreeing |

So the clone's log names **none** of them. It records its origin and version as properties, and
contains only the files the clone itself writes afterwards. A read of the clone **splices** the
origin's live set at that version with the clone's own log — which is not a new mechanism: the
splice planner already resolves one table reference to several sources and proves exact coverage
(Chapter 8, §8.3).

> **Key idea**
> This makes the lifetime question *simpler* rather than harder. The origin's sweeper does not have
> to normalise another table's paths into its own naming. It asks *"which versions of me does a
> clone still read?"* and keeps the live set of each — which is a question about its own log, which
> it already reads.

### What it costs, stated plainly

**A foreign Delta reader pointed at a clone's directory sees only the files the clone wrote, not
the rows it inherited.** The clone is not independently readable by the kernel, and **the
open-storage claim holds for ordinary tables and not for clones.**

That is a real cost and it is the right one. The alternative buys kernel-readable clones with
absolute paths that break the moment a warehouse is restored somewhere else — trading a
correctness property for an interoperability one. A clone that must be readable elsewhere is
**materialised**, which is the explicit copy the design already requires for a clone that must
travel without its origin, and which produces an ordinary self-contained table.

> **Pitfall**
> The decision also **cost a read path, and the document that made it failed to say so.** If a
> clone's log named the origin's files, the existing read path would have served a clone with no
> changes at all. Deciding that it names none of them means *this* engine must splice too — and
> until that existed, **a clone was a table that read as empty.** The omission is recorded rather
> than quietly fixed: the decision was argued on portability and on the lifetime question, both of
> which it wins; the cost it did not name was a piece of work. *A decision whose costs are listed
> incompletely is one somebody re-reads and mis-weighs.*

## 12.5 What each maintenance path does about it

**Orphan collection** takes as `reachable` the union of the origin's own live sets at every version
a clone still reads. The seam was already there — the planning function accepts `reachable` as a
parameter and the service passed an empty set — so the change is at the call site, not in the
decision.

**Retirement** already asks whether every lease that could name an input has drained. A clone is a
reader that outlives every lease, so a clone's live set is consulted alongside them: an input any
family member still names is not due, whatever its grace period says.

**Purge refuses.** A purge detaches a partition and drops it after quarantine; a clone naming those
files would be reading data the registry says was archived and the source says is gone. **A table
any clone still references is ineligible for purge**, and the refusal names the clones.

## 12.6 What a clone means for the rest of the system

**Backup and restore.** A backup of a clone alone is incomplete by construction: it references
files the origin owns. Backups are therefore taken at warehouse scope and record the lineage, and
**restoring a clone without its origin is refused** rather than restored into a table with missing
files.

**The audit chain.** The audit records the data versions an authorization decision saw. For a clone
that is not enough to reconstruct what was read years later, so the lineage — origin and version —
is part of what the chain records. Without it, two clones of the same table at different versions
are indistinguishable in the record.

**Time travel.** A clone's history **begins at its creation.** Asking a clone for a version before
that is refused rather than resolved into the origin's history: answering it would make the clone
appear to have a past it never had, and the origin remains directly queryable for anybody who
wants the real one. The origin's own time travel is unaffected by a clone existing — which is the
reachability mechanism's job to keep true.

## 12.7 The seven refusals

The plan named four. Building the list found three more, and each is a way the same silent loss
arrives from a different direction.

| Refused | Because |
|---|---|
| A clone whose origin has a purge in flight | The files it would name are being detached as it is created |
| A clone across tenants | A clone is a reference, so it is a way to read another tenant's bytes without a grant |
| A clone of a table mid-schema-evolution | The clone would name files under two schemas and belong to neither |
| **A clone at a version the origin no longer retains** | There is nothing to reference; the files were retired |
| **Dropping an origin a clone still references** | The drop is the deletion this design exists to prevent, arriving through the front door |
| **Purging a table a clone references** | §12.5 |
| **Time travel on a clone before its creation** | §12.6 |

Every one is a refusal rather than a warning, for the reason settled elsewhere in this system: a
warning defers the decision to whoever reads the log. Tenancy is checked **first**.

Fail-closed is demonstrated by twelve refused paths in one test — five creation refusals, three
removal, one read, two from a lineage that contradicts itself, and five malformed statements —
**with a control beside it**, because every one of those assertions is also satisfied by a function
that refuses everything.

## 12.8 What was measured

The soak's design is the interesting part: it runs the same scenario twice, once with maintenance
*told* about the clone and once without, and the second arm is the control that makes the first
mean something.

| | Inherited rows readable before maintenance | After |
|---|---|---|
| Maintenance knows about the clone | 120 | **120** |
| Maintenance does not | 120 | **0** |

The constant-cost property was measured directly: a clone is **1 file and 410 bytes, from origins
of 5 files and of 121 files.** The first version of that test asserted the two clones were
*identical* and failed by one byte — the version number differs — which is the correct amount of
strictness applied to the wrong property.

## 12.9 What the tests could not see

Six defects were found in this feature, and the last is the most instructive in this book.

**A drop refusal with no call site.** The predicate that answers *"may this table be dropped"* had
existed since an early step with **nothing calling it**, so a table a clone was the only reader of
could still be dropped. That is the deletion the design exists to prevent, and it was reachable.

**A clone could be created and then not touched.** Authorization checked the clone's own name, and
a clone created a moment ago has no policy rule of its own — so its creator could neither clone it
again nor drop it.

**"Is this name taken?" was asked of the policy rather than of the warehouse**, so a second clone
could be created over an existing one.

**A clone read as empty** — the read-path cost of §12.4, arriving as a defect because the decision
had not named it as work.

**A mutation proved the tests were counting the wrong thing.** Row counts came from the log, so
every test passed against a plan that could not open anything.

**And then: cloning had never worked against a table this server serves.** The clone statement
resolved a name as `warehouse/<name>`; discovery reads `warehouse/<schema>/<table>`. So
`CREATE TABLE q3 CLONE orders` on any real deployment answered *"the table to clone declares no
schema"*.

> **Pitfall**
> **Thirteen clone tests passed, because the fixture put its tables at the warehouse root — a
> layout no deployment has.** And the fixture was built through the product's own writer, which is
> the rule that exists to prevent exactly this class of defect, and it *still* encoded a shape the
> product does not use. That is the sharpest of the four defects found in the adversarial review
> of 2026-09-01, and it is the strongest argument in this book for executing through the front
> door with clients that know nothing about the code.

The exit criteria had the same problem in miniature. Walking them found that **two were missing
entirely** — *a clone at constant cost against a large table*, and *every refused clone path shown
to fail closed*. Both were then built. A criterion nobody wrote is a criterion that cannot fail.

## 12.10 A clone is not a snapshot

The distinction is worth stating because the two are easy to confuse and answer different
questions.

| | Clone | Snapshot |
|---|---|---|
| Pins | one table, one version | many tables, one consistent position |
| Appears as | a table in the catalogue | a name a query quotes |
| Read by | its own name | any table's name, *as of* it |
| Cost | one log with no files | one small document |
| Dropping it | a schema change | not |

A clone freezes a **thing**. A snapshot freezes a **moment**. Somebody comparing two quarters
wants clones; somebody who needs four tables to agree wants a snapshot.

They share the reclamation machinery, and deliberately so. Reclamation asks exactly one
question — *"does anything still read this?"* — and both can answer yes. They are unioned into
one input rather than becoming two rules, because two rules disagree eventually and the one that
loses deletes a file somebody is reading.

Snapshots are Chapter 19 §19.7a and [ADR-0019](../../adr/0019-named-snapshots.md).

### Neither of them is version history

A third confusion is worth heading off, because it is the one people arrive with. A snapshot is
a **tag**, not a branch, and `SHOW HISTORY OF` (§19.7b) is a list of commits, not a history you
can walk.

The reason is the same one that shaped this chapter. The log records **files**. A compaction
replaces every file in a table and changes not one row — so a file-level diff between two
versions would report a maintenance job as a total rewrite, and a "restore to version N" would
be restoring a file list rather than a state anybody chose. A diff that reports a compaction as a
change is worse than no diff, because it looks like an answer.

What the log honestly knows is what §19.7b prints: which versions exist, what each did, whether
the writer declared it a data change, and **which of them something is still keeping alive**.
That last column is the one that connects back to this chapter: a clone is one of the two things
that can appear in it. The row-level question is `M20`'s, and it needs a decision before it needs
code.


## 12.11 Status, and what would reopen this

Cloning is **complete**: its design gate was cleared before any code was written, and all five
exit criteria are met. It is described in this project's own status document as the first
milestone in several to close on its own terms — nothing external holds it, no production
deployment, no second machine, no drill against a real archive.

Three things would reopen the design:

- **A clone family large enough that the sweep's log reads are measurable.** The fix is bounded and
  known — cache each member's live set between sweeps, invalidated by its log version — and it is
  not worth building before there is a number.
- **Cross-node execution.** Execution is on one node today; if that changes, *"every log in the
  family is local"* stops being true and the lifetime question becomes a distributed one.
- **A clone that must outlive its origin.** Today the answer is materialise-then-drop. If that
  becomes a common request rather than a rare one, copy-on-drop for the shared subset is the next
  design to consider.

> **Key idea**
> The general lesson of this chapter is not about cloning. It is that **a feature can invalidate a
> premise that nothing states**. *A file belongs to exactly one table* was true, load-bearing in
> three places, and written down nowhere — which is why it survived a design review and had to be
> found by asking what the feature would break. The remedy is the one this book keeps arriving at:
> name the premise, in the document, before the code.
