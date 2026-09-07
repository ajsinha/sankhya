<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Operations

**Document ID:** SNK-OP-001
**Version:** 0.1.0
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Date:** 2026-09-06
**Companions:** [`ARCHITECTURE.md`](ARCHITECTURE.md) — why it is built this way. [`SECURITY.md`](SECURITY.md) — the posture and the policy file. [`GUIDE.md`](GUIDE.md) — every feature by worked example. [`QUICKSTART.md`](QUICKSTART.md) — build it and load data. [`POSTGRES.md`](POSTGRES.md) — what this does to a PostgreSQL cluster.

---

## 1. What this document is

The reference an operator needs to run this and nothing else. It answers: what do I install, what do I set, what does it write where, what do I watch, and what do I do at three in the morning.

It is deliberately not a tutorial — [`QUICKSTART.md`](QUICKSTART.md) is — and deliberately not a feature tour, which is [`GUIDE.md`](GUIDE.md). Where a design decision explains a behaviour, the explanation lives in [`ARCHITECTURE.md`](ARCHITECTURE.md) and is linked rather than repeated. Documents restating each other is how this project's last audit found 129 claims that nothing checked.

**Every statement here names the file that decides it**, and `cargo run -p xtask -- check-docs` verifies that every source path named below exists. It cannot verify that the prose still describes what the code does — that remains a review responsibility, and saying so is the point.

---

## 2. What you are operating

One process. `sankhya-server`, one per warehouse, enforced by an exclusive lock on the data directory (`crates/sankhya-server/src/main.rs`). There is no cluster, no coordinator election and no second node; multi-node is `M12` and needs a second machine.

Inside it, and this is the whole of it:

| | |
|---|---|
| **Three listeners** | The PostgreSQL wire protocol, Arrow Flight SQL, and an HTTP endpoint serving exactly `GET /metrics` |
| **One Tokio runtime** | A bare `#[tokio::main]`, multi-threaded, workers from `available_parallelism()` |
| **One memory pool** | A single `FairSpillPool` shared by every statement in the process |
| **One maintenance thread** | Compaction, retirement, orphan sweeping, on a tick |
| **Background tasks** | Snapshot and clone pin refresh, maintained-cube refresh, the declared-feed spool runner |

And, so it is not discovered by disappointment, **what is not running**: there is no change capture (no replication slot, no applier, no arrival buffer wired to ingest), no archival tiering, no REST gateway, no `/health` or `/ready` endpoint, no supervised PostgreSQL, and no working `sankhya-cli` — that binary prints one line and exits `2` (`crates/sankhya-cli/src/main.rs`).

The workspace keeps that list machine-readable rather than in prose. `UNREACHED` in `xtask/src/surfaces.rs` names every crate no binary reaches, each with the milestone that will change it, and the build fails both when an unlisted crate becomes unreachable *and* when a listed one becomes reachable and is not removed. `UNREACHABLE` in `xtask/src/catalogues.rs` does the same for error codes nothing can produce — which matters here, because **an alert rule written against one of those codes is permanently silent**. [`ARCHITECTURE.md`](ARCHITECTURE.md) §2 reads both lists.

---

## 3. Getting the binary

### 3.1 There is no release

Stated first because it is the thing an operator plans around. Nothing in this repository produces a shippable artifact: no tarball, no `.deb`, no `.rpm`, no checksums, **no SBOM and no signing**. The CI workflow (`.github/workflows/gate.yml`) builds the workspace, runs `cargo xtask check-all` and runs `cargo deny`; it publishes nothing. `xtask` has `check-package` and no `package`.

So today the binary comes from source:

```bash
cargo build --release --locked -p sankhya-server
# target/release/sankhya-server
```

`--locked` is not optional advice. The Arrow, Parquet, DataFusion and `object_store` family is exact-pinned, and two Arrow majors in one process make identically named types incompatible — a correctness hazard, not an inefficiency. [ADR-0001](adr/0001-dependency-pin-set.md) is the record and `cargo xtask check-dupes` is the gate.

### 3.2 Platform baseline, and the gap it currently has

Five targets are declared in `xtask/src/package.rs`, generated into [`PLATFORMS.md`](PLATFORMS.md):

| Triple | Role | Baseline |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Server | glibc 2.28 |
| `aarch64-unknown-linux-gnu` | Server | glibc 2.28 |
| `x86_64-unknown-linux-musl` | Server | musl |
| `aarch64-apple-darwin` | Server | macOS 12.0 |
| `x86_64-pc-windows-msvc` | **Client only** | — |

`cargo xtask check-package` reads what the built binary *requires* — `readelf --dyn-syms` for the highest `GLIBC_x.y`, `readelf -d` for the `DT_NEEDED` set — rather than what the build intended, because a binary built on a current distribution silently acquires symbol versions from it, links, runs and tests clean locally, and fails the first time somebody on an enterprise distribution starts it. Nothing on the build machine can surface that by construction. The check warns locally and **fails only when `SANKHYA_RELEASE` is set**, because a check that fails every developer's build is a check everybody learns to ignore.

> **The declared baseline is not currently met by the container.** The image builder is `rust:1.97-bullseye`; Debian 11 ships glibc 2.31 and the resulting binary needs `GLIBC_2.30`, measured by extracting it from the image rather than assumed. Debian 10 is end-of-life and the honest route to 2.28 is a cross-toolchain with an old sysroot, which has not been done. The gap is pinned in `xtask/src/package.rs` as `BUILDER`, printed on every `check-package` run, and the builder cannot be changed without re-measuring. **The image will not start on RHEL 8 or Debian 10.**

### 3.3 The container

`packaging/Dockerfile`, two stages, built by nothing in this repository — you build and push it yourself.

```bash
docker build -t ghcr.io/ajsinha/sankhya:0.1.0 -f packaging/Dockerfile .
docker run --rm \
  -v sankhya-data:/var/lib/sankhya \
  -e SANKHYA_WAREHOUSE=/var/lib/sankhya/warehouse \
  -e SANKHYA_DATA_DIR=/var/lib/sankhya/.sankhya \
  -e SANKHYA_LISTEN=0.0.0.0:5433 \
  -p 5433:5433 \
  ghcr.io/ajsinha/sankhya:0.1.0
```

The tag must equal the workspace version: `check-package` fails when a manifest names an image tag the workspace is not at, and fails when a manifest names an image no `Dockerfile` builds. Both were true once — the shipped Kubernetes manifest named an image that had never been built, a volume claim no manifest defined, and no Service at all.

The runtime stage is `debian:bullseye-slim` rather than `scratch`, and it installs `coreutils` **because the diagnostic shells out to `df`** to measure free space (§9). It runs as uid/gid `65532`, working directory `/var/lib/sankhya`, entrypoint `sankhya-server`, command `start`. `EXPOSE 5433 5434 9464` is documentation; it opens nothing.

> **A container cannot be given TLS.** Every TLS setting is file-only — there is no `SANKHYA_*` variable for a certificate — so an image configured purely by environment runs in the clear. Mount a configuration file. [`SECURITY.md`](SECURITY.md) §3.1.

---

## 4. Running it

### 4.1 The command surface

Subcommands only. There are **no command-line flags** other than `--help` and `--version`; anything unrecognised exits `2` with the usage text (`crates/sankhya-server/src/main.rs`).

| Command | Effect | Exit statuses |
|---|---|---|
| *(none)* or `start` | Serve | — |
| `doctor` | Report on the warehouse without starting the server | `0` clean, `1` findings, `2` a check could not run |
| `backup` | Record a manifest | `0` recorded, non-zero refused |
| `drill` | Prove the manifest still reads | `0` proven, `1` a table did not verify, `2` could not run |
| `attest <store>` | Prove a write-once store still refuses writes | `0` attested, `1` it allowed something, `2` nothing attempted |
| `hash-password` | Read a password from stdin, print a verifier | `0` |
| `--help`, `--version` | Say what this binary is | `0`, always |

`--help`, `--version` and `hash-password` are answered **before the configuration is read**, deliberately: asking a program what it is must not depend on a file being well formed, and for one release it did. `crates/sankhya-server/tests/configured.rs` runs the binary with a configuration path that does not exist to prove `--version` still answers.

> **`2` is never a pass, on any of them.** "I could not look" and "I looked and found nothing" both produce an empty finding list and are opposite facts. A monitoring system that merges them reports all-clear for a subsystem nobody examined. Alert on `2` separately.

### 4.2 systemd

`packaging/systemd/sankhya.service`. `Type=exec`, `Restart=on-failure`, `KillSignal=SIGTERM`, `TimeoutStopSec=45s`, `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `NoNewPrivileges=yes`, `ReadWritePaths=/var/lib/sankhya`, `ReadOnlyPaths=/etc/sankhya`.

Two settings there are load-bearing rather than hygiene:

- **`LimitNOFILE=65535`.** The wire door serves `MAX_CONNECTIONS = 1024` at once (`crates/sankhya-api-pg/src/listener.rs`), so a server inheriting the usual 1,024-descriptor default reaches it on connections alone.
- **`WorkingDirectory=/var/lib/sankhya` and `Environment=SANKHYA_CONFIG=…`.** The default configuration path is *relative to the working directory*, so a unit with neither gets no configuration file at all and does not say so. `check-package` fails a `*.service` under `packaging/` that sets neither.

### 4.3 Kubernetes

`packaging/kubernetes/deployment.yaml` — a Deployment, a `ReadWriteOnce` PersistentVolumeClaim of 100 GiB, and a ClusterIP Service exposing the two client ports and deliberately **not** the metrics port.

`replicas: 1`, and that is a correctness constraint rather than a starting point: the warehouse lock permits exactly one server per warehouse, because a second maintainer is two committers racing for the same log version and two retirement passes each blind to the other's readers.

Probes, and what they actually check:

| Probe | What it does | What it does *not* do |
|---|---|---|
| Liveness | TCP connect on 5433 | Nothing about lag — a liveness probe that fails on lag makes the orchestrator kill a healthy node and converts degradation into outage |
| Readiness | `GET /metrics` on 9464 | It does **not** evaluate pipeline lag. The manifest's comment says readiness is where lag belongs; that is an aspiration, not a check |

**There is no `/health` and no `/ready`.** Both are declared in `crates/sankhya-api-rest/src/plane.rs`'s route table, and `sankhya-api-rest` is a dependency of nothing — it is on `UNREACHED`. Do not point a probe at them.

### 4.4 The startup line is the deployment's self-description

Everything a person needs to know about how this process is exposed is printed once, in words, on every start. `Server::describe` in `crates/sankhya-server/src/wiring.rs` builds it:

```
SANKHYA 0.1.0
  tenant tenant:00000000-0000-0000-0000-000000000001, NO AUTHENTICATION — every connection is
  accepted, 10 policy rule(s), 10 table(s) known
  listening on 127.0.0.1:5433
  wire protocol unencrypted — passwords cross the network in plain text
  audit chain head 0000…0000 (0 record(s))
  connect with: psql -h 127.0.0.1 -p 5433 -U <user>
  metrics on http://127.0.0.1:9464/metrics
  maintaining 10 table(s) every 30s, compacting every 1 tick(s), sweeping every 120
  Arrow Flight SQL on 127.0.0.1:5434
```

Four things there are load-bearing. `NO AUTHENTICATION` and `PASSWORD UNVERIFIED` are **in capitals**, because both are opt-outs and an operator should see the consequence rather than have to check. The TLS posture is named **in words** every time, so a half-configured server cannot be mistaken for an encrypted one. The maintenance settings are printed, so a deployment that has disabled maintenance says so rather than quietly accumulating files. And **the bound address is printed, not the configured one** — told to bind port 0, this once printed `:0`, so the line whose only job is to say where to connect said nothing.

A table the server cannot open is named on stderr rather than omitted. A server that starts with three tables of four and says nothing produces an outage that looks, to whoever queries it, like a table nobody ever created.

> **Race to know about:** the columnar door is announced *before* its transport binds. A health check that trusts the banner will race it. Recorded rather than changed; the test waits.

### 4.5 Shutdown, drain, and the two numbers nobody relates

`SIGTERM` and `SIGINT` both shut down — one arrives from an orchestrator and the other from a terminal, and a server that handles only one is killed by the other. `SIGHUP` does not shut down; see §5.4.

The drain, in `crates/sankhya-api-pg/src/listener.rs`:

1. The accept loop breaks. New callers are **not refused**; they are simply not accepted.
2. If nothing is in flight, return immediately.
3. Otherwise log `draining before shutdown` with the in-flight count and wait up to `DRAIN` for every connection task to finish.
4. On timeout, warn with how many are still running and abort them.

Then the metrics listener is shut down and awaited, **after** the wire door, so the last scrape completes rather than truncating mid-body.

```rust
pub const DRAIN: Duration = Duration::from_secs(30);
```

That constant is the single source of truth, and the relationship an operator would otherwise have to remember is checked mechanically: `xtask/src/package.rs` parses `DRAIN` out of `crates/sankhya-api-pg/src/listener.rs` and compares it against every `terminationGracePeriodSeconds`, `TimeoutStopSec` and `stop_grace_period` under `packaging/`. Both shipped manifests declare **45 seconds**.

> **Why that check exists.** Two numbers decide whether a shutdown is orderly and they live apart: how long the server needs to finish work already in flight, and how long the orchestrator will wait before `SIGKILL`. They are edited by different people, in different files, for different reasons — and when the second is the shorter, **every deploy severs connections mid-result and clients see something indistinguishable from a crash.** If you write your own manifest, keep its grace strictly above 30 seconds.

Until `M6` this server had no drain at all: `serve_until` returned the moment shutdown resolved, its connection tasks were detached, and the doc comment above it described behaviour it did not have. A client mid-result saw a reset on every deploy.

---

## 5. Configuration

### 5.1 Precedence, and one thing the shipped file gets wrong

Implemented by `Configuration::load_with` in `crates/sankhya-config/src/lib.rs`, lowest precedence first: each named file, then each file's `.local` overlay, then the environment, then command-line arguments.

> **The command-line tier is unreachable from the shipped binary.** `sankhya-config` parses `--key=value`, and `crates/sankhya-server/src/main.rs` calls `load_with` passing an **empty** arguments map. The header comment in `config/application.yaml` lists `--key=value` as the highest tier; it is not true of `sankhya-server`. **Environment wins.**

`${NAME}` inside a value refers to another setting, an environment variable or an argument; `${NAME:default}` makes the reference optional. **An unresolved reference with no default fails the load** rather than reaching a connection string as a literal `${NAME}`.

**An unknown key is an error, not a warning** (`SNK-S0006`), and so is a value that does not parse. Silently ignored typos are a leading cause of production incidents:

```
sankhya: `warehouse.read_as_of` must be an integer and holds `18446744073709551615` —
`warehouse.read_as_of` came from a configuration file (config/application.yaml). Refused rather
than defaulted: a setting that silently becomes something else is a deployment behaving as though
it were configured when it is not
```

That refusal is right, and it is also, once, what the repository's own `config/application.yaml` produced: `read_as_of: 18446744073709551615` is `u64::MAX` and the loader reads the setting as a signed integer, so every subcommand — `--version` included — refused to start. One character, in a default file, of exactly the shape a test using its own fixture cannot see.

### 5.2 The complete `SANKHYA_*` table

Derived from `USAGE` and `legacy_environment()` in `crates/sankhya-server/src/main.rs` plus the three readers that bypass both. **This is every variable the server reads.**

| Variable | Sets | Default | Notes |
|---|---|---|---|
| `SANKHYA_CONFIG` | *(the file list)* | `config/application.yaml`, relative to the working directory | **Comma-separated**, lowest precedence first. A named file that does not exist is a hard refusal; a missing *default* file is skipped silently |
| `SANKHYA_WAREHOUSE` | `warehouse.path` | `./warehouse` | Root of `<schema>/<table>/` |
| `SANKHYA_LISTEN` | `server.listen` | `127.0.0.1:5433` | PostgreSQL wire protocol |
| `SANKHYA_FLIGHT_LISTEN` | `server.flight_listen` | `127.0.0.1:5434` | Arrow Flight SQL. **Always on.** A value that is not a socket address prints a complaint and Flight silently does not start — the process keeps serving |
| `SANKHYA_METRICS_LISTEN` | `server.metrics_listen` | `127.0.0.1:9464` | A bind failure prints `COULD NOT BIND METRICS` and is **not fatal** |
| `SANKHYA_DATA_DIR` | *(the data directory)* | `<warehouse>/../.sankhya` | See the warning below |
| `SANKHYA_READ_AS_OF` | `warehouse.read_as_of` | unset ⇒ everything published | A value that does not convert is refused at startup |
| `SANKHYA_NO_PASSWORD` | `server.require_password=false` | unset ⇒ a password is demanded | **Presence is the signal**; the value is ignored. An opt-*out*, so the insecure choice is deliberate |
| `SANKHYA_USER_FUNCTIONS` | `server.user_functions` | `false` | Whether `CREATE AGGREGATION` is accepted. It runs code the caller supplied |
| `SANKHYA_QUERY_MEMORY_BYTES` | *(the shared pool)* | `1073741824` — one gibibyte | Read in `crates/sankhya-server/src/execute.rs`. **Not a configuration key.** `0`, empty or unparseable leaves the default in force, because a pool of zero bytes is a server that starts and answers nothing, and an empty variable is how one gets set |
| `SANKHYA_STATEMENT_TIMEOUT_SECONDS` | *(the statement deadline)* | `1800` — thirty minutes | Read in `crates/sankhya-server/src/execute.rs`, into a `OnceLock`: **changing it needs a restart**. `0` means effectively no limit. **Not a configuration key** |
| `SANKHYA_FEED_INTERVAL_SECONDS` | *(the feed spool cadence)* | `30` | `0` or unparseable warns and uses 30. Separate from `maintenance.interval` on purpose: turning maintenance off is not a request to stop ingest |
| `SANKHYA_SOURCE_BACKUP` | *(manifest field)* | the literal `"unrecorded"` | Where the transactional backup this manifest binds to lives |
| `SANKHYA_SOURCE_DIGEST` | *(manifest field)* | the literal `"unrecorded"` | Its digest |

> **`SANKHYA_DATA_DIR` has two readers and only one of them works.** `legacy_environment()` maps it to a setting called `data.dir`, and **nothing reads `data.dir` back.** The directory actually used is computed by `data_dir()` in `crates/sankhya-server/src/main.rs`, which reads the environment variable directly. So `data: dir:` in a YAML file is **silently ignored**; only the variable takes effect. `config/application.yaml` ships a `data.dir` block, which is why this is worth knowing rather than academic.

Three further variables read by nothing in the server: `SANKHYA_RELEASE` (tightens `check-package`), `SANKHYA_SOAK_GB` / `SANKHYA_SOAK_TABLES` (the soak harness — see [`SOAK.md`](SOAK.md)), and a family of `SANKHYA_PG_*`, `SANKHYA_E2E_*`, `SANKHYA_TPCH_SCALE` used by tests and `xtask`.

### 5.3 The configuration file

`config/application.yaml` is the shipped default and is heavily commented; read it alongside this table. The sections are `server`, `warehouse`, `data`, `table` (per-table clustering), `maintenance`, and `policy`. Everything under `policy` is documented in [`SECURITY.md`](SECURITY.md) §5, including every field, every mask spelling, and the row-filter grammar.

Two settings have no environment variable and no other home:

- **`server.tls.*`** — `certificate`, `private_key`, `client_ca`, `require`. File-only. [`SECURITY.md`](SECURITY.md) §8.
- **`server.credentials.<user>`** and **`server.users.<user>`** — file-only. Write a verifier with `sankhya-server hash-password`.

> The shipped file's comment on `credentials` refers to "the same rule as `users` above" and **there is no `users:` block in the file.** The key is read; it is simply never exemplified. `server.users.<name>: reader, analyst` is the spelling.

### 5.4 What `SIGHUP` reloads

```bash
kill -HUP $(pidof sankhya-server)
```

The handler is in `crates/sankhya-server/src/main.rs`. On `SIGHUP` the **entire** configuration is re-read in the same precedence order — and exactly three keys are then applied, through the same `maintenance_policy()` function that startup uses, on the next tick:

| Reloaded on `SIGHUP` | Needs a restart |
|---|---|
| `maintenance.interval` | Everything else, without exception |
| `maintenance.compact_every` | — including `policy.rules.*`, `server.credentials.*`, `server.users.*`, `server.tls.*`, every listener address, `server.metrics_detail`, `server.user_functions`, `warehouse.*` |
| `maintenance.orphan_sweep_every` | |

A reload keeps the pending retirement queue and the tick counter — those are *state*, not configuration — so a reload cannot leak files.

Two behaviours to know before relying on it:

- **A reload that asks to disable maintenance is not honoured.** The thread keeps running under its previous settings and says so. Stopping maintenance is a restart, because starting again is not reversible without one.
- **With `maintenance.interval: 0` the handler is never installed at all**, and an unhandled `SIGHUP` terminates the process. On a server with maintenance disabled, `SIGHUP` is a restart with extra steps.

**A revoked grant does not take effect until the process restarts.** That follows from the table above and is stated plainly rather than left to be inferred from an absent row.

---

## 6. What bounds a query

The full argument is in [`ARCHITECTURE.md`](ARCHITECTURE.md) §3. What an operator needs is the list, the knob and the door it applies to.

| Bound | Value | Set by | Applies to |
|---|---|---|---|
| **Memory** | One `FairSpillPool`, 1 GiB | `SANKHYA_QUERY_MEMORY_BYTES` | Every statement in the process, **both doors** |
| **Result rows** | 10,000 | `MAX_RESULT_ROWS`, not configurable | The wire door only |
| **Statement deadline** | 30 minutes | `SANKHYA_STATEMENT_TIMEOUT_SECONDS` (restart) | The wire door only |
| **Connections** | 1,024 | `MAX_CONNECTIONS`, not configurable | The wire door only |
| **Message size** | 16 MiB | `MAX_MESSAGE_BYTES`, not configurable | The wire door only |
| **Metrics request size** | 8 KiB | `MAX_REQUEST_BYTES`, not configurable | The metrics door |

Four things follow that are easy to get wrong.

**The pool is shared, and that is the point.** It is built once, in `shared_runtime()` in `crates/sankhya-server/src/execute.rs`, and held in a `OnceLock`. Until Phase 5.6 it was built per statement, so each statement got its own gibibyte and ten concurrent statements got ten — a per-statement allowance wearing a bound's name, which is worse than no bound because it reads as solved. Fairness was the entire argument for choosing a fair pool over a greedy one: the failure to prevent is one statement taking every other connection down with it, and a fair pool makes the expensive query fail itself.

**A sort or a grouping past the bound spills. A hash join past it is refused**, because DataFusion's hash join does not spill. That asymmetry is in the `USAGE` text for the same reason it is here: it is the difference between a slow query and a failed one.

**Spill goes to the OS temp directory, not to your data directory.** `DiskManagerBuilder::default()` is what `shared_runtime()` passes, and its default mode is the OS temp directory — `TMPDIR`, so `/tmp` on Linux — with a ceiling of 100 GB before DataFusion itself complains. Nothing directs it at `SANKHYA_DATA_DIR`. **If `/tmp` is a small tmpfs, a large sort will fill memory you did not budget; if it shares a volume with anything else, a query can fill it.** Set `TMPDIR` deliberately. The architecture requires spill on a filesystem separate from the write-ahead log and the cache; that separation is not built.

**The Flight door has neither a row cap nor a deadline nor a connection cap.** It streams by design (`FR-API-07`), and its only bound is the shared pool. `crates/sankhya-server/src/flight.rs` does not go through `execute::run`, which is where both the cap and the timer live.

**Nothing else governs a query.** `sankhya-governor` contains admission control, a tenant quota model, a memory brake and a five-rung pressure ladder, all built and property-tested — and `admission::admit`, `assess` and `assess_memory` have **no callers outside their own crate's tests**. The one governor call on the query path is `Quotas::admit` in `crates/sankhya-server/src/wiring.rs`, and it passes a zeroed `Request::default()` against `Quota::generous()`, whose scan, row and storage ceilings are `u64::MAX`. `Quotas::observe` is never called, so the concurrency ceiling can never bind either. There is one tenant, fixed at startup. Read `SNK-R0002`'s entry in `UNREACHABLE` for the same fact stated by the build.

---

## 7. The query log

*"Which statements are slow?"* had no answer anywhere until Phase 5.8. The audit chain records every statement and is the right home for evidence — hash-linked, durable, tamper-evident — and it is the wrong thing to read when a server is slow, because reading it means reading a chain rather than grepping a log, and it carries no duration.

One line per statement now, emitted by `log_statement` in `crates/sankhya-server/src/audit.rs` on both the success and the refusal path:

| Field | Meaning |
|---|---|
| `subject` | Who ran it |
| `tenant` | Which tenant |
| `shape` | The statement's *shape* — see below |
| `scanned` | How many tables the plan touched |
| `row_count` | Rows returned |
| `millis` | How long it took |
| `outcome` | `answered` or `refused` |

Message: `a statement finished`.

**The statement itself is not in it, and neither is a refusal's reason.** A planner's message frequently quotes what the caller typed; that was `SEC-16`. `statement_shape` takes the first word stripped to ASCII letters, and a second word **only** if it is in a closed keyword list — so `create table` logs as `create table` and `select 'a-secret'` logs as `select`. `cargo xtask check-logging` enforces the prohibition on tenant data in any log line, trace attribute or metric label, and there is deliberately no suppression comment: a prohibition with an escape hatch is a prohibition with escapes in it.

There is **no plan hash**. Nothing in this build computes one.

### Where it goes, and how to configure it

To **stderr**, through `tracing_subscriber::fmt()`, as human-readable text. There is no file, no rotation and no JSON: the `json` feature is available in the workspace and the server does not select it. Filtering is `RUST_LOG`, defaulting to `info` — the query log is at `info`, so it is on unless you turn it off.

```bash
RUST_LOG=info                        # the default; query log on
RUST_LOG=warn                        # query log off
RUST_LOG=warn,sankhya_server=info    # this server's events only
```

Colour is conditional on stderr being a terminal, so lines reaching a file or `journald` carry no escape sequences — they did, once, and a test asserting on a field could not see it.

Under systemd this lands in the journal. If you want a file, redirect stderr; there is nothing in the server to point at a path.

---

## 8. Metrics

### 8.1 The endpoint

`GET /metrics` on `server.metrics_listen`, default `127.0.0.1:9464`. One route, matched **whole** rather than by prefix. Anything else gets `404` with the body `only GET /metrics is served here`; a request line over 8 KiB gets `413`. Served by `crates/sankhya-server/src/scrape.rs`, hand-rolled, **unauthenticated and unencrypted by design** — [`SECURITY.md`](SECURITY.md) §3.3 is the honest account of what that costs and what bounds it.

Loopback by default in every case: a metrics endpoint on every interface is a small permanent disclosure of the deployment's shape, and the safe choice should be the one an operator gets by not deciding.

### 8.2 The catalogue is the API

There is no `counter("some_name")` in this codebase. Recording a metric takes the metric's **declaration** — its meaning, unit, group and cardinality bound — so an undeclared metric is not refused at runtime, it cannot be typed. [`METRICS.md`](METRICS.md) is generated from `crates/sankhya-metrics/src/catalogue.rs` and diffed on every build, so a metric absent from that document is not merely undocumented: it is unrecordable.

Two checks run, and they are different checks. That the published catalogue matches the declarations is one. That every declared metric is actually **recorded somewhere in the source** is the other — generating documentation from a catalogue proves the document matches the catalogue and says nothing about whether the catalogue matches the program. Only the second is uncomfortable, because it is the one that fails.

### 8.3 The four that page

A metric that may page carries an `Alert`, and that field is **not** an `Option` (`crates/sankhya-metrics/src/metric.rs`) — so a paging metric structurally cannot exist without a runbook, and the build requires the file to exist *and* carry its *Symptom* / *What is actually wrong* / *What to do* sections. Of fifteen declared metrics, four page:

| Metric | Threshold | Lead time | Runbook |
|---|---|---|---|
| `sankhya_audit_unwritten_total` | above zero | **none** — the first failure is already a gap | [`audit-unwritten`](runbooks/audit-unwritten.md) |
| `sankhya_table_live_files_max` | approaching 1,000 | days, at ordinary write rates | [`compaction-debt`](runbooks/compaction-debt.md) |
| `sankhya_table_live_files` | as above, per table | as above | [`compaction-debt`](runbooks/compaction-debt.md) |
| `sankhya_maintenance_failures_total` | `increase(…[1h]) > 0` | days — file counts climb before a read is slow enough to notice | [`maintenance-stalled`](runbooks/maintenance-stalled.md) |

The interval by which a metric precedes user-visible failure is recorded beside it, because that interval is the entire justification for paging. An alert with no lead time fires when the user notices, which makes it a notification.

**Write the audit rule as two rules.** `sankhya_audit_unwritten_total` reads `0` from the moment the server starts — it has no labels, so the zero series exists before anything has failed, and that is deliberate: a metric that has never been sampled is stored nowhere, and a rule on `> 0` alone would treat *healthy*, *not started yet* and *the scrape is broken* as the same silence. So pair it:

```yaml
- alert: SankhyaAuditUnwritten
  expr: sankhya_audit_unwritten_total > 0
  for: 0m                     # no lead time; the first failure is already a gap
- alert: SankhyaMetricsGone
  expr: absent(sankhya_audit_unwritten_total)
  for: 5m                     # the exporter stopped, so the rule above cannot fire
```

The second rule is what makes the first trustworthy: without it, a server whose metrics endpoint has failed is indistinguishable from one whose audit is healthy. This works for every metric that is unlabelled or carries only restricted labels; `sankhya_table_live_files` is labelled by table, its values are discovered rather than declared, and for it an absent series is the ordinary state of a warehouse with no tables.

### 8.4 The three that are deliberately absent

[`ARCHITECTURE.md`](ARCHITECTURE.md) names four metrics that receive paging alerts. `NOT_YET_EMITTED` in `crates/sankhya-metrics/src/catalogue.rs` records why three of them are not exported: retained log volume (no ingest runs in this process, so no slot retains anything), transaction-identifier freeze age (a property of PostgreSQL, read by a supervisor that is not wired in), and archive jobs awaiting attention (archival is gated).

They are listed rather than declared, and that is the decision worth defending. **A gauge permanently reading zero is indistinguishable from a healthy subsystem.** Publishing three of them would produce a dashboard on which the replication lag is always fine, the freeze age is always young and the archive queue is always empty — for a deployment where none of those things is being measured at all.

### 8.5 Labels, caps, and the series to watch

A label is one of exactly two things: a **closed set** of permitted values, or a **deployment-scoped identifier under a cap**. There is deliberately no third variant, so a label that varies per row, per query or per user has no way to be declared. Putting a value where a dimension belongs is simultaneously the tenant-data leak and the cardinality explosion, and one construct prevents both.

Only one metric is labelled: `sankhya_table_live_files`, on `table`, capped at 200. Past the cap new series are **refused and counted** rather than created, because a gap gets noticed and a quiet inaccuracy does not.

**Watch `sankhya_metrics_rejected_total`.** It is exported at zero on all four of its reasons — `value_not_permitted`, `label_not_declared`, `label_missing`, `over_cap` — so a dashboard can tell *no events* from *not wired up*. Non-zero means either a call site disagrees with the catalogue or something has outgrown its cap.

Memory is counted at the **global allocator** (`crates/sankhya-alloc/src/lib.rs`), not at the query engine's pool. The pool tracks what its operators reserve, which is most of what a query uses and not all of it — decode buffers, network buffers and every third-party allocation sit outside it, and a query can stay inside its reservation and still exhaust the machine. Two atomic loads at scrape time is cheaper than any timer, and a timer would report the previous era.

> **Known defect, recorded rather than hidden.** The table gauges are built from the table set the server resolved, and a dropped table has been observed leaving its series behind, exported at zero. A dropped table leaving a gauge behind is a small thing; the same cause producing a dropped table that still answers queries is not.

---

## 9. The diagnostic

```bash
sankhya-server doctor
```

No flags. It reads the warehouse **directly, off disk, without starting the server** — deliberately, so it works at exactly the moment `start` will not. It authenticates nothing and has no principal; its access control is filesystem permissions.

`FR-OPS-17` asks it to report **time until a problem becomes user-visible** rather than a current value: *"compaction debt is 400 GB"* is far less actionable than *"query latency on this table will double in about nine days"*. The architectural consequence is the part that is easy to build around: **a time cannot be computed from one sample.** It needs a rate, a rate needs observations separated in time, and those need somewhere to live between runs.

So the diagnostic owns an append-only observation history at `<data-dir>/diagnostic-history.tsv` (`crates/sankhya-diagnostic/src/history.rs`), keeping 200 observations per measure. Three properties of it are deliberate: it is **beside** the warehouse rather than inside it, because the warehouse may be the finding; it is **not a table in this system**, because a diagnostic that needs a healthy database to report an unhealthy one is decoration; and it is **text, and damage is expected** — a process killed mid-append leaves a torn line, which is skipped and *counted*, and the count is reported.

**Run it hourly from cron.** The first run reports values and no dates: `Unknown` is a first-class outcome, and the diagnostic names what it is missing — too few observations, a poor fit, a crossing beyond the window — rather than inventing a date. That is uncomfortable on a first run and it is the correct discomfort, because a projection from one sample is a number with a date attached, and a date is precisely what gets believed and scheduled around.

What one run checks (`crates/sankhya-diagnostic/src/check.rs`):

| Check | Objective | Note |
|---|---|---|
| Compaction debt | 1,000 live files before latency suffers | Per table |
| Storage headroom | 10 GiB worth mentioning | Measured by shelling out to `df -P -k` — the workspace forbids `unsafe`, so there is no `statvfs`. This is why the container installs `coreutils`. A reading that cannot be taken is `None` and is said, **never zero** |
| Restore drill | 30 days since the last **pass** | Never the last *attempt* |
| Archive attestation | 90 days | Gated on there being an archive, which today is a hard-coded `false` because tiering is gated |

**Findings are ordered by *when*, not by severity.** Severity orders a list by how loudly each item shouts; time orders it by which must be dealt with first, and those are different orders. An operator reading top-down should be reading a schedule. A separate *"Could not run:"* block follows, and the exit status distinguishes it.

`check::replication_lag` exists, is exported, and is **called by nothing** — no caller passes it a lag trend, because nothing produces one.

---

## 10. Maintenance

One thread inside the server (`crates/sankhya-maintenance/src/service.rs`), started at boot and stopped by dropping its handle. Nothing outside the server drives it and no client needs to know it exists. It shipped as a library only tests called, so a running server accumulated files for as long as it was up; that is fixed and worth knowing as the shape of the bug class.

### 10.1 The settings

Three keys, in `maintenance`. There are **no environment variables** for maintenance.

| Key | Default | Meaning |
|---|---|---|
| `interval` | `30s` | Tick rate. A pass is bounded work, so this is the rate the warehouse catches up at, not how long it spends. Suffixes `s`, `m`, `h`. **`0` disables maintenance entirely**, for the one honest case: another process is doing it |
| `compact_every` | `1` | Ticks between compaction passes. Every tick, because a pass is already bounded and a partition below the merge thresholds costs only the decision not to merge it |
| `orphan_sweep_every` | `120` | Ticks between sweeps for files the log does not name — an hour at the default interval. A sweep walks the whole table directory, returns nothing most of the time, and what it collects is not urgent |

Everything else is a compiled default: a 24-tick retirement grace and a 2,400-tick leak backstop (`crates/sankhya-maintenance/src/execute.rs`), a one-week minimum age before a file counts as an orphan (`crates/sankhya-maintenance/src/orphans.rs`), and a 10,000-tick duty cycle. **`duty_cycle_ticks` is warehouse-wide and is not exposed as a configuration key at all**; there is no per-table maintenance setting.

An unparseable value stops startup rather than defaulting.

### 10.2 What a tick does, and why retirement lags

Compact if due → execute the merge → commit → checkpoint if due → **retire completed merges from a pending queue, one tick behind.**

The lag is the design. A merge's `remove` actions hide files from new readers and do not delete them, because a reader that listed files a moment before the merge is entitled to open them and has no way to announce that it is doing so. Deleting the inputs is a separate operation with three preconditions, all of which must hold: the replacement verifies by re-reading its row count from its footer at retirement time, not trusting it from the merge; no retained snapshot, lease or clone can resolve to the input; and the grace period has elapsed. An input failing any of them is **retained with a reason**, which is a correct outcome — retirement is an optimisation and declining it costs only disk. The one case that is an error is a missing or short replacement, which means the compaction did not happen and nothing may be removed at all.

There is deliberately **no command that compacts by hand.** The server holds the warehouse lock and is the only maintainer; a second process compacting the same tables is exactly what `cargo xtask check-writers` exists to stop. The lever is `maintenance.compact_every` and a `SIGHUP`.

### 10.3 What you can see of it

Four of the six are now exported. The maintenance handle carries counters — ticks, bytes reclaimed, ticks that declined to reclaim, failures, tables being maintained, merges awaiting retirement — and until 2026-09-06 **the server read none of them**: they were neither exported as metrics nor logged, which is why the compaction-debt page could tell you the duty cycle was too low with nothing to check that against. Ticks, bytes reclaimed, declines and failures are published on the maintenance cadence (§8.3). **Tables being maintained and merges awaiting retirement still are not**, and the first of those is the one an operator asks for by name — *"is my new table being maintained?"* — so it remains a gap rather than a completed sentence.

The observable surface is `sankhya_table_live_files_max` and, behind `server.metrics_detail`, the per-table `sankhya_table_live_files`; plus `sankhya-server doctor`, and the structured events the maintenance crate emits when a table is adopted, released, or starts and stops failing. A table whose compaction failed every thirty seconds used to be invisible while the aggregate reclaimed-bytes figure climbed from the other tables; failures are counted and reported on change now — once when they start and once when they stop.

---

## 11. Backup, and restoring from one

This is the section the durability chapter this document harvested did not contain, despite its title. Read §11.1 before anything else in it.

### 11.1 A backup is a manifest. It copies no data.

`sankhya-server backup` writes **one JSON file** — `<data-dir>/backup-manifest.json` — and nothing else. It does not copy a Parquet file, it does not touch your transactional store, and it does not archive anything anywhere.

What it does is **bind three artefacts to one point** and record what each was, so that three backups which do not agree with each other cannot be mistaken for one that restores:

- the transactional backup **you** took, by location and digest;
- the table versions in the warehouse, with row counts and checksums;
- the key generation.

The manifest is `crates/sankhya-backup/src/manifest.rs`. Its fields: `format`, `id` (a UUIDv7, shown as `backup:<uuid>`), `taken_at`, `source_restores_to`, `queryable_at`, `source` (location, restore position, artefact digest), `tables` (each with `table`, `version`, `covers_to`, `rows`, `checksum`, and `cloned_from` where it is a clone), `keys`, and `protect_until`. Tables are sorted by name so two manifests of the same state are byte-identical.

`Manifest::bind` **refuses** rather than recording a backup it can prove is broken, in three cases:

- **No tables.**
- **A table covering a position past where the source restores to.** After a restore the analytical tier would hold rows the transactional store no longer has; capture resumes behind them and republishes that range at different positions, so those rows arrive a second time under different identity — or sit there permanently as data with no origin. It is `SNK-S0002`'s shape one layer up and it is **not detectable afterwards from either side alone**, which is why it is checked when the manifest is built rather than when it is used. Every offending table is named, not just the first: fixing them one at a time means learning about the next only after another full backup.
- **A clone whose origin is not in the backup.** A clone's own log names *none* of its origin's files, so restoring it alone produces a table that is present, readable and **empty**, with nothing about it looking broken. A chain holds without the check knowing it is a chain: `a → b → c` binds when all three are present and is refused when the **middle** is absent.

> **Three things in this build are placeholders, and an operator must know which.** `covers_to` is written as `0` for every table, `source_restores_to` is written as `0`, and therefore `queryable_at` is always `0`. `keys` is hard-coded. `protect_until` is a hard-coded 90 days. With the environment unset, the source location and digest are the literal string `"unrecorded"`. So the *checksum* half of the manifest is real and verified; the *position* half is not yet, and the position check above cannot fire.

> **And one mechanism is unwired.** The seven-day protection that is supposed to keep a deleted backup's files sweepable-only-after-a-grace (`crates/sankhya-backup/src/protect.rs`) has **no caller outside its own crate**. Taking a backup does not pin its files against the orphan sweeper or against retirement. Drill soon after taking one, and keep a real copy — see §11.3.

Note also: `backup-manifest.json` is a **single file at a fixed name, overwritten on every run.** Taking a second backup destroys the first manifest. Copy it aside if you need to keep it.

### 11.2 Taking a backup

```bash
SANKHYA_WAREHOUSE=/var/lib/sankhya/warehouse \
SANKHYA_DATA_DIR=/var/lib/sankhya/.sankhya \
SANKHYA_SOURCE_BACKUP=s3://backups/pg-2026-09-06.dump \
SANKHYA_SOURCE_DIGEST=sha256:… \
sankhya-server backup
```

```
SANKHYA backup 0.1.0
  common.orders at version 5, 1000 row(s)
  common.regions at version 1, 250 row(s)
  …
  backup:01a05fd9-53ee-7723-a129-85da1719e9bd
  queryable at 0
  manifest /var/lib/sankhya/.sankhya/backup-manifest.json

This backup is unproven until it has been drilled: `sankhya-server drill`.
```

That last line is not decoration. Until a drill has passed, the manifest is a list of filenames somebody wrote down.

> A clone's own row count is genuinely zero, and the drill genuinely verifies zero rows for it. Its rows are covered by its origin's entry. **Read a clone's line as "nothing of its own", not "nothing at all"** — it reads like a warning and is not one.

### 11.3 Copying the bytes, which is yours to do

Because a manifest is not an archive, the copy is your job and the manifest is what proves the copy is good. Copy, with a tool that preserves the tree:

| Copy | Why |
|---|---|
| The whole **warehouse root** — every `<schema>/<table>/` including `_delta_log/`, plus `_snapshots/`, `_cubes/`, `_audit/` | A table directory is the unit of external readability; a clone is only complete alongside its origin, so copy the root and not a subset |
| `<data-dir>/backup-manifest.json` | Without it there is nothing to verify against |
| `<data-dir>/restore-drills.jsonl` and `attestations.log` | Evidence; append-only |
| The configuration directory | `application.yaml` carries the policy, the credentials and the TLS paths |
| Your transactional dump | SANKHYA neither takes nor verifies it. [`POSTGRES.md`](POSTGRES.md) |

Do **not** copy, and delete if you did: `cache/`, `spill/`, and `warehouse.lock`. The first two are regenerable; the third will refuse a start.

### 11.4 The drill, and what it actually proves

```bash
sankhya-server drill
```

It recomputes, table by table, the row count and the digest the manifest recorded, and compares. Reading the data back rather than listing files is the whole point: **a file-presence check passes on a truncated Parquet.** It passes on a file whose bytes were replaced with another table's. It passes on essentially every failure that actually occurs, because what goes wrong with a backup is almost never that a file is missing — a missing file is loud, and something notices. What goes wrong is that a file is there and wrong.

Both sides compute that digest through **one** implementation. Two would eventually differ on a null convention, a value rendering or a column order; every drill would then fail on data that is perfectly fine; and after the third false alarm the drills would stop being run. A verification that cries wolf consumes the attention a real failure needs.

A failure names its kind, because the two need different investigations. **A row count that matches with a different checksum means rows were altered** — every file present and the right length, which points at a writer that touched a frozen version. **A different row count means rows were lost or duplicated**, which points at retention or a restore.

> **The drill reads the warehouse the environment points at.** `run_drill` in `crates/sankhya-server/src/backup.rs` opens `Warehouse::at(warehouse)` — so run against a live server's warehouse it re-verifies the live warehouse in place, and run with `SANKHYA_WAREHOUSE` and `SANKHYA_DATA_DIR` pointing at a restored copy it verifies **the copy**. That second form is the one §11.5 uses, and it is the only verification of a restore this system offers.

Schedule it. Weekly is the shape the runbook assumes:

```cron
23 3 * * 0 /usr/local/bin/sankhya-server drill
```

### 11.5 Restoring — the procedure

There is **no restore command.** Nothing in this repository copies a backup back into place; `sankhya-cli` is a stub. Restoring is an operator procedure, and this is it. Every step is checkable against the code cited beside it.

**0 — Before you need it, prove the copy restores.** Untar last night's copy into a scratch path on the same host and run step 5 against it. That is a rehearsal of the whole of this section and it is the only way to learn that step 5 fails before you need step 5 to pass.

**1 — Stop the server.** `SIGTERM`, and wait. The drain is up to 30 seconds (§4.5). Do not `SIGKILL`: crash consistency is a real requirement and is met, but there is no reason to spend it.

**2 — Restore the transactional store first, by its own tooling.** `pg_restore`, `pg_basebackup`, or your platform's. SANKHYA does not do this and does not verify it. The manifest names the artefact you took, by location and digest; check that the thing you are restoring is the thing the manifest names. [`POSTGRES.md`](POSTGRES.md) records that the on-disk format is stock and unmodified, so this is an ordinary PostgreSQL restore.

**3 — Restore the warehouse tree** to the warehouse path, whole. Not a subset — a clone splices its origin's live set and restoring it without its origin gives you a table that is present, readable and empty.

**4 — Restore `backup-manifest.json`** into the data directory, and remove regenerable state: delete `cache/`, `spill/`, and `warehouse.lock` if a previous process died holding it. Leave `diagnostic-history.tsv` and the evidence files if you have them — a history is what makes the next `doctor` run able to project anything.

**5 — Prove it before serving it.**

```bash
SANKHYA_WAREHOUSE=/var/lib/sankhya/warehouse \
SANKHYA_DATA_DIR=/var/lib/sankhya/.sankhya \
sankhya-server drill
```

| Exit | Meaning | Do |
|---|---|---|
| `0` | Every table's rows and checksum match the manifest | Continue |
| `1` | Named tables did not verify | **Stop.** Read which kind of failure (§11.4); the restore is not complete |
| `2` | The drill could not run | **Stop.** Nothing was proven. This is not a pass |

**6 — Run the diagnostic**, which reads the warehouse without starting the server:

```bash
sankhya-server doctor
```

Expect a compaction-debt finding if the copy is old, and expect `Unknown` projections until the history has two observations.

**7 — Start**, and read the startup line (§4.4). It names the table count, the policy posture and the audit chain head. **If the table count is lower than it was, a table failed to open** and is named on stderr — a server that starts with three tables of four looks, to whoever queries it, like a table nobody ever created.

**8 — Reconcile the two tiers.** This build cannot do it for you: `queryable_at` is a placeholder, so the manifest cannot tell you how far the analytical copy has to be re-captured to catch the transactional store. With no capture running there is nothing to resume, and the analytical tier is as of the copy you restored. Record that gap explicitly for whoever reads the data; do not let it be inferred.

**What this procedure does not give you**, stated plainly rather than left to be discovered: no point-in-time recovery of the warehouse, no partial restore of one table, no automated verification that the two tiers agree, and no rollback if step 3 was wrong. Keep the previous copy until step 7 has passed.

### 11.6 Attestation, which proves a different claim

A drill proves you can read data back. An **attestation** proves a write-once store still refuses to change what it holds — a different claim, and one that decays *without anything touching your system*. A retention policy is replaced, a lifecycle rule is added, a bucket is recreated from a template, and the control is gone while every configuration readout still says it is there.

It works by **trying to break the archive**: it writes a probe object, then attempts to overwrite, delete and truncate it, and requires every one to be refused. Reading a configuration flag would pass in exactly the case this exists to catch.

Which is why it refuses without a `_non_production` marker **inside the archive**:

```
$ sankhya-server attest <path>
SANKHYA archive attestation 0.1.0

NOT ATTEMPTED. this store is not declared non-production. An attestation attempts the violations it
is checking for, so against real data a missing control means the drill itself inflicts the damage
```

The marker lives in the archive rather than on the command line on purpose: a `--non-production` flag survives in a runbook that gets copied, and the copy eventually runs somewhere it should not.

Exit `0` attested, `1` the store allowed something it must refuse, `2` nothing was attempted. **`2` is not a pass**: a write that failed because the path was wrong or credentials were missing has demonstrated nothing about immutability, and recording it as a refusal would let a broken drill certify a store it never touched.

### 11.7 Evidence

Append-only, under the data directory, and a failure is written with the same ceremony as a pass — a history with no failures across three years describes either a very good system or a drill that does not really run, and nothing in the history distinguishes them.

| File | Contents |
|---|---|
| `restore-drills.jsonl` | One line per drill: `at`, `backup`, `verdict` (`pass` / `FAIL` / `could-not-start`), `tables`, and the failures or the reason |
| `attestations.log` | One line per attestation |
| `diagnostic-history.tsv` | The diagnostic's observations (§9) |

*"When did we last prove we could restore?"* is answered with the last **pass**, never the last attempt. `could-not-start` is recorded distinctly for the same reason `doctor` exits `2` distinctly.

---

## 12. Snapshots, pins and leases

A named snapshot pins table versions so a reader can come back to them (`crates/sankhya-snapshot/src/expire.rs`):

```sql
CREATE SNAPSHOT q3_close EXPIRE AFTER 90 DAYS;
SHOW SNAPSHOTS;
DROP SNAPSHOT q3_close;
```

**Every snapshot expires.** There is no `EXPIRE NEVER` and no default: zero days is refused as immediate, and the ceiling is 730 days. Expiry **detaches a pin and deletes nothing**; retirement reclaims afterwards on its own grace period. Reading through an expired snapshot is **refused**, never silently answered from the present, and so is naming a table the snapshot does not cover.

Documents live in `<warehouse>/_snapshots/`. Note that `SET SNAPSHOT` — actually reading as of one — is **not built**, and is refused rather than accepted as a no-op (`crates/sankhya-server/src/snapshots.rs`).

Underneath, readers announce themselves through an epoch-based lease registry (`crates/sankhya-leases/src/lib.rs`): a reader pins before it resolves, and a sweeper asks whether every reader that pinned before a mark has finished. Every imprecision in it is arranged to **delay** reclamation and never to permit it early — a reader that could not announce makes the registry report that something is active, and a slot collision reports the older announcement.

Two rules from [`INVARIANTS.md`](TESTING.md) matter operationally: **a pin that cannot be read stops reclamation**, because contributing nothing is indistinguishable from protecting nothing; and a leaked lease does not stop reclamation for ever, because a registry with a leak and no backstop reclaims nothing, for ever, and says nothing about why. The backstop overrides the *lease* check only — a clone or snapshot pin still refuses.

---

## 13. The transactional tier

Not wired in. `sankhya-oltp-pg` supervises a cluster, is built and tested against a vendored PostgreSQL 17.11, and is on `UNREACHED` with `M8 §12.2` named against it: `Settings` has no transactional-store configuration. `sankhya-cdc-pg` is the same story for change capture.

So a running SANKHYA today serves a published warehouse and nothing supervises a database beside it. What SANKHYA *will* do to a PostgreSQL cluster, and the settings it needs — above all `max_slot_wal_keep_size`, which is the setting that prevents an analytical query from taking down the transactional store — is [`POSTGRES.md`](POSTGRES.md), which marks each item built, applied or designed.

---

## 14. What is on disk

The **warehouse root** contains only externally-meaningful published tables: `<schema>/<table>/`, each a directory of Parquet under `sank_data_date=YYYY-MM-DD/` partitions with its own `_delta_log/`. A foreign object appearing under it is detected at startup and refused rather than ignored. Underscore-prefixed directories — `_snapshots/`, `_cubes/`, `_audit/` — are SANKHYA's own and are skipped by table discovery.

The **data directory** (`SANKHYA_DATA_DIR`, default `<warehouse>/../.sankhya`) holds everything else this build writes:

| Path | Class | Contents |
|---|---|---|
| `warehouse.lock` | — | The exclusive one-server-per-warehouse lock |
| `backup-manifest.json` | durable | The most recent manifest, overwritten each run |
| `restore-drills.jsonl`, `attestations.log` | durable | Evidence |
| `diagnostic-history.tsv` | durable | The diagnostic's observations |

The architecture specifies more under this root — a `pg/` data directory, a write-ahead-log archive, capture state, a staging tier, a cache, a graph epoch cache, a key store — and none of it is written by this build, because the subsystems that would write it do not run. **Spill does not live here**; it goes to the OS temp directory (§6).

The one thing the filesystem must provide is `link(2)`: a commit claims its version by creating a link that fails when the name is taken, and **that refusal is the whole of the protocol's concurrency control**. `rename(2)` cannot provide it, because it replaces its destination silently — which is exactly what this code did until `M8`, so two committers could both see a version free and the second would overwrite the first with no error to either. ext4, xfs, btrfs, zfs, APFS and NTFS support hard links; **FAT and exFAT do not and are not supported.** Some network filesystems implement `link` unreliably, and the failure there is at least loud.

---

## 15. Runbooks

One per alert that can page, and the relationship is enforced rather than aspirational: a metric declaring an `Alert` names a runbook stem, and the build requires the file to exist and to carry its sections.

| Runbook | Trigger | In one line |
|---|---|---|
| [`audit-unwritten`](runbooks/audit-unwritten.md) | `sankhya_audit_unwritten_total` above zero | Records are being made and are not reaching disk. **No lead time — the first failure is already a gap**, and nothing a user sees changes |
| [`compaction-debt`](runbooks/compaction-debt.md) | `sankhya_table_live_files_max` near 1,000 | One table's queries get slower and nothing else on the box looks different. Lower `maintenance.compact_every` or `interval` and `SIGHUP` |
| [`maintenance-stalled`](runbooks/maintenance-stalled.md) | `increase(sankhya_maintenance_failures_total[1h]) > 0` | Cycles are running and at least one table is failing, so files accumulate unopposed. Separates a dead maintainer, a failing pass, a sweeper that cannot establish the pin set, and a duty cycle that is simply too low — four states that all read as "file counts are rising" |
| [`restore-drill`](runbooks/restore-drill.md) | `doctor` reports no passing drill, a stale one, or a drill exited `1` | Nothing is broken; what is wrong is epistemic. Distinguishes *never ran* from *ran and failed* from *the backup is not a backup*, and forbids taking a fresh backup to silence the alert |
| [`snk-s0001`](runbooks/snk-s0001.md) | `SNK-S0001` | Coverage gap: a query is refused because no tier covers part of the range it asked for. Fatal, and intermittent-looking |
| [`snk-s0002`](runbooks/snk-s0002.md) | `SNK-S0002` | Archive conflict: the registry says a range was purged and the catalogue says those rows are present. Every query on that table is refused until a human acts |
| [`snk-s0003`](runbooks/snk-s0003.md) | `SNK-S0003` | Archive verification failed. Terminal, no automatic retry; if it fired during a purge, nothing was removed |
| [`snk-s0004`](runbooks/snk-s0004.md) | `SNK-S0004` | Source endangered. **This is the system working** — the analytical tier is being sacrificed to protect the transactional store |
| [`snk-s0005`](runbooks/snk-s0005.md) | `SNK-S0005` | An invariant does not hold. This is a defect in SANKHYA, not a misconfiguration |
| [`snk-s0006`](runbooks/snk-s0006.md) | `SNK-S0006` | Configuration invalid; the process refuses to start, naming the key, the value and where the value came from |

> **Four of those ten name a code no query in this build can raise.** `SNK-S0001` through `SNK-S0004`, and the reason differs by code. `SNK-S0003` and `SNK-S0004` are on `UNREACHABLE` in `xtask/src/catalogues.rs`: nothing constructs them, because backup verification reports through its own types and an endangered source is the change-capture runtime. `SNK-S0001` and `SNK-S0002` are on `MAPPED_BUT_UNREACHABLE`, which is a different statement — the conversion onto the code exists and is tested (a `SpliceError::CoverageGap` reports as `SNK-S0001`, a refused reconciliation as `SNK-S0002`) and no call path in this build meets the condition. **An alert rule written against any of the four is permanently silent either way**, which is why they are listed rather than left to be discovered from an alert that never fires. The runbooks are kept because codes are permanent — removing one breaks every rule that references it — and [`ERRORS.md`](ERRORS.md) marks each, with the gap that has to close first.

Both errors this section used to list against `runbooks/compaction-debt.md` are fixed: it named a `sankhya` binary that does not exist, and said the diagnostic *authenticates*, which it does not — it reads the warehouse off disk with no principal, and what protects it is shell access to the host.

---

## 16. Errors

Every error that reaches a client carries a code, a class, a SQLSTATE and a remediation, and one classification drives six behaviours: retry policy, protocol status, SQL state, log level, metric labelling and alerting. [`ERRORS.md`](ERRORS.md) is generated from the catalogue and is the reference.

One distinction matters for alert rules. **`refused` is not `error`.** A quota held and a permission enforced are the system working, and counting them with genuine failures makes a healthy system under load indistinguishable from a broken one — which is how an error-rate alert comes to fire on correct behaviour. The four outcomes on `sankhya_queries_total` are `ok`, `error`, `refused` and `cancelled`, and `refused` is derived from the SQLSTATE class: `28000`, `28P01`, `42501`, `53200`, `53400`. A malformed statement is an `error`, which is right — a typo is the caller's fault, not the system holding a limit.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
