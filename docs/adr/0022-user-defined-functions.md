<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# ADR-0022 — A user's own function, written in Python

**Status:** Accepted · **Date:** 2026-09-02 · **Version:** 0.1.0 · **Milestone:** M18 — the design gate, before any implementation
**Status of the system:** Implementation — M0, M1, M3, M4, M7, M10 and M13 complete; M2 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Builds on:** [ADR-0010](0010-external-aggregations.md), [ADR-0020](0020-the-built-in-function-catalogue.md), [ARCHITECTURE](../ARCHITECTURE.md) §5.7

## Context

**Owner question, 2026-09-02:** *"is there a way a user writes python code and that can be added
to kernel? basically a stored proc I think but it should work on both oltp and olap."*

Half of this is already decided. [ADR-0010](0010-external-aggregations.md) settles how a
user-supplied **aggregation** is declared and run: a four-method contract, out of process, over
Arrow IPC, with its determinism *exercised* rather than trusted. `M18` carries it, and the
owner directive of 2026-09-01 extended it to merge functions in Python first, then Rust, C++
and Java.

What is not decided is the **scalar** case — a function of a row rather than of a group — and
the part of the question with a trap in it: *both tiers*.

## Decision 1 — Yes, and it is the same contract one shape wider

A user function is declared, not linked. It arrives as Python, runs **out of process** over
Arrow IPC, and is called with a **batch** rather than a row.

Batch, not row, for the reason that decides everything about the cost: an interpreter boundary
crossed per row is a boundary crossed a hundred million times in a scan, and the same boundary
crossed per batch of ten thousand is crossed ten thousand times. The contract that made an
aggregation viable makes a scalar viable, and by the same argument.

`ADR-0010`'s determinism check applies **more** strongly here, not less. An aggregation is
exercised once at declaration; a scalar is called per row, so a function whose answer depends on
dictionary ordering or a stray `random` seed produces a column that is wrong in a scatter of
places rather than wrong throughout — which is far harder to notice and far harder to explain.

## Decision 2 — One definition, one runtime, and the router adapts

This is the trap, and it is worth stating before the mechanism.

PostgreSQL has `PL/Python`. A user's function *could* be registered there as well, and then the
transactional tier could evaluate it directly — which sounds like exactly what *"works on both
OLTP and OLAP"* asks for.

**It is refused**, for the reason [ADR-0020](0020-the-built-in-function-catalogue.md) Decision 2
refuses a second implementation of a built-in, and the reason is stronger here:

- Two runtimes. `PL/Python` uses the interpreter the database was built against; the
  out-of-process worker uses the one SANKHYA ships. Two interpreter versions.
- Two library sets. A function importing `numpy` needs it installed in both, at versions
  nobody reconciles.
- Two answers, eventually. And the day they differ, which one a user gets depends on how their
  statement happened to route — a decision they cannot see and did not make.

Worse than for a built-in: **the user wrote this code**, and would reasonably assume there is
one of it.

So the rule is the built-ins' rule, unchanged:

> A statement calling a user-defined function is an **analytical statement**. The function is
> defined once, runs one way, and the user never learns which tier answered — because it does
> not change the answer.

`tier_for` already classifies statements this way, and a user function is one more name in the
catalogue it matches against. Nothing new is needed to make the invariant hold.

## Decision 3 — It is a function, not a procedure, and the difference is the writer

The question says *"basically a stored proc"*, and the distinction matters enough to draw.

A stored **procedure** can write. A function computes and returns.

`ARCHITECTURE` P1 says there is **one authoritative writer**, and everything downstream is a
derived, versioned, reproducible projection. A user function that could write would be a second
writer — reached from a query, running out of process, on a node that may not be the one holding
the maintenance lease. Every guarantee that rests on P1 would become conditional on what
somebody's Python did.

So: **a user function reads and returns. It cannot write, and that is not a limitation to be
lifted later.** A user who needs to write is writing to the transactional store, which is what
the transactional store is for.

## Decision 4 — Declared with its shape, so the catalogue can carry it

A declaration names what the built-ins' catalogue entries name — arity, argument shapes, return
shape, a description — because the catalogue is what a binding generates from and what the
router matches against. A function that cannot be described cannot be offered by
`db.fn.<name>`, listed by `functions()`, or routed.

It also declares its **determinism**, and that is checked rather than believed: the same batch
split two ways must give the same bits, exactly as `ADR-0010` requires of a merge.

## Decision 5 — The cost is stated, because it is the reason the built-in catalogue exists

A Python function crosses a process boundary and serialises a batch. Against a Rust built-in
that reads a borrowed slice, that is **two to three orders of magnitude**.

That number is the answer to the obvious question — *why write hundreds of built-ins if users
can write Python?* — and it is why both exist:

| | For |
|---|---|
| A built-in | Anything a hundred people need. Compiled, in process, borrowed slices. |
| A user function | The rule that is *this* firm's, that no catalogue will ever contain, and that would otherwise force the data out of the warehouse entirely. |

A user function is slower than a built-in and enormously faster than fetching a million rows to
a client to do the same thing. That is the comparison that matters, and it is the same one the
catalogue is measured against: **29× fewer bytes** for a value-at-risk computed in the warehouse
rather than in the client.

## What this does not decide

- ~~**Sandboxing and resource limits.**~~ **Decided 2026-09-03 in
  [ADR-0023](0023-the-sandbox-a-user-function-runs-in.md).** The boundary is the operating
  system and never the interpreter; each prohibition names its mechanism; where the mechanism
  does not exist the feature is refused rather than degraded; and creating a function is a grant
  rather than a right.
- **Languages beyond Python.** The owner directive names Rust, C++ and Java to follow. The
  contract is deliberately language-neutral — a batch in, a batch out — so adding one is a
  worker, not a redesign.
- **Where the worker runs** in a multi-node deployment. One node today; `M12` owns the rest.
- **Whether a user function may be materialised into a cuboid.** `ADR-0010` answers it for an
  aggregation, through its state. A scalar has no state, so the question is whether its output
  may be persisted — and that is a cache-invalidation decision, not this one.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
