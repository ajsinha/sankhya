# Runbook --- maintenance is failing

**Alert:** `sankhya_maintenance_failures_total` is above zero.

**Lead time:** days. File counts climb before any read is slow enough to notice.

## Symptom

Nothing, at first, and that is the point of this page existing. Maintenance runs on its own
thread on a fixed cadence; a pass that fails answers no query and returns no error to anyone.
The user-visible consequence arrives days later as
[`compaction-debt`](compaction-debt.md) --- and that runbook opens by telling you the alert
"almost never means compaction is broken", which is true when maintenance is running and
wrong here. This page exists so the two arrive as different alerts.

Read the four maintenance counters together. They separate four different situations that
all look like "file counts are rising":

| What you see | What it is |
|---|---|
| `rate(sankhya_maintenance_ticks_total[15m])` is zero | The maintainer is not running at all --- see below |
| Ticks rise, `sankhya_maintenance_failures_total` rises | Passes are running and failing. This page |
| Ticks rise, `sankhya_maintenance_declined_total` rises, bytes reclaimed flat | Healthy. Something is still reading the files: a lease, a clone or a snapshot |
| Ticks rise, nothing else moves, file counts climb | The duty cycle is too low. [`compaction-debt`](compaction-debt.md) |

## What is actually wrong

A maintenance pass compacts small files into larger ones and retires files no reader still
needs. A failed pass did neither, for at least one table. Files therefore accumulate at the
rate writes produce them, opposed by nothing.

The usual causes, in the order they are worth checking:

- **The filesystem refused a write.** Out of space, or the warehouse directory is no longer
  writable. The server log carries the underlying error; this counter only says it happened.
- **A table will not replay.** A corrupt or truncated commit in one table's log fails that
  table's pass every cadence. `sankhya-server doctor` names it.
- **The warehouse lock is held by another process.** Only one maintainer may act on a
  warehouse. A second server started against the same directory is refused the lock, which is
  the design working --- but if the *wrong* one holds it, the one you are watching does
  nothing.

If ticks are not rising at all, maintenance is disabled or its thread is gone. Check the
configuration first: `maintenance:` absent, or present with no `interval`, disables it, and
that is a supported configuration for a deployment whose warehouse another process maintains.
The server says which at startup.

## What to do

**First, find out which table.** The counter is unlabelled on purpose --- a per-table label
names the warehouse to an unauthenticated endpoint, the same reason
`sankhya_table_live_files` is gated. The diagnostic answers it to somebody who has
authenticated:

```bash
sankhya-server doctor
```

**Then fix the cause, not the symptom.** If the filesystem is full, reclaiming space lets the
next pass succeed with no further action --- maintenance retries on its own cadence and needs
no restart. If a table will not replay, that table is the diagnostic's business and the rest
of the warehouse is still being maintained; the counter stays above zero until it is dealt
with, which is correct.

**Do not disable maintenance to silence this.** The alert would clear and the file counts
would go on climbing, which is exactly the state that made this page necessary.

**Do not start a second server against the warehouse to "help".** One maintainer per
warehouse is enforced by the lock, and a second process compacting the same tables is the
second-writer failure `cargo xtask check-writers` exists to stop.

## What "fixed" looks like

- `sankhya_maintenance_failures_total` stops rising. It does not go down --- it is a counter,
  and the alert should be written on `increase(...[1h]) > 0` rather than on the total.
- `sankhya_maintenance_ticks_total` goes on rising, at roughly the configured cadence.
- `sankhya_maintenance_bytes_reclaimed_total` moves again, unless
  `sankhya_maintenance_declined_total` is rising too --- in which case a reader still holds
  the files and flat reclamation is the correct answer.
- `sankhya_table_live_files_max` stops climbing, then falls. It falls slowly: compaction
  works through the backlog at the configured duty cycle, so give it hours, not minutes.

