# How This Is Tested

> This chapter covers how every claim in this book is verified: the test suite, the
> mutation audit, the measurements and their controls, the soak, and the adversarial
> review. Its central claim is that a test written to catch a defect is not evidence that
> it catches it, and that the only way to know is to break the code on purpose and watch
> what complains. The chapter is therefore mostly a record of things that went wrong — a
> recurring defect where a surface is built, unit-tested and unreachable through the front
> door, and a longer list of tests that passed against the very defect they were named for.

## 23.1 The standard

Four numbers describe the mechanised half of verification:

| | |
|---|---|
| `cargo test --workspace` | 2797 tests, none of which needs a database |
| `python3 tools/mutation-audit.py` | 908 specific defects, applied one at a time |
| `cargo xtask check-all` | twenty repository invariants, each proven to fail when violated |
| `cargo xtask check-performance` | the `NFR-PERF` objectives, as a gate that can fail |

Those are the inputs, not the argument. The argument is what happens when they are wrong,
and this chapter is organised around that, because a chapter listing green figures is a
chapter that proves the figures were green.

Two ground rules shape everything below.

**Nothing is mocked.** The Parquet is real Parquet, the Delta logs are read back by an
independent kernel implementation, the TPC-H data is generated rather than fixtured, and
the PostgreSQL supervisor's tests run against a vendored 17.11 rather than a stub — because
there is no way to fake a database lifecycle usefully. The failures worth catching are
`initdb` refusing a non-empty directory, a postmaster that has started and is not yet
accepting connections, and a shutdown that leaves a lock file behind, and a stub producing
any of those would be asserting what its author already believed.

**Two of our own components agreeing proves nothing.** `sankhya-table-delta`'s
`tests/oracle.rs` reads every log this crate writes back with `delta_kernel`, an independent
implementation. That test earned itself immediately: the hand-written Delta log was
*invalid* — the `add` action's `partitionValues` field is non-nullable and was omitted
entirely — and this crate's own reader accepted it happily, because a reader ignores a field
it never writes. The log looked reasonable and round-tripped perfectly through both halves
of a pair that had been written by the same person.

> **Key idea**
> The unit of evidence is not "a test exists". It is "this test has been run against the
> defect it names and observed to fail". Everything in this chapter is machinery for
> producing that observation.

## 23.2 The recurring defect: built, tested, unreachable

This is the pattern that recurs more than any other in the project's history, and it is
worth naming precisely, because it defeats unit testing, mutation testing and code review
simultaneously.

**A surface is built. Its own tests call it directly, and they pass. The layer above it
either never routes to it, or intercepts the statement before it arrives. From outside, a
capability that nothing reaches is indistinguishable from one that was never built.**

The instances, in the order they were found:

| What was unreachable | What was passing while it was broken | How it was found |
|---|---|---|
| `sankhya-maintenance` — a working, tested compaction and retirement library | its own suite; a running server compacted nothing and retired nothing | a soak dying with a full disk |
| `sankhya-cube-sql` — the entire cube SQL surface, depended on by no crate | its own suite | reading a `Cargo.toml` for an unrelated reason |
| `sankhya-olap` — every vector, matrix and statistics function the guide documents | its own suite; the server answered `Invalid function` | a guide example failing |
| `sankhya-graph-sql` — five graph functions the guide names | its own suite | the same |
| Cuboid materialisation — the refresher built cuboids on a timer and **nothing ever read one** | eight cube exit criteria, and a test named `a_materialised_cuboid_answers_without_reading_the_fact_table` | asking the compiler which methods the server never calls |
| The security path — a policy row predicate and unregistered tables | unit tests proving both | wiring it to the front door |
| `sankhya-alloc` — a counting global allocator that **nothing installed** | its own suite; every allocation figure it exists to provide was unavailable | a crate-hygiene audit |
| `SHOW FEEDS` — answered by the wire layer as an empty session setting | the command's own unit tests, **and two mutations** | running an exit criterion over a real socket |
| `sales.orders` — the schema was discarded at registration | every query test, all of which used bare names | building `SHOW LINEAGE` for the SDK |
| Two tables of one name — the second silently replaced the first | every test, none of which had two | the same |
| `CREATE TABLE ... CLONE` — had never worked against a table this server serves | **thirteen clone tests**, on a fixture with a layout no deployment has | the same |

`SHOW FEEDS` is the sharpest of them, because the interception is *deliberate and correct*
in isolation. The wire layer answers catalogue queries itself, before the handler, so that a
client asking about `pg_class` gets an answer instead of *"no such table"*. Its recogniser is
lenient on purpose — it treats `SHOW <anything>` as a session setting, because real tools
generate settings queries in a dozen spellings and matching them literally works for the
client it was written against and fails for the next one. `SHOW FEEDS` is a `SHOW`. So it was
answered as a setting named `feeds`, whose value is nothing, and the statement never reached
the code that implements it.

The fix inverts the precedence: a handler may **claim** a statement it defines itself, and a
claimed statement bypasses the shortcut. The thing that defines a statement decides before
the thing that guesses at one. It defaults to claiming nothing, so every other handler
behaves exactly as it did.

Five responses follow from the pattern, and all five are now standing practice:

1. **`check-surfaces`** fails the build when a crate is reachable from nothing that ships,
   or is listed without a milestone. (Chapter 22.)
2. **Guide and tutorial examples are executed** by the test suite, so a documented capability
   that cannot be called fails a build. Every file in `docs/tutorials/` must appear in that
   list, so a tutorial cannot be added and quietly left unverified.
3. **Exit criteria are demonstrated through the front door a user has**, not through the
   library the criterion is about. That is what caught `SHOW FEEDS`; the guide test would
   *not* have, because it asserts an example is not refused, and an empty answer is not a
   refusal.
4. **The adversarial review** (§23.9) exists for exactly this class, and biases its whole
   method toward execution by clients that know nothing about the code.
5. **Every shipped SDK example is a test**, added 2026-09-02. The sixteen scripts under
   `sdk/python/examples/` and `sdk/sql/examples/` run against a live server from
   `crates/sankhya-server/tests/sdk_examples.rs` and `tests/sql_examples.rs`. The SQL gate reads
   each *statement's* outcome, not the script's, and checks both directions: a statement marked
   `-- REFUSES` must fail, and every other must succeed — because a demonstration of a refusal
   that quietly starts succeeding is a rule that has been removed and a document that still
   claims it.

   This is the response with the largest catch to date. Writing those examples found six
   defects that unit tests, mutation tests and a green gate had all missed: `CREATE CUBE` could
   not name a table in a schema at all; a leading `--` comment made the server fail to recognise
   its own statements; `SHOW HISTORY OF`'s `kept_by` named nothing and ignored clones; and the
   Python binding's cube and graph calls passed arguments into the wrong positions while
   stripping their options' names. Two shipped SQL examples contained statements that could
   never have run, one of them carrying a written note claiming it had been verified against a
   live server.

   The reason a *review* misses these is worth stating: a `psql` script with `ON_ERROR_STOP off`
   prints its errors and keeps going, so a wall of output reads as success. Only something that
   reads the exit of each statement can tell.

> **Key idea**
> A surface's own tests call the surface. That is the one place the defect cannot be. Every
> mechanism above moves the caller further away from the code — to a different crate, to the
> server, to a socket, to a third-party client — because distance from the implementation is
> the only variable that matters here.

## 23.3 A fixture whose shape is not the product's shape tests the fixture

The clone defect deserves its own section, because it is the failure that survives every
rule designed to prevent it.

`CREATE TABLE q3 CLONE orders` resolved a name as `warehouse/<name>`. Table discovery reads
`warehouse/<schema>/<table>`. So on any real deployment the statement answered *"the table
to clone declares no schema"* — the directory it looked in does not exist. Thirteen tests
passed, because the fixture put its tables at the warehouse **root**, a layout no deployment
has.

The fixture was built through the product's own writer. That is the rule — *a soak drives
the product's write path, never its own* — and it exists to prevent precisely this. It was
followed, and it was not enough. **Writing through the real path is necessary and not
sufficient; the shape has to be real too.**

The same failure has three other recorded forms:

- **The soak's first harness wrote through its own code** and reported `PASS` on flat,
  non-conforming tables for hours — ten-gigabyte runs, green, against a layout the product
  cannot produce. One run routed through `sankhya-publish` surfaced a MUST violation
  (`FR-CDC-14`, unbounded partition fan-out) in four minutes: 32,279 live files across ten
  tables, averaging 37 KB, against a compaction policy that targets 256 MB.
- **The filter-pushdown benchmark configured its own `ArrowWriter`.** It duplicated three of
  `WriterConfig`'s six decisions and silently dropped three more, including the row-group
  limit and the statistics truncation length. Pushdown is only as good as the page index, and
  the page index is emitted by writer settings — so the measurement was of a file SANKHYA
  would never produce, and a change to the product's layout could not have moved the number.
- **A test named for a layout tested a string formatter.**
  `the_partition_path_is_what_an_external_engine_expects` publishes nothing and reads
  nothing. Meanwhile every table declared `partitionColumns: ["sank_data_date"]` and wrote
  every file flat at the table root with `"partitionValues":{}`, against a schema that did
  not contain the column — a table that exists in no location the Delta protocol defines, and
  that Spark and Trino read as null for every row. The replacements assert against the files
  on disk, the commit log, and the Parquet contents.

> **Pitfall**
> "The fixture uses the real writer" is a check on the *path*, not on the *shape*. A fixture
> can be produced entirely by product code and still encode a directory layout, a partition
> pattern, or a log history that no deployment ever has — and every test built on it then
> measures the fixture.

## 23.4 Tests that passed against the defect they were named for

This is the project's longest list and its most useful one. Each entry is a test that was
written, reviewed, passed, and would not have failed on the defect it existed to catch.
None was found by reading.

**Found by mutation:**

| Test | Why it could not fail |
|---|---|
| The arrival tier's coverage property | Its generator started the publication frontier equal to the stream's origin, so the two could never diverge — and its assertion encoded the buggy expectation |
| `is_exact_cover`, the oracle every splice test leans on | It had no tests of its own. It could be changed to tolerate gaps, or to stop requiring the cover to reach the target, and the whole suite stayed green |
| *Open transactions are never published* | It aborted every unsealed transaction before flushing, so it never left one in flight — and an in-flight transaction is the steady state of a busy source |
| The provider's predicate-exactness claim | The provider could claim it evaluated predicates *exactly*, which lets the engine drop the filter from the plan, and every test passed — because not one of them had a `WHERE` clause |
| *A disjunction is never split* | It used `a = x OR a = y`, which the engine rewrites into an `IN` list before it reaches the code under test. The test exercised no disjunction at all |
| The driver's compaction removals | Checked through the constructors the driver *could* have called, not through what it actually committed. Swapping one for the other went unnoticed |
| The staging-name test | It asserted the file held *some* writer's body — which it does, because a small `fs::write` lands atomically. Showing the property needs 256 KB bodies so two interleaved writes cannot both land whole |
| The half-written-read test | It asserted every read was homogeneous. Writing onto a live path truncates first, so a reader catches a **short** file far more often than a mixed one — and `all()` on an empty slice is `true` |
| The security limit-pushdown test | It ran against `MemTable`, which ignores limits, so pushing a limit below the security filter changed nothing. A test whose subject ignores the thing under test proves nothing |
| The audit chain's previous-digest check | Every test that broke a link also broke the sequence number, which fires first. A competent attacker renumbers after a deletion |
| Statistics computation | It had **no tests in its own crate at all** — exercised only end to end from a different crate, so `cargo test -p sankhya-table` covered none of it. Writing the missing tests found the NaN bounds defect on the first run |

**Found by walking a claim rather than a test:**

| Test | Why it could not fail |
|---|---|
| `a_materialised_cuboid_answers_without_reading_the_fact_table` | Its own comment said *"proved by taking the fact table away"*. It never took the fact table away and never re-queried; it wrote a cuboid by hand and asserted the file existed. The first rewrite still failed, because a second connection to the same process is served from cache — it takes a **restart**, which is what a cuboid is actually for |
| `crates/sankhya-server/tests/five_minutes.rs` | The seven-step first-user journey, in a file whose header says *"this test runs on every build and proves the path works"*, had **no `#[test]` attribute on its function**. Nothing ran it. It passes in 0.05 s and always would have |
| `a_statement_pins_the_warehouse_for_as_long_as_it_runs` | It called `leases.pin()` itself, so it passed with the pin removed from `run_statement` altogether — the entire behaviour it was named for |
| The quadratic-replay regression guard | It spread its workload across thousands of commits, where linear file I/O dominates the quadratic term entirely. It passed against the quadratic replay |
| The memory-limit gate | Its first version used a two-megabyte pool, just above the line, so the query succeeded and the assertion never ran |
| The parallel-scan fix | Had **no test at all** — the defect was found by measurement and nothing was written to hold it. Months later the fix was replaced, and the only thing that caught the intermediate regression was a *deadline* test that happened to contain a self-check asserting its own plan ran in parallel |

**Found by measurement having no control:**

| Test | Why it could not fail |
|---|---|
| The `LogCache` contention test | Its first version asserted `> 100` lookups — which is *below* the blocked figure of 618. It passed with the global lock restored: a test of contention that could not detect contention |
| C2, the reader that must not wait | Its control compared throughput *shares*, which narrow when everything gets faster. It failed in the gate and passed by hand, for a reason with nothing to do with the read path |
| The performance gate's first version | It inherited `#[tokio::test]`'s current-thread runtime, so its eight concurrent clients took turns on one thread and the pivot reported 3029 ms — three times its budget, describing nothing the requirement is about |

Three of these were caught only because a *catalogue* of deliberate defects existed. That is
the next section.

> **Key idea**
> The general lesson, recorded because it applies to every test not yet audited: a test
> written to catch a defect is not evidence that it catches it. Until it has been run against
> that defect, assume it is in the same state as the twenty above.

## 23.5 The mutation audit

`tools/mutation-audit.py` keeps a catalogue of **608 specific, plausible defects** — not
arbitrary operator flips, but the mistakes a person could make in this code — applies each
one, runs the tests that claim to cover it, and reports whether anything failed. A
`SURVIVOR` is a gap in the tests, not necessarily a bug in the code.

Thirty-one entries survived the first time each was run. Two of the most recent were written
for the tiering encoding, and both exposed tests that did not test what their names claimed:
one compared two integer widths whose encodings already differ in length, so removing the
type tag changed nothing; one used a composite key that the framing bytes separate without
any length prefix.

**The mechanics are the interesting part**, because a tool that edits source in place is one
crash away from being the defect.

- Each file is read before it is edited and rewritten from that copy in a `finally`, and
  every file the run touched is verified byte-identical at the end.
- A `finally` does not run when the process is killed, so the same restore is installed as a
  signal handler, **and** every mutation is recorded in a sidecar file that a later run finds
  and undoes before doing anything else. The sidecar lives inside the repository on purpose:
  one in a temporary directory is one reboot away from being the thing that made the defect
  permanent.
- A lock file holds the owning process id. Two overlapping runs mutate the same files and
  restore each other's originals, producing a tree carrying several deliberate defects at
  once and no record of where they came from. That happened, and was harder to work out than
  the first case. A lock whose owner is gone is taken over rather than respected.
- The clean-tree check is on the files *this run mutates*, not the whole tree. An earlier
  version refused any uncommitted change, which sounded safer and was worse: it forced a
  commit before every audit, so the history filled with placeholder commits and the audit
  became something done after deciding the work was finished rather than before.

Two failures of the catalogue itself are worth recording.

**A deliberate defect reached a commit.** The reversed-comparison fix in the predicate reader
was reverted in the working tree — `5 < x` read as `x < 5`, so the reader skipped files that
did hold matching rows — because a run was killed hard enough to defeat the in-flight record.
The leak is invisible: one plausible-looking line in a file the commit was already touching.
The test proving that mutation caught was sitting red in `main`'s parent. `cargo xtask
check-mutations` now asserts that every catalogue entry still matches its source — no
compilation, milliseconds — so it gates every build rather than only a full audit, and it
catches catalogue drift by the same check. Four entries had already drifted that way through
ordinary refactoring.

**Three entries had not run since a comma went missing.** The catalogue is a Python list of
tuples, and a missing comma between two entries is not a syntax error — Python reads the
second tuple as an *element* of the first. One entry had swallowed the two that followed it,
and the outer entry ran `cargo test -p <tuple>`. The check reported *all 421 catalogue
entries match the source* while three of them were incapable of proving anything, because
every check in that file only ever looked at the first three fields, which are strings either
way. The check now validates the **shape** of every entry before its text.

**One equivalent mutant has been produced and recorded rather than quietly deleted**: a
change to a duplicated guard that left the second copy still refusing, so behaviour was
unchanged and no test could possibly have caught it. The rule that follows is now an
invariant: *a mutation catalogue entry must be able to fail*. An entry that cannot fail
teaches you to read `SURVIVED` as noise, which is the one habit that makes the whole exercise
worthless. Several entries have been deleted for it.

There is also a place where the catalogue deliberately holds **no** entry. `sankhya-testkit`
cannot catch the defect whose window is two instructions — taking an epoch before counting a
reader in `leases::pin` — because `drained` scans hundreds of slots and cannot complete inside
a window that narrow, however often the reader is descheduled. So no entry is written for it:
an entry whose mutation survives is a claim of coverage that does not exist. Reaching that
class needs a scheduler somebody controls, and **adopting `loom` is the recorded next step**.

## 23.6 Measurement as a test, and the control that makes it one

Three of ADR-0013's concurrency criteria are **measurements** rather than assertions:
writers to different tables do not contend; readers are never blocked by writers; contention
on one table degrades gracefully. A single global lock over the warehouse satisfies every
*safety* criterion, so a test asserting the code is correct cannot tell the shipped design
from the design the criteria forbid.

So each measurement is taken **twice in the same run on the same machine** — once as the code
stands, once with the same work serialized through one mutex — and the assertion is on the
distance between them. The control is not a fake of anything; it is a global serialization
point applied to the real function.

| | Measured | Behind one warehouse lock |
|---|---|---|
| C1, commits to eight tables against one | 36,972 → 178,259 commits/s (**4.82×**) | 37,456 → 33,940 (**0.91×**) |
| C1, the same end to end through `Publication` | 4.4×–5.9× | 0.9×–1.2× |
| C2, a reader's rate under four writers | **0.59–0.80** of idle, p99 165 µs → 227 µs | **0.00–0.07**, p99 in *seconds* |
| C3, sixteen writers on one contested version | all sixteen commit, worst rebase count **11** | — |

Getting to a control that works took five attempts, and the failures are more instructive
than the numbers.

**A threshold below the contended figure.** Twice in this repository a contention assertion
has been set at a level the *defect* also clears. The `LogCache` test asserted `> 100` where
the blocked figure was 618. That is not a weak test; it is a test that passes with the defect
restored.

**A control that could not go wrong in the way being tested for.** The first machine-capacity
guard ran a workload that shares nothing — pure arithmetic, same barrier, same thread count —
on one thread and on eight, and asked whether the machine scaled it. It does: under fair
scheduling eight threads collect eight times what one thread collects *however oversubscribed
the machine is*. That control reported near-linear scaling at load 36, in the same run where
the real write path managed 2.5×.

**A probe that could not see the failure arriving.** Free capacity is now read directly —
idle jiffies over a 200 ms window — but the first version sampled *once, before* the arms ran.
C3 failed the same day with the fix in place, because the measurement began on an idle machine
and finished on a saturated one. The check now brackets the measurement: `Window::open` refuses
a machine that is already busy, `Window::held` refuses one that *became* busy, and a
measurement whose window did not hold is discarded rather than asserted on.

**A guard that counted `iowait` as idle.** Right for a question about processor capacity,
wrong for this one: these arms encode Parquet and write files, so the disk is the contended
resource. C3 failed on a machine reporting eight idle cores minutes after a full rebuild — the
processors were free and the disk was not.

**A control expressed as a ratio of two arms.** `check-concurrency` builds its binaries
optimized and a plain `cargo test` does not; with every participant faster, a writer holds a
lock for less time, the serialized arm does better, and any control shaped as a ratio narrows
for reasons that have nothing to do with the path being measured. C2's control is now the
lock-sharing reader's p99 against the free reader's, which separates by four orders of
magnitude — 1.2 s against 106 µs — where the share was a factor of a few. C1's is now
`serialized < FLOOR`: **the serialized arm must fail the very threshold the free arm passes**,
which reuses a number the test already demands rather than calibrating a second one.

The final answer was to remove the interference rather than detect it. The measurements are
`#[ignore]`d, so the parallel suite skips them, and `check-concurrency` runs them one at a time
as the only cargo process, with every binary built *before* any of them is measured — because
`cargo test` compiles with as much parallelism as the machine has, and a measurement taken in
the seconds after that compile is taken on a machine still finishing it.

**A fifth joined them on 2026-09-03**, and how it was found is the point. The query log's
per-cube locking test was the suite's one intermittent failure — passing alone, failing under
load — and it had the capacity window that the other four have. The window was not enough: this
machine has enough cores to *look* idle while sixty-four threads are running on it, so
`Window::open` returned a window and `held()` agreed the machine had stayed quiet. Eight parallel
runs of its own suite produced ratios of **0.54, 0.77 and 1.07** for code whose true ratio is
above three.

Taking the best of five alternating rounds helped and did not fix it — the ratios rose to 1.08
through 1.64, still under the threshold. The measurement was contending with a suite that exists
to create contention, and no amount of estimator care removes that. It is `#[ignore]`d now and
listed in `check-concurrency` beside the others, which is where a measurement that needs a quiet
machine belongs.

> A flaky gate is worse than a missing one. It gets re-run until it passes, and from then on the
> number means nothing and nobody notices when it starts being wrong.

## 23.x The documentation is executed

`check-doc-numbers` catches a stale figure. `check-docs` catches a stale status line. Neither
catches **a sentence that stopped being true**, and on 2026-09-03 three of those surfaced in one
morning — each found by building something, none by a check:

- `sankhya-alloc`'s manifest and module header both said it was *the only* crate permitted to
  write `unsafe`. A second was needed, and the sentence had quietly stopped being true.
- `ADR-0021` said a `FixedSizeList` read back from Parquet cannot carry tensor metadata. That was
  not a property of the format; the writer was dropping it.
- Chapter 19 documented a `MEAN`/`MAX` cube defect, with a transcript, as something a reader
  *must know about before declaring a non-`SUM` measure*. It had been fixed two days earlier.
  Wrong in the more damaging direction: it told people a working feature did not work, and its
  advice would have kept somebody from declaring the measure they needed.

The third is the kind a machine can catch. `tests/book_sql.rs` extracts every ` ```sql ` block in
`docs/`, splits it into statements, runs each against a live server, and checks the **outcome in
both directions**: a statement shown as working must work, and one shown as refused — the book
marks those with a leading or trailing `-- ERROR:` — must be refused. A demonstration of a
refusal that quietly starts succeeding is a rule that has been removed and a document that still
claims it.

**Where the line is.** Much of the book is a transcript taken against a warehouse this fixture is
not: a `payments` graph, a `documents` table, a cube over another machine's data. So the rule is
narrower than *every statement runs* and much sharper than nothing:

> A statement shown as working must fail **only** because the object it names is absent.

Everything else is caught: a function renamed, a clause that no longer parses, an example that
names something it never created.

Its first run found six things, and the split is the interesting part — **four were defects in
the check itself**, which is what a new check should mostly find:

| | |
|---|---|
| Trailing `--` comments were kept, so a `;` before one went unnoticed and two statements ran as one | the check |
| Newlines inside `$$ … $$` were collapsed, which is fatal to Python's indentation | the check |
| A transcript's reply was read as a heading for the *next* statement, excusing one and holding another to a rule it never claimed | the check |
| Clauses and elisions — `WHERE …`, `SELECT ... FROM` — were run as statements | the check |
| `mat_of(2, 3, …)` was shown with five values where a 2×3 needs six | **the book** |
| A cube example named an aggregation the chapter never declared | **the book**, written that morning |

The last one is worth sitting with: it was introduced and caught the same day, by a check written
the same day, in a section documenting a feature built the same day. Documentation rot does not
need time.

One more thing had to be fixed, and it is the quietest failure in this chapter. The skips were
written to be *"loud and by name"* and were neither: `eprintln!` inside a **passing** test goes
into libtest's per-test capture and is printed only if the test fails. A criterion could have
stopped being measured on every run, indefinitely, behind a green gate. `capacity::skipped` now
writes to the descriptor directly, and `check-tests` lists every skipped measurement under its
count, followed by *"green means the rest"*.

> **Pitfall**
> A gate that fails at random is a gate that gets re-run until it passes, and a threshold
> nobody trusts is the same defect as a threshold with no control: the number stops meaning
> anything, and nobody notices when it starts being wrong.

## 23.7 The soak

A soak is not for *"it did not crash"*. That is what a soak reports and it is the one thing
nobody doubted. It exists for the class of failure invisible in any single sample and obvious
across a week: memory that grows a megabyte an hour, descriptors that are not returned, a
cache with no eviction, compaction that never quite catches up.

The criterion originally read, in its entirety, *"a multi-day soak"* — which is not something
anyone can fail. It is now:

> A soak passes when no bounded measure has a projection that crosses its threshold within
> the observation horizon.

Three refinements make that judgeable rather than noisy.

**A measure declares what kind of bounded it is.** *Steady* asks whether the slope is positive
(resident memory, descriptors, distinct metric series). *Per unit of work* asks whether the
ratio is drifting — audit records are supposed to grow, one per query; what must not grow is
records *per query*, and if it drifts down something is not being recorded, which is the worse
direction. *Sawtooth* trends the **peaks**: writes add files and compaction removes them, and
two such series look identical at any instant — one returns to the same floor every cycle, the
other starts a little higher each time, and the difference is only in the peaks.

**A fixed, declared warm-up prefix is discarded** — ten samples, stated in the report. Fixed
and declared is what keeps it honest: discarding *until the series looks flat* would hide every
leak by construction, because a leak is precisely a series that does not go flat.

**The horizon is bounded by what the run observed.** A run may speak about roughly three times
what it watched and no further. The first real run flagged memory `GROWING — reaching its limit
in about 6 minutes` after sixty rounds in **0.35 seconds**: half a second of samples
extrapolated to three weeks, a factor of three and a half million. A short run cannot pass a
long horizon; it reports that it was too short, and the answer is to run for longer rather than
to widen the limit.

**The harness is proven to notice.** `crates/sankhya-diagnostic/tests/leak.rs` injects one
failure of each shape and requires the run to fail on it: 20 MB/minute retained, four
descriptors a minute not returned, a sawtooth whose peaks climb, the same sawtooth ending on a
trough (still fails — peaks, not last readings), audit drifting from one to two records per
query **while its total looks healthy**, and nothing sampled at all, which reports `COULD NOT
JUDGE` and is a failure. Without those, a green soak would be green because nothing was capable
of turning it red.

### The result nobody expects

**Every defect the soak has found has been in the measuring apparatus, not in the system it
measures.** Four in the judgement, four in the runner. The four runner defects are the sharpest:

| Defect | What the report said while it was wrong |
|---|---|
| Commit versions from a global counter, refused as non-contiguous, error swallowed by `.ok()` | Healthy. Three minutes of writing, **nothing published** |
| Compaction that removed every live file and replaced it with one small batch | Would have been healthy — while ten gigabytes stopped being live at round 8 and the remaining 3h58m soaked an empty warehouse |
| A per-table limit judged against a **sum across ten tables** | `BREACHED — 4900 past the limit`, when every table held 490 |
| Compaction that never re-compacted its own output | Healthy for hours, then a slow climb — correctly flagged, and about the harness |

Three of the four produced a green report while measuring nothing, and the fourth produced a red
one about nothing. None would have appeared in a summary at the end; all four surfaced because
the run prints as it goes, which is the argument for reporting *during* a soak rather than at the
end of one.

There is a matching finding about what the soak was measuring at all. The loop counted a **log
replay** as a query — a real cost and a real leak surface, and not a read. The ten gigabytes sat
in Parquet files written once at seed time and read by nothing, anywhere in the binary. So a run
reporting *"10 GB, 5,130 queries, PASS"* measured the append and log paths and named its workload
after work it did not do. The counter is now called `planned`, and a real scan decodes rows and
returns a checksum — not for integrity, Parquet has its own, but so that *the scan read nothing*
is distinguishable from *the scan read zeros*.

### Where it stands

| Run | Judged | Scale | Result |
|---|---|---|---|
| 2026-08-26 | 4 hours | 10 GB | Passed the append, replay and compaction paths. It did not read the data, so it discharges the criterion only for the paths it touched |
| 2026-08-28 | 44 min | 10 GB | `PASS`, all seven measures steady; first run to exercise a cube. 2.41 bn rows scanned; 2,246 MB resident, steady |
| 2026-08-29 | 59 min | 10 GB | `PASS`; first run with materialisation load-bearing. 3.01 bn rows; **2,017 MB** — more work, longer run, less memory, because a cuboid read replaces a fact-table hydration rather than adding to it |
| 2026-08-30 | 45 min | **20 GB** | First `PASS` at the doubled scale. `warehouse_bytes` peaked at 41.58 GB, over the old flat budget by nine; `live_files` went 1,080 → 90 across a reclamation cycle |
| 2026-09-01 | 45 min | 20 GB | Every judged measure `PASS` — **and the run failed**, on reconciliation, which had never run before |

That last row is the pattern in miniature. Four tables reported missing rows in round numbers —
80,000, 160,000, 240,000 — and the numbers were the clue. `expected` is merged the moment a batch
is *absorbed*, and absorbing hands it to the fan-out accumulator, which defers partitions still
too small to be worth a file. The comparison was "what was handed over" against "what was
written", and those differ by design. A defect in the harness, not the product. **A check that has
never run is not evidence of anything, and its first run is as likely to find a defect in itself as
in the system.**

The scale doubling produced a matching finding. `SANKHYA_SOAK_GB` moved from ten to twenty and
three thresholds stayed where they were: `warehouse_bytes` at a flat 32 GB (3.2× a ten-gigabyte
target, so headroom silently fell from 22 GB to 12 GB), `live_files` at a flat 1,000, and a
`USAGE` line restating the harness's own default. The fix is structural rather than three new
numbers — a `Scale` type lives beside the measures and both limits are arithmetic on it — and it
exposed a passing unit test that had become scale-dependent, its fixture tuned to sit under the
flat 1,000. It now asks the declaration what the limit is and states the fixture as fractions of
it.

> **Key idea**
> A soak is a measuring instrument, and an instrument that has never been shown to be wrong is
> an instrument nobody has looked at hard enough. A soak that has never found a defect in
> *itself* has not been read closely enough to be trusted about anything else.

## 23.8 The adversarial review

The gate, the suite, the audit and the soak all check that the system does what it is built to
do. The adversarial review checks the other thing: **that what it is built to do is reachable,
and that the reasons it gives when it refuses are true.**

Its justification is one paragraph of evidence. Between 2026-08-31 and 2026-09-01, four defects
were found; every one had passing tests, several had passing *mutation* tests, and none was
found by the suite. They are the last four rows of the table in §23.2.

The method:

**One server, many reviewers.** One SANKHYA process, one warehouse, one schema per reviewer.
Nobody runs `cargo` — a workspace build takes minutes and tens of gigabytes, and several at once
take the box down; a review that kills the machine it is reviewing has proved nothing. Sharing is
not only a concession to the machine: several clients reading and writing concurrently in several
schemas *is* the isolation the server claims to provide, exercised by people trying to break it.

**Two kinds of client, because they fail differently.** Real `psql` 17.11 sends the catalogue
queries real tools send, in the spellings they send them — which is how `\dt` and the settings
queries get exercised without anybody thinking to write them down. The Python binding catches what
a program can and cannot do with the answer.

**Distinct lenses, not more reviewers.** Reviewers told to "find bugs" return the same three
findings. Each is given one property: reachability, refusals, isolation, concurrency and
durability, correctness.

**Every finding is verified before it is believed.** A reported finding is a claim. Each is
independently attacked — the verifier's job is to *refute* it — and only what survives is acted
on. Refuted claims are kept too: a refuted claim says where the system is confusing enough that a
careful reader got it wrong.

**Rounds, not one long run.** Round one runs blind; round two is aimed at what the first
disturbed. A single long round spends its second half re-finding what its first half already
found.

The set-up alone produced two findings before any reviewer started, which is worth recording as
evidence that the method works before it is applied. An ambiguous bare name said *"table not
found"* — true and useless, since a user cannot tell that from a typo; the refusal now names both
candidates and says to qualify, with the SQLSTATE unchanged, because a client dispatches on it and
a message is not an API. And the harness needed a client, so the Python SDK was written as the
review's instrument rather than as a deliverable, which meant its first user was somebody trying
to break it.

## 23.9 What is not verified

Stated plainly, because a testing chapter that omits this is the thing it warns against.

| Claim | State |
|---|---|
| The multi-day soak at acceptance scale with the reading workload | **Not done.** M6 exit criterion 4. The four-hour run at ten gigabytes did not read its data, so it discharges the criterion only for the paths it touched. At the acceptance scale a run needs at least a week to speak about three weeks |
| Recovery objectives, and anything needing a second machine | **Not measurable here.** M8 criteria 7–8 and §12.2 moved whole to M12. An objective measured on one host silently excludes network detection, machine loss and clock skew |
| Graph performance against a named public suite | **Not met, and not claimed.** M4's one carried criterion. The primitives are correct against brute force and bounded by construction; they are not measured at scale |
| A client/server compatibility matrix | **Not met.** One server version exists, so there is no matrix. Honestly untestable rather than skipped |
| The archive attestation drill against a real non-production archive | **Not done.** M9's gate criterion 3, which cannot be produced from development. Destructive purge stays disabled until M11 clears it |
| Production reconciliation | **Not schedulable by development.** M11 needs a deployment that does not exist |
| The two-instruction race class | **Not covered**, deliberately, and carrying no mutation entry. `loom` is the recorded next step |
| `pg_dump` against the wire protocol | **Recorded as failing** rather than omitted, because somebody will try it |
| The Parquet page row-count limit claim | **Unverified.** Measured for filter pushdown, not for this |
| The *"what does not exist"* list in `STATUS.md` | **Has accreted.** Several entries were written against M1 and have been answered since without being removed. Item-by-item audit is outstanding work |

Two more limits belong here because they are limits of the method rather than of a milestone.
`check-docs` catches only the mechanically checkable half of documentation rot; whether prose still
describes what the code does is a review responsibility. And `check-vocabulary` catches leakage,
not shape — a core can be immaculately neutral in its naming and still be bent toward one
industry, and only the reference packs catch that.

---

**The general lesson**, which is the reason this chapter is the length it is: every mechanism
described above was added after something got through the mechanisms that preceded it. The suite
did not catch what the mutation audit caught; the mutation audit did not catch what the gate
caught; the gate did not catch what the soak caught; and the soak did not catch what four people
with a `psql` binary caught in a day. Nothing here suggests the sequence has ended.

## A test that passes without running

`cargo test` shows a **passing** test's output to nobody. That is ordinarily a kindness, and it
was the hiding place for the worst measurement defect in this repository.

Fifteen end-to-end tests need a real PostgreSQL. Given none, each printed `skipping: set
SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run` and returned `ok`. The print went into a buffer
that is discarded on success, so the suite reported a pass, `check-all` reported green, and
nothing anywhere said that fifteen tests had declined to do anything --- among them
`read_your_own_writes`, which is **M1's headline property**, asserted complete by nine
documents.

This is not a test that fails silently. It is a test that **passes** silently while proving
nothing, which is strictly worse: a failure at least argues with you.

`sankhya_testkit::skipped` records the decision to a file instead, because a file survives
capture. `check-tests` empties it before the run and reads it after:

```
   DID NOT RUN    read_your_own_writes: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run
   15 test(s) passed without running --- set SANKHYA_REQUIRE_E2E=1 on a machine with
   PostgreSQL configured to make that a failure
```

A machine with no PostgreSQL is a legitimate machine to develop on, so this is reported rather
than refused. A machine that **cannot tell you** which tests it did not run is not legitimate,
which is why the report is unconditional and why CI sets `SANKHYA_REQUIRE_E2E=1`.
