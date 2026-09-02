# 16. Backup, restore and disaster

> This chapter specifies what SANKHYA promises about surviving a loss, and its central claim is
> that **a backup is a claim about a restore, so the only evidence that establishes it is a
> restore.** A manifest binds two positions rather than one, refuses to exist rather than record a
> disagreement between them, and refuses a clone whose origin it does not also contain. A drill
> reads the data back and recomputes its digest, because a file-presence check passes on nearly
> every failure that actually happens. The evidence keeps its failures. The chapter closes with
> what is *not* guaranteed, including a durability gap that no document in this repository had
> previously stated.

---

## 16.1 A backup is a manifest, not an archive

This system does not copy your data somewhere. It binds three artefacts to one point and protects
the files so they stay readable:

- the transactional backup **you** took, by its location and digest;
- the table versions in the warehouse;
- the key generation.

Three artefacts, because three backups that do not agree with each other are worse than one. Three
that agree restore a system; three that do not restore a puzzle, with nothing to say which is the
one to trust.

Taken against the review warehouse this book was written from:

```
$ sankhya-server backup
SANKHYA backup 0.1.0
  common.empty at version 0, 0 row(s)
  common.orders at version 5, 1000 row(s)
  common.regions at version 1, 250 row(s)
  probe_a.orders at version 3, 500 row(s)
  …
  sank.sank_quarantine at version 0, 0 row(s)

  backup:01a05fd9-53ee-7723-a129-85da1719e9bd
  queryable at 0
  manifest .../backup-manifest.json

This backup is unproven until it has been drilled: `sankhya-server drill`.
```

That last line is not decoration. Until a drill has passed, the manifest is a list of filenames
somebody wrote down.

## 16.2 There are two positions, and recording one has recorded the wrong one

`source_restores_to` is where the transactional store lands. `queryable_at` is the highest position
at which **every** table is complete — the minimum over their coverage, because a query joining two
tables can only be answered where both of them reach.

They are rarely equal. Tables publish at their own cadence, so at any instant some are further
behind than others and the transactional store is ahead of all of them. A manifest recording one
number and calling it *"the consistent point"* has recorded whichever of the two its author happened
to think of, and the difference between them is not noise: it is **how much re-capture a restore
implies** before a cross-table query can reach the source's position.

A backup binds to the second. Recording only the first is the commonest way this goes wrong.

> **Key idea** — The rule enforced when the manifest is **built**: no table may cover a position
> past where the source restores to. If one does, then after a restore the analytical tier holds
> rows the transactional store no longer has. Capture resumes behind them and republishes that
> range at different positions, so those rows arrive a second time under different identity — or
> sit there permanently as data with no origin. It is `SNK-S0002`'s shape one layer up, and it is
> **not detectable afterwards from either side alone**.

Which is why it is checked at build rather than at restore. A manifest that records an
inconsistency has recorded a broken backup *as a backup*, and the moment to discover that is not
the moment you need it:

```
refusing to record a backup whose analytical tier is ahead of its source. After restoring it,
1 table(s) would hold rows the transactional store no longer has; capture would resume behind them
and republish that range at different positions. Not detectable afterwards from either side alone:
sales.items covers to 900 and the source restores to 800
```

Every offending table is named, not just the first. Fixing them one at a time means learning about
the next only after another full backup, and an operator who takes two full backups to find two
omissions has paid twice for one mistake.

## 16.3 A backup of a clone alone is incomplete by construction

Zero-copy cloning (Chapter 12, *Cloning and lineage*) creates a table whose own log names **none** of
its origin's files: it reads the origin's live set at a version and splices its own log over it.
Restore such a table without its origin and you get a table that is present, readable and **empty**.
Nothing about it looks broken.

So each table snapshot records what it is a clone of, and the manifest **refuses** a backup
containing a clone whose origin it does not contain — at bind time, beside the position check, and
for the same reason. A chain holds without the check knowing it is a chain: each link is verified
against the backup's own contents, so `a → b → c` binds when all three are present and is refused
when the **middle** is absent. That the root is there does not help, because `a` splices `b`'s live
set rather than `c`'s.

The lineage is *recorded* rather than derived at restore, because at restore the origin may be the
thing that is missing, and a manifest that cannot say what it needed is a manifest that cannot say
what went wrong.

> **Pitfall** — A clone's own row count in a manifest is genuinely zero, and the drill genuinely
> verifies zero rows for it. In the run above, two clones of a 250-row table appear as
> `at version 0, 0 row(s)` and drill as `verified, 0 row(s)`. That is correct — their rows are
> covered by the origin's entry — and it reads like a warning. If you are eyeballing a manifest,
> read a clone's line as *"nothing of its own"*, not as *"nothing at all"*.

## 16.4 A drill reads the data back

`FR-OPS-15` is unusually blunt: *"an untested backup is a rumour."* The reason the verification must
read data rather than list files is that **a file-presence check passes on a truncated Parquet.** It
passes on a file whose bytes were replaced with another table's. It passes on essentially every
failure that actually occurs, because what goes wrong with a backup is almost never that a file is
missing: a missing file is loud, and something notices. What goes wrong is that a file is there and
wrong.

So the drill recomputes the digest recorded at backup time. It is expensive, it runs on a schedule
rather than on a request, and it is the only version of this that establishes anything:

```
$ sankhya-server drill
SANKHYA restore drill 0.1.0
  backup:01a05fd9-53ee-7723-a129-85da1719e9bd
  common.empty: verified, 0 row(s)
  common.orders: verified, 1000 row(s)
  common.regions: verified, 250 row(s)
  …
Proven. 12 table(s) read back and digested.
```

**Both sides compute that digest through one implementation.** Two would eventually differ on a null
convention, a value rendering or a column order; every drill would then fail on data that is
perfectly fine; and after the third false alarm the drills would stop being run. A verification that
cries wolf is worse than none, because it consumes the attention a real failure needs.

**A failure names its kind**, because the two need different investigations:

Symptom | Meaning | Where to look
---|---|---
`expected 1000 row(s) and found 940` | Rows were **lost or duplicated** | Retention, or a restore
`the row count matches at 1000 and the data does not` | Rows were **altered** — every file present, right length, wrong contents | A writer that touched a frozen version

Exit statuses, and the third is the one people get wrong:

Exit | Meaning
---|---
`0` | Proven
`1` | A table did not verify
`2` | The drill could not run

**Alert on `2` as well as `1`.** A monitor treating *"could not run"* as *"nothing wrong"* reports a
backup as proven when nothing examined it.

## 16.5 Evidence that omits failures is not evidence

The drill record is append-only, at `<data-dir>/restore-drills.jsonl`, and a failure is written with
the same ceremony as a pass:

```
{"at": 1787851608943991, "backup": "backup:01a0…", "verdict": "pass", "tables": 1}
{"at": 1787851616975478, "backup": "backup:01a0…", "verdict": "FAIL", "tables": 1,
 "failures": "sales.orders: could not be read (…part-0000.parquet: Parquet file too small)"}
```

A history with no failures across three years describes either a very good system or a drill that
does not really run, and nothing in the history distinguishes them. For the same reason,
*"could not start"* is recorded distinctly from *"ran and passed"* — the identical distinction the
diagnostic draws in Chapter 14, *Observability*, and the identical failure if they are merged.

An operator asking *when did we last prove we could restore* is answered with the last **pass**,
never the last attempt. And an operator who has never run one is told so at `critical`, on the first
run, with no history required — verified in Chapter 14 §14.6:

```
  [critical] backup — no restore drill has ever passed; this threshold has already been crossed.
```

## 16.6 Expiry and removal are separate, and the gap is the point

Deleting a backup does **not** release the snapshots it protects. A grace period of seven days
follows, and only then are the files sweepable.

The failure this prevents is specific and unrecoverable: a backup deleted by mistake — by an
operator clearing space, by a retention rule, by a script with the wrong argument — its files swept
by the next pass, and no way back even if the manifest is restored from somewhere minutes later.
`FR-STORE-21` makes the same trade for compaction (Chapter 15, *Maintenance, tiering and the data
lifecycle*, §15.2), and for the same reason. It costs storage that could have been reclaimed sooner
and buys a window in which a mistake is still a mistake.

Seven days is chosen as the span over which this kind of mistake is actually caught — long enough
that somebody returning from a week away still finds it recoverable.

## 16.7 Attestation proves a different claim

A drill proves you can read data back. An **attestation** proves a write-once store still refuses to
change what it holds, which is a different claim and one that decays *without anything touching your
system*. A retention policy is replaced, a lifecycle rule is added, a bucket is recreated by a
template, and the control is gone while every configuration readout still says it is there.

**It works by trying to break the archive**: writes a probe object, then attempts to overwrite,
delete and truncate it, and requires every one to be refused. Reading a configuration flag instead
would pass in exactly the case this exists to catch. Which is why it refuses without a
`_non_production` marker *inside the archive* — verified:

```
$ sankhya-server attest <path>
SANKHYA archive attestation 0.1.0

NOT ATTEMPTED. this store is not declared non-production. An attestation attempts the violations it
is checking for, so against real data a missing control means the drill itself inflicts the damage
```

Exit | Meaning
---|---
`0` | Attested — every violation refused, object unchanged
`1` | The store allowed something it must refuse
`2` | Nothing was attempted

**`2` is not a pass.** A write that failed because the path was wrong or credentials were missing has
demonstrated nothing about immutability, and recording it as a refusal would let a broken drill
certify a store it never touched.

## 16.8 Shutdown, and the two numbers nobody relates

A restore is not the only way to lose work. The commonest way is a deploy.

The drain order is a correctness property and is specified normatively:

1. Report not-ready; wait for load balancers to stop sending work. **Liveness stays healthy.**
2. Stop accepting new queries; let in-flight queries run to their deadline, then cancel with a typed
   error.
3. Stop the capture source, but **finish applying the in-flight batch**. A partial batch is rolled
   back entirely, never half-committed.
4. **Persist the applied position strictly after the commit is durable.** This ordering *is* the
   exactly-once guarantee.
5. Flush and close writers; release leases.
6. Drop graph epochs — derived state never blocks shutdown.
7. Stop the database gracefully; verify exit.
8. Flush telemetry. An unflushed exporter loses the traces of the incident being debugged.

A second termination signal escalates to abort **and logs exactly what was abandoned**. Termination
by force must always be safe; crash consistency is the real requirement.

> **Key idea** — Two numbers decide whether a shutdown is orderly and they live apart: how long the
> server needs to finish work already in flight, and how long the orchestrator will wait before
> `SIGKILL`. Nothing normally relates them. They are edited by different people, in different files,
> for different reasons — and when the second is the shorter, **every deploy severs connections
> mid-result and clients see something indistinguishable from a crash.** So the relationship is
> checked mechanically: `cargo xtask check-package` reads the drain deadline out of the source and
> compares it against every deployment manifest's grace.

The drain itself must be bounded and must exist. An unbounded drain hangs a shutdown on one stuck
client until the orchestrator's patience runs out and kills the process anyway, with the difference
that nobody chose the moment. And a shutdown that does not wait at all cannot be given a correct
grace, because there is nothing to wait for — it abandons work instantly, which reads as fast and is
the failure the grace exists to prevent. That was the state of this server until `M6`: `serve_until`
returned the moment shutdown resolved, its connection tasks were detached, and the doc comment above
it described the behaviour it did not have. A client mid-result saw a reset on every deploy.

## 16.9 The failure model, for the rows that concern durability

Failure | Behaviour
---|---
Applier crash mid-batch | Batch rolled back; resume from the last durable position; idempotent replay
Slot invalidated | Gap marker recorded; automatic re-snapshot; stale data served with **explicit provenance**
Incompatible schema change | Table quarantined; last consistent version remains queryable; events dead-lettered so the cursor still advances
Compaction interrupted | Resumes from checkpoint; at worst unreferenced files, reclaimed after an age threshold
Compaction conflicts with the applier | Compaction rebases and retries; **the applier never backs off**
Storage lacks conditional write | Detected at startup; multi-writer mode **refused**
Restored backup resurrects purged rows | Hot extent wins; read once, no double counting; inconsistency flagged; unified queries on that table refused until resolved
Node loss (executor) | Transparent; stateless
Node loss (coordinator) | Election; database failover — **not built**, `M12`

The storage requirement in that table is worth stating on its own, because it eliminates entire
deployment targets. A commit claims its version with `link(2)`, which fails when the name is taken —
**that refusal is the whole of the protocol's concurrency control**, and `rename` cannot provide it
because it replaces its destination silently. ext4, xfs, btrfs, zfs, APFS and NTFS all support hard
links; **FAT and exFAT do not and are not supported.** Some network filesystems implement `link`
unreliably, and the failure there is at least loud: the commit reports an error rather than losing an
update nobody is told about. On object stores the equivalent primitive is a conditional put —
`If-None-Match: *` on S3 and Azure, `ifGenerationMatch=0` on GCS — and a store that does not offer
one cannot host a warehouse safely.

That control was not always what it claimed. Until `M8`, `commit` claimed a version by checking the
file was absent and then renaming a staging file over it — and `rename(2)` replaces its destination
silently, so two committers could both see the version free and the second would overwrite the first
with no error to either. The rebase loop never ran, because the conflict it waits for was never
returned. It went unseen because every test had a single writer per version. Chapter 23, *How this is
tested*, treats the general lesson; the specific one is that **a safety property that has never been
contended is a safety property nobody has tested.**

## 16.10 What is guaranteed, and what is not

Stated as a list, because a durability chapter whose exclusions are implied is not a durability
chapter.

**Not guaranteed: durability across power loss.** There is **no `fsync` in the write path.** Nothing
in the workspace calls `fsync`, `sync_all` or `sync_data`. Commits are made atomic *with respect to
other writers and to readers* by `link(2)` and by staging-then-rename, and that is a different
property from being on the platter. A machine that loses power may lose recently committed versions
and recently written Parquet, and the log may be readable while a file it names is not. This is not
recorded in `ARCHITECTURE.md`, `STATUS.md` or any ADR, and it is stated here rather than left to be
discovered.

**Not covered: the transactional half.** This system binds itself to a PostgreSQL backup somebody
else took. It records the location and digest; it does not take one and does not verify one. **A
passing drill means the analytical tier restores and says nothing about the source.** Proving that is
a separate drill against your own database backup tooling, and it is deliberately not planned as this
system's job.

**Not measured: recovery objectives.** `M8`'s criterion 7 asks for *"recovery objectives measured and
published rather than estimated"*, and it moved whole to `M12` on 2026-08-30 for a reason that is a
machine rather than an estimate. An objective measured on one box silently excludes network
detection, machine loss and clock skew. Publishing it would be the same species of claim as a
contention threshold set below the contended figure — which this repository has shipped twice, and
which the concurrency criteria exist to have stopped doing. Cross-region replication is not
measurable here by definition.

**Not built: leader election, failover, replication, attached multi-node.** All of it is `M12`, and
all of it needs a second machine.

**Not run: the multi-day soak.** The harness exists, is proven to detect a leak, and runs short on
every build. A forty-five-minute judged run at twenty gigabytes passes with resident memory flat —
1.16 billion rows scanned, 21.5 GB reclaimed — and a fifty-nine-minute run with a cube served from
storage closed at 2.0 GB resident against the previous run's 2.2 GB. The scheduled multi-day run is a
change of duration and scale and moved to `M12`.

> **Key idea** — Every exclusion above shares a shape: the property is either *not measurable on one
> machine* or *not this system's to promise*. Neither is a reason to omit it. A backup chapter that
> lists only what it does is a chapter whose reader will discover the rest during an incident.
