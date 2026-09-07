# Runbook — compaction debt

**Alert:** `sankhya_table_live_files_max` is approaching or past 1,000.

The alerting series carries **no label**, so it says that *some* table has too many files and not
which one. That is deliberate: `/metrics` is unauthenticated by Prometheus's convention, and a
per-table label enumerates the warehouse to anybody who can reach the port (`SEC-08`). To find the
table, run `sankhya-server doctor` — or set `server.metrics_detail: true` where the metrics
interface is one clients cannot reach, which restores the per-table `sankhya_table_live_files`
series. The diagnostic reads the warehouse off disk and takes no principal; what protects it is
shell access to the host, not a login.

**Lead time:** days, at ordinary write rates.

**First, check maintenance is running at all.** This page is about a duty cycle that is too
low, and it looks identical to a maintainer that has stopped:

```promql
rate(sankhya_maintenance_ticks_total[15m])   # zero: not running. Nothing below applies
increase(sankhya_maintenance_failures_total[1h])  # above zero: `maintenance-stalled`, not this
```

If either says so, go to [`maintenance-stalled`](maintenance-stalled.md). The rest of this page
assumes passes are running and succeeding.

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

So this alert almost never means "compaction is broken" --- **once you have checked that
maintenance is running**, which is what the counters at the top of this page are for. It usually
means **the maintenance duty cycle is too low for this table's write rate.** Compaction is
running; it is not running often enough to keep up with this particular table.

`sankhya_maintenance_declined_total` rising while reclaimed bytes stay flat is the other benign
reading: passes are running and choosing not to reclaim, because a lease, a clone or a snapshot
still reads the files. That is the sweeper honouring a reader, and the fix is to release whatever
holds them --- not to raise the duty cycle, which will change nothing.

## What to do

**Now, to relieve it.** Lower the compaction interval in the configuration and reload:

```yaml
maintenance:
  compact_every: 1        # every tick rather than every Nth
  interval: 10s           # and tick more often
```

```bash
kill -HUP $(pidof sankhya-server)
```

The server reads the configuration again from the same files in the same precedence order and
applies the new policy on the next tick — no restart, and no window to wait for. It says what
it did, on stdout, so the reload can be confirmed rather than assumed.

**There is deliberately no command that compacts by hand.** The server holds the warehouse
lock and is the only maintainer of the warehouse it holds; a second process compacting the
same tables is the second-writer failure `cargo xtask check-writers` exists to stop. Earlier
versions of this runbook told you to run `sankhya maintenance compact`, which is a binary that
does not exist.

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

`sankhya_table_live_files_max` falls sharply at the compaction and then rises again
at the table's ordinary rate. If it rises back to the threshold faster than the duty cycle
brings it down, the duty cycle is still too low and this alert will return — the diagnostic
will say so with a date before it does.

## If compaction does not reduce the count

Then something is holding the old files: an open snapshot lease, or a reader that has not
finished. Retirement deliberately refuses to remove a file a reader might still reach, so the
count stays high and this is the system protecting a query rather than failing. Look for
long-running sessions and snapshot leases before looking at compaction.
