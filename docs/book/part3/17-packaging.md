# 17. Packaging and deployment

> This chapter covers what you install, where it runs, and how it is configured. Its central claim
> is that **the number of build targets is the number of things that can silently break**, and that
> a declared platform baseline nobody checks is a baseline nobody meets — a binary built on a
> current distribution silently acquires that distribution's symbol versions, links, runs and tests
> clean, and fails the first time a customer on an enterprise distribution starts it. Nothing on the
> build machine can surface that by construction, so the check reads what the binary *requires*
> rather than what the build intended. This build misses its own declared baseline by six versions,
> and says so.

---

## 17.1 One binary, three roles

There is one artifact, one configuration schema, and one process per node. The role is a
configuration value, not a build variant:

Role | Owns | Cardinality | Recovery
---|---|---|---
**Coordinator** | The transactional connection, the change applier, the maintenance scheduler, the archive engine, the catalog | Exactly one active | Leader election; database failover
**Executor** | Nothing durable — caches only | Many | Trivial; any node serves any query
**Graph** | Hydrated in-memory graph epochs | Partitioned by tenant | Rebuild from published tables

> **Key idea** — The honest statement about the single-binary constraint. There is no configuration
> in which several nodes share writable state with zero coordination. Either the object store is the
> coordinator, through atomic conditional writes, or the transactional database is. The constraint is
> satisfied in **packaging** — one artifact, one configuration file, one process per node — and it
> cannot be satisfied in **topology**, where a multi-writer cluster has exactly one logical
> coordinator by definition. SANKHYA's answer is that the coordinator is a *role of the same binary*.

Two independent lines of analysis arrived at this: commit serialisation for the table format, and
coordination of maintenance jobs. Convergence from unrelated directions is good evidence the
conclusion is correct.

Shape | Transactional tier | Nodes | Intended use
---|---|---|---
**Solo** | Managed child process | 1, library-embedded | Development, test, edge, single-user analysis
**Node** | Managed child process | 1, with listeners | Small deployments, appliances
**Cluster** | Attached, externally managed | Coordinator + N executors + graph nodes | Production at scale

**Managed mode is single-node.** That is a documented product boundary rather than a defect: a
highly available multi-node deployment requires a highly available transactional tier, which means
an externally managed cluster. Solo must require no network listener at all.

Of the three, only **Solo** and **Node** are reachable today, and multi-node is `M12` — leader
election, executor scale-out, failover and replication all need a second machine (Chapter 16,
*Backup, restore and disaster*, §16.10).

## 17.2 Supervision, and the three ways a supervised database goes wrong

*Embedded* here means a supervised child process, not a library linked into the address space.
SANKHYA carries PostgreSQL's source, verified by checksum, and builds it into a private prefix;
nothing is installed system-wide and no existing PostgreSQL is touched.

Startup is an explicit, observable sequence: acquire an exclusive lock on the data directory; verify
and extract embedded assets against their recorded checksums; initialise the database if absent, or
validate its catalog version; start the database and wait for readiness with bounded backoff; run
schema migrations; recover the change-capture position; open listeners; report ready.

Three failure modes are handled explicitly because each is fatal if missed:

- **Two processes over one data directory.** Prevented by the directory lock, taken before anything
  else.
- **An orphaned database process.** At startup, if a process record exists: adopt it if live and
  healthy, stop and restart it if live and unhealthy, clear the record if stale. Getting this wrong
  produces either corruption or a boot loop.
- **A supervisor that dies leaving its child running.** Parent-death signalling where the platform
  supports it, *plus* the boot-time check above, because signalling does not survive every
  termination path.

PostgreSQL 17 or later is required, and the reason is failover-capable logical replication slots.
Without them a routine database failover destroys the slot and forces a full re-snapshot of every
replicated table — a multi-hour outage of the analytical tier triggered by an ordinary availability
event.

The supervisor is **built and tested** against the vendored 17.11 and **is not wired into the
server**: `Settings` has no transactional-store configuration. It sits on the workspace's `UNREACHED`
list with `M8 §12.2` named against it, beside leader election, which is what will need a running
store.

## 17.3 One root, and three classifications of what is under it

```
${SANKHYA_DATA}/
  sankhya.lock          exclusive directory lock
  version.json          binary version, schema versions, asset checksums
  pg/                   database data directory and its socket
  pg-bin/               extracted database binaries + checksum stamp
  pg-bin-prev/          previous major, retained for in-place upgrade
  wal-archive/          point-in-time recovery archive
  cdc/                  slot state, checkpoints, dead-letter spill
  staging/              un-merged change log (NOT the published warehouse)
  cache/                object cache, footer cache        [regenerable]
  spill/                query spill files                 [regenerable]
  graph/                serialized epochs for fast rehydration [regenerable]
  audit/                local audit spool before shipping
  keys/                 wrapped data keys only, never raw material [secret]
  logs/
  tmp/
```

Each area is classified **durable**, **regenerable** or **secret**, and the classification drives
backup scope, disaster-recovery design and container volume layout. Regenerable areas are cleared on
boot after an unclean shutdown.

**The published warehouse is not under this root** when object storage is in use, and is
conceptually separate even when it is local. A warehouse is `<schema>/<table>/`, each table a
directory of Parquet under date partitions with its own `_delta_log` — Chapter 6, *Storage and the
open table log*, specifies it.

## 17.4 Configuration, and a setting that fails the load

Precedence, highest first:

1. `--key=value` on the command line
2. an environment variable
3. `config/application.local.yaml` — git-ignored, for what a machine needs and must not commit
4. `config/application.yaml`

`${NAME}` refers to another setting, an environment variable or an argument; `${NAME:default}` makes
the reference optional. **An unresolved reference with no default fails the load** rather than
reaching a connection string as a literal `${NAME}`.

**An unknown key is an error rather than a warning** (`SNK-S0006`), because silently ignored typos
are a leading cause of production incidents. So is a value that does not parse:

```
$ sankhya-server doctor
sankhya: `warehouse.read_as_of` must be an integer and holds `18446744073709551615` —
`warehouse.read_as_of` came from a configuration file (config/application.yaml). Refused rather than
defaulted: a setting that silently becomes something else is a deployment behaving as though it were
configured when it is not
```

That refusal is exactly right and it is also, verified in this session, what the **shipped
`config/application.yaml` in the repository root produces**: its `read_as_of: 18446744073709551615`
is `u64::MAX`, and the loader parses the setting as a signed integer. Any subcommand run from the
repository root without `SANKHYA_CONFIG` pointing elsewhere refuses to start. It is a one-character
defect in a default file, and it is the kind that a test using its own fixture cannot see — the same
shape as four defects an adversarial review found the day this was written.

The environment variables the server reads:

Variable | Meaning
---|---
`SANKHYA_CONFIG` | Path to the configuration file
`SANKHYA_WAREHOUSE` | Root of `<schema>/<table>/`
`SANKHYA_DATA_DIR` | The root in §17.3
`SANKHYA_LISTEN` | Wire-protocol address, default `127.0.0.1:5433`
`SANKHYA_METRICS_LISTEN` | Metrics address, default `127.0.0.1:9464`
`SANKHYA_NO_PASSWORD` | Spelled as an opt-**out**, so the insecure choice is deliberate. Leaving it unset makes a password *demanded*; it is *verified* only where `server.credentials` names the user. An empty list authenticates nobody, and the startup line capitalises that
`SANKHYA_USER_FUNCTIONS` | Whether `CREATE AGGREGATION` is accepted. Off by default: it runs code the caller supplied
`SANKHYA_READ_AS_OF` | The position to read as of
`SANKHYA_FEED_INTERVAL_SECONDS` | How often a feed looks at its spool; 30 by default
`SANKHYA_SOURCE_BACKUP`, `SANKHYA_SOURCE_DIGEST` | The transactional backup a manifest binds to

Listeners are loopback by default in every case. A metrics endpoint on every interface is a small
permanent disclosure of the deployment's shape, and the safe choice should be the one an operator
gets by not deciding.

## 17.5 The startup line is the deployment's self-description

Everything a person needs to know about how this process is exposed is printed once, in words, on
every start. From the review server this book was written against:

```
SANKHYA 0.1.0
  tenant tenant:00000000-0000-0000-0000-000000000001, NO AUTHENTICATION — every connection is
  accepted, 10 policy rule(s), 10 table(s) known
  listening on 127.0.0.1:55432
  wire protocol unencrypted — passwords cross the network in plain text
  audit chain head 0000…0000 (0 record(s))
  connect with: psql -h 127.0.0.1 -p 55432 -U <user>
  metrics on http://127.0.0.1:55433/metrics
  maintaining 10 table(s) every 30s, compacting every 1 tick(s), sweeping every 120
  Arrow Flight SQL on 127.0.0.1:5434
```

Four things are load-bearing there. **`NO AUTHENTICATION` is in capitals**, because
`SANKHYA_NO_PASSWORD` is an opt-out and an operator should see the consequence rather than have to
check. **The TLS posture is named in words** — one of `unencrypted`, `TLS offered`, `TLS required`,
`TLS required, and a client certificate with it` — every time, so a half-configured server cannot be
mistaken for an encrypted one (Chapter 13, *Security, tenancy and policy*, §13.6). **The maintenance
settings are printed**, so a deployment that has disabled maintenance says so rather than quietly
accumulating files. And **the bound address is printed, not the configured one** — told to bind port
0 this once printed `:0`, so the line whose only job is to say where to connect said nothing.

A table the server cannot open is named on stderr rather than omitted. A server that starts with
three tables of four and says nothing produces an outage that looks, to whoever queries it, like a
table nobody ever created.

## 17.6 Platform baselines, and the check that is the whole point

Bundled database binaries are dynamically linked, so a fully static artifact is not achievable for
the self-contained bundle. The alternative is a **declared platform baseline**: the oldest system the
artifact runs on, expressed as a maximum symbol version and a set of shared objects.

Platform | Target | Support | Baseline | Published as
---|---|---|---|---
Linux (x86-64) | `x86_64-unknown-linux-gnu` | **server** | `GLIBC_2.28` | tarball, rpm, deb
Linux (ARM64) | `aarch64-unknown-linux-gnu` | **server** | `GLIBC_2.28` | tarball, rpm, deb
Linux (x86-64, static) | `x86_64-unknown-linux-musl` | **server** | static (musl) | tarball
macOS (Apple silicon) | `aarch64-apple-darwin` | **server** | macOS 12.0 | tarball
Windows | `x86_64-pc-windows-msvc` | client only | — | —

`GLIBC_2.28` is RHEL 8 and Debian 10 — the oldest an enterprise is plausibly still running. The
static musl artifact is the one that *downloads* database binaries rather than bundling them, and it
is only achievable because the thing that cannot be static, PostgreSQL, is not in it.

> **Key idea** — The declaration is not the interesting part; the check is. A binary built on a
> current distribution silently acquires symbol-version requirements from it. The symbols are present
> locally, so it links, runs and tests clean, and the failure appears the first time a customer on an
> enterprise distribution tries to start it. **Nothing on the build machine can surface this by
> construction** — the machine is the reason it happens. So `cargo xtask check-package` reads what
> the binary *requires*, and fails only on a release build, because a check that fails every local
> build is a check everybody learns to ignore.

**This build requires `GLIBC_2.34`.** It would not start on RHEL 8, Ubuntu 20.04 or anything older
than RHEL 9. That gap is recorded rather than papered over by lowering the declared baseline to
whatever the build machine produces, which would quietly drop every enterprise distribution. Meeting
it needs a build against an old sysroot, which is release-pipeline work.

Three ways to hit an older baseline, and they are not equivalent:

Method | What it covers
---|---
`cargo-zigbuild` | Targets a chosen glibc directly, no container, no sysroot. The simplest answer for the Rust half
A build container or sysroot | The only answer for the **bundled PostgreSQL**, which is a C build and acquires its baseline the same way
musl, statically linked | Removes the question entirely, and only for the artifact that does not bundle PostgreSQL

**So the baseline of the self-contained artifact is set by PostgreSQL, not by the Rust binary.** That
is worth stating plainly, because tuning the Rust build alone and declaring victory is the obvious
mistake.

> **Pitfall** — The check was itself broken before it caught anything. `highest_glibc` matched a
> `GLIBC_` prefix against the whole symbol token, but `readelf` writes `statx@GLIBC_2.28` — so it
> matched nothing, found no requirements, and concluded every requirement was met. It passed the real
> binary against a baseline it misses by six versions. The unit test written against genuine
> `readelf` output is the only reason that did not ship, and it is why the parser is tested on real
> output rather than on a convenient sketch.

### What a filesystem must provide

**Hard links.** A commit claims its version with `link(2)`, which fails when the name is taken, and
that refusal is the whole of the protocol's concurrency control. ext4, xfs, btrfs, zfs, APFS and NTFS
support them. **FAT and exFAT do not, and are not supported.** Object stores need the equivalent
primitive — a conditional put — and a store that does not offer one cannot host a warehouse safely.
Chapter 16 §16.9 gives the reasoning.

### Windows, checked rather than assumed

The objection expected to be fatal — a case-insensitive filesystem where `Orders` and `orders`
collide — is already handled: every path segment is case-folded to lower-case ASCII, digits and
underscores, collisions are refused rather than disambiguated, and the platform device names (`aux`,
`con`, `nul`, `com1`…`lpt9`) are already reserved. **A warehouse is already Windows-path-safe.** What
is missing is the vendored PostgreSQL build and service integration, which is porting work rather
than a design problem. So the honest row is *client only*: any PostgreSQL driver connects to a
SANKHYA from Windows today, which is what most Windows users actually need, and the server runs under
WSL2 or a container until somebody builds it.

## 17.7 One artifact, or one per distribution

Both, answering different questions. A **tarball built at the oldest baseline** is one file that runs
everywhere newer, which is what an air-gapped install needs. **Native packages** integrate with the
distribution — the service unit, the user, the upgrade path — at the cost of a build and a test per
distribution. The matrix in §17.6 is what keeps that cost visible: five targets declared once in the
packaging source and generated into [`PLATFORMS.md`](../../PLATFORMS.md), rather than a script per
platform drifting from its siblings until one artifact behaves unlike the rest for a reason nobody
can find.

The termination grace in every deployment manifest is compared against the server's drain deadline by
the same check, for the reason Chapter 16 §16.8 gives. That comparison was correct and exercised by
nothing until a mutation shortening the Kubernetes grace below the drain **survived**: it lived only
in a command, and a check that is only a command is a check that is only sometimes made. It is a test
as well now.

## 17.8 The command surface

Subcommand | Effect | Exit statuses
---|---|---
*(none)* | Serve | —
`doctor` | Diagnose the warehouse without starting the server | `0` clean, `1` findings, `2` a check could not run
`backup` | Record a manifest | `0` recorded, non-zero refused
`drill` | Prove the manifest restores | `0` proven, `1` a table did not verify, `2` could not run
`attest <path>` | Prove a write-once store still refuses writes | `0` attested, `1` allowed something, `2` nothing attempted

`--help`, `--version` | Say what this binary is | `0`, always
anything else | Refused | `2`, with the usage text

`--help` and `--version` are answered **before the configuration is read**, deliberately. Asking a
program what it is must not depend on a file being well formed --- and for one release it did, which
is how the pitfall below came to be written.

> **Fixed** — This chapter previously recorded that there was no `--help` and no `--version`, that
> argument parsing was a match with a fall-through, and that **any unrecognised argument started the
> server** --- so a typo on a machine already running a SANKHYA was two maintainers on one warehouse,
> the two-committers race §15.3 names. Every arm is now named and the unnamed ones exit `2`.
> `crates/sankhya-server/tests/configured.rs` holds the tests, including one that runs the binary
> with a configuration path that does not exist to prove `--version` still answers.

## 17.9 The runtime the architecture specifies, and what runs today

The deployment story above describes a process. The architecture also specifies how that process
divides itself, and the distinction between *specified* and *running* matters here more than
anywhere else in Part III.

**Four runtimes, not one**, because the sync path and the query path are natural enemies — both want
processor time, memory and I/O — and the failure mode is asymmetric: a stalled applier stops log
reclamation, which can take down the source database.

Runtime | Sizing | Why isolated
---|---|---
**Control** | Small | Must stay responsive when everything else is saturated, or an orchestrator kills a healthy node
**Capture** | **Reserved cores** | Reservation, not prioritisation — priority schemes fail under sustained saturation
**Network** | Proportional | Latency-sensitive, not compute-bound
**Execution** | Remainder | Compute-bound; tolerates queuing

Memory is four disjoint pools with no lending between them. I/O isolation is **physical first, quota
second**: the write-ahead log, query spill and cache each live on separate filesystems or devices, so
a query that fills the spill volume is incapable of filling the log volume. Connection pools are
separate and individually capped for transactional writes, replication, analytical reads and
maintenance, with the replication slot reserved and never shared — a runaway analytical workload must
be structurally unable to exhaust connection slots and lock out the transactional writer.

Under pressure a typed bus feeds one centrally evaluated escalation ladder, and the first and
highest-leverage action is to **lengthen the commit interval**, which attacks the cause rather than
the symptom: fewer, larger files reduce compaction load, metadata volume and planning latency at
once. It is safe precisely because the arrival buffer preserves freshness as the commit rate falls
(Chapter 8, *The read path*), and it is bounded by buffer memory:

```
buffer_bytes ≈ write_rate × commit_interval × avg_change_size × safety_factor
```

**These two parameters must be tuned together.** If they are owned by different configuration sections
they will drift, and the failure will occur under exactly the load that triggered the backpressure.

**What runs today** is one process serving a published warehouse: the wire-protocol door, the Arrow
Flight SQL door, the metrics endpoint, the maintenance thread and the feed runner. The four-runtime
split, the reserved capture cores, the pressure bus and the escalation ladder are specified and
tested as logic, and **no running process drives capture**, so the ladder has nothing to escalate.
Chapter 26, *Roadmap and status*, is authoritative.

**Not built: container images and signing.** The platform baseline is checked and every deployment
manifest's grace is compared against the drain; the artifacts a release pipeline would produce are
not built, and the manifests assume an image somebody has to make. Pack bundles get **digest
pinning** rather than signatures — an operator pins the digests of bundles they have reviewed and
anything else is refused, including everything when nothing is pinned, because a trust policy that
defaults to trusting is not a policy. A digest proves the bytes are the bytes you pinned; it proves
nothing about who wrote them, and calling it a signature would be the overclaim. Chapter 21,
*Extensions and packs*, covers the rest.
