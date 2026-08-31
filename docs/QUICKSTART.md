<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Quickstart

**Status:** Implementation — M0–M8 complete; M8's scale-out half moved to M12 for want of a second machine; M9 in progress

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
the feature pins, clippy under the workspace's denied lints, the mutation catalogue's
agreement with the source, the generated metric and error catalogues' agreement with
their declarations, and every counted figure the documentation claims. **Each is
proven to fail when violated**, not merely to pass.

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
cargo test --workspace          # 1,997 tests, none of which needs a database
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
| `sankhya-math` | Reductions are deterministic regardless of partition order — the analytical counterpart to the `sankhya-types` property |
| `sankhya-server` `tests/five_minutes.rs` | **This guide, executed.** Generate a warehouse, start the real binary, connect over the real wire protocol, query, run the diagnostic, take a backup and prove it — seven documented steps, timed, on every build. It is a test so it cannot rot |
| `sankhya-api-rest` | A result too large for JSON comes back as a **Flight ticket rather than a refusal**, decided from the plan's estimate before anything is materialised; an estimate that was low abandons the response rather than truncating it; and a route matches its path whole |
| `sankhya-diagnostic`'s soak module | A steady baseline passes and every shape of injected leak fails: memory retained, descriptors not returned, a sawtooth whose peaks climb, and an audit drifting to two records per query **while its total looks healthy**. A run that sampled nothing does not pass |
| `sankhya-version` | An artefact from a newer release is refused **by name** rather than failing as a parse error somewhere in the middle; an older but supported one is read and never written back |
| `sankhya-backup` | A manifest refuses to bind an analytical tier that is ahead of its source; a drill catches altered rows that every file-presence check passes; deleting a backup does not release its files; and the evidence keeps the failures |
| `sankhya-metrics` | An undeclared metric cannot be recorded, a closed label refuses anything outside its set, and an identifier label stops adding series at its cap rather than growing without bound — and says it has |
| `sankhya-diagnostic` | A projection is never invented from one sample, never drawn through a sawtooth, and never extrapolated further than the observation window supports. Findings sort by *when*, not by how bad. A check that could not run is never counted as one that found nothing |

### The checks that are not tests

Three gates catch things a test suite structurally cannot. All three fail the build.

```bash
cargo xtask check-all            # every repository invariant — see below
python3 tools/mutation-audit.py  # 495 deliberate defects, applied one at a time
cargo xtask check-performance    # the NFR-PERF objectives, as a gate that can fail
SANKHYA_RELEASE=1 cargo xtask check-package   # the release artifact's platform baseline
```

**`check-all`** runs eleven invariants: the layer graph is acyclic and points the right
way, no file exceeds the length ceiling, no core crate names a domain concept, the
dependency set has no critical duplicates, the documentation's links and version claims
resolve and its status lines agree, test-only dependencies really are test-only, clippy
is clean under the workspace's denied lints across every target, no mutation is left
applied to the source, the generated metric and error catalogues still match their
declarations and every declared metric is actually recorded somewhere, and every figure a
document claims — test counts, catalogue sizes — still matches what the repository holds. Each is proven to fail when violated, not
merely to pass.

**The mutation audit** is the answer to "the tests pass, but do they test anything?" It
applies 495 specific defects one at a time and requires the suite to fail on each. Thirty-one
did not, the first time each was run — the most recent two were written for the tiering
encoding, and both exposed tests that did not test what their names claimed: one compared two
integer widths whose encodings already differ in length, so removing the type tag changed
nothing, and one used a composite key that the framing bytes separate without any length
prefix. That is precisely the silent-pass this tool exists to catch. Expect it to take a
while — it is 495 sequential `cargo test` runs, and it edits your source files as it goes,
restoring each one after. Run it on a clean tree.

**`check-performance`** is deliberately outside `check-all`: it generates a
scale-factor-1 dataset and needs a machine that is not otherwise busy.

**`check-package`** compares two numbers that live in different files and that nothing else
relates — the server's drain deadline and every deployment manifest's termination grace. When
the grace is the shorter of the two, every deploy kills the server mid-drain and clients see
resets that look like crashes. It also reads the **platform baseline** the built binary
actually requires. On a development build that is a warning; under `SANKHYA_RELEASE=1` it
fails, because a binary built on a current distribution silently requires symbol versions the
customer's enterprise distribution does not have, and the build machine cannot tell you so.
Today this build needs `GLIBC_2.34` against a declared baseline of `2.28` — see
[`STATUS.md`](STATUS.md) §10.4. Every supported platform, its baseline and what is published
for it are in [`PLATFORMS.md`](PLATFORMS.md).

---

## 4. Start the server and connect to it

```bash
cargo build --release -p sankhya-server
SANKHYA_NO_PASSWORD=1 \
SANKHYA_WAREHOUSE=./warehouse \
SANKHYA_LISTEN=127.0.0.1:5433 \
  ./target/release/sankhya-server
```

`SANKHYA_WAREHOUSE` is a directory of `<schema>/<table>/`, each table holding Parquet files
and a `_delta_log`. The server walks it at startup, reads each table's schema **out of its
own log** — not from a Parquet footer, which a table with no files yet does not have — and
opens it through the read path. A table it cannot open is named on stderr rather than
omitted: a server that starts with three tables of four and says nothing produces an outage
that looks, to whoever queries it, like a table nobody ever created.

In another shell, with any PostgreSQL client:

```bash
psql -h 127.0.0.1 -p 5433 -U you -d acme -c "SELECT version();"
psql -h 127.0.0.1 -p 5433 -U you -d acme -c "\dt"
```

`SANKHYA_NO_PASSWORD` is spelled as an opt-*out* so the insecure choice has to be made
deliberately, and the startup line says `NO AUTHENTICATION` in capitals when it is in force.

Statements execute against the Parquet on disk:

```
$ psql ... -c "SELECT region, count(*) AS n, round(sum(amount)) AS total
               FROM orders GROUP BY region ORDER BY region;"
 region |  n  | total
--------+-----+--------
 north  | 334 | 250250
 south  | 361 | 249251
        | 361 | 249750
(3 rows)
```

The third row's region is genuinely null, not an empty string. The distinction survives from
the Parquet page through the Arrow array to the wire, where it becomes a length of −1.

Columnar throughout: Parquet on disk, Arrow in memory, and the read path plans from the
table log alone — no directory listing and no footer reads — pruning files by the statistics
the log records.

**What the query path enforces.** A table the principal may not read is never registered in
the session, so naming it fails to resolve — indistinguishable from naming a table that does
not exist, which is deliberate: saying "you may not read that" would confirm it exists. A
policy row predicate is conjoined where no provider can decline it, so a tautology in the
query cannot widen it. Both are tested end to end.

### Vector columns and their kernels

A column can hold a vector per row — an embedding, a factor vector, a time-series window —
as `FixedSizeList<Float64, N>`, and the kernels are ordinary SQL functions:

```sql
SELECT title, vec_cosine_similarity(embedding, :query) AS score
FROM documents
ORDER BY score DESC
LIMIT 10;
```

`vec_dot`, `vec_euclidean`, `vec_cosine_similarity`, `vec_cosine_distance`, `vec_norm_l1`,
`vec_norm_l2`, `vec_sum`, `vec_mean`.

Linear algebra over matrix columns:

```sql
SELECT mat_determinant(covariance), mat_trace(covariance) FROM portfolios;
SELECT mat_solve(coefficients, observations) FROM systems;
SELECT mat_multiply(a, b) FROM pairs;
```

`mat_multiply`, `mat_transpose`, `mat_inverse`, `mat_solve`, `mat_vec`, `mat_determinant`,
`mat_trace`. A matrix is stored flat and its shape comes from **field metadata**, using
Arrow's canonical `arrow.fixed_shape_tensor` extension — a column with no declared shape is
refused rather than assumed square, because that guess is wrong for every rectangular matrix
and produces numbers from values that were never in the same row.

Vectors and matrices can be built in SQL, and the constructors emit their own shape — so a
matrix can be built and operated on without ever being stored:

```sql
SELECT mat_determinant(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0));   -- -2
SELECT vec_norm_l2(vec_of(3.0, 4.0));                        -- 5
SELECT mat_trace(mat_identity(4));                           -- 4
```

`vec_of`, `mat_of`, `mat_identity`. A matrix's shape is part of its *type*, so `mat_of`'s
dimensions must be literals and a wrong element count is refused **when the query is
planned** — not partway through a scan, after work has been done.

Matrix-returning functions carry their own shape too, so `mat_determinant(mat_multiply(a,
b))` works: the product knows it is `rows(a) × columns(b)`.

QR, SVD and eigendecomposition are **not** offered. They are where an in-house
implementation is genuinely worse than none: a subtly wrong SVD produces plausible singular
values.

**Every reducing kernel is bit-deterministic.** A dot product is a floating-point sum, and a
sum whose order depends on how the query was partitioned returns a different number when the
machine is busier. These go through the same compensated, order-fixed summation the rest of
the system uses, so two runs of the same ranking produce the same order rather than a
similar one. See [ADR-0005](adr/0005-array-columns-and-numeric-kernels.md) for why that
ruled out both Polars and a native BLAS.

Two costs worth knowing: **an array column cannot be pruned** — a minimum and maximum of a
vector prune nothing — so a table of embeddings prunes on `sank_data_date` and its scalar
columns only. And **an array cannot be a key column**, because array equality as row identity
is a bad idea and is refused rather than supported badly.

### Publishing a table from outside

An external system publishes through **this system's library**, not by assembling the
format itself:

```bash
cargo build --release -p sankhya-publish
./target/release/sankhya-publish verify ./warehouse/sales/orders
```

The format is open and documented — external engines read it directly, and the library is
in this repository under the same licence for anyone who wants to see exactly what it does.
What the library provides is not secrecy but **correctness by construction**: there is no
way to call it that produces a file without statistics, a schema that does not round-trip,
or an action missing a field the format requires.

The reason is asymmetry. A *reader* that misunderstands the format is wrong for itself,
recoverably. A *writer* that misunderstands it corrupts the table for everyone,
permanently, and undetectably — because the writer's own reader shares the
misunderstanding. This system made exactly that mistake once, writing its own format with
the specification open; see the defect table in [`STATUS.md`](STATUS.md).

`verify` does not assume the library was used, because a recommendation is not an
invariant. It reports *what* is wrong rather than *whether*, and distinguishes a finding
that makes queries **slow** from one that makes them **wrong** — exit 0 clean, 1 slow, 2
wrong, so a build gate can fail on one and not the other.

If a table is deficient — published by something that did not record statistics, say — it
can be repaired:

```bash
./target/release/sankhya-publish repair ./warehouse/sales/orders          # shows a plan
./target/release/sankhya-publish repair ./warehouse/sales/orders --apply  # carries it out
```

Repair **derives, never guesses**. Statistics are recomputed by reading the file, because
the file is the truth. Anything needing a guess — a missing schema, a key column that does
not exist — is refused with what a person has to decide, because a tool that invents a
plausible value writes it into the table permanently, with an operator's confidence attached.

It never deletes, and it appends a new version rather than rewriting a committed one: the
broken commit stays exactly as it was, so the repair is auditable and revertible and time
travel to before it still works.

To create a warehouse to try this against:

```bash
SANKHYA_WAREHOUSE=./warehouse \
  cargo test -p sankhya-server --test make_warehouse -- --ignored
```

That writes one table of 1,000 rows across four Parquet files, so the read path has
something to prune and to parallelise over.

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

## 7. Check the health of a warehouse

```bash
./target/release/sankhya-server doctor
```

It reads the warehouse directly and does not start the server, because the day you want a
diagnostic is often the day the server will not start.

```
SANKHYA doctor 0.1.0
  warehouse ./warehouse
  1 table(s)

  [note] table sales.orders — 990 live files; no projection is possible from 1
         observation(s): a time needs a rate, and a rate needs at least 2.

0 check(s) clean, 1 finding(s) of which 0 have a date, 0 check(s) could not run
```

**That is the correct output for a first run, and it is the whole point.** `FR-OPS-17` asks
for the time until a problem bites rather than its current value — and a time cannot be
computed from one sample. So the first run reports the value, refuses the date, and names
what is missing. Run it again after some load and it will tell you when:

```
  [warning] table sales.orders — 900 live files; at the current rate, about 1 day.
         Compact it: `sankhya maintenance compact --table sales.orders`. …
```

Put it in cron — hourly is enough — and the projections become real:

```cron
17 * * * * SANKHYA_WAREHOUSE=/srv/sankhya/warehouse /usr/local/bin/sankhya-server doctor
```

Exit status is `0` clean, `1` findings, `2` a check could not run. The third exists so a
monitoring system cannot read "I could not look" as "nothing found".

Full detail, including why it refuses to project through a sawtooth, is in
[`GUIDE.md` §10](GUIDE.md#10-the-diagnostic).

---

## 8. Watch what it is doing

```bash
curl -s http://127.0.0.1:9464/metrics
```

```
sankhya_queries_total{outcome="ok"} 412
sankhya_queries_total{outcome="refused"} 3
sankhya_query_duration_seconds_bucket{outcome="ok",le="0.025"} 388
sankhya_table_live_files{table="sales.orders"} 87
sankhya_metrics_rejected_total{reason="over_cap"} 0
```

Its own port, one route, loopback by default. The full list is [`METRICS.md`](METRICS.md),
which is **generated from the declarations** — recording a metric requires passing its
declaration, so an undeclared metric cannot be typed, and a declared one that nothing records
fails the build.

Two things to know before you build a dashboard on it:

- **`refused` is not `error`.** A quota held is the system working. An error-rate alert that
  counts them together fires on correct behaviour.
- **Watch `sankhya_metrics_rejected_total`.** Non-zero means a call site disagrees with the
  catalogue, or a label has outgrown its cap and that metric is now incomplete.

And when something fails:

```
ERROR:  [SNK-C0001] Error during planning: table 'sales.ordres' not found
DETAIL:  Correct the statement. The detail names the offending element.
```

The code is permanent and is what [`ERRORS.md`](ERRORS.md) and the
[runbooks](runbooks/) are indexed by. `DETAIL` is the catalogue's own remediation, so the
client and the documentation cannot disagree.

---

## 9. Prove the backup

```bash
./target/release/sankhya-server backup     # record a manifest
./target/release/sankhya-server drill      # prove it restores
```

A backup here is a **manifest**, not an archive: it binds the transactional backup you took,
the table versions in the warehouse and the key generation to one point, and protects the
files so they stay readable.

```
SANKHYA restore drill 0.1.0
  backup:01a04442-936a-73a1-bfd1-964c8cd66330
  sales.orders: verified, 1000 row(s)

Proven. 1 table(s) read back and digested.
```

**It reads the data back and recomputes its digest.** Try it — corrupt a file and drill again:

```bash
printf garbage > warehouse/sales/orders/part-0000.parquet
./target/release/sankhya-server drill; echo "exit=$?"
```

```
  sales.orders: could not be read (…part-0000.parquet: Parquet file too small)

NOT PROVEN. 1 of 1 table(s) did not verify — this backup would not restore what it claims
to hold.
exit=1
```

A file-presence check would have passed on that. It passes on almost every failure that
actually happens, because a *missing* file is loud — what goes wrong is that a file is there
and wrong.

Exit `0` proven, `1` a table did not verify, `2` could not run. **Alert on `2` as well**: a
monitor treating "could not look" as "nothing wrong" reports a backup as proven when nothing
examined it.

Both runs are kept in `<data-dir>/restore-drills.jsonl`, append-only and including the
failures — a drill history with no failures describes either a very good system or a drill
that does not really run, and nothing in the history says which. `doctor` reads the last
**pass** from it, never the last attempt.

---

## 10. Tidy up

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
| The server binary | **It runs, `psql` connects, and statements execute against real Parquet.** It walks a `<schema>/<table>/` warehouse at startup, reads each table's schema out of its own log, and opens it through the M3 read path — which plans from the table log alone and prunes files by recorded statistics. Authentication, policy-filtered catalogue answers, a hash-chained audit, and a query path that authorises, wraps each table in its policy decision, plans, executes and renders back. Nothing drives ingest, so everything it serves is already published |
| Streaming transport | **Not built.** Changes are drained through a SQL function rather than a replication connection. Neither mainstream Rust PostgreSQL client supports the replication protocol, so this is real work rather than wiring |
| Automatic table onboarding | **Working across many tables.** Schema, write strategy and path are derived from the replication stream alone; several tables capture independently from one interleaved stream and each reconciles against the source. Nothing drives it on a timer |
| Storage and the table log | **Working.** Each table gets its own Delta log; capture commits every file it publishes, and a restart recovers its position from that log rather than from memory. The Delta kernel reads these tables, which is what makes the open-storage claim testable rather than aspirational |
| Compaction and maintenance | **Working, and running in the server.** Fragmented partitions are planned, merged, committed and converged, with retirement refusing to remove anything a reader might still hold. Since M8 the server maintains its own warehouse on its own thread when a maintenance policy is configured — so the warehouse moves whether or not anybody is writing to it, and the read path re-resolves a table whose log has advanced |
| Analytical queries | **Working, and measured.** A table provider plans from the table log alone — no directory listing, no footer reads — prunes files by recorded statistics, feeds bounds and cardinalities to the optimizer, and resolves updated and deleted rows to one current version each. One SQL statement is answered from memory and Parquet at once, spliced so no position is counted twice or missed, and refused outright when the tiers do not cover the query's span. TPC-H at scale factor 1 meets its three performance objectives under a build gate. No result cache, no bloom filters, no partitioning |
| Mathematics | **Working.** Vectors and matrices as columns, and the kernels over them: elementwise, dot, norms, distances, statistics, calculus, and linear algebra through LU. Every reduction is bit-deterministic. Callable from SQL as `vec_*` and `mat_*`, with constructors that let a matrix be built and operated on without being stored. No QR, SVD or eigendecomposition |
| Publishing and repair | **Working.** A library and command-line tool for writing an external table, and a verifier that does not assume it was used. Repair fixes only what can be derived from evidence and refuses anything needing a guess |
| Query governance | **Working.** Deadlines and cancellation bounded at one batch per partition; admission control that refuses an aggregation too large to run rather than letting it take the process down, and says whether retrying could ever help |
| Backup and restore | **Working for the analytical half.** A manifest binds table versions and a key generation to a consistent point and refuses to record an inconsistency; a drill reads the data back and digests it; the evidence is append-only. Backing up the transactional store is your own tooling's job — the manifest binds to it and does not take it |
| Soak testing | **The harness works, is proven to detect a leak, and a forty-five-minute run at twenty gigabytes passes** — 1.16 billion rows scanned, resident memory flat, 21.5 GB reclaimed. The multi-day run is not done and moved to M12 with the rest of the scale-out work. A short run against the real server, under concurrent writes, queries and maintenance, runs on every build. See [`SOAK.md`](SOAK.md) |
| Cubes | **Working, and declarable from SQL.** A cube is a declared model over a published table — dimensions, levels, hierarchies, and how each measure may combine along each dimension — answered on demand with no build step. Slice, dice, roll-up and drill-down are table functions; every row carries its snapshot, its completeness and whether it came from a cuboid. `CREATE CUBE` and `DROP CUBE` are statements, and a drop reclaims what the cube materialised. No MDX, deliberately |
| Concurrency and data safety | **Working, and measured against a control.** Commits are per-table and atomic, files are published atomically, and reclamation never removes a file a reader holds. Each concurrency claim is measured twice in the same run — once as the code stands, once forced through one mutex — because a single warehouse lock satisfies every safety property while destroying concurrency. Leader election, executor scale-out and failover are **not built**: they need a second machine and moved to M12 |
| Lifecycle tiering | **Not built, and gated.** `sankhya-tiering` is deliberately empty. M9 is in progress and its first piece exists — an attestation drill that proves a write-once store still refuses writes by attempting to overwrite, delete and truncate it. **Destructive purge stays disabled until reconciliation has run clean in production**; building the purge path and arming it are two decisions |
| Packaging | **Checks, not artifacts.** The platform baseline is declared and the built binary is measured against it; every deployment manifest's termination grace is compared with the server's drain deadline. Container images and signing are not built |
| Upgrade and rollback | **Tested as far as one release allows.** Every on-disk format carries a version, an artefact from a newer release is refused by name rather than failing as a parse error, and a corpus of earlier-release artefacts is read on every build. Running the *previous binary* needs a previous binary |
| The diagnostic | **Working for four checks.** `doctor` walks the warehouse, records what it sees, and projects a date for compaction debt once it has two runs to compare, and reports how long the backup has been unproven — and, for a deployment that archives anything, how long the write-once controls have gone unattested. Storage headroom and replication lag are built as checks with nothing feeding them observations. The rest of `FR-OPS-16` — conformance, replica identity, archival consistency — is not built |
| Graph engine | **Working.** A typed, time-aware adjacency hydrated from published tables — no second store, no graph write path, an edge exists because a row exists. Traversal, weighted and k-shortest loopless paths, simple cycles, components, centrality, communities and multiplicative influence, each bounded and each reporting its own truncation. Five SQL table functions make them joinable against ordinary tables. Nothing drives hydration on a timer |
| The extension mechanism | **Working.** SANKHYA's own function traits rather than the engine's, so a pack survives the engine changing underneath it. Two reference packs from unrelated industries and one deliberately hostile pack whose every attempt is refused with a named error. A declarative tier expresses a pack as a file rather than a crate. **The loader is not wired into the server**: a running process exists, and nothing in it loads a bundle |
| API surfaces | **Two of four.** Real `psql` connects, authenticates, runs catalogue queries and recovers from errors. **Arrow Flight SQL** streams results as Arrow batches over gRPC, with authorization at planning and a ticket bound to the tenant it was issued to. The gRPC control plane and the REST gateway are not built |
| Multi-tenancy and security | **Working, and reachable through the server.** A statement arriving over the wire is authorised before a table is registered, so one the caller may not read does not resolve at all. One principal type established at the edge; a pure policy component whose every decision is a function of its inputs; a `Guard` that cannot be constructed except from an allowed decision, so a provider cannot be built without one. Row predicates are enforced above the scan where no provider can decline them, and their presence in the *final physical plan* is asserted. Quotas with typed errors and a hash-chained audit are wired into the query path. Per-tenant graph epochs and envelope encryption are built and tested but have no path through the front door |

The honest summary is that the **correctness contracts are built and tested and the
machinery that runs them continuously is not**. Every capability above is exercised by
the test suite; none of it is exercised by a process you can start.

[`STATUS.md`](STATUS.md) is the authoritative version of this table, including the
defects found along the way and what they cost to find.

[`GUIDE.md`](GUIDE.md) is the next thing to read: what to *do* with a running server,
worked through with examples. Every example on that page is executed by a test, so one that
stops working breaks the build rather than misleading a reader.

Progress is tracked in [`ROADMAP.md`](ROADMAP.md) and
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).
