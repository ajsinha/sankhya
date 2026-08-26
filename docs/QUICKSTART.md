# SANKHYA — Quickstart

**Status:** Implementation — M0–M3 complete, M4 in progress

This guide reflects what works **today**, and says plainly what does not yet. Anything
not listed here is not built.

Everything below runs on one machine with no container runtime, no message broker, no
object store and no cloud credentials.

---

## What you need

| | |
|---|---|
| Rust | 1.90 or later (`rustup` recommended) |
| C toolchain | `gcc`, `make`, `bison`, `flex`, `perl`, `pkg-config` |
| Libraries | `readline`, `zlib`, `openssl`, `icu` development headers |
| Disk | ~25 GB free if you run the full acceptance dataset |
| Memory | 8 GB is enough; the acceptance load is comfortable at 16 GB |

On Debian or Ubuntu:

```bash
sudo apt install build-essential bison flex libreadline-dev zlib1g-dev \
                 libssl-dev libicu-dev pkg-config
```

---

## 1. Build

```bash
git clone https://github.com/ajsinha/sankhya.git
cd sankhya
cargo build --workspace
```

The first build compiles a large dependency graph — expect several minutes. The
enforcement tooling builds in seconds and is worth running first:

```bash
cargo xtask check-all
```

That runs every repository invariant: the layer graph, the file-length ceiling, the
domain-vocabulary prohibition, the duplicate-dependency gate, and the documentation
checks. **All five are proven to fail when violated**, not merely to pass.

---

## 2. Build the vendored database

SANKHYA carries PostgreSQL's source, verified by checksum, and builds it into a private
prefix. Nothing is installed system-wide and no existing PostgreSQL is touched.

```bash
vendor/postgresql/build.sh
```

Roughly two minutes on a modern machine. It is idempotent — re-running it verifies the
checksum and exits if the build is already current.

```bash
.build/pg-install/bin/postgres --version     # PostgreSQL 17.11
```

**Why 17 or later:** failover-capable logical replication slots. Without them a routine
database failover destroys the slot and forces a full re-snapshot of every replicated
table — a multi-hour outage of the analytical tier triggered by an ordinary
availability event.

---

## 3. Run the tests

```bash
cargo test --workspace
```

Everything here runs without a database. The interesting parts:

| Suite | What it establishes |
|---|---|
| `sankhya-types` | Summation is order-independent — the property that decides fixed-point over floating point |
| `sankhya-cdc-model` | The wire decoder never panics on arbitrary input, and decodes a stream captured from a real server |
| `sankhya-cdc-apply` | A transaction is never split across batches, however events interleave |
| `sankhya-schema` | Every type round-trips exactly or is refused with a reason; naming collisions are refused rather than disambiguated; all ten tables onboard from the live stream alone |
| `sankhya-plan` | A query is answered from tiers covering its span **exactly once**; a session never reads from before a write it has already seen |
| `sankhya-table` | Text values become typed Arrow; an unparseable value is an error, never a null |
| `sankhya-ingest` | Several tables capture independently from one interleaved stream, with no rows lost or leaked between them; captured data digests identically to the source |
| `sankhya-datagen` | The generator is reproducible, which is what makes reconciliation meaningful |

---

## 4. Start a database and load data

```bash
# Initialise a throwaway cluster configured for logical replication
export PGBIN=$PWD/.build/pg-install/bin
export PGSOCK=/tmp/sankhya-sock
mkdir -p "$PGSOCK"

$PGBIN/initdb -D .build/pg -U sankhya --auth=trust -E UTF8 --no-sync

cat >> .build/pg/postgresql.conf <<'CONF'
wal_level = logical
max_replication_slots = 8
max_wal_senders = 8
listen_addresses = ''
unix_socket_directories = '/tmp/sankhya-sock'
max_slot_wal_keep_size = 8GB
CONF

$PGBIN/pg_ctl -D .build/pg -l .build/pg.log start -w
```

Create the fixture schema and a publication:

```bash
cargo run --release -p sankhya-datagen --bin sankhya-datagen -- ddl \
  | $PGBIN/psql -h $PGSOCK -U sankhya -d postgres -q

$PGBIN/psql -h $PGSOCK -U sankhya -d postgres -c \
  "CREATE PUBLICATION sankhya_all FOR ALL TABLES;"
```

See what a load would produce before running it:

```bash
cargo run --release -p sankhya-datagen --bin sankhya-datagen -- plan --gb 10
```

Then load. Start small:

```bash
cargo run --release -p sankhya-datagen --bin sankhya-datagen -- copy --gb 1 \
  | $PGBIN/psql -h $PGSOCK -U sankhya -d postgres -q -v ON_ERROR_STOP=1
```

The full acceptance dataset is `--gb 10`: **99.2 million rows across ten tables**,
about three minutes and 14 GB on disk.

The ten schemas are deliberately drawn from logistics, telemetry, retail, media,
energy and civic domains. None is financial — the general-purpose claim is tested
rather than asserted, and fixtures from a single industry would let a core quietly
shaped around that industry pass every test.

---

## 5. Watch capture work

```bash
export SANKHYA_PG_BIN=$PWD/.build/pg-install/bin
export SANKHYA_E2E_SOCKET=$PGSOCK
crates/sankhya-cdc-apply/tests/run_e2e.sh
```

This issues a workload, captures the resulting replication stream, decodes it, applies
it, and **reconciles the result against an independent count taken from the source** —
not against a second pass of our own decoder, which would let a shared defect cancel
out and pass.

Expect something like:

```
e2e: 933 messages decoded, 922 mutations across 4 transactions, reconciled against source
```

### One setting that will otherwise confuse you

If capture appears to see nothing while rows are plainly present, check:

```bash
$PGBIN/psql -h $PGSOCK -U sankhya -d postgres -c "SHOW synchronous_commit;"
```

Under `synchronous_commit = off` a transaction returns before its WAL reaches disk, and
**logical decoding reads flushed WAL only**. The change is durable enough to query and
entirely invisible to capture. Every symptom that normally indicates a problem looks
healthy — the source is up, the slot is valid, the rows are there, no errors are
reported. See [`adr/0002-async-commit-and-decoding-visibility.md`](adr/0002-async-commit-and-decoding-visibility.md).

---

## 6. Tidy up

```bash
$PGBIN/pg_ctl -D .build/pg stop -m fast
rm -rf .build/pg /tmp/sankhya-sock
```

The whole `.build/` directory is scratch and is safe to delete; `vendor/postgresql/build.sh`
recreates what it needs.

---

## What does not work yet

Stated explicitly, because a quickstart that implies more than exists is worse than one
that admits less.

| | Status |
|---|---|
| The server binary | **A stub.** There is no daemon to run, and no listener. Everything below is exercised through tests rather than through a running process |
| Streaming transport | **Not built.** Changes are drained through a SQL function rather than a replication connection. Neither mainstream Rust PostgreSQL client supports the replication protocol, so this is real work rather than wiring |
| Automatic table onboarding | **Working across many tables.** Schema, write strategy and path are derived from the replication stream alone; several tables capture independently from one interleaved stream and each reconciles against the source. Nothing drives it on a timer |
| Storage and the table log | **Working.** Each table gets its own Delta log; capture commits every file it publishes, and a restart recovers its position from that log rather than from memory. The Delta kernel reads these tables, which is what makes the open-storage claim testable rather than aspirational |
| Compaction and maintenance | **Working as a loop, not as a daemon.** Fragmented partitions are planned, merged, committed and converged, with retirement refusing to remove anything a reader might still hold. Nothing calls the loop on a timer |
| Analytical queries | **Working, and measured.** A table provider plans from the table log alone — no directory listing, no footer reads — prunes files by recorded statistics, feeds bounds and cardinalities to the optimizer, and resolves updated and deleted rows to one current version each. One SQL statement is answered from memory and Parquet at once, spliced so no position is counted twice or missed, and refused outright when the tiers do not cover the query's span. TPC-H at scale factor 1 meets its three performance objectives under a build gate. No result cache, no bloom filters, no partitioning |
| Query governance | **Working.** Deadlines and cancellation bounded at one batch per partition; admission control that refuses an aggregation too large to run rather than letting it take the process down, and says whether retrying could ever help |
| Graph engine | Not started |
| API surfaces | Not started |
| Multi-tenancy and security | Not started |

The honest summary is that the **correctness contracts are built and tested and the
machinery that runs them continuously is not**. Every capability above is exercised by
the test suite; none of it is exercised by a process you can start.

[`STATUS.md`](STATUS.md) is the authoritative version of this table, including the
defects found along the way and what they cost to find.

Progress is tracked in [`ROADMAP.md`](ROADMAP.md) and
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).
