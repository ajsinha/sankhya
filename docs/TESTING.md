# SANKHYA — Testing and evidence

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> How this repository decides that something is true. The gates that run on every build, the
> mutation catalogue that asks whether the tests would notice a defect, the benchmarks that
> have to exist before a speed figure may be published, and — at the end — what none of it
> catches.
>
> This document replaces `INVARIANTS.md`, the book's testing and invariants chapters, and
> `SOAK.md`. Those said overlapping things and drifted apart; where they disagreed, the copy
> furthest from the code was the stale one, every time.

## 1. The argument

A test proves that code does what a test says. It does not prove that the test would notice if
the code stopped. Nor does a green build prove that the checks ran, that they measured
anything, or that the thing they measured is the thing a document claims.

Twelve production-readiness audits found **129 things** in a repository whose gate was green.
Not one of them was found by a failing test, because none of them made a test fail. They were
found by people reading code against the documents describing it. That is the fact this
document is organised around, and the correct reading of it is that 129 is a **lower bound**.

So the evidence here comes in four kinds, and each answers a question the previous one cannot:

| Kind | Answers | Fails when |
|---|---|---|
| **Tests** | does it do the thing? | behaviour changes |
| **Gates** | does the *repository* still hold its own rules? | a rule is broken anywhere, including in a document |
| **Mutations** | would the tests notice if it stopped? | a deliberate defect survives the suite |
| **Benchmarks** | is the number real? | a published figure has nothing that produces it |

## 2. The gates

`cargo run -p xtask -- check-all` runs every check below and fails the build on any of them.

**This table is generated.** `cargo run -p xtask -- gate-table` emits it from the dispatch arms
themselves, and `check-docs` fails when what is here disagrees with what that prints. It is
generated because three documents previously carried a hand-typed version, all three said
*twenty*, all three were written when that was true, and none of them moved — by the time
anybody counted there were twenty-six. The five missing from the longest-lived copy were
`check-durability`, `check-mutation-coverage`, `check-benchmarks`, `check-attribution` and
`check-unsafety`: between them, the checks that keep durability and every published speed
figure honest. A hand-maintained inventory of a moving set is a second source of truth that
decays in silence.

<!-- BEGIN GATE TABLE -->
| Check | What it refuses |
|---|---|
| `check-atomic-writes` | a publish path that writes in place rather than renaming |
| `check-attribution` | a dependency whose licence notice did not follow it |
| `check-benchmarks` | a published speed figure that names nothing which produced it, or names something that does not exist |
| `check-build-tree` | a `target/` large enough to take the machine with it |
| `check-catalogues` | a metric nothing emits, an error code nothing can raise, or a pageable thing with no runbook |
| `check-concurrency` | a commit path that stops scaling with tables |
| `check-doc-numbers` | a figure in prose that no longer matches what produces it |
| `check-docs` | a broken link, a stale pin claim, a crate that does not exist, or a document that will not say what it claims is built |
| `check-dupes` | two versions of one heavy dependency in the graph |
| `check-durability` | a durable writer that syncs its bytes and not its directory entry |
| `check-features` | a feature flag that changes behaviour nothing tests |
| `check-invariants` | a rule naming a check that nobody runs |
| `check-layers` | a dependency pointing the wrong way through the layer graph |
| `check-lints` | the denied lint set across every target, and any warning at all from the shipping build |
| `check-loc` | a source file past the length a person reads in a sitting |
| `check-lock-order` | a nested lock acquisition that is not declared |
| `check-logging` | a log statement recording something a caller supplied |
| `check-mutation-coverage` | a crate that decides something and has no mutation entry |
| `check-mutations` | a catalogue entry that no longer matches the source it names |
| `check-objectives` | a service-level objective missing from the table that reports on it |
| `check-package` | a manifest naming an image nothing builds, or a drain shorter than the grace period |
| `check-performance` | the `NFR-PERF` objectives --- **not** part of `check-all`: it needs a quiet machine and minutes, and CI does not opt in |
| `check-surfaces` | a SQL surface or a crate that nothing can reach |
| `check-tests` | the test suite, and a test that passes by not running |
| `check-unsafety` | a third crate writing `unsafe`, or an opt-out that outlived its reason |
| `check-vocabulary` | a core crate naming a domain concept it must not know about |
| `check-writers` | a second writer to a warehouse |
<!-- END GATE TABLE -->

### The rules each gate holds, and why each was written

Every row below is a rule this system holds and the check that refuses to let it break. They
are here rather than in a document of their own because a list of rules and a list of the
checks enforcing them are the same list, and keeping them apart is how one of them came to name
a check nobody ran.

`check-invariants` reads this table and fails when a rule names a check `xtask` does not run —
so a rule cannot become decorative by having its enforcement quietly removed.

| Rule | Why it exists | Enforced by |
|---|---|---|
| **`sankhya-publish` is the only writer to a warehouse** | A second writer is a path that does not get the guarantees the first one enforces. While the CDC pipeline wrote its own files, its tables had no partition columns and violated `FR-STORE-20` — which every table on the other path satisfied | `check-writers` |
| Data **in** is `sankhya-ingest`; data **stored** is `sankhya-publish`; data **out** will be its own crate | Three responsibilities, three boundaries. Ingest decodes and conditions whatever arrives — Postgres capture, and later files, streams and API calls — then publishes | `check-writers`, `check-layers` |
| A crate may depend only on lower layers, or on its own | Cycles make the build order a matter of luck and make a change's blast radius unknowable | `check-layers` |
| Domain vocabulary never appears in a core crate | Risk and AML are *use cases*. A domain word in the engine is the first step to an engine that only serves one industry | `check-vocabulary` |
| No file grows past 1,500 lines of code | A file nobody will read in one sitting is a file whose invariants nobody knows | `check-loc` |
| The Arrow, Parquet, DataFusion and `object_store` family is exact-pinned, with no duplicate versions | Two Arrow majors make identically named types incompatible. That is a correctness hazard, not an inefficiency — see [ADR-0001](adr/0001-dependency-pin-set.md) | `check-dupes`, `check-features` |
| Every crate compiles under the workspace lint policy, with `unsafe` forbidden | A policy that is not run is a policy that is not held. It was not running for a while, and the milestone that was meant to enforce it had passed | `check-lints` |
| A figure in prose matches the repository | Seven documents claimed a test count that was two hundred short | `check-doc-numbers`, fixed by `sync-doc-numbers` |
| A source path named in prose exists | `GUIDE.md` promised its examples were executed by a file that did not exist | `check-docs` |
| Every document's status line names every milestone in progress | Seven documents **agreed** on a status that was wrong. Agreement is not accuracy | `check-docs` |
| Every metric, error code and platform is documented from its declaration | A hand-written table is correct the day it is written | `check-catalogues` |
| A generated document says it is generated | One that does not gets edited by hand, and the edit disappears | `check-catalogues` |
| No log statement records what a caller supplied | A log line carrying tenant data is a disclosure that survives in backups, and `#[instrument]` without `skip_all` records every argument | `check-logging` |
| A release binary starts on the oldest platform it claims to support | The build machine's glibc is not the deployment target's, and it cannot tell you that — see [`PLATFORMS.md`](PLATFORMS.md) | `check-package` |
| No lock is taken while another is held, unless the pair is declared with its order | A deadlock is not found by testing. A race appears under load — run it enough and the bad interleaving happens — but a deadlock needs two threads taking two locks in opposite orders at the same moment, and while only one path holds both there is no order to reverse and no amount of hammering finds anything. **The code that holds two locks is not the bug; the code written six months later that holds them the other way round is**, and by then the first ordering is invisible. Two such places existed on 2026-08-29: `QueryLog::record` held the map's read lock across the ring's, under a comment claiming it did not, because in edition 2021 a temporary in an `if let` scrutinee lives to the end of the block; and `CubeCatalog::resolve` held `cubes` across `declared` | `check-lock-order` |
| Every crate is reachable from something that ships, or listed with a **milestone** | The narrower version of this rule — only crates registering SQL functions — let about 2,600 lines through: a REST surface, a capture source, a counting allocator nothing installs, a set of port traits, and an entire declarative pack tier. Widening it to plain reachability immediately found Arrow Flight SQL, which `GUIDE.md` §7a documents and no binary can reach. A crate is a claim the repository makes about itself, and a milestone is what makes the claim keepable | `check-surfaces` |
| Every SQL surface is reachable from the server | Four crates registering SQL functions turned out, in one day, to be unreachable from the thing that serves SQL — each found by accident. A capability nothing reaches is indistinguishable from one that was never built | `check-surfaces` |
| No writer makes a file visible by writing to the path a reader will open, and no writer claims a name by first checking it is free | The technique was already implemented correctly three times here and wrongly four, because a three-line technique gets retyped rather than reused. The wrong half of it cost a commit: `commit` checked that a version was absent and then renamed onto it, and `rename(2)` replaces its destination silently — so two committers both saw the version free and the second overwrote the first, with no error to either. Seventeen hundred tests could not see it, because every one had a single writer | `check-atomic-writes`, `sankhya-atomicfs` tests |
| Every third-party package is attributed with its licence, and every upstream `NOTICE` travels with the binary | MIT, Apache-2.0, BSD, ISC and Unicode-3.0 each require the copyright notice to be reproduced with a **binary** distribution, and Apache-2.0 §4(d) requires upstream `NOTICE` files to be propagated. There was no such file, so shipping breached 426 permissive licences at once — the only outright distribution blocker in the dependency graph. Generated from `cargo metadata` rather than maintained, so adding a dependency and forgetting the notice is a build failure rather than a licence breach somebody else discovers | `check-attribution` |
| Every crate that decides something has a mutation entry | A headline figure of several hundred mutations reads as thorough and says nothing about **where** they are. Eighteen crates had none at all — about nineteen thousand lines, including the whole of M4's graph algorithms in a milestone marked complete, and the `pgoutput` decoder the README singles out as validated against a real stream. Nine of the mutations written to close that gap survived on their first run, and each was a real hole in the tests | `check-mutation-coverage` |
| Every figure this repository publishes about speed names what produced it, and the name resolves | `ADR-0020` published three speed tables --- 24.9x for borrowing a row, a per-kernel table, "10 to 15 times faster" --- and **nothing in the repository ever produced any of them**. `criterion` was in the pin set and used by no crate: zero `benches/` directories, zero `[[bench]]` targets. Each restatement, in `rows.rs` and in `STATUS.md`, made the numbers look more established, and the ADR's own rule --- *"every claim about speed carries its number"* --- is what let them stand, because a figure in prose reads as measured. The first version of this check confirmed the two publishing crates had a `benches/` directory and that every target compiled, which is necessary and proves nothing about any particular number: a directory is not a measurement. So a ratio written as `N× faster` or `N× slower` on one line, in any document under `docs/`, must now name its provenance in the same paragraph --- `[bench: crate/group]`, resolved against the `benchmark_group` names declared under that crate's `benches/`; `§Section` of `STATUS.md`, resolved against its headings; `[rejected: why]` for an alternative this build does not contain; or `[historical: why]` for a run against code that no longer exists. A citation that does not resolve fails the build, because one that does not resolve is worse than none: it reads as checked. It reads `docs/`, `README.md`, `sdk/`, `packaging/` and the deck generator, because a figure published outside `docs/` is published just the same. **What it does not catch, stated rather than implied**: a ratio with no direction word beside it --- a bare `4×` in a table of objectives is a target rather than a measurement, and treating every such cell as a claim would make the marker something people add to silence the gate rather than something they read --- and a direction word that wraps to the next line. Provenance is checked per *paragraph*, so one citation can cover a figure it does not back; the `[rejected:]` form exists because that happened while this check was being written | `check-benchmarks` |
| Every service-level objective is reported on | `PERF-05`. Eighteen `NFR-PERF-*` objectives are stated and the table reporting their state listed eleven --- seven were absent, not recorded as unmet, simply not there, including `NFR-PERF-06`, which is the function catalogue's own requirement. An objective recorded as unmet is a decision somebody took; one that is missing is one nobody has to think about, and to a reader scanning for red it looks exactly like one that is fine. The check has no opinion about whether an objective is met --- omission is the failure it exists to catch, because omission is the one a reader cannot see | `check-objectives` |
| A chapter of the book says which of what it describes is built | `R4` survived inside the fix for `R4`: the status check compared the documents that *declared* a `**Status:**` line, and a document with none simply did not participate. A repository-wide search across `docs/book/` found no such line in any of twenty-seven chapters, so the entire book was invisible to the check written to stop documentation drift --- which is how four chapters came to carry milestone claims the canonical line contradicts. Opting out cost nothing, so an absent header in the book is now a failure rather than a skip | `check-docs` |
| A crate writing `unsafe` is on a list that says why, and a crate on the list still writes it | `sankhya-alloc` held the only exception since it was written, in a manifest and a module header that both said so. A second was needed for the sandbox and the sentence had quietly stopped being true. The check fails both ways: an unlisted opt-out, and a permitted crate that no longer writes `unsafe` — because a permission that outlives its reason is a permission nobody re-examines | `check-unsafety` |
| Every writer a commit can point at syncs its bytes **and** its directory entry | Nothing in this workspace called `fsync` until 2026-09-03, so `publish` returning meant bytes in the page cache and a name in a directory entry — both of which a crash discards. A commit could be acknowledged and then be a log entry pointing at a Parquet file that is short, empty, or full of what those blocks held before. The directory sync is the half that gets forgotten: a rename is a directory modification, and unsynced it can lose the name while keeping the blocks. Held by a source check rather than a test, because `fsync` cannot be observed from inside the process that calls it | `check-durability` |
| The build tree is not allowed to consume the machine | Cargo names artefacts by input hash and never removes the ones a rebuild supersedes. Three days of ordinary work grew `target/` to 482 GB and took the disk to 95%, which is how a forty-five-minute soak died at t+2833s and wrote a zero-byte report explaining why | `check-build-tree`, swept by `sweep` |
| A mutation catalogue entry must be **able to fail** | An entry that cannot fail teaches you to read `SURVIVED` as noise. Several have been deleted for this | `check-mutations`, review |
| The test suite runs on every build | It did not. Thirteen static checks passed while a maintenance test failed, and the reported test count came from *counting test functions* — a number equally correct whether they pass or not | `check-tests` |
| A throughput measurement is taken on a machine that is not busy | `ADR-0013`'s C1–C3 are measurements, and `cargo test --workspace` saturates every core. Three in-process guards were tried and each was necessary without being sufficient, so the measurements are `#[ignore]`d and run **alone**, as the only cargo process. They remain part of the full gate rather than beside it, because a measurement moved out of the gate is a measurement that stops being taken | `check-concurrency` |
| A soak drives the **product's** write path, never its own | A harness that writes through its own code measures its own code. Ten-gigabyte runs reported `PASS` for hours against a layout the product cannot produce | `check-writers` |

**A rule whose third column names a *test* rather than a check is verified by that test and not
by this table.** `check-invariants` extracts `check-` tokens only, so those rows are a pointer,
not a guarantee — stated here because it would otherwise read as one.

### What runs outside `check-all`, and why

Two things, both because they need a quiet machine and minutes rather than seconds:

- **`check-performance`** — the `NFR-PERF` objectives against TPC-H at scale factor 1.
- **`cargo bench`** — the benchmarks. `check-all` *builds* every benchmark target, so one that
  stops compiling fails the build; it does not run them, because a timing taken on a machine
  doing something else describes the machine.

A check that people learn to skip is worse than one they have to invoke, which is the argument
for the split. It is also the argument against it: **no automated build has ever failed on a
performance budget**, because CI runs `check-all` and nothing opts in. That is recorded here
rather than in a footnote, because it is the largest hole in this document.

## 3. The mutation catalogue

`tools/mutation-audit.py` holds **909** specific defects. Each is applied to the source, the
suite is run, and the entry passes only if the suite **fails**. A mutation that survives is a
hole in the tests, named and located.

It is the only mechanism here that asks the right question. A test suite with high coverage and
weak assertions passes everything and notices nothing; the catalogue finds that directly, by
breaking the code and watching.

**What it has found, which is the reason to keep paying for it.** Thirty-one mutations survived
on their first run. Five entries turned out to be equivalent mutants no test could ever have
caught. Six were inert until corrected — two did not compile, one was an equivalent mutant
deleted rather than repaired, and one was anchored on a guard that appears twice so it patched
the harmless copy. Four revealed tests that did not test what their names claimed. **Three
exposed defects in tests rather than in code, and all three were the same defect: an unbounded
wait, so that removing a deadline hung the build rather than failing it.** A hang is strictly
worse than a failure — it takes the build with it and reports nothing — so every wait now goes
through one bounded helper.

More recently it has caught bad *mutations* as well as bad tests, which is the same discipline
turned on itself: an entry that tested a log message's wording rather than its behaviour, one
that asserted `LogCache` caches rather than that the caller uses it, and one that added a
`println!` beside the code it was meant to replace so the property was never removed. Each
survived, each was rewritten, and each is recorded in `REMEDIATION.md` rather than quietly
fixed.

**A mutation that hangs the build is worse than one that survives.** One sandbox entry made
`spawn` block on a full pipe; it was removed, with the reasoning recorded, rather than left to
take a future build down.

## 4. Benchmarks

Every figure this repository publishes about speed must have a benchmark that produces it, and
`check-benchmarks` builds every benchmark target on every run.

That rule exists because the alternative was tried. `ADR-0020` published three speed tables —
24.9× for borrowing a row instead of copying it, a per-kernel table, and *"10 to 15 times
faster"* — and **nothing in this repository's history produced any of them.** They were restated
in two other places until they read as established fact. The tell was internal: only the fast
arm was anomalous, it was non-monotone in width, and 0.85 ms for a scalar reduction implied
about 39 GB/s, above the machine's memory bandwidth. The fast arm had almost certainly been
deleted by the optimiser, because its result was unused.

The rule the same ADR already stated — *"every claim about speed carries its number"* — is
exactly what let that stand: a figure in prose reads as measured. It now reads **"and the number
carries the benchmark"**, and both arms of every benchmark here consume their results.

A benchmark ratio belongs to a machine, a dataset and a build, so what is published is the
benchmark, and the figures beside it name all three.

## 5. The soak

A long run under load, watching what grows. The question is not whether the system is correct
for one query but whether anything accumulates: memory, descriptors, files, log entries,
lock contention.

**What it found, and what it did not.** Resident memory rose from 1,223 MB to 2,458 MB and
settled at 2,271 MB — a shape worth knowing and not the same thing as "flat", which is what
three documents said. And the soak's verdict is *printed*, not asserted, so a regression in it
would be reported and would not fail anything. Both facts are here rather than in the summary
they contradict.

## 6. What none of this catches

This is the section to read if you are deciding how much to trust the rest.

- **Documentation is gated syntactically, not semantically.** Links resolve, pinned versions
  match, backticked crate names exist, status lines agree. No gate checks a behaviour, a
  configuration key, an environment variable, a command name, or a claim that something is
  enforced. Every one of the 129 findings lived in that space.
- **A crate name inside a fenced code block is invisible to `check-docs`**, which pairs single
  backticks. That is exactly where stale architecture diagrams live.
- **Numbers propagate from source comments into prose** without either being checked. "299
  source files publish through the atomic writer" travelled from a comment in the gate's own
  source into a chapter; ten files import it.
- **Nine of the gate modules have no test that a bad input is rejected.** A check that has never
  been shown to fail is a check nobody has shown to work.
- **Fifteen of eighteen service-level objectives are unmeasured**, and the gate that measures
  the other three drives the engine directly rather than crossing the server or the wire — so
  every per-statement cost is outside the measured path.
- **Fourteen end-to-end tests pass without running** when PostgreSQL is not configured, which is
  the CI configuration. Setting `SANKHYA_REQUIRE_E2E=1` makes that a failure; CI does not.
- **The concurrency measurements are skipped in CI** and have never been taken there.

## 7. Running it

```bash
cargo run -p xtask -- check-all       # every gate above
cargo test --workspace                # 2,830 tests
python3 tools/mutation-audit.py       # 920 defects, one at a time --- hours
python3 tools/mutation-audit.py --check   # every entry still matches its source, in seconds
cargo bench -p sankhya-math           # and -p sankhya-functions
cargo run -p xtask -- check-performance   # the objectives; needs a quiet machine
```

The mutation catalogue edits your source files as it goes and restores them. `--check` is the
one to run habitually: it proves every entry still names real code, which is the failure mode
that makes a mutation pass silently.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
