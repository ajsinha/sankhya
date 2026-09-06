# 15. Maintenance, tiering and the data lifecycle

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> This chapter covers everything that happens to data when nobody is looking at it: compaction,
> retirement, orphan sweeping, cuboid collection, quarantine expiry and the archival ladder that
> ends in a purge. Its central claim is that **removal is always a separate, later, weaker-privileged
> operation than the thing that made removal possible**, and that the gap between them is the
> feature. Compaction adds and retirement removes. Detach is reversible and drop is not. Archive,
> purge and drop are three gates and only the third is irreversible. Every one of those separations
> costs storage and buys a window in which a mistake is still a mistake.

---

## 15.1 One scheduler, and a ladder that may preempt a query

One scheduler covers **both** the transactional and the analytical sides. That is not tidiness:
both draw from the same machine budget and must be prioritised against each other. A freeze
emergency and a compaction backlog cannot be arbitrated by two independent schedulers, because
neither knows what the other is doing with the disk.

Class | Examples | Budget
---|---|---
**Safety** | Transaction-identifier freeze, slot-lag remediation | **May preempt queries**
**Availability** | Log and disk reclamation, emergency compaction | **May preempt queries**, audited
**Performance** | Compaction, delete merging, statistics | Within duty cycle
**Housekeeping** | Expiry, orphan cleanup, partition rotation, tiering | Within duty cycle, windows preferred
**Optional** | Re-clustering, cold view refresh | Windows only; first deferred

The preemption exception is deliberate and explicit: **a wraparound emergency or a full volume is
worse than a slow query**, and a scheduler that cannot express that will eventually make the wrong
call. Waiting never promotes a job out of its class — a housekeeping job that has been queued for a
day is still housekeeping — because a promotion-by-age rule is how an optional job comes to preempt
a query at month end.

Two properties make the scheduler safe to interrupt at any instant:

- **Jobs checkpoint at natural granularity and resume.** A job that can only run to completion will
  never complete on a busy system, so a job that cannot checkpoint is refused rather than started.
- **Every job is safe to run twice.** A job killed at any instant leaves no corruption — at worst
  unreferenced files, which the orphan cleaner reclaims after an age threshold exceeding the
  maximum possible commit duration.

> **Key idea** — Maintenance quality is the analytical latency budget, not a background nicety.
> Small-file accumulation and unmerged deletes are the two leading causes of slowness, and both are
> produced by the sync path itself. The system therefore contains a structural feedback loop —
> sync creates the mess, maintenance clears it, queries pay if maintenance falls behind — which is
> why compaction is a first-class subsystem with its own objectives rather than a cron job.

Maintenance is also the reason a multi-node deployment needs coordination at all. The query path is
genuinely stateless; *"who compacts this table"* is not answerable without a coordinator. That
election runs through the transactional store rather than a bespoke consensus implementation —
correct, small, and using infrastructure already present. It is **not built**; it needs a second
machine and moved to `M12` with the rest of scale-out.

## 15.2 Compaction adds; a separate operation removes

Lakehouse compaction has the write-amplification shape of a log-structured merge tree, and the
naive approach is catastrophic: recompacting a whole large partition every hour while it receives a
small increment rewrites the entire partition per hour.

```
  L0   micro-batch files, arrival order, small
        │  merge many
        ▼
  L1   sorted within file, medium
        │  merge several
        ▼
  L2   sorted across the partition, full statistics
        │
        ▼
  SEALED — never rewritten again
```

Each byte is written once at each level: roughly **3× total write amplification instead of two
orders of magnitude**. When a partition's newest data falls behind a watermark it is compacted once
to the top level and **sealed**, and a sealed partition is never rewritten. That bounds total
compaction work to a function of data volume rather than of data volume multiplied by elapsed time.

The rule that makes frequent compaction safe is that **a merge never deletes anything**. It writes
a new file and leaves its inputs in place, so a reader holding a snapshot continues reading files
that are still there. There is no window in which a file under a reader disappears.

Deleting the inputs is a distinct operation, and every one of three preconditions must hold for a
given file:

1. **The replacement verifies.** Its row count is re-read from its footer *at retirement time*, not
   trusted from the merge. A merge may have completed hours earlier.
2. **No retained snapshot can resolve to the input.** Time travel and long sessions both pin a
   position; a file a pinned snapshot may reach is kept however old it is.
3. **The grace period has elapsed.** A reader that listed files a moment before the merge is
   entitled to open them and has no way to announce that it is doing so. The grace must exceed the
   longest query the deployment permits.

An input failing any precondition is **retained with a reason**, which is a correct outcome rather
than a failure — retirement is an optimisation, and declining it costs only disk. The one case that
is an error is a missing or short replacement: that means the compaction did not happen, and
nothing may be removed at all.

> **Key idea** — A directory listing is not a file set. Between a merge and the retirement of its
> inputs the directory holds **both** — the new file and the files it replaced, the same rows twice
> — for at least a full grace period, by design. A planner given a listing plans a merge over
> already-superseded inputs and duplicates those rows permanently; a reader given a listing
> double-counts every merged row for the window's duration. This is the concrete reason the system
> needs a table log rather than merely liking the idea: *which files are live* is not answerable
> from the filesystem once compaction has run.

### What it is worth, measured

Small files cost query **planning** — listing, footer reads, metadata resolution — rather than
scanning. That predicts a roughly *fixed* penalty per query, which should dominate short queries
and amortise away on long ones. Measured over 20,000,000 rows, 400 fragments against the single
file they merge into:

Query | 400 files | 1 file | Ratio | Absolute overhead
---|---|---|---|---
Short — one narrow range | 16.9 ms | 3.8 ms | **4.42×** | 13.1 ms
Long — full aggregation | 121.0 ms | 97.3 ms | **1.24×** | 23.7 ms

The prediction holds: the overhead stays within the same order across a query doing thirty times
more work while the *ratio* collapses. Fragmentation is an interactive-latency problem, not a
throughput one — which is what makes it worth attention, since interactive latency is what anyone
notices. Merging also reduced the data by **2.23×**, largely through better compression across a
larger block.

That measurement needed a larger fixture before it meant anything. An earlier run over 1,000,000
rows showed 4.43× and 3.77× — apparently uniform, and it would have been read as *"more files are
slower"*. The long query simply was not long enough for planning to amortise against. **A
measurement that cannot distinguish the hypothesis from its negation is not evidence**, a theme
Chapter 23, *How this is tested*, returns to repeatedly.

## 15.3 Maintenance runs inside the server, and the settings that govern it

Since `M8` the server maintains its own warehouse on its own thread. The warehouse therefore moves
whether or not anybody is writing to it, which is visible on disk — the review warehouse this book
was written against holds:

```
<warehouse>/common/orders/_delta_log/00000000000000000005.json
<warehouse>/common/orders/sank_data_date=2026-09-02/compacted-000001-0000.parquet
```

One file, named by the merge that produced it, under the date partition Chapter 7, *The date axis*,
requires. The settings are three numbers and their defaults are argued rather than picked:

```yaml
maintenance:
  interval: 30s          # 0 disables maintenance entirely
  compact_every: 1       # ticks between compaction passes
  orphan_sweep_every: 120
```

`interval` is the rate the warehouse catches up at, not how long a pass spends — a pass is bounded
work. Setting it to `0` is for exactly one honest case: another process is doing it. **Two
maintainers on one warehouse are two committers racing for the same version.**

`compact_every: 1` because a pass is already bounded by `max_files_per_pass`, and a partition below
the merge thresholds costs only the decision not to merge it. Raising it trades promptness for
quiet; set it too high and small files accumulate faster than they are merged, which shows up as
`sankhya_table_live_files` climbing rather than sawtoothing (Chapter 14, *Observability*).

`orphan_sweep_every: 120` — an hour at the default interval — because a sweep walks the whole table
directory, returns nothing most of the time, and what it collects is not urgent. An orphan appears
only when a merge writes its output and then loses the race to commit it, and the age policy will
not release one until it is a week old in any case.

> **Pitfall** — Wiring maintenance into the server introduced a defect that neither decision
> contained alone. A server resolved its table providers **once**, at start; that was sound while a
> served warehouse did not move. Once maintenance ran in-process, compaction replaced files and
> retirement deleted the ones it replaced, so a provider fixed at boot named files that were gone,
> and queries failed with a missing-file error naming a path nobody asked about. Retirement's grace
> period is not the protection here: it protects a reader that listed shortly before a merge, not
> one that listed at startup and has been serving from that listing since. With shipping defaults a
> deployment would have begun failing queries about twelve minutes in. The fix is to re-resolve
> before registering, and it had a side effect worth having — a running server now sees data
> committed after it started, which it never did before.

That re-resolution covers a table whose log has *advanced*. It does not yet cover a table that has
been **removed**. Verified on the running server: a table present at boot and dropped afterwards
loses its directory from the warehouse and keeps its place in the served set — still listed by
`information_schema.tables`, still exporting a live-files gauge, and, for a clone, still answering
`SELECT count(*)` with 250 rows spliced from an origin that is still there. The clone records
themselves are correct; `SHOW LINEAGE OF` that table refuses. A table created *and* dropped inside one
server's lifetime disappears properly, so the gap is in what boot registers rather than in the drop.
Chapter 14, *Observability*, §14.7 records the transcript.

## 15.4 Three lifetimes for a cube, and the collection each one needs

A cube's storage is governed by the same add-then-remove discipline. Chapter 10,
*Multidimensional analysis*, defines the model; what belongs here is who pays for it and who
reclaims it.

| | Persisted | Materialised | Maintained by | Ends when
---|---|---|---|---
**Ephemeral** | no | no | nothing | the session ends
**Declared** | yes | no | nothing | it is dropped
**Maintained** | yes | yes | the warehouse | it is dropped

`target_lag` on a Maintained cube is a **staleness target, not a schedule**. `target_lag = 5` means
*the cells may be at most five commits behind*, not *rebuild every five commits*. A schedule
rebuilds when nothing has changed and fails to rebuild when a build takes longer than its interval;
a target says what you actually want. Staleness here is exact rather than estimated, because a
stored cuboid records the version it was computed at, and **a cuboid past its target is never served
as though it were fresh** — the answer falls back to live aggregation, which is slower and right,
and says `materialised = false`.

Three reclamation paths, and they are not interchangeable:

- **Superseded cuboids** are collected. A cuboid at an old snapshot can never be selected, so it is
  garbage the moment the table advances. This fell between the two existing mechanisms — the orphan
  sweep finds unreferenced files *within* a table, and a stale cuboid is a whole table no log
  mentions — which is the same shape as the defect that once filled a disk in the soak.
- **The ordinary cuboid sweep deliberately keeps** anything belonging to a cube whose current
  version it cannot find. Deleting on a guess is how a cache becomes a data loss.
- **`DROP CUBE` therefore reclaims every cuboid the cube materialised**, and it is the only moment
  at which that storage can be released. Nothing else will ever reclaim it.

That is also why there is no `CREATE OR REPLACE CUBE`, verified on the running server:

```
psql> CREATE CUBE probe_e_sales FROM "probe_e.scratch" …;
ERROR:  the cube `probe_e_sales` already exists. Drop it first: replacing a cube retires every
        cuboid it materialised, which is not something a re-run of a script should do silently
```

**Ephemeral is the intended default and is not what `CREATE CUBE` does today.** A plain `CREATE
CUBE` persists a definition under the warehouse's `_cubes/`, visible to every other connection.
There is no syntax yet for asking for an ephemeral one, so on a shared server a reader exploring
publishes their exploration to everybody. `M14` builds the ephemeral lifetime with the **mandatory
expiry** §15.6 explains; until then, drop what you declare.

## 15.5 Quarantine expiry, on the ingest side

A declared feed quarantines a record that does not fit: written whole, exactly as it arrived, into
`sank.sank_quarantine` — a table, not a directory of rejected files — alongside the reason, a stable
code, the position it arrived at, and a fingerprint of the declaration that refused it. Whole,
because a record reduced to an error message cannot be replayed, and replay is the only actual
remedy. Chapter 9, *Capture and ingest*, covers the refusals themselves.

The quarantine carries a **mandatory retention**, defaulting to thirty days, and a retention of zero
is not expressible. The lifecycle question is *how* it expires, and the answer is the one this
chapter keeps giving: **partition detach, never row deletion.** `DEC-23` gets no exception here, and
a detach stays reversible until retirement's grace period runs.

The maintenance tick calls the expiry job. On a deployment with no feeds declared — the review
server, for instance — the table exists, is empty, and `SHOW FEEDS` returns no rows, which is the
correct answer rather than an error: *none declared* and *this server does not do feeds* are
opposite facts with opposite responses.

## 15.6 Tiering: three gates, and only one is irreversible

Everywhere else in this system the published tier is *derived*: if it is wrong, rebuild it from the
source. That safety net is what makes capture defects survivable. Tiering removes it — once a
partition is purged, the published copy is the only copy, and any defect in it is permanent and
undetectable after the fact.

> **Key idea — the prime directive.** Data may not be removed from the system of record until its
> replacement is proven durable, complete, byte-faithful, immutable and covered by the applicable
> retention obligation. The proof is machine-checked, recorded, and **there is no flag to skip it.**

Gate | Effect | Reversible
---|---|---
**Archive** | Copy, verify, tag. Nothing is removed | Fully — a no-op on the source
**Purge** | Detach. Data leaves the live table but remains on disk | Trivially — re-attach
**Drop** | Remove from quarantine | **Never**

> **The recommended production configuration is: schedule enabled, stop at Archive, purge performed
> deliberately by a human a few times a year under dual control.** That delivers continuous
> automatic proof that the published copy is complete and correct — the valuable half — while
> keeping the irreversible half rare and considered. A deployment that never advances past Archive
> still gets most of the benefit at none of the risk.

### Eligibility, and the trap the capture path sets

A table is tiering-eligible only if it is **append-only by contract** and **range-partitioned on the
tiering key**. There is a convergence worth noting: partitioning is independently required on
high-volume time-shaped tables to make retention a metadata operation rather than a bulk delete
generating enormous bloat. The same schema decision serves both purposes, both are made at design
time, and both are expensive to retrofit.

The capture path replicates deletes. An archival purge implemented as a row deletion would propagate
and **erase from the published tier exactly the data the purge existed to preserve** — quietly. Four
independent layers stand against it:

Layer | Mechanism | Property
---|---|---
**Primitive** | Purge is partition detach then drop; row deletion is never used | The purge *cannot* emit a delete event, because it deletes no rows
**Publication guard** | Tiering-eligible tables exclude delete and truncate from their publication | Even a defective code path cannot propagate a deletion
**Applier tripwire** | The applier holds the archival extent map and treats any delete in an archived range as a **fatal alarm** | Catches a mis-scoped publication or a hand-made slot
**Attestation** | A transactional marker committed with the registry change | Provenance and ordering. **Observability, never safety**

Two plausible alternatives were evaluated and rejected, and both rejections are recorded so nobody
re-proposes them. **Marker-bracketed suppression** — emit deletes, have the applier suppress them —
fails if a marker is lost, reordered, or the applier restarts mid-bracket; *never make a safety
property depend on a message arriving*. **A session-level replication role** does not work at all:
it disables triggers and rules and has no effect on logical decoding, which reads the write-ahead
log directly.

### The gated state machine

```
Proposed → Frozen → Replicated → Verified → Durable → Sealed
         → Detaching → Detached → Quarantined → Dropped → Complete
                     ↘ NeedsAttention  (terminal until an operator acts)
```

Every transition is committed **before** the corresponding real-world action, and every phase is
idempotent and resumable *including within a phase*, so a crash late in a long verification does not
restart it.

**Verification is exhaustive, not sampled**: row count, primary-key set equality via a digest over
sorted blocks, and per-column checksums over a canonical byte encoding. Count equality alone is not
evidence. Routine reconciliation may sample; purge verification may not. The canonical encoding
carries a **lossless-or-reject rule** — types that cannot round-trip faithfully make a table
ineligible, checked at *policy creation* rather than at purge time, because discovering at purge
time that a column cannot round-trip is discovering it too late.

**Verification failure is terminal until a human acts.** There is no automatic retry, because failure
means a defect exists and retrying is the wrong response.

### Quarantine, and the second refusal the reaper makes

The detached partition is retained for a grace period — seven days by default — during which
re-attachment is trivial. `Grace::of(0)` is a refusal rather than a value: a grace of nothing is the
requirement not being implemented rather than being configured, and making it unrepresentable costs
one constructor and removes the setting somebody reaches for when a disk is full at four in the
morning.

What quarantine insures against is precisely what verification cannot catch. Verification proves the
archive matches the source at the moment of the copy. It cannot prove the *policy* was right — that
the range was the one somebody meant, that the tiering key meant what its author thought, that a
timezone did not move a year's boundary. Those are found days later by a person, and the only thing
that helps then is the partition still being on disk.

The reaper refuses two things. The first is obvious: a partition inside its grace period. The second
is the one worth having: **if the registry no longer claims the range** — a restore that lost the
entry, an entry withdrawn by hand — **the quarantined copy is the only copy**, and reaping it on age
would be the permanent loss quarantine exists to prevent, performed by the machinery meant to
prevent it. Ten thousand days does not make it safe.

Re-attachment is one call because it is two invariants. Re-attaching without withdrawing the
archival entry leaves a range the registry claims and the catalog has attached — a disagreement.
Withdrawing without re-attaching leaves the range in neither tier — a coverage gap. There is no
order to get wrong because there are not two calls.

### Structural prevention of an accidental purge

The state machine's entry point requires an authorization value whose **only two constructors** are
the command path and the schedule evaluator. No maintenance job can synthesise one. Consequently,
enumerating the constructors of that type is a **complete audit of every way data can leave the
system of record** — a review procedure that takes seconds and cannot be circumvented by adding a
caller.

The planning command is *always* a dry run, and *always* is doing the work: a `--dry-run` flag
defaulting to true is one argument away from not being one. So planning returns a proposal with no
method that does anything, and the acting path cannot be reached without a digest that only planning
can produce — taken over the cluster, the policy, the table and every range in order, so an approval
for `[0, 200)` cannot authorise `[0, 300)` by editing a command line.

## 15.7 After a purge: coverage, corrections and rehydration

Queries spanning hot and archived ranges are unioned automatically, under an authority rule that
eliminates a class of drift defects: **the source catalog is authoritative for whether data is still
hot; the archival registry is authoritative for provenance and the cold side.** Both are read within
the same source snapshot used for the hot scan — the registry lives in the same database, so this is
free.

The tie-break rule is **total**. An uncovered range intersecting the predicate fails with a typed
error (`SNK-S0001`) rather than answering short. A range the registry believes cold but the catalog
shows attached — a restored backup resurrecting purged rows — is read once from the source, so there
is no double counting even in the failure case, while the inconsistency is separately flagged
(`SNK-S0002`) and unified queries on that table are refused until an operator resolves it.

> **Pitfall** — Zero rows affected is a silent wrong answer. A tiering operation that finds nothing
> to do and reports success is indistinguishable from one whose range was mistyped. Every path here
> that could return *nothing matched* names the range instead.

**Corrections default to a compensating entry in the hot tier** referencing the original. This is how
record-keeping already works: a posted entry is reversed, not erased. It preserves the audit trail
completely, requires no rewrite, and is available whatever the archive's immutability controls say.
Controlled rewrite exists for the cases that need it and cannot be constructed without retaining the
prior version and recording an amendment link — a rewrite that keeps no prior version is
indistinguishable from the archive having always said the new thing, which is the property archives
exist to have.

**Rehydration carries a mandatory expiry**, and that is the load-bearing property rather than the
read-only flag. `RSK-35` is *"rehydrated copies accumulate into a shadow system of record"*, and that
failure **has no moment**. Nobody rehydrates a shadow system of record; they rehydrate one range for
one investigation, and then another, over a multi-year horizon, each individually reasonable. There
is no day on which somebody could have decided otherwise — which is exactly why it cannot be a
decision made per rehydration. An expiry of zero is unrepresentable and one longer than ninety days
is refused: not a safety property, since a person can rehydrate again, but a bound on how far a
single decision reaches.

Four properties are types rather than checks, because each is the sort of thing a review confirms on
the day and nothing enforces afterwards:

Property | How
---|---
Excluded from every publication | The constructor refuses a schema not asserted excluded
Never attached to the live parent | The same constructor refuses the parent's own schema
Read-only | There is no method that writes and no mode that is not `ReadOnly`
Mandatory expiry | There is no constructor without one

**A migrated table keeps its name.** After migrating a table whole to the published tier it remains
visible in the catalog under the same name, marked cold and read-only, because a table that vanishes
breaks every downstream tool and saved query — a table nobody has written to for four years is still
named in dashboards, in a quarterly report, and in a query somebody pastes from a wiki page. That
creates its own trap, since a visible table whose archive covers four of its five years answers four
years of questions without mentioning the fifth, so migration asks the registry to cover the table's
whole declared key domain and refuses with every hole.

## 15.8 What is built, and what is gated

`M9`'s eleven work items are built and its exit criteria were demonstrated on 2026-08-31: purge end
to end with verification, quarantine and rollback; the anomaly guard halting an intentionally
defective policy; nineteen refusal paths shown to fail closed.

**The gate is not cleared, and that is not a formality.** One criterion needs the archive attestation
drill run against a real non-production archive, which cannot be produced from development. And a
separate decision — `M11`, production reconciliation — holds the *arming* of destructive purge.
**Building the purge path and arming it are two decisions**, and `sankhya-tiering` remains on the
workspace's `UNREACHED` list with that milestone named against it: a crate nothing reaches is a claim
the repository does not keep, and listing it with a reason is how that claim stays honest.

The attestation drill itself is built and runnable, and it works by **trying to break the archive**:
it writes a probe object, then attempts to overwrite, delete and truncate it, and requires every one
to be refused. Reading a configuration flag would pass in exactly the case this exists to catch — a
retention policy that still reports `enabled` and no longer applies. Which is why it refuses to run
against a store not declared non-production, verified here:

```
$ sankhya-server attest <path>
SANKHYA archive attestation 0.1.0

NOT ATTEMPTED. this store is not declared non-production. An attestation attempts the violations it
is checking for, so against real data a missing control means the drill itself inflicts the damage
```

The marker lives *in the archive* rather than on the command line on purpose: a `--non-production`
flag survives in a runbook that gets copied, and the copy eventually runs somewhere it should not.
Chapter 16, *Backup, restore and disaster*, covers why exit `2` — *nothing was attempted* — must be
alerted on rather than read as a pass.

Beyond `M9`, one milestone is named and unbuilt. **`M19`, the data lifecycle policy**, is one
declaration governing how data ages across *both* tiers, and the reframe that makes it safe is the
one this chapter has been making throughout: nothing moves. Capture already published it, so ageing
rows out of the transactional store is a **release** gated on reconciliation's proof that the
analytical copy exists. Detach, never delete; reversible for a grace period; and a read of released
data is **refused by name** rather than answered short — which is the failure nearly every product
ships.
