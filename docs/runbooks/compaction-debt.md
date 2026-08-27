# Runbook — compaction debt

**Alert:** `sankhya_table_live_files` for one table is approaching or past 1,000.
**Lead time:** days, at ordinary write rates.

## Symptom

Queries against one table get slower, and only that table. Nothing else on the box looks
different: CPU is unremarkable, the disk is not full, other tables answer normally. The
slowdown is gradual rather than sudden, which is why it usually gets noticed as "the
dashboard feels sluggish lately" rather than as an incident.

## What is actually wrong

A scan pays **per file** — opening it, reading its footer, deciding from the recorded
statistics whether it can be skipped. That cost is paid whether or not a single row is read.
A table of a thousand small files and a table of ten large ones holding identical data cost
about the same to *read* and very different amounts to *plan*, and the small-file table is
the one that hurts.

Files accumulate because capture commits what it has when its commit interval elapses. That
is correct behaviour — a longer interval would mean staler data — and it means file count
rises continuously and is brought back down only by compaction.

So this alert almost never means "compaction is broken". It usually means **the maintenance
duty cycle is too low for this table's write rate.** Compaction is running; it is not running
often enough to keep up with this particular table.

## What to do

**Now, to relieve it:**

```bash
sankhya maintenance compact --table <schema>.<table>
```

Compaction is safe to run against a live table. It commits a new file set and retires the old
files only once no reader can still hold them, so a query running across the compaction sees
a consistent set either way.

**Then, so it does not recur.** Check the diagnostic first, because it will tell you whether
this is one table or a general shortfall:

```bash
sankhya-server doctor
```

If one table is named, raise the maintenance duty cycle for that table. If several are, the
duty cycle is too low overall and raising it per table is a treadmill.

**Do not** shorten retention to reduce file count. Retention governs how long superseded
files are kept for readers and snapshots; shortening it removes files a reader may still be
holding, and the reader's failure will look nothing like this alert.

## What "fixed" looks like

`sankhya_table_live_files` for the table falls sharply at the compaction and then rises again
at the table's ordinary rate. If it rises back to the threshold faster than the duty cycle
brings it down, the duty cycle is still too low and this alert will return — the diagnostic
will say so with a date before it does.

## If compaction does not reduce the count

Then something is holding the old files: an open snapshot lease, or a reader that has not
finished. Retirement deliberately refuses to remove a file a reader might still reach, so the
count stays high and this is the system protecting a query rather than failing. Look for
long-running sessions and snapshot leases before looking at compaction.
