# Runbook — the restore drill

**Trigger:** `sankhya-server doctor` reports `no restore drill has ever passed`, or that the
last passing drill was longer ago than the objective. Or a drill exited `1`.

## Symptom

Nothing is broken. Queries work, ingest works, the warehouse is fine. What is wrong is
epistemic: **nobody knows whether the backup would restore.**

That is why this is easy to leave. It never causes an incident until the day it causes the
only incident that cannot be recovered from.

## What is actually wrong

Depends which of three you have, and they are not the same problem.

### 1. No drill has ever passed

Either drills have never run, or every one of them has failed. **The diagnostic cannot tell
you which and deliberately does not guess** — it reports the last *pass*, not the last
attempt, because an operator asking "when did we last prove we could restore" must not be
answered with the time of a failure.

Read the evidence:

```bash
tail -20 <data-dir>/restore-drills.jsonl
```

An empty or missing file means no drill has run. Lines with `"verdict": "FAIL"` mean they
have run and the backup does not restore, which is a different and much more urgent
situation.

### 2. The last pass is older than the objective

Drills have stopped running. Check whatever schedules them — cron, a timer unit, an
orchestrator job. The commonest cause is that the schedule was never created, because
`sankhya-server backup` was run by hand once and the drill was assumed to follow.

### 3. A drill exited `1`

**The backup is not a backup.** Treat this as an incident, not a maintenance task.

## What to do

### If drills have never run

```bash
sankhya-server backup     # record a manifest
sankhya-server drill      # prove it
```

Then schedule it. Weekly is a reasonable default against the thirty-day objective:

```cron
23 3 * * 0  /usr/local/bin/sankhya-server drill
```

Exit `0` proven, `1` a table did not verify, `2` the drill could not run. **Alert on `2` as
well as `1`.** A monitor that treats "could not run" as "nothing wrong" reports a backup as
proven when nothing looked at it, which is the exact failure this whole mechanism exists to
prevent.

### If a drill is failing

The output names the table and what kind of failure it is, and the distinction matters:

| What it says | What it means |
|---|---|
| `expected N row(s) and found M` | Rows were **lost or duplicated**. Something wrote to a version that was supposed to be frozen, or a file was removed |
| `the row count matches and the data does not` | Rows were **altered**. Every file is present and the right length. This is the failure a file-presence check never reaches |
| `could not be read (…)` | The file is missing or corrupt. It names the file |
| `the manifest's own record of this table is unreadable` | The **manifest** is damaged, not the data. Different artefact, different investigation |

**Do not take a fresh backup to make the alert stop.** That is the natural move and it
destroys the evidence: a new manifest digests whatever is there now and passes, which
records the current state as correct without ever establishing whether it is. If the data was
altered, you have just certified the altered version.

Instead:

1. **Keep the failing manifest.** It is the record of what the data was supposed to be.
2. **Establish which side is wrong.** If the table reconciles against the transactional
   source, the warehouse copy is fine and the manifest's digest was computed over something
   else — look for a restore or a manual file operation between the two. If it does not
   reconcile, the warehouse copy is wrong and the backup is the good copy.
3. **Check whether files were retired underneath the backup.** Protection expires and then
   releases after a grace period; a backup past its horizon whose grace also lapsed has no
   claim on its files any more. `doctor` will not tell you this — the manifest will, in
   `protect_until`.

### If a drill cannot run at all

Exit `2`. The manifest could not be read, or the evidence could not be written. The second is
worth saying out loud: **a drill whose result was not recorded is indistinguishable from one
that never ran**, so it is reported as a failure to run rather than as a pass with a
footnote.

## What "fixed" looks like

A drill exits `0` and the evidence file has a `"verdict": "pass"` line newer than the
failures. Not "the alert stopped" — the alert stops when a drill passes, and a drill passes
when a fresh backup is taken, which is why the instruction above is not to do that.

## What this runbook cannot help with

The transactional half. This system binds itself to a PostgreSQL backup somebody else took;
it does not take one and does not verify one. The manifest records its location and digest,
and proving *that* artefact restores is a separate drill against your database backup tooling.
A SANKHYA drill passing means the analytical tier restores. It says nothing about the source.
