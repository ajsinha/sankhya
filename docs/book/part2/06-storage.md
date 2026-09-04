# 6. Storage and the open table log

> A directory of Parquet files is a storage layout; it is not a table. This chapter shows why —
> between a merge and the retirement of its inputs, the directory holds the same rows twice, for
> at least a full grace period — and builds from that the case for an open transaction log. It then gives the two mechanical properties every write in the system obeys
> (a file becomes visible all at once; a name is claimed by an operation that fails rather than
> replaces), and the compaction level structure that bounds write amplification to roughly 3×.
> Every number here is from this project's own benchmarks, with its conditions.

## 6.1 The layout

```
<warehouse_root>/
  <schema>/                    mirrors the source schema name
    <table>/                   self-contained; the unit of external readability
      _delta_log/
      sank_data_date=YYYY-MM-DD/
        <data files>
```

One name spans four naming domains — source identifier, object path, catalog namespace, and the
name a user types — so a table's origin is identifiable without a lookup table. Because
PostgreSQL folds unquoted identifiers to lower case, for the large majority of tables all four
are **the same string with no transformation at all**.

Three properties are engineered rather than assumed. Escaping is human-legible, not hashed, and
the separator chosen is not legal in an unquoted source identifier — *so its presence is itself
a signal that a transformation occurred*. Collisions are refused loudly at onboarding, never
silently merged; an earlier proposal to disambiguate by hash suffix was withdrawn, because it
guaranteed uniqueness by destroying the readability that was the entire point. And identity is
recoverable from the table directory alone, with no catalog and no SANKHYA process running,
because relatability must survive the system being switched off.

The warehouse root contains **only** externally-meaningful published tables. Internal state
lives in a separate data directory, and a foreign object appearing under the warehouse root is
detected at startup and refused rather than ignored.

The warehouse path is a *published interface*. Additive schema changes are backward-compatible
for consumers and are applied automatically. A table rename is a breaking interface change to
consumers SANKHYA cannot see, and breaking changes require a human. A column rename and a table
rename therefore have opposite policies — with field identifiers enabled a column rename is
metadata-only and fully automatic, while a table rename breaks a path. Same word, different
contracts.

## 6.2 Why there is a log at all

This is the argument that decides the whole chapter, and it is concrete rather than
philosophical.

Between a merge and the retirement of its inputs, the directory holds **both** — the file that
was written and the files it replaced, *the same rows twice*. That window lasts at least a full
grace period and exists by design (§6.6). So anything answering *"which files belong to this
table"* by listing the directory is wrong for the whole of it:

- A planner given a listing will plan a merge whose inputs include files an earlier merge
  already superseded, and the result contains those rows twice — **permanently**, this time.
- A reader given a listing double-counts every merged row for the duration of the window.

> **Key idea**
> This is the concrete reason the system needs a table log rather than merely liking the idea.
> *"Which files are live"* is not answerable from the filesystem once compaction has run, and
> both correctness properties above depend on answering it.

The live set is therefore a first-class value carried across maintenance ticks, not something
derived from storage. The property is exercised by a negative test: a query registered against
the *directory* is shown to return merged rows twice, while the same query against the *live
set* returns them once.

## 6.3 The log, written by hand

The table log is emitted by SANKHYA directly — a few hundred lines covering `protocol`,
`metaData`, `add` and `remove`, one JSON object per line, staged and renamed so a reader never
observes a partial commit. Concurrency control is the Delta protocol's own: a writer picks the
next version and fails if someone took it, and the loser rebases, because its decisions were
made against a state that no longer exists.

> **Pitfall**
> *"Fails if someone took it"* is the property the design requires and, until the concurrency
> milestone, **not the one the code delivered**. `commit` claimed a version by checking the file
> was absent and then renaming a staging file over it — and `rename(2)` replaces its destination
> silently. Two committers could both see the version free, and the second would overwrite the
> first with no error to either. The rebase loop never ran, because the conflict it waits for was
> never returned. It went unseen because **every test had a single writer per version**.
> §6.4 is the fix; [ADR-0013](../../adr/0013-concurrency-and-data-safety.md) is the record.

### A commit says how long it is

The first line of every commit is a `commitInfo` seal carrying the number of actions that
follow. The reader counts what it reads and refuses the commit when the two disagree.

The reason is that the alternative is indistinguishable from success. A commit body truncated
by a crash — a partial write, a full disk, a killed process between the staging write and
the sync — replays as a *shorter commit*, and a commit whose lines are all missing replays
as a commit that did nothing. Neither is an error to a reader that simply parses the lines it
finds. Worse, the state is cemented: a retry at the same version is refused as `VersionTaken`,
so the next commit lands on top of the truncated one and every `add` the crash swallowed is
gone from the live set for good.

An action-less body is therefore rejected outright rather than read as an empty commit. SANKHYA
never writes one, so the only way to observe one is the failure this seal exists to catch.

Two properties come along with it. Lines are parsed as generic JSON before their `kind` is
read, so an action from another engine is *counted and passed over* rather than turned into a
parse failure that renders the table permanently unreadable — which is the protocol's own
rule for forward compatibility. And the seal is itself an action for counting purposes, so a
reader cannot satisfy the count by mistaking the header for data.

### The kernel as an oracle

The Delta kernel is a **dev-dependency**, used as an independent oracle: it reads the log
SANKHYA wrote and must agree about the schema, the version and the live set. Keeping it test-only
keeps eighty-four packages and a duplicated HTTP client out of the shipped binary, and that the
dependency stays test-only is checked mechanically rather than left to review.

The oracle earned its place on its first run. The log this system wrote was **invalid**: the
`add` action's `partitionValues` field is non-nullable and had been omitted. It round-tripped
through SANKHYA's own reader perfectly, because a reader ignores a field it never writes.

> **Key idea**
> Two implementations agreeing is worth nothing when the same author wrote both sides. This is
> the same rule as the single-writer rule of Chapter 5 and the reconciliation-harness rule of
> Chapter 9, and it is the reason the open-storage claim is stated as *exercised* rather than as
> *supported*.

### Checkpoints

Every ten versions the reconciled state is written as a single Parquet file with a
`_last_checkpoint` pointer, and readers start from it.

| Commits | Cold replay | From a checkpoint | Speed-up | Checkpoint size |
|---|---|---|---|---|
| 1,000 | 1.90 ms | 0.39 ms | 5× | 32 KiB |
| 10,000 | 28.7 ms | 3.21 ms | 9× | 269 KiB |
| 50,000 | 141 ms | 13.9 ms | 10× | 1.3 MiB |

The beneficiary is mostly *other engines*. They have no cache and start cold on every query, so
without a checkpoint an external reader opens one file per commit before it reads a row.

A checkpoint holds exactly what replay produces, which makes it safe in a specific way: **it can
always be discarded.** A missing file, a corrupt pointer, or one left behind by a table dropped
and recreated at the same path all fall back to the log and cost a replay rather than an answer.
Writing it is a *maintenance* job and not part of committing, because a commit that had to
checkpoint could fail for a reason that does not matter.

A file-set cache sits above the same path for this process's own reads:

| Commits | Cold replay | Cache, unchanged table | Cache, after one commit |
|---|---|---|---|
| 1,000 | 1.66 ms | 23 µs | 43 µs |
| 10,000 | 22.1 ms | 224 µs | 371 µs |
| 50,000 | 140 ms | 1.20 ms | 1.88 ms |

That is 117× on an unchanged table and 75× after a commit. Log replay itself was also made
linear: at 50,000 commits it went from **1.96 s to 117 ms**, and the regression guard asserts the
*shape* — 16.1× for four times the files against about 4× for the fixed version — rather than a
threshold that a quadratic implementation could still pass.

### Statistics in the log — a recorded reversal

Bounds and null counts are written into the log alongside the row count. This reverses an
earlier decision, and the reversal is recorded rather than quietly made.

The original objection stands as the discipline that governs what is written: every bound comes
from code that refuses to produce one it cannot justify. An unrecognised type gets no bound; an
unorderable value gets no bound; a merge that would narrow a bound drops it instead; and a value
the protocol cannot represent exactly — a non-finite float, bytes that are not text — is omitted
rather than approximated.

The reason for reversing is the open-storage claim itself. **An external engine can prune only
on what the log tells it.** Keeping bounds private to SANKHYA means every other reader scans
everything, which undercuts the reason for choosing an open format at all. The bar is therefore
higher now rather than lower: a malformed statistic costs *other people* answers, in engines
that cannot be fixed from here.

The cardinality sketch stays out, because the protocol has nowhere to put it. A column read back
from the log therefore reports zero distinct values — which is a trap for whatever reads that
figure first, and is named here so it is not discovered as a defect.

### One detail that is silent if wrong

A compaction's `remove` actions declare `dataChange: false`. Compaction rewrites files without
changing rows, and a reader streaming changes from the table would otherwise see every compacted
row as a deletion followed by a re-insertion — **a flood of spurious changes proportional to how
well maintenance is working.**

## 6.4 Atomic publication

Every defect found in this system's write paths during the concurrency audit was the same shape
twice over: *a file becoming visible non-atomically, or a claim that replaces instead of
failing.* Four instances existed simultaneously — the table commit, the cube catalogue save, the
checkpoint pointer, and the backup manifest.

The revealing detail is that **the technique was already known and applied unevenly**. Three
places did it right, four did it wrong, and nothing checked which.

### Property 1 — atomic publication

Any file a reader can name becomes visible **all at once or not at all**. It is written to a
staging name no other writer can be using, then linked or renamed into place.

The staging name carries the process id and a per-process counter. A shared staging name is its
own defect: two writers racing for one destination write the same temporary path, and either can
publish the other's bytes.

### Property 2 — a claim fails rather than replaces

Where a name must be claimed exclusively — a commit version above all — the operation **fails
when the destination exists.** It never replaces it.

| Substrate | Primitive | Failure signal |
|---|---|---|
| Local filesystem | `link(2)` | `EEXIST` |
| S3, Azure | Conditional put | `If-None-Match: *` |
| GCS | Conditional put | `ifGenerationMatch=0` |

The required property is identical, and stating it this way is what keeps the two
implementations honest against each other. It follows that **a store which does not offer a
conditional put cannot host a warehouse safely**, and the system should find that out at
configuration time rather than during a race.

This is not a new concurrency design. It is the protocol's existing optimistic control — *a
writer picks the next version and fails if someone took it, and the loser rebases* — finally
given a primitive that can deliver it. Everything above it already handled the loss correctly.

The two operations are separate functions with names that say which is which. `publish` makes a
file visible all at once, replacing whatever was there: last writer wins. `claim` makes a file
visible all at once and **fails if the name is taken**. Using `publish` where `claim` was meant
silently loses the loser's work.

There is a subtlety in `claim`'s implementation worth stating because the obvious version is
wrong: `File::create_new` claims the name atomically but then writes *into* it, so a reader that
opens between the claim and the last byte sees a partial file. The correct sequence is to write
a staging file in full and then hard-link it into place.

### Property 3 — a data file name is used once

Atomic publication settles what a reader sees of *one* write. It says nothing about a second
write to the same name, and that is a distinct hazard with a distinct fix: the publisher, not the
caller, decides the name. A caller asking for `part.parquet` while attempting version 7 gets
`part-v0000007-1a2f3.parquet`.

The two halves of that name answer two different collisions.

**The version** is the readable half, and it settles the single-publisher case. A publisher that
flushes one logical name twice — which the accumulator does, once per round — used to write the
same path twice, and the second write truncated the first while the first's `add` was still in
the log: rows acknowledged to a caller were gone from disk while the live set insisted they were
there. It records the commit the write was *attempting*, which is usually the commit it landed
at; a writer that loses the race rebases onto a later version and keeps the file it has already
written, because the bytes do not depend on which commit names them.

**The token** — a process id and a per-process counter — is the half that makes the guarantee,
and it is there because the version alone does not. Two publishers read the same `next_version`,
so both intend the same version, so both compute the same name from the same caller-supplied one.
That is the collision the concurrency tests could not see, because they gave every writer a
distinct file name. It is the same scheme `atomicfs` uses for staging names, for the same reason:
two writers racing for one destination must not be able to pick one temporary path.

Compaction has the same requirement and a different key. Its output is named from a sequence,
and that sequence was a counter that started at zero on every restart, so a restarted service
reissued a name the log already held. The planner then chose that file as its own merge input,
the writer truncated it, and the commit added and removed one path — taking the merged
partition out of the live set. The sequence is now **recovered from the log**: the highest
`compacted-NNNNNN` in the live set, plus one.

Under both of these sits a floor. `write_parquet` opens with `create_new`, so a name that
already exists is refused rather than truncated — because a data file whose name exists
belongs to rows some log still refers to. This floor is what found the accumulator defect: the
reasoning above is reconstructed from a test that started failing, not from a review.

It found a second one, quieter. The publisher's *report* still named the file the caller had
asked for, while the write, the `add` action and the error path all used the versioned name — so
a caller was handed a path that does not exist. Harmless to a caller that counts the result, a
missing file to any caller that opens it, and compaction is the second kind.

### The rule, stated so it can be checked

> **No writer may make a file visible by writing to the path a reader will open, and no writer
> may claim a name by first checking that it is free.**

Both halves are mechanically checkable and are checked by a gate: `fs::write` and `File::create`
targeting a live path are refused outside the one publishing helper, and `exists()`-then-
`rename` is refused outside it too.

The reason this is a gate and not a paragraph: *a convention applied by hand held in three
places and lapsed in four.* That is what conventions do.

### Reclamation waits for readers, not for a proxy

The third hazard in the same audit was deletion. Three paths removed files a reader might hold,
and each was guarded by a *proxy* for reader activity rather than by reader activity:

| Path | Guard | Proxy dimension |
|---|---|---|
| Compaction inputs | 24 grace ticks | time |
| Orphan sweep | 604,800 ticks (seven days) | time |
| Superseded cuboids | 100 table versions | **version space** |

Version space is the weakest of the three: under continuous ingest a hundred versions can pass
in seconds, while an analytical scan can run for minutes. This was not theoretical — the failure
was observed as an error naming a Parquet file the caller had never mentioned.

So deletion is gated on **no reader holding the file**. A reader registers what it resolved;
reclamation skips anything registered; and the elapsed-time guards remain as a *backstop against
a leaked registration*, which is what they are actually good at. They are demoted rather than
deleted, because a registry with a leak and no backstop never reclaims anything — the same
failure met from the other direction.

The registry's own summary states the property both ways round: *nothing is deleted while
somebody is reading it, and no reader ever waits to say so.*

### The backstop was written as an `and`

The paragraph above is what the code said. What it did was `old_enough && unreachable`, with
both as requirements — so the backstop was not a backstop. One leaked registration held every
merge input on disk for ever *and* held the queue naming them in memory for ever, growing by one
entry per merge, and nothing reported why.

One number cannot mean both things, so there are two. The grace period is a **minimum**: nothing
is retired before it, drained or not. The leak backstop is a **maximum**, and it applies only
where the registry still claims a reader — past it, a registration that old is not a query, it is
an announcement that was never withdrawn. The default is a hundred grace periods.

Two things the backstop deliberately does not override. It fires only against the *lease* check;
a clone pin and a snapshot pin still refuse the file, because those are not proxies for anything
and there is no timeout at which they become wrong. And when it fires it says so in the tick
report, because a backstop firing is never routine: it means a lease leaked, and that is a defect
somewhere else that nothing else in the system would have surfaced.

### Four ways to answer "nothing reads this" when something did

Every defect the audit found in reclamation had the same shape, and none of them was a race.
Each fired on a schedule, deleted data something was still reading, and reported nothing.

| Where | The answer it gave | Why |
|---|---|---|
| A clone's pin | *no clone reads this* | The lineage records `sales.orders`; the sweeper had a directory, and a directory knows only its leaf |
| The orphan sweep | *no snapshot reads this* | It honoured clone pins and not snapshot pins — while retirement, a hundred lines away, unioned both |
| An unreadable snapshot | *this pins no files* | The document would not parse, and the failure was discarded |
| A leaked lease | *this reader is still here* | Above |

The first is worth dwelling on, because it was false in **every deployment** rather than
sometimes. `discover` only ever walks `<warehouse>/<schema>/<table>`, so the qualified form is
always what gets recorded — and the comparison against the bare directory name could therefore
never be true. It survived because the one test covering it built its table at the warehouse root
and recorded a bare name: the single shape in which the two forms cannot disagree.

The second was not a race either, and in the opposite way to how that usually reads. Retirement
correctly declines a pinned file *for ever*, which **guarantees** that file crosses the orphan
sweep's age threshold. Every snapshot older than a week lost its files, reliably.

> **Key idea**
> A pin that could not be read now **stops reclamation** rather than contributing nothing.
> Contributing nothing is indistinguishable from *"this protects no files"*, so the one case
> where the system did not know what was protected was the case in which it deleted it. The
> earlier reasoning here weighed a full disk against lost rows and got the order backwards:
> reclamation that stops is visible in a report, costs storage, and is fixed by deleting one
> file. Reclamation that runs on an unknown pin set is visible when somebody queries, and
> nothing fixes it. Compaction continues either way — it adds files and removes none.

### One server per warehouse

`claim` serialises two committers **at a version**. That is the whole of its scope, and
maintenance lives outside it.

Two servers on one warehouse each plan compactions against a live set the other is changing, each
retire merge inputs against a lease registry that cannot see the other's readers, and each sweep
orphans against an age threshold that has no idea a file belongs to a commit the other has not
written yet. None of that races at a version, so none of it was caught — and nothing prevented
it: there was no lock file, no pid file and no advisory lock anywhere.

`flock(2)` is the better primitive and is out of reach: `unsafe_code` is `forbid` at the
workspace root and `libc` is confined to the sandbox crate by `check-layers`. So the lock is a
file, claimed with the same exclusive create as everything else here and naming the process that
holds it. What a file cannot do by itself is notice that its holder died, and a lock that refuses
for ever after one crash is a lock an operator learns to delete on sight — which is no lock.

Liveness is therefore established exactly, from `/proc`, and the recorded value is the holder's
**start time** and not only its pid, because pids are reused and a lock broken on a reused pid is
two servers on one warehouse:

| What is found | What happens |
|---|---|
| No lock file | It is taken |
| A pid with no process, or a pid whose start time differs | The holder is gone; the stale lock is taken |
| A pid with the recorded start time | The holder is running; startup is refused, naming it |
| A lock that cannot be parsed, or no `/proc` to ask | Startup is refused, and the operator is told to remove the file |

The last row is deliberately the unhelpful one. Every automatic way out of it ends in two servers
on one warehouse, which is the thing being prevented.

## 6.5 What the commit protocol buys, measured

Two properties were measured against a control taken in the same run, because every safety
property here is also satisfied by a single lock over the warehouse — the design the milestone
existed to forbid.

| | As built | Behind one warehouse lock |
|---|---|---|
| Commits to eight tables, against one table's rate | 36,972 → 178,259 commits/s (**4.82×**) | 37,456 → 33,940 (**0.91×**) |
| The same, end to end through a publish | 4.4× to 5.9× | 0.9× to 1.2× |
| A reader's rate under four writers | 0.59–0.80 of idle, p99 227 µs | 0.00–0.07, p99 in **seconds** |
| Sixteen writers on one contested version | all commit; worst rebase count 11 | — |

Two details make the numbers mean something. With a lock over *only the commit* — not the whole
publish — an eight-writer publish still scales 1.87×, because encoding Parquet is untouched and
is most of a publish; so the end-to-end figure and the commit-path figure answer different
questions and both are reported. And the measurement itself required four corrections: the arms
interfering with each other under a workspace test run, a capacity probe that counted `iowait`
as idle (right for processor capacity, wrong for arms that write Parquet), a window that held at
both ends but not in the middle, and binaries being compiled while a sub-second measurement ran.
The measurements are now `#[ignore]`d and run alone by a dedicated gate — *the interference
removed rather than detected* — and they stay inside the gate, because a measurement moved out
of a gate is a measurement that stops being taken.

The rebase loop's budget is also empirical rather than chosen. Over sixteen hundred real commits
with eight writers on one table, the rebase count had a mean of five, a median of three, a p95
of fourteen, a p99 of twenty-five and a longest run of fifty-three — against an original budget
of sixteen. The budget is now 256. A backoff was tried and is *not* there: it moved the mean from
5.0 to 4.5 and the p99 from 25 to 35. It made the tail worse.

## 6.6 Compaction

### The level structure

```
  L0   micro-batch files, arrival order, small
        │  merge many
  L1   sorted within file, medium
        │  merge several
  L2   sorted across the partition, full statistics, bloom filters where warranted
  SEALED — never rewritten again
```

Each byte is written once at each level, giving roughly **3× total write amplification instead of
two orders of magnitude**. When a partition's newest data falls behind a watermark it is
compacted once to the top level and *sealed*; a sealed partition is never rewritten. This bounds
total compaction work to a function of data volume rather than of data volume multiplied by
elapsed time.

The naive alternative is worth stating so the bound is legible: recompacting a whole large
partition every hour while it receives a small increment rewrites the entire partition per hour.

### A merge never deletes anything

The rule that makes frequent compaction safe: **a merge writes a new file and leaves its inputs
in place.** A reader holding a snapshot continues reading files that are still there. There is
no window in which a file under a reader disappears.

*Retirement* is a separate operation, with three preconditions that must all hold for a given
file:

1. **The replacement verifies.** Its row count is re-read from its footer at retirement time,
   not trusted from the merge — a merge may have completed hours earlier.
2. **No retained snapshot can resolve to the input.** Time travel and long sessions both pin a
   position; a file a pinned snapshot may reach is kept however old it is.
3. **The grace period has elapsed.** A reader that listed files a moment before the merge is
   entitled to open them and has no way to announce that it is doing so. The grace period must
   exceed the longest query the deployment permits.

An input failing any precondition is **retained with a reason**, which is a correct outcome
rather than a failure: retirement is an optimisation, and declining it costs only disk. The one
case that *is* an error is a missing or short replacement — that means the compaction did not
actually happen, and nothing may be removed at all.

Separating the two operations means the frequent, cheap one carries essentially no risk, and
the dangerous one runs rarely and under stricter conditions. The code split follows the
conceptual one: compaction policy and retirement live in different modules.

### What compaction is worth

Twenty million rows, 400 fragments, against the one file they merge into:

| Query | 400 files | 1 file | Ratio | Absolute overhead |
|---|---|---|---|---|
| Short — one narrow range | 16.9 ms | 3.8 ms | **4.42×** | 13.1 ms |
| Long — full aggregation | 121.0 ms | 97.3 ms | **1.24×** | 23.7 ms |
| On-disk size | 61.0 MB | 27.4 MB | **2.23×** | — |

Fragmentation is therefore an *interactive-latency* problem rather than a throughput one — which
is what makes it worth attention, since interactive latency is the thing anyone notices. The
absolute overhead stays in the same order (13 ms to 24 ms) while the ratio collapses, which is
the shape of a fixed cost.

An earlier run of the same benchmark used one million rows and produced 4.43× and 3.77×.
Apparently uniform — and it would have been read as *"more files are slower"*, which is true
and useless. **A measurement that cannot distinguish the hypothesis from its negation is not
evidence.** The twenty-million-row run separates a latency effect from a throughput effect;
the one-million-row run could not.

### Commit cadence, and metadata economics

$$\text{commit\_interval} = \mathrm{clamp}\left(\frac{\text{target\_landing\_file\_size}}{\text{observed\_ingest\_rate}},\ \text{floor},\ \text{ceiling}\right)$$

A table receiving a trickle commits rarely; a table receiving a torrent commits often. One rule,
no per-table tuning, and it directly prevents the pathology where a small table's metadata
exceeds its data.

The compounding argument is worth stating explicitly: **checkpoint size is proportional to live
file count**, so small-file compaction reduces metadata cost *quadratically* — fewer files makes
each checkpoint smaller *and* permits checkpointing less often.

### The compaction conflict is typed

Compaction that loses a commit race to the applier is a logical no-op over disjoint files and
should rebase and retry. A conflict where the *inputs themselves* were modified is fatal and must
reschedule. Distinguishing them in the type is what makes aggressive compaction safe — and the
safety rule above it is INV-2's: in any conflict between maintenance and the applier, maintenance
backs off.

## 6.7 File geometry, and two verified constraints

Physical layout decisions are made at write time and are expensive to undo, so they are
architecture rather than tuning: target file size, row-group size, the page index (enabled — it
is what makes intra-row-group pruning possible), compression, encoding (byte-stream-split for
floats), bloom filters (column-specific, never table-wide), and clustering applied by the
compactor rather than by the ingest path, because clustering requires a sort and ingest appends
unsorted.

Two constraints were verified and are the kind of finding that is lost if it is not written
down. **Multi-dimensional clustering in the Delta library has an open row-duplication defect
and must not be used until it is resolved.** And **enabling deletion vectors silently disables
predicate pushdown** — a second, independent reason to keep them off.

## 6.8 Caching is free, and one key is not

A cache keyed by object path requires no invalidation protocol. Entries never go stale; they
only become unreferenced.

Correctness is free; only eviction policy remains, and eviction policy is a performance
question. This is stated explicitly because engineers who have built caches over mutable stores
will otherwise design an invalidation protocol this system does not need. Five layers use it:
table metadata, the footer and page index, byte ranges, decoded batches, and results.

**One mutable key exists in the entire design**: the mapping from a table to its latest version.
Its time-to-live is bounded by the freshness objective. It is named explicitly because it is the
single place a stale cache produces a stale answer.

Two of the cache keys are security requirements rather than performance ones, and omitting
either is a breach mechanism. The **policy bundle version** must be in the plan-cache key —
without it, a revocation does not take effect for any query whose plan is already cached: data
served after it was forbidden, with a passing test suite. And the **evaluated entitlement set**
must be in the result-cache key, because a key that omits it does not return a *stale* answer,
it returns **someone else's**, correctly and quickly. The result cache is not built; its key is,
for exactly that reason.

Admission policy is second-access, so a single full scan cannot evict the working set, plus
unconditional admission for freshly compacted files, which are hot by definition. Content for
encrypted columns is cached as ciphertext.

## 6.9 Statistics, and the rule they live under

The file format and the table log both omit the statistic the optimiser most needs: **distinct-
value counts**, which drive join ordering. SANKHYA therefore maintains its own per-column,
per-partition statistics — a mergeable cardinality sketch, bounds, null fraction, average width
and a quantile sketch — refreshed at compaction, when the data has already been read and the
marginal cost is near zero. This is what makes join ordering work on arbitrary user schemas
where nobody has run an analysis command.

> **Key idea**
> **A statistic may make a query slower. It may never make a query wrong.**

Four asymmetries follow, and each is enforced:

- **Bounds may skip a file only when they prove nothing in it can match.** Anything uncertain —
  an absent bound, a type that does not line up, a comparison that cannot be made — means the
  file is read. A needless read costs time; a wrong skip costs an answer, and nothing downstream
  can detect it.
- **Unknown is not unbounded.** An absent bound means "may match anything"; filling it in with a
  default turns a missing statistic into a wrong one.
- **A merge may not narrow a bound.** Where both sides hold values and either lacks a bound, the
  merged bound is absent. This matters because compaction *merges* statistics rather than
  recomputing them, so a defect here appears only after maintenance has run, on data that was
  correct when it was written.
- **Distinct-value estimates never touch pruning.**

The safety property is property-tested directly: if the skip predicate returns true, no value in
the file satisfies the predicate. The converse is deliberately *not* asserted — an implementation
that never skipped anything would be slow and correct, and only one direction is a defect.

The cardinality sketch is HyperLogLog with 4,096 registers per column, merging by register-wise
maximum so a merged file's sketch equals the sketch of its inputs' union *exactly*; accuracy is
within 5% from 10 to 100,000 distinct values. The hash is fixed and process-independent, because
a seed that varied per process would make two nodes disagree about a plan — and that
disagreement would present as a bug in the optimiser rather than as what it is.

Computing statistics at compaction costs **83 ms of a 357 ms merge — 23%** over five million rows
across three columns, down from 164 ms (36%) before the kernels were vectorised. Bounds alone had
previously cost 29% of the merge in a scalar loop, five times what the cardinality sketch costs.

Two defects in this area are worth carrying because they are the class the rule exists to
prevent: a NaN once narrowed a bound *below* the true maximum, so a file holding 2.5 would be
skipped for `f > 0` because its statistics claimed it topped out at −1.5 — silently and
undetectably. And a reversed comparison shipped from the mutation audit's own catalogue: `5 < x`
read as `x < 5`, so the reader skipped files that did hold matching rows.

## 6.10 Sorting, and the objective that named the wrong preconditions

Choosing a sort order without domain knowledge follows a priority list: partition columns are
excluded; then the **commit position**, which is always present, always monotonic and *free*,
because data already arrives in that order — it gives perfect pruning for every as-of query,
which is the one predicate shape guaranteed to exist; then the primary key; then a
low-cardinality column prepended; then an observed key from the profiler.

Measured on TPC-H Q6 at scale factor 1, which selects one year in seven of the ship date:

| Layout | Single query | p95 at 8 clients |
|---|---|---|
| Generation order | 222 ms | 1,819 ms |
| Sorted by ship date | **31 ms** | **234 ms** |

That is 7.8× at concurrency, entirely from row groups skipped on their statistics before any
decoding begins.

> **Pitfall**
> The performance objective this satisfies names bloom filters and late materialisation as its
> preconditions. **Neither turned out to be the lever.** Sorting was the third thing, and it was
> the one that mattered — which is worth recording, because the objective's own list of
> preconditions would have sent someone to build the wrong two.

## 6.11 What is not built

| Not built | Note |
|---|---|
| Object-store backend | Everything published today goes to a local filesystem. M12. The conditional-put property it must have is already written down |
| Bloom filters | Off by design where they do not pay; not built where they would. They pay only when a value is likely *absent* from most row groups |
| Deletion vectors, column mapping | Deliberately absent; deletion vectors additionally disable predicate pushdown |
| Multi-part and V2 checkpoints, and log cleanup | Nothing deletes the commits a checkpoint subsumes, so the log directory grows without bound |
| The result cache | Its **key** exists, because key correctness is a security property and the right time to fix it is before anything caches |
| The persisted cardinality sketch | A column read back from the log reports zero distinct values |
| Partitioning on the streaming arrival path | The batch publish path is partitioned; the ingest path writes flat. Chapter 7 §7.9 |

One documentation note, since this book is assembled from the repository: the `table-delta`
crate's own header still lists checkpoints, partition values and statistics among the things it
"deliberately does not implement". All three are implemented. The header describes an earlier
state, and the architecture document and the invariants are the current record.
