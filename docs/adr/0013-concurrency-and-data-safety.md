<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# ADR-0013 — Concurrency and data safety, end to end

**Status:** Accepted · **Date:** 2026-08-28 · **Milestone:** M8
**Builds on:** [ADR-0009](0009-the-cube-lifecycle.md), [ADR-0012](0012-open-capabilities.md)

## Context

The direction, in the owner's words:

> *Is read, cube construction, update, roll-up, slice-dice concurrency safe? If not then M8
> should make these concurrency safe. Look at the whole platform and make it concurrency safe
> end to end. This whole system needs very high level of concurrency and data safety.*

An audit of every crate was done to answer it. The result divides cleanly, and the division is
the useful part.

### What is already safe, and why

| Path | Why it is safe |
|---|---|
| Read and query | A `SessionContext` per statement; providers are `Arc`-shared and immutable; `LogCache` guards its map with a `Mutex` |
| Roll-up, slice, dice | Pure functions over `&Cells`; `Published::cells` is an `Arc` and is never mutated |
| Cube construction | `Hydrated` and `QueryLog` hold `parking_lot::RwLock`; concurrent hydration of one key wastes work and cannot corrupt |
| Memory safety generally | `unsafe` appears in one crate, `sankhya-alloc`, and only to delegate `GlobalAlloc` |

**Nothing in the compute layers needs fixing.** The type system did that work. It is worth
stating explicitly, because the instinct on being asked "is it concurrency safe" is to harden
everything, and hardening what is already safe adds contention and hides the parts that are not.

### What is not safe, and the one shape it has

Every defect found is the same shape twice over: **a file becoming visible non-atomically, or a
claim that replaces instead of failing.**

| | Where | What happens |
|---|---|---|
| 1 | `table-delta::commit` | Claims a version by `path.exists()` then `rename`. `rename(2)` replaces its destination silently, so two committers both see the version free and the second overwrites the first — a commit vanishes with **no error to either party**, and the rebase loop never runs because the `VersionTaken` it waits for is never returned |
| 2 | `cube::catalogue::save` | `fs::write` onto the live path. A cube definition is truncated then rewritten, so a concurrent `load` reads a partial file |
| 3 | `table-delta::checkpoint` | The checkpoint parquet is correctly staged and renamed; the `_last_checkpoint` pointer beside it is written directly |
| 4 | `server::backup` | The manifest is written directly onto its path |

The revealing detail is that **the technique is already known here and applied unevenly.** The
commit body, the checkpoint parquet and `diagnostic::history` all stage and rename correctly.
Three places do it right, four do it wrong, and nothing checks which.

### The third hazard: reclamation without readers

Three paths delete files a reader may be holding, and **no reader registry exists anywhere** in
the system:

| Path | Guard | What the guard is |
|---|---|---|
| Compaction inputs (`execute.rs`) | `grace_ticks = 24` | elapsed maintenance ticks |
| Orphan sweep (`orphans.rs`) | `min_age_ticks = 604800` | elapsed seconds — seven days |
| Superseded cuboids (`wiring.rs`) | `CUBOID_DRIFT_TOLERATED = 100` | elapsed **table versions** |

Each is a heuristic in a proxy dimension, and each is documented as one. `CUBOID_DRIFT_TOLERATED`
even says why it exists: *"a query that resolved a cuboid a moment ago is still reading it and a
file deleted from under a running scan fails naming a path the caller never mentioned."*

The guards are generous and mostly work. They are not guarantees, and version-space is the
weakest proxy of the three: under continuous ingest a hundred versions can pass in seconds,
while an analytical scan can run for minutes. **This is not theoretical — the failure was
observed during M7**, as an error naming a Parquet file the caller had never mentioned.

## Decision

**Two properties, and one mechanism, applied everywhere.**

### Property 1 — Atomic publication

Any file a reader can name becomes visible **all at once or not at all**. Written to a staging
name no other writer can be using, then linked or renamed into place.

The staging name carries the process id and a per-process counter. A shared staging name is its
own defect: two writers racing for one destination write the same temporary path, and either can
publish the other's bytes.

### Property 2 — A claim fails rather than replaces

Where a name must be claimed exclusively — a commit version above all — the operation **fails
when the destination exists**. It never replaces it.

On a local filesystem that is `link(2)`, which returns `EEXIST`. On an object store it is a
conditional put: `If-None-Match: *` for S3 and Azure, `ifGenerationMatch=0` for GCS. The required
property is identical, and stating it this way is what keeps the two implementations honest
against each other.

This is not a new concurrency design. It is the protocol's existing optimistic control —
*"a writer picks the next version and fails if someone took it, and the loser rebases"* — finally
given a primitive that can deliver it. Everything above it already handles the loss correctly.

### Mechanism — reclamation waits for readers, not for a proxy

Deletion is gated on **no reader holding the file**, not on elapsed ticks or versions.

The pattern is already in this system: `ARCHITECTURE.md` §CDC describes an epoch-based immutable
ring where *"readers take a reference and are never blocked by, and never block, the writer."*
The same shape applies to files. A reader registers what it resolved; reclamation skips anything
registered; the elapsed-time guards remain as a **backstop against a leaked registration**,
which is what they are actually good at.

Elapsed-time guards are kept and demoted, not deleted. A registry with a leak and no backstop
never reclaims anything, which is the failure this warehouse has already met from the other
direction.

## The rule, stated so it can be checked

> **No writer may make a file visible by writing to the path a reader will open, and no writer
> may claim a name by first checking that it is free.**

Both halves are mechanically checkable, and a gate — `check-atomic-writes` — will check them:
`fs::write` and `File::create` targeting a live path are refused outside the one publishing
helper, and `exists()`-then-`rename` is refused outside it too.

A convention applied by hand held in three places and lapsed in four. That is what conventions
do, and it is why this becomes a gate rather than a paragraph.

## Safe is not the same as concurrent

The owner's requirement is two things, and conflating them would satisfy neither:

> *It is ok to push the M8 estimate, but once M8 is done the system needs to be **highly
> concurrent**.*

**Safety** is correctness under concurrent access. **Concurrency** is throughput under it. A
single global lock around every write delivers the first perfectly and destroys the second, and
it would pass every test in the section above.

So the properties are stated together, and the second is measured rather than asserted:

| | Property | How it is shown |
|---|---|---|
| S1 | A commit is never lost | N writers, one version: exactly one wins, the rest are told |
| S2 | No reader sees a partial file | N readers against a writer republishing: every read parses |
| S3 | No file is deleted while read | Reclamation under continuous scan: no missing-path failure |
| C1 | Writers to **different tables** do not contend | Throughput scales with writers; per-table commit paths, never a global one |
| C2 | Readers are never blocked by writers | Read latency under write load is flat |
| C3 | Contention on **one** table degrades gracefully | Rebase-and-retry, bounded; a failure is diagnosable, not a hang |

> **Measured 2026-08-29, each against a control taken in the same run.** C1: commits to eight
> tables run at **4.82×** one table's rate, where the same commits behind one warehouse lock
> run at **0.91×**. C2: a reader holds **0.59–0.80** of its idle rate under four writers with a
> p99 of 227 µs, where a reader sharing a lock with those writers holds **0.00–0.07** and waits
> seconds for a turn. C3: sixteen writers on one contested version all commit, worst rebase
> count **eleven**.
>
> The control is the part worth keeping. Every S-property above is satisfied by one lock over
> the warehouse, so a C-measurement without a serialized arm beside it cannot distinguish the
> design from the one it forbids — and a threshold chosen without both states is taste. C1 is
> measured twice for the same reason: end to end through a publish, a lock over **only** the
> commit still scales 1.87×, because encoding Parquet is untouched and is most of a publish.
>
> **Amended 2026-08-31.** A serialized arm cannot say whether the *machine* could have scaled
> anything, and C1's end-to-end measurement duly failed inside `cargo test --workspace` at a
> load average of 36 on a 24-core machine, with nothing wrong with the code. All three
> measurements ask for free capacity directly now — `sankhya-testkit::capacity` — and skip
> loudly, by name, when the cores they need are not there. A ratio over a workload that shares
> nothing was tried first and does not work: fair scheduling gives every runnable thread an
> equal share, so that control reports near-linear scaling however busy the machine is.
>
> The skips were also invisible, on both ends: libtest captures a passing test's output, and
> `check-tests` printed only a count. **A criterion that quietly stops being measured is worse
> than one that fails**, so the skip now writes past the capture and the gate lists what was
> skipped beneath its total.
>
> **Amended again the same day.** Sampling free capacity *before* the arms was still not enough:
> C3 failed with that fix in place, having begun on an idle machine and finished on a saturated
> one. The check now brackets the measurement — a window opened before and closed after — and a
> measurement whose window did not hold is discarded rather than asserted on.
>
> **And that was still not enough.** A third failure arrived with the window holding at both
> ends, because a sub-second measurement can be ruined by a transient neither probe sees. The
> measurements are therefore `#[ignore]`d and run by `check-concurrency` **alone, as the only
> cargo process** — the interference removed rather than detected. They remain inside the gate,
> because a measurement moved out of it is a measurement that stops being taken.

C1 is why [`ARCHITECTURE.md`'s standing note](../ARCHITECTURE.md) — *"keep the commit path
per-table, never globally serialized"* — stops being a design seam and becomes an exit
criterion. The cheapest way to fix everything in this document is one lock over the warehouse,
and that is precisely the outcome C1 exists to forbid.

C2 is the property the CDC ring already has and the file layer does not: *"readers take a
reference and are never blocked by, and never block, the writer."* Reclamation must be built to
preserve it, which rules out any design where a reader takes a lock a sweeper can hold.

## No global lock, and where the temptation actually is

The owner, unprompted and correctly:

> *Don't make one global lock — we don't want a Python GIL-like handicap.*

This is the failure mode the C-properties exist to forbid, and it is worth being concrete about
where it would creep in, because two thirds of this design cannot introduce it even by accident.

**The claim and publish primitives need no lock at all.** `link(2)` and `rename(2)` are atomic
in the kernel, per directory entry. Two writers on different tables never touch the same inode,
and two writers on the same table contend for exactly one name — which is the contention the
protocol is *supposed* to have, and which resolves by rebase rather than by waiting. There is
nothing to serialize and no lock to take.

**The reader registry is where a GIL would appear**, and it is the whole of the risk. The naive
implementation is one `Mutex<HashSet<PathBuf>>` for the warehouse: every reader locks it to
register, every sweeper locks it to check, and the system acquires a global mutex on the read
path. That is a GIL with a filesystem accent — and it would pass every safety criterion in this
document, which is exactly why the concurrency criteria are stated beside them.

Three rules keep it out:

1. **Per-table, never per-warehouse.** Registration is scoped to the table being read, so
   readers of different tables share no structure. This is the same rule as *"keep the commit
   path per-table, never globally serialized"*, applied one layer up, and it is why that note
   graduates from a design seam to an exit criterion.

2. **Readers must not take a lock a sweeper can hold.** Registration is an atomic counter
   increment or an epoch publish, not a mutex acquisition. A sweeper reads the epochs and skips
   what is live; it never makes a reader wait. This is the property the CDC ring already has and
   the file layer does not: *"readers take a reference and are never blocked by, and never
   block, the writer."*

3. **The read path may not grow a lock it did not have.** Reads today take no warehouse-wide
   lock, and this work must not add one. If a design cannot register a reader without one, the
   design is wrong — reclamation may be conservative and delay a deletion, and it may not slow
   a query down to be sure.

**The measurement is exit criterion 5**, and it is stated in the form that catches this: read
latency flat under write load. A global lock cannot pass it, however carefully it is written.

## Techniques, and the sites that need them

The owner:

> *Use concepts of lock striping where needed, be smart and creative.*

Applying that lens to the read path found three choke points that are **not safety defects** —
every one is memory-safe and returns correct answers — but are throughput defects of exactly the
kind criterion 5 exists to catch. Worth separating: the earlier audit asked *"can this corrupt?"*
and these were invisible to it, because the answer is no. The question here is *"does this
serialize?"*, and the answer is yes.

### The choke points

| Site | Today | Cost |
|---|---|---|
| `LogCache::live_files` | **One `Mutex<HashMap<PathBuf, Replay>>` for every table**, and it is **held across filesystem I/O** — `newest_after` probes the log, and a cold replay runs to completion inside it | Every query on every table takes one lock. A cold replay of a large log blocks queries against unrelated tables for its whole duration |
| `QueryLog::record` | `asks.write()` — a write lock over **all** cubes | Taken on every cube query, so cube navigation serializes across unrelated cubes |
| `Hydrated::put` | `entries.write()` — a write lock over all cubes and measures | One cube's hydration blocks every other cube's cache read |
| `Server::servable` | `RwLock<Vec<ServableTable>>`, read on every query | Read-mostly with rare writes; a lock is being paid for on the hot path to protect a list that almost never changes |

### The techniques, matched to them

**1. Don't hold a lock across I/O — the biggest win, and not a striping problem.**
`LogCache` should take the lock to *look*, release it, do the filesystem work, then re-acquire to
*install*, resolving a concurrent install by version comparison. Striping a lock that is held
across a disk read only reduces how many threads wait; removing the I/O from the critical
section changes what they are waiting for. Do this first.

**2. Lock striping for the cache maps.** A fixed array of shards, indexed by a hash of the table
root: `shards[hash(path) % N]`. Different tables then never contend at all, and N is chosen from
core count rather than guessed. The same for `QueryLog` and `Hydrated`, keyed by cube name.

**3. Per-entry locks under a shared map lock.** Better than striping where the work under the
lock is per-key: hold a *read* lock on the map to find an `Arc<Mutex<Ring>>`, release it, then
take the tiny per-entry lock. Concurrent records to different cubes never meet; two records to
one cube contend for one small mutex and nothing else. `QueryLog` fits this exactly — the ring
is a handful of pointers and the lock is held for a push.

**4. Atomic swap instead of a lock, for read-mostly state.** `Server::servable` is read on every
query and written when the warehouse changes. That is the classic `arc_swap::ArcSwap` shape: a
reader does an atomic load and takes no lock at all; a writer builds a new `Vec` and swaps the
pointer. The read path loses a lock rather than gaining a faster one — which is the goal, since
**the best lock on a hot read path is the one that is not there**.

**5. Epoch counters, not a set, for the reader registry.** Per table, an atomic counter a reader
increments and decrements, plus the version it pinned. A sweeper reads the counters and skips
anything pinned. No allocation, no map, no mutex, and nothing a reader can block on — which is
rule 2 of the previous section, satisfied by construction rather than by care.

### Partition-granular lock pools — adopted, with one exclusion

The owner:

> *At table level there can be a pool of locks which basically guard a partition.*

**Adopted.** Tables here are partitioned by design — `FR-STORE-20` requires partition columns —
so the partition is a real boundary with real independence behind it, not an arbitrary shard key.
Two operations on different partitions of one table genuinely do not interact.

A **pool** is also the right shape rather than a lock per partition, and the distinction matters
more than it looks. Partitions are open-ended: a daily-partitioned table grows one per day,
forever. A lock per partition is unbounded memory plus a map lookup — and that map then needs its
own lock, which recreates the choke point one level down. A fixed pool indexed by
`hash(partition) % N` is constant memory, no allocation, and no lookup. Collisions cost a
spurious wait between two unrelated partitions, which is cheap and bounded; the pool is padded to
cache lines so neighbouring locks do not false-share.

Where it applies:

| Site | Why partition granularity is right |
|---|---|
| **Compaction** | Compaction merges the small files *within* a partition. Two partitions of one table are wholly independent work, and today a large table is compacted serially. `ARCHITECTURE.md` already names maintenance throughput as *"the most likely place the architecture must change first"* — this is that change |
| **The reader registry** | A sweeper reclaiming files in partition `P` needs to know only whether a reader is pinning `P`. Per-partition pinning means a long scan of one partition never delays reclamation anywhere else — strictly better than the per-table registry proposed above, and it replaces it |
| **Orphan sweeping** | Same argument: unreferenced files are found and removed per partition, so the sweep parallelizes and its blast radius per lock is one partition |

**Where it does not apply, and this is worth being plain about: the commit itself.**

A Delta commit is table-scoped by the protocol — one monotonic version per table, one log. Two
writers appending to *different* partitions still contend for the same next version number, and
no lock granularity can change that, because the contention is over a counter rather than over
data. Partitioning the lock would only move the queue.

But the observation still pays off there, in a different currency. Two writers whose files land
in disjoint partitions have **no logical conflict** — they are serialized by bookkeeping, not by
meaning. That is exactly what `FR-STORE-23`'s typed conflict is for, and partition sets are what
make the classification cheap: the loser compares its partitions against the winner's, and if
they are disjoint it rebases without recomputing anything. Where they overlap, the conflict is
real and must reschedule.

So the idea is adopted in three places, and in the fourth it becomes a *conflict test* rather
than a lock — which is the better answer there anyway, since the winner has already committed and
there is nothing left to wait for.

### The rule behind the choices

**Lock what is contended, not what is convenient.** Every structure above was written with one
lock because one lock is the obvious correct thing, and each is correct. They became choke points
by being on a path that got hot later — which is the ordinary way this happens, and the reason
criterion 5 measures rather than reviews.

## Consequences

**M8 gains a block before scale-out, and the estimate moves.** Accepted by the owner on
2026-08-28: *"it is ok to push the M8 estimate."* Recorded as a scope increase rather than
absorbed silently, so the original 16–20 engineer-week figure is not quietly reinterpreted.

**The ordering is a decision, not a preference.** Leader election is how a system *avoids
needing* concurrency safety, so it is tempting to do it first and call the problem solved. But
M8's shape is multi-node with cache-affinity routing: many readers on other nodes, racing with a
leader's compaction and retirement, holding paths that leader is deleting. Safety must exist
before the topology that stresses it — otherwise the first failure arrives looking like a
networking fault, and is debugged as one.

**Every fix needs a failing test first.** Every defect above was invisible to a suite of
seventeen hundred tests, for one reason: every test had a single writer. A test that cannot
observe a race is not evidence about races, and adding the fix before the test would produce a
suite that agrees with the fix rather than one that would have caught the bug.

**`hard_link` constrains the filesystem, and the constraint is accepted.** ext4, xfs, APFS and
NTFS support it; FAT does not, and some network mounts are unreliable. Owner decision,
2026-08-29: **ruling out FAT is fine.**

Recording it as a decision rather than an implementation detail, because it is now load-bearing
in two directions. A warehouse on a filesystem without hard links cannot claim a commit version
safely, and the failure would not be a refusal --- `link` would return `EPERM` or `ENOSYS` and
the commit would report an I/O error rather than a lost update, which is at least loud. And
§12.2 builds on the same primitive: the object-store spelling is a conditional put, and a store
that does not offer one cannot host a warehouse. `sankhya-objectstore`'s conformance probe
exists to find that out at configuration time rather than during a race.

## What this does not decide

Where the reader registry lives when readers are on other nodes — in M8 that is a distributed
question and probably belongs with leader election rather than with this. Whether the object-store
path is built now or when a store is first supported. And whether the elapsed-time backstops keep
their current values once they are backstops rather than the guard.
