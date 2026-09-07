# Runbook --- maintenance is failing

**Alert:** `increase(sankhya_maintenance_failures_total[1h]) > 0`.

Written as an increase rather than on the total, because it is a counter: a rule on
`> 0` fires permanently after one transient failure and clears only on restart.

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
| `rate(sankhya_maintenance_ticks_total[15m])` is zero | The maintainer is not running, **or maintenance is configured off** --- check the configuration first, because both read the same |
| Ticks rise, `increase(sankhya_maintenance_failures_total[1h])` above zero | Cycles are running and at least one table is failing. This page |
| Ticks rise, `increase(sankhya_maintenance_declined_total[1h])` above zero, bytes reclaimed flat | **Not healthy.** The pin set cannot be established: a snapshot or clone document that cannot be read, or a pinned version whose files will not resolve. The warehouse is deliberately not shrinking |
| Ticks rise, nothing else moves, file counts climb | The duty cycle is too low. [`compaction-debt`](compaction-debt.md) |

**Do not divide one of these by another.** `sankhya_maintenance_ticks_total` counts one per
cycle; `declined` and `failures` count one per **table** per cycle. On a fifty-table warehouse
a single bad cycle adds fifty to one and one to the other, so `declined` routinely exceeds
`ticks` and the ratio is in no unit at all. Read each against its own rate.

## What is actually wrong

A maintenance pass compacts small files into larger ones and retires files no reader still
needs. A failed pass did neither, for at least one table. Files therefore accumulate at the
rate writes produce them, opposed by nothing.

The usual causes, in the order they are worth checking:

- **The filesystem refused a write.** Out of space, or the warehouse directory is no longer
  writable. The server log carries the underlying error; this counter only says it happened.
- **A table will not replay.** A corrupt or truncated commit in one table's log fails that
  table's pass every cadence. `sankhya-server doctor` names it.

An earlier version of this page listed *"the warehouse lock is held by another process"* third.
It cannot be the cause: a server that fails to take the lock exits `3` before anything starts,
so there is no state in which the server you are watching is running, publishing these metrics,
and losing a lock race. It has been removed rather than left as something to check at 3 a.m.

If ticks are not rising at all, maintenance is disabled or its thread is gone, **and the four
counters cannot tell you which.** With maintenance off nothing publishes them, and they read a
flat zero --- which is what a thread that died on its first cycle also reads. `absent()` does
not help either: the series is present, because an unlabelled metric is emitted at zero from
startup. Check the configuration and the startup line, which say so in words:

```
  maintaining 12 table(s) every 30s, compacting every 4 tick(s), sweeping every 16
```

`maintenance:` absent, or present with `interval: 0`, disables it --- a supported
configuration for a deployment whose warehouse another process maintains. **On such a
deployment the alert on this page is disarmed**, and nothing in the metrics says so.

## What to do

**First, find out which table.** The counter is unlabelled on purpose --- a per-table label
names the warehouse to an unauthenticated endpoint, the same reason `sankhya_table_live_files`
is gated. The diagnostic answers it. It takes **no principal**: it reads the warehouse off
disk, and what protects it is shell access to the host rather than a login.

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

