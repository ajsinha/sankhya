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
already has 2,724 tests, 741 mutations and a 25-check gate, and all of it was green while the
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

### Phase 3 — what has landed so far

**3.1 `deterministic_sum` is exact again.** The fast path's proof concluded from a margin under
the *largest term* that truncation could not move the answer, and what is returned is the
**total**. All four inputs the audit ran now return the exact value; §10.4 of the book has the
post-condition and why counting the terms that actually lost bits keeps the fast path free.

The catalogue's one entry for this code changed the constant `100` to `20`, which the random
property test catches — it protected the constant and not the proof.

**3.5 `irr`, `drawdown`, `f_test` — three plausible wrong numbers.**

- `irr` returned `Ok(10.0)` for a hundred-flow project whose true rate is eleven per cent, and
  whose value at the returned rate is `-989`. Two defects: a discount factor that underflows near
  a rate of minus one makes the value a `NaN`, and `NaN` compares false against everything, so
  neither the refusal nor the bracket test could fire. And testing the extremes is only a bracket
  when the function crosses once between them — this sequence has two roots, so the corrected
  guard first refused a sequence every textbook answers. The range is now **scanned outward from
  zero**, and the answer is checked against the equation it is defined by before it is returned.
- `drawdown` divided by a non-positive peak, so `[-100, -200]` reported a **positive** drawdown
  for a series that doubled its loss, and a peak of zero returned `0.0` — a default where a
  refusal was meant. A cumulative profit-and-loss curve crossing zero is the ordinary input. It
  now refuses and says what to reach for instead.
- `f_test` took its lower tail as `1 - upper`, which the module header forbids by name. A double
  near one has no bits below about `1e-16`, so every lower tail smaller than that was reported as
  **zero** — in the direction that makes a finding look stronger. It now uses the distribution's
  own reciprocal symmetry.

### What the mutation catalogue turned out to be doing

Verifying the new entries found a survivor whose *entry* was wrong rather than whose code was
uncovered: it named `if !value.is_finite() {` and there are two of those in `reduce.rs`, so it had
been mutating the early scan in `exact_sum` — where a second guard masks it — rather than the
expansion it was written for.

A mutation replaces the **first** occurrence, so this is mechanically checkable, and
`check-mutations` now checks it. It immediately found **fifteen** entries naming text that occurs
more than once, and one over-declared count where Phase 2's commit seal had removed a site.

Fourteen were mislabels: the entry mutated whichever site came first. One was a hole. The lease
ceiling is applied when a lease is **granted** and when it is **renewed**, the entry named text at
both, and the renewal path was therefore covered by nothing — a lease that cannot be granted past
the maximum but can be renewed past it has no maximum, which is the forgotten lease the registry
exists to make impossible. Three entries were added for second sites that nothing had been
testing.

This is `COR-08`'s shape one level up: *the catalogue protects the constant and not the proof.*

**3.2 A schema change reaches the log (`FMT-01`).** This was live data loss with one binary
reading its own files. A compatible change updated the in-memory shape and incremented
`schema_changes_applied`; `Publication::create` is the **only** writer of `schemaString` and every
caller gates it on the table being new, so the added column's data was encoded, written into
Parquet, and unreachable by every query, permanently — while the metric said the change had been
applied. `warehouse.rs` carried a comment reading *"a schema evolution writes a new one"*,
describing a function that did not exist.

Two pieces were missing and both are now there. `latest_metadata` reads the table's declared
shape back — nothing ever had, which is also why checkpoints cannot be written (`OPS-21`) — and
`Publication::evolve` writes the current metadata back with the schema replaced, so the id, the
partition columns and every configuration entry survive. The commit carries metadata **alone**: a
schema change moves no rows.

Ordering matters and is now enforced: everything captured under the old shape is published before
the new one is adopted, so no batch spans two schemas.

The two smaller holes beside it are closed too. A `NOT NULL` column arriving at the source
classified as an ordinary addition, because the added-column arm never looked at nullability —
the one route around the `ColumnTightened` refusal, surfacing later as a scan error. And
`ColumnsReordered` was declared and **never constructed anywhere**, so a pure reorder returned
*"compatible, and nothing changed"*.

**Two pre-existing survivors closed on the way.** The file-sequence recovery parsed a whole file
stem as a number, and Phase 2's naming change had quietly made that parse fail — so the sequence
restarted at zero on every restart and nothing noticed, because the per-write token had taken over
the job of keeping names unique. The recovery is repaired, its comment no longer claims a safety
role it has handed over, and the property it does still deliver — that a directory listing is in
the order the files were written — is now asserted.

**3.7 Statements that were acknowledged and did nothing.**

- `CLI-06`: `remember_setting` was called only from the simple-`Query` arm, so a client using
  Parse/Bind/Execute got a success tag, the handler validated the snapshot, and **every
  subsequent query read the present**. pgjdbc, psycopg3, asyncpg and SQLAlchemy all use the
  extended protocol by default, so this was the path almost every real client takes — and there
  is no symptom, because the reply is a `CommandComplete` either way.
- `CLI-07`: both the wire layer and the server split a `SET` on whitespace, so
  `SET SNAPSHOT='eod'` named a setting called `snapshot='eod'` with an empty value and fell
  through to the arm that accepts any `SET` as a no-op. `SET VERSION OF sales.orders=2` broke
  the same way one token further along, making every table the same setting.
- `CLI-09`: the `by` **value** was never checked against the cube's dimensions, and the
  comparison was case-sensitive while every keyword in the file is not. `by=regoin` — or
  `by=Region` on a cube spelling it `region` — kept no dimension, rolled the axis away, and
  returned a subtotal labelled as a breakdown. `where` was checked; `by` was not, in the
  function directly below the one that checks it.

`CLI-07` was two parsers making the same mistake, written by the same hand within a week, and it
is now **one function**. Two parsers agreeing is worth nothing when they are wrong together; one
is what makes the wire layer's memory and the server's validation talk about the same statement
by construction.

**Two more survivors closed.** The catalogue-versus-handler split is decided in two places — the
simple protocol and the extended one — and a single catalogue entry named text occurring in both,
so the extended path, which almost every driver takes, was tested by nothing.

**3.4 Materialisation that changed the answer.**

- `COR-04`: the materialised cuboid's key carried the cube, the definition, the snapshot and
  the scope — and **not the measure**. So the first measure to be materialised wrote each shape,
  every later one found `exists()` true and skipped, and reads built the same measure-free key
  and labelled whatever came back with the measure they had asked for. On the shipped fixture a
  maintained `sales` cube returned `amount`'s numbers under the name `ratio`, and answered a
  `Rule::None` measure out of a stored aggregate — the one thing the ancestor-answerability
  machinery exists to prevent. The in-memory catalog had this exact defect and was fixed by
  keying on `(cube, measure)`; the on-disk key never got the same treatment.
- `COR-05`: `to_batch` stored `contributions.exact_sum()` whatever the rule said, and
  `from_batch` read it back with `add_reduced`, which answers with the stored value for **every**
  rule. A measure declared `MAX ALONG region` and maintained answered `70.0` over facts `30.0,
  40.0` where the live path answers `40.0`; `MEAN` answered `70.0` against `35.0`.

`COR-05` is the 2026-09-01 defect that `cube_rules.rs` was written to pin, resurrected one layer
down — and the file could not see it, because **every test in it declares its cube without
`MAINTAINED`** and so reads the live path. It now declares one both ways and compares them,
which is the property exit criterion 3a states: materialisation changes *where* an answer is
computed, never *what* it is.

Only a sum composes without rounding, so only a sum keeps its expansion; every other rule
reduces to one number at materialisation time, and rolling that up further is governed by
`answerable_from`, which already refuses the rules that do not decompose. A rule with no
reduction from partials — `Rule::None`, `Rule::Supplied` — is **not materialised at all**, rather
than written as a zero, because that would be materialisation turning a refusal into a number.

**3.3 Capture — three of five.**

- `ING-01`: the position held records **published** while the resume skipped by **line index**.
  Those are the same number only when every line so far fitted and there were no blanks, so a
  source that stopped part-way republished a record on restart — one duplicate per preceding
  refusal or blank, silently and permanently. `ADR-0018`'s amendment chose *never re-ingest* over
  *never duplicate* for exactly this reason, and the test that existed asserted the conflation in
  its own name. The field is now `read_through` and says what it holds.
- `ING-05`: the batcher's match ended in `_ => {}` and `Truncate` fell into it. The source table
  is emptied and the analytical copy keeps every row. A test asserted the decoder *parses*
  truncate; nothing asserted anything acted on it. Applying one is not something this pipeline
  can do — the copy is an unfolded change log with no key-based fold (`ING-09`) — so the table is
  **quarantined**, which stops publication, leaves the last consistent version queryable, and
  keeps consuming events so the replication cursor still advances.
- `ING-07`: the binder is genuinely strict and then `*value as f32` turned `1e308` into an
  infinity, one step after it had said the value fitted. The capture path had the same defect by
  a second route, because `parse::<f32>()` returns `Ok(inf)` rather than an error. Both refuse
  now, and an infinity the source *actually sent* still passes — dropping that would be the
  reverse mistake.

`ING-06` (a TOASTed unchanged value discards the whole row update) and `ING-10` are not done.
`ING-06` needs a row-lookup path into published Parquet that does not exist anywhere, so it is a
design piece rather than a repair.

**3.6 What a value looks like on the wire.**

- `CLI-01`: microsecond timestamps — this project's own canonical unit — were rendered with
  `.to_string()` on the raw `i64` under OID 1114/1184, so `psql` printed `1756545242000000` and
  JDBC and psycopg raised. The other three units fell through to Arrow's display, which writes
  `T` between date and time and `Z` for the zone where PostgreSQL writes a space and a numeric
  offset. `bytea` went out as bare hex, which a driver decodes as the *characters*. **Zero tests
  touched a timestamp.**
- `CLI-05`: both array readers copied the raw value buffer, where Arrow writes `0.0` under a
  null. The composition the documentation advertises is exactly the one that produces them —
  `ts_max_drawdown(ts_rolling_mean(prices, 3))`, whose leading nulls are deliberate — so it was
  reduced against a price of nothing. A null element now gives a null answer, which is what this
  reader already did for a null vector and for the reason its own doc gives. The parity soak
  could not see it because all three of its paths call the same kernel.

**One half of `CLI-01` is not fixed, and the reason is worth recording.** The catalogue and the
result set disagree about a **zone-aware** column because they read different sources: the result
set types a column from the Arrow schema the scan produces, which comes from the Parquet footer
and keeps the timezone, while the catalogue types it from the declared schema in the log — and
`schemaString` writes `"timestamp"` for both zone-aware and naive, because the Delta protocol has
one timestamp type. The catalogue's own mapping is corrected, and it is not enough. Making the
two agree means recording the zone in the log or normalising the read path to the declaration:
a format decision, not a rendering fix. A test that passed against the present behaviour would
pin the disagreement instead of the property, so there is none.

**3.8 `COR-19` does not reproduce, and there is now a guard saying so.**

The finding reads: the hydration cache key omits the grain, so `by=region` then
`by=region|period` in one session returns region-level totals with no `period` column and no
error. The key does omit the grain — that part is exactly as described.

A test was built to reproduce it and could not. It took three attempts to make the test mean
anything, and the first two are the more useful record:

1. The first version used the shipped fixture cube, which is **not maintained**, so nothing was
   ever served from a cuboid and the assertion passed against a path it never took.
2. The second declared a maintained cube and still passed — because a maintained cube has no
   cuboids until the refresher runs, and `materialised` came back `f` on every row. Only adding
   *that* assertion exposed it.
3. The third drives the server's own refresher, asserts a row came from a cuboid, and reads the
   cache's hit count either side of the second query so a miss cannot pass for a hit.

With all three conditions established, the finer query still answers correctly: the `period`
column is present, the row count grows, and the two grains agree on the total.

So the guard is kept and the finding is **not** claimed as fixed. What changed underneath it may
be Phase 3.7's `by=` validation, which now refuses a grain the cells cannot answer instead of
rolling the axis away — but that is a hypothesis, and the honest statement is that the described
sequence does not produce the described answer. **`COR-22` — a snapshot now verifies that it spans one instant.** `CREATE SNAPSHOT` read each
table's version in a loop, so a writer committing between two of the reads left one table
recorded before its commit and another after it: a snapshot describing a state the warehouse was
never in. That is the crate's central claim — two figures quoted from one snapshot describe the
same moment — and the old comment called it *"as close to one instant as this can make them"*,
which is honest about the mechanism and does not match what the feature says it does.

There is no warehouse-wide commit sequence to read, and locking every table would put a writer
behind a reader. So the set is **confirmed**: read every version, read them all again, accept
only if nothing moved. A set identical across that window was valid throughout it. A warehouse
too busy for five attempts to agree gets a refusal rather than a snapshot that is quietly not
one.

**`COR-21` — a snapshot pinning a version its table no longer has is refused at the `SET`.**
`SET VERSION OF` already checked this, under a comment saying why: `live_files_at` replays up to
a version and stops, so asking for one beyond the log silently answers with the newest. The
snapshot path read the same way and did not check, so such a snapshot read **the present** and
said nothing — a report quoting it would be about now while claiming to be about then.

**`COR-20` — a pinned session's cells are no longer served to an unpinned one.** The hydration
key held the table's present version, deliberately, so a configured `read_as_of` could not
freeze the cache for the life of the process; nothing in it said whether the session that filled
it was reading from a position it had chosen. A pinned read's whole promise is that it does not
move, and it was leaking into reads that promise the opposite. The key now carries a digest of
the session's `SET SNAPSHOT` and `SET VERSION OF` settings — the settings rather than the
versions they resolve to, because resolving here would be a second place that has to agree with
the read path about what a pin means.

**One mutation removed rather than answered.** The snapshot's confirming second read is only
observable while something else is committing; on a quiet warehouse a copy of the first pass is
indistinguishable from a second read. Catching it needs a writer committing continuously and an
assertion that five attempts all fail, which passes or fails on how fast the machine is. Same
rule as the `fsync` calls: a flaky gate is worse than an uncaught mutation.

### Still open in Phase 3

`3.8` is complete — three fixed and `COR-19` recorded as not reproducible — `3.3` is three
findings of five, and one half of `3.6` is
recorded above as a format decision rather than a repair. Three pre-existing survivors in
`sankhya-publish` and one entry whose mutation does not compile were found while verifying this
work and are not yet closed; they are coverage gaps in the write path rather than defects in it.

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

### Phase 4 — what has landed so far

**4.1 A password is checked against something (`SEC-01`).** The entire check was that one had
been *presented* and was non-empty. There was no credential store, no hash and no comparison
anywhere in the workspace — and because the username is self-asserted, that means any client
connected as any user, including one this server had never heard of, by sending any byte string.
It was the single most serious finding in the report, and Phase 0.8 disclosed it in the startup
line and the documentation rather than leaving it implied.

`sankhya-credential` holds the verifier and nothing else: PBKDF2-HMAC-SHA256, in the four-field
shape PostgreSQL's SCRAM verifier uses. It is the only crate that reaches for `ring`, the same
way `sankhya-sandbox` is the only one that reaches for `libc` — a second crate deriving its own
key material is a second chance to get an iteration count or a comparison wrong.

Four decisions are worth stating.

- **The same switch as roles.** An empty `server.credentials` is the old behaviour, because an
  operator who has configured nothing has decided nothing and a server that began refusing every
  connection on upgrade is a server nobody upgrades. Naming one user decides the list is the
  list, and a user absent from it is refused.
- **The iteration count is in the stored line**, so raising the default does not invalidate the
  credentials already written down — which is what makes the default movable at all.
- **One refusal for both failures.** "No such user" and "wrong password" are the same message;
  telling them apart turns the login into a directory of who exists here.
- **`hash-password` exists**, and is answered beside `--help` before any configuration is read,
  because an operator whose configuration is broken is exactly the one who needs to write a
  credential into it. It reads the password from standard input: an argument is in the shell
  history and in `ps` output for every user on the machine.

The startup line now names three postures rather than two, and still capitalises the one that is
not authentication — `require_password` set with an empty credential map is exactly the old
behaviour and must still look wrong in a log.

**This is not SCRAM**, and §13.6a says so. PostgreSQL's challenge-response never sends the
password; this verifies one the client sent in cleartext, which is why the transport posture is
printed beside it. Getting from *never verified* to *verified against a stored key* closes
`SEC-01`. Getting from *cleartext over TLS* to *challenge-response* is a protocol change.

**The mutation catalogue found a break the test suite hid.** Three of the six new entries came
back `no compile`, which looked like badly written mutations and was not: eight other test files
construct `Settings` and none had the new field, so the whole server test build was broken while
`cargo test --test wiring` passed happily. Running one target is not running the suite.

**And the digest that says where a session reads from moved to where it is computed.** It began
in `wiring.rs`, was extracted to its own module when that file reached the line limit, and broke
the whole server test build --- eight test files pull the server's sources in by `#[path]`, so a
sibling module the binary can see does not exist inside a test binary at all. It is now
`Caller::position_digest`, in the crate that holds the settings it reads, which is where it
belonged: it is a property of the caller, computed from what the caller said.

That move exposed a hole the earlier work had left. Replacing the end-to-end `COR-20` test with
unit tests on the key removed the only thing checking that the wiring **passes** the digest, and
the mutation saying so came back `SURVIVED`. A cache key can be perfectly designed and never
reached; this warehouse keeps finding that shape, and this time the mutation catalogue found it
rather than a user. There is now a server test that a pinned session gains no cache hits where an
unpinned one does --- counted in hits rather than misses, because not every measure is cached on
every query and a rise in misses would therefore say nothing about whether an entry was shared.

**4.2 Column masks are applied (`SEC-02`).** They were declared in the policy, merged into the
guard, hashed into the visibility scope, reported by `masked_columns()` and `mask_for()`, and
documented in three places. Nothing read them: `scan` conjoined the row predicate and returned
every column exactly as the provider produced it. Worse than absent, because the documentation is
what an operator decides on.

The scan now ends in a projection that replaces each masked column, and three decisions are worth
stating.

- **The mask goes above the row filter, not below it.** A policy predicate is written about the
  data. Masking first would compare `'north'` against a row of stars and match nothing, which
  shows the principal no rows and looks like a working restriction --- the same failure
  `parse_predicate` already refuses to allow.
- **A predicate over a masked column is declared unsupported**, whatever the provider would have
  taken. Pushed down it is evaluated against the real value below the mask, and the row count
  answers the question without ever printing it: `WHERE email = 'ana@example.com'` matching once
  says she banks here. A control that governs only what is displayed governs nothing.
- **The masks are built from Arrow rather than from `concat`/`repeat`/`right`**, because `concat`
  treats a null as the empty string --- so the obvious implementation turns a customer with no
  address into `***`, inventing a value where there is none. A constant mask replaces nulls
  deliberately; a partial mask preserves them. They differ because leaving a null alone under a
  constant mask publishes which rows have no value.

A mask that cannot be applied --- text over an integer, or a column the table does not have --- is
refused when the table is opened rather than on the first query that selects it.

**The first pushdown mutation survived, and it was the fixture rather than the code.** `MemTable`
declines every filter, so `Unsupported` and the provider's own answer are indistinguishable
through it and the refusal was unreachable. It is now tested against a provider that reports
`Exact` and means it --- the same trap `LimitHonouringTable` was written for two phases ago, and
the second time the catalogue caught it before the commit rather than after.

**4.3 A name in a statement is not a path (`SEC-06`).** Three statement families built
`warehouse.join(DIRECTORY).join(format!("{name}.json"))` from a name a client typed, constrained
only to be non-empty and whitespace-free. `Path::join` replaces the whole path on an absolute
component and honours `..` on a relative one, so `CREATE AGGREGATION /var/tmp/x` wrote there and
`DROP SNAPSHOT ../_cubes/regional` deleted that. The worst target is a snapshot document: an
absent snapshot pins nothing, so deleting one releases the files the sweeper was holding back.

`sankhya-atomicfs::name` holds the rule and the three builders now return a `Result` rather than
a `PathBuf` --- which is the load-bearing part, because a check that can be forgotten will be.
There were already four copies of the path-building line and one copy of the restriction, in the
crate that needed it least.

- **An allow-list.** Letters, digits, `_`, `-` and `.`, ASCII only, with `.` and `..` refused.
  The deny-list it replaces has to stay complete against a NUL byte, a drive letter, a trailing
  dot Windows strips, a reserved device name, and a Unicode character that normalises to a
  separator. Non-ASCII is refused rather than normalised: two names differing only in
  normalisation form are one file on macOS and two on Linux.
- **The quoted spelling was the reachable one.** `CREATE CUBE ../x` is a syntax error, because
  the cube tokenizer builds a bare word out of alphanumerics and `_`; `CREATE CUBE "../x"` is
  copied verbatim, which is what makes a cube called `"Level"` expressible. The first mutation
  survived against the bare form and was caught only once the test used the quoted one.
- **A fourth site, which the audit did not name.** `CREATE TABLE … CLONE` does not escape
  upward, and the reason is an accident of `split_once('.')` rather than a check. What is wrong
  without one even today is `sub/dir`: the clone lands in a directory the catalogue does not
  scan, which is a table that exists and cannot be found.

**Two mutations survived first, and both were the test rather than the code.** One aimed a
traversal at a bookkeeping directory that did not exist yet, so the escape failed on `ENOENT`
and would have passed against a server with no check at all. The other used `DROP CUBE`, which
refuses a cube absent from the served set before it builds any path --- so the dangerous-looking
statement is the unreachable one and `CREATE` is where the write goes.

`4.4` through `4.8` are not started.

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
