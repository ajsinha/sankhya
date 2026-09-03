<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# What SANKHYA changes about PostgreSQL

**Document ID:** SNK-PG-001 · **Version:** 0.1.0 · **Status:** Implementation — M0–M8, M10 and M13 complete; M8's scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Governs:** `sankhya-oltp-pg`, `sankhya-cdc-pg`
**Decides with:** [DEC-02](REQUIREMENTS.md), [ADR-0021](adr/0021-vectors-matrices-across-the-tiers.md), [ADR-0001](adr/0001-dependency-pin-set.md)

PostgreSQL is the transactional tier — the writer of record, the authoritative copy, the thing
a point lookup is answered from. This document says exactly what SANKHYA does to it, and why
each change is necessary rather than convenient.

It exists because the question an operator asks first is the one a feature list never answers:
*what are you going to do to my database?*

---

## The short answer

**Nothing that a person could not do by hand, and nothing that is not written down here.**

| | |
|---|---|
| Is PostgreSQL forked or patched? | **No.** Stock upstream binaries, unmodified. |
| Is it linked into the SANKHYA binary? | **No.** It is a child process. |
| Are there SANKHYA extensions in C? | **No.** |
| Is the on-disk format changed? | **No.** `pg_dump`, `pg_basebackup` and a standard restore all work. |
| Can you take SANKHYA away and keep the database? | **Yes.** Stop the supervisor and the cluster is an ordinary PostgreSQL cluster. |

That last row is the one that matters. Every change below is either a **configuration setting**
an operator could set themselves, or an **object in a schema** they could drop. There is no
state SANKHYA leaves in PostgreSQL that only SANKHYA can read.

---

## 1. Two modes, and which changes apply to each

`FR-OLTP-03` and `FR-OLTP-04`.

**Managed mode.** SANKHYA carries the PostgreSQL binaries, runs `initdb` on first boot, and
supervises the cluster as a child process for its whole lifecycle. One process tree, one
artifact, no external installation, no DBA action. This is what makes a single-binary
evaluation possible.

**Attached mode.** SANKHYA connects to a cluster somebody else operates. It changes far less,
asks for a documented minimum privilege set, and degrades gracefully when an optional privilege
is absent rather than refusing to start.

> **Managed mode is single-node, deliberately.** A supervised child belongs to one host.
> Multi-node deployments use attached mode against an externally managed, highly-available
> cluster. `REQUIREMENTS.md` records that as a product boundary rather than a gap.

Everything below is marked **[managed]**, **[attached]** or **[both]**.

---

## 2. Cluster creation — [managed]

`initdb` is run with exactly these arguments:

| Argument | Why |
|---|---|
| `-U sankhya` | The bootstrap superuser. Named rather than defaulting to the operating-system user, so the cluster's owner does not depend on who happened to start it. |
| `--auth=trust` | See below — it is safe **because** of the listener setting, and only because of it. |
| `-E UTF8` | The encoding every text column is read as downstream. A cluster in another encoding would produce mojibake in the analytical copy, discovered months later. |
| `--no-sync` | On **creation only**. A cluster that has no data yet has nothing to lose to a crash, and syncing an empty cluster is seconds of startup for no protection. It does **not** affect the running cluster's durability. |

### Why `trust` authentication is not a hole here

It would be an open database over TCP. It is not one over a Unix socket inside a directory the
operator already owns, because the boundary is the same boundary the data directory itself has:
anyone who can reach the socket can already read the files.

This is only true while the listener setting below holds. **The two are one decision** and
changing either without the other is how a private cluster becomes a public one.

---

## 3. The listener — [managed]

```
listen_addresses = ''
unix_socket_directories = '<data directory>/sockets'
```

The postmaster accepts connections over a Unix domain socket inside the data directory and
**nowhere on the network**. There is no port.

That is what lets a managed cluster hold the system of record without an operator reasoning
about firewalls, and it is what makes `trust` above defensible. A deployment that needs network
access to the transactional tier is a deployment that wants attached mode.

---

## 4. Settings for change capture — [both]

> **State: required and not yet applied.** `sankhya-oltp-pg` supervises a cluster and does not
> configure these, and `sankhya-cdc-pg` is built and unwired. So a managed cluster today would
> **not** support logical replication, and this section is the specification the wiring must
> meet rather than a description of what runs. Recorded plainly because a settings table that
> looks like a description of the present is worse than an empty one.

Capture reads the write-ahead log through a logical replication slot. That needs:

| Setting | Value | Why this and not less |
|---|---|---|
| `wal_level` | `logical` | The minimum at which the log carries enough to reconstruct a row. `replica` is not enough and produces a slot that yields nothing. **Requires a restart.** |
| `max_replication_slots` | ≥ 1 per SANKHYA instance, plus the operator's own | A slot is a named position. Exhausting them fails slot creation at the worst time — during a re-snapshot after an incident. |
| `max_wal_senders` | ≥ `max_replication_slots` | Each active slot holds a sender. |
| `max_slot_wal_keep_size` | **Set to a real bound, never `-1`** | See below. This is the most important setting in this document. |

### `max_slot_wal_keep_size` — the setting that prevents an outage

A logical replication slot retains write-ahead log from its restart position forward. If
SANKHYA's consumer stalls — starved of CPU by a runaway analytical query, say — PostgreSQL
retains log **indefinitely** until the filesystem fills, at which point the database shuts down.

**The failure mode is that an analytical query takes down the transactional system.** In any
production deployment that is the worst possible outcome.

`max_slot_wal_keep_size` bounds it: past the bound, PostgreSQL invalidates the slot instead of
filling the disk. That protects the database and **an invalidated slot cannot be resumed** — the
analytical tier needs a full re-snapshot of every replicated table, which is a multi-hour
outage of *reads* rather than an outage of *writes*.

That is the right trade and it is not a free one, which is why `sankhya-cdc-pg` watches the
slot's retained bytes and reports the approach rather than the arrival. Learning a slot is at
80% of its bound is enormously better than discovering it was destroyed. This was not
theoretical during development: a slot was invalidated by exactly this, which is why the
monitoring exists.

### PostgreSQL 17 or later, not "16+"

PostgreSQL 17 introduced **failover-capable logical replication slots**. Without them, a routine
database failover destroys the slot and forces a full re-snapshot — a multi-hour analytical
outage triggered by an ordinary high-availability event. The version floor is that one feature.

---

## 5. Schema objects SANKHYA creates — [both]

> **State: designed, not built.** Nothing creates these yet.

All of it lives in a schema of its own, so an operator can see what is SANKHYA's at a glance and
drop the lot in one statement.

| Object | Purpose |
|---|---|
| A schema, `sankhya` | Everything below lives in it. Nothing is created in `public` or in a user's schema. |
| A publication | Which tables are captured. Named, so `\dRp+` shows exactly what is replicated. |
| A slot | The capture position. One per instance. |
| A catalogue table | Where a matrix column's **shape** is recorded — see §7. |

**`REPLICA IDENTITY` on captured tables** is the one change that touches a *user's* table.
Logical replication needs enough of the old row to identify an update or a delete; a table with
no primary key needs `REPLICA IDENTITY FULL`, which makes the log carry the whole row.

That is a real cost on a wide table, and it is **not applied silently**: a table without a usable
identity is reported, with the statement that would fix it, and capture of that table does not
start until somebody runs it. Changing a table's replica identity is a decision about write
amplification on the transactional tier, and it is the operator's.

---

## 6. Extensions — [both]

> **State: decided, not vendored.** [ADR-0021](adr/0021-vectors-matrices-across-the-tiers.md)
> Decision 1 names `pgvector` as the candidate; pinning a third-party extension is
> [ADR-0001](adr/0001-dependency-pin-set.md)'s question and has not been answered.

| Extension | Required? | What it buys |
|---|---|---|
| none | — | **Arrays need nothing.** `float8[]` has been in PostgreSQL for decades, so a `VECTOR(n)` column works on a stock cluster. |
| `pgvector` | optional | A `vector(n)` type that carries its width, and HNSW / IVFFlat indexes for nearest-neighbour retrieval. |

### The rule that governs the index, and why it is not obvious

> **An index may rank candidates. Only a kernel may report a distance.**

`pgvector`'s indexes are approximate — that is what makes them fast. An approximate index that
*ranks* and a SANKHYA function that *scores* are two implementations of one notion of distance,
and [ADR-0020](adr/0020-the-built-in-function-catalogue.md) Decision 2 refuses that pairing:
two implementations agree until they do not, and the day they disagree the answer depends on
which path served it.

So a query using the index retrieves *k* candidates and **rescores them with the kernel**. The
number a user is shown is never the index's opinion, and a caller comparing a returned distance
against a threshold is comparing against the same arithmetic a full scan would give.

---

## 7. Column types — [both]

> **State: decided, not built.** [ADR-0021](adr/0021-vectors-matrices-across-the-tiers.md).

| SANKHYA type | In PostgreSQL | In Parquet |
|---|---|---|
| `ARRAY(t)` | `t[]` — native, no extension | `List<t>` |
| `VECTOR(n)` | `float8[]` with a generated `CHECK (array_length(c, 1) = n)`, or `vector(n)` with `pgvector` | `FixedSizeList<Float64, n>` |
| `MATRIX(r, c)` | `float8[]` with a `CHECK` on `r · c`, shape in SANKHYA's catalogue | `FixedSizeList<Float64, r·c>` with shape in field metadata |

**The width is enforced by a constraint SANKHYA generates**, because PostgreSQL's array type
carries none: `ARRAY[1,2,3]` and `ARRAY[1,2]` have the same type. The constraint means a row
that does not fit is refused by the store itself rather than by whatever reads it next.

That matters more than it sounds. The width is what lets a kernel take a contiguous slice
instead of copying per row, and a similarity between a 384-dimensional embedding and a
512-dimensional one is not a near miss — it is a different question.

**A matrix's shape is recorded in SANKHYA's catalogue, not in a column comment.** A comment is
documentation; the shape is a fact the read path needs in order to reshape a flat array. Putting
it where a `COMMENT ON COLUMN` could silently change it would make the shape editable by anybody
with DDL rights, with no way to notice.

---

## 8. What SANKHYA will not do to your PostgreSQL

Stated as prohibitions, because a list of what a system *does* never answers the question an
operator is actually asking.

- **It will not alter a user's table without being told.** The one change that touches one —
  `REPLICA IDENTITY` — is reported with its statement and waits for a person.
- **It will not create objects outside its own schema.**
- **It will not install an extension it has not declared here.**
- **It will not change a setting a restart depends on while the cluster is running.**
  `wal_level` needs a restart, and a supervisor that restarted the system of record to fix its
  own configuration would be an outage caused by a convenience.
- **It will not hold a slot without bounding it.** See §4.
- **It will not leave state only it can read.** Every object above is an ordinary PostgreSQL
  object, readable with `psql` and droppable with `DROP`.

---

## 9. Attached mode: the privilege set — [attached]

> **State: designed, not built.**

| Privilege | Needed for | If absent |
|---|---|---|
| `REPLICATION` | Creating and reading the slot | Capture cannot run. Refused at startup, naming the privilege. |
| `CREATE` on a schema | The `sankhya` schema and its catalogue | Refused at startup with the statement to grant it. |
| `SELECT` on captured tables | The initial snapshot | That table is not captured; the others are. |
| `ALTER` on captured tables | Setting `REPLICA IDENTITY` | Reported with the statement, and capture of that table waits. |

**Graceful degradation is by table, not by cluster.** A table SANKHYA may not read is one table
missing from the analytical copy, reported by name — not a system that refuses to start. The
opposite behaviour turns a permissions oversight on one table into a total outage.

---

## 10. What is built today

Because a document that describes an intention in the present tense is the kind that gets
believed.

| | State |
|---|---|
| Supervising a cluster — `initdb`, start, readiness, stop | **Built and tested** against a vendored PostgreSQL 17.11 |
| `listen_addresses = ''` and the socket | **Built** |
| Slot health monitoring — retained bytes, the approach to the bound | **Built and tested** |
| Wiring any of it into the server | **Not built.** `Settings` has no OLTP configuration; this is M8 §12.2 |
| Capture settings (§4) | **Not applied.** A managed cluster today would not support logical replication |
| Schema objects (§5), extensions (§6), types (§7) | **Not built** |
| The rule for routing a statement that calls a built-in | **Built and tested**, before the router, because it is unaffordable to retrofit |

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
