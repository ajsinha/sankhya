<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — The invariants, and what each one cost to learn

**Status:** Implementation — M0–M8 and M10 complete; M8's scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11

This system has more than fifty crates. Nobody holds that in their head, and a rule held only
in somebody's head has a failure rate — this project has the evidence, below, in the column
that says how each of these was found.

So every invariant here names **where it is enforced**. If the third column says a check, the
build fails when the rule is broken. If it says a test, one test fails. If it says *nothing
yet*, the rule is a statement of intent and is marked as such, because an unenforced rule
presented as an enforced one is the exact failure mode the rest of this document is about.

`cargo xtask check-invariants` verifies that every check named here exists.

---

## 1. The shape of the system

| Rule | Why | Enforced by |
|---|---|---|
| **`sankhya-publish` is the only writer to a warehouse** | A second writer is a path that does not get the guarantees the first one enforces. While the CDC pipeline wrote its own files, its tables had no partition columns and violated `FR-STORE-20` — which every table on the other path satisfied | `check-writers` |
| Data **in** is `sankhya-ingest`; data **stored** is `sankhya-publish`; data **out** will be its own crate | Three responsibilities, three boundaries. Ingest decodes and conditions whatever arrives — Postgres capture, and later files, streams and API calls — then publishes | `check-writers`, `check-layers` |
| A crate may depend only on lower layers, or on its own | Cycles make the build order a matter of luck and make a change's blast radius unknowable | `check-layers` |
| Domain vocabulary never appears in a core crate | Risk and AML are *use cases*. A domain word in the engine is the first step to an engine that only serves one industry | `check-vocabulary` |
| No file grows past 1,500 lines of code | A file nobody will read in one sitting is a file whose invariants nobody knows | `check-loc` |
| The Arrow, Parquet, DataFusion and `object_store` family is exact-pinned, with no duplicate versions | Two Arrow majors make identically named types incompatible. That is a correctness hazard, not an inefficiency — see [ADR-0001](adr/0001-dependency-pin-set.md) | `check-dupes`, `check-features` |
| Every crate compiles under the workspace lint policy, with `unsafe` forbidden | A policy that is not run is a policy that is not held. It was not running for a while, and the milestone that was meant to enforce it had passed | `check-lints` |

## 2. Storage

| Rule | Why | Enforced by |
|---|---|---|
| Every analytical table carries `sank_data_date` and is **partitioned on it** | `FR-STORE-20`, no exemption for size or purpose. Retention becomes a metadata operation instead of a bulk delete | `sankhya-publish` tests |
| A partition column is in the **schema**, the **path**, and the **add action** | Delta requires all three. A column present in only one of them reads as null for every row in Spark and Trino | `sankhya-publish` tests |
| A batch spanning partitions becomes several files in **one commit** | A reader must never see half a batch | `sankhya-publish` tests |
| A batch touching many partitions does not write one tiny file per partition | `FR-CDC-14`. Unguarded, a 5,000-row append over ninety days writes ninety files of fifty-five rows | `sankhya-publish` tests |
| Clustering is **declared**, never inferred | The engine cannot tell a meaningful query boundary from a merely low-cardinality column. Guessing sorts a table for queries nobody runs, at every compaction, for ever | `sankhya-maintenance` tests |
| A partition still receiving writes is merged **without** sorting | Ordering it produces a layout correct until the next append, for the cost of a full sort every pass | `sankhya-maintenance` tests |
| Commit versions are contiguous | A gap makes it impossible to tell whether a log has more commits without listing all of them | `sankhya-table-delta`, `sankhya-publish` tests |
| The log lags the filesystem, never leads it | A file on disk with no log entry is invisible and reclaimable. A log entry with no file makes every query fail | `sankhya-ingest` crash-safety tests |

## 3. Answers

| Rule | Why | Enforced by |
|---|---|---|
| **Absent is not zero** | "No transactions this period" and "transactions netting to zero" lead to opposite actions. One printed as the other turns a missing feed into a clean report | `sankhya-cube` tests |
| **Truncated is not complete** | A search that stopped early is a lower bound. Rendered as a total, it is what an operator acts on | `sankhya-graph-algo`, `sankhya-cube` tests |
| **Filtered is not complete** | Two principals may legitimately see different totals; neither may be presented as *the* total without saying so | `sankhya-cube` tests |
| **Could not run is not nothing found** | The two look identical in a report and mean opposite things | throughout; `sankhya-diagnostic` tests |
| A measure with no declared aggregation rule is **refused**, never defaulted to summation | Summing a balance across time gives a figure that is plausible, wrong, and indistinguishable from a correct one | `sankhya-cube` tests |
| Materialisation changes **where** an answer is computed, never **what** it is — compared by bits | A cache that changes results is not a cache. Two-stage roll-ups round twice, so partial aggregates are stored unrounded | `sankhya-cube-sql` exit-criteria tests |

## 4. Documentation

Every rule here exists because a document said something untrue and nothing noticed.

| Rule | Why | Enforced by |
|---|---|---|
| A figure in prose matches the repository | Seven documents claimed a test count that was two hundred short | `check-doc-numbers`, fixed by `sync-doc-numbers` |
| A source path named in prose exists | `GUIDE.md` promised its examples were executed by a file that did not exist | `check-docs` |
| Every document's status line names every milestone in progress | Seven documents **agreed** on a status that was wrong. Agreement is not accuracy | `check-docs` |
| Every guide example is executed, or listed with a reason | A block that is neither fails the build | `sankhya-server` guide test |
| Every metric, error code and platform is documented from its declaration | A hand-written table is correct the day it is written | `check-catalogues` |
| A generated document says it is generated | One that does not gets edited by hand, and the edit disappears | `check-catalogues` |

## 4a. Operability

| Rule | Why | Enforced by |
|---|---|---|
| No log statement records what a caller supplied | A log line carrying tenant data is a disclosure that survives in backups, and `#[instrument]` without `skip_all` records every argument | `check-logging` |
| A release binary starts on the oldest platform it claims to support | The build machine's glibc is not the deployment target's, and it cannot tell you that — see [`PLATFORMS.md`](PLATFORMS.md) | `check-package` |
| No lock is taken while another is held, unless the pair is declared with its order | A deadlock is not found by testing. A race appears under load --- run it enough and the bad interleaving happens --- but a deadlock needs two threads taking two locks in opposite orders at the same moment, and while only one path holds both there is no order to reverse and no amount of hammering finds anything. **The code that holds two locks is not the bug; the code written six months later that holds them the other way round is**, and by then the first ordering is invisible. Two such places existed on 2026-08-29: `QueryLog::record` held the map's read lock across the ring's, under a comment claiming it did not, because in edition 2021 a temporary in an `if let` scrutinee lives to the end of the block; and `CubeCatalog::resolve` held `cubes` across `declared` | `check-lock-order` |
| Every crate is reachable from something that ships, or listed with a **milestone** | The narrower version of this rule --- only crates registering SQL functions --- let about 2,600 lines through: a REST surface, a capture source, a counting allocator nothing installs, a set of port traits, and an entire declarative pack tier. Widening it to plain reachability immediately found Arrow Flight SQL, which `GUIDE.md` §7a documents and no binary can reach. A crate is a claim the repository makes about itself, and a milestone is what makes the claim keepable | `check-surfaces` |
| Every SQL surface is reachable from the server | Four crates registering SQL functions turned out, in one day, to be unreachable from the thing that serves SQL — each found by accident. A capability nothing reaches is indistinguishable from one that was never built | `check-surfaces` |
| No writer makes a file visible by writing to the path a reader will open, and no writer claims a name by first checking it is free | The technique was already implemented correctly three times here and wrongly four, because a three-line technique gets retyped rather than reused. The wrong half of it cost a commit: `commit` checked that a version was absent and then renamed onto it, and `rename(2)` replaces its destination silently — so two committers both saw the version free and the second overwrote the first, with no error to either. Seventeen hundred tests could not see it, because every one had a single writer | `check-atomic-writes`, `sankhya-atomicfs` tests |
| The build tree is not allowed to consume the machine | Cargo names artefacts by input hash and never removes the ones a rebuild supersedes. Three days of ordinary work grew `target/` to 482 GB and took the disk to 95%, which is how a forty-five-minute soak died at t+2833s and wrote a zero-byte report explaining why | `check-build-tree`, swept by `sweep` |

## 4b. Configuration

| Rule | Why | Enforced by |
|---|---|---|
| A malformed configuration file **fails the load** | A process with three of its four settings behaves plausibly and wrongly, and the missing one is discovered by whatever it breaks | `sankhya-config` tests |
| An unresolved `${...}` with no default **fails the load** | A placeholder is visible in a config dump and invisible in a connection string, which is where the value goes. It turns a configuration error into a network one, at a distance from its cause | `sankhya-config` tests |
| An unparseable typed value is **refused**, never defaulted | `port=eighty` silently becoming 8080 is a deployment behaving as though it were configured when it is not | `sankhya-config` tests |
| Every value knows **where it came from** | "The timeout is thirty seconds" does not answer "why is the timeout thirty seconds", and the value is identical whether it came from a file, the environment or a flag | `sankhya-config` tests |
| A secret does not print itself, and reading it takes a word a reviewer can see | A password in a log line survives in every backup of that log; rotating it does not remove it | `sankhya-config` tests |
| A reload **says what changed**, and keeps the working configuration if the new one is broken | A reload nobody is told about is indistinguishable from a bug, and a process on a good configuration must not be pushed onto a bad one because somebody saved mid-edit | `sankhya-config` tests |

## 5. Evidence

| Rule | Why | Enforced by |
|---|---|---|
| A mutation catalogue entry must be **able to fail** | An entry that cannot fail teaches you to read `SURVIVED` as noise. Several have been deleted for this | `check-mutations`, review |
| The test suite runs on every build | It did not. Thirteen static checks passed while a maintenance test failed, and the reported test count came from *counting test functions* — a number equally correct whether they pass or not | `check-tests` |
| A throughput measurement is taken on a machine that is not busy | `ADR-0013`'s C1–C3 are measurements, and `cargo test --workspace` saturates every core. Three in-process guards were tried and each was necessary without being sufficient, so the measurements are `#[ignore]`d and run **alone**, as the only cargo process. They remain part of the full gate rather than beside it, because a measurement moved out of the gate is a measurement that stops being taken | `check-concurrency` |
| A check is tested by a case it must **reject** | A test asserting a check passes on clean input passes just as happily when the check never reports anything | `xtask` tests |
| A guard against writing is never verified **by writing** | It was, once, before the guard had compiled in — and left 40 MB outside the project root | `sankhya-diagnostic` soak tests |
| A soak drives the **product's** write path, never its own | A harness that writes through its own code measures its own code. Ten-gigabyte runs reported `PASS` for hours against a layout the product cannot produce | `check-writers` |
| Nothing is created or deleted outside the project root | *nothing yet* — the soak refuses paths outside it, but no check covers the repository as a whole |

## 6. What is not enforced, and is therefore only intent

Listed because the alternative is a document that reads as though everything above is
guaranteed.

- **The project-root rule** binds the soak and nothing else. Any other tool could still write
  outside the repository.
- **`sankhya-maintenance` must preserve the layout `sankhya-publish` established.** It is an
  allowed writer and nothing checks that its output is still partitioned correctly.
- **Compaction targets 256 MB and the soak's harness sets its own thresholds.** Nothing
  reconciles the two, so a soak can be healthy under settings production never uses.

---

## Where to go next

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — why the system is shaped this way
- [`STATUS.md`](STATUS.md) — what is built, what is not, and what was found wrong
- [`SOAK.md`](SOAK.md) — the long-run method, and four attempts at the judgement
