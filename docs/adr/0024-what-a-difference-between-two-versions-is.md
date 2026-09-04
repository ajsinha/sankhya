<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# ADR-0024 — What a difference between two versions is

**Status:** Accepted · **Date:** 2026-09-03 · **Version:** 0.1.0 · **Milestone:** M20 — the design gate, before any implementation
**Status of the system:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Builds on:** [ADR-0019](0019-named-snapshots.md), [ADR-0013](0013-concurrency-and-data-safety.md)

## Context

`SHOW HISTORY OF <table>` lists every commit and `SET VERSION OF <table> = <n>` reads one. What
neither answers is the question people actually ask between two reporting runs:

> *Did anything change, and how much?*

`M20` was deferred on 2026-09-02 with a specific objection, and it is the right one:

> The log records file-level adds and removes, so a compaction — which rewrites files without
> changing a row — would show as a total replacement. **A diff that reports a compaction as a
> change is worse than no diff, because it looks like an answer.**

So the question this ADR settles is not *how* to diff. It is **what a difference is**, and the
answer decides whether the feature can be cheap.

## Decision 1 — A difference is a change to **rows**, and a compaction is not one

The log already knows. Every add and every remove carries `dataChange`, and a compaction writes
`false` on both sides: it replaces files and changes no row. `history` reports it per commit
already, and `describe` names such a commit *compacted* precisely so a reader does not read a
file count as a row count.

> **A difference is computed from the commits that declared a data change, and from no others.**

That is not a heuristic. It is the writer's own statement about what it did, and every writer in
this system is one of ours.

The alternative — comparing the rows themselves — is refused in Decision 4, and it is worth
saying why the cheap answer is also the *more* correct one here: a row-level comparison of a
compacted table would find no differences either, having read every byte to discover it.

## Decision 2 — It reports what the log can prove, and nothing it would have to guess

Four numbers and a list, all read from the log, none requiring a single Parquet file to be
opened:

| | |
|---|---|
| `commits` | How many commits between the two versions **declared a data change** |
| `rows_added` | The `numRecords` of every file those commits added |
| `rows_removed` | The `numRecords` of every file those commits removed |
| `files_added` / `files_removed` | The same, counted as files |

`rows_added` and `rows_removed` are exact, because `numRecords` is written by the writer that
wrote the file and is the same number the reader will find in it.

**What this deliberately does not report is `rows_changed`.** An update in this system is a
remove and an add, and nothing in the log says the two are the same row. Reporting a *changed*
count would mean guessing which removal pairs with which addition — and the guess would be right
often enough to be trusted and wrong exactly when a key was rewritten, which is the case somebody
is diffing to find. A number that is usually right is worse here than two numbers that are always
right.

## Decision 3 — A compaction between the two versions is **named**, not omitted

Filtering compactions out of the arithmetic is correct and, alone, would produce a second
misleading answer: a diff reporting *nothing changed* over a range in which every file was
rewritten tells the truth about rows and leaves the reader wondering why the table's storage
looks nothing like it did.

So the result carries `compactions`, counted separately. The reader is told **both**: no rows
changed, and the files underneath them were replaced *n* times.

> A number withheld to avoid confusing somebody is a number they will eventually need, and will
> then have to get from a place that has no obligation to be right.

## Decision 4 — A row-level diff is out of scope, and is not deferred, it is **refused for now**

Producing *which rows* differ means reading both versions in full and joining them on a key the
system does not know it has. That is a body of work with its own design — a key declaration, a
join strategy, a bound on the result — and none of it is needed to answer *did anything change*.

More to the point, the two questions have different costs by three or four orders of magnitude,
and putting them behind one name would let somebody ask the expensive one by accident. If a
row-level diff is ever built it gets its own statement and its own refusal when the table has no
key to join on.

## Decision 5 — The range is inclusive of the later version and exclusive of the earlier

`BETWEEN 4 AND 7` means *what happened after 4, up to and including 7* — the changes that would
be new to a reader who last read version 4.

Stated because the other reading is defensible and the two differ by exactly one commit, which is
the difference between a reconciliation that ties out and one that is off by whatever that commit
carried. A range whose endpoints are ambiguous is a range somebody will interpret the other way
once.

A version beyond the log is refused by name rather than clamped to the newest — the same rule
`SET VERSION OF` follows, for the same reason: a caller who asked for a version nobody has must
not be handed a different one silently.

## Consequences

**It is cheap, and that is a property rather than an optimisation.** Reading the log is bounded
by the number of commits, not by the size of the table, so this is answerable on a table nobody
would consider scanning. That is what makes it usable in the place it is wanted — between two
reporting runs, before deciding whether to re-run anything.

**It composes with snapshots.** `ADR-0019` makes a snapshot a named position; this makes the
distance between two positions a number. A reconciliation that quotes a snapshot can now say what
has happened since it.

**It cannot tell you a value changed.** Two commits that between them remove one row and add one
row report exactly that, and a reader who needs to know whether it was the *same* row has to look.
This ADR says so rather than approximating it.

## What this does not decide

- **Whether a row-level diff is ever in scope**, and under what key declaration.
- **Diffing across a clone's lineage.** A clone shares files with its parent, so *what changed
  between this clone and what it was cloned from* is a different question with a cheaper answer,
  and it is not this one.
- **Diffing a cube.** A cube's cells are derived, so their difference is a function of the fact
  table's, and whether that is worth surfacing separately is a question for the day somebody asks
  it.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
