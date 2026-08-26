# ADR-0002 — Asynchronous commit delays visibility to logical decoding

**Status:** Accepted · **Date:** 2026-08-25 · **Milestone:** M0
**Relates to:** `DEC-09` (freshness is a read-path property), `NFR-REL-03` (no loss)

## Context

Discovered while building the end-to-end capture test. A workload committed
successfully and its rows were visible to ordinary queries, yet the replication slot
reported **no pending changes at all**. The instance was configured with
`synchronous_commit = off` for bulk-load throughput.

The measurement that settled it:

```
after insert:  pg_current_wal_lsn       = 4/1137C000
               pg_current_wal_flush_lsn = 4/11375CC8
```

The write position had advanced; the flush position had not.

## The mechanism

Under asynchronous commit a transaction returns to the client once its commit record
is in the WAL *buffer*, before that buffer reaches disk. **Logical decoding reads
flushed WAL only.** There is therefore a window in which a transaction is:

- durable enough to be visible to ordinary queries on the primary, and
- entirely invisible to change capture.

Under a quiet workload the window closes only when the WAL writer next runs, so it is
bounded by `wal_writer_delay` rather than by anything the capture loop controls.

## Why this matters beyond the test

It is a genuine floor on capture freshness, and it is invisible in every symptom that
usually indicates a problem: the source is healthy, the slot is active and valid, rows
are queryable, and the pipeline reports no error. It simply sees nothing.

It also interacts with the durability chain. The applied position may only advance
after the corresponding commit is durable downstream; if the *upstream* commit was
never durable to begin with, the guarantee is weaker than it appears — a crash could
lose a transaction the client believes committed. That is a property of the source's
configuration, not of SANKHYA, but SANKHYA's stated recovery-point objective inherits
it and must say so rather than quietly assuming otherwise.

## Decision

1. **Document asynchronous commit as reducing the recovery-point objective**, and
   surface it in the diagnostic rather than leaving an operator to discover it.
   `synchronous_commit = off` on a captured instance means acknowledged transactions
   can be lost on crash, and SANKHYA cannot compensate.
2. **The capture loop measures lag against the flush position, not the write
   position.** Comparing against the write position would report permanent lag that no
   amount of consumption could close.
3. **Tests that need to observe a change immediately must wait on the flush position**,
   not merely issue the commit. The helper that does so is retained even though the
   test instance now uses synchronous commit, because it documents the real boundary
   and guards against the setting drifting back.
4. **The bulk-load path may still use asynchronous commit**, since nothing is capturing
   during it. The setting is restored before capture begins.

## Consequences

The freshness budget gains a term that is not under SANKHYA's control. The
requirements' end-to-end latency decomposition must include *WAL flush* as its first
stage, and the capacity model must note that tuning it away trades durability for
latency at the source, which is the operator's decision to make and not ours.
