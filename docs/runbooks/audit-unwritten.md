# Runbook — the audit is not being written

**Alert:** `sankhya_audit_unwritten_total` is above zero.

**Lead time:** none. The first failure is already a gap.

## Symptom

Nothing that a user sees. Queries are answered normally, refusals are refused normally, and
`sankhya_audit_records_total` goes on rising — because the record is still being *made*. It is
not reaching the disk.

The server also says so at startup when it could not open the file at all:

```
  audit chain head 0000…0000 (0 record(s)), IN MEMORY ONLY --- it will be lost at the next
  restart, and `_audit/chain.jsonl` could not be opened
```

## What is actually wrong

The audit is a hash-linked chain, and each record links to the digest of the one before it. It
lives in `_audit/chain.jsonl` under the warehouse and is synced before the statement that caused
it is answered. This counter rises when that write or that sync fails.

The consequences are worth separating, because only the second is urgent:

- **The chain in memory is still correct.** Verification passes, the head is real, and everything
  recorded so far this run is available through `SHOW` and to the process.
- **The chain on disk is now shorter than the chain in memory**, and the records that did not
  reach it are lost at the next restart. Worse, the *next* record that succeeds will link to a
  digest whose predecessor is not in the file — so the stored chain will fail verification at
  that point, permanently, and the gap is where the failure began.

## Why it is not a refusal

A write that fails does not stop the statement. That is a decision rather than an oversight: this
server does not promise to refuse service when it cannot audit, and a warehouse that stops
answering because a log partition filled would be an outage caused by its own bookkeeping. What
it must not do is fail *silently*, which is why this counter exists — an audit that has quietly
stopped recording looks exactly like a quiet server.

If your deployment needs the stronger posture — refuse rather than answer unaudited — that is a
change to `Server::append`, and it is the kind of change that should be made deliberately and
written down, not discovered.

## What to check, in order

1. **Is the disk full?** `df -h` on the filesystem holding the warehouse. This is the common
   cause and the counter starts rising the moment it fills.
2. **Are the permissions right?** The server must be able to write `_audit/` under the warehouse.
   A warehouse restored from a backup, or copied with `sudo`, often is not writable by the
   account the server runs as.
3. **Is the file there and is it a file?** `ls -l <warehouse>/_audit/chain.jsonl`. A directory of
   that name, or a dangling symlink, produces the same counter.
4. **Read the log.** The failure is logged with the system's own message at `ERROR`, which names
   which of the three it is.

## Recovering

Fix the cause, then **restart the server**. The restart is not optional and it is not cosmetic:
the chain is read back at startup, so a restart is what makes the file and memory agree again.
Until then every further record extends a chain the file does not have.

Before restarting, take the head from the **startup line of the run that is about to end** — it
is in the log where the server began:

```
  audit chain head 3f1c… (12874 record(s))
```

That is the only surface the head has today: there is no statement that reports it, and this
runbook is not going to pretend otherwise. Keep the value and the record count. A chain cannot
detect its own truncation — removing the last *n* records leaves one that verifies perfectly — so
a head from before the restart, held somewhere the server cannot write, is the only evidence of
how long the chain was.

> **This is a gap.** The head is printed once, at start, and the count that matters is the one at
> the moment the writing failed. Mirroring it continuously is what §13.5 says makes truncation
> detectable, and there is nothing in this server that does it.

## Verifying the fix

- `sankhya_audit_unwritten_total` stops rising.
- The startup line no longer says `IN MEMORY ONLY`.
- `wc -l <warehouse>/_audit/chain.jsonl` grows as statements are answered.

## What this does not protect against

An attacker with write access to the disk. They can truncate the tail, and no local check detects
that. Publishing the head somewhere append-only is what makes the true length knowable, which is
why it is printed at every start — see §13.5.
