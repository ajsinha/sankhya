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
already has 2807 tests, 741 mutations and a twenty-check gate, and all of it was green while the <!-- figures-as-measured-then -->
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

**4.4 A statement runs as somebody (`SEC-03`, `SEC-04`).** Three surfaces reached state without
asking who was asking.

**Flight ran every request as a literal.** The name was read from the metadata, checked
non-empty, and discarded; every request then executed as the subject `"flight"`, whose roles came
out of the same default branch as any unknown name's. A user an operator had deliberately left
out of `server.users` connected to that always-bound port and read.

The module's own comment explained why that was safe --- *"today loses nothing… every user of a
tenant gets the same roles"* --- and it was true when it was written. It stopped being true the
day roles became per-subject, with no code changing and no test failing. That is the failure mode
an explanation has and a check does not.

The ticket now carries the subject, and redemption checks it. Checking only the tenant did not
merely make a leaked ticket usable: it made one usable **at the entitlements of the person it was
issued to**, because the plan inside it was made under their roles and Flight deliberately does
not re-authorize at redemption. A `skhyft1` ticket no longer decodes --- honouring one would mean
choosing a subject for it, and every available choice is the hole this closes.

**`DROP SNAPSHOT` took no principal**, so any caller could drop any snapshot --- and a snapshot
holds files back from the sweeper, so dropping one releases them. `INVARIANTS.md` claims the
maintenance scheduler is structurally incapable of destroying retained history. It is; a
statement was doing it instead. The rule now is the subject who took it, or a caller who may read
every table it pins, which is what keeps an operator able to clean up after somebody who has left.
A document that cannot be read is refused rather than dropped: not knowing what it pins is not
permission to release it.

**`RESUME FEED` took no principal either.** `SHOW FEEDS` stays ungated --- it reports what the
server is doing and there is no table to check a scope against. `RESUME FEED` restarts an ingest
`ADR-0018` halted *because its source changed shape*, so resuming one decides that records of an
unknown shape should start landing in a table again. It is authorized against the table the feed
writes into.

All three refusals are the same sentence as "there is no such thing", because saying "you may not
touch that" confirms it exists.

**4.5 The sandbox (`SEC-09`–`SEC-14`).** Six findings, and they share a shape: a mechanism named
correctly, applied incompletely, and asserted by nothing. `ADR-0023` described a stronger boundary
than the one that existed, and §13.7a's table repeated the description.

- **`SEC-09`, the worker was root in its own namespace.** The map read `0 <server uid> 1`, and
  `execve` of a file with no file capabilities only drops the capability set when the effective
  uid is not zero --- so the interpreter started holding every capability the namespace had. It
  maps to `65534` now. **Outside** the namespace it is still the server's user and no
  unprivileged mechanism changes that: the kernel permits one map line whose parent-side id must
  be the writer's own, and a distinct id needs `newuidmap` setuid plus a `/etc/subuid` range. That
  is a deployment decision, and `ADR-0023` Decision 2 is amended to say so rather than closed.
- **`SEC-10`, a user function could kill the server.** `unshare(CLONE_NEWPID)` puts a process's
  *children* in the namespace and leaves the caller behind --- and the caller was the process that
  then `exec`ed into the worker. It forks once more now, so the worker is PID 1 of a namespace
  holding nothing else. Two consequences had to be handled: the process left outside must close
  every descriptor above the standard streams, because `spawn` does not return until every copy of
  its close-on-exec pipe closes; and the worker must be tethered with `PR_SET_PDEATHSIG`, because
  PID 1 of a namespace is reaped by nobody.
- **`SEC-11`, the jail held every binary on the machine.** *No subprocess* is delivered entirely by
  the empty jail, and the jail bound `sys.base_prefix` --- `/usr` on a system interpreter. It asks
  `sysconfig` by name now, binds the interpreter **as a file** rather than the directory it sits
  in, and refuses any answer that is a directory of programs. The promise is stated narrower than
  before: the linker's directories have to be there and some hold executables, so what is
  delivered is no shell, no `/bin`, and nothing in `/usr/bin` but the interpreter.
- **`SEC-12` and `SEC-13` are one cause.** Output was read only after the child exited, so a
  grandchild holding the pipe blocked the caller for ever --- inside a DataFusion accumulator on a
  Tokio worker thread --- and a child writing past a pipe's capacity blocked and was reported as
  having run out of *time*, which made the `OutOfRoom` arm unreachable at its shipped 64 MiB. Both
  streams are now read on threads of their own, and the bound is counted while the run is going.
- **`SEC-14`, the probe checked one mechanism out of fifteen.** It now calls the same function a
  spawn calls and names the step that refused.

**Two mutations were removed rather than kept, and the second is the more interesting.** One --- a
bounded wait on a stream --- survives because the PID namespace makes the case it guards against
impossible: nothing can outlive PID 1 of that namespace to hold a pipe. The other --- keeping the
descriptors the intermediate inherited --- *is* caught, and still had to go: with it applied the
output test blocks inside `spawn` with a full pipe, burns no CPU, never trips `RLIMIT_CPU`, and
hangs the gate rather than failing it. A mutation that hangs the build is worse than one that
survives, because a survivor is a line on a list somebody reads.

**4.6 What a refusal and a listing say (`SEC-16`–`SEC-18`).** None of these returns a row the
caller may not see. Each tells the caller something about rows they may not see.

- **`SEC-16`.** `SELECT nosuchcol FROM orders` was answered with every column of every table in
  the plan's scope. The half the caller typed is kept; the half they did not is gone. **The first
  attempt fixed the wrong function**: the message that reaches a client is built from the engine's
  string a second time in `plan_failure`, not from the detail the classifier carries, so changing
  `classify` alone left the leak exactly where it was and the tests were what said so.
- **`SEC-17`.** Bare-name claims were counted over every servable table with the authorization
  running afterwards. The visible half enumerated qualified names in a refusal; the half with no
  string in it made a hidden `payroll.orders` stop the caller's own `sales.orders` from resolving
  under its bare name, so anybody could ask whether a table of a given name existed somewhere they
  could not look. Authorizing first also makes the word mean what it says: a name is contested
  when *this caller* could mean two things by it.
- **`SEC-18`.** Five listings, each now filtered by the rule that already governed the thing being
  listed --- the fact table for a cube, the pinned tables for a snapshot. `SHOW AGGREGATIONS`
  keeps the names for everybody and shows the **source** only where `server.user_functions` is on,
  which is the switch that decides who could have created one.

**`SHOW FEEDS` took two attempts and the wrong one is instructive.** Filtering its rows by the
same rule removed a feed whose target table does not exist --- and a feed that halted *because its
table is missing* is exactly what an operator opens the statement to find. A control that hides
the thing it is meant to report is not a control. The name and state go to everybody; the **halt
reason** is what is withheld, because that is what carries the file and the record. Where the
table does not exist there is nothing to withhold about, so the reason is shown.

**One correction to 4.4.** It recorded that `SHOW FEEDS` had no table to check a scope against.
That stopped being true in the same change that wrote it: recording each feed's target so `RESUME`
could be authorized gave `SHOW` the table it was said to lack.

**And one thing this cannot yet demonstrate through the front door.** The cube-listing filter
needs a policy that grants *something* and not the fact table; a caller granted nothing is refused
a session before any listing is reached. `SEC-15` --- 4.8 --- is what makes that expressible in a
deployment, so the property is tested in process until then, and the test says so.

**And one flake fixed on the way past.** `the_bound_is_per_partition_and_scales_with_parallelism`
failed inside `check-all` and passed on its own: its *lower* bound asserts that the plan really
ran in parallel, which is a property of the machine rather than of the code, and a box busy
compiling the workspace serialises eight partitions into fewer. The upper bound --- a partition
that kept going past the deadline --- still has to hold every time. The lower one is now the best
of three attempts, the same treatment the contention measurement got in Phase 2 and for the same
reason: a gate that fails for a reason nobody can act on is a gate people learn to re-run.

**4.7 The audit, and the endpoint (`SEC-07`, `SEC-08`).**

**`SEC-08` first, because it is smaller.** `/metrics` is unauthenticated by Prometheus's
convention, and `sankhya_table_live_files` was labelled with the fully-qualified name of every
servable table --- so anybody who could reach the port could enumerate the warehouse. The module
justified this by saying *"no label may carry tenant data --- the metric catalogue enforces it
structurally"*, and it does not: a label is bounded by **cardinality**, not by content. **A test
required the label to be present**, so a server that closed the leak would have failed its own
suite.

What an alert fires on is now `sankhya_table_live_files_max`, which carries no label --- what
pages is that *some* table has too many files, and which one is a question `sankhya doctor`
answers to somebody who has authenticated. The breakdown is behind `server.metrics_detail`, off by
default, the same shape as `user_functions`. The runbook says which series to alert on and how to
get the name.

**`SEC-07` had two halves and both were serious.**

*What the record said.* The only append site hardcoded no row filter and no masks, recorded no
version, no statement and no row count, and passed the first two words of the statement where a
table belongs. §13.5 lists four fields as not optional and none was populated --- and the
restrictions field was not merely empty: it asserted `allowed, no filter, no masks` on statements
where a filter was applied. An empty field does not answer a question; a false one answers it
wrongly, and an audit is read as evidence.

A read now writes one entry per table **the plan scanned**. One per table because a restriction
belongs to a table and not to a statement; from the plan because recording every *authorized*
table attributes a row count to tables nobody read, and a substring search over the SQL would put
a table in an audit trail because its name appeared in a string literal. The graph epoch stays
`None` rather than being guessed.

**A first attempt recorded the statement verbatim, and an existing test refused it.** A statement
carries the values a query filtered on, and copying them into a durable log makes the audit a
second place the data lives --- with different retention and different access control than the
table. That test predates this work and is right: §13.5's four fields do not include the statement
text, and what `SEC-07` complained about is that the *shape* was being recorded **as the table**.
The shape now goes in the field for it and the table in the field for the table. The lesson is the
one this phase keeps producing: a finding names a symptom, and reading it as a specification
produces a second defect.

*Whether it survived.* `Chain` was a `Vec`, so the tamper-evident audit was erased by a restart.
It is written to `_audit/chain.jsonl`, one JSON object per line, synced before the statement is
answered, and read back at startup so the first record of a run links to the last of the previous
one. A failed write is counted and logged rather than swallowed, and a server with nowhere to
write says `IN MEMORY ONLY` in its startup line.

**One mutation had to be rewritten twice** before it compiled --- the shape that expresses "do not
write it down" has to keep the `Option`'s type --- which is a small thing, and the reason it is
recorded is that a mutation that does not compile is silently no coverage at all.

**4.8 A policy the binary can be configured with (`SEC-15`).** `start()` --- the only path the
shipped binary takes --- built a policy granting `reader` read on every discovered table, and **no
configuration key loaded a policy set at all**. The row-predicate enforcement, which §13.2 spends
four pages on and which is the best-tested code in the repository, had never run outside a test.

This is a different kind of finding from the rest of the phase. Nothing was wrong; the thing was
unreachable, and every page describing it described a capability nobody could configure.

- Each rule is **named**, and the name is the operator's: a refusal that says "rule 3 does not
  parse" is one somebody has to count to.
- The table must be **qualified**. A bare `orders` means one table today and two the day somebody
  adds a schema, and the rule would then apply to neither --- a contested bare name resolves
  nowhere.
- A rule that does not parse **stops the server**. A policy with a rule quietly dropped permits
  more than it says, and whoever wrote it believes it is in force.
- An absent policy is not an error, and the startup line says `NO POLICY CONFIGURED` in capitals
  --- a server that answers the same way whether or not somebody wrote a policy is one where
  writing a policy is indistinguishable from not writing one.

**This also closed the gap 4.6 had to leave open.** The cube-listing filter could not be
demonstrated through the front door because a caller granted nothing is refused a session before
any listing runs, and there was no way to grant *something and not the fact table*. There is now,
and `tests/disclosure.rs` exercises the row predicate and the column mask over a real socket
against a policy read from a file --- both of which had, until this item, only ever run in process.

**Phase 4 is complete.**

---

### Phase 5 — what has landed so far

**5.1a The audit chain is bounded, and its timestamp is real (`OPS-04`).** Phase 4.7 made the
chain durable and left it unbounded: the records were a `Vec` that only ever grew, appended on
every statement **and every catalogue listing** --- every `\dt`, every JDBC metadata call, every
tab-completion. Roughly 3 to 5 GB a day at a hundred statements a second, 26 GB at a thousand.

A running process holds the most recent 1,024 records. The file is the chain. Two figures
deliberately still describe the whole of it rather than the window: `len`, because a count that
shrank as records aged out is one nobody could compare against what they mirrored --- and
comparing it is the only way a truncated chain is ever noticed --- and `head`, kept separately
from the record carrying it because that record may be gone. Startup reads a window too, since
loading a year of audit before answering anything turns unbounded growth into an unbounded boot.

**The timestamp was `*clock += 1`.** An audit's times were `1, 2, 3`, restarting at 1 each boot,
under a comment saying *"a real deployment supplies wall-clock time here"* --- which none did,
because none could. That comment was protecting something real: a component that reads a clock
cannot be replayed. But the reproducible ordering was never the timestamp's job; it is the
record's `sequence`, which is what the chain links and what verification checks. So `at` is
wall-clock microseconds now, and the counter had no readers left and was deleted rather than kept
as a field nothing uses.

**Something was given up.** The digest covers the time, so two runs of the same statements no
longer produce the same head --- and a test relied on exactly that, comparing heads across two
fresh servers to prove the subject reaches the audit. It caught the change, correctly. The test
now asserts the property it was always about: that each record names and hashes the user who ran
the statement. Byte-identical replay across processes was never a property of an audit; it was a
property of a counter standing in for a clock.

**5.1b Nothing bounded memory (`OPS-05`, `OPS-06`, `OPS-07`).**

**The row limit was checked after the whole result was in memory.** `frame.collect()`
materialised everything and *then* the count was compared against the limit --- so a statement
returning ten million rows against a limit of ten thousand allocated all ten million first, and
the refusal arrived after the damage. A bound enforced by a check that runs afterwards is not a
bound. The result is streamed now and stopped at the limit, holding at most one batch past it.
Refusing rather than truncating is the older decision and stands.

**DataFusion ran on an unbounded pool.** There was no `MemoryPool`, no `FairSpillPool` and no
`DiskManager` anywhere in the workspace, and `sankhya-governor` states the consequence exactly ---
*"hash joins do not spill… it exhausts memory and the operating system terminates the process"*
--- while nothing acted on it. Queries now share a `FairSpillPool` sized by
`SANKHYA_QUERY_MEMORY_BYTES`, one gibibyte by default, with a disk manager behind it.

- **Fair rather than greedy**, because the failure is one statement taking the machine down and
  every other connection with it. A greedy pool serves whoever asks first and starves the rest; a
  fair one makes the expensive query fail *itself*, which is the query that should get the error.
- **Spilling is not a silver bullet.** A sort or a grouping that will not fit finishes slowly on
  disk. A hash join cannot spill, so it is refused whatever the limit is --- worth knowing before
  somebody raises the limit expecting the join to start working.
- **Zero means "say nothing", not "allow nothing".** A pool of zero bytes is a server that starts
  and answers nothing, and an empty environment variable is how one gets set.

**Three tests had to be rewritten before they proved anything.** The first asserted only that the
server survived an expensive query --- true of an unbounded pool too, on a machine with enough
RAM. The second used `SELECT 1` to check that a bound of zero falls back to the default; a
statement that reserves nothing is answered by a pool of nothing, so it passed either way. The
third made its point by materialising twenty million rows, which under the mutation took so long
the catalogue run had to be killed --- the same trap as the sandbox mutation in 4.5. Each now
asks the question cheaply first: a hash join against a megabyte, a sort against a bound of zero,
a modest overrun before the large one.

**5.2 An `accept()` error killed the server (`OPS-08`).** The PostgreSQL door's accept loop
was `accepted?`. The error propagated out of the serve loop and out of `main`, so the process
exited --- and `ECONNABORTED`, which is what a load balancer produces every time a health
check opens a connection and closes it before the handshake, is one of the errors it exited
on. The comment three lines below said *"a failed connection is that connection's problem,
not the server's"*; it described the **serve** error while the accept path did the opposite.

**The other two doors were wrong in the other direction.** The metrics endpoint and the
columnar door both had `let Ok((stream, _)) = accepted else { continue }`, which looks safe
and is a hot loop: a descriptor shortage does not clear because the loop asked again
immediately --- the retry fails instantly and the loop burns a core competing with the very
tasks holding the descriptors it is waiting for. And a listener that is genuinely broken
fails identically on every call for ever, so `continue` is a process that is up, answering
nothing, and reporting nothing. An orchestrator restarts a process that dies and stares at
one that lives.

Three loops, three answers, none right, and the disagreement was only visible to somebody
reading all three next to each other --- which is not how anybody reads code that lives in
three crates. So `sankhya-accept` decides, once: routine per-connection failures continue, a
shortage pauses fifty milliseconds before trying again, and anything unrecognised stops. The
default is **stop**, deliberately: an error nobody has classified becomes a crash with the
error in it rather than a silent hot loop an operator diagnoses from a CPU graph.

**And the shortage is now much harder to reach.** There was no connection cap --- the number
of connections was whatever clients asked for, each one a descriptor --- and the shipped
systemd unit set no `LimitNOFILE=`, so the server inherited the login default of 1024 and
reached it on connections alone before opening a single data file. The door serves 1,024 at
once and the unit asks for 65,535; past the cap the accept branch is simply disabled, so
callers wait in the kernel backlog and are served as connections finish. Refusing at the
door is the better failure: a client sees a connection error, which is a thing clients
retry, rather than the shortage landing on the connections already being served.

**The test sets the limit in a shell.** `setrlimit` is process-wide, so a test binary cannot
lower its own descriptor limit without lowering it for every other test in the same binary.
`ulimit -n 64; exec sankhya-server start` sets it for exactly one process --- which is also
how an init system does it, and is the reason the unit file now says so too. The test opens
a hundred and twenty connections against that limit and then asks the server a question:
before the fix, nothing was listening to ask.

**5.3 "I could not look" was recorded as "there is nothing" (`OPS-12`).** Fourteen places
read a directory as `let Ok(entries) = read_dir(x) else { return empty }`. That answers *"there
is nothing here"* to the question *"what is here?"* whenever the true answer is *"nobody could
tell"*, and the two are different claims wherever anything acts on them.

**The worst of it was the diagnostic.** An unreadable warehouse --- an unmounted NFS export, a
path with the wrong ownership --- produced no tables and no complaints, so the server started
and served an empty catalogue. `sankhya-server doctor`, the tool an operator reaches for at
exactly that moment, printed "0 table(s)", "Nothing to report" and exited clean, and the
documented hourly cron stayed green straight through a dropped mount. The machinery for saying
otherwise was already there: `doctor` has a third exit status meaning *"a check could not
run"*, and it is fed entirely from `discover`'s list of what it could not open. `discover`
simply never put the warehouse itself on that list.

**And the same claim where it decides a deletion.** `resolve` answered `Absent` for a
warehouse it could not list. A snapshot pins files only if its table resolves, so on an
unmounted export the pin was dropped and the sweeper was free to reclaim the files it was
protecting --- under a reader. That is the deletion `sankhya-server/src/snapshots.rs` was
already hardened against arriving through a different door, and it now has a `Resolved`
variant of its own rather than borrowing the one that means the table is gone.

**Not existing stays silent, everywhere.** A warehouse directory is created on first use, a
deployment with no feeds has no feed directory, and most warehouses declare no cubes and no
aggregations. Complaining about those would be a warning on every first start, which is how a
warning stops being read. The distinction is `ErrorKind::NotFound` against everything else,
made the same way in all five places.

**One left deliberately.** `materialised_shapes` reads the cuboid store, and an unreadable one
means the query is answered from the fact table instead --- slower, and not wrong. A cuboid is
a cache, and its own doc comment already says there is nothing to report.

**5.4 Maintenance was blind and partial (`OPS-10`, `OPS-11`).**

**The table list was a startup snapshot.** `tables_under` discovers rather than reads a
configuration file, and its own doc comment explains why: a configured list goes stale the
first time somebody creates a table, and the maintenance thread would then quietly not
maintain it, *which looks exactly like maintenance working*. The caller then took that
discovered list once, at startup, and froze it --- the same staleness arriving through a
different door. A table created after the server came up was maintained by nobody, for the
life of the process. The set is re-read at the top of every cycle now, a table is adopted with
a line saying so, and a table that has gone is released rather than failing for ever.

**A failed tick was discarded.** `Err(_) => continue`. A table whose compaction failed every
thirty seconds was invisible --- and the aggregate reclaimed-bytes figure kept climbing from
the other tables, so a dashboard showed a warehouse being looked after. Failures are counted
on the handle and reported when the error *changes*, once when it starts and once when it
stops, which is the hysteresis the declined-to-reclaim complaint already used: a line every
thirty seconds is a line nobody reads.

**And the crate had no `tracing` calls at all.** Four `eprintln!`s, with no timestamp, level or
target, in a binary that has installed a subscriber since before any of them were written --- so
an operator could not tell *when* a table stopped being maintained, which is the first question
anybody asks. They are structured events now, with the table as a field rather than interpolated
into the message.

**A worse bug fell out of testing this.** The failure-counting test would not go red: making a
table's `_delta_log` unreadable did not fail the tick. `read_actions_after` walks commits
forward and stopped on `!path.exists()` --- and `Path::exists` answers `false` for **every**
failure, including "the directory holding this file cannot be searched". So a log under a
half-mounted export, or with the wrong ownership, ended the walk at version zero and the table
replayed as **empty**: a `SELECT` against it returned no rows and *succeeded*. Zero rows that
are wrong is the failure this whole system is arranged against, and it was one `chmod` away.
`try_exists` distinguishes them.

**5.5 A diagnostic that never looked, and a catalogue of alerts that could not fire
(`OPS-25`, `OPS-26`).**

**`doctor` could never warn about a filling disk.** `check::storage_headroom` was written,
tested, exported --- and called by nothing. The hook for it had been there all along:
`collect::record` exists precisely because free space needs a reading the diagnostic crate
cannot take under `forbid(unsafe_code)`, and its doc says the caller that can measure it
passes the number in. No caller ever did. So the hourly cron the documentation recommends
would have stayed green until the write path stopped.

It measures with `df -P` rather than a syscall or a new dependency --- the same judgement
`soak/sample.rs` made when it read `/proc/self/status` instead of wrapping a crate around it.
A reading that cannot be taken is `None` and is *said*, never zero: zero free bytes is a
plausible reading and a catastrophic one, and a failure that returned it would page somebody
about a healthy disk.

**The only remediation on the only alert that can page named a binary that does not exist.**
`sankhya maintenance compact --table ...`, in the compaction-debt runbook and in the finding's
own remediation text. There is no `sankhya` binary; the CLI is a stub that prints "not built
yet" and exits 2. A remediation naming a command that is not there costs the person reading it
at three in the morning the time it takes to find out, and it is the moment they stop trusting
the rest of the runbook. Both now name the lever that exists: lower `maintenance.compact_every`
and send `SIGHUP`, which the server already reloads without a restart. There is deliberately no
hand-compaction command --- the server holds the warehouse lock and a second process compacting
the same tables is the second-writer failure `check-writers` exists to stop.

**Twelve of twenty-one documented error codes were produced by nothing.** The catalogue's own
first sentence said these were the codes "this system can produce". Four of the six that page
were among them, so an alert rule written from the document was permanently silent --- which
is indistinguishable from a healthy system right up until it is not.

Deleting them is wrong for the reason the catalogue itself gives: codes are permanent, because
removing one breaks every runbook and alert rule that references it. What was wrong was the
claim. `check-catalogues` now fails when a code nothing constructs is not declared unreachable,
and fails again when a declared one starts being produced and the note is left behind --- the
same stale-excuse guard `check-unsafety` and `check-mutation-coverage` already carry. The
generated document reads that list, so it says which codes cannot fire and why.

**Nine of the twelve wait on a subsystem that does not exist** --- the change-capture runtime
(`ING-00`), the archival tier, the governor that decides nothing. **Three do not, and they are
the worse half:** commit conflicts, cancellation and backup verification all happen today and
are reported through crate-local types that nothing maps onto their catalogue codes. Mapping
them is what remains of `OPS-25`; it is recorded here and in the `UNREACHABLE` list rather than
left to be rediscovered.

**5.6 What a statement cost before it read a row (`OPS-21`, `OPS-22`).**

**A correction to 5.1b first, because it was wrong in the way that matters.** `bounded_session`
built a fresh `RuntimeEnv` --- and therefore a fresh `FairSpillPool` --- on every call, so each
statement got its own gibibyte. Ten concurrent statements got ten, and the machine died exactly
as it had before, while the setting, its help text and this document all said the bound was what
a server's queries may use **between them**. A pool that is not shared is not a bound; it is a
per-statement allowance wearing a bound's name, which is worse than no bound because it reads as
solved. Fairness was the whole argument for choosing `FairSpillPool`, and with a pool each there
is nothing to be fair about. One runtime now, built once.

The test that was supposed to prove it did not. A front-door version --- two sessions, two hash
joins against a megabyte --- passes either way, because a per-statement pool refuses each of them
against a megabyte of its own. It was thrown away for one that asks the question directly: the
two sessions' runtimes, and the pools inside them, must be the same object.

**Checkpoints were written only by tests (`OPS-21`).** So every log replay in the system --- at
startup, on every statement's freshness probe, in `doctor`, in the diagnostic's file count ---
ran from version zero: one `exists()`, one read and one JSON parse per commit, per table, for the
life of the warehouse. The reason recorded for not wiring it was that `checkpoint_if_due` needs
the table's `Metadata` and the log crate had no reader, and that a checkpoint written from a
fabricated default would tell every external reader a schema the table does not have. The first
half expired --- `latest_metadata` exists now --- and the second still stands, which is why this
reads the metadata and skips a table that has none rather than supplying one.

**The freshness probe replayed every log from the beginning, holding the cache built to stop it.**
`warehouse::refresh` takes a `&LogCache` and then called `live_files` free-standing, so every
statement replayed every table's log from version zero to discover the ordinary case: that
nothing had changed. At a thousand commits a table that is a thousand file reads per table per
statement.

**A test here passed against the defect too, and the catalogue caught it.** It asserted that
`LogCache` caches --- a property of `LogCache`, true before the change and after it. The mutation
that reverts `refresh` to the free-standing call survived, which is exactly what a mutation
catalogue is for. The test now asserts the thing that separates the two: after a refresh, the
cache it was handed must already know the table.

**Not measured, and this document will not claim it was.** What changed is the cost model ---
from replay proportional to a table's whole history on every statement, to replay proportional to
what has arrived since the last one --- and a benchmark that demonstrates it belongs with 6.2,
where the benchmarks that do not exist are built. `OPS-22`'s remaining part, the per-column
HyperLogLog sketch carried on every file, is not addressed here.

**5.7 A door that cannot be moved, and a manifest that cannot be applied (`RUN-10`,
`RUN-11`).**

**Arrow Flight SQL was unconfigurable and unexposed.** `SANKHYA_LISTEN` and
`SANKHYA_METRICS_LISTEN` existed; `SANKHYA_FLIGHT_LISTEN` did not, so the columnar door could
be moved only by writing a configuration file --- and a container image is configured by
environment. Two instances on one host therefore always collided on 5434. The shipped
Kubernetes manifest declared 5433 and 9464 and nothing else, so Flight SQL, a headline feature
of this system, was unreachable in every deployment made from it.

The test for it found something small on the way: the server prints *"Arrow Flight SQL on
&lt;address&gt;"* **before** the transport binds, so a health check that trusted the banner
would race it. The wire door's banner, printed after its bind, is the one to copy. Recorded
here rather than changed, and the test waits rather than asserting at once.

**The Kubernetes manifest could not be applied by anybody.** It named
`ghcr.io/ajsinha/sankhya:0.1.0` and there was no Dockerfile, Containerfile or compose file
anywhere in the repository. It referenced a `persistentVolumeClaim` no manifest defined, and it
shipped no Service, so nothing routed to 5433 --- a manifest that starts a server nobody can
connect to, on a volume that does not exist, from an image that was never built.

All four are now here. The Dockerfile's builder image is chosen by the declared platform
baseline rather than by taste: `glibc` 2.28 is what `xtask/src/package.rs` promises, so the
build stage is Debian 11 and switching to Alpine would silently change the target. The runtime
stage is not `scratch` because the storage-headroom check shells out to `df` and TLS
verification needs a certificate store; a `scratch` image would report that it could not
measure free space on every run, which is honest and useless. The claim is one replica and
`ReadWriteOnce`, because the warehouse takes a lock and refuses a second server --- a volume two
pods could mount is a volume that lets the deployment scale itself into `COR-15`.

**And the gate that keeps it true.** `check-package` now reads every `image:` a manifest names,
fails when no Dockerfile exists, and fails when the tag and the workspace version disagree ---
because two version numbers in two files that nothing relates is how a manifest comes to name an
image that was never pushed. Both arms were checked against the defect they describe before
being believed.

**The image was built, and building it found the thing writing it had got wrong.** A Dockerfile
nobody has built is the same class of claim as a runbook naming a binary that does not exist, so
it was built: it compiles, `--version` answers from it, `df` is present for the storage check,
and it runs as uid 65532 as the manifest's `runAsUser` requires. The comment in it originally
said the builder was chosen to meet the declared `glibc` 2.28 baseline. **It does not.** Debian
11 ships `glibc` 2.31 and the binary the image carries needs `GLIBC_2.30` --- measured by
extracting it and reading its version references, not assumed --- so an image built from this
file will not start on the oldest platform the project says it supports.

That gap is now written down in three places and closed in none. Debian 10 is end-of-life and
its archive has moved, so pinning the builder to it is a build that breaks on a schedule nobody
controls; the honest route to 2.28 is a cross-toolchain with an old sysroot, which is work this
has not done. What is done is that the two numbers cannot drift silently: `check-package` holds
the measured figure against the builder image it was measured from, prints the gap on every run,
and fails when somebody changes the builder without re-measuring.

**5.8 A query log, and events with a time on them (`OPS-24`).**

**There was no query log at all.** The audit chain records every statement and is the right
home for *evidence* --- hash-linked, durable, tamper-evident. It is the wrong thing to read
when a server is slow: reading it means reading a chain rather than grepping a log, and it
carries no duration, so *"which statements are slow?"* and *"is this server busy?"* had no
answer anywhere in the system. One `tracing` line per statement now carries who ran it, the
statement's shape, how many tables it scanned, how many rows came back, how long it took, and
whether it was refused.

**The statement itself is not in it, and neither is the refusal's reason.** `ARCHITECTURE`
§17.1 makes query text tenant data, `check-logging` enforces it, and `SEC-07` settled the same
question for the audit. A planner's refusal frequently quotes what the caller typed --- `SEC-16`
was exactly that leak reaching a client --- so the log says a statement was refused and the
audit says which one it was.

**Two audit tests were codifying the leak.** One asserted the recorded shape was
`select region`; the other asserted, three lines apart, that nothing a caller supplied reaches
the audit **and** that `select region` was there. `region` is a column the caller named. Both
now require `select` and refuse `select region`, which is the property they were always about.

**And writing the test found the shape leaking too.** `statement_shape` took the first two
words, which is right for `create table` and `show feeds` and wrong for `select nosuchcolumn`
--- a column the caller chose --- and worse for `select 'a-secret'`, which is a value. It had
been doing that in the **audit** since 5.1a and nothing had looked. The second word is now kept
only when it is one of ours, against a closed list; anything else is treated as caller data,
because a shape one word too short costs an operator a little precision and one word too long
puts a literal into a durable log.

**Six runtime events still went to `println!`.** A feed that stopped, a feed that refused, a
quarantine expiry, a reload refused, Flight stopping. The audit's sentence was *"an operator
cannot determine when a feed halted"*, and that is exactly right: a `println!` carries no
timestamp, no level and no target, in a binary that has had a subscriber installed since before
any of them were written. The startup banner stays on stdout --- it is a human running a
command --- and the events that happen afterwards are structured, with the feed as a field
rather than interpolated into a sentence.

**A test asserting on a field could not see it.** `tracing_subscriber` defaults to ANSI on, so
this server was colouring output that normally goes to a file, to journald, or to a collector
--- escape sequences in every line, and a field an operator filters on reading as
`\x1b[3mfeed\x1b[0m\x1b[2m=\x1b[0mpostings`. Colour is now conditional on stderr actually
being a terminal.

**One thing this phase's own gate turned up, and then turned up again.** The concurrency
check's quiet-machine guard reads CPU idle from `/proc/stat`, and the measurement it guards ---
commits per second --- is bound by the **disk**. At the end of a `check-all` run, with the build
output still flushing, the cores were idle, the window opened, and the scaling assertion failed
describing the machine rather than the code. It was recorded rather than fixed, on the argument
that a guard right about the common case beats one that does not exist.

It happened a second time, on the next full run, which settles the argument.

**The reason it was invisible is worth stating, because `iowait` was already being counted as
busy.** `/proc/stat` attributes `iowait` only when a CPU is idle *and has a pending I/O of its
own*. A machine that has just finished a large build has nothing runnable — the compiler has
exited — while kernel flush threads write gigabytes of dirty pages. That work belongs to no
CPU's idle accounting, so the machine reads as genuinely, correctly idle while its disk is
saturated.

So the guard now reads the condition itself rather than a proxy for it: `Dirty` plus
`Writeback` from `/proc/meminfo`, which is a few megabytes on a quiet machine and several
gigabytes while a build drains. Past a generous ceiling the measurement **skips loudly** rather
than failing — which is what a measurement that cannot be taken should do, and is the
distinction the whole `SKIPPED` mechanism exists for. A platform that cannot be asked is still
not a reason to skip.

**Phase 5 is complete except for `OPS-21`'s remaining half and `OPS-23`.** Checkpoints are
written (5.6) and the double log replay is gone (5.6), but `OPS-22`'s per-column HyperLogLog
sketch --- 4 KiB per column per file, provably zero, merged on every plan --- is untouched, and
so is `OPS-23`. Both are performance work whose claims have to be measured rather than asserted,
which is 6.2's subject and is where they belong. `OPS-21`, `OPS-23` and `OPS-24` --- checkpoints that are never
written, the query log that does not exist, and the remaining `println!` runtime events --- are
not done either; `OPS-22`'s per-statement cost is 5.6 and the rest belong with it, because
every one of them is about what a statement costs and what it leaves behind. `OPS-10`'s other half --- exposing the maintenance
counters on `/metrics`, where an operator would look before reading a log --- belongs with the
observability work in 5.5 and is not done here: the handle now counts what needs exposing,
and nothing reads it.

### Phase 6 — what has landed so far

**6.1 and 6.2 The 24.9× does not reproduce, and there were no benchmarks to reproduce it with
(`PERF-01`, `PERF-02`).** Done together, because a false number cannot be retracted into nothing
--- something has to replace it, and there was no way to measure anything.

**`criterion` was in the pin set and used by no crate.** Zero `benches/` directories, zero
`[[bench]]` targets. And `[profile.bench]` was configured for a profile no target used, while
`check-performance` ran `--release` against a workspace with **no `[profile.release]`** --- so
the performance gate measured cargo's defaults, no LTO, sixteen codegen units. A gate that
measures a build nobody ships reports a number nobody can act on. There is a release profile
now, matching the bench profile on purpose: a benchmark and the gate that guards it must measure
the same build or a regression appears in one and not the other.

**What the retracted numbers were.** `ADR-0020` published **24.9× / 7.2× / 2.1×** for borrowing
a row instead of copying it, restated in `rows.rs` and again in `STATUS.md`. No code anywhere in
this repository's history produced them --- the audit searched the working tree, every branch,
`git log -S` on each figure, deletions and stashes. The tell was internal: against a fresh run
([bench: sankhya-functions/row-access]) the *copying* arm was 2.4× faster and the *borrowing*
arm 27× faster, only the fast arm was
anomalous, it was non-monotone in width, and 0.85 ms for a scalar reduction implied about
39 GB/s --- above memory bandwidth. **The fast arm was almost certainly deleted by the
optimiser**, its result being unused, while the copying arm survived because allocation has
side effects.

**Measured now, by a benchmark that is a build target and whose arms consume their results:**
14.3× at width 8, 5.1× at 64, 1.5× at 512. Two runs agreed within 6%. That is neither the
published 24.9× nor the audit's reconstruction of 2.2×, and the disagreement is the argument:
a ratio is a property of a machine, a dataset and a build, so the repository ships the
benchmark rather than the number.

**Two more tables are retracted rather than replaced.** The per-kernel figures compared
`vector::dot` and its neighbours against what the same call returned *before* --- and the
"before" is the sorted-only implementation, which no longer exists. A before-and-after ratio
whose "before" has been deleted cannot be re-run by anybody. *"10 to 15 times faster"* described
a lane-parallel loop this system does not use, and the same paragraph already said the 15× was
never available; it is marked unmeasured where it stands. The *"agreement on 200,000 randomized
vectors"* row named a one-off experiment nobody can repeat, and is replaced by the two property
tests that check the same properties on every build.

**The reduction table is replaced with a measurement, and with what the measurement is not.**
`3.3× / 3.0× / 3.0×` at 64 / 512 / 4096, against the published `1.5× / 2.0× / 2.7×`. The
fallback runs only where the fixed-point route *declines*, so the two arms sum different numbers
by construction --- the benchmark asserts each reaches the route it is named after, and the
first version of it used a spread of exponents the fixed-point route takes without difficulty,
which that assertion caught. The published table implied a speedup on identical input, which
cannot have been measured either.

**And the rule that let all this stand has a second half now.** *"Every claim about speed
carries its number"* is what made a figure in prose read as measured. It now reads *"and the
number carries the benchmark"*, and `check-benchmarks` builds every benchmark target on every
run and fails when a crate that publishes figures has none --- because a benchmark that stops
compiling is a figure that has quietly stopped being reproducible, which is the state these
tables were in.

**6.3 Fifteen of eighteen objectives unmeasured, and seven not in the table at all
(`PERF-05`).** The table reporting the state of the `NFR-PERF` objectives listed eleven. Seven
were absent — not recorded as unmet, not recorded as untested, simply not there — including
`NFR-PERF-06`, which is the function catalogue's own requirement and the workload its
performance claims are about.

That is a worse failure than a bad verdict. An objective recorded as unmet is a decision
somebody took; an objective that is not in the table is one nobody has to think about, and to a
reader scanning the table for red it reads exactly like one that is fine. It is the same shape
as a metric nothing emits and an alert that can never fire: **absence rendering as health.**

So the fix is a check rather than an edit. `check-objectives` fails when a stated objective is
missing from the table that reports on it, and it has no opinion about whether one is met —
omission is the failure it catches, because omission is the one a reader cannot see. It
reproduced the audit's seven independently on its first run. All eighteen are now listed:
three met, and the rest marked unmeasured or not-claimed with the reason. `STATUS.md`'s
"Performance objectives met" row now says *met for three of eighteen*, and its cancellation row
says *demonstrated, not measured* — the two-hundred-millisecond bound in `NFR-PERF-16` is
established by nothing and that row said "Met" against it.

### Two defects a fresh reading found, both introduced by this remediation

Three reviewers were asked to read the documentation against the code rather than against other
documents. They found what is below, and it is worth separating from the documentation work:
**these are live defects in code written during Phase 5, and both are in mechanisms built to
prevent exactly the class of failure they exhibit.**

**The audit stopped being written to disk after 1,024 records, silently, with its alert at
zero.** `append` took the record it had just added by index — `chain.records().get(chain.len())`
— and those two numbers count different things *on purpose*. `len` is the whole chain, kept
whole because a count that shrank as records aged out is one nobody could compare against what
they mirrored, and comparing it is the only way a truncated chain is ever noticed.
`records()` is the retained window of 1,024. So past the thousand-and-twenty-fourth record the
index was out of range, the write silently stopped, and the `Some(Err(..))` arm — the one that
increments `sankhya_audit_unwritten_total`, which pages — became unreachable.

On a restart it was immediate rather than eventual. Startup loads through `read_windowed`,
which counts every line into the total while keeping a window in memory, so on any warehouse
with more than a window of history the **first** statement of the new process missed and the
audit was never written again for that process's entire life. The tamper-evident record, and
the alert for its absence, both off, together, from one arithmetic assumption.

Introduced by 5.1a, which is the item that made the chain durable and bounded. The test now
runs 1,200 statements and reads the file; it failed at exactly 1,024. The fix takes the last
record rather than computing where it ought to be, which is a fact about `append` rather than a
relationship between two counters that are deliberately not the same number.

**And the reachability gate from 5.5 was defeated by a substring.** It asked whether the
sources contain `Error::CoverageGap {`, and `sankhya-plan` has a `SpliceError::CoverageGap`
whose text contains that string. So `SNK-S0001` read as producible while nothing in the
workspace could raise it — a **thirteenth** unreachable code, `Class::Fatal`, with a runbook
written for it and an alert rule on it that would never have fired. Four of the six codes that
page cannot fire; the catalogue said three. The check built in 5.5 to find precisely this
failed at precisely this, and a reviewer found it by reading the gate rather than trusting it.

**Since closed, in the half that could be closed.** `SpliceError::CoverageGap` now converts to
`SNK-S0001`, carrying the positions --- the remediation says to investigate capture continuity
and retention, and neither question can be asked without them. `BeyondFrontier` converts to
`SNK-T0003` rather than to the fatal code, because being early is a freshness question that
resolves by waiting and paging for it would be wrong. `Overlap` converts to `SNK-S0005`: the
rows are present twice, not missing.

That exposed the gate asking the wrong question. `UNREACHABLE` asks *does anything construct
this?*, an operator asks *can this fire?*, and the two came apart the moment the mapping
existed --- a code with a conversion and no call path would have read as **produced**, which is
the same documented lie in the other direction. So there are two lists now, guarded in opposite
directions: `UNREACHABLE` fails the build when an entry becomes constructible, and
`MAPPED_BUT_UNREACHABLE` fails it when an entry stops being constructible, because that entry
claims a mapping exists.

**And `SNK-S0002` turned out to be the sharper find.** `FR-TIER-23` requires a conflict to make
unified queries on the affected table fail *with a typed error*. It produced `Option::None` ---
no code, no remediation, no name for what went wrong. `Unservable::NotReconciled` was declared,
documented in `unify::plan`'s `# Errors` as returned *"when the witness is for another table"*,
and constructed nowhere; `plan` takes the table **from** the witness, so it could not detect the
case its own documentation described. The refusal now belongs to `Registry::servable_or_refuse`,
where the witness is obtained, and maps onto the code.
It requires a word boundary now, with a test that a different type's variant of the same name
does not count.

**A third structural finding, and the one with the widest reach.** `R4` — *documents assert;
nothing checks the assertion* — survived **inside the fix for `R4`**. The status check compares
the documents that *declare* a `**Status:**` line, and a document with none simply did not
participate. A search across `docs/book/` found no such line in any of its twenty-nine
chapters: the entire book was invisible to the check written to stop documentation drift, and
opting out cost nothing. That is how `part1/01-introduction.md` — in the section headed *"the
one to read before believing anything else in this book"* — still carried **"Complete: M0
through M8, M10 and M13"**, the exact sentence item 0.7 retracted from thirteen documents, and
how `part5/26-roadmap.md` still called M13 complete. An absent header in the book is now a
failure, all twenty-nine chapters carry the canonical line, and both false claims are corrected
with the reason they survived recorded beside them.

**6.4 The price of determinism was stated nowhere (`PERF-07`).** Everything the reduction
decision published was a ratio against **this project's own previous code**, so a reader came
away believing the kernels had become fast. They had become faster than they were.

Measured against an ordinary `iter().sum()` — the thing a reader would have written, and what
every other engine does — the guarantee costs **57× at eight values, 35× at 64, 10.7× at 512 and
12.2× at 4,096**. The narrow case is the expensive one and narrow is the common one here: a
window of readings, a term structure, a short curve. `exact_sum` accumulates into an `i128`,
which cannot be autovectorised, and walks the values more than once, so at eight values the
fixed overhead is the whole cost.

The trade is defensible — a warehouse whose totals move when the machine is busier is not one
anybody can reconcile against — but only with the price on the page beside it, which is what
`ADR-0020` now carries. An independent audit reconstructing this measured 32× at width 8 and 6×
at 4,096; these figures are from a different machine and build and are **worse at both ends**.
They are published as measured rather than reconciled to the friendlier number.

**6.6 A protocol declaration that was parsed and thrown away (`FMT-02`).** `Action::Protocol`
was read out of the log and discarded — the replay's arm was `Action::Protocol { .. } => {}` —
and no comparison against a ceiling existed anywhere in the workspace.

That is not a missing feature, it is a wrong answer. **Reader version 2 is column mapping**, so
physical column names no longer match logical ones and a reader ignoring the mapping returns
**every column as null**. **Version 3 brings deletion vectors**, so a deleted row stays in its
file with a vector beside it recording the deletion, and a reader ignoring the vector serves the
file whole and **returns deleted rows as live**. A table another engine had upgraded was read
anyway, with this reader understanding only the parts of it that happen to look like version 1.

The protocol's entire purpose is that a writer declares what a reader must understand. A reader
that ignores the declaration has made the declaration pointless, and `FR-OPS-12` already states
the rule for the write side: refuse a table whose protocol this build does not fully support,
and report degradation rather than silently misreading. This is the read half. The ceiling is
checked in `read_actions_after` — the one place every reader passes through — because a ceiling
enforced in some readers and not others is a table that is refused by a query and served by a
compaction.

**The rest of the `FMT` family is not closed.** Checkpoints still write `partitionColumns`,
`configuration` and `partitionValues` empty (`FMT-03`); an unknown action variant still ends a
table, which is the same defect inverted — fatal on a benign unknown action, silent on a
semantics-changing unknown field (`FMT-04`); aggregation documents are still read by substring
scan over the whole file, including the author's own source (`FMT-05`); feed positions still
parse "never run" from any JSON object (`FMT-06`); and `deny_unknown_fields` appears nowhere
(`FMT-07`).

**6.5 A refusal that was lifted, and the eight places nobody told (`FEA-05`).** QR, SVD and
eigendecomposition ship. `crates/sankhya-math/src/decompose.rs` implements them by Jacobi
rotation on symmetric input, refusing a non-symmetric matrix rather than symmetrising it, and
they are registered as six SQL functions. Eight documents and two module comments said they were
**deliberately absent**, with the reasoning — a subtly wrong SVD produces plausible singular
values, which is worse than none — stated each time.

The reasoning was right when written. The decision changed for a defensible reason. What did not
happen is the retraction, and one of the comments sat three lines above the module declaring the
code.

**This is the worst class of claim in the repository, and it is worth saying why.** A reader
checks a capability; they do not check a refusal. A refusal is the one statement a reader is
entitled to treat as permanent — it is why `docs/book/part1/04-what-it-is-not.md` was the most
credibility-earning document in the set before it was folded into `STATUS.md`. Reversing one
silently spends exactly the credit that document earned.

Corrected in all of them, each as a marked retraction rather than a silent edit, and each
carrying the thing that *is* still true: the decomposition family is order-fixed and
reproducible run to run and is **not compensated** — it does not route through
`deterministic_sum`, and it is the family a risk calculation uses. That distinction was
documented as one property until a reviewer read the code.

**The rest of `FEA` is not closed and is now stated in one place rather than fourteen.** There
is no write path from SQL; there is no change-capture runtime, so the CDC crate carries no
client dependency and cannot open a connection; the graph is registered against a freshly
constructed empty catalogue on every session and can never answer; the pack loader is not wired,
and the two "flagship packs" the README described never existed — `packs/` holds telemetry,
logistics and an adversarial fixture. Cube hierarchies are validated and ignored.

### The adversarial review

Five reviewers were asked to break this system rather than confirm it works: correctness,
security, the gates, operations, and the documentation's claims. The operations reviewer ran
it — built it, started it, killed it, corrupted a Parquet file, filled a disk, and followed the
runbooks. What follows is what they found and what was done. **Several are defects introduced
by this remediation**, which is the most useful thing about the exercise.

**The columnar door authenticated nobody, and it is on by default.** It read a **sankhya-user** metadata
header, checked it was non-empty, and served that user's session — with no credential
of any kind. The wire door refuses a connection when `server.require_password` is set and no
password arrives, and then verifies what did arrive against `server.credentials`. Worse than an
open door: `Server::principal` stamps the record `Authentication::Password` when passwords are
required, so the audit would have said *authenticated by password* about a caller who presented
none. Both doors call the same `Handler::authenticate` now, from `caller_of`, which every
request passes through — two call sites is how one of them comes to be missed, which is the
whole defect.

**A clone read under `SET SNAPSHOT` or `SET VERSION OF` returned zero rows and succeeded.**
`resolve_as_of` passes no `inherited`, and `ADR-0016` Decision 1a is that a clone's log names
none of its origin's files — so it resolved a log naming nothing, and an empty file set is
answered with `EmptyExec` rather than an error. The tag said `SELECT 0`, on the one feature
whose entire purpose is a reproducible report. The ordinary read path has always branched on
`inherited`, under a comment naming this exact failure; the two time-travel paths were written
afterwards, never given the branch, and copied `inherited` into the table they built without
using it to build it. The test prints *"the clone answered 0 rows under a snapshot and 1000
without one"*.

**The mutation runner was not checking that a test ran.** `judge()` returned *caught* for any
non-zero exit that was not a compile error — and `cargo test -p xtask --test package`, for a
crate with no `tests/` directory, prints *"no test target named `package`"* and exits 101. Two
entries were passing on that, and **both survive their real suite**. This is `R3` — *gates
report green when they measure nothing* — inside the mechanism built to detect exactly that. A
verdict now requires libtest to print `test result: FAILED`; `--check` validates that a named
target exists and that no two entries name one site; a hang is its own verdict rather than an
exception thirty minutes in. The catalogue is **907 distinct defects, not 909**.

Writing the tests those two entries needed, the first fixture used `- image:` where the parser
requires `image:` at the start of the line — so both rejecting tests passed *vacuously*, through
the check's no-images-found branch. The same failure mode, in the test written to fix it, caught
only because the accepting case was written too.

**Two servers over one warehouse were permitted, and corrupted the audit permanently.** The lock
was `<data_dir>/warehouse.lock`, and the data directory is a per-process setting — so two
servers differing only in `SANKHYA_DATA_DIR` each took a lock, both started, and neither said
anything. They then appended to one `_audit/chain.jsonl` from two chains that each began at
sequence zero, and the next start reports that the log has been reordered. There is no way back
from it, nothing observes the state while it is happening, and the shipped deployment's
`replicas: 1` is documented as a correctness constraint resting on this lock.

The first fix was wrong and is worth recording: deriving the file's *name* from the warehouse
while leaving it in the data directory changes nothing when the data directories differ — it is
still a different file. The lock lives in the warehouse now, under `_locks/`, beside `_audit/`
and `_snapshots/`, which discovery already skips.

**And chasing that exposed a second defect.** The test helper wrote a top-level `warehouse:`
key, which the loader does not read — so every server it started ran against the **default**
warehouse and created bookkeeping in the repository. The tests passed regardless, because the
lock they were about lived in the data directory, which the test does control. Moving the lock
is what made it visible.

**`doctor` reported CLEAN on a warehouse that is not there.** One transposed character in a path
gave `0 table(s)`, `Nothing to report`, exit 0. `discover` exempts `NotFound` deliberately — a
warehouse is created on first use, and complaining would warn on every first start. That
reasoning does not transfer to `doctor`, which creates nothing and is the one thing run from
cron forever: the case it exists to catch *is* the exempted one. A decision from 5.3, right for
the server and wrong for the diagnostic.

**An unreadable table directory was skipped in silence.** `Path::is_dir()` answers `false` on
`EACCES` exactly as it does on absent, so a table whose directory permissions changed was
classified *not a table*: the server started, said nothing, `doctor` reported clean, and the
only symptom was a client being told the table does not exist. One level down — an unreadable
`_delta_log` — was already loud. The directory above it was the gap, and a `chown` that missed
a directory is the ordinary way to arrive there.

**`SIGHUP` killed the server when maintenance was disabled.** The handler was installed only
when a maintenance thread existed, and `maintenance.interval: 0` produces none — so the signal
took its default disposition and the process died with no drain and no line. That is reachable
by the ordinary path: the live-files gauge pages regardless of whether maintenance runs, so the
alert fires, the operator opens the compaction-debt runbook, follows its remediation — which is
`kill -HUP` — and kills the server, which `Restart=on-failure` brings back to fire again.

**And moving the lock found three tests that were not testing what they said.** Each was named
*"it survives a restart"* and each started a **second server on the same warehouse** with a
different data directory while the first was still running — one of them under a doc comment
reading *"a second server over the same warehouse, which is what a restart is"*. It is not: it
is the two-writer state, and the tests could only be written that way because the lock was
somewhere a second server would miss. They stop the first server now and reuse its data
directory, which is what a restart does and what makes the audit and the diagnostic history
part of the thing being tested.

**The protocol ceiling was bypassed whenever a checkpoint existed.** `FMT-02`'s refusal went
into `read_actions_after`, which reads JSON commits — and `live_files` starts from a checkpoint
and then reads only the commits **after** it. The protocol lives on a checkpoint row whose `add`
is null, and the reader skipped exactly those rows, so a reader-version bump made before the
checkpoint was parsed by nothing.

That produces the state `log.rs` warns against by name: a table **refused by a query and served
by a compaction**. The compaction is the damaging half — it reads the raw Parquet, ignores the
deletion vectors a version 3 table depends on, and commits the result. Logically deleted rows
come back, into a file every external reader will now believe, written by a process reporting
success. A backup taken from it, and a restore drill *verified* against it, are the same shape.

**And the fix was masked by a fallback that is right for a different reason.** Every checkpoint
failure fell back to a full replay, because *"a checkpoint carries no information the log does
not"* — true of a corrupt checkpoint, false of one declaring a protocol this build cannot
honour. That is not *this checkpoint is unreadable*; it is *this table is not one I can read*.
The refusal propagates now; everything else still falls back.

**Still open, and named rather than closed:** seven of eleven metrics export no series until first use,
including the only page with no lead time. The maintenance counters are exported nowhere. Four
runbooks are for codes that cannot fire. `check-benchmarks` verifies that two hardcoded
directories are non-empty and associates no figure with any benchmark.

**Still open in Phase 6:** the rest of 6.6, 6.7 and 6.8.

---

---

# Phase 5 — Operability

| | Finding | The failure |
|---|---|---|
| 5.1a | `OPS-04` | The audit chain is an unbounded in-memory `Vec`, and its timestamp is a counter |
| 5.1b | `OPS-05`–`OPS-07` | Nothing bounds memory |
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
| 6.7 | `ING-00` `ING-08` `ING-09` | ~~There is no change-capture runtime; reconciliation compares nothing~~ — **closed as far as it can be, 2026-09-06.** `ING-00` stands and is not closable here: a capture runtime is a PostgreSQL replication client and a milestone, not a remediation, and it is named as the reason behind eleven other entries rather than left implicit. `ING-08`'s self-comparison is gone — the soak's expectation comes from the writer as rows are written, and it flushes the accumulator first, because absorbed rows sit in a buffer until a partition is worth a file. `ING-09` is stated where a reader looks ([`ARCHITECTURE.md`](ARCHITECTURE.md) §3.8) rather than only in the audit, `WriteStrategy`'s doc comment stopped implying a merge path exists, and `_sankhya_commit_ts` is **null instead of `1970-01-01`** — a wrong instant a reader cannot tell from a real one is worse than a missing one |
| 6.8 | `RUN-08` `RUN-09` `RUN-15` | ~~The stale transcripts, the corruption demo that proves nothing, and the small stumbles~~ — **closed 2026-09-06** |

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
