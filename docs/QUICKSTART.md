# SANKHYA — Quickstart

**Status:** the project is in early implementation. This guide reflects what works
**today**, and says plainly what does not yet. Anything not listed here is not built.

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
| Analytical query engine | **A vertical slice works.** A captured workload becomes Parquet and answers SQL, with an exact decimal sum matching the source. There is no table provider, no catalog and no server around it yet |
| The server binary | **A stub.** There is no daemon to run yet |
| Automatic table onboarding | **Working across many tables.** Schema, write strategy and path are derived from the replication stream alone; several tables capture independently from one interleaved stream and each reconciles against the source. There is no long-running process driving it yet |
| Graph engine | Not started |
| API surfaces | Not started |
| Multi-tenancy and security | Not started |

What *does* work today is the foundation those depend on: a verified dependency set, a
correct wire decoder validated against a real server, an apply path with its
transaction invariant under test, a lossless type mapping checked against a real
schema, and a reproducible dataset at realistic scale.

Progress is tracked in [`ROADMAP.md`](ROADMAP.md) and
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).
