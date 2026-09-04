<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# Remediation plan

**Derived from:** `AUDIT_REPORT.md` — 129 findings across twelve audits
**Date:** 2026-09-03 · **Commit audited:** `710848b`
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> **The status line above is now correct — item 0.7, done.** It previously read *"M0–M8, M10 and
> M13 complete"* in all thirteen documents, and `check-docs` enforced that agreement while looking
> only for the literal *"in progress"* — so *"substantially complete"*, *"closed on four of five"*
> and *"complete on six of eight"* passed straight through. The gate now recognises a **count of
> criteria** as the mark of an unsettled milestone, and the first thing it caught was a fifth
> wrong claim none of the twelve audits found: M13, which every document called complete and
> STATUS calls *substantially built*.

## The one rule

**No fix lands without a test written the way production calls it.**

This is not a general plea for testing. It is the specific lesson of this audit. The repository
already has 2,632 tests, 741 mutations and a 25-check gate, and all of it was green while the
shipped configuration prevented the server from starting, no password was ever verified, and
compaction was corrupting external readability on every tick. The tests were not absent. They were
**calling the code differently from the way production calls it** — against a fixture the
documentation never hands the reader, through a helper that bypasses the server, in a shape where
the defect could not appear.

So for every item below, the acceptance criterion is not "a test passes". It is: *the test enters
through the same door a user does, and it fails before the fix.*

## How this plan is sequenced

Severity alone would order this list badly. Four principles override it.

1. **Root causes before symptoms.** Four findings each explain many others. Fixing the root
   collapses the list; fixing symptoms one by one does not.
2. **Truth before repair.** A fix you cannot prove is a belief. The verification harness is
   therefore Phase 1 — *not* the last phase, because everything after it is unverifiable without it.
3. **Live damage before latent damage.** A defect writing bad state right now outranks a worse
   defect that has not fired yet.
4. **Honesty is instant; correctness is not.** Where a fix needs design, the *disclosure* of the
   defect still ships in Phase 0. A user must never be misled while waiting for a repair.

## The four root causes

Fix these and roughly forty findings close behind them.

| | Root cause | Symptoms it explains |
|---|---|---|
| **R1** | The test fixture and the documented recipe silently diverged | `RUN-03` `RUN-05` `RUN-14` `CLM-16` `FEA-07` and most doc-vs-reality findings |
| **R2** | There is no CI — the gate runs only when someone chooses | `CLM-21` `DEP-04` `PERF-06`, and the fact that all 129 survived |
| **R3** | Gates report green when they measure nothing | `CLM-16` `PERF-05` `RUN-13` `CNF-02` `FEA-07` `OPS-12` |
| **R4** | Documents assert; nothing checks the assertion | `CLM-01` `PERF-01` `PERF-03` `PERF-04` `RUN-07` `RUN-13` `FEA-05` |

---

# Phase 0 — Stop the bleeding, and stop lying — **DONE 2026-09-03**

**Shape:** hours, not days. Mostly one-line changes and documentation.
**Why first:** every item here either unblocks all subsequent manual verification, removes an
active hazard, or corrects something a reader is being told right now.

| | Finding | Action | Proof |
|---|---|---|---|
| 0.1 | `RUN-01` `OPS-01` | `config/application.yaml:52` — `u64::MAX` → `i64::MAX`, or delete the line | A test that starts the binary **with the shipped config**, not a synthetic one |
| 0.2 | `RUN-06` | `main.rs:436` — explicit `--help`/`--version` arms before the `_ => {}` fallthrough | Unrecognised argument exits non-zero without binding a port |
| 0.3 | `SEC-05` | `CREATE AGGREGATION` behind an explicit opt-in, default off | Unauthenticated arbitrary code execution is refused by default |
| 0.4 | `DEP-01` | Generate the third-party attribution file | **The only outright distribution blocker in the report** |
| 0.5 | `CNF-01` | Compaction writes real `partitionValues` | See Phase 2; the *fix* is one field, so it ships now |
| 0.6 | `RUN-12` `OPS-03` | `packaging/systemd/sankhya.service` — add `WorkingDirectory=` and `SANKHYA_CONFIG=` | The unit produces a configured server, not an anonymous one |
| 0.7 | `CLM-01` | Correct the status line in ten documents and the README | `check-docs` already enforces agreement — it was agreeing on a false statement |
| 0.8 | `SEC-01` `RUN-04` | **Disclose** that passwords are not verified, in the startup line and the docs | The repair is 2.1; the disclosure cannot wait for it |
| 0.9 | `RUN-02` | Link `docs/book/` from the README's "Start here" table | Twenty-seven chapters, currently referenced by nothing |
| 0.10 | `RUN-07` | Settle the `sankhya-tiering` contradiction across three documents | Two of them contradict the one they call authoritative |

> **0.1 is a single character and it unblocks `doctor`, `backup`, `drill`, `attest`, `--version`
> and `--help`.** Nothing else in this plan can be manually verified until it lands.

### What landed, and what it cost

All ten, each with a test that fails without the fix, and `check-all` green.

Three of them grew in the doing, and the growth is the interesting part:

- **0.1 had a second mouth.** The parser's `unwrap_or(u64::MAX)` would have read
  `read_as_of: -5` as *read everything published* — the exact silent reinterpretation the
  refusal text promises never happens. It now refuses.
- **0.2 was coupled to 0.1.** `--help` was answered *after* the configuration loaded, so the
  broken config refused the one command a stuck stranger types. Help and version are now
  answered before any file is read, and the test proves it by naming a file that does not exist.
- **0.6 became a gate.** `check-package` now fails any service unit that says neither
  `WorkingDirectory=` nor `SANKHYA_CONFIG=`, because a unit that starts unconfigured has no
  symptom: the port answers.

**0.7 caught a finding the audit missed.** Correcting the status line meant widening `check-docs`
to recognise a *count of criteria* — "four of five", "six of eight" — as the mark of an
unsettled milestone. It immediately flagged a **fifth** wrong claim that none of the twelve audits
found: **M13 is "substantially built"** in STATUS's own table, and every document called it
complete. The corrected line was itself wrong until the widened gate said so.

That is the argument for Phase 1 in one incident: the fix to the check was worth more than the
correction it was written to enforce.

**Two gates caught this work, which is the system behaving correctly.** `check-mutations` found
that 0.5 had moved the source a catalogue entry names; the entry was updated and a **new mutation
added** for the partition values themselves. And `book_sql` — the gate built two days earlier ---
found that 0.3 had made the book's `CREATE AGGREGATION` examples refuse. The book harness now runs
as an operator who granted the capability, and the closed default is held by its own test.

---

# Phase 1 — Build the floor you will stand on — **DONE 2026-09-03**

**Shape:** the largest single investment in this plan, and it must not be deferred.
**Why here:** Phases 2–5 produce roughly a hundred fixes. Without this phase, none of them can be
shown to work, and the next audit finds the same class of defect again.

### 1.1 Unify the fixture with the documented recipe — `R1`
`crates/sankhya-server/tests/common/mod.rs:262` builds a `sales` cube, `period` and `margin_pct`
that `make_warehouse` never writes. Either the recipe produces what the fixture builds, or the
gates point at the recipe. **Then assert in CI that they cannot diverge again.**

This one change takes the tutorials from 3/21 to 21/21, fixes `RUN-14`, and removes the largest
source of doc-vs-reality findings in the report.

### 1.2 Stand up CI — `R2`
There is none. Add: `--locked`, a pinned toolchain, `check-all`, advisory and licence scanning
(`DEP-04`). Until this exists, "the gate passes" means "it passed on one machine, once, when
someone remembered".

### 1.3 Make silent-green impossible — `R3`
- `CLM-16` — fifteen PostgreSQL end-to-end tests skip to green invisibly. A skip must be loud, and
  a skip in CI must fail.
- `check-concurrency` returns `ok` having taken zero measurements. `taken == 0` becomes a failure.
- `CNF-02` — the oracle test cannot prove what its README claims: hand-written schema, zero-byte
  files, no partitioned table. Make it read a real table written by a real writer.
- `FEA-07` — the catalogue drift test is tautological.
- `PERF-06` — `check-performance` joins `check-all`, and the NFR gate goes through the server
  rather than around it.

### 1.4 Teach the doc gate to read words — `R4`
`xtask/src/docnumbers.rs:150` recognises four markers and walks back over **digits**. Every count
that drifted is spelled as an English word — *eight*, *ten*, *twelve*, *eleven invariants*, *one
table*. Widen it, and extend it to the claims it has never covered: performance figures
(`PERF-01`, `PERF-03`, `PERF-04`) and feature status (`FEA-05`).

### 1.5 Close the mutation blind spot
`CLM-08` — eighteen crates have no mutation entry at all.

---

### Phase 1 — what landed

**1.1 The fixture and the recipe are one thing.** `make_warehouse` now calls
`common::write_warehouse`, so they cannot drift by construction. Verified the way a reader
does it: generate the warehouse, start a server, run every tutorial block. **3 of 21 became 21
of 21** — 17 answer, 4 refuse with the documented reason. QUICKSTART's own query returns its
printed output character for character.

**1.2 CI exists.** `.github/workflows/gate.yml`: `--locked` builds, `check-all`, `cargo-deny`
against a written licence policy, and a job that builds against the declared `rust-version` so
the MSRV is a fact rather than a claim. `rust-toolchain.toml` pins 1.97.1, settling three
different numbers for one fact.

**1.3 Silent green is no longer possible.**

- `check-concurrency` fails when it measures nothing, with a loud named escape for CI.
- **Fifteen tests that passed without running** now record it through
  `sankhya_testkit::skipped`, because `cargo test` discards a passing test's output. The gate
  prints `DID NOT RUN` for each; `SANKHYA_REQUIRE_E2E=1` makes it a failure.
- The **kernel oracle** moved to where the production writers are. It fails without the
  `CNF-01` fix with `delta_kernel`'s own error.
- The **catalogue drift test** compared the served list against the list it is served from.
  Reading the engine's real registry instead immediately found `derived()` — documented in
  the book, registered by the server, described nowhere, so no SDK binding offered it.

**1.4 The doc gate reads words.** Every count that had drifted was spelled out, and the gate
walked back over digits. Scoping matters: word-matching `" tests"` flagged ordinary prose, so
words are allowed only where the figure can only be global.

**1.5 Mutation coverage is complete.** Eighteen crates had no entry at all, about nineteen
thousand lines. All now covered, with four listed as declarative by exception, and
`check-mutation-coverage` fails if that regrows.

### What writing those mutations found

Nine survived on first run. Each was a real gap, and closing them added tests for: the
**visit-budget boundary** in Dijkstra; the decoder's **negative-length and unknown-kind
refusals**; the **absolute** overlay rebuild bound; **pack-versus-pack** name collision; lease
renewal **at the exact expiry instant**; the sacrifice rung **at** its threshold; and
`Trust::is_allowed` — the two lines deciding whether third-party code runs in this
process, which nothing had ever called.

Three mutations were **removed rather than answered**: a negative ticket lifetime, an
unreachable `while` in the gRPC shutdown loop, and one that referenced a later binding and so
never compiled. A mutation describing a defect the code cannot produce is not evidence, and a
test contorted until it fails is worse than no test.

---

# Phase 2 — Data loss — **DONE 2026-09-04**

**Why before wrong answers:** a wrong number can be recomputed. Deleted bytes cannot.

| | Finding | The failure |
|---|---|---|
| 2.1 | `CNF-01` (verify) | Confirm Phase 0.5 against a real kernel reader on a partitioned table |
| 2.2 | `COR-14` | **There is no `fsync` anywhere.** Every durability claim rests on it |
| 2.3 | `COR-01` `COR-03` `OPS-09` `OPS-13` | Four independent paths to sweeping data something still pins |
| 2.4 | `COR-02` `COR-06` | Two publishers collide on a data-file name; compaction names restart at zero on restart |
| 2.5 | `COR-15` | Nothing prevents two servers on one warehouse |
| 2.6 | `OPS-15` | A crash mid-commit reads as a successful **empty** commit |
| 2.7 | `OPS-14` | A leaked lease stops reclamation for ever |
| 2.8 | `CNF-05` | `deletionTimestamp` is a tick counter, so a conformant external `VACUUM` deletes every superseded file **immediately** |

### Phase 2 — what landed

All eight, each with a test written the way production calls it, and `check-all` green.

**2.1 The kernel reads what compaction wrote.** Three oracle tests against `delta_kernel`, on a
partitioned table built by the real publisher and merged by the real driver. The first fails
without 0.5's fix with the kernel's own error — *"Found unmasked nulls for non-nullable
StructArray field"* — which is what an external reader would have said instead of reading.

**2.2 `fsync`, and the half that gets forgotten.** The file before the rename, the **directory**
after it. Three mutations were written for these calls and all three survived, correctly: `fsync`
cannot be observed from inside the process that calls it. `check-durability` asserts the source
property instead, the same shape `check-atomic-writes` already takes and for the same reason. A
mutation nothing can catch is a permanent survivor that teaches people to ignore the list.

**2.3 Four ways to answer "nothing reads this" when something did.** None of them was a race;
each fired on a schedule and reported nothing.

- A clone's pin was recorded as `sales.orders` and the sweeper asked for `orders`. False in
  **every deployment** — `discover` only ever walks `<warehouse>/<schema>/<table>` — and it
  survived because the one test covering it built its table at the warehouse root, the single
  shape in which the two forms cannot disagree. The new tests use production's layout.
- The orphan sweep honoured clone pins and not snapshot pins. Retirement, a hundred lines below,
  unioned both. The pin set is now resolved **once per tick** and handed to both paths, because
  the defect was two paths each computing their own answer.
- A snapshot document that would not parse, and a snapshot naming an ambiguous table, each
  contributed nothing — and nothing is what a table with no snapshots contributes. Reclamation
  now **stops** when the pin set is unknown and says so in the tick report.
- A poisoned mutex silently stopped the pin refresh for ever, so the sweeper went on reclaiming
  against whatever pins were current at the moment of the panic. Poison is recovered from now, as
  the maintenance side of that same lock already did.

**A fourth path the audit did not name.** `pinned_by_*` skipped a pinned version it could not
read, justified as *"the sweep falls back to the age threshold"*. True of the orphan sweep, false
of retirement, which has no age fallback: it saw no paths and so saw no reason to keep the file.
One paragraph had been written for two mechanisms.

**2.4 A data file name is used once.** The publisher, not the caller, decides the name: it
carries the version being attempted **and** a per-write token, and compaction's sequence is
recovered from the log rather than from a counter that restarts at zero. Under both,
`write_parquet` opens with `create_new`, so a name that already exists is refused rather than
truncated.

The token is not decoration. The version settles one publisher flushing one name twice; it does
nothing about two publishers, because both read the same `next_version` and so both intend the
same version and compute the same name. That is the half `COR-06` is actually about, and the
concurrency tests could not see it because they gave every writer a distinct file name. The new
test gives them the same one.

**That floor found a defect nothing else had.** The accumulator flushes one logical name once per
round, so its second flush truncated the first file while the first's `add` was still in the log
--- acknowledged rows gone from disk, and the live set insisting they were there. It also found
that `Published.file` still reported the *caller's* name while the write, the `add` and the error
path all used the versioned one, so a publisher handed back a path that does not exist. The
kernel oracle is that caller, and its merge failed.

**2.5 One server per warehouse.** `claim` serialises two committers at a version and that is its
entire scope; maintenance lives outside it. `flock(2)` is out of reach — `unsafe_code` is
`forbid` and `libc` is confined to the sandbox crate — so the lock is a file naming the process
that holds it, and liveness is established from `/proc` using the holder's **start time** as well
as its pid, because a lock broken on a reused pid is two servers on one warehouse. A lock that
cannot be interpreted refuses startup rather than being broken; every automatic way out of that
case ends in the failure being prevented.

**2.6 A commit says how long it is.** The first line is a seal carrying the number of actions
that follow, and a reader that counts something different refuses. A truncated body used to
replay as a *shorter* commit and an empty one as a commit that did nothing — and it was
cemented, not transient, because a retry is refused as `VersionTaken` and the next version lands
on top. Lines are parsed as generic JSON before their kind is read, so another engine's action is
counted and passed over rather than rendering the table permanently unreadable.

**2.7 The backstop was written as an `and`.** Both conditions were requirements, so one leaked
lease held both copies of every compacted partition on disk for ever and one entry per merge in
memory for ever. Two thresholds now: `grace_ticks` is a minimum and `leak_ticks` a maximum. It
overrides the lease check and **nothing else** — a clone pin and a snapshot pin still refuse the
file, because there is no timeout at which those become wrong — and it is reported when it
fires, because a backstop firing means a lease leaked.

**2.8 A timestamp is a clock, not a counter.** `deletionTimestamp: 3` is three milliseconds after
1970, so every superseded file was instantly past any retention interval and a conformant external
`VACUUM RETAIN 168 HOURS` would have deleted the lot — out from under readers holding leases,
and reported safe by `DRY RUN` first. The two retention mechanisms could not see each other
because one was reading a counter as a clock.


---

# Phase 3 — Wrong answers

The failure this product exists to prevent: a number of the right magnitude and no meaning.

| | Finding | The failure |
|---|---|---|
| 3.1 | `COR-08` | `deterministic_sum` returns approximations and its proof is wrong — **a regression introduced 2026-09-02** |
| 3.2 | `FMT-01` | A schema change is adopted in memory and never written to the log |
| 3.3 | `ING-01` `ING-05` `ING-06` `ING-07` `ING-10` | Capture: duplicate rows on resume, `TRUNCATE` silently discarded, TOAST dropping row updates, `inf` after the binder said it fitted |
| 3.4 | `COR-04` `COR-05` | A materialised cuboid's key has no measure; materialisation stores the sum whatever the rule was |
| 3.5 | `COR-07` `COR-09` `COR-10` | `irr`, `drawdown`, `f_test` — each returns a plausible wrong number rather than refusing |
| 3.6 | `CLI-01` `CLI-05` | Every timestamp on the wire is a raw integer; a null inside an array reads as `0.0` |
| 3.7 | `CLI-06` `CLI-07` `CLI-09` `RUN-07` | Silent no-ops: `SET SNAPSHOT` lost, unknown `by=` changing the grain, trailing comments returning a bogus empty row |
| 3.8 | `COR-19`–`COR-29` | Remaining snapshot, cache and cube arithmetic |

---

# Phase 4 — Security

**Why after correctness:** the system is not yet exposed, and 0.3 and 0.8 already removed the
active hazard and the false impression. This phase makes the guarantees real.

| | Finding | The failure |
|---|---|---|
| 4.1 | `SEC-01` | No password is ever verified — the check is *non-empty* |
| 4.2 | `SEC-02` | Column masks are never applied |
| 4.3 | `SEC-06` | Path traversal from three statement names |
| 4.4 | `SEC-03` `SEC-04` | Flight authorizes as a literal; two statement families reach state with no authorization |
| 4.5 | `SEC-09`–`SEC-14` | The sandbox: runs as the server's user, a function can kill the server, "no subprocess" is not delivered, the output cap cannot fire, the probe tests the wrong mechanism |
| 4.6 | `SEC-16`–`SEC-18` | Disclosure: DataFusion field lists, contested tables, five unfiltered listings |
| 4.7 | `SEC-07` `SEC-08` | The audit record is structurally empty and volatile; `/metrics` enumerates every table **and a test asserts the leak** |
| 4.8 | `SEC-15` | The shipped binary can only express "everything" or "nothing" |

---

# Phase 5 — Operability

| | Finding | The failure |
|---|---|---|
| 5.1 | `OPS-04`–`OPS-07` | Nothing bounds memory; the audit chain is an unbounded in-memory `Vec` |
| 5.2 | `OPS-08` | An `accept()` error kills the server |
| 5.3 | `OPS-12` | "I could not look" is recorded as "there is nothing" |
| 5.4 | `OPS-10` `OPS-11` | Maintenance is blind and partial |
| 5.5 | `OPS-21`–`OPS-26` | Degradation, observability, runbooks |
| 5.6 | `OPS-22` | The per-statement cost: double full log replay per table, checkpoint decode per table, a fresh `SessionContext` with ~150 UDF registrations, and a `SessionState` **deep clone per table** |
| 5.7 | `RUN-10` `RUN-11` | Flight unconfigurable and unexposed; the Kubernetes manifest names an image no Dockerfile builds |

---

# Phase 6 — Close the gap between what is claimed and what is true

| | Finding | Action |
|---|---|---|
| 6.1 | `PERF-01` | **Retract the 24.9×.** It does not reproduce — measured 2.2×, and the fast arm was optimised away |
| 6.2 | `PERF-02` `FEA-06` | There are no benchmarks. Build them, or stop publishing numbers |
| 6.3 | `PERF-05` | Fifteen of eighteen `NFR-PERF` objectives are unmeasured and **seven are missing from the table that reports on them** |
| 6.4 | `PERF-07` | State the absolute price of determinism — 6× to 32× against an ordinary sum — not only the ratio against our own previous code |
| 6.5 | `FEA-01`–`FEA-05` | No write path; the graph engine can never answer; packs cannot load; cube hierarchies are validated and ignored; QR/SVD/eigen shipped while six documents call them deliberately absent |
| 6.6 | `FMT-02`–`FMT-09` | Protocol versions written and never read; unknown actions killing a table; no migration mechanism |
| 6.7 | `ING-00` `ING-08` `ING-09` | There is no change-capture runtime; reconciliation compares nothing |
| 6.8 | `RUN-08` `RUN-09` `RUN-15` | The stale transcripts, the corruption demo that proves nothing, and the small stumbles |

---

# What this plan deliberately does not contain

- **Anything requiring a second machine.** `M12` still parks there, and this plan does not pretend
  otherwise.
- **A production run.** Nothing here substitutes for one.
- **Re-auditing.** The findings are recorded with file and line; they can be re-checked in seconds.
- **Effort estimates in days.** They would be invented. The phases are ordered by dependency, and
  each item's size is legible from its description.

# The honest caveat

Twelve auditors reading for three days found 129 things in a repository whose gate was green.
**The correct reading of that number is that it is a lower bound.** This plan closes what was found.
It does not, by itself, make the system production-ready — it makes the system one where the next
hundred findings would be visible.

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
