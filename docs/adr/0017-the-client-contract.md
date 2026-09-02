<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# ADR-0017 — What a client may assume, and what it may never decide

**Status:** Accepted · **Date:** 2026-08-31 · **Milestone:** M14 — the design gate, before any implementation
**Builds on:** [ADR-0006](0006-flight-sql.md), [ADR-0009](0009-the-cube-lifecycle.md), [ADR-0010](0010-external-aggregations.md), [ADR-0016](0016-zero-copy-cloning.md), [`DEC-44`](../ARCHITECTURE.md), [`DEC-47`](../ARCHITECTURE.md)

## Context

Everything this system does is reachable today only by somebody willing to write SQL over a
socket on the same machine. `M14` puts a package in front of it, and `M16` adds two more
bindings behind the same promises.

The order matters. A contract written once and implemented three times produces three clients
that agree. Three clients written separately and reconciled later produce a contract that is
whatever the first one happened to do --- including its accidents, which by then are somebody's
production code.

So this decides the client contract before the client exists. It is the same reasoning as
[ADR-0016](0016-zero-copy-cloning.md), taken at the same point in a milestone and for the same
reason: the decisions below are cheap now and expensive once a wheel is installed somewhere.

## What is already settled elsewhere

Named here so this document does not appear to re-decide them:

- **Flight SQL is the data door** ([ADR-0006](0006-flight-sql.md)); the PostgreSQL wire protocol
  stays for tools nobody wrote for this system.
- **An external aggregation is a contract, not a function**, and it runs **out of process**
  ([ADR-0010](0010-external-aggregations.md)).
- **A cube has three lifetimes** ([ADR-0009](0009-the-cube-lifecycle.md)).
- **Every error carries a permanent code and a remediation** (`DEC-44`), and **a statement the
  system will not honour is refused rather than confirmed and discarded** (`DEC-47`).

## Decision 1 — A binding contains no logic the server does not also enforce

A client may **anticipate** a refusal so the message arrives sooner and reads better. It may
never **be** the refusal.

The failure this prevents is specific. Suppose the Python binding rejects a cube whose measure
declares no rule for a dimension, and the Java binding does not. The rule now lives in Python.
The Java user gets whatever the server does, the Python user gets a different product, and
the second binding is a documented way around a correctness rule.

**The test:** delete every SDK and nothing about what the system permits, refuses or audits
changes. Anything that fails that test is server work wearing a client's clothes.

The corollary is that a client's convenience layer must be **derived** where it can be:
capability discovery, error remediation and cube description come from the server rather than
from a table compiled into the package, because a table compiled into the package is a copy that
goes stale silently.

## Decision 2 — A refusal crosses the wire as data, never as a sentence

This system's refusals name what to do: *"drop it first"*, *"materialise them first"*, *"a clone
of this table still reads it"*. `may_drop` names the clones that would break; the tiering
refusals name the partitions; the backup refusals name the position.

**All of it is structured.** A refusal carries:

| Field | Why it cannot be folded into the message |
|---|---|
| `code` | the stable identity `DEC-44` already guarantees; a client dispatches on it |
| `sqlstate` | what a generic driver on the other door understands |
| `remediation` | the half that says what to do, which a rendered string drops |
| `subjects` | the **names** a refusal cites --- clones, partitions, dimensions --- as a list |

`subjects` is the one that is easy to omit and expensive to add later. Without it a client that
wants to show *"three clones read this table"* must parse the message, and the message becomes an
API nobody meant to publish and nobody may reword.

A binding maps `code` to a typed exception and leaves the other three intact on it.

## Decision 3 — Results stream, and no binding collects on the caller's behalf

`MAX_RESULT_ROWS` bounds what a statement returns. A client asking for a hundred million rows
must stream, and a binding that collects before it yields turns a working query into an
out-of-memory kill on a laptop, with the server having done nothing wrong.

So the streaming call is the **primitive**, and every convenience --- a dataframe, a list of
rows --- is a named opt-in on a result the caller has decided is small. Back-pressure belongs to
the transport: a slow consumer slows the scan rather than buffering it into the client.

## Decision 4 — A long operation is a commit, and the commit is the answer

Materialising a cuboid, cloning a large table, ingesting a file and taking a backup can each
outlast a sensible timeout. The obvious design is a job registry: return a handle, poll it.

**Refused for now.** A registry is a second durable state machine --- entries that outlive
their operation, need their own reclamation, their own authorization, and their own answer to
*"what happens when the server restarts mid-job?"*. This system already has exactly one durable
record of what happened, and it is the log.

Instead: **every long operation's effect is a commit whose name the client already knows.** A
disconnected client does not ask the server what it was doing; it asks the warehouse what is
there.

- Did my clone happen? The table exists, or it does not.
- Did the cuboid materialise? It is present at *(definition version, snapshot, scope, cuboid)*,
  or it is a miss and the next query recomputes.
- Did the ingest land? The table's version moved, or it did not.

Two obligations follow, and they are the price of not having a registry:

1. **Every long operation is idempotent under re-issue**, or refuses with a refusal that names
   the object it found --- never a partial second attempt.
2. **No operation leaves a state only the disconnected client could describe.** This is
   `FR-TIER-08`'s argument for the purge journal, applied to a network boundary.

A job handle stays available later, for an operation that genuinely cannot be expressed as a
commit. None has been found yet, and inventing the registry before that operation exists is how
a warehouse acquires a second system of record.

## Decision 5 — Version skew is a connection-time refusal

A client is installed independently of the server. The contract carries a version, exchanged at
connection, and a mismatch is refused **there** --- naming both versions --- rather than
surfacing eleven calls later when a field turns out to be missing.

Same reasoning as `sankhya-version` on artefacts: fail where a person can act, not where the
absence happens to be noticed.

**Built 2026-09-02.** The version rides in a startup parameter, `sankhya_contract`, and the
server announces its own the same way. A client that declares one outside the served range is
refused with `08004` --- the code for a server declining to establish a connection --- naming
both versions in the message and carrying them as `subjects`.

**A client that declares nothing is not refused**, and that is the load-bearing half. `psql`,
JDBC and every ordinary PostgreSQL driver have no contract version because they are not
SANKHYA bindings, and refusing them would refuse the ecosystem this door exists for. Only a
client that *says* which contract it speaks is held to it.

## Decision 6 — The session is a permission's context, never its cache

A connection carries a `Principal`, and every statement is authorized at the choke point exactly
as a local one is. A client **never** caches an authorization decision: a grant revoked between
two calls has to take effect on the second.

This also fixes what an ephemeral cube is. It is **server-side session state with a mandatory
expiry**, not client-side state the server has forgotten about --- because its cuboids, if it
materialises any, are storage. A cube that materialises and is never dropped is `RSK-35` in a
different costume, and a client that dies is not an instruction to keep its cubes forever.

## Decision 7 — The Python binding is pure Python

Not a compiled extension.

`pyarrow` is already required --- it is what an Arrow result *is* on the Python side --- and it
carries a Flight client. A Rust core behind `pyo3` would add a per-platform wheel matrix, a
build toolchain for anyone on an unlisted platform, and an ABI to keep in step with three
Python versions, all to accelerate a layer that Decision 1 says must contain no logic.

**If the client is thin, its language does not matter. If its language matters, it is not thin
enough.**

This is reversible in the direction that costs least: a compiled fast path may be added later
for a measured bottleneck, behind the same contract.

## Decision 7a — Where the bindings live

**Owner directive, 2026-09-01.** One `sdk/` directory at the repository root, one subdirectory
per language:

```
sdk/
  python/     M14
  java/       M16
  rust/       M16
```

Not inside `crates/`, because only one of the three is a Rust crate and putting the other two
under a Cargo workspace directory would be a lie about what builds them. Not one repository per
binding, because Decision 1 makes the *contract* the thing that must not diverge, and three
repositories are three release cadences and three chances for one of them to fall behind the
server it is thin over.

**Each subdirectory carries its own quickstart.** A user who arrives at `sdk/python/` — from a
package index, from a link, from a colleague — needs to get to a working query without first
reading the warehouse's documentation. The root `QUICKSTART.md` starts a server; `sdk/python/`
assumes one is running and starts from `pip install`.

That is a duplication, and a deliberate one: the alternative is a binding whose documentation
lives somewhere its user is not.

## Decision 8 — Examples are gated artefacts

The binding ships a runnable example per capability, and the gate executes them against a real
server.

An example that does not run is documentation that lies, and it lies to the person least able to
tell --- somebody meeting the product for the first time, who cannot distinguish *"this is
wrong"* from *"I am holding it wrong"*. Examples are therefore held exactly as tests are: one
that breaks fails the build.

They are also the closest thing this project has to a **user's** acceptance test. Every other
test in the repository was written by the same hand that wrote the code it checks. An example is
written for somebody else to run.

## Consequences

**The server grows first.** Lineage, dependents, capability discovery and structured refusals
are all server work that a client merely surfaces. That is the shape Decision 1 forces, and it
is why `M14` is not "write a Python package".

**Refusals become part of the API surface.** Once `code` and `subjects` are dispatched on, they
are as public as a function signature: a code may be added, and one already published may not be
repurposed. `DEC-44`'s catalogue is where that is enforced.

**No client can be tested against a mock.** A binding whose tests mock the server tests its
author's belief about the server. The gate runs it against a real one, which makes the client's
test suite slower and worth having.

## What this does not decide

**Federated identity.** `FR-SEC-03` names tokens, mutual TLS and scram. The transport is `M14`'s
gate; which identity providers are supported, and how a token's claims map to a `Principal`,
waits until there is a deployment with an opinion --- guessing produces a mapping nobody uses.

**Asynchronous Python.** The contract is synchronous. Whether the binding also offers `async`
awaits somebody who needs it; adding it later is additive, and adding it now doubles the surface
before anyone has held the first one.
