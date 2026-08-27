# SANKHYA — Quickstart

**Status:** Implementation — M0–M4 complete, M5 in progress

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
domain-vocabulary prohibition, the duplicate-dependency gate, the documentation checks,
the feature pins, clippy under the workspace's denied lints, and the mutation
catalogue's agreement with the source. **Each is proven to fail when violated**, not
merely to pass.

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
cargo test --workspace          # 630 tests, none of which needs a database
```

Everything here runs without a database, in well under a minute. Nothing is mocked: the
Parquet is real Parquet, the Delta logs are read back by an independent kernel, and the
TPC-H data is generated rather than fixtured.

**The transactional and capture half:**

| Suite | What it establishes |
|---|---|
| `sankhya-types` | Summation is order-independent — the property that decides fixed-point over floating point |
| `sankhya-cdc-model` | The wire decoder never panics on arbitrary input, and decodes a stream captured from a real server |
| `sankhya-cdc-apply` | A transaction is never split across batches, however events interleave |
| `sankhya-cdc-pg` | A replication slot is never created or dropped in a way that could silently lose a position |
| `sankhya-schema` | Every type round-trips exactly or is refused with a reason; naming collisions are refused rather than disambiguated; all ten tables onboard from the live stream alone |
| `sankhya-ingest` | Several tables capture independently from one interleaved stream, with no rows lost or leaked between them; a restart recovers its position from the table log; captured data digests identically to the source |
| `sankhya-datagen` | The generator is reproducible, which is what makes reconciliation meaningful |

**The storage half:**

| Suite | What it establishes |
|---|---|
| `sankhya-table` | Text values become typed Arrow; an unparseable value is an error, never a null; compaction merges without changing what a query returns |
| `sankhya-table-delta` | The log survives a torn write and a gap in the version sequence. **`tests/oracle.rs` is the one to read first**: it reads every log this crate writes back with `delta_kernel`, an independent implementation, because two of our own components agreeing proves nothing |
| `sankhya-table-memory` | The arrival buffer never releases a segment publication has not covered — the defect that made a mid-stream table claim positions it never held |
| `sankhya-stats` | Recorded bounds are never narrower than the truth, including under NaN and integer overflow. A bound that is too *wide* costs a wasted read; one that is too narrow is a wrong answer |
| `sankhya-maintenance` | Compaction converges; retirement refuses to remove a file a reader might still hold; orphan sweeping refuses to remove one a retained snapshot still reaches |

**The analytical half — most of what M3 added:**

| Suite | What it establishes |
|---|---|
| `sankhya-readpath` `tests/provider.rs` | Planning reads the table log alone — no directory listing, no footer reads — and a many-file scan is genuinely parallel at the scan node |
| `sankhya-readpath` `tests/pruning.rs` | Files the catalogue proves irrelevant are skipped, and **the same query returns the same answer with and without the catalogue**. Pruning that changes an answer is the failure mode |
| `sankhya-readpath` `tests/spliced.rs` | One SQL statement is answered from memory and Parquet at once, with no position counted twice or missed, and refused outright when the tiers do not cover the query's span |
| `sankhya-readpath` `tests/mutable.rs` | An updated row is returned once, at its current version, and a deleted one not at all |
| `sankhya-readpath` `tests/hostile.rs` | A malformed predicate, an empty tier and a corrupt footer produce errors rather than panics or wrong answers |
| `sankhya-olap` `tests/tpch.rs` | TPC-H at scale factor 1. **`tests/cross_engine.rs` is the honest one**: every query's result is compared against the engine's own listing-based plan over the same files, so a provider bug cannot hide behind a self-consistent answer |
| `sankhya-olap` `tests/exactness.rs` | An approximate answer is labelled approximate. A sketch-derived count never presents itself as exact |
| `sankhya-governor` | Deadlines and cancellation are bounded at one batch per partition; an aggregation too large to run is refused up front, and the refusal says whether retrying could ever help |
| `sankhya-numeric` | Reductions are deterministic regardless of partition order — the analytical counterpart to the `sankhya-types` property |

### The checks that are not tests

Three gates catch things a test suite structurally cannot. All three fail the build.

```bash
cargo xtask check-all            # every repository invariant — see below
python3 tools/mutation-audit.py  # 127 deliberate defects, applied one at a time
cargo xtask check-performance    # the NFR-PERF objectives, as a gate that can fail
```

**`check-all`** runs eight invariants: the layer graph is acyclic and points the right
way, no file exceeds the length ceiling, no core crate names a domain concept, the
dependency set has no critical duplicates, the documentation's links and version claims
resolve and its status lines agree, test-only dependencies really are test-only, clippy
is clean under the workspace's denied lints across every target, and no mutation is left
applied to the source. Each is proven to fail when violated, not merely to pass.

**The mutation audit** is the answer to "the tests pass, but do they test anything?" It
applies 127 specific defects one at a time and requires the suite to fail on each. Thirteen
did not, the first time it ran. Expect it to take a while — it is 127 sequential
`cargo test` runs, and it edits your source files as it goes, restoring each one after.
Run it on a clean tree.

**`check-performance`** is deliberately outside `check-all`: it generates a
scale-factor-1 dataset and needs a machine that is not otherwise busy.

---

## 4. Start the server and connect to it

```bash
cargo build --release -p sankhya-server
SANKHYA_NO_PASSWORD=1 SANKHYA_LISTEN=127.0.0.1:5433 ./target/release/sankhya-server
```

In another shell, with any PostgreSQL client:

```bash
psql -h 127.0.0.1 -p 5433 -U you -d acme -c "SELECT version();"
psql -h 127.0.0.1 -p 5433 -U you -d acme -c "\dt"
```

`SANKHYA_NO_PASSWORD` is spelled as an opt-*out* so the insecure choice has to be made
deliberately, and the startup line says `NO AUTHENTICATION` in capitals when it is in force.

**Statements are refused, deliberately and by name.** The read path exists and is tested;
it is not connected to this front door yet. A `SELECT` against a real table returns an error
saying so, because an empty result would look like a table with no rows and a plausible zero
would look like an answer.

---

## 5. Start a database and load data

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

## 6. Watch capture work

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

## 7. Tidy up

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
| The server binary | **It runs, and `psql` connects to it.** A wire-protocol front door that authenticates, answers catalogue queries from a policy-filtered table list, and hash-chains what it did into an audit. **It does not execute statements yet** — the read path is built and tested but not connected to the front door, and a statement gets a named refusal saying exactly that rather than an empty result |
| Streaming transport | **Not built.** Changes are drained through a SQL function rather than a replication connection. Neither mainstream Rust PostgreSQL client supports the replication protocol, so this is real work rather than wiring |
| Automatic table onboarding | **Working across many tables.** Schema, write strategy and path are derived from the replication stream alone; several tables capture independently from one interleaved stream and each reconciles against the source. Nothing drives it on a timer |
| Storage and the table log | **Working.** Each table gets its own Delta log; capture commits every file it publishes, and a restart recovers its position from that log rather than from memory. The Delta kernel reads these tables, which is what makes the open-storage claim testable rather than aspirational |
| Compaction and maintenance | **Working as a loop, not as a daemon.** Fragmented partitions are planned, merged, committed and converged, with retirement refusing to remove anything a reader might still hold. Nothing calls the loop on a timer |
| Analytical queries | **Working, and measured.** A table provider plans from the table log alone — no directory listing, no footer reads — prunes files by recorded statistics, feeds bounds and cardinalities to the optimizer, and resolves updated and deleted rows to one current version each. One SQL statement is answered from memory and Parquet at once, spliced so no position is counted twice or missed, and refused outright when the tiers do not cover the query's span. TPC-H at scale factor 1 meets its three performance objectives under a build gate. No result cache, no bloom filters, no partitioning |
| Query governance | **Working.** Deadlines and cancellation bounded at one batch per partition; admission control that refuses an aggregation too large to run rather than letting it take the process down, and says whether retrying could ever help |
| Graph engine | **Working.** A typed, time-aware adjacency hydrated from published tables — no second store, no graph write path, an edge exists because a row exists. Traversal, weighted and k-shortest loopless paths, simple cycles, components, centrality, communities and multiplicative influence, each bounded and each reporting its own truncation. Five SQL table functions make them joinable against ordinary tables. Nothing drives hydration on a timer |
| The extension mechanism | **Working.** SANKHYA's own function traits rather than the engine's, so a pack survives the engine changing underneath it. Two reference packs from unrelated industries and one deliberately hostile pack whose every attempt is refused with a named error. A declarative tier expresses a pack as a file rather than a crate. No loader is wired into a running process, because there is not one |
| API surfaces | **The wire protocol works; the rest is not started.** Real `psql` connects, authenticates, runs catalogue queries and recovers from errors. Arrow Flight SQL, the gRPC control plane and the REST gateway are not built |
| Multi-tenancy and security | **Working as components, not as a running system.** One principal type established at the edge; a pure policy component whose every decision is a function of its inputs; a `Guard` that cannot be constructed except from an allowed decision, so a provider cannot be built without one. Row predicates are enforced above the scan where no provider can decline them, and their presence in the *final physical plan* is asserted. Per-tenant graph epochs, quotas with typed errors, a hash-chained audit and envelope encryption with rotation that never touches data |

The honest summary is that the **correctness contracts are built and tested and the
machinery that runs them continuously is not**. Every capability above is exercised by
the test suite; none of it is exercised by a process you can start.

[`STATUS.md`](STATUS.md) is the authoritative version of this table, including the
defects found along the way and what they cost to find.

Progress is tracked in [`ROADMAP.md`](ROADMAP.md) and
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).
