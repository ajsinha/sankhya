<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Build Status

**Updated:** 2026-08-29 · Tracks what is *actually built* against
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).

This document exists because a plan describes intent and a roadmap describes ambition;
neither tells you what runs today. Where the two disagree, this one is right.

---

## Milestone progress

| Milestone | Planned | State |
|---|---|---|
| **M0** Foundations, spikes, walking skeleton | 10–12 ew | **Complete**, merged to `main` |
| **M1** Zero-configuration sync and read-your-own-writes | 14–18 ew | **Complete** |
| **M2** Ingest correctness and durability | 24–28 ew | **Substantially complete** — batching invariants, source-safety ladder, reconciliation, idempotence, crash safety, schema evolution and the backfill handoff all exist and are tested. What remains is the slot *lifecycle* driver and the snapshot *reader* — the correctness contracts are in place, the machinery that runs them on a timer is not |
| **M3** Query engine and storage performance | 28–34 ew | **Complete**, all six exit criteria met — closed 2026-08-26. One criterion was corrected first: it required cancellation inside user code, which does not exist until M4, and that clause moved to M4. Parts of the work breakdown remain unbuilt and are listed under *M3, closed* below |
| **M4** Graph engine and the extension mechanism | 26–32 ew | **Complete.** Every exit criterion met; see below |
| **M5** Tenancy, security and API surfaces | 22–28 ew | **Closed.** Four of five exit criteria met; the fifth needs a second server version to exist. Two of four API surfaces built — the wire protocol and Flight SQL. The control plane and its gateway are **deferred to M6**, because what they expose is built there |
| **M6** Operability, packaging and hardening | 2026-08-28 | **Complete.** Six of seven exit criteria met. Criterion 4 accepted on a forty-five-minute judged run by owner decision — `PASS` over 44 minutes with all seven measures steady, on the first soak to exercise a cube. **Criterion 7 carried into M8**: §10.8's size decision and route table are built and tested; the gRPC transport and every write path are not. See [SOAK.md](SOAK.md) |
| **M7** Multidimensional analysis — cubes, slice/dice, roll-up, consolidation | 2026-08-28 | **Complete.** All eight exit criteria pass against a cube hydrated from a published table. Declared complete once before, on 2026-08-27, and retracted the same day: the hydration path did not exist and every criterion passed on cells its own fixture supplied. Both that gap and the write-only materialisation found on 2026-08-28 are closed. See below, and [ADR-0007](adr/0007-the-cube-model.md) |
| **M8** **Concurrency and data safety** | 2026-08-30 | **Complete on six of eight**, and the other two moved rather than met. S1–S3 and C1–C3 are proven, each concurrency criterion measured against a control taken in the same run. **§12.2 and criteria 7–8 moved whole to M12 on 2026-08-30** by owner decision: both need a second machine, and a recovery objective measured on one host excludes the failures the criterion exists to price. The one item in §12.2 that could not be safely parked — the shard-set seam — was designed first and turned out to be mislabelled; see [ADR-0015](adr/0015-the-shard-set-seam.md). **Rescoped 2026-08-28** by owner directive after an end-to-end audit found a version claim that could lose a commit silently, four files published non-atomically, and three reclamation paths guarding against a proxy rather than against readers. See [ADR-0013](adr/0013-concurrency-and-data-safety.md) |
| **M9** Tiering | 12–16 ew | **In progress**, started 2026-08-30. **Gated** on the drills in [`IMPLEMENTATION_PLAN.md` §13](IMPLEMENTATION_PLAN.md): the restore drill exists from M6 §10.3, the archive attestation drill does not. Gate criterion 1 moved to M11 on 2026-08-28, and **destructive purge stays disabled until M11 clears it** — building the purge path and arming it are two decisions |
| **M10** Zero-copy cloning | 10–14 ew | After M9. **Design-gated: no code before an accepted ADR**, covering shared-file lifetime, the maintenance interaction, and what a clone means for backup, tiering, audit and time travel |
| **M11** Production reconciliation | — | **Not schedulable by development.** Needs a production deployment that does not exist. Holds M9's gate criterion 1 and the arming decision for destructive purge |
| **M12** Scale-out, HA and disaster recovery, then production-like acceptance | 20–26 ew | **Needs a second machine**, which is why it holds M8 §12.2 and criteria 7–8 as of 2026-08-30. The project's exit criteria: 12 h, two machines, 100 GB, 50 readers, 20 writers. `CREATE CUBE` is a blocking dependency here and is **not** hardware-blocked, so it can be built at any point before the run |

---

## Partition fan-out, found by the soak on its first honest run

The fix for the partitioning defect introduced a different one, and `FR-CDC-14` names it:

> A single commit batch SHALL NOT produce unbounded file fan-out. A batch touching many
> partitions MUST NOT write one tiny file per partition **without a guard**.

`Publication::append` wrote one file per partition per batch, with no guard. A 200,000-row
batch spread over ninety days becomes ninety files; a 5,000-row append becomes ninety files of
fifty-five rows.

**Measured, not reasoned about: 32,279 live files across ten tables in four minutes,
averaging 37 KB, against a compaction policy that targets 256 MB.** The soak's judge breached
`live_files` on its own — *"compaction is not keeping up with the write rate… if the peaks
climb, each cycle starts further behind than the last"* — which is the harness doing exactly
what it exists for.

**This is the argument for routing the soak through `sankhya-publish`, demonstrated.** The
previous harness wrote through its own code and reported `PASS` on flat, non-conforming tables
for hours. One run through the shipping write path surfaced a MUST violation in four minutes.

`ARCHITECTURE` §6.4.2 had specified the guards and nothing had built them. `sankhya-publish`
now has `fanout`: a minimum file size below which a partition waits, a deferral age after
which it is written however small (waiting for ever is not deferral, it is loss), a cap on
partitions per commit, and the bulk path — feed an `Accumulator` everything, flush once, and
each partition is written once in full rather than once per input batch.

**The alarm is the part that matters**, and §6.4.2 says so: *"the guards buy time; the alarm
gets the design fixed. Silently absorbing it would be the failure."* `Strain::explain` reports
sustained fan-out and names the cause — a partition granularity finer than the arrival
pattern — because no amount of deferral fixes that.

## Date partitioning, corrected 2026-08-27

Found by being asked whether Delta was following the partition-by-date decision. It was not,
and the tables were **malformed** rather than merely unpartitioned.

Every table declared `partitionColumns: ["sank_data_date"]` and then wrote every file flat at
the table root with `"partitionValues":{}`, against a schema that did not contain the column.
It existed in no location the Delta protocol defines. An external engine reads such a column
as null for every row and prunes nothing — and `CON-08` requires Spark and Trino to read
these tables directly, `FR-STORE-20` requires every analytical table to carry the column and
be partitioned on it.

Corrected in `sankhya-publish`: the column is part of the schema, files land in
`sank_data_date=YYYY-MM-DD/`, every add action carries its partition value, one batch
spanning several dates becomes several files in a **single commit**, and each file carries
the date of the partition it sits in rather than of the row — a stamp disagreeing with its
directory is a table that reconciles differently depending on which a reader trusts.

**It survived because of a test named for the layout that tested a string formatter.**
`the_partition_path_is_what_an_external_engine_expects` publishes nothing and reads nothing.
The replacements assert against the files on disk, the commit log, and the Parquet contents.

### Still not partitioned: the ingest path

`sankhya-ingest` creates tables with no partition columns and writes flat. That is internally
consistent — it declares nothing and delivers nothing — but it means `FR-STORE-20` is met on
the batch publish path and **not on the streaming arrival path, which is where most data
lands**. `FR-STORE-24` is explicit that the column is derived *during ingest* and lives on the
analytical side, which is precisely this path. `Onboarded` carries no date axis, so the
automatic layout selection `ARCHITECTURE` §9.8 describes is absent rather than unwired.

**And there is no timestamp to derive one from.** `_sankhya_commit_ts` is declared as a
system column on every ingested table and written as literal `0` for every row —
`encode.rs` appends zero, and `Mutation` carries `commit_lsn` and no timestamp at all. The
comment beside it says the value is "recorded for human reading only", which it is not: it is
recorded for nothing. So an ingest date axis would have to come from the wall clock at write
time, and that is a decision to take deliberately rather than to discover halfway through the
change.

Not started. Sized here rather than begun, because the CDC commit path carries careful
crash-safety reasoning — sequence-derived names, commit-strictly-after-write, rebasing on
version conflict — and a partitioning change touches all three.


## M8, complete on six of eight

### Where it stands, as of 2026-08-30

The rest of this section is *why*. This is *what*, for somebody picking the work up cold.

| §12.1 Concurrency and data safety | |
|---|---|
| `sankhya-atomicfs`, atomic version claim, `check-atomic-writes` | done |
| `sankhya-leases`, wired through maintenance and the query path | done |
| `check-lock-order` and the two nested-lock sites it found | done |
| Lock striping: `LogCache`, `QueryLog`, `servable` | done — `Hydrated` **deliberately not**, see below |
| `sankhya-testkit` | done, with a measured floor |
| C1–C3, the three measurement criteria | done — measured against a control in the same run |
| Crate hygiene | 55 crates → **51**; `alloc` **wired**, and `api-rest`, `cdc-pg`, `pack` each carry a dated milestone. `ports` is **decided: delete and still present** — the removal was blocked and the disposition is recorded in `xtask/src/surfaces.rs` rather than acted on |

| §12.2 Scale-out — **moved to M12 on 2026-08-30** | |
|---|---|
| gRPC transport, Arrow Flight SQL served | done here — M6's carried criterion 7, partly |
| `sankhya-oltp-pg` supervisor | done here, tested against vendored PostgreSQL 17.11 |
| The shard-set seam | **decided, not moved** — [ADR-0015](adr/0015-the-shard-set-seam.md); no code change was required |
| Leader election, attached mode, executor scale-out, replication, key management, metering | **moved to M12** |
| The REST gateway's transport, and the multi-day soak | **moved to M12** — criterion 8 |

**Exit criteria: six of eight proven.** S1–S3 (no lost commit, no partial read, nothing
deleted while read) have tests and mutations. **C1–C3 are now measured**, each against a
control measured in the same run on the same machine: writers on different tables scale
**4.8×** where the same commits behind one warehouse lock scale **0.91×**; a reader under
write load holds **0.59–0.80** of its idle rate where a reader sharing a lock with the
writers holds **0.00–0.07**; and sixteen writers contending for one table all commit, with a
worst rebase count of eleven.

**The two that are not proven left the milestone on 2026-08-30**, by owner decision, and went
whole to M12 along with §12.2. The reason is a machine, not an estimate.

Most of §12.2 is in fact buildable on one host — leader election, fencing and a lost lease are
proven by contending *processes*, and a partition can be induced with `SIGSTOP` or a firewall
rule. What cannot be produced here is the evidence criterion 7 asks for. It requires *"recovery
objectives measured and published rather than estimated"*, and an objective measured on one box
silently excludes network detection, machine loss and clock skew — publishing it would be the
same species of claim as **a contention threshold set below the contended figure**, which this
repository has shipped twice and which §12.1 exists to have stopped doing. Cross-region
replication is not measurable here by definition.

**M12 was chosen over a new milestone** because M12 already declared the dependency — *"M8 for
the concurrency properties, attached mode and multi-node operation"* — already requires two
machines, and had no work breakdown precisely because it expected §12.2 to supply one. One
blocker, one milestone.

**The seam was the one thing that could not simply be parked**, and it is the reason the move
took a day rather than an edit. Both `IMPLEMENTATION_PLAN.md` and `DEC-14` recorded *"allowing a
table reference to resolve to a shard set"* as near-free now and an expensive retrofit later.
Read against the code, it was **mislabelled**: `plan_splice` already resolves one reference to
several sources and proves exact coverage, and `AddFile.partition` already records every file's
partition values, so the resolution layer was never single-valued. The expensive reading —
shards as independently committed logs — costs a cross-shard commit protocol and lands on exit
criterion 1, not on resolution. It is refused rather than deferred, and **no code change was
required**, which is the finding rather than the convenient answer. [ADR-0015](adr/0015-the-shard-set-seam.md).

**The next piece of work is M9**, not attached mode. When M12 is picked up, attached mode is
still the entry point, because leader election runs against it and everything else runs against
leader election.

### Two decisions a newcomer would otherwise re-litigate

**`Hydrated` was deliberately left alone.** ADR-0013 lists it as a fourth choke point; that was
written from reading the code. Measured, readers against a concurrent writer came in at a ratio
of **0.81** to readers alone --- no contention, because `put` runs on a cache *miss* after a
hydration that dwarfs it. Striping it would make its capacity approximate for no gain.

**Managed PostgreSQL does not unblock leader election.** `REQUIREMENTS.md` records managed mode
as single-node; multi-node uses *attached* mode, and leader election runs against that. The
supervisor was still worth building first, but it is not on that critical path.

### C1 to C3 — the three criteria a global lock would pass, and the control that catches it

S1 to S3 are safety, and one lock over the warehouse satisfies every one of them. That is
the whole reason ADR-0013 states three more beside them, and states them as **measurements**:
*writers to different tables do not contend*, *readers are never blocked by writers*,
*contention on one table degrades gracefully*. A test that asserts the code is correct cannot
tell those apart from the design they forbid.

So each measurement is taken twice in the same run on the same machine --- once as the code
stands, once with the same work serialized through one mutex --- and the assertion is on the
distance between them. The control is not a fake of anything: it is a global serialization
point applied to the real function, which is exactly the shape the criteria forbid. **A test
whose threshold cannot separate the two states is the failure this repository has already
shipped twice**, most recently a contention assertion set *below* the contended figure, which
passed with the defect restored.

| | Measured | Behind one warehouse lock |
|---|---|---|
| **C1**, commits to eight tables against one | **36,972 → 178,259 commits/s (4.82×)** | 37,456 → 33,940 (**0.91×**) |
| **C1**, the same end to end through `Publication` | 4.4× to 5.9× | 0.9× to 1.2× |
| **C2**, a reader's rate under four writers | **0.59 to 0.80** of idle, p99 165 µs → 227 µs | **0.00 to 0.07**, p99 measured in *seconds* |
| **C3**, sixteen writers on one contested version | all sixteen commit, worst rebase count **11** | — |

**C1 is measured twice, and the second one is why.** The end-to-end publish measurement is
what a writer experiences, and it is too blunt to be the only one: with a lock over **just the
commit** --- the narrower defect, and by far the likelier one --- an eight-writer publish still
scales **1.87×**, because encoding Parquet is untouched and is most of a publish. A threshold
of two would have passed that. The claim is about the commit path, so the commit path gets its
own measurement in `sankhya-table-delta`, where the encoding cannot mask it, and the mutation
that puts one lock around every commit in the warehouse is caught there.

**C2 is not 1.00, and saying so is the point.** Four writers encoding Parquet on the same
filesystem cost a reader something real --- page cache, directory metadata, memory bandwidth
--- and a criterion phrased as *flat* invites a test that either lies or is set so loose it
proves nothing. What is ruled out is a reader **waiting** for a writer, and the control arm is
what separates those: 0.7 of idle with a p99 of a fifth of a millisecond, against a reader that
shares a lock with the writers and waits seconds for a turn.

There is a second half to C2 that the ratio cannot state. A reader of the table **being
written** must do more work as commits arrive --- it has a longer log to replay --- so its rate
is not expected to be flat at all. What must hold there is that it is never refused, never
blocked, and never shown a state older than one it has already seen, and that is asserted
directly against `declared_rows` rather than inferred from a rate.

### The third control, added 2026-08-31: the machine itself

The serialized arm answers *"is this path serialized?"*. It cannot answer *"could anything have
scaled here?"* --- and `check-all` runs `cargo test --workspace`, which is dozens of test
binaries holding every core. C1's end-to-end measurement failed there at a load average of 36
on a 24-core machine, scaling 2.47x against a floor of three, with nothing wrong with the code.

That failure mode is worse than it looks. A gate that fails at random is a gate that gets
re-run until it passes, and a threshold nobody trusts is the same defect as a threshold with no
control --- the number stops meaning anything and nobody notices when it starts being wrong.

**The obvious control does not work, and finding out why was the useful part.** The first
attempt ran a workload that shares nothing --- pure arithmetic, same barrier, same thread count
--- on one thread and on eight, and asked whether the machine scaled it. It does. Under fair
scheduling every runnable thread gets an equal share, so eight threads collect eight times what
one thread collects **however oversubscribed the machine is**: that control reported near-linear
scaling at load 36, in the same run where the real write path managed 2.5x. A proxy that cannot
go wrong in the way being tested for is not a control.

So free capacity is read directly instead --- idle jiffies over a 200 ms window, `iowait`
counted as idle because a core blocked on a disk is a core the test could have used. It lives in
`sankhya-testkit::capacity`, because **all three criteria had the same hole**: C1 end-to-end in
`sankhya-publish`, C1's commit path in `sankhya-table-delta` and C2's read latency in
`sankhya-readpath` each guarded on how many cores the machine *has* and none on how many were
free. Two of the three had already failed that way in this repository.

### And that guard was necessary without being sufficient

It sampled once, **before** the arms ran. C3 then failed on the same day with the fix already in
place: the measurement began on an idle machine and finished on a saturated one, because under
`cargo test --workspace` other binaries start and finish continuously. A point-in-time probe
cannot see that.

So the check now brackets the measurement rather than preceding it. `Window::open` refuses a
machine that is already busy, `Window::held` refuses one that *became* busy, and a measurement
whose window did not hold is discarded rather than asserted on --- **a measurement taken on a
machine that became busy during it is not a measurement.** Two versions of this guard have now
been wrong in two different ways, which is worth recording: the first could not go wrong in the
way being tested for, and the second could not see the failure arriving.

The decision is split from the sampling so the rule has a test. `capacity::enough` is a pure
function of *how much idle capacity was found* and *how much is needed*, and it is asserted
directly --- including that an unreadable platform permits the measurement rather than skipping
it everywhere, which would silently stop measuring the criteria on every system that does not
publish free capacity. The sampling cannot be driven from a test without the test becoming the
load it is measuring, which is the problem the mechanism exists to solve, so that half is
verified by hand under an oversubscribed machine.

### A skip nobody could see

The skips were written to be *"loud and by name"*, and they were neither. `eprintln!` inside a
**passing** test goes into libtest's per-test capture and is printed only if the test fails, so
the announcement went nowhere; and `check-tests` prints a bare count on success, so it would not
have carried the line even if libtest had. A criterion could have stopped being measured on
every run, indefinitely, behind a green gate.

Both ends are fixed. `capacity::skipped` writes to the descriptor directly --- the capture is
installed on the print macros rather than on the stream --- and `check-tests` now lists every
skipped measurement under its count, followed by *"green means the rest"*. Verified by running
under an oversubscribed machine without `--nocapture` and reading the lines back.

The measurements are unchanged when the machine is quiet: C1 at 7.81x free against 0.85x behind
one lock, on the run that confirmed the guard does not fire.

### The write path was quadratic in a table's own history

C3 found a defect that had nothing to do with contention, by being a measurement rather than
an assertion. Eight writers on one table ran at **11%** of the rate the same eight reached
across eight tables --- and, worse, at **less than half the rate of a single writer**. Rebasing
was costing more than the parallelism it was buying.

`Publication::next_version` asked `newest_after(root, None)`, which walks from version zero
with one `exists()` probe per version. A table at version *v* costs *v* system calls, a
publisher asks once per append **and once per rebase**, and so a single table's write path was
quadratic in its own history. At sixteen hundred commits that is eight hundred stat calls
before each commit, against a commit that costs about twenty microseconds.

Nothing was wrong with the answer, which is why it survived: `next_version` returned the right
version every time, every test passed, and the cost was invisible to all of them because
**every test had a short log**. The same shape as the lock that was never unsafe --- correct
answers, and the question that finds it is not *"can this corrupt?"*.

The fix is a floor rather than a counter, and the distinction is load-bearing. A counter held
beside the log is a counter that can be wrong about somebody else's commit. A **floor** is a
place to start probing from, and every step of the probe is still a filesystem check --- so a
floor that is stale costs a longer walk, and a floor that is *ahead* of the log, which a
restore from backup would produce, makes the walk answer nothing and is discarded rather than
believed. The contended figure went from 11% to **24–51%** of the uncontended rate, and from
below a single writer to consistently above one.

### The retry budget was set at the p95, and a backoff made the tail worse

With the walk gone, contention got fast enough to expose the next thing: writers being refused
after exhausting their rebase budget while nothing was wrong.

Measured over sixteen hundred real commits with eight writers on one table, the rebase count
is **heavy-tailed** --- a mean of five, a median of three, a p95 of fourteen, a p99 of
twenty-five and a longest run of fifty-three. `append_all_rebasing` was passing **sixteen**,
which sits at the p95: about one append in twenty would have been refused as contention with
nothing contended about it beyond ordinary luck. It is now `REBASE_BUDGET`, two hundred and
fifty-six, which is four times the longest measured run and still bounds a writer that is
genuinely being outpaced.

**A backoff was tried and is not there.** Spinning and yielding before each retry, perturbed
per writer so that losers would not resume in step, moved the mean from 5.0 to 4.5 and moved
the p99 from 25 to 35 --- it made the tail *worse*. The tail is not writers colliding in
lockstep; it is the scheduler, and no amount of politeness in that loop changes it. Recording
the negative result is worth more than the code would have been.

### Three catalogue entries that had not run since a comma went missing

The mutation catalogue is a Python list of tuples. A missing comma between two entries is not
a syntax error --- Python reads the second tuple as an *element* of the first --- so one entry
had swallowed the two that followed it. The outer entry ran `cargo test -p <tuple>`, and the
two it had eaten never ran at all.

It survived because every check in that file only ever looked at the first three fields, which
are strings either way. `--check` reported *all 421 catalogue entries match the source* while
three of them were incapable of proving anything. The check now validates the **shape** of
every entry before it validates its text, and the count is 442: the two that were swallowed,
plus three new ones, then five for cube DDL, five for the attestation drill and six for
tiering eligibility.

### The allocator is installed, and the stranded crates got their decisions

`sankhya-alloc` was the plan's own example of near-free: a counting `GlobalAlloc`, built and
tested, that **nothing installed**, so every allocation figure it exists to provide was
unavailable. It is now the server's global allocator, and `sankhya_memory_in_use_bytes` and
`sankhya_memory_peak_bytes` are sampled at the moment of a scrape --- two atomic loads, which
is cheaper than any timer that would report the previous era.

The first version of that read `crate::ALLOCATOR` directly from the scrape module, and it
broke the build in a way worth recording, because the shape recurs. `scrape.rs` is compiled
twice: once into the server binary, and once into `tests/observability.rs`, which
`#[path]`-includes it in order to drive the real endpoint rather than a copy of it. A
`#[global_allocator]` static exists only in the first of those, so a module that names it
compiles in the binary and fails in the test that proves the binary works. The reference now
goes through `sankhya_alloc::in_use()` and `peak()`, which return `Option`: the binary calls
`announce(&ALLOCATOR)` as its first statement and gets figures, and a test binary running on
the system allocator announces nothing and leaves the two gauges unset --- which is the truth
about a test binary, and better than a zero that reads like a measurement.

The other four are decisions rather than code, and the distinction matters: two of them are
**not M8's work**, and recording them as M8 decisions is how they would have stayed lost.

- **`sankhya-ports` --- delete.** Nothing implements a single trait in it, and its own header
  claims `Clock` and `IdGen` are injected everywhere and enforced by lint, neither of which is
  true. A crate whose documentation asserts a property the workspace does not have is worse
  than an empty one.
- **`sankhya-pack` --- M4 §8.6.** The declarative tier is a *planned* tier, and `ARCHITECTURE`
  expects it to express the substantial majority of a real pack. It is built; the loader that
  reads a bundle directory into a running server is what was never finished. That is M4's
  remainder, not an M8 hygiene decision, and deleting it would discard a milestone's work.
- **`sankhya-cdc-pg` --- M2's remainder.** The slot lifecycle, the lag thresholds and the
  source-safety ladder are built and tested. What is missing is the driver that runs them on a
  timer, which is precisely what M2 already records as outstanding.
- **`sankhya-api-rest` --- M8 §12.2, with criterion 8.** Serving the route table needs an HTTP
  listener, HTTP authentication and a row estimate taken **before** anything is materialised
  --- `deliver` refuses a count taken afterwards, because materialising a result to measure it
  is the cost the cap exists to avoid. That is a feature, and it belongs beside the rest of
  criterion 7.

### A generated document somebody had edited by hand

`check-catalogues` was **failing on `develop`**, and had been. `PLATFORMS.md` carries a header
saying it is generated and must not be edited, and an owner decision from 2026-08-29 --- which
filesystems a warehouse may live on, and that FAT and exFAT may not --- had been written into
it directly.

Two failures at once, and the second is the dangerous one. The gate was red, so nobody could
tell what else was red. And the next `write-catalogues` would have **deleted an owner
decision** silently, leaving no trace of which paragraph went missing. The section now lives in
the generator that produces the file, which is where prose that has to survive belongs.

### §12.1e — a harness that provokes a race, and the floor it cannot reach

`sankhya-testkit`: `Hammer` releases workers on a barrier, oversubscribes the machine four
times so the scheduler preempts *inside* short windows, and joins every worker before returning
results --- the hang designed out rather than remembered. `Until` sets its stop flag on drop, so
a panic releases workers instead of stranding them. `Jitter` is seeded, which is the point:
"we could not reproduce it" is what turns a concurrency bug into a permanent resident.

It is held to its own standard. `the_harness_finds_a_lost_update` runs a racy counter and
**fails if nothing is lost**; its pair runs the same contention against an atomic and asserts
nothing is lost, so the first cannot be satisfied by a harness that merely breaks things.

**And the floor is measured.** It does not catch the defect whose window is two instructions ---
taking an epoch before counting a reader in `leases::pin`. Four times oversubscription does not
help, because `drained` scans hundreds of slots and cannot complete inside a window that narrow
however often the reader is descheduled. So the mutation catalogue carries **no entry** for it:
an entry whose mutation survives is a claim of coverage that does not exist. Reaching that class
needs a scheduler somebody controls --- `loom`, which substitutes its own atomics under a `cfg`
so nothing ships with a hook. **Adopting `loom` is the recorded next step for that class.**

### §12.1 — the version claim is atomic, and four writers publish all at once

`sankhya-atomicfs` is a new layer-0 crate with no dependencies and two functions. `publish`
makes a file visible all at once; `claim` does that **and fails if the name is taken**. The
distinction is the whole of the protocol's concurrency control, and using the first where the
second was meant loses the loser's work in silence --- which is what `commit` was doing.

`commit` now claims through `link(2)`, which returns `EEXIST`, so a loser is told `VersionTaken`
and the rebase loop that has existed since M5 finally runs. The cube catalogue, the
`_last_checkpoint` pointer and the backup manifest are published rather than written onto their
live paths. `check-atomic-writes` refuses the two shapes anywhere else, and `INVARIANTS.md`
carries the rule --- `check-invariants` refused the build until it did.

**The one case the gate found that was already right.** `write_parquet` creates its file and
streams into it, and that is safe for a reason the gate cannot see: no log names the path until
the file is closed, so no reader can ask for it, and a partial file left by a crash is
unreferenced and collected by the orphan sweep. Safety there comes from **ordering**, not from
atomicity. It is excused with that reasoning rather than changed, and routing it through
`publish` would have meant buffering a whole Parquet file in memory.

### The read path stopped serializing on one lock

`LogCache` held a single `Mutex<HashMap<PathBuf, Replay>>` over **every** table --- and held it
**across the filesystem work**: the probe for a newer version and the replay of whatever it
found. Every query on every table took that lock, so a cold replay of a large log blocked
queries against unrelated tables for its whole duration.

Nothing about it was unsafe, which is exactly why it survived: it returned correct answers and
no correctness test could tell. The audit that found the commit defect walked straight past it,
because that audit asked *"can this corrupt?"* and the answer was no. The question that finds
these is *"does this serialize?"*.

Two changes, and the first matters more. The lock is **no longer held across I/O** --- the map
is locked only long enough to find a table's entry, and the reading happens under that table's
own lock. And the map is **striped** across 64 shards. Striping alone would have been the lesser
fix: it reduces how many threads wait, while taking the I/O out of the critical section changes
what they wait for. Two queries on the *same* table still serialize, and must, because they are
advancing one replay.

Measured: while a 400-commit table replays continuously, a second table manages **46,570**
lookups. With one lock put back it manages **618** --- a factor of seventy-five.

**The first version of that test asserted `> 100`**, which is *below* the blocked figure, so it
passed with the global lock restored: a test of contention that could not detect contention.
The threshold is now three thousand, chosen from both measurements rather than from taste.

### PostgreSQL is supervised, and "embedded" means what the requirements say it means

`sankhya-oltp-pg` was one line of source. It now creates, starts, proves ready and stops a
PostgreSQL cluster, and its six tests run against the vendored **17.11** rather than a stub ---
because there is no way to fake a database lifecycle usefully. The failures worth catching are
`initdb` refusing a non-empty directory, a postmaster that has started and is not yet accepting
connections, and a shutdown that leaves a lock file behind, and a stub producing any of those
would be asserting what its author already believed. When the vendored build is absent the
tests **skip loudly and by name** rather than passing quietly.

`REQUIREMENTS.md` DEC-02 settled the claim this implements: PostgreSQL is not linked into the
binary and does not run in-process. It is a child whose whole lifecycle SANKHYA owns, so an
operator sees one process tree and no DBA action. That is defensible; the literal reading of
"embedded" is not.

Four properties are load-bearing and each has a mutation proving its test:

- **`initdb` is never run over an existing cluster.** Re-initialising a live data directory
  destroys the system of record, and a restart is the ordinary case rather than the exception.
- **`listen_addresses = ''`.** The postmaster is reachable only through a Unix socket inside the
  data directory, which is what lets a managed cluster hold the system of record without an
  operator reasoning about firewalls. Asserted by finding the socket, because a setting nobody
  checks is one somebody tidies away.
- **Readiness is asked of the cluster, never remembered.** A supervised child can die without
  telling its parent, and a supervisor that trusts its own bookkeeping reports a database as
  healthy while it is gone.
- **A supervisor that goes away takes its child with it.** Otherwise a panic leaves a postmaster
  owning a data directory nothing owns, and the next start finds the lock held by a process that
  is not its child.

**No new dependency.** Everything drives the vendored programs --- `initdb`, `pg_ctl`,
`pg_isready` --- which are the same ones an operator would run, so the supervisor cannot drift
from what doing it by hand produces. Pooling and migrations need a client library, and that is
an open decision rather than one made by reaching for a crate.

**Managed mode is single-node, deliberately.** `REQUIREMENTS.md` records it as a product
boundary: multi-node uses *attached* mode against an externally managed cluster, and M8's leader
election runs against that. Building the supervisor first is still right --- it is what makes a
single-binary evaluation real, and it needs nothing this workspace does not already have.

### Deadlock is a different problem from a race, and needed a different answer

A race appears under load: run it enough times and the bad interleaving happens, which is why
hammer tests found four of them this milestone. A **deadlock** needs two threads taking two
locks in opposite orders at the same moment --- and while only one call path holds both, there
is no order to reverse and no amount of hammering finds anything.

That is what makes it dangerous rather than merely hard. **The code that holds two locks is not
the bug. The code written six months later that holds them the other way round is**, and by
then the first ordering is one function among thousands and nothing announces it.

So `check-lock-order` does not look for deadlocks. It looks for the precondition --- a place
where two locks are held at once --- and requires each one to be declared with its order. Two
existed, both introduced within hours of the check that found them:

**`QueryLog::record` held the map's read lock across the ring's lock, under a comment saying it
did not.** In edition 2021 a temporary in an `if let` scrutinee lives to the end of the whole
`if let`, so `if let Some(ring) = self.asks.read()...` holds the guard through `ring.lock()`.
Binding it to a `let` first is not a style preference; it is the difference between the comment
being true and being false.

**`CubeCatalog::resolve` held `cubes` across two `declared` acquisitions.** Nothing took them
the other way round, so there was no cycle --- which is exactly the state in which an ordering
gets established by accident and reversed by somebody who never knew it existed.

The check is syntactic and single-function, and says so: it cannot see a lock taken inside a
function called while a guard is held, because that needs a call graph, and a lint that is half
a call graph reports confidently about the half it has. What it catches is the shape that
appeared here twice in one day.

### The cube query path stopped serializing too

`QueryLog::record` runs on **every** cube query and took a write lock over the whole map to
push one entry, so every cube's navigation serialized against every other cube's. Each cube's
ring is now behind its own small lock; recording takes a read lock to find it, releases that,
and holds the ring's lock for the length of a push.

Measured as a ratio on one machine in one run, which is what makes it a measurement of the code
rather than of the hardware: recording to eight **different** cubes against recording to
**one**. With per-cube locks the ratio is **3.19** --- different cubes genuinely do not meet.
With the map's write lock put back it is **1.06**, because then the map lock is the only lock
that matters and eight cubes are as slow as one.

### §12.1c — reclamation waits for readers

`sankhya-leases`, layer 0, no dependencies. Readers do not register *what* they hold --- that
would be a shared set behind a mutex on the read path, which is a GIL with a filesystem accent.
They announce *when they started*, into a slot nobody else writes, with one atomic store. A
sweeper reads the slots and takes the oldest.

That is enough because a reader pins **before** it resolves: a file that stopped being
referenced at epoch *e* is unreachable once every reader that started before *e* has finished,
and a reader that starts later resolves a log that does not name it. Nothing needs to know which
files which reader holds.

The path is wired end to end. `Maintainer::watching` takes the registry, the epoch is marked at
the moment a commit stops referencing a merge's inputs, and `retire_due` waits for it to drain.
`Server::run_statement` pins for the life of a statement, and `main` hands the maintenance
thread **the same registry** --- two would be worse than none, because the sweeper would watch
one nobody announces into, conclude the warehouse idle, and delete files under live queries
while every test of either half passed.

`grace_ticks`, `min_age_ticks` and `CUBOID_DRIFT_TOLERATED` all remain, demoted from the
protection to a **backstop against a leaked announcement**, which is the job they are actually
good at. A registry with a leak and no backstop reclaims nothing for ever, which is the failure
this warehouse has met from the other direction.

### Four defects in the crate built to prevent defects

`slot_for` took its modulus from the constant rather than the actual slot count, so every
registry built at another size indexed past its own array.

**A reader whose slot was already taken was announced nowhere.** When the older pin holding that
slot was released first, every slot read free and a sweeper concluded the warehouse was idle
--- while that reader was inside. That is precisely the failure the crate exists to prevent,
inside the crate written to prevent it.

The epoch was taken *before* the reader was counted, leaving a window in which a reader existed
and was invisible; a hammer test found it in twenty rounds out of four hundred.

And a test that **hung rather than failed**: an assertion inside `thread::scope` left twelve
reader threads spinning on a stop flag nobody would ever set.

### Tests that passed against the behaviour they were named for

Four, now, and they are the most useful thing in this milestone.

**Asking the registry twice observes two different instants.** `drained` and then
`oldest_active` are not one observation, and a reader in its pending window makes the second
answer conservatively. Counting that as a violation made two tests fail two runs in five, and
the flakiness was entirely mine. The property has to be checked against what the readers
themselves record.

**A test that marks and asks in the same instant essentially never drains** while readers churn
--- which is correct behaviour and proves nothing. Real reclamation marks when a file stops
being referenced and deletes on a later tick, so the test has to defer too.

**A test that pins by hand tests the registry, not the server.** The first version of
`a_statement_pins_the_warehouse_for_as_long_as_it_runs` called `leases.pin()` itself and passed
with the pin removed from `run_statement` altogether --- the entire behaviour it was named for.
It now runs real statements in another thread and watches from outside.

**A lease test with a zero grace period has no window to observe.** Retirement then happens in
the same tick as the merge, so there is no moment in which a reader can arrive.

### Two tests that passed against the defect they were named for

Both were caught by mutation testing, and both are the same lesson at different sizes.

**The staging-name test.** A shared staging name lets two writers publish each other's bytes.
The first test asserted the file held *some* writer's body --- which it does, because a small
`fs::write` lands atomically, so the winner still gets a well-formed body belonging to somebody
who was told they lost. The property is that the winner's **own** bytes are on disk, and
showing it needs 256 KB bodies so two interleaved writes cannot both land whole.

**The half-written-read test.** It asserted every read was homogeneous --- all `a` or all `b`.
Writing onto a live path truncates first, so a reader catches a **short** file far more often
than a mixed one, and `all()` on an empty slice is `true`. The test passed against precisely
the defect it was written for until it asserted the **length**.

Neither would have been found by reading the tests. Both were found by mutating the code and
noticing the suite did not care.

## Cube DDL, built 2026-08-30

Not part of a milestone, and built out of order on purpose. `CREATE CUBE` was one of M12's
three dependencies and **the only one hardware did not block** — the other two are a second
machine and the scale-out work that moved there with §12.2. Doing it now costs nothing that
waiting would have saved, and leaves M12 blocked on exactly one thing instead of two.

### What a client can now say

```sql
CREATE CUBE sales FROM orders
  DIMENSION geography FROM regions ON region_id (LEVEL country = country_code, …)
  MEASURE amount (SUM ALONG geography, SUM ALONG period)
  MAINTAINED WITHIN 5 VERSIONS
  PINNED (geography);

DROP CUBE sales;
```

The grammar covers everything the stored form does — levels, parent-child hierarchies,
declared roll-up edges, every additivity rule, the staleness target and pinned shapes — because
a cube a file can declare and a statement cannot is the gap reopened rather than closed.

### Three decisions worth not re-litigating

**The parser sits before the engine, and must be able to say "not mine".** `CREATE CUBE` is not
SQL, so `sqlparser` rejects it before any DataFusion hook could see it, and every extension
point that exists sits downstream of a successful parse. So it is recognised first — which
means the load-bearing test is not that a cube parses but that **everything else in the
language passes through untouched**, including malformed SQL, whose error must come from the
component that owns the language.

**Nothing in the parser validates a cube.** It produces a `Definition` and stops. `validate`
turns that into a `Cube` and reports *every* rejection rather than the first. A second
implementation of those rules in the parser would be a second implementation to disagree with
the first, and the disagreement would show up as a cube a file can declare and a statement
cannot — the very gap being closed.

**There is no `CREATE OR REPLACE CUBE`.** Replacing a cube retires every cuboid it
materialised, and that must not happen because somebody re-ran a script.

### The finding: a drop had to reclaim, or the storage was permanent

`retire_superseded` **deliberately retains** a cuboid whose cube has no known current version —
*"deleting on a guess is how a cache becomes a data loss"* — and that is right for a cube it
merely cannot see. It also means a dropped cube's cuboids would have been kept **forever, on
purpose, by the one mechanism that could have reclaimed them**. Nothing would have errored and
no query would have failed; the directories simply never go away.

`DROP CUBE` is the only moment at which anything knows the difference between *gone* and
*unrecognised*, so `cuboid::retire_cube` reclaims there. It parses each directory name rather
than matching a prefix — the names are length-prefixed precisely so they can be read back — so
dropping `sales` cannot take `sales_archive`'s cuboids with it. There is a mutation for that
one, because a prefix match is the obvious shortcut and it passes every other test.

### Cost

`Server.cubes` became `RwLock<Arc<Vec<Cube>>>`: readers take the lock, clone one pointer and
drop it, so **nothing is held across a hydration**. Writers are DDL and rare; readers are every
statement, which is why it is an `RwLock` rather than a `Mutex` and why the `Arc` is inside it
rather than the `Vec` being cloned per statement.

Fifty new tests — 21 on the grammar, 11 through a running server, 18 on retirement — and 5 new mutations.

## M9, started 2026-08-30 — the gate first

### The archive attestation drill

M9's gate has three criteria. Criterion 1 moved to M11 on 2026-08-28 because it needs a
production deployment. Criterion 2 — the restore drill — was built in M6 §10.3. **Criterion 3,
an archive attestation drill on a non-production archive, did not exist**, and is built now,
before any tiering code.

That ordering is the point rather than a preference. `sankhya-tiering/src/lib.rs` argues it in
its only eleven lines — *"This crate staying empty until then is the gate working, not the gate
being ignored"* — and writing the purge state machine while the gate meant to hold it is
unbuilt would have made that sentence false.

### It attempts the violation rather than reading the configuration

`RSK-28` is *"immutability controls silently removed by a later storage policy change"*, and its
stated detection signal is *"attestation check failing"* — which needs a check that can fail.

An object-lock flag read back as `enabled` is **exactly what a silently-replaced bucket policy
still reports**. Attestation from configuration would pass in precisely the scenario it exists
to catch, which makes it worse than nothing: it turns an unknown into a false assurance. So the
drill writes a probe object and then attempts to overwrite, delete and truncate it, requiring
every one to be refused.

This is the argument [`drill`](../crates/sankhya-backup/src/drill.rs) already settled for
backups, applied to a different claim. A backup is proven restorable by reading it back, not by
checking that a manifest claims a row count.

### Three properties that are easy to get wrong

**An attempt that could not be made is not a pass.** A write that failed because the path was
wrong or credentials were missing has demonstrated nothing, and recording it as a refusal would
let a broken drill certify a store it never touched. Same distinction `Evidence::could_not_start`
draws, and `passed()` requires every violation to have been *attempted*.

**Three violations, asked separately.** Object-lock retention routinely stops an overwrite while
a lifecycle rule expires the object; POSIX does the same thing, since unlink is a property of the
directory rather than the file. A drill that asked "is it immutable" and stopped at the first
refusal would call that protected.

**It refuses to run against production**, and that is a safety property rather than a policy. The
drill is a controlled attempt at corruption: if the control holds nothing happens, and **if the
control is gone the attempt succeeds** — which against real data is the loss the control existed
to prevent, inflicted by the check for it. The assertion is a `_non_production` file inside the
archive, not a command-line flag, because a flag survives in a copied runbook and the copy
eventually runs somewhere it should not.

### Reachable, not merely built

`sankhya-server attest <archive>`, exit `0`/`1`/`2` with "could not attempt" distinct from
"allowed"; evidence appended to `<data-dir>/attestations.log`; and an `archive-attestation`
check in `doctor` at a ninety-day objective — longer than the restore drill's thirty on purpose,
since this is a scheduled exercise and the risk moves at the speed of infrastructure change.

That check is **silent when nothing is archived**, which is every deployment today. `has_archive`
returns `false` and says why in its own doc comment, so the day tiering starts writing archives
the check is turned on in one place rather than discovered missing by somebody wondering why it
never fired.

**What this does not do is clear the gate.** The drill exists; a run against a real
non-production archive is still required, and cannot be produced from development.

Twenty-two new tests and 5 new mutations. One of those mutations survived its first run: the count check in
`passed()` is unreachable through `attest`, which always attempts all three, so no test had
ever built an `Attestation` any other way --- and a short list is exactly what a partial run
or a truncated record produces. `passed()` is a property of a public type rather than of its
one current constructor, and the audit is what said so.

### Step 1 — the policy model and eligibility

`sankhya-tiering` is no longer empty. It holds `policy`: what a tiering policy declares, and
every reason a table may not have one.

**Everything is decided at policy creation.** `FR-TIER-10` is explicit that a type which cannot
round-trip must make a table ineligible *there*, not at purge time — and the reason is what
purge time means: a partition already detached, a verification that cannot complete, and data
that is neither in the source nor provably in the archive.

**Every reason, never the first.** A table failing on four counts reports four refusals.
`FR-TIER-26` requires the planning command to report *"every failing precondition rather than
the first"*, and this is where that starts. Reporting one per attempt is how somebody fixes the
float column, re-runs, learns about the mutable contract, and stops reading the output.

### What "canonical byte encoding" has to mean, and what it excludes

Verification compares source against archive by per-column checksum over a canonical encoding.
For that to mean anything the encoding needs one property:

> **Two values are equal if and only if they encode to the same bytes.**

Both halves fail independently, and only one of them loses data. If equal values encode
differently, a faithful archive is reported as a mismatch and the purge halts on a defect that
is not there — annoying, and safe. If **unequal values encode identically**, a corrupted
archive verifies as faithful and the purge proceeds.

Two logical types break it:

| Type | Why | Refused as |
|---|---|---|
| `Float32`, `Float64` | Breaks it in *both* directions at once: `-0.0 == 0.0` with different bytes, and two `NaN`s can share bytes while comparing unequal | `FloatingPoint` |
| `Json` | Its stored text is not determined by its value — same document, different key order, different bytes | `UnstableTextForm` |

**Neither is normalised, and that is the decision.** Collapsing `-0.0` or canonicalising a JSON
document would make the encoding canonical and make the archive **not byte-faithful to the
source** — which is the property being checked. The verification would then pass while the
archived bytes differed from what was purged. A table needing JSON archived can store it as
`Utf8` and take responsibility for its own canonical form, which is an honest thing to ask.

`canonical_encoding` matches **every** variant rather than listing the bad ones, so adding a
logical type to `sankhya-schema` fails to compile until somebody decides what archiving it
means. A deny-list would admit the new type silently, and the first evidence would be a
checksum mismatch during a purge.

### Two rules that fail towards refusal

**Unvaulted identifiers.** `FR-TIER-25` makes a table carrying them ineligible *by default*,
because tiering converts a cheap erasure into an expensive one. Nothing in this system
classifies a column as a direct identifier, and a classifier guessing from names would be
confidently wrong about `customer_ref` in both directions — so it is an assertion the policy's
author makes, and **the absence of the assertion is a refusal rather than a permission**.

**A table with no columns** is reported alone. Every other rule passes vacuously over an empty
schema — no column has a bad type when there are no columns — and "eligible except for having
no columns" invites somebody to read the rest of the report.

Thirteen tests and 6 mutations.

### Step 3 — the purge state machine, and the two ways in

`FR-TIER-03` makes a claim that is unusual because it is about *reading*: **enumerating the
constructors of the authorization value is a complete audit of every way data can leave the
system of record.** That is true only if the type cannot be built any other way, so `Origin` is
private, there is no `Default`, no `new`, and nothing public to assemble one from.
`Authorization::from_command` and `Authorization::from_schedule` are the list, and `grep`
finding them is the audit.

A boolean parameter would have let any of the jobs `FR-TIER-02` names — maintenance, retention,
compaction, vacuum, expiry — pass `true`, and the audit would then be a search of every call
site rather than of two constructors. Same shape as `Guard` in `sankhya-catalog`, reused
deliberately: it is the one mechanism here that makes *"was this checked?"* a question the
compiler answers.

**A schedule that cannot name its approver cannot construct one.** `FR-TIER-34` requires audit
records to name the service principal *and* the human definer and approver, because *"the
scheduler did it" is not an acceptable audit answer* — so those are constructor arguments, and
the attribution is carried into **every** journal entry rather than once at the start. A journal
read years later has to say who authorised the phase in front of the reader.

### Why the journal is written before the action

`FR-TIER-08` requires every transition to be committed *before* the corresponding real-world
action, and only one of the two orders survives a crash between them.

| Order | A crash leaves | Recovery |
|---|---|---|
| **Write, then act** | a journal entry for something that may not have happened | re-run the phase — safe, and why idempotence is a requirement rather than a nicety |
| Act, then write | a partition detached with nothing recording it | resume believes the phase never ran, re-detaches, and either fails against a partition that is gone or succeeds against a different one |

There is no recovery from the second that does not involve somebody reading storage by hand.
The cost of the first is that a resumed run repeats work it may already have done, and a phase
recorded twice is **the expected case** rather than a fault.

### Verification is unskippable structurally, not by discipline

`FR-TIER-15` says there SHALL be no flag that skips verification, and that *"verification is
structurally absent from every path that could bypass it"*. A `skip_verification: bool` nobody
passes is one merge away from somebody passing it.

So the phases form a chain — `Planned → Verified → Gated → Marked → Detached → Dropped →
Recorded` — and each one's `requires()` names the phase that must precede it. There is no
argument to omit because there is no parameter. A test asserts the property over the whole
order rather than at one step: **every destructive phase has `Verified` somewhere behind it**,
so adding a phase later cannot open a path around it.

`Detached` is where destructive begins. Everything up to `Marked` is undone by doing nothing;
from `Detached` a person is involved. That is the line M11 arms.

**The kill switch is checked when a phase is entered and there is no method to interrupt one.**
`FR-TIER-32` requires it to stop new phases and *never* abort a job mid-detach — so the absence
of an interrupt is the guarantee, not an omission.

### A defect the tests found

`resume` seeded its replay from an empty list, so the first real journal entry looked like a
skipped phase and **every journal it was written to read was refused**. A purge is `Planned` by
existing; nothing is journalled to enter it. Sixteen tests, and three of them failed on it.

Sixteen tests and 5 mutations.

### Step 4 — exhaustive verification

`FR-TIER-09` names three checks and then says the thing that matters: **count equality alone is
not evidence**. A partition of a million rows copied with every value replaced by its default
has the right count. So verification is a row count, primary-key set equality via a Merkle
digest over sorted blocks, *and* per-column checksums over the canonical byte encoding --- all
three, on every run, with no path that computes fewer.

### The corruption a cheaper check cannot see

The obvious implementation checksums each column independently over its own sorted values. It
is order-independent, which is the property that appears to be wanted, and it is blind to this:

> Take two rows and exchange their `amount` values.

Every column's multiset is unchanged, so every independent per-column checksum matches. The row
count matches. The primary-key set matches. An archive in which two accounts' balances have
been swapped verifies as faithful, and the source is then purged.

So rows are sorted by their **encoded primary key** and every column is checksummed in that
order. The result is still independent of the order rows were read in --- which is the real
requirement, since an archive scan and a source scan have no reason to agree on it --- while
remaining a check on the association between a key and its row.

### A hole in comparing source against archive at all

Two scans pointed at the wrong place agree about everything, because there is nothing to
disagree about: same count, same empty key set, same columns. Source-against-archive **passes**,
and the purge detaches a partition nobody read.

The plan already knows how many rows the partition holds --- it is what the blast-radius limit
is computed against --- so `compare` takes it as well, and "both sides scanned nothing" is now
the loudest failure available rather than a pass.

### `FR-TIER-15` moved from the doc comment to the compiler

Step 3 claimed verification was structurally unskippable and delivered half of it: the phase
chain put `Verified` behind every destructive phase, but nothing stopped a caller from
journalling `Verified` without having verified anything.

`Verification::proof` now returns a `Proof` only when the comparison found nothing. It has a
private field, no constructor, no `Default` --- and deliberately no `Clone` or `Copy`, so a
proof cannot be earned once and passed again for the next partition. `Purge::entering` refuses
`Phase::Verified` outright and `Purge::verified` is the only way in. There is no argument to
omit because there is no parameter, and no way to fabricate the evidence because the type does
not offer one.

### Two defects this found in what was already written

**The eligibility rules admitted a table that cannot be verified.** Nothing required a primary
key, and primary-key set equality is not a question that can be asked without one. The refusal
would have arrived at verification time, with the partition already marked --- which is the
exact failure `FR-TIER-10` moved every other type rule to policy creation to avoid. `Column`
now carries `key`, and `Ineligible::NoPrimaryKey` is checked with the rest.

**The state machine's module doc described a design that was never built.** It said each phase
is *"a separate type that can only be built from the previous one"*; what exists is a phase
enum whose `requires()` names its predecessor. The doc was written from the sketch and not
corrected when the shape changed, which is the kind of rot no gate catches --- it was
prose about types that do not exist, in a file that compiles.

### The audit found two tests that were not testing what they said

Both survived their first run. **The type tag**: the test that was meant to cover it compared
`Int32` against `Int64`, whose encodings differ in *length*, so removing the tag changed
nothing --- the property needs two types of the same width, and `TimestampUtc` against
`TimestampLocal` is exactly that pair and the one the tag exists for. **The length prefix**: the
composite-key test used `("ab", "c")` against `("a", "bc")`, which the tag and presence bytes
separate on their own. The prefix only earns its place against a value containing the encoder's
own framing, so the test now uses one --- two distinct keys that concatenate to identical bytes
without it.

Twenty tests and 9 mutations.

### Step 5 — the archival registry

`FR-TIER-16` divides the question in two, and the division is the design. The source catalog is
authority for the **hot** extent --- what is still attached --- and the registry is authority
for the **cold** extent. Neither is authority for both, because they are written by different
things at different times: a partition is detached by a purge and the catalog notices, while
the archive was written before the detach and nothing in the catalog ever knew about it.
Reading one and inferring the other is how a range comes to be served twice, or not at all.

### An overlap is refused where somebody can still explain it

Two entries covering the same rows of the same table are two claims about where those rows are.
There is no rule for choosing between them that is not a guess, and the guess would be made at
query time, when nobody is watching. `Registry::record` refuses the overlap at the moment it
would be created; `Registry::from_entries` does the same on restore, so a restore that would
produce an ambiguous registry fails rather than serving from it.

Ranges are half-open, `[from, until)`. With inclusive bounds two adjacent partitions either
share a day or leave a hole on one, and which of the two happened depends on whoever wrote the
second entry.

### A hole is reported, never assumed hot

`Coverage` walks the wanted range and reports every sub-range no entry claims. `FR-TIER-17`
makes an uncovered range intersecting the predicate a **coverage-gap error** rather than an
empty result, and that is the right severity: a query that quietly returns fewer rows than
exist is the failure tiering is most able to cause and least able to detect.

### Expiry cannot run without asking

`FR-TIER-22` requires snapshot expiry to be *structurally incapable* of removing a snapshot an
entry still references. A function that consults the registry can be called with the
consultation skipped, so `Pins` is a value instead: expiry takes one, the only way to obtain one
is `Registry::pins`, and the registry stops being something expiry remembers to ask. The pin
lifts when the retention basis lapses, and a legal hold outlives the basis --- a hold with an
end date is a retention basis, and the ones that matter do not have one.

### Reconciliation refuses rather than answering plausibly

`FR-TIER-23` runs on startup and after any restore. A range the registry believes cold and the
catalog shows attached is a conflict, and the affected table stops being servable by a unified
query --- `Servable` is a witness, like the verification `Proof`, obtainable only from a
reconciliation with nothing to say about that table. **A table nobody reconciled is not
servable either**, which is the correct answer for a process that has not run the check yet and
the reason this is not a boolean somebody defaults to `true`.

`FR-TIER-24` is the other half: `Registry::delta` is what a restore reports before serving.
The dangerous side is `removed` --- an entry that vanished across a restore is a range the
system now believes was never archived, and the first evidence would be a query answering from
a source that no longer holds it.

### A third eligibility rule, found by needing it

A range has to be *ordered* to be shown covered, and the canonical encoding answers equality
questions rather than ordering ones --- a big-endian `i64` sorts wrongly across zero. So a range
is a pair of ordinals, and a tiering key whose type has no ordinal is now
`Ineligible::TieringKeyNotOrdinal`. `has_ordinal` matches every logical type exhaustively for
the same reason `canonical_encoding` does.

That is the third rule the eligibility check was missing and the second found by building the
thing downstream of it. Both were the same shape: a policy that would have been accepted, and
refused later at a point where refusing costs a detached partition.

The marker written to write-once storage before the detach (`FR-TIER-12`) is one line of text
rather than a serialisation format. Its reader is a person with a copy of an object store and
no build of this software, and a format that needs a parser is a format that needs a *version*
of the parser.

Twenty-two tests and 10 mutations.

### Step 6 — the four-layer purge defence

`DEC-15` states the trap and it is worth restating whole: the capture path replicates deletes,
so an ordinary `DELETE` used to purge tiered data would faithfully propagate and **erase from
the published tier exactly the data the purge was meant to preserve**. The purge would work,
the replication would work, and the archive would be gone.

| | Layer | Where it lives | What it is for |
|---|---|---|---|
| 1 | Purge is detach then drop, never row deletion | `machine::Phase` --- the chain has no delete in it | The property itself |
| 2 | Delete and truncate excluded from the publication | `Ineligible::PublicationPropagatesDeletes`, refused at policy creation | A defective code path cannot propagate what is not published |
| 3 | The applier refuses a delete or truncate in an archived range | `defence::Extents::consider` | A defect upstream of the publication is still caught |
| 4 | A marker committed with the registry change | `defence::Marker` | Provenance, and **never** safety |

**Only the first is load-bearing, and layer 4 says so about itself.** A scheme in which deletes
are emitted and the applier suppresses them between two markers fails if a marker is lost,
reordered, or the applier restarts mid-bracket --- and it fails *open*, by applying the deletes.
Never make a safety property depend on a message arriving. `Marker`'s own `Display` ends with
*"provenance only, and no delete was emitted for it"*, because the place somebody would be
tempted to lean on it is the place to say it.

### Halt, not skip, and not a warning

`FR-TIER-06` is specific: a delete or truncate falling in an archived range is a **fatal alarm,
not a warning and not a skipped record**. Both of the softer options are decisions made where
nobody is: a warning defers the decision to whoever reads the log, and a skip leaves the source
and the published tier permanently disagreeing about a row nobody was told about. So the verdict
type has two values and `Skip` is not one of them.

### Two asymmetries the requirement implies rather than states

**A truncate is fatal whether or not it can be placed.** It names no rows, so there is no key to
compare, and *"every row"* necessarily includes every archived one. Asking which range it landed
in is a category error.

**A delete that cannot be located fails closed.** A delete decoded from a relation with no
replica identity carries no key, and *"we could not tell"* must not become *"apply"*. The cost
of halting on one that turns out to have been outside every archive is an operator's afternoon;
the cost of the other direction is a permanent, undetectable loss of a retained record.

Insert and update are deliberately outside this layer's claim. An update reaching an archived
range is a different failure with a different answer --- `FR-TIER-19`'s compensating entry ---
and folding it in here would make the tripwire the place people argue about corrections.

### Layer 1 audited by enumeration

The phase chain has no delete in it, and the test that says so walks `Phase::ALL` rather than
naming the phases it expects. That is the same shape as `Authorization`'s two constructors: a
phase added later that removed rows would have to get past a test that never heard of it.

The fourth eligibility rule, `PublicationPropagatesDeletes`, is declared rather than observed
and its absence is a refusal --- the same argument as `identifiers_vaulted`. Somebody has to
have looked.

Eleven tests and 5 mutations. One of the five did not compile on its first run --- narrowing the
truncate arm made the match non-exhaustive --- which the catalogue reports rather than counting
as a pass.

### Step 7 — cross-tier query unification, and what "total" is doing in that sentence

`DEC-25`: a user who queries six years of history and silently receives two has been handed a
wrong answer by a system that knew better. So a predicate spanning both tiers is unioned, and
the rule that decides which tier answers which part is **total** --- every point falls into
exactly one of four cases, each with an answer decided here rather than at the point of
surprise.

| Catalog | Registry | Answer |
|---|---|---|
| attached | silent | read hot |
| detached | covers it | read cold |
| attached | covers it | **read hot, exactly once**, and flag the inconsistency separately |
| detached | silent | **fail** with a coverage gap |

The third case is a restored backup resurrecting purged rows. Hot wins and the range is read
*once*, so the failure case does not become double-counting on top of an inconsistency --- and
the inconsistency is reported rather than absorbed, because a query that papers over it is a
query that stops anybody finding out.

The fourth is the loud one. A coverage gap means the rows are in neither tier, which is either a
defect or a purge that lost its registry entry, and **answering without them would be a silently
short answer**. Every hole is named, not the first.

### The property four tests cannot state

The four cases are four tests. What they cannot assert is that there is no fifth, so a property
test generates arbitrary hot and cold extents and an arbitrary predicate and requires that the
plan's segments are disjoint, in key order and cover the predicate *exactly* --- or that the
refusal names holes wholly inside it. That is what "total" means as a checkable claim, and it is
the assertion that fails if a case is ever added without an answer.

### Two seams that would have been easy to get wrong

**The witness is required, not consulted.** `plan` takes the `Servable` from step 5, so a
planner cannot reach it without a reconciliation that had nothing to say about the table.
`FR-TIER-23` stops being something the planner remembers to check.

**Reconciliation and the tie-break run at different times, and the tests say so.** The witness in
these tests is taken against an *empty* hot extent on purpose. Reconciliation happens at startup
and after a restore; the query-time rule has to stay total for disagreements that appear after
it, which is exactly what a resurrected backup produces. A test that could only reach the planner
with the two authorities already agreeing could not reach the case the rule exists for.

### Zero rows affected is a silent wrong answer

`FR-TIER-18` says it in those words, and the refusal is built around it. A statement reaching an
archived range gets a typed error naming the archive and the correction mechanism --- a
compensating entry in the hot tier, or the controlled rewrite that retains the prior version and
records an amendment link. A count of zero would say those rows do not exist. They do; they are
somewhere the statement cannot reach.

A predicate covering the whole declared key domain is flagged, which is the tiering equivalent of
a missing partition filter: it should surface as a warning long before it surfaces as a
forty-minute query.

Twelve tests and 6 mutations. One did not compile at first --- a guarded arm does not count
towards exhaustiveness --- which the catalogue reported rather than scoring as a pass, for the
second time in two steps.

### Step 8 — quarantine, and the two things the reaper must refuse

`DEC-24` prices it: *"it costs a week of disk and buys reversible recovery from a defect
discovered late. Against permanent loss of a retained record, this is the cheapest insurance in
the system."*

**What it insures against is precisely what verification cannot catch.** Verification proves the
archive matches the source at the moment of the copy. It cannot prove the *policy* was right ---
that the range was the one somebody meant, that the tiering key meant what its author thought,
that a timezone did not move a year's boundary. Those are found days later by a person, and the
only thing that helps then is the partition still being on disk.

This is also why `FR-TIER-04` separates detach from drop with quarantine between them. **Detach
is undone by re-attaching; drop is undone by nothing.** A detached partition is a catalog
change --- the files are there, unreferenced --- and putting it back is metadata. Once dropped,
the way back is a restore at best and a rehydration at worst, both of which are operations
somebody schedules rather than performs.

### Age is not a sufficient condition

The reaper refuses two things. The first is obvious: a partition inside its grace period, which
is the mechanism.

The second is not, and it is the one worth having. **If the registry no longer claims the range
--- a restore that lost the entry, an entry withdrawn by hand --- the quarantined copy is the
only copy**, and reaping it on age would be the permanent loss quarantine exists to prevent,
performed by the machinery meant to prevent it. Ten thousand days does not make it safe. Same
shape as the orphan sweeper refusing to reclaim a file a retained snapshot still reaches, and
the registry claiming *something* about the table is not the registry claiming *this* range.

### A grace period of zero is not expressible

`Grace::of(0)` is a refusal rather than a value. A grace of nothing is `FR-TIER-13` not being
implemented rather than being configured, and making it unrepresentable costs one constructor
and removes the setting somebody reaches for when a disk is full at four in the morning. The
default is seven days.

### Re-attachment is one call because it is two invariants

Re-attaching without withdrawing the archival entry leaves a range the registry claims and the
catalog has attached --- the disagreement step 7 has to serve hot and flag. Withdrawing without
re-attaching leaves the range in neither tier --- a coverage gap. Both are recoverable and
neither should be reachable by forgetting a step, so `reattach` does both halves: there is no
order to get wrong because there are not two calls.

Re-attaching a range the registry did not claim reports that rather than returning success:
something already withdrew it, and two facts that disagree should reach a person. After the
grace period the refusal points at rehydration by name, because `FR-TIER-13` promises
re-attachment is *simple* and after the files are gone that promise could only be kept in name.

Eleven tests and 6 mutations.

### Step 9 — rehydration, and the failure with no moment

`FR-TIER-20` names four properties together: a rehydration loads into a schema **excluded from
every publication**, is **never attached to the live parent**, is **read-only**, and carries a
**mandatory expiry**. Each is the sort of property a review confirms on the day and nothing
enforces afterwards, so each is a type rather than a check.

| Property | How |
|---|---|
| Excluded from every publication | `Target::loading_into` refuses a schema not asserted excluded |
| Never attached to the live parent | the same constructor refuses the parent's own schema, and nothing here takes a parent |
| Read-only | `Rehydration` has no method that writes and no mode that is not `ReadOnly` |
| Mandatory expiry | `Expiry` cannot be zero and `Rehydration` has no constructor without one |

`ReadOnly` is a unit type rather than an enum with one variant used today, because an enum
invites a second variant and the second variant is the writable copy the requirement forbids.
And the two schema assertions are checked separately: excluding the live schema from every
publication does not make it a sensible place to load a copy of the parent.

### Why the expiry is the load-bearing one

`RSK-35` is *"rehydrated copies accumulate into a shadow system of record"*, and that failure
**has no moment**. Nobody rehydrates a shadow system of record; they rehydrate one range for one
investigation, and then another, over a multi-year horizon, and each one is individually
reasonable. There is no day on which somebody could have decided otherwise --- which is exactly
why it cannot be a decision made per rehydration.

So an expiry of zero is unrepresentable, and one longer than ninety days is refused as well.
That second limit is not a safety property --- a person can rehydrate again --- but a bound on
how far a single decision reaches. A copy granted for years is the multi-year risk taken in one
step.

### Two reapers that look alike and are opposites

The quarantine reaper weighs age against whether the registry still claims the range, because a
quarantined partition may be the only copy there is. The rehydration reaper weighs nothing: a
rehydrated copy is a copy of an archive that still exists, so dropping it loses nothing and
**keeping** it is the risk. Accumulation is reported as count *and* age of the oldest, because
one copy held for a year and fifty held for a day are different problems and neither is visible
in the other's number.

### A correction is a reversal, not an erasure

`FR-TIER-19` defaults corrections to a compensating entry in the hot tier referencing the
original, which is how record-keeping already works: a posted entry is reversed, not erased. It
preserves the audit trail completely and is available whatever the archive's immutability
controls say, because it touches nothing archived.

Controlled rewrite exists for the cases that need it and cannot be constructed without the two
things that make it survivable. A rewrite that retains no prior version is indistinguishable
from the archive having always said the new thing --- which is the property archives exist to
have --- and one that records no amendment link is a corrected archive that does not say it was
corrected.

Fourteen tests and 8 mutations.

### Step 10 — whole-table migration, and what a table's name is worth

`FR-TIER-21` is short and easy to underrate: after migrating a table whole to the published tier
it *"remains visible in the catalog under the same name, backed by the published tier, marked
cold and read-only"*, because **a table that vanishes breaks every downstream tool and saved
query**.

A table nobody has written to for four years is still named in dashboards, in a report somebody
runs each quarter, in a view three other views are built on, and in a query somebody pastes from
a wiki page. Dropping the name turns one storage decision into a morning of unrelated failures
in places nobody connected to tiering.

So `migrate` produces a `Cold` table rather than removing anything, and `Cold` carries the
original name because its constructor is given one name and uses it for both sides. Renaming
during a migration would have to be written on purpose.

### The trap the requirement creates

The table stays visible. That is the point, and it is also the danger: **a visible table whose
archive covers four of its five years answers four years of questions without mentioning the
fifth.** `FR-TIER-17` calls the general form a coverage gap and makes it an error; the same rule
applies here one step earlier, before the migration rather than at each query.

`migrate` therefore asks the registry to cover the table's whole declared key domain and refuses
with every hole. It reuses `Registry::coverage` rather than reimplementing it, so the definition
of *covered* cannot drift between the two places that depend on it.

An empty declared domain is refused separately. *"Wholly archived"* over an empty domain is a
claim about no rows that an empty registry satisfies, which would migrate a table nobody checked.

`Cold::writable` is `const fn` returning `false` with no path that sets it. A migrated table with
a writable state would be a table whose rows are in an immutable archive and whose catalog says
otherwise — and because every key of the domain is inside an archived range, `FR-TIER-18`'s typed
refusal already applies at every point of it rather than depending on a flag being read.

Eight tests and 4 mutations.

### Step 11a — the command surface, the plan digest, and seven permissions

`FR-TIER-26` says the planning command *"SHALL always be a dry run"*, and **always** is the word
doing the work. A `--dry-run` flag defaulting to true is one argument away from not being one, so
`propose` returns a `Proposal` and a `Proposal` has no method that does anything. The path that
acts starts at `clear`, which cannot be reached without a digest, and the digest cannot be
produced except by planning.

### What the digest binds, said as the mistake it catches

An approval is read by a person and the arguments are typed by a machine, and **between those two
the ranges are where a mistake hides.** A digest that were merely a random token would say
*"somebody planned something recently"*. This one is taken over the cluster, the policy, the
table and every range in order, so it says *"somebody planned **this**"* --- and a plan approved
for `[0, 200)` cannot authorise `[0, 300)` by editing the command line.

The cluster is inside the digest as well as being asserted separately, which is not redundancy: a
plan approved on staging would otherwise authorise the same ranges on production, defeating the
cluster assertion with the thing meant to complement it.

**It expires because a plan is a statement about a table's contents at a moment.** Rows arrive; a
boundary outside the retention basis this morning is inside it tonight. An approval with no
expiry approves whatever the table holds when somebody gets round to running it, which is not
what the approver read.

### One place that reports everything, and one that reports the first thing

The two are opposite on purpose. A plan's failing preconditions are reported **all together**,
because an operator fixes them in one sitting --- that is `FR-TIER-26`, and it is the same
argument the eligibility rules make. An invocation's checks return the **first** failure, because
they answer *"should this run at all"* and there is nothing to fix in a runbook that names the
wrong cluster.

### Separation of duty is checked against the policy, not in the abstract

`FR-TIER-33` names seven distinct permissions and one prohibition: a policy's definer may not
also approve it or execute a purge under it. **The person this catches is not a malicious one.**
It is a competent one working alone at the end of a long day, who writes a policy with a boundary
a day out and then approves their own work because they are the person who understands it. Every
step is reasonable and the review that was supposed to happen did not, because it was the same
person twice.

Holding both permissions is ordinary in a small team, so the conflict is between a definer and an
approver **of the same policy** rather than between two permissions in the abstract. `Execute` is
included alongside approval and purge because the difference between executing a plan and
executing a purge is one of arguments rather than of permissions, and a rule that read the
arguments would be a rule somebody has to apply correctly at each call site.

Seventeen tests and 10 mutations.

### Step 11b — the schedule, the anomaly guard, and the evidence pack

`FR-TIER-31` states the recommended production configuration, and it sounds like a compromise
and is not: **continuous automatic archive and verification, with purge performed deliberately by
a human.** That is the valuable half --- continuous machine proof that the published copy is
complete and correct --- at none of the risk. A deployment that never advances past `Archive`
still gets most of the benefit.

| Gate | Effect | Reversible |
|---|---|---|
| **Archive** | copy, verify, tag; nothing is removed | fully --- a no-op on the source |
| **Purge** | detach; data leaves the live table and stays on disk | trivially --- re-attach |
| **Drop** | remove from quarantine | **never** |

So a new schedule is disabled, unapproved, and stops at `Archive`. Each of those is a default
because **the safe configuration should be what somebody gets by not deciding.**

### What the anomaly guard is looking for, and why the median

`FR-TIER-29` names three failures --- a clock error, a timezone defect, a mis-edited policy ---
and they share a shape. **Nothing is broken.** The code is correct, the policy is valid, the
schedule fires on time, and the number of rows in scope is wrong by orders of magnitude because a
boundary moved. No correctness check can see that; only the size can.

The comparison is against the **trailing median, not the mean**, because the mean is moved by the
very outlier being looked for: one enormous run drags the average up and makes the next enormous
run look ordinary. A test pins that with a history whose mean is 102 and median is 2.

Two cases that a naive guard gets wrong, both tested. **A schedule's first live run has no
history**, and halting it would mean no schedule could ever have a second run --- the approval of
the plan digest stands in for the comparison there. And **a history of empty runs** gives a median
of zero, which times any factor is zero, so a naive comparison halts on the first range a
schedule moves after a quiet week.

### Blast radius stops cleanly rather than refusing

`FR-TIER-30` applies limits per run **and** cumulatively per day: per-run alone is defeated by a
schedule that fires hourly, and per-day alone lets one run take the whole allowance in a single
mistake. On reaching one the run stops *at* the limit and reports which one --- a schedule that
refuses outright when it is one range over makes no progress at all, and an operator who has to
raise a limit to get any work done raises it too far. The report names the limit that actually
bound, because an operator raising the wrong number learns nothing.

### The evidence pack, and every word of "alone"

`FR-TIER-35`: a signed evidence pack per archive, **generatable years later from the write-once
manifest alone**. Not from the registry, which lives in a database that may not exist; not from
this software, which may not build; not from a key server or a runbook. The question is *"here is
an archive and a marker --- what is this, where did it come from, who authorised it, and is it
intact?"*, asked by somebody who was not there about a system nobody still runs.

That forced a change to the marker from step 5. It was one line, argued as being for a person
rather than a parser --- and a pack generatable from it alone has to be machine-readable too. It
is now `key=value`, one per line: a value containing spaces (a retention basis is a sentence)
needs no escaping, the first `=` is the separator so a value containing one needs none either,
and **an unknown key is ignored rather than rejected**, which is the only way a reader written
today survives a marker written in 2031.

### The seal, and why it is written out

The workspace has no signing dependency, and adding one unreviewed at this hour is a larger
decision than it looks. `HMAC-SHA256` over a hash already carrying the audit chain and the Merkle
tree is a standard construction rather than an invention --- and the way to make an
implementation of one trustworthy is not care but **published test vectors**. `RFC 4231`'s cases
are asserted directly, including case 6, where the key is longer than the block and must be
hashed rather than truncated, which is the branch implementations get wrong.

A keyed seal rather than a public-key signature is the honest fit for what this proves: the
reader is the organisation that made the archive, checking its own record has not been altered.
The seal is taken over the pack's content sorted by key, so a marker re-emitted with its lines
rearranged is the same evidence and seals identically --- otherwise re-writing a manifest would
invalidate its own seal.

Twenty-eight tests and 14 mutations.

## M7, complete

Added 2026-08-27 by owner directive and placed before scale-out: cubes are a stated
differentiator and multi-node deployment is table stakes.

### What was built

| Crate | Layer | What it holds |
|---|---|---|
| `sankhya-cube-algo` | 1 | The additivity algebra, ancestor answerability, hierarchy consolidation, the cuboid lattice and greedy selection. No dependencies |
| `sankhya-cube` | 3 | The validated `Cube`, consolidation on the graph engine, the sparse cube and its navigation, completeness, materialisation, the write-back overlay |
| `sankhya-cube-sql` | 4 | Roll-up and slice as table functions, with everything that qualifies a number as a column |

### The three findings worth keeping

**Criterion 3a was not met, and the cause was arithmetic.** A cube rolls up in stages, every
stage rounds, and `round(round(a+b) + round(c+d))` is not `round(a+b+c+d)`. Fixing the
*order* of summation makes one reduction reproducible; it does nothing about
**associativity**, and a materialised cuboid is exactly a re-association of the same
addition. Measured at one ULP — large enough for two reports to disagree by a penny, small
enough that nobody can point at a defect. A materialised aggregate now stores its value
unrounded as a Shewchuk expansion, and rounds once when read.

**Criterion 2 is met in a stronger form than its own wording.** It asks that summing a
semi-additive measure across time be *rejected at planning time*. It is not rejected — it is
**not expressible**, because the reduction operator is the measure's and never the caller's.

**Completeness cannot be computed from what survived.** A withheld row leaves no trace, so
an aggregate counting what arrived and dividing by what arrived reports itself complete
however much policy removed. The withheld count comes from the filter or it does not exist.

### The gap that was found by being asked, and then closed

**The hydration path did not exist.** A `Definition` named a fact table and validated that
the name was well formed; nothing read it. Every `Cells` in existence was built by a
navigation operation or a test fixture, so the cube was an algebra with a SQL façade over
data the caller supplied. All eight exit criteria passed, and every one of them supplied its
own cells — which is precisely why passing them did not surface it. **A criterion that never
has to read a published table cannot tell you whether the cube can.**

`hydrate.rs` and `publish_from_fact_table` now close it: the cube reads the table its
definition names, through the same session that will query it. `tests/end_to_end.rs` supplies
a *table* and makes the cube find it, which is the test whose absence allowed the claim.

A row that cannot be placed — a null key, a null measure — is **counted, never dropped**, and
returned as a `Completeness`. Skipping such rows leaves totals quietly short, which is the
same failure as a policy-filtered total presented as complete, so it gets the same machinery.

**A related defect in the same area.** The SQL surface computed `Completeness::complete(rows
that survived)`, which reports complete however much was lost — the exact trap
`sankhya_cube::complete` documents, implemented one crate away from the warning. Completeness
now comes from hydration and is a required field on `Published`, so a fixture cannot quietly
claim a cube saw all of its input.

### What is still not there

MDX, deliberately — see ADR-0007.

The query log now exists (`sankhya-cube/src/querylog.rs`), so §11.6's greedy selection runs
against what people have actually asked for rather than against the whole lattice. A bounded
ring per cube, where the repetition *is* the weighting: a shape asked ten times appears ten
times and counts ten times, recency falls out of old entries being overwritten, and a cube
nobody has queried gets its base cuboid and nothing else. It records a shape — which cube,
which dimensions were grouped by — and has nowhere to put a member, a predicate or a
principal, which is worth asserting rather than assuming of a query log.

### Materialisation was write-only, and four things were wrong at once

Found 2026-08-28 by asking the compiler which methods the server never calls. The answer was
`materialised` and `cuboid_root` — **so the refresher built cuboids on a timer and nothing
ever read one.** The storage was spent, the target lag was checked, the sweep collected the
old ones, and every query went to the fact table regardless. The seventh instance of a
capability that is built, tested and unreachable, and the most expensive, because this one was
also writing files.

Four defects, and each hid the next:

**The test that should have caught it was named for the behaviour and did not test it.**
`a_materialised_cuboid_answers_without_reading_the_fact_table` said in its own comment
*"Proved by taking the fact table away. If the answer still comes back, it did not come from
there"* — and it never took the fact table away and never re-queried. It wrote a cuboid by
hand and asserted the file existed. It now does what it says, and the first rewrite of it
still failed to: a second connection to the same process is served from the in-memory cache,
so the fact table can be deleted and the answer still arrives with no cuboid involved. It
takes a **restart**, which is the thing a cuboid is actually for.

**A cuboid could not say how much of the fact table reached it.** `Completeness` was not
stored, so a cuboid read back could only ever be served as complete — the exact trap
`sankhya_cube::complete` documents. It is now two columns in the stored batch. Not Arrow
schema metadata: a probe wrote a batch with metadata through the product's writer, read it
back through DataFusion and got `{}`. Storing it where a reader cannot see it would have been
worse than not storing it, because the absence would have looked like a value.

**The unrestricted cuboid could serve nobody.** The refresher writes scope 0, and
`Guard::scope_digest` hashes the tenant and the table, so no real caller's digest is ever 0
and the lookup could only miss. The rule from ADR-0008 is now stated as what it means — an
unrestricted cuboid may serve a caller whose guard **withholds nothing** — and asked of the
guard rather than of a digest comparison that cannot succeed.

**The provenance column reported its own input.** `materialised` was filled from the
`materialise` argument the caller passed, so the column answering *"why was this fast?"*
answered with whatever the query had typed. It agreed with reality by accident for the whole
of M7, because nothing served a cuboid and the honest answer was `false` for every query ever
run. It now reports `Published::from_cuboid`.

### A journey test that never ran

Found by the same question. `crates/sankhya-server/tests/five_minutes.rs` — the seven-step
first-user journey, and the file whose own header says *"this test runs on every build and
proves the path works"* — had **no `#[test]` attribute on its function**. Nothing ran it. It
passes in 0.05 s and always would have.

Worth recording next to the cuboid finding because it is the same failure in a different
place: a claim about verification that nothing was checking. The file even contains the line
*"a budget that can never be reached is documentation with a `#[test]` attribute on it"*,
which it managed to be the inverse of.

### Tutorials, and why they are executed

`docs/tutorials/` was added: four hands-on documents covering a first cube, making one fast,
completeness under policy, and every refusal with what to do instead.

Their SQL is executed by the same test as the guide's, and a second test asserts that every
file in `docs/tutorials/` is in that list — so a tutorial cannot be added and quietly left
unverified. A tutorial is the document a reader trusts most, because they are following it
step by step with no independent way to tell a stale instruction from a current one, so an
untested one rots in the worst place available.

The first draft proved the point immediately: it used `cube_names()`, which does not exist,
and `by=region, period`, which parses `period` as a separate option because the options string
is itself comma-separated. Both were caught by the test on the first run.

### §11.6's three levels of control, wired

| Level | Who sets it | How |
|---|---|---|
| Definition | whoever models the cube | `pinning(["region"])`, persisted in the catalogue |
| Configuration | the operator | `cubes.budget_rows`, read at startup |
| Session | the caller | `materialise=false` or `materialise=pinned` on the query |

The session level only ever narrows. There is no spelling that widens anything, because a
caller who could raise the budget would be granting themselves an operator's storage — a
resource exhaustion with a polite interface.

`materialise=false` also bypasses the in-memory cache when what is cached came from a cuboid.
Without that the reproducibility check compares a cuboid against itself and agrees, which is
the one way it can fail to do its job.

Two smaller things fell out. `Arguments::boolean` became dead once the provenance column
stopped reading it, and was removed rather than left as a helper nothing calls. And
`materialise`'s *value* is now validated: `args.rs` refuses an unknown option name, but a
value it never reads is never checked, so `materialise=pinnd` would have passed the name check
and silently taken its default — the failure unknown-option refusal exists to prevent,
arriving through the value instead of the name.

Four ADRs were written for what comes next, and each was prompted by a question worth
recording: [ADR-0008](adr/0008-serving-cubes-under-policy.md) on serving under policy,
[ADR-0009](adr/0009-the-cube-lifecycle.md) on the three lifetimes,
[ADR-0010](adr/0010-external-aggregations.md) and
[ADR-0011](adr/0011-sdaf-declared-dependencies.md) on external aggregations that declare what
they need, and [ADR-0012](adr/0012-open-capabilities.md) on what a standing artefact must
declare before the system will maintain it on somebody's behalf.

### A cube that could only exist at compile time

Cube definitions were *"registered against a session by the embedding application"*, so a
cube lasted exactly as long as a process and a server could not serve one. That read like a
missing storage layer. It was not.

`Measure` held `name: &'static str` and `rules: &'static [Along]`. **A measure was a
compile-time construct**, so a definition could name only measures a Rust source file had
already spelled out, and no amount of persistence code could have loaded one — a definition
read from disk has nowhere to put its own measure's name. The persistence gap was a symptom
and the type was the cause.

Both are owned now, which cost an allocation per declared measure, once, when a definition is
built. `catalogue.rs` writes a definition to `<warehouse>/_cubes/<name>.json` and reads it
back, under an underscore so table discovery and the orphan sweep already skip it — a cube
definition is not data and must never be mistaken for a table.

The stored form is its own type rather than `Serialize` on the model. `sankhya-cube-algo` has
zero dependencies and serde would end that; and a stored definition is a *format*, so
deriving it from an internal struct silently promises never to rename a field. Here a
refactor breaks a compile instead of orphaning every cube on disk.

**An unknown rule name is refused, never defaulted.** A `Last` that came back as `Sum` turns a
closing balance into the sum of twelve month-end balances — right magnitude, right sign,
entirely wrong — and defaulting is precisely the failure the additivity model exists to
prevent. A catalogue file that will not parse is reported with its path rather than skipped,
because a server that comes up healthy with a cube missing sends somebody to the wrong place.

### The server knows its cubes, and cannot yet answer with them

The catalogue existed and nothing read it — the same shape `sankhya-maintenance` was in that
morning, and a capability nothing in production reaches is indistinguishable from one that
was never built. The server now loads every definition at startup, **validates** it, and
reports the count and any failure beside the tables that would not open. A malformed
definition is a complaint, not an outage: one bad JSON file must not stop a server whose other
cubes and every table are fine.

Validating at startup rather than at first use is the point. A measure with no rule along a
declared dimension is exactly what the additivity model exists to catch, and catching it when
somebody runs a query means reporting a deployment error to a user who did nothing wrong, at
whatever hour they happened to ask.

### Cubes answer, under the policy the caller is subject to

A cube is queryable. `cube_rollup` and `cube_slice` are registered into the session a
statement runs in, and hydration reads the fact table **through that session** --- so the
cells a cube is built from are filtered by the same `SecuredTable` that filters a plain
`SELECT`.

That is the whole authorization story, and the absence is the point: there is no
cube-specific authorization code, so there is no second implementation of the rule to
disagree with the first. Two principals with different entitlements get different totals, and
the test asserting it is the one that would fail loudest if that ever stopped being true ---
100 unrestricted against 30 for a principal filtered to one region.

Hydration is cached across statements, keyed by *(cube, measure, definition version,
snapshot, scope digest)*. The scope digest hashes what a guard **permits** --- tenant, table,
action, row filter, column masks --- and deliberately excludes the subject, so a thousand
analysts across six roles produce six entries rather than a thousand copies of six answers.
Any difference in what is visible changes the digest; who is looking does not.

`Guard::scope_digest` carries four mutations against it, including the one that is easy to
forget: putting the subject *in* would be safe and useless, and a suite testing only
separation would not notice.

### A client can discover a cube instead of hardcoding it

`cubes()`, `cube_dimensions(cube)` and `cube_measures(cube)` are ordinary table functions
returning ordinary rows, so a UI, a notebook or an agent composing SQL discovers the model
with the same `SELECT` it uses for everything else. No second protocol exists to keep in step
with the first.

Two columns are there because a client that lacked them would draw something wrong rather
than fail. **`depth`** carries the level order — coarse to fine is a fact about the model, not
about how rows arrived, and a client sorting the result without it draws a list where there is
a hierarchy. **`composes`** says whether a measure can be rolled up at all — a UI offering
"roll up by time" on a ratio offers a button that cannot work, and finding that out at query
time is worse than not offering it.

Describing reads no data. A picker that cost a hydration per keystroke is a picker nobody
leaves switched on, so hydration stays gated on a statement naming a navigation function while
description is registered always.

### Cuboids on disk, refreshed without a caller, and collected

The materialised tier of [ADR-0008](adr/0008-serving-cubes-under-policy.md) and the Maintained
lifetime of [ADR-0009](adr/0009-the-cube-lifecycle.md) are built.

A cuboid is a published table keyed by *(definition version, snapshot, **scope**, cuboid)*.
The scope is the addition, and here it is stronger than a cache key: **two scopes are two
tables**, so a bug in the lookup cannot serve one principal's rows to another, because the
rows are not in the file being read.

Cells are stored as the components of their Shewchuk expansion and rounded once when read.
Exit criterion 3a asks for bit-identical results with materialisation on and off, and a cube
rolls up in stages where every stage rounds — fixing the *order* of summation makes one
reduction reproducible and does nothing about **associativity**, which is exactly what a
materialised cuboid is. A test round-tripping one cell passed under a mutation that stored the
rounded total; the loss only appears when a stored partial is added to another, so the test
that catches it rolls two of them up.

`target_lag` is a **staleness target, not a schedule** — Snowflake's framing for dynamic
tables. Staleness here is exact rather than estimated, because a cuboid records the version it
was computed at, and a cuboid past its target is never served as though it were fresh: the
answer falls back to live aggregation and says `materialised = false`.

A maintained cube is built **with nobody logged in**. The refresher has no principal, so it
builds the *unrestricted* cuboid — which may serve only an unrestricted caller. **Background
refresh therefore helps dashboards and service accounts and does nothing for a restricted
analyst**, whose cuboids are built by their own queries. Pre-building named scopes is a
decision nobody has made and is not taken by implication.

And superseded cuboids are collected. One at an old snapshot can never be selected, so it is
garbage the moment the table advances — and it fell between the two mechanisms that existed:
the orphan sweep finds unreferenced files *within* a table, and this is a whole table no log
mentions. The same shape as the defect that filled a disk in the soak, reintroduced by adding
cuboids and closed the same afternoon.

### The cube path under sustained load, with materialisation actually reachable

`PASS` over 59 judged minutes against a two-hour horizon, all seven measures steady, on
2026-08-29 --- **the first soak in which a cube was served from storage.** Until M7 closed the
day before, `materialised` was dead code and every cube query went to the fact table however
many cuboids the refresher had built.

196 rounds, 1,960 publications, **37.2 GB read across 3.01 billion rows**, the cube answered 49
times, and maintenance reclaimed 11.66 GB over 1,651 ticks while holding the warehouse at 9 GB
and 90 live files throughout.

Resident memory closed at **2.0 GB against the 2.2 GB of the previous run** --- more work, a
longer run, and less memory. That is the direction materialisation was supposed to move it and
the first measurement rather than the first assertion: a cuboid read replaces a fact-table
hydration rather than adding to it. See [SOAK.md §7b](SOAK.md).

### The earlier cube run

`PASS` over 44 judged minutes with all seven measures steady, on the first soak to exercise a
cube — 157 rounds, 2.41 billion rows scanned, the cube answered 39 times, maintenance
reclaimed 11.42 GB. See [SOAK.md](SOAK.md) for the two defects the earlier runs found and why
neither was visible to a unit test.

Resident memory settles at **2.2 GB against a 779 MB baseline without cubes**. That difference
is measured rather than assumed, and it is a plateau rather than a climb: the last reading
*fell* by 128 MB, and a leak does not give memory back.

The remaining cost is `Contributions` retaining every raw `f64` per cell — roughly 1.5 GB of
plateau. It is real and it does not grow, which changes it from a correctness risk to an
optimisation with a known price. Doing it means swapping `deterministic_sum` for `Exact`, which
moves `Sum` in the last bit and lands directly on exit criterion 3's bit-identity tests — so it
is worth doing deliberately rather than under the impression that something is leaking.

**Answering from a materialised ancestor now happens.** It was the last piece of §11.6 that
was designed and unreachable, and closing it mattered for a reason beyond completeness: exit
criterion 3b — *a non-additive measure is never answered from a materialised ancestor* —
was passing **vacuously**, because nothing answered from an ancestor at all.

`plan` picks the narrowest cuboid that may legally answer, and a cuboid is a candidate only
when the measure permits every roll-up between it and the query. The grain a statement needs is
the union, across every cube call in it, of the dimensions grouped by and the dimensions a dice
restricts — the second because `where=region:north` must find a `region` column to restrict,
even though slicing drops that axis from the result.

Two things fell out of it, and both were the same shape as the milestone's other findings.
`materialised` was reading a cuboid at the **cube's** grain rather than the one its key names,
which worked for exactly as long as the only cuboid ever read was the base one — whose grain
*is* the cube's — and named a missing column the moment an ancestor was chosen. And the first
test of the dice rule could not fail: every materialised cuboid in its fixture happened to
contain `region`, so it could not tell the two behaviours apart, and a mutation dropping
`where=` survived it. It now pins `[period]`, which puts a cuboid on disk that is cheap,
current and unable to express the query.

**What is still not there.** A cuboid is pre-built only for the unrestricted scope.

The statement text is scanned for cube function names to decide what to hydrate, which is
deliberately crude: a `SessionContext` is built per statement, `TableFunctionImpl::call` is
synchronous while reading a table is not, and a false positive costs a cache lookup while a
false negative costs a query that fails to resolve a cube it named. It is replaced when the
surface grows a resolver of its own.

<details>
<summary>Superseded: why this was blocked before 2026-08-28</summary>

**A cube was loaded but not queryable, and that was a design decision rather than an
omission.** Hydration had nowhere correct to go:

- `session_for` builds a context **per statement**, so hydrating there reads the whole fact
  table on every query.
- Hydrating once at startup and sharing the cells across principals would hand every caller
  the same totals whatever policy says — the disclosure through arithmetic that `FR-QUERY-13`
  and §11.5 exist to prevent, and the kind that leaves no trace in a result.

The correct answer was a cache keyed by snapshot **and** the principal's visible scope ---
which is what was built.

</details>

The other absent half — a definition as a row in a system table, so the store is the warehouse
rather than a JSON file beside it — waits on the catalogue proper.


## The defect wiring maintenance into the server introduced, and fixed

Two decisions, each correct alone, were not put together.

A server resolves its table providers **once**, in `start()`: `resolve_with` reads the log,
builds a file list, and the provider holds it. That was sound while a served warehouse did not
move --- the server runs no ingest, and `Settings::read_as_of` says so.

Then the server started **maintaining the warehouse in-process**. Compaction replaces files
and retirement deletes the ones it replaced, so the warehouse moves whether or not anybody is
writing to it. A provider fixed at boot names files that are gone, and the query fails with a
missing-file error naming a path nobody asked about.

**Retirement's grace period is not the protection here.** It protects a reader that listed
shortly before a merge --- twenty-four ticks --- and cannot protect one that listed at startup
and has been serving from that listing since. With shipping defaults a deployment would have
begun failing queries roughly twelve minutes in: one grace period after the first merge.

Reproduced before it was reasoned about, because reasoning about it is how a wrong answer gets
written down confidently. `crates/sankhya-server/tests/maintenance_and_readers.rs` holds both
halves: the provider from before retirement can no longer read, and a server re-resolving
before it registers reads every row across its own maintenance.

**And it fixed something else that had been true all along.** A running server never saw data
committed after it started. That was defensible while the warehouse did not move; re-resolving
a table whose log has moved makes new commits visible as a side effect worth having.

---

## M6, closed 2026-08-28

Criterion 4 accepted on a forty-five-minute judged run by owner decision, with the gap from
the multi-day pipeline it asks for written into `IMPLEMENTATION_PLAN.md` rather than argued
away. Criterion 7 --- the gRPC transport and the write paths --- is carried into M8, not
waived.

### The soak, and what it took to make it say anything

Three runs. The first exhausted the disk at t+2833s and wrote a **zero-byte report**
explaining why: compaction replaced files and nothing retired them, because
`sankhya-maintenance` was a library the server did not depend on and nothing in production
ever called. The second died one sample short of `live_files`' first verdict. The third
returned `PASS` with all seven measures steady --- 168 rounds, 2.58 billion rows scanned,
resident memory ending at 779 MB, maintenance reclaiming 29.91 GB across 911 ticks.

Both causes were fixed where they were, not worked around: maintenance runs on a thread the
warehouse owns and the server starts at boot, and the warehouse's own size is a watched
measure with a stated budget enforced *in the loop*, so a breach is reported while there is
still room to write the report.

### Two defects the passing run reported about itself

**The fan-out alarm never de-duplicated.** It keyed a "report once" set on its own rendered
message, and the message counts batches --- so every rendering was unique and a standing
condition printed 168 times, which is exactly what the code's own comment forbids. It now
keys on the shape of the strain: which table, the average rounded to a whole partition, and
the widest batch. A *worsening* condition is a different condition and is still reported.

**And what it was reporting was a workload nothing produces.** Every row was dated `id % 90`,
in the fill and in the steady-state rounds alike, so every batch touched all ninety
partitions for the whole run. That is a backfill --- which the fill genuinely is --- and it
is not what arrival looks like afterwards. A source feeding a warehouse continuously produces
rows dated *now*, touching one partition or two across a midnight.

So the alarm was right about what it was shown, and what it was shown was a backfill labelled
as arrival. The harness now models both, with the newest day advancing as the run goes on so
the hot partition moves and compaction has to keep up with a partition being appended to
rather than one that is finished. **The daily axis `FR-STORE-20` mandates is not the
problem**; the harness's idea of arrival was.

### §10.8 — A gateway that refuses to become the bulk plane

`FR-API-06` is the interesting half, and its reason is a **product** reason rather than an
operational one:

> Bulk data SHALL NOT be offered over JSON. Serializing analytical results as JSON destroys
> the zero-copy premise and **defines published benchmarks downward**.

The failure it prevents is not a server running out of memory. REST is the convenient
surface, so people will use it for bulk extract *because* it is convenient — and then measure
the system through it. A columnar engine benchmarked through a JSON encoder is a JSON encoder
benchmark, and that is the number that gets published. The cap exists so the convenient path
does not become the measured path, which is why it is hard rather than raisable.

**A large result is a redirection, not a refusal.** It comes back as a **Flight ticket** —
the same query, already planned and authorized, redeemable over the columnar path. A `413`
sends somebody to ask for a bigger cap; a ticket sends them to the surface built for what
they are doing.

**You cannot count the rows to decide whether to return the rows.** Materialising a result in
order to measure it is precisely the cost the cap exists to avoid, so the decision is taken
from the plan's estimate before anything is materialised. Estimates are wrong, so there is a
second guard: encoding stops the moment the actual output passes the cap, and the partial
response is **abandoned rather than truncated** — a JSON array cut short is either invalid or,
worse, valid and silently short, and a client cannot tell the second from a small answer.

**Routes are declared and matched whole.** The scrape endpoint served
`/metrics/../etc/passwd` on a prefix match in `§10.2` — harmless there because it reads no
files, and exactly the shape that becomes a traversal the moment something does. Once was
enough to make it a rule rather than a fix.

**What `FR-API-04` names and this does not serve is recorded as data, with reasons.** Jobs,
archive operations, mutating tenancy and policy administration, and the structured graph API.
An API that quietly omits half a requirement reads as complete — and the jobs case is the
sharpest: with no scheduler running, the endpoint would list nothing forever, and a client
cannot tell *"no jobs are running"* from *"nothing runs jobs"*. That is the same reasoning
that deferred the control plane out of `M5` in the first place.

**Not built:** the gRPC transport itself, and the write paths. What exists is the surface's
shape and the decision `FR-API-06` turns on, both tested; wiring them to tonic and to an
audited write path is the remainder.

---

### §10.7 — The soak, and four attempts at judging one

The section read *"A multi-day soak"* in its entirety, which is not something anybody can
fail. The criterion is now falsifiable — **no bounded measure projects a crossing within the
observation horizon** — and inconclusive counts as a failure, because a run whose sampling
broke must not report the same green as one that ran properly.

**Three kinds of bounded, not one.** The naive soak complains when any number rises, and half
of them are supposed to. A measure declares whether it must be flat, flat *per unit of work*,
or a sawtooth whose **peaks** must not climb. The second catches what a total never can — an
audit drifting from one record per query to two, while its total rises exactly as it should.
The third is a distinction no point-in-time diagnostic can draw: two oscillating series look
identical at any moment, and one is a system keeping up while the other starts each cycle
further behind.

**Four attempts, each finding something by running it:**

- **The diagnostic's linearity gate is wrong for a soak.** A healthy measure is noisy and
  flat, which has an r² near zero — there is no trend to explain — so the baseline run
  reported memory, descriptors and the history file as unjudgeable while nothing was wrong.
  The same trap as r² on a constant series, corrected in `sankhya-math` earlier for the same
  reason: undefined is not bad.
- **A half-second run reported a memory leak.** Sixty rounds finished in 0.35 seconds and a
  process allocates as it starts; across the opening of a run that looks exactly like a
  linear climb. Fixed by a declared warm-up prefix **and** by bounding the horizon to what
  the run observed — half a second extrapolated to three weeks is a factor of three and a
  half million. `sankhya-diagnostic` already carried that guard and applying it there and
  not here was the omission.
- **Every caller got the warm-up arithmetic wrong**, both of them, overshooting by exactly
  the prefix. A calculation both call sites get wrong on the first attempt does not belong at
  the call site.
- **A summary spans less than what it summarises.** Peaks sit inside their windows, so the
  peak series spans less than the run — and the entitlement is a property of the run.

**The harness is proven to notice**, which is the part that would otherwise be an untested
backup by another name: one injection per shape of failure, each required to fail the run.

**Not done:** the multi-day run at the ten-gigabyte scale, and retained evidence. The
scheduled run is a change of duration and scale rather than a first attempt at the whole
thing. [`SOAK.md`](SOAK.md) has the method, the numbers and the reasoning.

---

### §10.6 — Versions, and the difference between damage and the future

**Three on-disk formats were added this session and none of them carried a version.** The
backup manifest, the restore-drill evidence and the diagnostic history. That is the gap
`§10.6` exists to close, and it was made in `§10.1` and `§10.3` — which is how these gaps are
usually made: a format is invented to solve a problem, and versioning it is not part of the
problem.

**A parse error and "this is from the future" are different facts, and only one says what to
do.** An artefact written by a newer release, read by an older one, fails somewhere in the
middle of parsing — an unknown field, a number that will not fit, a restructured object. The
error reads `invalid type: string, expected u64 at line 14 column 9`, and an operator reads
that as **corruption**. They go looking for a damaged disk, a truncated write, a bad copy. The
answer was "upgrade the binary", and nothing in front of them said so.

So every format now carries its version **first in the file**, read before anything else is
understood, and a future version is refused by name — with both numbers and an instruction.
The ordering is the whole point: a version buried at the end of a JSON object is a version you
learn only after successfully parsing everything you were trying to avoid parsing.

**The four axes, and why independence is load-bearing.** `FR-OPS-11` requires the internal
schema, the database major version, the table protocol and the wire APIs to version
independently. One product version covering all four means every change to any of them is a
change to all of them: an upgrade touching only the wire protocol reads as a storage-format
change and gets the caution one deserves — and, worse, the reverse, a genuine storage break
hiding inside a release that looked like a wire change.

**Backwards is the direction that decides whether you can roll back.** A new release reading
old data is the easy direction and the one everybody tests. Whether the *old* release can read
what the new one wrote is the question, and after the upgrade is not the moment to answer it.
So every format declares its rollback consequence — `safe`, `tolerated`, or **`ONE-WAY`** — and
that column exists so the decision is visible *before* the upgrade rather than discovered
during the rollback.

**A corpus, because a fixture is an old binary's behaviour preserved.** Testing an upgrade
properly means running release *n−1* against release *n*'s data. One release exists, so there
is no earlier binary to run — and there does not need to be. What is needed is an earlier
binary's **output**: artefacts as previous releases wrote them, checked in and read by every
build. Unlike a binary, a fixture never stops building, never needs a toolchain that has been
removed, and is legible in a diff.

The fixtures are **hand-written rather than generated**, deliberately. A generated fixture
regenerates when the format changes, agrees with the current code by construction, and proves
nothing at all.

**What is not tested:** running the previous binary, because there is not one. That is the
honest limit of `§10.6` until a second release exists — and it is also what unblocks `M5`'s
carried-forward client/server version matrix.

[`VERSIONS.md`](VERSIONS.md) is generated from the declarations, including the rollback
procedure.

---

### §10.4 — Packaging, and the two numbers nobody relates

**The drain did not exist.** `serve_until` returned the moment shutdown resolved; its spawned
connection tasks were detached, so dropping the runtime cancelled them abruptly. The doc
comment above it said *"connections already running finish on their own, because cutting a
client off mid-result is indistinguishable to them from a crash"* — describing the behaviour
it did not have, which is how it survived review. A client mid-result saw a reset on every
deploy.

That had to be fixed before packaging could mean anything: **you cannot choose a termination
grace for a process that does not drain.** A `JoinSet` now holds the handles, shutdown waits
for them, and the wait is bounded — because an unbounded drain hangs on one stuck client
until the orchestrator's patience runs out and kills the process anyway, with the difference
that nobody chose the moment.

**Then the check that relates the two.** A server's drain deadline and an orchestrator's
termination grace live in different files, are edited by different people, and nothing
normally connects them. When the grace is the shorter, every deploy kills the server
mid-drain and clients see resets that look like crashes. `check-package` reads the drain out
of the source, reads the grace out of every manifest under `packaging/`, and fails when a
manifest allows less time than the server takes.

**The platform baseline, which is where a Rust binary usually fails to install.**
`IMPLEMENTATION_PLAN` §10.4 already called for *a build against an old platform baseline
rather than a fully static binary*. What it did not say is that a baseline nobody checks is a
baseline nobody meets. The declared baseline is `GLIBC_2.28` — RHEL 8, Debian 10 — and
`check-package` reads what the binary actually requires.

**This build requires `GLIBC_2.34`.** It would not start on RHEL 8, Ubuntu 20.04 or anything
older than RHEL 9, and nothing on the build machine can tell you that: the symbol is present
locally, so it links, runs and tests clean. It is discovered by a customer. The check reports
it as a warning on a development build and **fails** when `SANKHYA_RELEASE` is set, because
failing every local build on a property only the release environment can satisfy would train
everybody to ignore it — the same warn-versus-fail distinction `check-loc` already makes.

Meeting the baseline needs a build against an old sysroot, which is release-pipeline work.
The gap is recorded rather than papered over by lowering the declared baseline to whatever
this machine produces, which would quietly drop every enterprise distribution.

**A test caught the check being broken before the check caught anything.** `highest_glibc`
matched a `GLIBC_` prefix against the whole symbol token — but `readelf` writes
`statx@GLIBC_2.28`, so it matched nothing, found no requirements, and concluded every
requirement was met. It passed the real binary against a baseline it misses by six versions.
The unit test written against genuine `readelf` output is the only reason that did not ship,
and it is the reason the parser is tested on real output rather than on a convenient
sketch.

**The support matrix is data, not prose.** Five targets, each with its baseline and its
artifact formats, declared once in `xtask/src/package.rs` and generated into
[`PLATFORMS.md`](PLATFORMS.md). The number of build targets is the number of things that can
silently break, and a script per platform drifts from its siblings until one artifact behaves
unlike the rest for a reason nobody can find.

**The baseline of the self-contained artifact is set by PostgreSQL, not by the Rust binary.**
Worth stating because tuning the Rust build and declaring victory is the obvious mistake: our
binary could be musl-static and the bundle would still require whatever `glibc` PostgreSQL was
built against. `cargo-zigbuild` targets a chosen `glibc` for the Rust half without a
container; the C half needs an old sysroot, and a container is excluded from *running* this
system, never from building it.

**Windows, checked rather than assumed.** The objection I expected to be fatal — a
case-insensitive filesystem, where `Orders` and `orders` collide — is already handled:
`sankhya-schema` case-folds every path segment to lower-case ASCII, digits and underscores,
and refuses collisions rather than disambiguating them. The platform device names (`aux`,
`con`, `nul`, `com1`…`lpt9`) are already reserved, with a comment saying why. **A warehouse is
already Windows-path-safe.** What is missing is the vendored PostgreSQL build and service
integration, which is porting work rather than a design problem. So the honest row is *client
only*: any PostgreSQL driver connects from Windows today, which is what most Windows users
need, and the server runs under WSL2 or a container until somebody builds it.

**Two tests found defects in things I had just written.** Requiring every server target to
state a baseline caught macOS declared with none — it has one, `MACOSX_DEPLOYMENT_TARGET`,
and calling it "not applicable" said the question does not arise when in fact it arises and
nobody answered it. And a mutation shortening the Kubernetes grace below the drain
**survived**: the comparison was correct and nothing exercised it, because it lived only in
`check-package`. A check that is only a command is a check that is only sometimes made, so it
is now a test as well.

**Not built:** container images and signing. Both need infrastructure this environment does
not have — a container runtime, which the five-minute claim exists to avoid needing, and a
signing key. The manifests assume an image that a release pipeline has to produce.

---

### §10.5 — The five-minute experience

`M6`'s first exit criterion, and the plan is explicit about the form: *"it must be a test so
it cannot rot"*. `crates/sankhya-server/tests/five_minutes.rs` is the quickstart, executed on
every build — generate a warehouse, start the real binary as a subprocess, connect over the
real wire protocol, query, aggregate, run the diagnostic, take a backup, prove it. Seven
documented steps, and any of them breaking breaks the build.

**The measured journey is about forty milliseconds**, from a built binary.

**The claim as written cannot be met from source, and the test says so rather than measuring
around it.** A first-time user's five minutes includes `cargo build`, which takes several
minutes on a cold machine and is dominated by dependencies — `QUICKSTART.md` has always said
so. The five-minute promise is a promise about a **released artifact**, which makes exit
criterion 1 depend on `§10.4` packaging. Recording that is more useful than a green test
measuring the wrong interval.

**A thousand rows is deliberately small, and the suite has two larger sizes for the two
larger questions.** `check-performance` runs TPC-H at scale factor 1 on a quiet machine for
the latency objectives; the soak runs ten tables and ten gigabytes for days, to establish
that nothing grows without bound. Conflating the three is how a suite comes to prove nothing,
and `§10.7a` now specifies the third — which previously read, in its entirety, "a multi-day
soak".

**And tightening the budget does not rescue the timing assertion either.** This warehouse
holds a thousand rows in four files; no plausible scaling regression is visible at that size.
Somebody making the read path open every Parquet footer would still finish in milliseconds.
The budgets catch a phase *breaking* or slowing by two orders of magnitude — a deadlock, a
retry loop, a sleep left behind — and nothing subtler. Scaling belongs to
`cargo xtask check-performance`, which generates a scale-factor-1 dataset and sits outside
`check-all` for exactly that reason. Splitting them is the point: one proves the path works
on every build, the other proves it is fast on a quiet machine.

**Three defects, and one of them was in the test itself.**

- **The banner printed the configured address, not the bound one.** Told to bind port 0 it
  printed `:0` — the line whose only job is to say where to connect said nothing, and a test
  wanting an ephemeral port had no way to learn which one it got. In three places: the
  startup line, the `psql` invitation, and the metrics URL.
- **`describe()` printed it a second time**, so the real port appeared beside a literal `:0`.
- **The test hung instead of failing.** It read the banner with no deadline, so a mutation
  that stopped the server announcing its port blocked forever and took the whole build with
  it. Found by running that mutation. A test that hangs is strictly worse than one that
  fails, because a failure names what broke — the read is now bounded and reports "it never
  said" distinctly from "it took too long".

---

### §10.3 — Backup, protection and the restore drill

Three requirements, each existing because of a specific way backups fail.

**`FR-OPS-13` — the manifest refuses to exist rather than recording a disagreement.** The
requirement's reason is blunt: *"three backups that do not agree with each other are worse
than one"*. Worse, because three that agree restore a system and three that do not restore a
puzzle, with nothing to say which is the one to trust.

There turned out to be **two positions, not one**, and conflating them is the defect the
manifest exists to prevent. `source_restores_to` is where the transactional store lands.
`queryable_at` is the highest position at which *every* table is complete — the minimum over
their coverage, because a query joining two tables can only be answered where both reach.
They are rarely equal: tables publish at their own cadence. Recording one number and calling
it "the consistent point" means recording whichever one the author happened to think of, and
the difference between them is exactly how much re-capture a restore implies.

The rule enforced at **build** time: **no table may cover a position past where the source
restores to.** If one does, then after a restore the analytical tier holds rows the
transactional store no longer has; capture resumes behind them and republishes that range at
different positions. It is `SNK-S0002`'s shape one layer up, and it is not detectable
afterwards from either side alone — which is why it is checked when the backup is recorded
rather than when it is needed.

**`FR-OPS-14` — expiry and removal are two steps.** Deleting a backup does not release its
files. The failure that prevents: a backup deleted by mistake, the files swept before anybody
notices, and no way back even if the manifest is recovered five minutes later. Seven days of
grace, which is the span over which this kind of mistake is actually caught. `FR-STORE-21`
makes the same trade for compaction — only add files, remove them later — for the same
reason.

**`FR-OPS-15` — the drill reads the data back.** *"An untested backup is a rumour."*

A file-presence check passes on a truncated Parquet, on a file whose bytes were replaced with
another table's, and on essentially every failure that actually happens — because what goes
wrong with a backup is almost never that a file is missing. A missing file is loud. What goes
wrong is that a file is there and wrong. So the drill recomputes the digest, which is
expensive and is the only version of this that means anything.

Both the backup and the drill compute that digest through **the same code**. Two
implementations would drift on a null convention or a value rendering, every drill would fail
on data that is fine, and after the third false alarm nobody would run drills.

**The evidence records failures, or it is marketing.** A drill history with no failures in
three years describes either a very good system or a drill that does not really run, and
nothing in the history says which. Append-only, failures written with the same ceremony as
passes, and "could not start" recorded distinctly from "ran and passed" — the same
distinction the diagnostic draws, for the same reason.

`sankhya-server backup` and `sankhya-server drill`, exiting `0` proven, `1` a table did not
verify, `2` could not run. Demonstrated end to end against a real warehouse: take a backup,
corrupt a file, watch the drill catch it and both outcomes land in the record.

**The check with a property no other has.** The diagnostic now reports how long the backup has
been unproven — and it is the only check in that crate that gives a **firm** date on a first
run. Everything else needs two samples because a value alone implies no rate. Staleness rises
at exactly one second per second and always has, so it needs no observing. The natural
instinct is to feed it through the same `Trend` machinery as everything else, which would
collect a week of samples to estimate a rate that is already known exactly, and report
`TooFewObservations` in the meantime about the one thing that needs none.

**Two catalogue entries were wrong in ways only running them showed.** Refactoring the log
replay so that time travel and replay-to-the-end share their ordering rules moved two
existing mutations, and `check-mutations` failed the build rather than letting them pass
silently. And a new entry named the crate the *code* lives in rather than the crate whose
tests notice — it reported SURVIVED while the defect was caught, which is precisely how you
learn to read survivors as noise.

---

### §10.2 — Observability and the error catalogue

Two catalogues, both **generated into documentation from the declarations themselves**, and a
check that fails the build when the document and the source disagree. `M6`'s sixth exit
criterion asks for exactly that — *"generated from the same source as the catalog"* — and the
clause matters more than it reads: a hand-written table of error codes is correct on the day
it is written and wrong by the second release, with nothing to say which entry went stale.

**The metric catalogue is the API, not documentation of it.** Recording takes a
`&'static Metric` from the catalogue, so there is no `counter("some_name")` and an undeclared
metric is not refused at runtime --- it cannot be typed. Every exported series therefore has
a documented meaning, a unit, a group and a bound on its cardinality, because those are
fields on the thing you had to pass.

**The tenant-data prohibition is structural.** `ARCHITECTURE` §17.1 says no metric label may
contain tenant data. A label declares either a closed set of permitted values --- anything
else is refused --- or a deployment-scoped identifier under a cap. There is deliberately no
third variant, so a label that varies per row has no way to be declared. Past the cap, new
series are refused **and counted**: the metric goes incomplete and says so, rather than
growing without bound or going quietly wrong.

**Two checks, not one.** Generating a document from a catalogue proves the document matches
the catalogue and says nothing about whether the catalogue matches reality. So
`check-catalogues` separately requires every declared metric to be recorded somewhere in the
source. `ARCHITECTURE` §17.1 names four metrics that page; only compaction debt is declared,
because the other three measure machinery that does not run in this process and three gauges
permanently reading zero are indistinguishable from three healthy subsystems. The gap is
published in `METRICS.md` rather than filled.

**Runbooks are enforced, not aspirational.** A pageable metric's `runbook` field is not an
`Option`, and the check requires the file to exist *and* to carry its Symptom / What is
actually wrong / What to do sections. Seven exist. `M6`'s fifth exit criterion holds rather
than being something to audit later.

**Five defects, four of them in the path a user actually takes:**

- **Errors reaching clients carried no code and no remediation.** The catalogue had existed
  since M0 and the wire path did not go through it: a failed query returned the engine's own
  message with a SQLSTATE guessed from substrings. The errors a person actually meets were
  precisely the ones with nothing to look up. Exit criterion 6 was false.
- **`CREATE TABLE` succeeded and did nothing durable.** DataFusion will run DDL against its
  own in-memory catalogue, so the statement returned a success tag, the table existed for the
  rest of that connection, and it was gone on reconnect. Not an error, not a wrong number ---
  *a confirmation of something that did not occur*, which is the worst shape available. Fixed
  by planning and executing in two steps, because `SessionContext::sql` runs DDL during
  planning and a check on the returned plan is already too late.
- **The commonest error of all was misclassified.** DataFusion 55 wraps plan errors in
  `Diagnostic` to attach a source span, so matching on `Plan` never fired and "table not
  found" fell through to the catch-all.
- **`Box::leak` on every scrape** --- a few hundred bytes every fifteen seconds, forever, in
  the component whose job is to report that kind of thing.
- **A prefix-matched route** served `/metrics/../etc/passwd`. Harmless against an endpoint
  that reads no files, and exactly the shape that becomes a traversal when one does.

And two tests that did not test what they claimed: a cardinality-budget test using a metric
with no labels, and a refusal-classification test reaching only one of the three states the
table covers. Both were found by the mutation catalogue, not by reading.

**The tenant-data prohibition is now checked rather than asserted.** `ARCHITECTURE` §17.1
forbids caller data in any log line; metric labels were already structural and logs had only
the sentence. `cargo xtask check-logging` closes it — and it is aimed at the case nobody
writes deliberately: **`#[instrument]` records every argument of the function it decorates**,
so three words on `fn query(&self, sql: &str)` put every statement any client sends into the
log, predicate values included, with nothing at the call site saying so. There is no
suppression comment, because a prohibition with an escape hatch becomes a prohibition with
escapes in it.

Its own mutations then showed the word-boundary logic was **entirely unexercised**: every
test was rejected by plain substring absence, so neither half of the boundary was ever
reached. `%sql` is a real substring of `%sqlx`, and the leading and trailing checks reject
different things — one constructed case each. Third time this milestone a survivor exposed a
test passing for a reason unrelated to its name, which is the specific value of mutation
testing over coverage: both lines were covered throughout.

**What is not built:** distributed tracing spans. `FR-OPS-16`'s remaining checks --- conformance, replica identity,
archival consistency --- belong to §10.1 and are not built either.

---

### §10.1 — The diagnostic

`sankhya-server doctor`. Walks the warehouse, records what it sees, and reports findings
ordered by *when* rather than by how bad.

`FR-OPS-17` is the requirement, and it has a consequence it does not state:

> The diagnostic SHALL report **time until a problem becomes user-visible**, not merely its
> current value.

**A time cannot be computed from one sample.** It needs a rate, a rate needs observations
over time, and observations over time need somewhere to keep them between runs. That second
half is easy to miss — the projection arithmetic looks like the hard part and is not. A
diagnostic with the arithmetic and no history satisfies the requirement on paper and never
once in practice, because every run is the first run.

So the crate has two halves. `projection.rs` turns a series into a date or a named refusal;
`history.rs` is an append-only text file beside the warehouse that makes a second run
possible. It is deliberately not a table in the system being diagnosed: a diagnostic that
cannot run when the database is unhealthy is a diagnostic that cannot run on the day it is
needed.

**What it refuses to do, and why each refusal is its own outcome:**

| Refusal | The failure it prevents |
|---|---|
| Fewer than two observations | A date invented from one sample is a number with a calendar entry attached |
| A poor linear fit | A sawtooth — debt accumulating and being compacted away — fits a line badly *by construction*, and a date through one reports where in the cycle the samples fell |
| Beyond the horizon | Four days of samples projecting six months out is arithmetic, not evidence. The horizon is three times the observed span |
| No elapsed time | Every observation shares an instant |

A measure *near* the threshold with no rate yet still speaks up, undated, at `note`
severity. Silence at 990 of 1,000 files reads as health, and it is not.

**Four defects found while building it, three by running it rather than by testing it:**

- **The direction of concern was read from the slope.** It cannot be: a measure falling
  while the threshold sits above it is receding, and the slope-based reading called it
  already-crossed. Which direction is trouble is the caller's fact, not the data's.
- **Linearity was asked after direction.** A sawtooth averages to roughly no slope, so the
  direction test reached first and answered *receding* — an affirmative all-clear drawn from
  data that supports no conclusion at all. The order is now load-bearing and is tested.
- **`linear_fit` called a constant series a bad fit.** `r²` is 0/0 there, and the code
  returned zero on the reasoning that the fit explains none of the variance. Arithmetically
  defensible; it reads as "these points are not described by a line" about the straightest
  series there is. A horizontal line through a horizontal series is a perfect fit, so it now
  returns one. This was a real defect in `sankhya-math`, found by its first caller who cared.
- **"1 days".** A `contains` assertion hid it — `"about 1 days"` contains `"about 1 day"`.
  Found by reading the output, and the test now pins the whole phrase.

**And two catalogue entries that could never have failed.** One mutation was anchored on a
guard whose text appears twice, so it patched the harmless copy in `line()` and reported
SURVIVED; another claimed `{}` rounds `f64` where `{:?}` does not, which is false — both
round-trip, and the real reason to prefer `{:?}` is that `1e-300` under `{}` is 302
characters. The first was re-anchored; the second was deleted, along with the code comment
making the same false claim.

**What is checked today:** compaction debt, end to end. Storage headroom and replication lag
exist as checks with nothing feeding them observations — free space needs a platform call
`forbid(unsafe_code)` will not permit, so the caller that has one passes the number in, and
nothing in this process advances a replication position. The rest of `FR-OPS-16` —
conformance, replica identity, archival consistency — is not built.

Exit status is `0` clean, `1` findings, `2` a check could not run. The third exists because
a monitoring system treating "I could not look" as "nothing found" is the failure this whole
crate is arranged against.

---

## M5, closed

Closed 2026-08-27. Tenancy is enforcement rather than retrofit --- the tenant parameter has
been on every interface since M0, which is why this was twenty-odd weeks rather than sixty.

| Exit criterion | State |
|---|---|
| Mainstream client tooling connects and works, verified by a compatibility matrix | **Met, for one surface of four.** Real `psql`, `pg_isready` and `pg_dump` binaries driven against the release server: `cargo test -p sankhya-server --test compatibility -- --ignored`. `pg_dump` is recorded as **failing** rather than omitted, because somebody will try it |
| Isolation provably enforced across every surface, including the graph tier | **Met.** All five surfaces the demonstration names, in one test with one pair of tenants — SQL, the columnar prefix, a graph traversal, a cached result, and the error message |
| Mutation score on the policy component above its threshold | **Met.** 11 of 11 mutations caught, including removing the tenant comparison, letting grants outvote a denial, and treating absent grants as permission |
| Audit records reproduce exactly what a principal saw, including data versions | **Met.** The row filter and column masks that were applied, plus the table snapshot and graph epoch that answered |
| Compatibility matrix between client and server versions tested, not asserted | **Not met, and carried forward.** One server version exists, so there is no matrix to test. This is honestly untestable rather than skipped, and it becomes real when a second version ships |

### What is built and what is not

Two of four API surfaces are built, and the other two are **deferred rather than missing**.

The wire protocol came first deliberately --- `FR-API-02` calls it the highest-adoption-value
surface, and it is the one that makes every other capability reachable by a person rather
than by a test. **Flight SQL** followed, because `FR-API-01` names it the *primary bulk data
plane*: the wire protocol is a row protocol, so the last step of every query converts
columnar batches into rows, and that conversion is the whole cost of a large extract.

The **gRPC control plane and REST gateway are deferred to M6** by owner decision.
`FR-API-04` says what a control plane exposes --- jobs, health, archive operations --- and
each of those is built in M6 or M9. Building the surface first would mean endpoints for jobs
no scheduler runs and archives that do not exist: a plausible-looking API returning a
placeholder, which is the kind of thing that gets believed.

### Two things worth recording

**The type-level guarantee is a `Guard` that cannot be constructed except from an allowed
decision.** No public constructor, no public fields, no `Default`. Anything requiring one in
its signature cannot be called without a decision having been made --- and the failure that
guards against is not a wrong policy but a code path that never consulted one.

**The enforcement does not trust the provider.** The first implementation handed the policy
predicate to the underlying provider as a pushdown filter, and `MemTable` **declines**
filters --- so every row came back and the table was secured in name only. No error. The
predicate is now offered *and*, unless the provider promises exactness, enforced above the
scan where nothing can decline it.

### Three defects the tests found

| What | How it was found |
|---|---|
| **The secured table was secured in name only.** The policy predicate was offered to the provider and the provider declined it, so every row came back with no error raised anywhere | The first run of the test written to check it. Correctness now never depends on the provider cooperating |
| **A mutation survived twice before the test was honest.** `push the limit below the security filter` kept passing, because the test ran against `MemTable`, which ignores limits too. A test whose subject ignores the thing under test proves nothing | The mutation audit, twice. It now runs against a provider that honours a limit, with the forbidden rows ordered first so the cut bites |
| **Removing the audit chain's previous-digest check left every test green.** Every test that broke a link also broke the sequence number, which fires first — so the link check was never the thing catching anything. A competent attacker renumbers after a deletion | The mutation audit. That test now exists, along with one for a spliced record |

---

## M4, closed

Closed 2026-08-27. The graph engine and the extension mechanism, built together because
the graph algorithms are the most demanding consumer of the extension API --- building them
apart would have proved the API against nothing.

| Exit criterion | State |
|---|---|
| Graph performance objectives met against a named public suite | **Not met, and not claimed.** No public graph suite is wired into the pipeline. The primitives are correct against brute force and bounded by construction; they are not yet *measured* at scale. This is the one M4 criterion carried forward, and it is carried as unmet rather than reinterpreted |
| The incremental-equals-full property test green | **Met.** Two property tests, 400 cases. Verified to fail: making the overlay ignore its pending batches turns three tests red |
| Memory per vertex and per edge published | **Met.** `Epoch::footprint()` measures a real epoch rather than estimating from type sizes, because the interner's key storage dominates and depends entirely on key length |
| Time-respecting traversal returns no time-violating path | **Met.** `a_static_path_that_time_forbids_is_not_returned` builds the smallest case: two edges whose order forbids the path they appear to form |
| Each reference pack's change touches zero core files | **Met, mechanically.** `check-layers` refuses any pack dependency outside `sankhya-ext`, `sankhya-types`, `sankhya-error`, and refuses any core dependency on a pack |
| The adversarial pack's every attempt rejected with a named error | **Met.** Seven attempts, seven named refusals |
| The extension API within its size budget, with no escape-hatch types | **Met.** Nothing in `sankhya-ext` re-exports an engine type. `Value` and `LogicalType` are SANKHYA's own |
| Cancellation demonstrated within its bound inside pack code | **Met, with a stated cost.** See below |

### Cancellation, and what it actually costs

A pack function in a deliberate infinite loop that ignores the cancellation flag *is*
stopped, and the query fails with an error naming the pack rather than hanging. The
mechanism is worth stating plainly because it is not free.

The call runs on its own thread and is **abandoned** when the bound passes. Rust has no
safe way to kill a thread and this repository has no unsafe code, so a genuinely
non-terminating function leaks one thread until the process ends. `Sandbox::abandoned()`
counts them, so a pack that does this is visible rather than suspected.

The alternatives are worse: hanging the query forever, or killing a thread mid-allocation
and corrupting the allocator for everything else. A bounded, observable, attributable leak
is an operational problem with an obvious fix. The other two are outages.

The declarative tier does not need any of this. Its expression language has no loop, no
recursion, no call and no I/O, so a declarative function *cannot* be the one that hangs a
query. It is safe by construction rather than by supervision, and that restriction is the
point --- an expression language with loops is a programming language, and one loaded from
a configuration file is a remote code execution feature with extra steps.

### On signing

`FR` asks for pack bundles to be signed and verified. What ships is **digest pinning**: an
operator pins the digests of bundles they have reviewed and anything else is refused,
including everything when nothing is pinned, because a trust policy that defaults to
trusting is not a policy.

That is a real control and it is deliberately not called a signature. A digest proves the
bytes are the bytes you pinned; it proves nothing about who wrote them. Public-key signing
is the better answer and needs a cryptographic dependency, which this repository adds by
decision record rather than alongside a feature. `Verifier` is a trait, so adding it later
changes no caller.

### Three defects the tests found

| What | How it was found |
|---|---|
| **The negative-weight guard fired by luck.** Shortest-path checked each edge as it relaxed it, and Dijkstra settled the destination by a cheap direct edge before ever examining the negative one — so the same graph passed or failed depending on the query | Writing the test for it. The check is now a property of the structure, recorded at build time, and the refusal is its own error type so "cannot answer for this graph" cannot be read as "no route exists" |
| **Community detection collapsed two clusters into one.** Label propagation's monster-community failure, on the smallest interesting case: two triangles joined by one edge. Determinism, which reproducibility demands, made it *worse* — the randomised form at least sometimes stalls first | The test that specified the behaviour. Replaced with modularity optimisation, whose objective falls when dense clusters merge across a thin bridge, so the search resists the collapse rather than needing to stop in time |
| **A whole source file was never compiled.** It was not declared in `lib.rs`, so it contributed nothing and was checked by nothing — and it contained a match arm naming a struct field that does not exist | Wiring the module in. "It compiles" means nothing about a file the compiler never saw |

---

## M3, closed

Closed 2026-08-26. All six exit criteria are met, one of them after the criterion itself
was corrected and one after the objectives were measured against the workloads they
describe. Both corrections are recorded below rather than folded into a pass.

| Gate | State |
|---|---|
| Performance objectives met in the pipeline, against named public-suite queries | **Met.** `NFR-PERF-02` 13 ms, `NFR-PERF-03` 796 ms, `NFR-PERF-04` 648 ms, against budgets of 250 ms, 1 s and 3 s — asserted by `cargo xtask check-performance`, which fails the build. See the correction below: this gate was previously read as failing, against queries the objectives do not describe |
| Cancellation demonstrated within its bound, at the points M3 controls | **Met.** Bounded at one batch per partition inside a real query, across threads, and under periodic checking within its interval. The clause "including inside user code" has moved to M4 — see below |
| A hostile aggregation under a constrained memory limit is rejected rather than terminating the process | **Met.** An aggregation that cannot reduce anything is refused under a one-megabyte pool, by name — and the process runs the same query to completion afterwards. Repeated five times, so a refusal that leaked its reservation would show up |
| Plan snapshots stable; the SQL-semantics corpus green | **Met.** Thirty-nine semantics cases pinned by hand from the standard's rules; plan *shapes* pinned rather than plan text, plus assertions on the optimisations that fail silently |
| Cross-engine semantic differences enumerated in a tested list | **Met, and it found three ways the analytical tier returns a wrong number.** Fifteen cases run against both engines; agreements are pinned too, so a *new* divergence fails the test |
| Compaction holds file counts within policy under continuous ingest | **Met.** Forty ticks of capture with maintenance sweeping every third; the live count never passes the urgent threshold and no row is lost |

### The exit criterion that could not be met by M3

Criterion 2 read *"including inside user code"*. There is no user code until M4 builds the
extension mechanism, so as written M3 was gated on a later milestone and could not close
on its own terms. The clause has moved to M4's exit criteria, where the mechanism it
tests is built.

This is a correction to the plan, not a waiver. Nothing that was going to be tested is
now untested; the same test is asked one milestone later, of the milestone that can
answer it. The related case — cancelling a query while it *queues* for memory — is also
not claimed here, because admission never blocks: it returns a decision and the caller
waits, and that caller is M5's server.

### The objectives were being read against the wrong queries

The performance gate was recorded as failing, on Q6 at 386 ms against a 250 ms objective
and Q5 at 1149 ms against 1 s. Both numbers are real and both still stand. What was wrong
was the mapping.

`NFR-PERF-02` says **selective needle lookup**. Q6 applies three range predicates and
returns about a hundred thousand rows of six million, touched across every file in the
table — a different mechanism entirely, answered by scanning fast rather than by not
scanning. Measured against an actual point lookup, the objective is met at **13 ms
against 250 ms**, and met by statistics pruning alone, without the bloom filters its own
precondition names.

`NFR-PERF-03` says **multi-dimensional pivot, warm, pruned**, with a stated precondition
that a partition predicate be present. Q3 is that shape and meets it at 796 ms. Q5 is a
six-way join with no partition predicate — and since partitioning is not built, no query
can satisfy that precondition today. So Q5 is measured and published, and deliberately
**not** gated, rather than a passing test being written around it.

The honest form of the earlier statement was never "the system is too slow"; it was
"these objectives have never been tested". They are tested now. The mapping was mine, it
was approximate, and it was described as approximate at the time — but an approximate
mapping used as a gate is a gate on the wrong thing, in whichever direction it errs.

**A measurement that could not have failed.** The first version of the gate inherited
`#[tokio::test]`'s default current-thread runtime, so its eight concurrent clients took
turns on one thread and the pivot reported 3029 ms — three times its budget, describing
nothing the requirement is about. The gate now asserts it is on a multi-threaded runtime
before it measures anything.

### What M3 did not build

Closed does not mean complete. These are inside the M3 work breakdown, are not built, and
are not exit criteria: delete resolution into plan-time row selections, file ordering by
statistics for early termination, bloom filters, per-column encoding chosen from measured
statistics, quantile sketches, the footer and byte-range and decoded-batch caches, table
partitioning, and leader election.

The *result* cache does not exist either — but its **key** does, because what makes a
result-cache key correct is a security property and the right time to fix it is before
anything is caching. A key that omits the entitlement set does not return a stale answer;
it returns someone else's, correctly and quickly.

Nothing above is load-bearing for M4, which is why M3 closes with them open rather than
holding the milestone until a work breakdown is exhausted.

---

## The six-way join, explained

Q5 was recorded for several weeks as **39 % behind the engine's own listing table over
the same files, cause unknown**. An unexplained gap against a baseline reading identical
data is the kind of finding that is usually a wrong answer waiting to be noticed, so it
blocked the milestone rather than being filed as a performance nit.

It was two defects, both found by printing the two physical plans side by side instead of
reasoning about them.

**The scan reported no size.** `TableProvider::statistics` describes the whole table and
is read during logical planning. Join selection runs later, on the physical plan, and
reads the statistics of the scan node — which come from the file-scan configuration and
default to unknown. Every small table therefore looked *unmeasurable* at the exact moment
the engine chose how to join it, so it repartitioned tables it could have broadcast: five
rows of `region` shuffled across every core. Three of the five joins were the wrong mode.
A table reporting no size is also sorted against tables that do, so one absent figure
moved every join in the query, not just its own.

**The provider was grouping files by hand.** It dealt files round-robin across the target
partition count. The engine splits file groups by byte range, which balances on size and
is strictly better than anything done by counting files — but only above a size threshold,
below which it leaves a single group alone, and a single group is a single partition. So
the division of labour now follows the engine's own threshold: above it, hand over one
group; below it, deal them out, because otherwise nobody will. Doing both was worse than
either, and measurably so — the engine was rebalancing an arrangement already unbalanced
by file count, and the join paid about seven percent for it.

Together: **578 ms to 450 ms, against the listing table's 447 ms.** Parity, and the plans
are now identical operator for operator.

**The guard that did not exist.** The round-robin dealing had itself been added to fix a
real defect — the scan ran on one core — and no test was ever written for it. It was
caught only because a *deadline* test contained a self-check asserting its own plan ran in
parallel. Removing the dealing broke that self-check, which is the only reason the
regression was not shipped. There is now a direct test that a scan of thirty-two files
runs on more than one partition, asserted at the scan itself rather than above it, because
a repartition can manufacture eight partitions from one serial reader — which is exactly
the shape the original defect had.

---

## TPC-H, at scale factor 1

Every other measurement in this document isolates one mechanism. These are queries
somebody else wrote, so the numbers can be compared to something other than themselves.

Data generated at scale factor 1 — 8,661,245 rows, 261 MiB of Parquet — written through
this system's own write path. Five rounds at the stated concurrency, after two warm-up
executions, on a multi-threaded runtime — asserted, because on a current-thread one the
clients take turns and the figure describes nothing.

| | Clients | Median | p95 | Objective | |
|---|---|---|---|---|---|
| **needle lookup** — one row of six million by key | 8 | 11 ms | **13 ms** | `NFR-PERF-02` selective needle lookup, < 250 ms | **met** |
| **Q3** shipping priority — three-way join, top-N | 8 | 777 ms | **796 ms** | `NFR-PERF-03` pivot, < 1 s | **met** |
| **Q1** pricing summary — full scan, eight aggregates | 4 | 562 ms | **648 ms** | `NFR-PERF-04` wide scan, < 3 s | **met** |
| **Q6** forecasting revenue — narrow range filter | 8 | 386 ms | 491 ms | *not the workload any objective describes* | published, not gated |
| **Q5** local supplier volume — six-way join | 8 | 1149 ms | 1202 ms | `NFR-PERF-03`, whose partition-predicate precondition it cannot satisfy | published, not gated |

The first three are the gate, and `cargo xtask check-performance` fails the build if any
of them regresses. The last two are published because they are the queries a reader will
recognise, and withholding them because they are unflattering would be the worse choice.

**Q5 and Q6 are not failures against these objectives; they are not measured by them.**
That distinction is argued in full under *M3, closed* above, including why the previous
mapping was wrong in both directions. The short form: `NFR-PERF-02` describes finding one
row, and Q6 returns a hundred thousand; `NFR-PERF-03` requires a partition predicate, and
partitioning is not built, so Q5 could not satisfy it however fast it ran.

**These are met on hardware below the reference node.** The requirements name 32 physical
cores and 357 GB; this is twelve cores and 62 GB. That makes the results conservative
rather than qualified — but the reference node has never been measured on, so the numbers
that would be published with it do not exist.

**The first run of this measured the wrong thing**, and finding out why produced the
correction below. It used a bare engine rather than this system's configured session, so
the numbers described the engine's defaults. Running it properly made three of four
queries *slower* — Q6 by 2.6× — which is how a setting that had been asserted as required
since M3 began turned out to be a cost.

**What this is not:** an audited TPC-H result. Scale factor 1 on a development machine,
single node, four of twenty-two queries, no substitution rules, no refresh streams. Using
the name for anything more would be a misuse of it.

---

## Mutable tables, resolved

Capture records inserts, updates and deletes as rows. The read path used to union its
tiers and stop, so a row that had been updated came back **twice** — once as it was, once
as it is. `COUNT(*)` said two; `SUM` added the old value to the new one. Nothing in the
result indicated it.

It is resolved now, by declared capability rather than by heuristic:

- **Append-only** — union, as before. No deduplication, no sort, no key comparison. Most
  high-volume tables are append-only, so most queries take this path and it must cost
  nothing at all, not "a cheap check".
- **Mutable** — the latest version of each key wins, and a key whose latest version is a
  deletion is absent.

Expressed as a logical plan the optimizer can see through, rather than as an opaque
physical operator that would have to re-implement every optimisation inside itself.

**The capability comes from the source, not from a guess.** The relation states which
columns identify a row — its replica identity — and that statement arrives with the
relation description. Reading it is taking the declaration, not inferring one. A relation
declaring no identity is append-only *for reading*, and that is a statement about what can
be done rather than a prediction: with no key there is nothing to resolve versions
against, so an update could not be applied even if one arrived. The fix for that is the
source's replica identity, which onboarding already warns about.

Three things this gets right that a first attempt would not:

**The deletion filter runs after the resolution, not before.** Filtering tombstones first
removes the deletion and lets the *previous* version win — so a deleted row returns
holding the values it had before it was deleted. That is worse than the duplication it
replaces, because it looks like data.

**A key must be declared and must exist.** No key means no latest-version-per-key to
resolve to, and defaulting to whole-row identity turns every update into a new row — the
original defect, reached by a different route. A key naming a column that is not there is
refused for the same reason.

**Scanning a resolved table directly is refused.** The planner inlines the resolution and
never calls `scan`, so nothing exercises that path — which is why it is worth a test. A
fallback that quietly scanned the raw table would serve every version again, and the only
symptom would be wrong numbers.

**Why the suite never caught the original defect:** every test that read captured data used
append-only fixtures. The read path had never been asked a question about a row that
changed.

---

## What this system's own storage costs

TPC-H now runs through SANKHYA's provider rather than the engine's file listing — every
table carrying a commit position, committed to a log, resolved and pruned through the
catalogue. That makes the numbers a measurement of this system rather than of the engine
underneath it, and it made two costs visible.

| Single query, SF=1 | Engine's file listing | SANKHYA's provider |
|---|---|---|
| Q1 scan + aggregates | 450 ms | 462 ms |
| Q6 selective filter | 213 ms | 219 ms |
| Q3 three-way join | 357 ms | 372 ms |
| Q5 six-way join | 415 ms | 578 ms |

**Parity on scans, 12% and 39% behind on joins.** Four causes were found in the end. Two
are described here; the other two — the scan reporting no size to join selection, and the
provider grouping files by hand — are under *The six-way join, explained* above, and
closing them brought Q5 to 450 ms against the listing table's 447 ms. The table above is
kept at its original numbers because the sections below explain what each fix was worth,
and rewriting the starting point would erase that.

**The scan used one core.** The provider put every file into a single file group, which is
a single partition — everything above it could only round-robin batches that had been read
serially. Invisible in a result: the answers were identical and the query used one
twenty-fourth of the machine.

**The read position was enforced when it could not matter.** Reading at a pinned position
costs a column read and a predicate *per table*, so it compounds with join arity. When no
tier holds anything past the target the filter provably removes nothing — and the column
it reads need not be scanned at all. Most queries read the latest data, so most queries
were paying for a facility they were not using.

The commit position itself turned out **not** to be a cost: it is monotone, so it
delta-encodes to +0.1% on a 174 MiB table. That was the first hypothesis and it was wrong.

**What remained was eventually explained**, after some weeks of being recorded here as
unexplained — which was the right way to hold it. The answer is above; it was two further
defects, and neither was the one that would have been guessed. Both were found by printing
the two physical plans next to each other rather than by reasoning about them, which is
the general lesson worth keeping.

---

## Clustering, worth 7.8× on a range scan

This was originally written up as "the fix that closes `NFR-PERF-02`", which was wrong
twice over — Q6 is not the workload that objective describes (see *M3, closed* above), and
the objective's own named preconditions are not the lever either. Bloom filters do not
apply to Q6, which has no equality predicate; late materialization costs rather than
saves. **Sorting does**, and that finding stands on its own without an objective attached
to it.

Q6 selects one year in seven of `l_shipdate`. Written in arrival order every row group
holds the whole date range, so the bounds exclude nothing and the query reads the entire
table. Written in date order most row groups fall outside the year and are skipped on
their statistics, before any decoding.

| Layout | Single query | p95 at 8 clients |
|---|---|---|
| Generation order | 222 ms | 1819 ms |
| Sorted by ship date | **31 ms** | **234 ms** |

**7.8× at eight clients.** Compaction now does this: a settled partition is written in the
order the policy declares. No objective is claimed from it — the number is the point.

Two details that are the design rather than the implementation. Only *settled* partitions
are sorted — ordering one that is still receiving writes means ordering it again
tomorrow, for a layout correct until the next append. And the key is **declared, not
inferred**: a key chosen from observed queries would change under a workload shift and
rewrite the whole table to follow it, which costs more than the ordering is worth.

Q5, the six-way join, is still over its objective. Clustering does not help a join, and
what would is not built.

---

## A required setting that was costing 2.6×

`datafusion.execution.parquet.pushdown_filters` — late materialization — was in the list
of settings asserted at startup. It is not any more, and the way it left the list is worth
recording.

It went in on reasoning: late materialization is widely described as the largest scan
optimization available, and it defaults to off, so leaving it off is a large win silently
forfeited. It was then measured at **1.02× — neutral** on a synthetic scan, and pinned
anyway on the grounds that neutral is not harmful.

Measured on TPC-H it is not neutral. It costs at every selectivity tried:

| Rows surviving the filter | Off | On | |
|---|---|---|---|
| 1 in ~6,000,000 | 5.6 ms | 5.5 ms | 1.02× |
| 1 in ~1,500 | 4.5 ms | 4.8 ms | 0.94× |
| 1 in ~60 | 4.0 ms | 4.6 ms | 0.87× |
| 1 in ~7 | 116.6 ms | 162.0 ms | **0.72×** |
| all rows | 110.6 ms | 111.7 ms | 0.99× |

With filter reordering compounding it, Q6 went from 357 ms to **917 ms** at eight clients.

**Why it does not help is the useful part.** Late materialization saves decoding payload
columns for rows a predicate eliminates — and on this data those rows were already
eliminated by row-group and page statistics, before decoding began. That is why the
selective queries above finish in four to six milliseconds. Pushdown cannot save work
that is not being done; it adds per-row bookkeeping to the scan that remains. The cheaper
mechanism had already won.

It is left at the engine's default rather than pinned off, because the evidence supports
*not always* rather than *never* — on data with no useful bounds it should look different,
and the right form of that decision is per query from the statistics rather than pinned
for everyone.

**The pattern, which is the part worth keeping:** both errors came from asserting a
mechanism with a good reputation on reasoning rather than on a measurement of this
system's own data. The first measurement was too narrow to contradict the reasoning. The
second was a workload somebody else designed, and did.

---

## Where the two engines disagree

SANKHYA presents one copy of the data through two engines, and the same question asked of
each is supposed to get the same answer. Mostly it does — which is what makes the
exceptions dangerous, because nobody checks a figure that has agreed a thousand times.

Fifteen cases are now run against both, with the agreements pinned as well as the
differences. Three of the differences produce a **wrong number rather than an error**:

| | Transactional tier | Analytical tier |
|---|---|---|
| Summing past a 64-bit integer | `9223372036854775808` | **`-9223372036854775808`** |
| Multiplying past a 64-bit integer | refuses | **`0`** |
| Summing decimals past 38 digits | `100000000000000000000000000000000000000` | **`99999999999999997748809823456034029569`** |

The third is the one that matters most. Fixed-point decimal was chosen *because* money
must be exact, and on overflow the analytical tier returns something close to the right
answer instead of refusing. Close is the wrong kind of wrong.

Three further differences change a figure's precision or ordering without making it
wrong: `avg` of integers is arbitrary-precision on one side and a 64-bit float on the
other; division to a repeating fraction gives twenty significant digits against sixteen;
and text orders by the database's collation on one side and by bytes on the other, so any
paged or ranked result over text is in a different order in the two tiers.

**A mitigation exists and is partial.** The statistics catalogue can predict, before a
query runs, whether summing a column *can* overflow — bounds and row count give an upper
bound on the total. It errs toward "might", because a false alarm costs a refused query
and a missed one costs a wrong number nobody notices.

Its limit is worth stating plainly: bounds are held as 64-bit integers, so a decimal
column exceeding about nineteen digits has no representable bound and the check answers
"unknown". The columns most likely to overflow a 38-digit decimal are exactly the ones it
cannot reason about. Widening the bound type to 128 bits would close that, and has not
been done. Nothing yet consults the check.

---

## What runs today

| Capability | Evidence |
|---|---|
| The optimizer plans on the catalogue rather than a guess | Bounds, null counts and cardinality reach it from SANKHYA's own statistics — cardinality marked *inexact*, because an optimizer told a count is exact may conclude a column is unique, and being wrong about that is a different plan rather than a slower one |
| A row that has been updated is returned once, with its new value | By declared capability: union for append-only, latest-version-per-key for mutable, with a deletion suppressing the row rather than reverting it |
| A settled partition is written in the order it declares | Compaction sorts, which is what turns row-group bounds into an index — **7.8×** on the query whose objective was being missed, and the only one of that objective's three named preconditions that turned out to matter |
| A required setting is required because it was measured, not because it sounds right | Filter pushdown was asserted at startup for five milestones and is not any more — measured a cost at every selectivity, up to 2.6× on a full query |
| The pinned dependency set compiles with no critical duplicates | `cargo xtask check-dupes`, [ADR-0001](adr/0001-dependency-pin-set.md) |
| PostgreSQL 17.11 builds from vendored, checksum-verified source | `vendor/postgresql/build.sh` |
| The wire decoder handles a real replication stream | Conformance suite against captured bytes |
| The decoder never panics on arbitrary or corrupted input | Property tests plus single-byte corruption of real messages |
| A transaction is never split across batches | Property test over randomised interleavings |
| Types round-trip exactly or are refused with a reason | 73 real columns across 10 tables |
| Naming mirrors the source; collisions are refused | All 10 tables map with no transformation |
| A query is answered from tiers covering its span exactly once | Property test asserting exact cover |
| Tables onboard from the stream alone | All 10 tables, schema derived not declared |
| Captured data becomes queryable Parquet | Exact decimal sum matches the source |
| Several tables capture independently from one interleaved stream | 4 tables, each reconciling against the source |
| Capture holds up at scale | 1,000,000 rows across all 10 tables at ~285k rows/s, every table reconciling |
| A session sees its own write analytically | Wrote, capture caught up in 6 ms, the query returned the row |
| Capture cannot endanger its own source | Five-rung ladder escalating strictly below the database's own limit, validated against a real slot |
| Captured data provably matches the source | 3,000 rows digested independently on both sides, no discrepancies |
| A restart cannot duplicate data | Replay is filtered per row; every crash point across a constructed stream yields each row exactly once |
| A schema change never corrupts data | Additive changes apply automatically; anything whose intent cannot be inferred quarantines, keeps consuming so the cursor advances, and requires an operator to adopt the new shape |
| Backfill meets streaming with no gap and no overlap | Verified against a live slot; a late slot is shown to drop real positions into neither half |
| Small-file accumulation is detected and planned against | Two independent triggers, bounded passes, coverage preserved exactly, and a plan that always reduces the file count |
| Compaction runs without changing any answer | The same aggregate over 12 fragments and over the file they merge into, compared row for row; row counts verified against the inputs before the merge is reported as successful |
| Compaction cannot remove a file a reader might still be holding | Retirement refuses inside the grace period, refuses while a snapshot is pinned at or before the merged coverage, and refuses entirely if the replacement is missing or short |
| A write is visible before it is durable | The arrival tier answers from memory and drops out of the splice once published; the two tiers are shown to abut exactly with no change to the planner |
| The arrival tier never loses or double-counts a position | Property tests over arbitrary interleavings of appends and publication: every position between the durable frontier and the target appears exactly once, and a scan never returns a position the published tier already holds |
| Memory pressure cannot cost data | Nothing is released until publication covers it; a full tier refuses new work and names publication as the cause, and the refusal clears once publication catches up |
| One SQL query is answered from memory and Parquet at once | 700 positions published and 300 still in memory sum to the whole thousand, with the 700-position overlap counted once; a pinned query sees only its target; provenance names both tiers and their intervals |
| A gap between tiers refuses the query rather than answering it short | Verified for a genuine gap, for a target past every tier, and for a tier that started mid-stream |
| What the engine means by a query is written down | Thirty-nine cases across three-valued logic, aggregates, grouping, joins, ordering and coercion. Every expected value written by hand — a corpus that computes its expectation the way the engine does is a tautology |
| An optimisation that stops working is a failing test | Plans are pinned by operator shape, not text: a byte-exact snapshot fails on cosmetic upstream change and gets regenerated unread, at which point it catches nothing |
| Memory is counted where it is actually spent | A counting allocator, quarantined in the one crate where `unsafe` is permitted — the workspace `forbid`s it everywhere else. Growth, release, reallocation-by-difference, the peak, and concurrent allocation are all verified against a real installed allocator |
| Capture and maintenance commit to the same log without either stopping the other | Capture rebases onto the next free version when a compaction takes the one it was about to use — bounded, so a runaway committer is a diagnosable failure rather than a pipeline that appears to hang |
| A query stops within one batch of its deadline | Demonstrated on a real query planned and executed by the engine over real Parquet — it returns an error rather than fewer rows, and the clock is shown to have been read no more times than the deadline allows |
| A deadline and a cancellation are told apart | Different variants with a `retryable` flag: a deadline may succeed with longer to run, a cancellation is a decision somebody made. A client that cannot tell them apart cannot decide whether to retry |
| A query the system cannot afford never starts | Admission estimates from plan cardinality and queues or refuses; the queue is bounded, so a refusal arrives immediately rather than after a timeout |
| A client can tell a permanent refusal from a temporary one | "Too large for the pool" and "too large right now" are distinct answers with a `retryable` flag — collapsing them is how a client retries forever |
| One tenant cannot starve another, or take an idle pool | A floor is honoured under global pressure; a cap binds even when nothing else is running |
| Pressure escalates through one ladder, evaluated centrally | Five rungs from a typed signal bus; the last one — the only one that loses continuity — is reachable by the two source signals and nothing else, property-tested |
| Maintenance work is arbitrated against one budget across both sides | Strict class ladder; safety and availability work preempts queries and ignores the duty cycle, everything below it does not; a job that cannot checkpoint is refused rather than started; waiting never promotes a job out of its class |
| The maintenance scheduler cannot destroy retained history | There is no erasure class to configure — the guard is on the type, and an exhaustive match makes adding a variant a compile error |
| A fragmented warehouse is cleared by ticking, and converges | 60 fragments reduced by repeated ticks until nothing is left to do, with the row count unchanged; urgency maps onto the class ladder, so a degrading partition preempts and an ordinary one waits; one failing partition does not block the rest |
| A query is unaffected by compaction running underneath it | The same aggregate before and after a merge, over the live set |
| The live set is durable and readable by other engines | SANKHYA writes the Delta transaction log itself, and `delta_kernel` — a dev-dependency used as an independent oracle — reads it and agrees about the schema, version and live files, including across a compaction where four superseded fragments are still on disk |
| Capture publishes into a table log | Each table gets its own log; every file is committed with its row count, the creating commit carries the schema, and a translation that is not exact refuses to publish rather than publishing something similar |
| A restart resumes from the log, not from memory | File sequence and log version are both recovered from the table's whole commit history — not from the live set, so a compacted-away name is never reused while a reader may still resolve it |
| The whole storage loop runs through the log | 24 fragments published and committed, compacted tick by tick with each tick one atomic version, then queried by a reader given nothing but the table root — same answer, fewer live files, every superseded file still on disk |
| SANKHYA owns its table provider | Files and row counts come from the table log; scan execution is DataFusion's own Parquet source. Splices memory and files, refuses gaps at planning time, and refuses a file the log cannot state a row count for |
| Order statistics are exact and the convention is named | Three conventions, shown to disagree at the 99th percentile — which is the only place anyone asks for one. An unorderable value is refused rather than placed somewhere; an empty input is refused rather than answered with zero |
| The quantile of a sum is not computed as the sum of quantiles | Demonstrated with numbers: two exposures whose worst cases fall in different scenarios give −100 combined and −190 under the naive composition |
| A skipped file never hides a matching row | Property-tested over arbitrary values and predicates, and again over *merged* statistics — compaction merges rather than recomputes, so a merge that narrowed a bound would produce a defect appearing only after maintenance ran |
| Distinct-value counts are estimated well enough to order a join | Within 5% from 10 to 100,000 distinct values, exact under merge, and reproducible across processes — a per-process hash seed would make two nodes disagree about a plan and the disagreement would look like an optimizer bug |
| An approximate function cannot answer an exact question by accident | Rejected at planning time, including inside a subquery or a `HAVING` clause; a permissive session still gets a watermark naming what it used |
| A cache key cannot omit what the answer depended on | The entitlement set and the policy version are constructor arguments, not fields — a field can be left at its default, an argument has to be passed. Keys are byte-identical across processes, pinned so a change to the hash is deliberate |
| An interrupted compaction's leftovers are reclaimed, and nothing else is | Age is the only thing separating an orphan from a file mid-commit, so the threshold is a week by default; a file any retained snapshot reaches is kept however old, and the log is not a candidate at all |
| A cold reader starts from a checkpoint, and any reader may ignore one | **10×** at fifty thousand commits; the kernel reads a checkpoint this system wrote by hand, and is proven to *use* it rather than tolerate it — the commits it covers are deleted and the table still resolves |
| A warm process pays for what changed, not for the whole history | Table file sets are cached and resumed; the cache cannot go stale because it never trusts its own version, and asking costs one filesystem probe rather than a directory listing |
| Log replay scales linearly with a table's history | Guarded by measuring the *ratio* between two sizes rather than a clock, so it means the same on any machine — and proven to fail on the quadratic implementation it replaced |
| A file is prunable from the moment it is published | Capture computes statistics from the batch it just encoded — the same data, already in memory — so a file does not wait for maintenance to become skippable |
| Statistics survive a restart and other engines can read them | Bounds and null counts are written into the table log itself, so a fresh process prunes exactly as a warm one does — and the kernel reads a log carrying them |
| Compaction computes the statistics the provider prunes on | Bounds, null counts, widths and a cardinality sketch, produced by the merge that was already reading the data — no separate analysis pass and nothing for an operator to remember to run |
| The provider skips files the catalogue proves irrelevant | Nine of ten files pruned on a point lookup, five of ten on a range, and none at all on a disjunction, a predicate over an uncatalogued column, or no predicate. The same query returns the same answer with and without the catalogue |
| Planning does no file I/O | 800 files plan in 1.37 ms against 10.33 ms for a directory listing — **7.5×**, widening with file count |
| A dependency declared test-only actually is | `cargo xtask check-features` reads the manifests; proven to fail when the oracle is moved into `[dependencies]` |
| The tests guarding each core invariant are verified against the defect they claim to catch | `tools/mutation-audit.py` — 519 specific defects applied one at a time, each required to fail the suite. Thirty-one did not when first run; five catalogue entries turned out to be equivalent mutants no test could ever have caught, six entries were inert until corrected — two did not compile, and one was an equivalent mutant deleted rather than repaired, four more survived because the tests naming them exercised a different guard or lived in another crate, — one was anchored on a guard that appears twice so it patched the harmless copy, and one named the crate the *code* lives in rather than the crate whose tests notice — four revealed tests that did not test what their names claimed --- two of them in the tiering encoding, where the type-tag test compared two widths whose encodings already differ in length, and the length-prefix test used a key the tag bytes separate on their own, and chasing two others produced documentation corrections rather than new tests. Three mutations exposed defects in *tests* rather than in code, and all three were the same defect: an unbounded wait, so that removing a deadline hung the build rather than failing it. The five-minute journey read the server's banner with no timeout; both drain tests awaited the server task with none. A hang is strictly worse than a failure — it takes the build with it and reports nothing — so every wait now goes through one bounded helper rather than a timeout somebody has to remember at each call site. The catalogue also checks that each entry still *matches* its source before applying it: a refactor moved four of them, and a mutation that no longer applies passes silently, which is the failure this tool exists to prevent |

---

## Mathematics, arrays and the date axis

Added after M5 closed, by owner directive, and recorded here because a capability nobody
documents is one nobody finds.

| | |
|---|---|
| **Array columns** | `FixedSizeList<Float64, N>` for known dimension, `List` for the ragged case. The values round-trip exactly through the table format; the fixed width is carried in field metadata, so an external reader ignoring it sees a correct variable-length array rather than something wrong |
| **Vector kernels** | Elementwise, dot, L1 and L2 norms, euclidean and cosine distance. Every reduction bit-deterministic under permutation, asserted by a test whose fixture is itself proven adversarial — a naive sum fails on it |
| **Linear algebra** | Multiply, transpose, trace, identity, `matvec`, and LU with partial pivoting giving determinant, inverse and solve. The pivot breaks ties on the lowest row index, without which two builds could factor differently |
| **Statistics** | Variance (two-pass), covariance, correlation, skewness, excess kurtosis, median, standardise, least squares |
| **Calculus** | Central-difference derivatives, trapezoid and Simpson integration, cumulative integral |
| **SQL surface** | `vec_*` and `mat_*` functions, plus constructors so a matrix can be built and operated on without being stored. A matrix's shape lives in Arrow's canonical `fixed_shape_tensor` metadata and a column without one is refused rather than assumed square |
| **The date axis** | `sank_data_date`, of type `DATE`, declared per table and never defaulted per row. Granularity declarable. Nothing yet writes partitioned directories — the declaration exists and the partitioning does not |
| **Publishing and repair** | A library and CLI for writing an external table, a verifier that does not assume it was used, and repair that derives rather than guesses |

Not built, deliberately: QR, SVD and eigendecomposition. They are where an in-house
implementation is worse than none, because a subtly wrong SVD produces plausible singular
values.

---

## What does not exist

Stated plainly, because a status document that omits this is marketing.

- **No server.** The binary is a stub; there is no daemon, no listener, no lifecycle.
- **No streaming transport.** Changes are drained through a SQL function rather than a
  replication connection. The decoder and pipeline are transport-agnostic by design, but
  the transport itself is unwritten. Note that neither mainstream Rust PostgreSQL client
  supports the replication protocol, so this is real work rather than a wiring exercise.
- **No slot lifecycle.** No creation policy and no position advancement. The
  source-safety ladder now exists and is tested against a real slot, but nothing drives
  it on a timer yet — it is a decision function without a caller.
- **No backfill reader.** The handoff *contract* is built and verified against a live
  slot, but nothing yet reads the existing rows. Only changes after a slot exists are
  captured.
- **The arrival tier is a retention contract, not the buffer the architecture
  describes.** What exists is the part that governs correctness: coverage, per-row
  filtering at the durable frontier, and the rule that publication alone releases
  memory. What does not exist is §5.4's epoch ring, the per-epoch key digests that let
  a historical query skip the tier at no cost, and per-tenant sub-caps. Nothing yet
  wires the tier into the ingest path either, so read-your-own-writes still waits for
  publication in practice.
- **The cardinality sketch is not persisted.** The protocol has nowhere to put it, so a
  column read back from the log reports zero distinct values. Nothing currently reads
  that figure, but it is a trap for whatever does first.
- **No multi-part or V2 checkpoints, and no log cleanup.** A checkpoint is written as a
  single file, which is fine into the millions of live files and not beyond; and nothing
  deletes the commits a checkpoint subsumes, so the log directory grows without bound even
  though nothing reads most of it.
- **Nothing routes a captured table to the resolved provider automatically.** The
  capability is derived from what the source declared and the resolution works end to
  end, but the caller still has to assemble the two — there is no catalog mapping a table
  name to its provider, because there is no catalog.
- **The governor decides but governs nothing.** Admission and the pressure ladder are
  built and tested, and nothing calls either: no memory pool reports its occupancy, no
  subsystem publishes a signal, and no query passes through admission on its way to
  running. They are decision functions without callers, like the maintenance scheduler
  was before the driver.
- **No counting allocator and no spill isolation.** The architecture requires true
  accounting outside the engine's own pool, and spill on a different filesystem from the
  write-ahead log. Neither exists. Deadline propagation and cancellation *do* — bounded
  at one batch per partition — which is why they are no longer listed here.
- **Exact order statistics buffer their input.** Selection is linear rather than
  `n log n`, so it beats sorting, but every observation must be resident. `FR-QUERY-08`
  asks for a bounded-memory algorithm over large inputs and this is not one. Exact and
  bounded are independent properties, and only the first is delivered.
- **The exactness gate is not wired into a session.** `check_exactness` is a function
  with no caller: nothing carries the session's exactness setting, and nothing attaches
  the watermark to a result.
- **No catalog.** The table provider exists and resolves mutable tables correctly, but
  nothing maps a table *name* to one, so the caller still has to assemble it.
- **No deletion vectors, column mapping or partition values in the log.** Row counts,
  bounds and null counts are written, and checkpoints are; everything else the protocol
  permits is not. A reader requiring any of them refuses these tables, which is the
  correct outcome — refusing is visible, and a partially-implemented protocol feature is
  not.
- **No table partitioning.** Every scan is over the whole table, pruned by file
  statistics rather than by partition. This is why `NFR-PERF-03`'s partition-predicate
  precondition cannot be satisfied by any query today.
- **Nothing calls the maintenance loop on a timer.** The tick is built and tested end to
  end, but a caller has to invoke it, supply the live set and supply the pinned snapshot
  positions retirement checks against.
- **The server runs and executes statements.** Real `psql` connects, authenticates, runs
  catalogue queries and ordinary SQL — aggregation, expressions, null semantics — against a
  policy-wrapped provider — over **real Parquet on disk**, through the M3 read path, which
  plans from the table log alone and prunes files by recorded statistics. The server walks
  a `<schema>/<table>/` warehouse at startup and reads each table's schema out of its own
  log rather than inferring it from a footer, because a table with no files yet has no
  footer and one whose files predate a column would be missing it.
- **The security path is now reachable.** A table the principal may not read is never
  registered, so naming it fails to resolve rather than confirming it exists; a policy row
  predicate is enforced where no provider can decline it, and a tautology cannot widen it.
  Both were provable in unit tests before and unreachable through the server — an
  unreachable enforcement point is one nobody has confirmed is on the path.
- **One API surface of four.** The wire protocol works. Arrow Flight SQL, the gRPC control
  plane and the REST gateway are not built.
- **Per-tenant graph epochs and envelope encryption are still unreachable through the
  server.** Built and tested; nothing wires them to the front door.
- Nothing drives graph hydration on a timer and no process loads a pack bundle.

---

## Known defects found and fixed

Recorded because the interesting information is usually in what went wrong.

| Defect | How it surfaced |
|---|---|
| Rows of the transaction that first introduces a table were silently refused | Only visible with several tables interleaved; a single-table test cannot see it |
| The conformance fixture's central assertion passed vacuously — the "large" value compressed 80× and stayed inline, so it was never withheld | Caught by making the generator fail loudly if the value does not exceed the out-of-line threshold |
| The fixture capture tool injected newline separators into a binary stream | The decoder was right and the capture was wrong; found at the first byte after a transaction boundary |
| The layer rule forbade same-layer dependencies, which was wrong rather than strict | A vocabulary crate legitimately building on another |
| The documentation-rot check found a stale version claim on its first run | Its own first execution |
| **Zone offsets were stripped rather than applied, shifting a whole timestamp column by four hours** | End-to-end reconciliation against the source. Every value stayed internally consistent, so nothing looked wrong until the two sides were compared |
| **Duplicate suppression worked per batch rather than per row, so a batch spanning the restart boundary republished its already-durable half** | Crash-safety tests sweeping every possible interruption point. A resent stream does not rebatch identically, which a single hand-picked crash point would not have revealed |
| **A restart would have overwritten a live file.** The pipeline's file sequence was in-memory state starting at zero, so a restarted capture wrote `00000000.parquet` over a file that was still live and still referenced | Committing to the log. The overwrite had always been possible, but nothing could see it: the file count does not change, no error is raised, and the rows in the overwritten file simply become different rows. It surfaced as a version conflict — the log refusing to create a table that already existed — and the overwrite was the real defect behind it |
| **The test written for that overwrite could not detect it, twice over.** It asserted on file *names*, which an overwrite does not change; and its fixture published a single file per run, so resuming from the highest committed sequence and resuming from the lowest were the same number | The mutation audit, on two consecutive attempts. Now asserted on the log's own history — a path added twice *is* the overwrite — with a fixture that publishes at every transaction boundary, as continuous capture does |
| **The log described where files were but not what was in them.** With no row counts, a compaction plan driven from the log could not state what it expected to merge | The first tick planned from the log rather than from a value threaded out of the writer. The merge's own row-count check refused the plan — the guard worked, and what it caught was that the log was incomplete rather than that the merge was wrong. `numRecords` is now written and read |
| **Capture and maintenance could not both commit to the same table.** The publish path held its next version in memory and failed outright when a compaction took it | The first test that ran capture and maintenance together. Before it, every test had exactly one committer. Failing there inverts the ordering rule — a compaction could stop capture — so the publish path now rebases onto the next free version, which is what the error message had been telling it to do all along |
| **Vectorised bounds became unsafe in the presence of NaN.** Arrow's aggregate kernels propagate NaN, so `max` over a column containing one returns NaN; the NaN guard then refused it and left whatever bound had been recorded so far — a maximum *below* the true maximum | The unit test written for the NaN guard, on its first run. The narrowed bound is the one direction that matters: a file holding 2.5 would be skipped for `f > 0` because its statistics claimed it topped out at −1.5, silently and undetectably. A NaN result now invalidates the fast path and the column is measured again skipping NaNs, so the cost falls only on columns that actually contain one |
| **The hand-written Delta log was invalid, and this crate's own reader accepted it happily.** The `add` action's `partitionValues` field is non-nullable and was omitted entirely | The kernel, on the very first read. The log looked reasonable and round-tripped through this crate perfectly, because a reader ignores a field it never writes. Two implementations agreeing is worth nothing when one of them wrote both sides |
| **A directory listing is not a file set, and both the planner and the read path were treating it as one.** Compaction only ever adds, so between a merge and the retirement of its inputs the directory holds both — the same rows twice, by design, for at least a full grace period | Writing the convergence test. Re-observing the directory each tick made the planner merge files an earlier merge had already superseded. Nothing is wrong on disk; the *readers* were wrong. The published tier now names its files individually, the live set is carried across ticks, and the negative case is a test: the same query against the directory returns the merged rows twice |
| **The arrival tier declared coverage it did not hold.** A tier starting mid-stream reported from the durable frontier rather than from its own oldest segment, so it claimed every position before its first captured transaction | The first query spliced across two real tiers. The splice found an exact cover that did not exist, so the query would have been *answered* with the missing positions silently absent — the failure mode the splice exists to prevent, produced by the tier lying to it. This is the normal case rather than an edge case: a table onboarded from a running stream starts mid-stream by construction |
| **The property test written to catch that defect did not catch it.** Its generator started the publication frontier equal to the stream's origin, so the two could never diverge, and its assertion encoded the buggy expectation | Deliberately reverting the fix and finding the suite still green. The generator now starts publication at zero independently of the origin, and fails within a second on the reverted code |
| **A commit shipped one of the mutation audit's own deliberate defects, with a failing test.** The reversed-comparison fix in the predicate reader was reverted in the tree — `5 < x` was read as `x < 5` — so the reader skipped files that did hold matching rows | Reconciling the working tree before the merge. The audit restores what it mutates through an in-flight record and signal handlers, but nothing survives a `kill -9`, and the leak is invisible: the diff is one plausible-looking line in a file the commit was already touching. The test that proves the mutation caught was sitting red in `main`'s parent. Now guarded by `cargo xtask check-mutations`, which asserts every catalogue entry still matches its source — no compilation, milliseconds, so it gates every build rather than only a full audit. It catches catalogue drift by the same check |
| **The Parquet writer's default compression was never enabled.** The workspace pin omitted `zstd`, so every write on the default configuration panicked inside the column writer | The first crate to use the writer *without* also depending on DataFusion. Cargo unifies features across dependencies **and dev-dependencies**, and DataFusion — a dev-dependency of the writer's own crate — was quietly supplying the feature. The crate's entire test suite passed while the library was broken for every real consumer. Now guarded by `cargo xtask check-features`, which reads the manifest rather than the resolved graph, because the resolved graph is precisely what hides it |

---

## On the tests

Two of this project's invariant tests were reviewed, passed, and would not have failed
on the defect they were written for. Neither was found by reading them.

- The arrival tier's coverage property started the publication frontier at the stream's
  origin, so the two could never diverge — and its assertion asserted the buggy value.
- `is_exact_cover`, the oracle every splice test leans on, had no tests of its own. It
  could be changed to tolerate gaps between tiers, or to stop requiring the cover to
  reach the target, and the whole suite stayed green. Every splice test would have gone
  on passing over a planner returning partial covers.

A fourth was not a weak test but a **missing** one. The fix that made the provider's scan
run on more than one core had no test at all — the defect had been found by measurement,
and nothing was written to hold it. Months later that fix was replaced with a better one,
and the only thing that caught the intermediate regression was a *deadline* test which
happened to contain a self-check asserting its own plan ran in parallel. A test written
for one thing guarded another by accident, which is not a mechanism anyone should rely on
twice. There is now a direct test, asserted at the scan node rather than above it, because
a repartition can manufacture eight partitions from one serial reader.

A third was found the same way: the property named *open transactions are never
published* aborted every unsealed transaction before flushing, so it never left one in
flight. A mutation that published rows from transactions that had not committed survived
it untouched — and an in-flight transaction is the steady state of a busy source, not an
edge case.

`tools/mutation-audit.py` now keeps a catalogue of specific, plausible defects, applies
each one and reports whether anything fails. It refuses to run on a dirty tree, and it
flags a catalogue entry that no longer matches the source — a stale entry proves nothing
while looking like coverage, which is the failure mode the tool exists to find.

A fourth was found the same way, later: the driver's compaction removals were checked
only through the constructors they *could* have called, not through what the driver
actually committed. Swapping one for the other went unnoticed.

A fifth: the table provider could claim it evaluated predicates *exactly* — which gives
the engine permission to drop the filter from the plan entirely — and every test still
passed, because not one of them had a `WHERE` clause.

A sixth: the test asserting that a disjunction is never split used `a = x OR a = y`,
which the engine rewrites into an `IN` list before it reaches the code under test. The
test exercised no disjunction at all.

A ninth was found by the audit *hanging* rather than reporting: a threaded cancellation
test looped forever when the cancellation check was removed, so a defect that should have
been a failure in seconds became a thirty-minute stall. The loop is bounded now — a test
that stalls a pipeline is how a real defect gets discovered in someone else's build
rather than in this one.

An eighth: the regression guard written for the quadratic replay above passed against
the quadratic replay. It spread its workload across thousands of commits, where linear
file I/O dominates the quadratic term entirely.

A seventh, and the most useful: statistics computation had no tests in its own crate at
all. It was exercised only through an end-to-end test in a *different* crate, so
`cargo test -p sankhya-table` covered none of it and the audit reported two survivors
immediately. Writing the missing tests found the NaN bounds defect above on the first
run.

The catalogue also produced one **equivalent mutant** — a change to a duplicated guard
that left the second copy still refusing, so behaviour was unchanged and no test could
possibly have caught it. That is worth recording rather than quietly deleting: an entry
that can never fail trains you to read "SURVIVED" as noise, which is the one habit that
makes the whole exercise worthless.

**The general lesson, recorded because it applies to every test not yet audited:** a test
written to catch a defect is not evidence that it catches it. Until it has been run
against that defect, it should be assumed to be in the same state as the four above.

---

## Measurements

On a 24-core machine with NVMe storage.

| | |
|---|---|
| Cold `cargo check`, full critical dependency family | 32.9 s |
| Vendored PostgreSQL build | ~2 min, 35 MB installed |
| Synthetic generation | ~147 MB/s |
| Bulk load, 10 tables | 99,235,357 rows / 10 GiB in 188.7 s (~526k rows/s) |
| On-disk size after load | 14 GB |

### Query-engine settings

| | |
|---|---|
| Filter pushdown, 5M rows / 523 MiB / 1-in-10,000 selectivity | **1.02× — neutral** |
| The same measurement with a compressible payload | 0.74× — *slower*, an artefact of the fixture |
| The same measurement, fixture written by the product's writer (2026-08-28) | 1.06×, 0.94×, 0.96× across runs — noise around neutral |

**This corrected a claim rather than confirming one.** Earlier drafts of the
requirements and architecture documents asserted that filter pushdown was worth roughly
an order of magnitude. It is not, on this shape. The setting is still pinned — neutral
is not harmful and the benefit is expected on wider payloads — but the documents now
record the measurement instead of the assumption.

The first attempt showed pushdown *slower*, which was a fixture artefact: a repetitive
padding string dictionary-encodes so well that decoding it is nearly free, so avoiding
that decode saves nothing while the row-selection bookkeeping still costs. Worth
recording, because a benchmark whose data is unrepresentative produces confident wrong
numbers rather than obviously wrong ones.

The companion claim about the Parquet page row-count limit has **not** been measured and
should be read as unverified.

**The fixture was writing its own Parquet, and that was the third thing wrong with it.**
Found 2026-08-28 while auditing test code for reimplemented infrastructure. The benchmark
configured its own `ArrowWriter`: it duplicated three of `WriterConfig`'s six decisions --- zstd,
page statistics, the page row limit --- and silently dropped `max_row_group_row_count`, the
statistics truncation length and the commit-LSN encoding. Pushdown is only as good as the page
index and the page index is emitted by writer settings, so the measurement was of a file
Sankhya would never produce, and a change to the product's layout could not have moved the
number.

Routed through `sankhya_table::write_parquet` the figure is unchanged in substance --- noise
around neutral, which is what the TPC-H measurement in `session.rs` already concluded from
better evidence --- so nothing downstream of it moves. What changed is that the number is now
about the product. The test asserts only what the measurement supports: pushdown does not
change the answer, and is not materially slower.

The shape is also the reason, and the file's own comment had it backwards. `needle` is
`id % 10_000` over sequential ids, so every 20,000-row page spans the column's whole range: the
predicate eliminates 99.99% of *rows* and no *pages* at all, and decoding is per page. The
comment called this "the ordinary shape of an analytical table". It is not, and the claim went
unchallenged for as long as the fixture was also choosing its own encoding.

### Compaction

400 fragments totalling 20,000,000 rows, merged into one file.

| | 400 files | 1 file | Ratio |
|---|---|---|---|
| Short query — one narrow range | 16.9 ms | 3.8 ms | **4.42×** |
| Long query — full aggregation | 121.0 ms | 97.3 ms | **1.24×** |
| On-disk size | 61.0 MB | 27.4 MB | **2.23×** |

**This confirmed a claim, having first failed to test it.** The architecture asserts that
small files cost query *planning* rather than scanning, which predicts a roughly fixed
per-query penalty — dominant on short queries, amortised away on long ones. The
measurement bears that out: the absolute overhead stays in the same order (13 ms to
24 ms) across a query doing thirty times more work, while the ratio collapses from 4.42×
to 1.24×.

The first run used 1,000,000 rows and produced 4.43× and 3.77× — apparently uniform, and
readable only as "more files are slower". The long query was not long enough for
planning to amortise against. The fixture was scaled until the two hypotheses gave
different answers; before that it was not evidence for either.

The practical consequence is that fragmentation is an **interactive-latency** problem
rather than a throughput one, which is the reason it is worth a first-class subsystem.

### Query planning

400 fragments and up, planned but not executed, best of three.

| Files | Provider | Directory listing | |
|---|---|---|---|
| 50 | 0.54 ms | 1.15 ms | **2.2×** |
| 200 | 0.63 ms | 3.02 ms | **4.8×** |
| 800 | 1.37 ms | 10.33 ms | **7.5×** |

The advantage widens with file count, which is what the metadata-only claim predicts:
the provider reads no Parquet footers, so its cost does not scale with the number of
files it is planning over.

**It does not scale with nothing, though.** Sixteen times the files costs the provider
about 2.5× more planning, because it replays the table log and the log grows with commit
count. The cost has moved from one seek per file to one sequential read — which is a much
better shape and is not the same as free. It is also the argument for log checkpoints,
which are not built.

### Where the memory limit bites

The gate is that a hostile query is *refused* rather than fatal, so the first thing to
establish is where refusal actually begins.

| Pool | An aggregation that cannot reduce anything |
|---|---|
| 64 KiB – 1 MiB | Refused: `Resources exhausted … SingleHashAggregateStream` |
| 8 MiB and above | Succeeds, 400,000 groups |

The operator **errors rather than spilling**, which is what makes admission worth having:
without it a query gets far enough to fail, having consumed the machine on the way.

**The first version of this test used two megabytes and passed while proving nothing** —
just above the line, so the query succeeded and the assertion never ran. The threshold is
now measured rather than guessed, and the test asserts the *operator* that ran out of
room as well as the fact that something did, so an aggregation that quietly started
spilling would be visible rather than silently making the gate meaningless.

### Replaying the table log

One add per commit, replayed from the first.

| Commits | Before | After |
|---|---|---|
| 100 | 0.32 ms | 0.34 ms |
| 1,000 | 1.99 ms | 3.58 ms |
| 10,000 | 81.5 ms | 20.5 ms |
| 50,000 | **1.96 s** | **117 ms** |

**The first column is a defect, not a cost.** The replay searched the accumulated file
list for every add and filtered it for every remove — quadratic in the number of files,
invisible in every test, and two seconds per query plan at fifty thousand commits. A
table committing every ten seconds reaches that inside a week.

Positions are now held in an index. What remains is linear and is dominated by opening
one file per commit, which is what checkpoints exist to fix rather than anything an
algorithm can.

The regression guard measures the **ratio** between two sizes rather than elapsed time,
so it means the same thing on any machine. Its first version put the actions in thousands
of separate commits and passed against the very implementation it was written to catch —
the quadratic term is per action, not per commit, so file I/O buried it. Concentrating
the actions into ten commits separates them: 16.1× for four times the files against about
4× for the fixed version.

### Checkpointing the log

A cold reader — an external engine, or a process that has just started.

| Commits | Replay | From a checkpoint | | Checkpoint size |
|---|---|---|---|---|
| 1,000 | 1.90 ms | 0.39 ms | **5×** | 32 KiB |
| 10,000 | 28.7 ms | 3.21 ms | **9×** | 269 KiB |
| 50,000 | 141 ms | 13.9 ms | **10×** | 1.3 MiB |

This is the number that matters to **other engines**, which have no cache and start cold
every time. A Spark job reading a fifty-thousand-commit table opened fifty thousand files
before reading a row.

The kernel reads a checkpoint this system writes by hand — nested structs, maps and lists
in Parquet, against a protocol it does not own. And it is shown to *use* it rather than
merely tolerate it: the commits the checkpoint covers are deleted, leaving it as the only
record of those files, and the table still resolves. Without that step both checkpoint
tests would have passed whether the kernel read the file or ignored it and replayed,
which is precisely the shape of a test that proves nothing.

**Nothing depends on a checkpoint being present, correct, or parseable.** It holds exactly
what replay produces, so every failure — a missing file, a corrupt pointer, one left
behind by a table that was dropped and recreated — falls back to the log and costs a
replay rather than an answer. That is what makes it defensible to write this by hand: the
worst a bad checkpoint can do is be ignored.

### Caching a table's file set

The same table, replanned by a long-running process.

| Commits | Cold replay | Cache, unchanged | Cache, one new commit |
|---|---|---|---|
| 1,000 | 1.66 ms | 23 µs | 43 µs |
| 10,000 | 22.1 ms | 224 µs | 371 µs |
| 50,000 | 140 ms | **1.20 ms** | **1.88 ms** |

**117× on an unchanged table, 75× after a commit**, and both are now proportional to what
changed rather than to the table's history. The residual 1.2 ms is copying fifty thousand
file entries into the answer, which is what the caller asked for.

Two things had to change to get there, and each was worth about an order of magnitude:

- **Asking is a probe, not a listing.** Versions are contiguous — enforced at commit
  rather than assumed — so a reader that knows the state at version *n* asks whether
  *n+1* exists. Listing instead made every lookup proportional to the whole history, which
  is the cost the cache existed to remove.
- **The index is kept, not rebuilt.** Resuming from a bare file list rebuilds the position
  index over every live file, swapping *linear in the history* for *linear in the table* —
  better, and still not right.

The cache cannot go stale, and that is structural rather than careful: it never trusts its
own version, there is no invalidation, no expiry and no notification to miss. A table
dropped and recreated at the same path is detected and replayed from the beginning, rather
than resumed from a base describing files that no longer exist.

### Computing statistics at compaction

5,000,000 rows across three columns, merged.

| | |
|---|---|
| Merge including statistics | 357 ms |
| The statistics alone | 83 ms — **23% of the merge** |
| The same, before using vectorised kernels | 164 ms — 36% |

**This corrected a claim.** The first version of the module said bounds came from Arrow's
vectorised aggregates and were "close to free". They did not and were not: bounds were
computed in a scalar loop costing 29% of the merge on its own, five times what the
cardinality sketch costs. The comment was written before the measurement and was false
when written.

With the kernels actually used, bounds and widths are close to free and the remaining
23% is almost entirely the sketch, which has to hash every value. That cost is paid
because a cardinality estimate is the one statistic neither the file format nor the table
log carries.

23% on top of a merge is not nothing. It is still much cheaper than the alternative,
which is a second full pass over storage — and it means statistics arrive without anyone
running an analysis command, which matters on a system where tables onboard themselves
from a replication stream and the tables nobody thought about are exactly the ones that
would have none.

### File pruning

200 files of 2,000 rows, the same query with and without the catalogue.

| Predicate selects | With statistics | Without | |
|---|---|---|---|
| One file in 200 | 1.26 ms | 9.21 ms | **7.3×** |
| A tenth of the table | 3.89 ms | 10.81 ms | **2.8×** |
| Everything | 17.52 ms | 17.71 ms | **1.0×** |

The gradient is the point. Pruning helps in proportion to what a predicate can exclude,
so a single figure would be meaningless — and the last row matters as much as the first:
when nothing can be pruned, consulting the catalogue costs nothing measurable.

Every row of that table was checked to return the same answer with and without the
statistics, because a pruning measurement that does not verify the answer is measuring
how fast it can be wrong.

### Deterministic reduction

Reduction sums in a canonical order with Neumaier compensation on top. Searching which
of the two actually delivers order-independence produced a correction rather than a
confirmation.

| | |
|---|---|
| Randomised inputs tried, spanning 120 orders of magnitude | 3,000 |
| Permutations where compensation *alone* gave a different total | **0** |
| Hand-built adversarial cases where it did | **0** |

**Compensation is what makes the total order-independent in practice.** The canonical
sort — which the first draft of the documentation credited with the property — changes
no behaviour that could be observed. What it contributes is a *guarantee*: Neumaier's
error bound bounds the error without proving bit-identity across permutations, and its
compensation term is itself one `f64` that can lose bits when corrections span extreme
ranges. Sorting makes the result a function of the multiset by construction.

The sort is therefore defence in depth, at `n log n` against `n`. It is kept because the
figures it exists for have to be defended later, and *"we could not find a
counterexample"* is a weaker thing to say than *"there cannot be one"* — but the
documentation now says which of the two mechanisms is doing the work.

### Capture at scale

One million rows across all ten tables, in five interleaved transactions.

| | |
|---|---|
| Rows written | 1,000,000 in 2.0 s (~500k rows/s) |
| Messages decoded | 1,000,060 |
| **Rows captured through the pipeline** | **1,000,000 in 3.5 s (~285k rows/s)** |
| Retained write-ahead log during the run | 1,053 MB |
| Files published | 10 |
| Bytes published | 6.3 MiB |

**On the compression figure**, because it would otherwise be misleading: 6.7 bytes per
row reflects *this* data, which is deliberately regular — sequential identifiers,
patterned labels, timestamps clustered within a single run. Dictionary encoding,
delta-encoded monotonic columns and zstd all do unusually well on it. Do not read it as
a general ratio; the honest range against a realistic schema is closer to 3×–15× the
source's footprint, driven mostly by cardinality and index count. See
`REQUIREMENTS.md` §7.4 on estimates versus measurements.

Reproduce with:

```bash
SANKHYA_PG_BIN=$PWD/.build/pg-install/bin SANKHYA_E2E_SOCKET=/tmp/sankhya-sock \
  cargo test -p sankhya-ingest --test scale --release -- --ignored --nocapture
```

---

## How to check this document is honest

```bash
cargo xtask check-all            # every repository invariant: layers, file length, doc
                                 # links, version claims, feature pins, clippy with the
                                 # workspace's denied lints across every target, and that
                                 # no mutation is still applied to the source
cargo test --workspace           # 2,046 tests, none of which needs a database
cargo xtask check-performance    # the NFR-PERF objectives, as a gate that can fail
python3 tools/mutation-audit.py  # 519 specific defects, applied one at a time
crates/sankhya-cdc-apply/tests/run_e2e.sh   # capture against a live database
```

The performance gate needs a quiet machine and several minutes, which is why it is not in
`check-all`. Everything else runs unattended.

[`QUICKSTART.md`](QUICKSTART.md) walks through building it from nothing.
