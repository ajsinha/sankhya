<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Build Status

**Updated:** 2026-08-26 · Tracks what is *actually built* against
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).

This document exists because a plan describes intent and a roadmap describes ambition;
neither tells you what runs today. Where the two disagree, this one is right.

---

## Milestone progress

| Milestone | Planned | State |
|---|---|---|
| **M0** Foundations, spikes, walking skeleton | 10–12 ew | **Complete**, merged to `main` |
| **M1** Zero-configuration sync and read-your-own-writes | 14–18 ew | **Complete** |
| **M2** Ingest correctness and durability | 24–28 ew | **Substantially complete** — batching invariants, source-safety ladder, reconciliation, idempotence, crash safety, schema evolution and the backfill handoff all exist and are tested. What remains is the slot *lifecycle* driver and the snapshot *reader* — the correctness contracts are in place, the machinery that runs them on a timer is not |
| **M3** Query engine and storage performance | 28–34 ew | **Complete**, all six exit criteria met — closed 2026-08-26. One criterion was corrected first: it required cancellation inside user code, which does not exist until M4, and that clause moved to M4. Parts of the work breakdown remain unbuilt and are listed under *M3, closed* below |
| **M4** Graph engine and the extension mechanism | 26–32 ew | **Complete.** Every exit criterion met; see below |
| **M5** Tenancy, security and API surfaces | 22–28 ew | **Closed.** Four of five exit criteria met; the fifth needs a second server version to exist. Two of four API surfaces built — the wire protocol and Flight SQL. The control plane and its gateway are **deferred to M6**, because what they expose is built there |
| **M6** Operability, packaging and hardening | 2026-08-28 | **Complete.** Six of seven exit criteria met. Criterion 4 accepted on a forty-five-minute judged run by owner decision — `PASS` over 44 minutes with all seven measures steady, on the first soak to exercise a cube. **Criterion 7 carried into M8**: §10.8's size decision and route table are built and tested; the gRPC transport and every write path are not. See [SOAK.md](SOAK.md) |
| **M7** Multidimensional analysis — cubes, slice/dice, roll-up, consolidation | 2026-08-28 | **Complete.** All eight exit criteria pass against a cube hydrated from a published table. Declared complete once before, on 2026-08-27, and retracted the same day: the hydration path did not exist and every criterion passed on cells its own fixture supplied. Both that gap and the write-only materialisation found on 2026-08-28 are closed. See below, and [ADR-0007](adr/0007-the-cube-model.md) |
| **M8**–**M9** **Concurrency and data safety**, scale-out, then tiering | — | Next. **Rescoped 2026-08-28 to 24–30 ew** by owner directive: an end-to-end concurrency audit found a version claim that could lose a commit silently, four files published non-atomically, and three reclamation paths guarding against a proxy rather than against readers. §12.1 runs before scale-out. See [ADR-0013](adr/0013-concurrency-and-data-safety.md). Also carries soak criterion 7 — the gRPC transport and write paths — and the scheduled multi-day run |

---

## Partition fan-out, found by the soak on its first honest run

The fix for the partitioning defect introduced a different one, and `FR-CDC-14` names it:

> A single commit batch SHALL NOT produce unbounded file fan-out. A batch touching many
> partitions MUST NOT write one tiny file per partition **without a guard**.

`Publication::append` wrote one file per partition per batch, with no guard. A 200,000-row
batch spread over ninety days becomes ninety files; a 5,000-row append becomes ninety files of
fifty-five rows.

**Measured, not reasoned about: 32,279 live files across ten tables in four minutes,
averaging 37 KB, against a compaction policy that targets 256 MB.** The soak's judge breached
`live_files` on its own — *"compaction is not keeping up with the write rate… if the peaks
climb, each cycle starts further behind than the last"* — which is the harness doing exactly
what it exists for.

**This is the argument for routing the soak through `sankhya-publish`, demonstrated.** The
previous harness wrote through its own code and reported `PASS` on flat, non-conforming tables
for hours. One run through the shipping write path surfaced a MUST violation in four minutes.

`ARCHITECTURE` §6.4.2 had specified the guards and nothing had built them. `sankhya-publish`
now has `fanout`: a minimum file size below which a partition waits, a deferral age after
which it is written however small (waiting for ever is not deferral, it is loss), a cap on
partitions per commit, and the bulk path — feed an `Accumulator` everything, flush once, and
each partition is written once in full rather than once per input batch.

**The alarm is the part that matters**, and §6.4.2 says so: *"the guards buy time; the alarm
gets the design fixed. Silently absorbing it would be the failure."* `Strain::explain` reports
sustained fan-out and names the cause — a partition granularity finer than the arrival
pattern — because no amount of deferral fixes that.

## Date partitioning, corrected 2026-08-27

Found by being asked whether Delta was following the partition-by-date decision. It was not,
and the tables were **malformed** rather than merely unpartitioned.

Every table declared `partitionColumns: ["sank_data_date"]` and then wrote every file flat at
the table root with `"partitionValues":{}`, against a schema that did not contain the column.
It existed in no location the Delta protocol defines. An external engine reads such a column
as null for every row and prunes nothing — and `CON-08` requires Spark and Trino to read
these tables directly, `FR-STORE-20` requires every analytical table to carry the column and
be partitioned on it.

Corrected in `sankhya-publish`: the column is part of the schema, files land in
`sank_data_date=YYYY-MM-DD/`, every add action carries its partition value, one batch
spanning several dates becomes several files in a **single commit**, and each file carries
the date of the partition it sits in rather than of the row — a stamp disagreeing with its
directory is a table that reconciles differently depending on which a reader trusts.

**It survived because of a test named for the layout that tested a string formatter.**
`the_partition_path_is_what_an_external_engine_expects` publishes nothing and reads nothing.
The replacements assert against the files on disk, the commit log, and the Parquet contents.

### Still not partitioned: the ingest path

`sankhya-ingest` creates tables with no partition columns and writes flat. That is internally
consistent — it declares nothing and delivers nothing — but it means `FR-STORE-20` is met on
the batch publish path and **not on the streaming arrival path, which is where most data
lands**. `FR-STORE-24` is explicit that the column is derived *during ingest* and lives on the
analytical side, which is precisely this path. `Onboarded` carries no date axis, so the
automatic layout selection `ARCHITECTURE` §9.8 describes is absent rather than unwired.

**And there is no timestamp to derive one from.** `_sankhya_commit_ts` is declared as a
system column on every ingested table and written as literal `0` for every row —
`encode.rs` appends zero, and `Mutation` carries `commit_lsn` and no timestamp at all. The
comment beside it says the value is "recorded for human reading only", which it is not: it is
recorded for nothing. So an ingest date axis would have to come from the wall clock at write
time, and that is a decision to take deliberately rather than to discover halfway through the
change.

Not started. Sized here rather than begun, because the CDC commit path carries careful
crash-safety reasoning — sequence-derived names, commit-strictly-after-write, rebasing on
version conflict — and a partitioning change touches all three.


## M7, complete

Added 2026-08-27 by owner directive and placed before scale-out: cubes are a stated
differentiator and multi-node deployment is table stakes.

### What was built

| Crate | Layer | What it holds |
|---|---|---|
| `sankhya-cube-algo` | 1 | The additivity algebra, ancestor answerability, hierarchy consolidation, the cuboid lattice and greedy selection. No dependencies |
| `sankhya-cube` | 3 | The validated `Cube`, consolidation on the graph engine, the sparse cube and its navigation, completeness, materialisation, the write-back overlay |
| `sankhya-cube-sql` | 4 | Roll-up and slice as table functions, with everything that qualifies a number as a column |

### The three findings worth keeping

**Criterion 3a was not met, and the cause was arithmetic.** A cube rolls up in stages, every
stage rounds, and `round(round(a+b) + round(c+d))` is not `round(a+b+c+d)`. Fixing the
*order* of summation makes one reduction reproducible; it does nothing about
**associativity**, and a materialised cuboid is exactly a re-association of the same
addition. Measured at one ULP — large enough for two reports to disagree by a penny, small
enough that nobody can point at a defect. A materialised aggregate now stores its value
unrounded as a Shewchuk expansion, and rounds once when read.

**Criterion 2 is met in a stronger form than its own wording.** It asks that summing a
semi-additive measure across time be *rejected at planning time*. It is not rejected — it is
**not expressible**, because the reduction operator is the measure's and never the caller's.

**Completeness cannot be computed from what survived.** A withheld row leaves no trace, so
an aggregate counting what arrived and dividing by what arrived reports itself complete
however much policy removed. The withheld count comes from the filter or it does not exist.

### The gap that was found by being asked, and then closed

**The hydration path did not exist.** A `Definition` named a fact table and validated that
the name was well formed; nothing read it. Every `Cells` in existence was built by a
navigation operation or a test fixture, so the cube was an algebra with a SQL façade over
data the caller supplied. All eight exit criteria passed, and every one of them supplied its
own cells — which is precisely why passing them did not surface it. **A criterion that never
has to read a published table cannot tell you whether the cube can.**

`hydrate.rs` and `publish_from_fact_table` now close it: the cube reads the table its
definition names, through the same session that will query it. `tests/end_to_end.rs` supplies
a *table* and makes the cube find it, which is the test whose absence allowed the claim.

A row that cannot be placed — a null key, a null measure — is **counted, never dropped**, and
returned as a `Completeness`. Skipping such rows leaves totals quietly short, which is the
same failure as a policy-filtered total presented as complete, so it gets the same machinery.

**A related defect in the same area.** The SQL surface computed `Completeness::complete(rows
that survived)`, which reports complete however much was lost — the exact trap
`sankhya_cube::complete` documents, implemented one crate away from the warning. Completeness
now comes from hydration and is a required field on `Published`, so a fixture cannot quietly
claim a cube saw all of its input.

### What is still not there

MDX, deliberately — see ADR-0007.

The query log now exists (`sankhya-cube/src/querylog.rs`), so §11.6's greedy selection runs
against what people have actually asked for rather than against the whole lattice. A bounded
ring per cube, where the repetition *is* the weighting: a shape asked ten times appears ten
times and counts ten times, recency falls out of old entries being overwritten, and a cube
nobody has queried gets its base cuboid and nothing else. It records a shape — which cube,
which dimensions were grouped by — and has nowhere to put a member, a predicate or a
principal, which is worth asserting rather than assuming of a query log.

### Materialisation was write-only, and four things were wrong at once

Found 2026-08-28 by asking the compiler which methods the server never calls. The answer was
`materialised` and `cuboid_root` — **so the refresher built cuboids on a timer and nothing
ever read one.** The storage was spent, the target lag was checked, the sweep collected the
old ones, and every query went to the fact table regardless. The seventh instance of a
capability that is built, tested and unreachable, and the most expensive, because this one was
also writing files.

Four defects, and each hid the next:

**The test that should have caught it was named for the behaviour and did not test it.**
`a_materialised_cuboid_answers_without_reading_the_fact_table` said in its own comment
*"Proved by taking the fact table away. If the answer still comes back, it did not come from
there"* — and it never took the fact table away and never re-queried. It wrote a cuboid by
hand and asserted the file existed. It now does what it says, and the first rewrite of it
still failed to: a second connection to the same process is served from the in-memory cache,
so the fact table can be deleted and the answer still arrives with no cuboid involved. It
takes a **restart**, which is the thing a cuboid is actually for.

**A cuboid could not say how much of the fact table reached it.** `Completeness` was not
stored, so a cuboid read back could only ever be served as complete — the exact trap
`sankhya_cube::complete` documents. It is now two columns in the stored batch. Not Arrow
schema metadata: a probe wrote a batch with metadata through the product's writer, read it
back through DataFusion and got `{}`. Storing it where a reader cannot see it would have been
worse than not storing it, because the absence would have looked like a value.

**The unrestricted cuboid could serve nobody.** The refresher writes scope 0, and
`Guard::scope_digest` hashes the tenant and the table, so no real caller's digest is ever 0
and the lookup could only miss. The rule from ADR-0008 is now stated as what it means — an
unrestricted cuboid may serve a caller whose guard **withholds nothing** — and asked of the
guard rather than of a digest comparison that cannot succeed.

**The provenance column reported its own input.** `materialised` was filled from the
`materialise` argument the caller passed, so the column answering *"why was this fast?"*
answered with whatever the query had typed. It agreed with reality by accident for the whole
of M7, because nothing served a cuboid and the honest answer was `false` for every query ever
run. It now reports `Published::from_cuboid`.

### A journey test that never ran

Found by the same question. `crates/sankhya-server/tests/five_minutes.rs` — the seven-step
first-user journey, and the file whose own header says *"this test runs on every build and
proves the path works"* — had **no `#[test]` attribute on its function**. Nothing ran it. It
passes in 0.05 s and always would have.

Worth recording next to the cuboid finding because it is the same failure in a different
place: a claim about verification that nothing was checking. The file even contains the line
*"a budget that can never be reached is documentation with a `#[test]` attribute on it"*,
which it managed to be the inverse of.

### Tutorials, and why they are executed

`docs/tutorials/` was added: four hands-on documents covering a first cube, making one fast,
completeness under policy, and every refusal with what to do instead.

Their SQL is executed by the same test as the guide's, and a second test asserts that every
file in `docs/tutorials/` is in that list — so a tutorial cannot be added and quietly left
unverified. A tutorial is the document a reader trusts most, because they are following it
step by step with no independent way to tell a stale instruction from a current one, so an
untested one rots in the worst place available.

The first draft proved the point immediately: it used `cube_names()`, which does not exist,
and `by=region, period`, which parses `period` as a separate option because the options string
is itself comma-separated. Both were caught by the test on the first run.

### §11.6's three levels of control, wired

| Level | Who sets it | How |
|---|---|---|
| Definition | whoever models the cube | `pinning(["region"])`, persisted in the catalogue |
| Configuration | the operator | `cubes.budget_rows`, read at startup |
| Session | the caller | `materialise=false` or `materialise=pinned` on the query |

The session level only ever narrows. There is no spelling that widens anything, because a
caller who could raise the budget would be granting themselves an operator's storage — a
resource exhaustion with a polite interface.

`materialise=false` also bypasses the in-memory cache when what is cached came from a cuboid.
Without that the reproducibility check compares a cuboid against itself and agrees, which is
the one way it can fail to do its job.

Two smaller things fell out. `Arguments::boolean` became dead once the provenance column
stopped reading it, and was removed rather than left as a helper nothing calls. And
`materialise`'s *value* is now validated: `args.rs` refuses an unknown option name, but a
value it never reads is never checked, so `materialise=pinnd` would have passed the name check
and silently taken its default — the failure unknown-option refusal exists to prevent,
arriving through the value instead of the name.

Four ADRs were written for what comes next, and each was prompted by a question worth
recording: [ADR-0008](adr/0008-serving-cubes-under-policy.md) on serving under policy,
[ADR-0009](adr/0009-the-cube-lifecycle.md) on the three lifetimes,
[ADR-0010](adr/0010-external-aggregations.md) and
[ADR-0011](adr/0011-sdaf-declared-dependencies.md) on external aggregations that declare what
they need, and [ADR-0012](adr/0012-open-capabilities.md) on what a standing artefact must
declare before the system will maintain it on somebody's behalf.

### A cube that could only exist at compile time

Cube definitions were *"registered against a session by the embedding application"*, so a
cube lasted exactly as long as a process and a server could not serve one. That read like a
missing storage layer. It was not.

`Measure` held `name: &'static str` and `rules: &'static [Along]`. **A measure was a
compile-time construct**, so a definition could name only measures a Rust source file had
already spelled out, and no amount of persistence code could have loaded one — a definition
read from disk has nowhere to put its own measure's name. The persistence gap was a symptom
and the type was the cause.

Both are owned now, which cost an allocation per declared measure, once, when a definition is
built. `catalogue.rs` writes a definition to `<warehouse>/_cubes/<name>.json` and reads it
back, under an underscore so table discovery and the orphan sweep already skip it — a cube
definition is not data and must never be mistaken for a table.

The stored form is its own type rather than `Serialize` on the model. `sankhya-cube-algo` has
zero dependencies and serde would end that; and a stored definition is a *format*, so
deriving it from an internal struct silently promises never to rename a field. Here a
refactor breaks a compile instead of orphaning every cube on disk.

**An unknown rule name is refused, never defaulted.** A `Last` that came back as `Sum` turns a
closing balance into the sum of twelve month-end balances — right magnitude, right sign,
entirely wrong — and defaulting is precisely the failure the additivity model exists to
prevent. A catalogue file that will not parse is reported with its path rather than skipped,
because a server that comes up healthy with a cube missing sends somebody to the wrong place.

### The server knows its cubes, and cannot yet answer with them

The catalogue existed and nothing read it — the same shape `sankhya-maintenance` was in that
morning, and a capability nothing in production reaches is indistinguishable from one that
was never built. The server now loads every definition at startup, **validates** it, and
reports the count and any failure beside the tables that would not open. A malformed
definition is a complaint, not an outage: one bad JSON file must not stop a server whose other
cubes and every table are fine.

Validating at startup rather than at first use is the point. A measure with no rule along a
declared dimension is exactly what the additivity model exists to catch, and catching it when
somebody runs a query means reporting a deployment error to a user who did nothing wrong, at
whatever hour they happened to ask.

### Cubes answer, under the policy the caller is subject to

A cube is queryable. `cube_rollup` and `cube_slice` are registered into the session a
statement runs in, and hydration reads the fact table **through that session** --- so the
cells a cube is built from are filtered by the same `SecuredTable` that filters a plain
`SELECT`.

That is the whole authorization story, and the absence is the point: there is no
cube-specific authorization code, so there is no second implementation of the rule to
disagree with the first. Two principals with different entitlements get different totals, and
the test asserting it is the one that would fail loudest if that ever stopped being true ---
100 unrestricted against 30 for a principal filtered to one region.

Hydration is cached across statements, keyed by *(cube, measure, definition version,
snapshot, scope digest)*. The scope digest hashes what a guard **permits** --- tenant, table,
action, row filter, column masks --- and deliberately excludes the subject, so a thousand
analysts across six roles produce six entries rather than a thousand copies of six answers.
Any difference in what is visible changes the digest; who is looking does not.

`Guard::scope_digest` carries four mutations against it, including the one that is easy to
forget: putting the subject *in* would be safe and useless, and a suite testing only
separation would not notice.

### A client can discover a cube instead of hardcoding it

`cubes()`, `cube_dimensions(cube)` and `cube_measures(cube)` are ordinary table functions
returning ordinary rows, so a UI, a notebook or an agent composing SQL discovers the model
with the same `SELECT` it uses for everything else. No second protocol exists to keep in step
with the first.

Two columns are there because a client that lacked them would draw something wrong rather
than fail. **`depth`** carries the level order — coarse to fine is a fact about the model, not
about how rows arrived, and a client sorting the result without it draws a list where there is
a hierarchy. **`composes`** says whether a measure can be rolled up at all — a UI offering
"roll up by time" on a ratio offers a button that cannot work, and finding that out at query
time is worse than not offering it.

Describing reads no data. A picker that cost a hydration per keystroke is a picker nobody
leaves switched on, so hydration stays gated on a statement naming a navigation function while
description is registered always.

### Cuboids on disk, refreshed without a caller, and collected

The materialised tier of [ADR-0008](adr/0008-serving-cubes-under-policy.md) and the Maintained
lifetime of [ADR-0009](adr/0009-the-cube-lifecycle.md) are built.

A cuboid is a published table keyed by *(definition version, snapshot, **scope**, cuboid)*.
The scope is the addition, and here it is stronger than a cache key: **two scopes are two
tables**, so a bug in the lookup cannot serve one principal's rows to another, because the
rows are not in the file being read.

Cells are stored as the components of their Shewchuk expansion and rounded once when read.
Exit criterion 3a asks for bit-identical results with materialisation on and off, and a cube
rolls up in stages where every stage rounds — fixing the *order* of summation makes one
reduction reproducible and does nothing about **associativity**, which is exactly what a
materialised cuboid is. A test round-tripping one cell passed under a mutation that stored the
rounded total; the loss only appears when a stored partial is added to another, so the test
that catches it rolls two of them up.

`target_lag` is a **staleness target, not a schedule** — Snowflake's framing for dynamic
tables. Staleness here is exact rather than estimated, because a cuboid records the version it
was computed at, and a cuboid past its target is never served as though it were fresh: the
answer falls back to live aggregation and says `materialised = false`.

A maintained cube is built **with nobody logged in**. The refresher has no principal, so it
builds the *unrestricted* cuboid — which may serve only an unrestricted caller. **Background
refresh therefore helps dashboards and service accounts and does nothing for a restricted
analyst**, whose cuboids are built by their own queries. Pre-building named scopes is a
decision nobody has made and is not taken by implication.

And superseded cuboids are collected. One at an old snapshot can never be selected, so it is
garbage the moment the table advances — and it fell between the two mechanisms that existed:
the orphan sweep finds unreferenced files *within* a table, and this is a whole table no log
mentions. The same shape as the defect that filled a disk in the soak, reintroduced by adding
cuboids and closed the same afternoon.

### The cube path under sustained load, with materialisation actually reachable

`PASS` over 59 judged minutes against a two-hour horizon, all seven measures steady, on
2026-08-29 --- **the first soak in which a cube was served from storage.** Until M7 closed the
day before, `materialised` was dead code and every cube query went to the fact table however
many cuboids the refresher had built.

196 rounds, 1,960 publications, **37.2 GB read across 3.01 billion rows**, the cube answered 49
times, and maintenance reclaimed 11.66 GB over 1,651 ticks while holding the warehouse at 9 GB
and 90 live files throughout.

Resident memory closed at **2.0 GB against the 2.2 GB of the previous run** --- more work, a
longer run, and less memory. That is the direction materialisation was supposed to move it and
the first measurement rather than the first assertion: a cuboid read replaces a fact-table
hydration rather than adding to it. See [SOAK.md §7b](SOAK.md).

### The earlier cube run

`PASS` over 44 judged minutes with all seven measures steady, on the first soak to exercise a
cube — 157 rounds, 2.41 billion rows scanned, the cube answered 39 times, maintenance
reclaimed 11.42 GB. See [SOAK.md](SOAK.md) for the two defects the earlier runs found and why
neither was visible to a unit test.

Resident memory settles at **2.2 GB against a 779 MB baseline without cubes**. That difference
is measured rather than assumed, and it is a plateau rather than a climb: the last reading
*fell* by 128 MB, and a leak does not give memory back.

The remaining cost is `Contributions` retaining every raw `f64` per cell — roughly 1.5 GB of
plateau. It is real and it does not grow, which changes it from a correctness risk to an
optimisation with a known price. Doing it means swapping `deterministic_sum` for `Exact`, which
moves `Sum` in the last bit and lands directly on exit criterion 3's bit-identity tests — so it
is worth doing deliberately rather than under the impression that something is leaking.

**Answering from a materialised ancestor now happens.** It was the last piece of §11.6 that
was designed and unreachable, and closing it mattered for a reason beyond completeness: exit
criterion 3b — *a non-additive measure is never answered from a materialised ancestor* —
was passing **vacuously**, because nothing answered from an ancestor at all.

`plan` picks the narrowest cuboid that may legally answer, and a cuboid is a candidate only
when the measure permits every roll-up between it and the query. The grain a statement needs is
the union, across every cube call in it, of the dimensions grouped by and the dimensions a dice
restricts — the second because `where=region:north` must find a `region` column to restrict,
even though slicing drops that axis from the result.

Two things fell out of it, and both were the same shape as the milestone's other findings.
`materialised` was reading a cuboid at the **cube's** grain rather than the one its key names,
which worked for exactly as long as the only cuboid ever read was the base one — whose grain
*is* the cube's — and named a missing column the moment an ancestor was chosen. And the first
test of the dice rule could not fail: every materialised cuboid in its fixture happened to
contain `region`, so it could not tell the two behaviours apart, and a mutation dropping
`where=` survived it. It now pins `[period]`, which puts a cuboid on disk that is cheap,
current and unable to express the query.

**What is still not there.** A cuboid is pre-built only for the unrestricted scope.

The statement text is scanned for cube function names to decide what to hydrate, which is
deliberately crude: a `SessionContext` is built per statement, `TableFunctionImpl::call` is
synchronous while reading a table is not, and a false positive costs a cache lookup while a
false negative costs a query that fails to resolve a cube it named. It is replaced when the
surface grows a resolver of its own.

<details>
<summary>Superseded: why this was blocked before 2026-08-28</summary>

**A cube was loaded but not queryable, and that was a design decision rather than an
omission.** Hydration had nowhere correct to go:

- `session_for` builds a context **per statement**, so hydrating there reads the whole fact
  table on every query.
- Hydrating once at startup and sharing the cells across principals would hand every caller
  the same totals whatever policy says — the disclosure through arithmetic that `FR-QUERY-13`
  and §11.5 exist to prevent, and the kind that leaves no trace in a result.

The correct answer was a cache keyed by snapshot **and** the principal's visible scope ---
which is what was built.

</details>

The other absent half — a definition as a row in a system table, so the store is the warehouse
rather than a JSON file beside it — waits on the catalogue proper.


## The defect wiring maintenance into the server introduced, and fixed

Two decisions, each correct alone, were not put together.

A server resolves its table providers **once**, in `start()`: `resolve_with` reads the log,
builds a file list, and the provider holds it. That was sound while a served warehouse did not
move --- the server runs no ingest, and `Settings::read_as_of` says so.

Then the server started **maintaining the warehouse in-process**. Compaction replaces files
and retirement deletes the ones it replaced, so the warehouse moves whether or not anybody is
writing to it. A provider fixed at boot names files that are gone, and the query fails with a
missing-file error naming a path nobody asked about.

**Retirement's grace period is not the protection here.** It protects a reader that listed
shortly before a merge --- twenty-four ticks --- and cannot protect one that listed at startup
and has been serving from that listing since. With shipping defaults a deployment would have
begun failing queries roughly twelve minutes in: one grace period after the first merge.

Reproduced before it was reasoned about, because reasoning about it is how a wrong answer gets
written down confidently. `crates/sankhya-server/tests/maintenance_and_readers.rs` holds both
halves: the provider from before retirement can no longer read, and a server re-resolving
before it registers reads every row across its own maintenance.

**And it fixed something else that had been true all along.** A running server never saw data
committed after it started. That was defensible while the warehouse did not move; re-resolving
a table whose log has moved makes new commits visible as a side effect worth having.

---

## M6, closed 2026-08-28

Criterion 4 accepted on a forty-five-minute judged run by owner decision, with the gap from
the multi-day pipeline it asks for written into `IMPLEMENTATION_PLAN.md` rather than argued
away. Criterion 7 --- the gRPC transport and the write paths --- is carried into M8, not
waived.

### The soak, and what it took to make it say anything

Three runs. The first exhausted the disk at t+2833s and wrote a **zero-byte report**
explaining why: compaction replaced files and nothing retired them, because
`sankhya-maintenance` was a library the server did not depend on and nothing in production
ever called. The second died one sample short of `live_files`' first verdict. The third
returned `PASS` with all seven measures steady --- 168 rounds, 2.58 billion rows scanned,
resident memory ending at 779 MB, maintenance reclaiming 29.91 GB across 911 ticks.

Both causes were fixed where they were, not worked around: maintenance runs on a thread the
warehouse owns and the server starts at boot, and the warehouse's own size is a watched
measure with a stated budget enforced *in the loop*, so a breach is reported while there is
still room to write the report.

### Two defects the passing run reported about itself

**The fan-out alarm never de-duplicated.** It keyed a "report once" set on its own rendered
message, and the message counts batches --- so every rendering was unique and a standing
condition printed 168 times, which is exactly what the code's own comment forbids. It now
keys on the shape of the strain: which table, the average rounded to a whole partition, and
the widest batch. A *worsening* condition is a different condition and is still reported.

**And what it was reporting was a workload nothing produces.** Every row was dated `id % 90`,
in the fill and in the steady-state rounds alike, so every batch touched all ninety
partitions for the whole run. That is a backfill --- which the fill genuinely is --- and it
is not what arrival looks like afterwards. A source feeding a warehouse continuously produces
rows dated *now*, touching one partition or two across a midnight.

So the alarm was right about what it was shown, and what it was shown was a backfill labelled
as arrival. The harness now models both, with the newest day advancing as the run goes on so
the hot partition moves and compaction has to keep up with a partition being appended to
rather than one that is finished. **The daily axis `FR-STORE-20` mandates is not the
problem**; the harness's idea of arrival was.

### §10.8 — A gateway that refuses to become the bulk plane

`FR-API-06` is the interesting half, and its reason is a **product** reason rather than an
operational one:

> Bulk data SHALL NOT be offered over JSON. Serializing analytical results as JSON destroys
> the zero-copy premise and **defines published benchmarks downward**.

The failure it prevents is not a server running out of memory. REST is the convenient
surface, so people will use it for bulk extract *because* it is convenient — and then measure
the system through it. A columnar engine benchmarked through a JSON encoder is a JSON encoder
benchmark, and that is the number that gets published. The cap exists so the convenient path
does not become the measured path, which is why it is hard rather than raisable.

**A large result is a redirection, not a refusal.** It comes back as a **Flight ticket** —
the same query, already planned and authorized, redeemable over the columnar path. A `413`
sends somebody to ask for a bigger cap; a ticket sends them to the surface built for what
they are doing.

**You cannot count the rows to decide whether to return the rows.** Materialising a result in
order to measure it is precisely the cost the cap exists to avoid, so the decision is taken
from the plan's estimate before anything is materialised. Estimates are wrong, so there is a
second guard: encoding stops the moment the actual output passes the cap, and the partial
response is **abandoned rather than truncated** — a JSON array cut short is either invalid or,
worse, valid and silently short, and a client cannot tell the second from a small answer.

**Routes are declared and matched whole.** The scrape endpoint served
`/metrics/../etc/passwd` on a prefix match in `§10.2` — harmless there because it reads no
files, and exactly the shape that becomes a traversal the moment something does. Once was
enough to make it a rule rather than a fix.

**What `FR-API-04` names and this does not serve is recorded as data, with reasons.** Jobs,
archive operations, mutating tenancy and policy administration, and the structured graph API.
An API that quietly omits half a requirement reads as complete — and the jobs case is the
sharpest: with no scheduler running, the endpoint would list nothing forever, and a client
cannot tell *"no jobs are running"* from *"nothing runs jobs"*. That is the same reasoning
that deferred the control plane out of `M5` in the first place.

**Not built:** the gRPC transport itself, and the write paths. What exists is the surface's
shape and the decision `FR-API-06` turns on, both tested; wiring them to tonic and to an
audited write path is the remainder.

---

### §10.7 — The soak, and four attempts at judging one

The section read *"A multi-day soak"* in its entirety, which is not something anybody can
fail. The criterion is now falsifiable — **no bounded measure projects a crossing within the
observation horizon** — and inconclusive counts as a failure, because a run whose sampling
broke must not report the same green as one that ran properly.

**Three kinds of bounded, not one.** The naive soak complains when any number rises, and half
of them are supposed to. A measure declares whether it must be flat, flat *per unit of work*,
or a sawtooth whose **peaks** must not climb. The second catches what a total never can — an
audit drifting from one record per query to two, while its total rises exactly as it should.
The third is a distinction no point-in-time diagnostic can draw: two oscillating series look
identical at any moment, and one is a system keeping up while the other starts each cycle
further behind.

**Four attempts, each finding something by running it:**

- **The diagnostic's linearity gate is wrong for a soak.** A healthy measure is noisy and
  flat, which has an r² near zero — there is no trend to explain — so the baseline run
  reported memory, descriptors and the history file as unjudgeable while nothing was wrong.
  The same trap as r² on a constant series, corrected in `sankhya-math` earlier for the same
  reason: undefined is not bad.
- **A half-second run reported a memory leak.** Sixty rounds finished in 0.35 seconds and a
  process allocates as it starts; across the opening of a run that looks exactly like a
  linear climb. Fixed by a declared warm-up prefix **and** by bounding the horizon to what
  the run observed — half a second extrapolated to three weeks is a factor of three and a
  half million. `sankhya-diagnostic` already carried that guard and applying it there and
  not here was the omission.
- **Every caller got the warm-up arithmetic wrong**, both of them, overshooting by exactly
  the prefix. A calculation both call sites get wrong on the first attempt does not belong at
  the call site.
- **A summary spans less than what it summarises.** Peaks sit inside their windows, so the
  peak series spans less than the run — and the entitlement is a property of the run.

**The harness is proven to notice**, which is the part that would otherwise be an untested
backup by another name: one injection per shape of failure, each required to fail the run.

**Not done:** the multi-day run at the ten-gigabyte scale, and retained evidence. The
scheduled run is a change of duration and scale rather than a first attempt at the whole
thing. [`SOAK.md`](SOAK.md) has the method, the numbers and the reasoning.

---

### §10.6 — Versions, and the difference between damage and the future

**Three on-disk formats were added this session and none of them carried a version.** The
backup manifest, the restore-drill evidence and the diagnostic history. That is the gap
`§10.6` exists to close, and it was made in `§10.1` and `§10.3` — which is how these gaps are
usually made: a format is invented to solve a problem, and versioning it is not part of the
problem.

**A parse error and "this is from the future" are different facts, and only one says what to
do.** An artefact written by a newer release, read by an older one, fails somewhere in the
middle of parsing — an unknown field, a number that will not fit, a restructured object. The
error reads `invalid type: string, expected u64 at line 14 column 9`, and an operator reads
that as **corruption**. They go looking for a damaged disk, a truncated write, a bad copy. The
answer was "upgrade the binary", and nothing in front of them said so.

So every format now carries its version **first in the file**, read before anything else is
understood, and a future version is refused by name — with both numbers and an instruction.
The ordering is the whole point: a version buried at the end of a JSON object is a version you
learn only after successfully parsing everything you were trying to avoid parsing.

**The four axes, and why independence is load-bearing.** `FR-OPS-11` requires the internal
schema, the database major version, the table protocol and the wire APIs to version
independently. One product version covering all four means every change to any of them is a
change to all of them: an upgrade touching only the wire protocol reads as a storage-format
change and gets the caution one deserves — and, worse, the reverse, a genuine storage break
hiding inside a release that looked like a wire change.

**Backwards is the direction that decides whether you can roll back.** A new release reading
old data is the easy direction and the one everybody tests. Whether the *old* release can read
what the new one wrote is the question, and after the upgrade is not the moment to answer it.
So every format declares its rollback consequence — `safe`, `tolerated`, or **`ONE-WAY`** — and
that column exists so the decision is visible *before* the upgrade rather than discovered
during the rollback.

**A corpus, because a fixture is an old binary's behaviour preserved.** Testing an upgrade
properly means running release *n−1* against release *n*'s data. One release exists, so there
is no earlier binary to run — and there does not need to be. What is needed is an earlier
binary's **output**: artefacts as previous releases wrote them, checked in and read by every
build. Unlike a binary, a fixture never stops building, never needs a toolchain that has been
removed, and is legible in a diff.

The fixtures are **hand-written rather than generated**, deliberately. A generated fixture
regenerates when the format changes, agrees with the current code by construction, and proves
nothing at all.

**What is not tested:** running the previous binary, because there is not one. That is the
honest limit of `§10.6` until a second release exists — and it is also what unblocks `M5`'s
carried-forward client/server version matrix.

[`VERSIONS.md`](VERSIONS.md) is generated from the declarations, including the rollback
procedure.

---

### §10.4 — Packaging, and the two numbers nobody relates

**The drain did not exist.** `serve_until` returned the moment shutdown resolved; its spawned
connection tasks were detached, so dropping the runtime cancelled them abruptly. The doc
comment above it said *"connections already running finish on their own, because cutting a
client off mid-result is indistinguishable to them from a crash"* — describing the behaviour
it did not have, which is how it survived review. A client mid-result saw a reset on every
deploy.

That had to be fixed before packaging could mean anything: **you cannot choose a termination
grace for a process that does not drain.** A `JoinSet` now holds the handles, shutdown waits
for them, and the wait is bounded — because an unbounded drain hangs on one stuck client
until the orchestrator's patience runs out and kills the process anyway, with the difference
that nobody chose the moment.

**Then the check that relates the two.** A server's drain deadline and an orchestrator's
termination grace live in different files, are edited by different people, and nothing
normally connects them. When the grace is the shorter, every deploy kills the server
mid-drain and clients see resets that look like crashes. `check-package` reads the drain out
of the source, reads the grace out of every manifest under `packaging/`, and fails when a
manifest allows less time than the server takes.

**The platform baseline, which is where a Rust binary usually fails to install.**
`IMPLEMENTATION_PLAN` §10.4 already called for *a build against an old platform baseline
rather than a fully static binary*. What it did not say is that a baseline nobody checks is a
baseline nobody meets. The declared baseline is `GLIBC_2.28` — RHEL 8, Debian 10 — and
`check-package` reads what the binary actually requires.

**This build requires `GLIBC_2.34`.** It would not start on RHEL 8, Ubuntu 20.04 or anything
older than RHEL 9, and nothing on the build machine can tell you that: the symbol is present
locally, so it links, runs and tests clean. It is discovered by a customer. The check reports
it as a warning on a development build and **fails** when `SANKHYA_RELEASE` is set, because
failing every local build on a property only the release environment can satisfy would train
everybody to ignore it — the same warn-versus-fail distinction `check-loc` already makes.

Meeting the baseline needs a build against an old sysroot, which is release-pipeline work.
The gap is recorded rather than papered over by lowering the declared baseline to whatever
this machine produces, which would quietly drop every enterprise distribution.

**A test caught the check being broken before the check caught anything.** `highest_glibc`
matched a `GLIBC_` prefix against the whole symbol token — but `readelf` writes
`statx@GLIBC_2.28`, so it matched nothing, found no requirements, and concluded every
requirement was met. It passed the real binary against a baseline it misses by six versions.
The unit test written against genuine `readelf` output is the only reason that did not ship,
and it is the reason the parser is tested on real output rather than on a convenient
sketch.

**The support matrix is data, not prose.** Five targets, each with its baseline and its
artifact formats, declared once in `xtask/src/package.rs` and generated into
[`PLATFORMS.md`](PLATFORMS.md). The number of build targets is the number of things that can
silently break, and a script per platform drifts from its siblings until one artifact behaves
unlike the rest for a reason nobody can find.

**The baseline of the self-contained artifact is set by PostgreSQL, not by the Rust binary.**
Worth stating because tuning the Rust build and declaring victory is the obvious mistake: our
binary could be musl-static and the bundle would still require whatever `glibc` PostgreSQL was
built against. `cargo-zigbuild` targets a chosen `glibc` for the Rust half without a
container; the C half needs an old sysroot, and a container is excluded from *running* this
system, never from building it.

**Windows, checked rather than assumed.** The objection I expected to be fatal — a
case-insensitive filesystem, where `Orders` and `orders` collide — is already handled:
`sankhya-schema` case-folds every path segment to lower-case ASCII, digits and underscores,
and refuses collisions rather than disambiguating them. The platform device names (`aux`,
`con`, `nul`, `com1`…`lpt9`) are already reserved, with a comment saying why. **A warehouse is
already Windows-path-safe.** What is missing is the vendored PostgreSQL build and service
integration, which is porting work rather than a design problem. So the honest row is *client
only*: any PostgreSQL driver connects from Windows today, which is what most Windows users
need, and the server runs under WSL2 or a container until somebody builds it.

**Two tests found defects in things I had just written.** Requiring every server target to
state a baseline caught macOS declared with none — it has one, `MACOSX_DEPLOYMENT_TARGET`,
and calling it "not applicable" said the question does not arise when in fact it arises and
nobody answered it. And a mutation shortening the Kubernetes grace below the drain
**survived**: the comparison was correct and nothing exercised it, because it lived only in
`check-package`. A check that is only a command is a check that is only sometimes made, so it
is now a test as well.

**Not built:** container images and signing. Both need infrastructure this environment does
not have — a container runtime, which the five-minute claim exists to avoid needing, and a
signing key. The manifests assume an image that a release pipeline has to produce.

---

### §10.5 — The five-minute experience

`M6`'s first exit criterion, and the plan is explicit about the form: *"it must be a test so
it cannot rot"*. `crates/sankhya-server/tests/five_minutes.rs` is the quickstart, executed on
every build — generate a warehouse, start the real binary as a subprocess, connect over the
real wire protocol, query, aggregate, run the diagnostic, take a backup, prove it. Seven
documented steps, and any of them breaking breaks the build.

**The measured journey is about forty milliseconds**, from a built binary.

**The claim as written cannot be met from source, and the test says so rather than measuring
around it.** A first-time user's five minutes includes `cargo build`, which takes several
minutes on a cold machine and is dominated by dependencies — `QUICKSTART.md` has always said
so. The five-minute promise is a promise about a **released artifact**, which makes exit
criterion 1 depend on `§10.4` packaging. Recording that is more useful than a green test
measuring the wrong interval.

**A thousand rows is deliberately small, and the suite has two larger sizes for the two
larger questions.** `check-performance` runs TPC-H at scale factor 1 on a quiet machine for
the latency objectives; the soak runs ten tables and ten gigabytes for days, to establish
that nothing grows without bound. Conflating the three is how a suite comes to prove nothing,
and `§10.7a` now specifies the third — which previously read, in its entirety, "a multi-day
soak".

**And tightening the budget does not rescue the timing assertion either.** This warehouse
holds a thousand rows in four files; no plausible scaling regression is visible at that size.
Somebody making the read path open every Parquet footer would still finish in milliseconds.
The budgets catch a phase *breaking* or slowing by two orders of magnitude — a deadlock, a
retry loop, a sleep left behind — and nothing subtler. Scaling belongs to
`cargo xtask check-performance`, which generates a scale-factor-1 dataset and sits outside
`check-all` for exactly that reason. Splitting them is the point: one proves the path works
on every build, the other proves it is fast on a quiet machine.

**Three defects, and one of them was in the test itself.**

- **The banner printed the configured address, not the bound one.** Told to bind port 0 it
  printed `:0` — the line whose only job is to say where to connect said nothing, and a test
  wanting an ephemeral port had no way to learn which one it got. In three places: the
  startup line, the `psql` invitation, and the metrics URL.
- **`describe()` printed it a second time**, so the real port appeared beside a literal `:0`.
- **The test hung instead of failing.** It read the banner with no deadline, so a mutation
  that stopped the server announcing its port blocked forever and took the whole build with
  it. Found by running that mutation. A test that hangs is strictly worse than one that
  fails, because a failure names what broke — the read is now bounded and reports "it never
  said" distinctly from "it took too long".

---

### §10.3 — Backup, protection and the restore drill

Three requirements, each existing because of a specific way backups fail.

**`FR-OPS-13` — the manifest refuses to exist rather than recording a disagreement.** The
requirement's reason is blunt: *"three backups that do not agree with each other are worse
than one"*. Worse, because three that agree restore a system and three that do not restore a
puzzle, with nothing to say which is the one to trust.

There turned out to be **two positions, not one**, and conflating them is the defect the
manifest exists to prevent. `source_restores_to` is where the transactional store lands.
`queryable_at` is the highest position at which *every* table is complete — the minimum over
their coverage, because a query joining two tables can only be answered where both reach.
They are rarely equal: tables publish at their own cadence. Recording one number and calling
it "the consistent point" means recording whichever one the author happened to think of, and
the difference between them is exactly how much re-capture a restore implies.

The rule enforced at **build** time: **no table may cover a position past where the source
restores to.** If one does, then after a restore the analytical tier holds rows the
transactional store no longer has; capture resumes behind them and republishes that range at
different positions. It is `SNK-S0002`'s shape one layer up, and it is not detectable
afterwards from either side alone — which is why it is checked when the backup is recorded
rather than when it is needed.

**`FR-OPS-14` — expiry and removal are two steps.** Deleting a backup does not release its
files. The failure that prevents: a backup deleted by mistake, the files swept before anybody
notices, and no way back even if the manifest is recovered five minutes later. Seven days of
grace, which is the span over which this kind of mistake is actually caught. `FR-STORE-21`
makes the same trade for compaction — only add files, remove them later — for the same
reason.

**`FR-OPS-15` — the drill reads the data back.** *"An untested backup is a rumour."*

A file-presence check passes on a truncated Parquet, on a file whose bytes were replaced with
another table's, and on essentially every failure that actually happens — because what goes
wrong with a backup is almost never that a file is missing. A missing file is loud. What goes
wrong is that a file is there and wrong. So the drill recomputes the digest, which is
expensive and is the only version of this that means anything.

Both the backup and the drill compute that digest through **the same code**. Two
implementations would drift on a null convention or a value rendering, every drill would fail
on data that is fine, and after the third false alarm nobody would run drills.

**The evidence records failures, or it is marketing.** A drill history with no failures in
three years describes either a very good system or a drill that does not really run, and
nothing in the history says which. Append-only, failures written with the same ceremony as
passes, and "could not start" recorded distinctly from "ran and passed" — the same
distinction the diagnostic draws, for the same reason.

`sankhya-server backup` and `sankhya-server drill`, exiting `0` proven, `1` a table did not
verify, `2` could not run. Demonstrated end to end against a real warehouse: take a backup,
corrupt a file, watch the drill catch it and both outcomes land in the record.

**The check with a property no other has.** The diagnostic now reports how long the backup has
been unproven — and it is the only check in that crate that gives a **firm** date on a first
run. Everything else needs two samples because a value alone implies no rate. Staleness rises
at exactly one second per second and always has, so it needs no observing. The natural
instinct is to feed it through the same `Trend` machinery as everything else, which would
collect a week of samples to estimate a rate that is already known exactly, and report
`TooFewObservations` in the meantime about the one thing that needs none.

**Two catalogue entries were wrong in ways only running them showed.** Refactoring the log
replay so that time travel and replay-to-the-end share their ordering rules moved two
existing mutations, and `check-mutations` failed the build rather than letting them pass
silently. And a new entry named the crate the *code* lives in rather than the crate whose
tests notice — it reported SURVIVED while the defect was caught, which is precisely how you
learn to read survivors as noise.

---

### §10.2 — Observability and the error catalogue

Two catalogues, both **generated into documentation from the declarations themselves**, and a
check that fails the build when the document and the source disagree. `M6`'s sixth exit
criterion asks for exactly that — *"generated from the same source as the catalog"* — and the
clause matters more than it reads: a hand-written table of error codes is correct on the day
it is written and wrong by the second release, with nothing to say which entry went stale.

**The metric catalogue is the API, not documentation of it.** Recording takes a
`&'static Metric` from the catalogue, so there is no `counter("some_name")` and an undeclared
metric is not refused at runtime --- it cannot be typed. Every exported series therefore has
a documented meaning, a unit, a group and a bound on its cardinality, because those are
fields on the thing you had to pass.

**The tenant-data prohibition is structural.** `ARCHITECTURE` §17.1 says no metric label may
contain tenant data. A label declares either a closed set of permitted values --- anything
else is refused --- or a deployment-scoped identifier under a cap. There is deliberately no
third variant, so a label that varies per row has no way to be declared. Past the cap, new
series are refused **and counted**: the metric goes incomplete and says so, rather than
growing without bound or going quietly wrong.

**Two checks, not one.** Generating a document from a catalogue proves the document matches
the catalogue and says nothing about whether the catalogue matches reality. So
`check-catalogues` separately requires every declared metric to be recorded somewhere in the
source. `ARCHITECTURE` §17.1 names four metrics that page; only compaction debt is declared,
because the other three measure machinery that does not run in this process and three gauges
permanently reading zero are indistinguishable from three healthy subsystems. The gap is
published in `METRICS.md` rather than filled.

**Runbooks are enforced, not aspirational.** A pageable metric's `runbook` field is not an
`Option`, and the check requires the file to exist *and* to carry its Symptom / What is
actually wrong / What to do sections. Seven exist. `M6`'s fifth exit criterion holds rather
than being something to audit later.

**Five defects, four of them in the path a user actually takes:**

- **Errors reaching clients carried no code and no remediation.** The catalogue had existed
  since M0 and the wire path did not go through it: a failed query returned the engine's own
  message with a SQLSTATE guessed from substrings. The errors a person actually meets were
  precisely the ones with nothing to look up. Exit criterion 6 was false.
- **`CREATE TABLE` succeeded and did nothing durable.** DataFusion will run DDL against its
  own in-memory catalogue, so the statement returned a success tag, the table existed for the
  rest of that connection, and it was gone on reconnect. Not an error, not a wrong number ---
  *a confirmation of something that did not occur*, which is the worst shape available. Fixed
  by planning and executing in two steps, because `SessionContext::sql` runs DDL during
  planning and a check on the returned plan is already too late.
- **The commonest error of all was misclassified.** DataFusion 55 wraps plan errors in
  `Diagnostic` to attach a source span, so matching on `Plan` never fired and "table not
  found" fell through to the catch-all.
- **`Box::leak` on every scrape** --- a few hundred bytes every fifteen seconds, forever, in
  the component whose job is to report that kind of thing.
- **A prefix-matched route** served `/metrics/../etc/passwd`. Harmless against an endpoint
  that reads no files, and exactly the shape that becomes a traversal when one does.

And two tests that did not test what they claimed: a cardinality-budget test using a metric
with no labels, and a refusal-classification test reaching only one of the three states the
table covers. Both were found by the mutation catalogue, not by reading.

**The tenant-data prohibition is now checked rather than asserted.** `ARCHITECTURE` §17.1
forbids caller data in any log line; metric labels were already structural and logs had only
the sentence. `cargo xtask check-logging` closes it — and it is aimed at the case nobody
writes deliberately: **`#[instrument]` records every argument of the function it decorates**,
so three words on `fn query(&self, sql: &str)` put every statement any client sends into the
log, predicate values included, with nothing at the call site saying so. There is no
suppression comment, because a prohibition with an escape hatch becomes a prohibition with
escapes in it.

Its own mutations then showed the word-boundary logic was **entirely unexercised**: every
test was rejected by plain substring absence, so neither half of the boundary was ever
reached. `%sql` is a real substring of `%sqlx`, and the leading and trailing checks reject
different things — one constructed case each. Third time this milestone a survivor exposed a
test passing for a reason unrelated to its name, which is the specific value of mutation
testing over coverage: both lines were covered throughout.

**What is not built:** distributed tracing spans. `FR-OPS-16`'s remaining checks --- conformance, replica identity,
archival consistency --- belong to §10.1 and are not built either.

---

### §10.1 — The diagnostic

`sankhya-server doctor`. Walks the warehouse, records what it sees, and reports findings
ordered by *when* rather than by how bad.

`FR-OPS-17` is the requirement, and it has a consequence it does not state:

> The diagnostic SHALL report **time until a problem becomes user-visible**, not merely its
> current value.

**A time cannot be computed from one sample.** It needs a rate, a rate needs observations
over time, and observations over time need somewhere to keep them between runs. That second
half is easy to miss — the projection arithmetic looks like the hard part and is not. A
diagnostic with the arithmetic and no history satisfies the requirement on paper and never
once in practice, because every run is the first run.

So the crate has two halves. `projection.rs` turns a series into a date or a named refusal;
`history.rs` is an append-only text file beside the warehouse that makes a second run
possible. It is deliberately not a table in the system being diagnosed: a diagnostic that
cannot run when the database is unhealthy is a diagnostic that cannot run on the day it is
needed.

**What it refuses to do, and why each refusal is its own outcome:**

| Refusal | The failure it prevents |
|---|---|
| Fewer than two observations | A date invented from one sample is a number with a calendar entry attached |
| A poor linear fit | A sawtooth — debt accumulating and being compacted away — fits a line badly *by construction*, and a date through one reports where in the cycle the samples fell |
| Beyond the horizon | Four days of samples projecting six months out is arithmetic, not evidence. The horizon is three times the observed span |
| No elapsed time | Every observation shares an instant |

A measure *near* the threshold with no rate yet still speaks up, undated, at `note`
severity. Silence at 990 of 1,000 files reads as health, and it is not.

**Four defects found while building it, three by running it rather than by testing it:**

- **The direction of concern was read from the slope.** It cannot be: a measure falling
  while the threshold sits above it is receding, and the slope-based reading called it
  already-crossed. Which direction is trouble is the caller's fact, not the data's.
- **Linearity was asked after direction.** A sawtooth averages to roughly no slope, so the
  direction test reached first and answered *receding* — an affirmative all-clear drawn from
  data that supports no conclusion at all. The order is now load-bearing and is tested.
- **`linear_fit` called a constant series a bad fit.** `r²` is 0/0 there, and the code
  returned zero on the reasoning that the fit explains none of the variance. Arithmetically
  defensible; it reads as "these points are not described by a line" about the straightest
  series there is. A horizontal line through a horizontal series is a perfect fit, so it now
  returns one. This was a real defect in `sankhya-math`, found by its first caller who cared.
- **"1 days".** A `contains` assertion hid it — `"about 1 days"` contains `"about 1 day"`.
  Found by reading the output, and the test now pins the whole phrase.

**And two catalogue entries that could never have failed.** One mutation was anchored on a
guard whose text appears twice, so it patched the harmless copy in `line()` and reported
SURVIVED; another claimed `{}` rounds `f64` where `{:?}` does not, which is false — both
round-trip, and the real reason to prefer `{:?}` is that `1e-300` under `{}` is 302
characters. The first was re-anchored; the second was deleted, along with the code comment
making the same false claim.

**What is checked today:** compaction debt, end to end. Storage headroom and replication lag
exist as checks with nothing feeding them observations — free space needs a platform call
`forbid(unsafe_code)` will not permit, so the caller that has one passes the number in, and
nothing in this process advances a replication position. The rest of `FR-OPS-16` —
conformance, replica identity, archival consistency — is not built.

Exit status is `0` clean, `1` findings, `2` a check could not run. The third exists because
a monitoring system treating "I could not look" as "nothing found" is the failure this whole
crate is arranged against.

---

## M5, closed

Closed 2026-08-27. Tenancy is enforcement rather than retrofit --- the tenant parameter has
been on every interface since M0, which is why this was twenty-odd weeks rather than sixty.

| Exit criterion | State |
|---|---|
| Mainstream client tooling connects and works, verified by a compatibility matrix | **Met, for one surface of four.** Real `psql`, `pg_isready` and `pg_dump` binaries driven against the release server: `cargo test -p sankhya-server --test compatibility -- --ignored`. `pg_dump` is recorded as **failing** rather than omitted, because somebody will try it |
| Isolation provably enforced across every surface, including the graph tier | **Met.** All five surfaces the demonstration names, in one test with one pair of tenants — SQL, the columnar prefix, a graph traversal, a cached result, and the error message |
| Mutation score on the policy component above its threshold | **Met.** 11 of 11 mutations caught, including removing the tenant comparison, letting grants outvote a denial, and treating absent grants as permission |
| Audit records reproduce exactly what a principal saw, including data versions | **Met.** The row filter and column masks that were applied, plus the table snapshot and graph epoch that answered |
| Compatibility matrix between client and server versions tested, not asserted | **Not met, and carried forward.** One server version exists, so there is no matrix to test. This is honestly untestable rather than skipped, and it becomes real when a second version ships |

### What is built and what is not

Two of four API surfaces are built, and the other two are **deferred rather than missing**.

The wire protocol came first deliberately --- `FR-API-02` calls it the highest-adoption-value
surface, and it is the one that makes every other capability reachable by a person rather
than by a test. **Flight SQL** followed, because `FR-API-01` names it the *primary bulk data
plane*: the wire protocol is a row protocol, so the last step of every query converts
columnar batches into rows, and that conversion is the whole cost of a large extract.

The **gRPC control plane and REST gateway are deferred to M6** by owner decision.
`FR-API-04` says what a control plane exposes --- jobs, health, archive operations --- and
each of those is built in M6 or M9. Building the surface first would mean endpoints for jobs
no scheduler runs and archives that do not exist: a plausible-looking API returning a
placeholder, which is the kind of thing that gets believed.

### Two things worth recording

**The type-level guarantee is a `Guard` that cannot be constructed except from an allowed
decision.** No public constructor, no public fields, no `Default`. Anything requiring one in
its signature cannot be called without a decision having been made --- and the failure that
guards against is not a wrong policy but a code path that never consulted one.

**The enforcement does not trust the provider.** The first implementation handed the policy
predicate to the underlying provider as a pushdown filter, and `MemTable` **declines**
filters --- so every row came back and the table was secured in name only. No error. The
predicate is now offered *and*, unless the provider promises exactness, enforced above the
scan where nothing can decline it.

### Three defects the tests found

| What | How it was found |
|---|---|
| **The secured table was secured in name only.** The policy predicate was offered to the provider and the provider declined it, so every row came back with no error raised anywhere | The first run of the test written to check it. Correctness now never depends on the provider cooperating |
| **A mutation survived twice before the test was honest.** `push the limit below the security filter` kept passing, because the test ran against `MemTable`, which ignores limits too. A test whose subject ignores the thing under test proves nothing | The mutation audit, twice. It now runs against a provider that honours a limit, with the forbidden rows ordered first so the cut bites |
| **Removing the audit chain's previous-digest check left every test green.** Every test that broke a link also broke the sequence number, which fires first — so the link check was never the thing catching anything. A competent attacker renumbers after a deletion | The mutation audit. That test now exists, along with one for a spliced record |

---

## M4, closed

Closed 2026-08-27. The graph engine and the extension mechanism, built together because
the graph algorithms are the most demanding consumer of the extension API --- building them
apart would have proved the API against nothing.

| Exit criterion | State |
|---|---|
| Graph performance objectives met against a named public suite | **Not met, and not claimed.** No public graph suite is wired into the pipeline. The primitives are correct against brute force and bounded by construction; they are not yet *measured* at scale. This is the one M4 criterion carried forward, and it is carried as unmet rather than reinterpreted |
| The incremental-equals-full property test green | **Met.** Two property tests, 400 cases. Verified to fail: making the overlay ignore its pending batches turns three tests red |
| Memory per vertex and per edge published | **Met.** `Epoch::footprint()` measures a real epoch rather than estimating from type sizes, because the interner's key storage dominates and depends entirely on key length |
| Time-respecting traversal returns no time-violating path | **Met.** `a_static_path_that_time_forbids_is_not_returned` builds the smallest case: two edges whose order forbids the path they appear to form |
| Each reference pack's change touches zero core files | **Met, mechanically.** `check-layers` refuses any pack dependency outside `sankhya-ext`, `sankhya-types`, `sankhya-error`, and refuses any core dependency on a pack |
| The adversarial pack's every attempt rejected with a named error | **Met.** Seven attempts, seven named refusals |
| The extension API within its size budget, with no escape-hatch types | **Met.** Nothing in `sankhya-ext` re-exports an engine type. `Value` and `LogicalType` are SANKHYA's own |
| Cancellation demonstrated within its bound inside pack code | **Met, with a stated cost.** See below |

### Cancellation, and what it actually costs

A pack function in a deliberate infinite loop that ignores the cancellation flag *is*
stopped, and the query fails with an error naming the pack rather than hanging. The
mechanism is worth stating plainly because it is not free.

The call runs on its own thread and is **abandoned** when the bound passes. Rust has no
safe way to kill a thread and this repository has no unsafe code, so a genuinely
non-terminating function leaks one thread until the process ends. `Sandbox::abandoned()`
counts them, so a pack that does this is visible rather than suspected.

The alternatives are worse: hanging the query forever, or killing a thread mid-allocation
and corrupting the allocator for everything else. A bounded, observable, attributable leak
is an operational problem with an obvious fix. The other two are outages.

The declarative tier does not need any of this. Its expression language has no loop, no
recursion, no call and no I/O, so a declarative function *cannot* be the one that hangs a
query. It is safe by construction rather than by supervision, and that restriction is the
point --- an expression language with loops is a programming language, and one loaded from
a configuration file is a remote code execution feature with extra steps.

### On signing

`FR` asks for pack bundles to be signed and verified. What ships is **digest pinning**: an
operator pins the digests of bundles they have reviewed and anything else is refused,
including everything when nothing is pinned, because a trust policy that defaults to
trusting is not a policy.

That is a real control and it is deliberately not called a signature. A digest proves the
bytes are the bytes you pinned; it proves nothing about who wrote them. Public-key signing
is the better answer and needs a cryptographic dependency, which this repository adds by
decision record rather than alongside a feature. `Verifier` is a trait, so adding it later
changes no caller.

### Three defects the tests found

| What | How it was found |
|---|---|
| **The negative-weight guard fired by luck.** Shortest-path checked each edge as it relaxed it, and Dijkstra settled the destination by a cheap direct edge before ever examining the negative one — so the same graph passed or failed depending on the query | Writing the test for it. The check is now a property of the structure, recorded at build time, and the refusal is its own error type so "cannot answer for this graph" cannot be read as "no route exists" |
| **Community detection collapsed two clusters into one.** Label propagation's monster-community failure, on the smallest interesting case: two triangles joined by one edge. Determinism, which reproducibility demands, made it *worse* — the randomised form at least sometimes stalls first | The test that specified the behaviour. Replaced with modularity optimisation, whose objective falls when dense clusters merge across a thin bridge, so the search resists the collapse rather than needing to stop in time |
| **A whole source file was never compiled.** It was not declared in `lib.rs`, so it contributed nothing and was checked by nothing — and it contained a match arm naming a struct field that does not exist | Wiring the module in. "It compiles" means nothing about a file the compiler never saw |

---

## M3, closed

Closed 2026-08-26. All six exit criteria are met, one of them after the criterion itself
was corrected and one after the objectives were measured against the workloads they
describe. Both corrections are recorded below rather than folded into a pass.

| Gate | State |
|---|---|
| Performance objectives met in the pipeline, against named public-suite queries | **Met.** `NFR-PERF-02` 13 ms, `NFR-PERF-03` 796 ms, `NFR-PERF-04` 648 ms, against budgets of 250 ms, 1 s and 3 s — asserted by `cargo xtask check-performance`, which fails the build. See the correction below: this gate was previously read as failing, against queries the objectives do not describe |
| Cancellation demonstrated within its bound, at the points M3 controls | **Met.** Bounded at one batch per partition inside a real query, across threads, and under periodic checking within its interval. The clause "including inside user code" has moved to M4 — see below |
| A hostile aggregation under a constrained memory limit is rejected rather than terminating the process | **Met.** An aggregation that cannot reduce anything is refused under a one-megabyte pool, by name — and the process runs the same query to completion afterwards. Repeated five times, so a refusal that leaked its reservation would show up |
| Plan snapshots stable; the SQL-semantics corpus green | **Met.** Thirty-nine semantics cases pinned by hand from the standard's rules; plan *shapes* pinned rather than plan text, plus assertions on the optimisations that fail silently |
| Cross-engine semantic differences enumerated in a tested list | **Met, and it found three ways the analytical tier returns a wrong number.** Fifteen cases run against both engines; agreements are pinned too, so a *new* divergence fails the test |
| Compaction holds file counts within policy under continuous ingest | **Met.** Forty ticks of capture with maintenance sweeping every third; the live count never passes the urgent threshold and no row is lost |

### The exit criterion that could not be met by M3

Criterion 2 read *"including inside user code"*. There is no user code until M4 builds the
extension mechanism, so as written M3 was gated on a later milestone and could not close
on its own terms. The clause has moved to M4's exit criteria, where the mechanism it
tests is built.

This is a correction to the plan, not a waiver. Nothing that was going to be tested is
now untested; the same test is asked one milestone later, of the milestone that can
answer it. The related case — cancelling a query while it *queues* for memory — is also
not claimed here, because admission never blocks: it returns a decision and the caller
waits, and that caller is M5's server.

### The objectives were being read against the wrong queries

The performance gate was recorded as failing, on Q6 at 386 ms against a 250 ms objective
and Q5 at 1149 ms against 1 s. Both numbers are real and both still stand. What was wrong
was the mapping.

`NFR-PERF-02` says **selective needle lookup**. Q6 applies three range predicates and
returns about a hundred thousand rows of six million, touched across every file in the
table — a different mechanism entirely, answered by scanning fast rather than by not
scanning. Measured against an actual point lookup, the objective is met at **13 ms
against 250 ms**, and met by statistics pruning alone, without the bloom filters its own
precondition names.

`NFR-PERF-03` says **multi-dimensional pivot, warm, pruned**, with a stated precondition
that a partition predicate be present. Q3 is that shape and meets it at 796 ms. Q5 is a
six-way join with no partition predicate — and since partitioning is not built, no query
can satisfy that precondition today. So Q5 is measured and published, and deliberately
**not** gated, rather than a passing test being written around it.

The honest form of the earlier statement was never "the system is too slow"; it was
"these objectives have never been tested". They are tested now. The mapping was mine, it
was approximate, and it was described as approximate at the time — but an approximate
mapping used as a gate is a gate on the wrong thing, in whichever direction it errs.

**A measurement that could not have failed.** The first version of the gate inherited
`#[tokio::test]`'s default current-thread runtime, so its eight concurrent clients took
turns on one thread and the pivot reported 3029 ms — three times its budget, describing
nothing the requirement is about. The gate now asserts it is on a multi-threaded runtime
before it measures anything.

### What M3 did not build

Closed does not mean complete. These are inside the M3 work breakdown, are not built, and
are not exit criteria: delete resolution into plan-time row selections, file ordering by
statistics for early termination, bloom filters, per-column encoding chosen from measured
statistics, quantile sketches, the footer and byte-range and decoded-batch caches, table
partitioning, and leader election.

The *result* cache does not exist either — but its **key** does, because what makes a
result-cache key correct is a security property and the right time to fix it is before
anything is caching. A key that omits the entitlement set does not return a stale answer;
it returns someone else's, correctly and quickly.

Nothing above is load-bearing for M4, which is why M3 closes with them open rather than
holding the milestone until a work breakdown is exhausted.

---

## The six-way join, explained

Q5 was recorded for several weeks as **39 % behind the engine's own listing table over
the same files, cause unknown**. An unexplained gap against a baseline reading identical
data is the kind of finding that is usually a wrong answer waiting to be noticed, so it
blocked the milestone rather than being filed as a performance nit.

It was two defects, both found by printing the two physical plans side by side instead of
reasoning about them.

**The scan reported no size.** `TableProvider::statistics` describes the whole table and
is read during logical planning. Join selection runs later, on the physical plan, and
reads the statistics of the scan node — which come from the file-scan configuration and
default to unknown. Every small table therefore looked *unmeasurable* at the exact moment
the engine chose how to join it, so it repartitioned tables it could have broadcast: five
rows of `region` shuffled across every core. Three of the five joins were the wrong mode.
A table reporting no size is also sorted against tables that do, so one absent figure
moved every join in the query, not just its own.

**The provider was grouping files by hand.** It dealt files round-robin across the target
partition count. The engine splits file groups by byte range, which balances on size and
is strictly better than anything done by counting files — but only above a size threshold,
below which it leaves a single group alone, and a single group is a single partition. So
the division of labour now follows the engine's own threshold: above it, hand over one
group; below it, deal them out, because otherwise nobody will. Doing both was worse than
either, and measurably so — the engine was rebalancing an arrangement already unbalanced
by file count, and the join paid about seven percent for it.

Together: **578 ms to 450 ms, against the listing table's 447 ms.** Parity, and the plans
are now identical operator for operator.

**The guard that did not exist.** The round-robin dealing had itself been added to fix a
real defect — the scan ran on one core — and no test was ever written for it. It was
caught only because a *deadline* test contained a self-check asserting its own plan ran in
parallel. Removing the dealing broke that self-check, which is the only reason the
regression was not shipped. There is now a direct test that a scan of thirty-two files
runs on more than one partition, asserted at the scan itself rather than above it, because
a repartition can manufacture eight partitions from one serial reader — which is exactly
the shape the original defect had.

---

## TPC-H, at scale factor 1

Every other measurement in this document isolates one mechanism. These are queries
somebody else wrote, so the numbers can be compared to something other than themselves.

Data generated at scale factor 1 — 8,661,245 rows, 261 MiB of Parquet — written through
this system's own write path. Five rounds at the stated concurrency, after two warm-up
executions, on a multi-threaded runtime — asserted, because on a current-thread one the
clients take turns and the figure describes nothing.

| | Clients | Median | p95 | Objective | |
|---|---|---|---|---|---|
| **needle lookup** — one row of six million by key | 8 | 11 ms | **13 ms** | `NFR-PERF-02` selective needle lookup, < 250 ms | **met** |
| **Q3** shipping priority — three-way join, top-N | 8 | 777 ms | **796 ms** | `NFR-PERF-03` pivot, < 1 s | **met** |
| **Q1** pricing summary — full scan, eight aggregates | 4 | 562 ms | **648 ms** | `NFR-PERF-04` wide scan, < 3 s | **met** |
| **Q6** forecasting revenue — narrow range filter | 8 | 386 ms | 491 ms | *not the workload any objective describes* | published, not gated |
| **Q5** local supplier volume — six-way join | 8 | 1149 ms | 1202 ms | `NFR-PERF-03`, whose partition-predicate precondition it cannot satisfy | published, not gated |

The first three are the gate, and `cargo xtask check-performance` fails the build if any
of them regresses. The last two are published because they are the queries a reader will
recognise, and withholding them because they are unflattering would be the worse choice.

**Q5 and Q6 are not failures against these objectives; they are not measured by them.**
That distinction is argued in full under *M3, closed* above, including why the previous
mapping was wrong in both directions. The short form: `NFR-PERF-02` describes finding one
row, and Q6 returns a hundred thousand; `NFR-PERF-03` requires a partition predicate, and
partitioning is not built, so Q5 could not satisfy it however fast it ran.

**These are met on hardware below the reference node.** The requirements name 32 physical
cores and 357 GB; this is twelve cores and 62 GB. That makes the results conservative
rather than qualified — but the reference node has never been measured on, so the numbers
that would be published with it do not exist.

**The first run of this measured the wrong thing**, and finding out why produced the
correction below. It used a bare engine rather than this system's configured session, so
the numbers described the engine's defaults. Running it properly made three of four
queries *slower* — Q6 by 2.6× — which is how a setting that had been asserted as required
since M3 began turned out to be a cost.

**What this is not:** an audited TPC-H result. Scale factor 1 on a development machine,
single node, four of twenty-two queries, no substitution rules, no refresh streams. Using
the name for anything more would be a misuse of it.

---

## Mutable tables, resolved

Capture records inserts, updates and deletes as rows. The read path used to union its
tiers and stop, so a row that had been updated came back **twice** — once as it was, once
as it is. `COUNT(*)` said two; `SUM` added the old value to the new one. Nothing in the
result indicated it.

It is resolved now, by declared capability rather than by heuristic:

- **Append-only** — union, as before. No deduplication, no sort, no key comparison. Most
  high-volume tables are append-only, so most queries take this path and it must cost
  nothing at all, not "a cheap check".
- **Mutable** — the latest version of each key wins, and a key whose latest version is a
  deletion is absent.

Expressed as a logical plan the optimizer can see through, rather than as an opaque
physical operator that would have to re-implement every optimisation inside itself.

**The capability comes from the source, not from a guess.** The relation states which
columns identify a row — its replica identity — and that statement arrives with the
relation description. Reading it is taking the declaration, not inferring one. A relation
declaring no identity is append-only *for reading*, and that is a statement about what can
be done rather than a prediction: with no key there is nothing to resolve versions
against, so an update could not be applied even if one arrived. The fix for that is the
source's replica identity, which onboarding already warns about.

Three things this gets right that a first attempt would not:

**The deletion filter runs after the resolution, not before.** Filtering tombstones first
removes the deletion and lets the *previous* version win — so a deleted row returns
holding the values it had before it was deleted. That is worse than the duplication it
replaces, because it looks like data.

**A key must be declared and must exist.** No key means no latest-version-per-key to
resolve to, and defaulting to whole-row identity turns every update into a new row — the
original defect, reached by a different route. A key naming a column that is not there is
refused for the same reason.

**Scanning a resolved table directly is refused.** The planner inlines the resolution and
never calls `scan`, so nothing exercises that path — which is why it is worth a test. A
fallback that quietly scanned the raw table would serve every version again, and the only
symptom would be wrong numbers.

**Why the suite never caught the original defect:** every test that read captured data used
append-only fixtures. The read path had never been asked a question about a row that
changed.

---

## What this system's own storage costs

TPC-H now runs through SANKHYA's provider rather than the engine's file listing — every
table carrying a commit position, committed to a log, resolved and pruned through the
catalogue. That makes the numbers a measurement of this system rather than of the engine
underneath it, and it made two costs visible.

| Single query, SF=1 | Engine's file listing | SANKHYA's provider |
|---|---|---|
| Q1 scan + aggregates | 450 ms | 462 ms |
| Q6 selective filter | 213 ms | 219 ms |
| Q3 three-way join | 357 ms | 372 ms |
| Q5 six-way join | 415 ms | 578 ms |

**Parity on scans, 12% and 39% behind on joins.** Four causes were found in the end. Two
are described here; the other two — the scan reporting no size to join selection, and the
provider grouping files by hand — are under *The six-way join, explained* above, and
closing them brought Q5 to 450 ms against the listing table's 447 ms. The table above is
kept at its original numbers because the sections below explain what each fix was worth,
and rewriting the starting point would erase that.

**The scan used one core.** The provider put every file into a single file group, which is
a single partition — everything above it could only round-robin batches that had been read
serially. Invisible in a result: the answers were identical and the query used one
twenty-fourth of the machine.

**The read position was enforced when it could not matter.** Reading at a pinned position
costs a column read and a predicate *per table*, so it compounds with join arity. When no
tier holds anything past the target the filter provably removes nothing — and the column
it reads need not be scanned at all. Most queries read the latest data, so most queries
were paying for a facility they were not using.

The commit position itself turned out **not** to be a cost: it is monotone, so it
delta-encodes to +0.1% on a 174 MiB table. That was the first hypothesis and it was wrong.

**What remained was eventually explained**, after some weeks of being recorded here as
unexplained — which was the right way to hold it. The answer is above; it was two further
defects, and neither was the one that would have been guessed. Both were found by printing
the two physical plans next to each other rather than by reasoning about them, which is
the general lesson worth keeping.

---

## Clustering, worth 7.8× on a range scan

This was originally written up as "the fix that closes `NFR-PERF-02`", which was wrong
twice over — Q6 is not the workload that objective describes (see *M3, closed* above), and
the objective's own named preconditions are not the lever either. Bloom filters do not
apply to Q6, which has no equality predicate; late materialization costs rather than
saves. **Sorting does**, and that finding stands on its own without an objective attached
to it.

Q6 selects one year in seven of `l_shipdate`. Written in arrival order every row group
holds the whole date range, so the bounds exclude nothing and the query reads the entire
table. Written in date order most row groups fall outside the year and are skipped on
their statistics, before any decoding.

| Layout | Single query | p95 at 8 clients |
|---|---|---|
| Generation order | 222 ms | 1819 ms |
| Sorted by ship date | **31 ms** | **234 ms** |

**7.8× at eight clients.** Compaction now does this: a settled partition is written in the
order the policy declares. No objective is claimed from it — the number is the point.

Two details that are the design rather than the implementation. Only *settled* partitions
are sorted — ordering one that is still receiving writes means ordering it again
tomorrow, for a layout correct until the next append. And the key is **declared, not
inferred**: a key chosen from observed queries would change under a workload shift and
rewrite the whole table to follow it, which costs more than the ordering is worth.

Q5, the six-way join, is still over its objective. Clustering does not help a join, and
what would is not built.

---

## A required setting that was costing 2.6×

`datafusion.execution.parquet.pushdown_filters` — late materialization — was in the list
of settings asserted at startup. It is not any more, and the way it left the list is worth
recording.

It went in on reasoning: late materialization is widely described as the largest scan
optimization available, and it defaults to off, so leaving it off is a large win silently
forfeited. It was then measured at **1.02× — neutral** on a synthetic scan, and pinned
anyway on the grounds that neutral is not harmful.

Measured on TPC-H it is not neutral. It costs at every selectivity tried:

| Rows surviving the filter | Off | On | |
|---|---|---|---|
| 1 in ~6,000,000 | 5.6 ms | 5.5 ms | 1.02× |
| 1 in ~1,500 | 4.5 ms | 4.8 ms | 0.94× |
| 1 in ~60 | 4.0 ms | 4.6 ms | 0.87× |
| 1 in ~7 | 116.6 ms | 162.0 ms | **0.72×** |
| all rows | 110.6 ms | 111.7 ms | 0.99× |

With filter reordering compounding it, Q6 went from 357 ms to **917 ms** at eight clients.

**Why it does not help is the useful part.** Late materialization saves decoding payload
columns for rows a predicate eliminates — and on this data those rows were already
eliminated by row-group and page statistics, before decoding began. That is why the
selective queries above finish in four to six milliseconds. Pushdown cannot save work
that is not being done; it adds per-row bookkeeping to the scan that remains. The cheaper
mechanism had already won.

It is left at the engine's default rather than pinned off, because the evidence supports
*not always* rather than *never* — on data with no useful bounds it should look different,
and the right form of that decision is per query from the statistics rather than pinned
for everyone.

**The pattern, which is the part worth keeping:** both errors came from asserting a
mechanism with a good reputation on reasoning rather than on a measurement of this
system's own data. The first measurement was too narrow to contradict the reasoning. The
second was a workload somebody else designed, and did.

---

## Where the two engines disagree

SANKHYA presents one copy of the data through two engines, and the same question asked of
each is supposed to get the same answer. Mostly it does — which is what makes the
exceptions dangerous, because nobody checks a figure that has agreed a thousand times.

Fifteen cases are now run against both, with the agreements pinned as well as the
differences. Three of the differences produce a **wrong number rather than an error**:

| | Transactional tier | Analytical tier |
|---|---|---|
| Summing past a 64-bit integer | `9223372036854775808` | **`-9223372036854775808`** |
| Multiplying past a 64-bit integer | refuses | **`0`** |
| Summing decimals past 38 digits | `100000000000000000000000000000000000000` | **`99999999999999997748809823456034029569`** |

The third is the one that matters most. Fixed-point decimal was chosen *because* money
must be exact, and on overflow the analytical tier returns something close to the right
answer instead of refusing. Close is the wrong kind of wrong.

Three further differences change a figure's precision or ordering without making it
wrong: `avg` of integers is arbitrary-precision on one side and a 64-bit float on the
other; division to a repeating fraction gives twenty significant digits against sixteen;
and text orders by the database's collation on one side and by bytes on the other, so any
paged or ranked result over text is in a different order in the two tiers.

**A mitigation exists and is partial.** The statistics catalogue can predict, before a
query runs, whether summing a column *can* overflow — bounds and row count give an upper
bound on the total. It errs toward "might", because a false alarm costs a refused query
and a missed one costs a wrong number nobody notices.

Its limit is worth stating plainly: bounds are held as 64-bit integers, so a decimal
column exceeding about nineteen digits has no representable bound and the check answers
"unknown". The columns most likely to overflow a 38-digit decimal are exactly the ones it
cannot reason about. Widening the bound type to 128 bits would close that, and has not
been done. Nothing yet consults the check.

---

## What runs today

| Capability | Evidence |
|---|---|
| The optimizer plans on the catalogue rather than a guess | Bounds, null counts and cardinality reach it from SANKHYA's own statistics — cardinality marked *inexact*, because an optimizer told a count is exact may conclude a column is unique, and being wrong about that is a different plan rather than a slower one |
| A row that has been updated is returned once, with its new value | By declared capability: union for append-only, latest-version-per-key for mutable, with a deletion suppressing the row rather than reverting it |
| A settled partition is written in the order it declares | Compaction sorts, which is what turns row-group bounds into an index — **7.8×** on the query whose objective was being missed, and the only one of that objective's three named preconditions that turned out to matter |
| A required setting is required because it was measured, not because it sounds right | Filter pushdown was asserted at startup for five milestones and is not any more — measured a cost at every selectivity, up to 2.6× on a full query |
| The pinned dependency set compiles with no critical duplicates | `cargo xtask check-dupes`, [ADR-0001](adr/0001-dependency-pin-set.md) |
| PostgreSQL 17.11 builds from vendored, checksum-verified source | `vendor/postgresql/build.sh` |
| The wire decoder handles a real replication stream | Conformance suite against captured bytes |
| The decoder never panics on arbitrary or corrupted input | Property tests plus single-byte corruption of real messages |
| A transaction is never split across batches | Property test over randomised interleavings |
| Types round-trip exactly or are refused with a reason | 73 real columns across 10 tables |
| Naming mirrors the source; collisions are refused | All 10 tables map with no transformation |
| A query is answered from tiers covering its span exactly once | Property test asserting exact cover |
| Tables onboard from the stream alone | All 10 tables, schema derived not declared |
| Captured data becomes queryable Parquet | Exact decimal sum matches the source |
| Several tables capture independently from one interleaved stream | 4 tables, each reconciling against the source |
| Capture holds up at scale | 1,000,000 rows across all 10 tables at ~285k rows/s, every table reconciling |
| A session sees its own write analytically | Wrote, capture caught up in 6 ms, the query returned the row |
| Capture cannot endanger its own source | Five-rung ladder escalating strictly below the database's own limit, validated against a real slot |
| Captured data provably matches the source | 3,000 rows digested independently on both sides, no discrepancies |
| A restart cannot duplicate data | Replay is filtered per row; every crash point across a constructed stream yields each row exactly once |
| A schema change never corrupts data | Additive changes apply automatically; anything whose intent cannot be inferred quarantines, keeps consuming so the cursor advances, and requires an operator to adopt the new shape |
| Backfill meets streaming with no gap and no overlap | Verified against a live slot; a late slot is shown to drop real positions into neither half |
| Small-file accumulation is detected and planned against | Two independent triggers, bounded passes, coverage preserved exactly, and a plan that always reduces the file count |
| Compaction runs without changing any answer | The same aggregate over 12 fragments and over the file they merge into, compared row for row; row counts verified against the inputs before the merge is reported as successful |
| Compaction cannot remove a file a reader might still be holding | Retirement refuses inside the grace period, refuses while a snapshot is pinned at or before the merged coverage, and refuses entirely if the replacement is missing or short |
| A write is visible before it is durable | The arrival tier answers from memory and drops out of the splice once published; the two tiers are shown to abut exactly with no change to the planner |
| The arrival tier never loses or double-counts a position | Property tests over arbitrary interleavings of appends and publication: every position between the durable frontier and the target appears exactly once, and a scan never returns a position the published tier already holds |
| Memory pressure cannot cost data | Nothing is released until publication covers it; a full tier refuses new work and names publication as the cause, and the refusal clears once publication catches up |
| One SQL query is answered from memory and Parquet at once | 700 positions published and 300 still in memory sum to the whole thousand, with the 700-position overlap counted once; a pinned query sees only its target; provenance names both tiers and their intervals |
| A gap between tiers refuses the query rather than answering it short | Verified for a genuine gap, for a target past every tier, and for a tier that started mid-stream |
| What the engine means by a query is written down | Thirty-nine cases across three-valued logic, aggregates, grouping, joins, ordering and coercion. Every expected value written by hand — a corpus that computes its expectation the way the engine does is a tautology |
| An optimisation that stops working is a failing test | Plans are pinned by operator shape, not text: a byte-exact snapshot fails on cosmetic upstream change and gets regenerated unread, at which point it catches nothing |
| Memory is counted where it is actually spent | A counting allocator, quarantined in the one crate where `unsafe` is permitted — the workspace `forbid`s it everywhere else. Growth, release, reallocation-by-difference, the peak, and concurrent allocation are all verified against a real installed allocator |
| Capture and maintenance commit to the same log without either stopping the other | Capture rebases onto the next free version when a compaction takes the one it was about to use — bounded, so a runaway committer is a diagnosable failure rather than a pipeline that appears to hang |
| A query stops within one batch of its deadline | Demonstrated on a real query planned and executed by the engine over real Parquet — it returns an error rather than fewer rows, and the clock is shown to have been read no more times than the deadline allows |
| A deadline and a cancellation are told apart | Different variants with a `retryable` flag: a deadline may succeed with longer to run, a cancellation is a decision somebody made. A client that cannot tell them apart cannot decide whether to retry |
| A query the system cannot afford never starts | Admission estimates from plan cardinality and queues or refuses; the queue is bounded, so a refusal arrives immediately rather than after a timeout |
| A client can tell a permanent refusal from a temporary one | "Too large for the pool" and "too large right now" are distinct answers with a `retryable` flag — collapsing them is how a client retries forever |
| One tenant cannot starve another, or take an idle pool | A floor is honoured under global pressure; a cap binds even when nothing else is running |
| Pressure escalates through one ladder, evaluated centrally | Five rungs from a typed signal bus; the last one — the only one that loses continuity — is reachable by the two source signals and nothing else, property-tested |
| Maintenance work is arbitrated against one budget across both sides | Strict class ladder; safety and availability work preempts queries and ignores the duty cycle, everything below it does not; a job that cannot checkpoint is refused rather than started; waiting never promotes a job out of its class |
| The maintenance scheduler cannot destroy retained history | There is no erasure class to configure — the guard is on the type, and an exhaustive match makes adding a variant a compile error |
| A fragmented warehouse is cleared by ticking, and converges | 60 fragments reduced by repeated ticks until nothing is left to do, with the row count unchanged; urgency maps onto the class ladder, so a degrading partition preempts and an ordinary one waits; one failing partition does not block the rest |
| A query is unaffected by compaction running underneath it | The same aggregate before and after a merge, over the live set |
| The live set is durable and readable by other engines | SANKHYA writes the Delta transaction log itself, and `delta_kernel` — a dev-dependency used as an independent oracle — reads it and agrees about the schema, version and live files, including across a compaction where four superseded fragments are still on disk |
| Capture publishes into a table log | Each table gets its own log; every file is committed with its row count, the creating commit carries the schema, and a translation that is not exact refuses to publish rather than publishing something similar |
| A restart resumes from the log, not from memory | File sequence and log version are both recovered from the table's whole commit history — not from the live set, so a compacted-away name is never reused while a reader may still resolve it |
| The whole storage loop runs through the log | 24 fragments published and committed, compacted tick by tick with each tick one atomic version, then queried by a reader given nothing but the table root — same answer, fewer live files, every superseded file still on disk |
| SANKHYA owns its table provider | Files and row counts come from the table log; scan execution is DataFusion's own Parquet source. Splices memory and files, refuses gaps at planning time, and refuses a file the log cannot state a row count for |
| Order statistics are exact and the convention is named | Three conventions, shown to disagree at the 99th percentile — which is the only place anyone asks for one. An unorderable value is refused rather than placed somewhere; an empty input is refused rather than answered with zero |
| The quantile of a sum is not computed as the sum of quantiles | Demonstrated with numbers: two exposures whose worst cases fall in different scenarios give −100 combined and −190 under the naive composition |
| A skipped file never hides a matching row | Property-tested over arbitrary values and predicates, and again over *merged* statistics — compaction merges rather than recomputes, so a merge that narrowed a bound would produce a defect appearing only after maintenance ran |
| Distinct-value counts are estimated well enough to order a join | Within 5% from 10 to 100,000 distinct values, exact under merge, and reproducible across processes — a per-process hash seed would make two nodes disagree about a plan and the disagreement would look like an optimizer bug |
| An approximate function cannot answer an exact question by accident | Rejected at planning time, including inside a subquery or a `HAVING` clause; a permissive session still gets a watermark naming what it used |
| A cache key cannot omit what the answer depended on | The entitlement set and the policy version are constructor arguments, not fields — a field can be left at its default, an argument has to be passed. Keys are byte-identical across processes, pinned so a change to the hash is deliberate |
| An interrupted compaction's leftovers are reclaimed, and nothing else is | Age is the only thing separating an orphan from a file mid-commit, so the threshold is a week by default; a file any retained snapshot reaches is kept however old, and the log is not a candidate at all |
| A cold reader starts from a checkpoint, and any reader may ignore one | **10×** at fifty thousand commits; the kernel reads a checkpoint this system wrote by hand, and is proven to *use* it rather than tolerate it — the commits it covers are deleted and the table still resolves |
| A warm process pays for what changed, not for the whole history | Table file sets are cached and resumed; the cache cannot go stale because it never trusts its own version, and asking costs one filesystem probe rather than a directory listing |
| Log replay scales linearly with a table's history | Guarded by measuring the *ratio* between two sizes rather than a clock, so it means the same on any machine — and proven to fail on the quadratic implementation it replaced |
| A file is prunable from the moment it is published | Capture computes statistics from the batch it just encoded — the same data, already in memory — so a file does not wait for maintenance to become skippable |
| Statistics survive a restart and other engines can read them | Bounds and null counts are written into the table log itself, so a fresh process prunes exactly as a warm one does — and the kernel reads a log carrying them |
| Compaction computes the statistics the provider prunes on | Bounds, null counts, widths and a cardinality sketch, produced by the merge that was already reading the data — no separate analysis pass and nothing for an operator to remember to run |
| The provider skips files the catalogue proves irrelevant | Nine of ten files pruned on a point lookup, five of ten on a range, and none at all on a disjunction, a predicate over an uncatalogued column, or no predicate. The same query returns the same answer with and without the catalogue |
| Planning does no file I/O | 800 files plan in 1.37 ms against 10.33 ms for a directory listing — **7.5×**, widening with file count |
| A dependency declared test-only actually is | `cargo xtask check-features` reads the manifests; proven to fail when the oracle is moved into `[dependencies]` |
| The tests guarding each core invariant are verified against the defect they claim to catch | `tools/mutation-audit.py` — 408 specific defects applied one at a time; all 357 fail the suite. Twenty-nine did not when first run; five catalogue entries turned out to be equivalent mutants no test could ever have caught, six entries were inert until corrected — two did not compile, and one was an equivalent mutant deleted rather than repaired, four more survived because the tests naming them exercised a different guard or lived in another crate, — one was anchored on a guard that appears twice so it patched the harmless copy, and one named the crate the *code* lives in rather than the crate whose tests notice — two revealed tests that did not test what their names claimed, and chasing two others produced documentation corrections rather than new tests. Three mutations exposed defects in *tests* rather than in code, and all three were the same defect: an unbounded wait, so that removing a deadline hung the build rather than failing it. The five-minute journey read the server's banner with no timeout; both drain tests awaited the server task with none. A hang is strictly worse than a failure — it takes the build with it and reports nothing — so every wait now goes through one bounded helper rather than a timeout somebody has to remember at each call site. The catalogue also checks that each entry still *matches* its source before applying it: a refactor moved four of them, and a mutation that no longer applies passes silently, which is the failure this tool exists to prevent |

---

## Mathematics, arrays and the date axis

Added after M5 closed, by owner directive, and recorded here because a capability nobody
documents is one nobody finds.

| | |
|---|---|
| **Array columns** | `FixedSizeList<Float64, N>` for known dimension, `List` for the ragged case. The values round-trip exactly through the table format; the fixed width is carried in field metadata, so an external reader ignoring it sees a correct variable-length array rather than something wrong |
| **Vector kernels** | Elementwise, dot, L1 and L2 norms, euclidean and cosine distance. Every reduction bit-deterministic under permutation, asserted by a test whose fixture is itself proven adversarial — a naive sum fails on it |
| **Linear algebra** | Multiply, transpose, trace, identity, `matvec`, and LU with partial pivoting giving determinant, inverse and solve. The pivot breaks ties on the lowest row index, without which two builds could factor differently |
| **Statistics** | Variance (two-pass), covariance, correlation, skewness, excess kurtosis, median, standardise, least squares |
| **Calculus** | Central-difference derivatives, trapezoid and Simpson integration, cumulative integral |
| **SQL surface** | `vec_*` and `mat_*` functions, plus constructors so a matrix can be built and operated on without being stored. A matrix's shape lives in Arrow's canonical `fixed_shape_tensor` metadata and a column without one is refused rather than assumed square |
| **The date axis** | `sank_data_date`, of type `DATE`, declared per table and never defaulted per row. Granularity declarable. Nothing yet writes partitioned directories — the declaration exists and the partitioning does not |
| **Publishing and repair** | A library and CLI for writing an external table, a verifier that does not assume it was used, and repair that derives rather than guesses |

Not built, deliberately: QR, SVD and eigendecomposition. They are where an in-house
implementation is worse than none, because a subtly wrong SVD produces plausible singular
values.

---

## What does not exist

Stated plainly, because a status document that omits this is marketing.

- **No server.** The binary is a stub; there is no daemon, no listener, no lifecycle.
- **No streaming transport.** Changes are drained through a SQL function rather than a
  replication connection. The decoder and pipeline are transport-agnostic by design, but
  the transport itself is unwritten. Note that neither mainstream Rust PostgreSQL client
  supports the replication protocol, so this is real work rather than a wiring exercise.
- **No slot lifecycle.** No creation policy and no position advancement. The
  source-safety ladder now exists and is tested against a real slot, but nothing drives
  it on a timer yet — it is a decision function without a caller.
- **No backfill reader.** The handoff *contract* is built and verified against a live
  slot, but nothing yet reads the existing rows. Only changes after a slot exists are
  captured.
- **The arrival tier is a retention contract, not the buffer the architecture
  describes.** What exists is the part that governs correctness: coverage, per-row
  filtering at the durable frontier, and the rule that publication alone releases
  memory. What does not exist is §5.4's epoch ring, the per-epoch key digests that let
  a historical query skip the tier at no cost, and per-tenant sub-caps. Nothing yet
  wires the tier into the ingest path either, so read-your-own-writes still waits for
  publication in practice.
- **The cardinality sketch is not persisted.** The protocol has nowhere to put it, so a
  column read back from the log reports zero distinct values. Nothing currently reads
  that figure, but it is a trap for whatever does first.
- **No multi-part or V2 checkpoints, and no log cleanup.** A checkpoint is written as a
  single file, which is fine into the millions of live files and not beyond; and nothing
  deletes the commits a checkpoint subsumes, so the log directory grows without bound even
  though nothing reads most of it.
- **Nothing routes a captured table to the resolved provider automatically.** The
  capability is derived from what the source declared and the resolution works end to
  end, but the caller still has to assemble the two — there is no catalog mapping a table
  name to its provider, because there is no catalog.
- **The governor decides but governs nothing.** Admission and the pressure ladder are
  built and tested, and nothing calls either: no memory pool reports its occupancy, no
  subsystem publishes a signal, and no query passes through admission on its way to
  running. They are decision functions without callers, like the maintenance scheduler
  was before the driver.
- **No counting allocator and no spill isolation.** The architecture requires true
  accounting outside the engine's own pool, and spill on a different filesystem from the
  write-ahead log. Neither exists. Deadline propagation and cancellation *do* — bounded
  at one batch per partition — which is why they are no longer listed here.
- **Exact order statistics buffer their input.** Selection is linear rather than
  `n log n`, so it beats sorting, but every observation must be resident. `FR-QUERY-08`
  asks for a bounded-memory algorithm over large inputs and this is not one. Exact and
  bounded are independent properties, and only the first is delivered.
- **The exactness gate is not wired into a session.** `check_exactness` is a function
  with no caller: nothing carries the session's exactness setting, and nothing attaches
  the watermark to a result.
- **No catalog.** The table provider exists and resolves mutable tables correctly, but
  nothing maps a table *name* to one, so the caller still has to assemble it.
- **No deletion vectors, column mapping or partition values in the log.** Row counts,
  bounds and null counts are written, and checkpoints are; everything else the protocol
  permits is not. A reader requiring any of them refuses these tables, which is the
  correct outcome — refusing is visible, and a partially-implemented protocol feature is
  not.
- **No table partitioning.** Every scan is over the whole table, pruned by file
  statistics rather than by partition. This is why `NFR-PERF-03`'s partition-predicate
  precondition cannot be satisfied by any query today.
- **Nothing calls the maintenance loop on a timer.** The tick is built and tested end to
  end, but a caller has to invoke it, supply the live set and supply the pinned snapshot
  positions retirement checks against.
- **The server runs and executes statements.** Real `psql` connects, authenticates, runs
  catalogue queries and ordinary SQL — aggregation, expressions, null semantics — against a
  policy-wrapped provider — over **real Parquet on disk**, through the M3 read path, which
  plans from the table log alone and prunes files by recorded statistics. The server walks
  a `<schema>/<table>/` warehouse at startup and reads each table's schema out of its own
  log rather than inferring it from a footer, because a table with no files yet has no
  footer and one whose files predate a column would be missing it.
- **The security path is now reachable.** A table the principal may not read is never
  registered, so naming it fails to resolve rather than confirming it exists; a policy row
  predicate is enforced where no provider can decline it, and a tautology cannot widen it.
  Both were provable in unit tests before and unreachable through the server — an
  unreachable enforcement point is one nobody has confirmed is on the path.
- **One API surface of four.** The wire protocol works. Arrow Flight SQL, the gRPC control
  plane and the REST gateway are not built.
- **Per-tenant graph epochs and envelope encryption are still unreachable through the
  server.** Built and tested; nothing wires them to the front door.
- Nothing drives graph hydration on a timer and no process loads a pack bundle.

---

## Known defects found and fixed

Recorded because the interesting information is usually in what went wrong.

| Defect | How it surfaced |
|---|---|
| Rows of the transaction that first introduces a table were silently refused | Only visible with several tables interleaved; a single-table test cannot see it |
| The conformance fixture's central assertion passed vacuously — the "large" value compressed 80× and stayed inline, so it was never withheld | Caught by making the generator fail loudly if the value does not exceed the out-of-line threshold |
| The fixture capture tool injected newline separators into a binary stream | The decoder was right and the capture was wrong; found at the first byte after a transaction boundary |
| The layer rule forbade same-layer dependencies, which was wrong rather than strict | A vocabulary crate legitimately building on another |
| The documentation-rot check found a stale version claim on its first run | Its own first execution |
| **Zone offsets were stripped rather than applied, shifting a whole timestamp column by four hours** | End-to-end reconciliation against the source. Every value stayed internally consistent, so nothing looked wrong until the two sides were compared |
| **Duplicate suppression worked per batch rather than per row, so a batch spanning the restart boundary republished its already-durable half** | Crash-safety tests sweeping every possible interruption point. A resent stream does not rebatch identically, which a single hand-picked crash point would not have revealed |
| **A restart would have overwritten a live file.** The pipeline's file sequence was in-memory state starting at zero, so a restarted capture wrote `00000000.parquet` over a file that was still live and still referenced | Committing to the log. The overwrite had always been possible, but nothing could see it: the file count does not change, no error is raised, and the rows in the overwritten file simply become different rows. It surfaced as a version conflict — the log refusing to create a table that already existed — and the overwrite was the real defect behind it |
| **The test written for that overwrite could not detect it, twice over.** It asserted on file *names*, which an overwrite does not change; and its fixture published a single file per run, so resuming from the highest committed sequence and resuming from the lowest were the same number | The mutation audit, on two consecutive attempts. Now asserted on the log's own history — a path added twice *is* the overwrite — with a fixture that publishes at every transaction boundary, as continuous capture does |
| **The log described where files were but not what was in them.** With no row counts, a compaction plan driven from the log could not state what it expected to merge | The first tick planned from the log rather than from a value threaded out of the writer. The merge's own row-count check refused the plan — the guard worked, and what it caught was that the log was incomplete rather than that the merge was wrong. `numRecords` is now written and read |
| **Capture and maintenance could not both commit to the same table.** The publish path held its next version in memory and failed outright when a compaction took it | The first test that ran capture and maintenance together. Before it, every test had exactly one committer. Failing there inverts the ordering rule — a compaction could stop capture — so the publish path now rebases onto the next free version, which is what the error message had been telling it to do all along |
| **Vectorised bounds became unsafe in the presence of NaN.** Arrow's aggregate kernels propagate NaN, so `max` over a column containing one returns NaN; the NaN guard then refused it and left whatever bound had been recorded so far — a maximum *below* the true maximum | The unit test written for the NaN guard, on its first run. The narrowed bound is the one direction that matters: a file holding 2.5 would be skipped for `f > 0` because its statistics claimed it topped out at −1.5, silently and undetectably. A NaN result now invalidates the fast path and the column is measured again skipping NaNs, so the cost falls only on columns that actually contain one |
| **The hand-written Delta log was invalid, and this crate's own reader accepted it happily.** The `add` action's `partitionValues` field is non-nullable and was omitted entirely | The kernel, on the very first read. The log looked reasonable and round-tripped through this crate perfectly, because a reader ignores a field it never writes. Two implementations agreeing is worth nothing when one of them wrote both sides |
| **A directory listing is not a file set, and both the planner and the read path were treating it as one.** Compaction only ever adds, so between a merge and the retirement of its inputs the directory holds both — the same rows twice, by design, for at least a full grace period | Writing the convergence test. Re-observing the directory each tick made the planner merge files an earlier merge had already superseded. Nothing is wrong on disk; the *readers* were wrong. The published tier now names its files individually, the live set is carried across ticks, and the negative case is a test: the same query against the directory returns the merged rows twice |
| **The arrival tier declared coverage it did not hold.** A tier starting mid-stream reported from the durable frontier rather than from its own oldest segment, so it claimed every position before its first captured transaction | The first query spliced across two real tiers. The splice found an exact cover that did not exist, so the query would have been *answered* with the missing positions silently absent — the failure mode the splice exists to prevent, produced by the tier lying to it. This is the normal case rather than an edge case: a table onboarded from a running stream starts mid-stream by construction |
| **The property test written to catch that defect did not catch it.** Its generator started the publication frontier equal to the stream's origin, so the two could never diverge, and its assertion encoded the buggy expectation | Deliberately reverting the fix and finding the suite still green. The generator now starts publication at zero independently of the origin, and fails within a second on the reverted code |
| **A commit shipped one of the mutation audit's own deliberate defects, with a failing test.** The reversed-comparison fix in the predicate reader was reverted in the tree — `5 < x` was read as `x < 5` — so the reader skipped files that did hold matching rows | Reconciling the working tree before the merge. The audit restores what it mutates through an in-flight record and signal handlers, but nothing survives a `kill -9`, and the leak is invisible: the diff is one plausible-looking line in a file the commit was already touching. The test that proves the mutation caught was sitting red in `main`'s parent. Now guarded by `cargo xtask check-mutations`, which asserts every catalogue entry still matches its source — no compilation, milliseconds, so it gates every build rather than only a full audit. It catches catalogue drift by the same check |
| **The Parquet writer's default compression was never enabled.** The workspace pin omitted `zstd`, so every write on the default configuration panicked inside the column writer | The first crate to use the writer *without* also depending on DataFusion. Cargo unifies features across dependencies **and dev-dependencies**, and DataFusion — a dev-dependency of the writer's own crate — was quietly supplying the feature. The crate's entire test suite passed while the library was broken for every real consumer. Now guarded by `cargo xtask check-features`, which reads the manifest rather than the resolved graph, because the resolved graph is precisely what hides it |

---

## On the tests

Two of this project's invariant tests were reviewed, passed, and would not have failed
on the defect they were written for. Neither was found by reading them.

- The arrival tier's coverage property started the publication frontier at the stream's
  origin, so the two could never diverge — and its assertion asserted the buggy value.
- `is_exact_cover`, the oracle every splice test leans on, had no tests of its own. It
  could be changed to tolerate gaps between tiers, or to stop requiring the cover to
  reach the target, and the whole suite stayed green. Every splice test would have gone
  on passing over a planner returning partial covers.

A fourth was not a weak test but a **missing** one. The fix that made the provider's scan
run on more than one core had no test at all — the defect had been found by measurement,
and nothing was written to hold it. Months later that fix was replaced with a better one,
and the only thing that caught the intermediate regression was a *deadline* test which
happened to contain a self-check asserting its own plan ran in parallel. A test written
for one thing guarded another by accident, which is not a mechanism anyone should rely on
twice. There is now a direct test, asserted at the scan node rather than above it, because
a repartition can manufacture eight partitions from one serial reader.

A third was found the same way: the property named *open transactions are never
published* aborted every unsealed transaction before flushing, so it never left one in
flight. A mutation that published rows from transactions that had not committed survived
it untouched — and an in-flight transaction is the steady state of a busy source, not an
edge case.

`tools/mutation-audit.py` now keeps a catalogue of specific, plausible defects, applies
each one and reports whether anything fails. It refuses to run on a dirty tree, and it
flags a catalogue entry that no longer matches the source — a stale entry proves nothing
while looking like coverage, which is the failure mode the tool exists to find.

A fourth was found the same way, later: the driver's compaction removals were checked
only through the constructors they *could* have called, not through what the driver
actually committed. Swapping one for the other went unnoticed.

A fifth: the table provider could claim it evaluated predicates *exactly* — which gives
the engine permission to drop the filter from the plan entirely — and every test still
passed, because not one of them had a `WHERE` clause.

A sixth: the test asserting that a disjunction is never split used `a = x OR a = y`,
which the engine rewrites into an `IN` list before it reaches the code under test. The
test exercised no disjunction at all.

A ninth was found by the audit *hanging* rather than reporting: a threaded cancellation
test looped forever when the cancellation check was removed, so a defect that should have
been a failure in seconds became a thirty-minute stall. The loop is bounded now — a test
that stalls a pipeline is how a real defect gets discovered in someone else's build
rather than in this one.

An eighth: the regression guard written for the quadratic replay above passed against
the quadratic replay. It spread its workload across thousands of commits, where linear
file I/O dominates the quadratic term entirely.

A seventh, and the most useful: statistics computation had no tests in its own crate at
all. It was exercised only through an end-to-end test in a *different* crate, so
`cargo test -p sankhya-table` covered none of it and the audit reported two survivors
immediately. Writing the missing tests found the NaN bounds defect above on the first
run.

The catalogue also produced one **equivalent mutant** — a change to a duplicated guard
that left the second copy still refusing, so behaviour was unchanged and no test could
possibly have caught it. That is worth recording rather than quietly deleting: an entry
that can never fail trains you to read "SURVIVED" as noise, which is the one habit that
makes the whole exercise worthless.

**The general lesson, recorded because it applies to every test not yet audited:** a test
written to catch a defect is not evidence that it catches it. Until it has been run
against that defect, it should be assumed to be in the same state as the four above.

---

## Measurements

On a 24-core machine with NVMe storage.

| | |
|---|---|
| Cold `cargo check`, full critical dependency family | 32.9 s |
| Vendored PostgreSQL build | ~2 min, 35 MB installed |
| Synthetic generation | ~147 MB/s |
| Bulk load, 10 tables | 99,235,357 rows / 10 GiB in 188.7 s (~526k rows/s) |
| On-disk size after load | 14 GB |

### Query-engine settings

| | |
|---|---|
| Filter pushdown, 5M rows / 523 MiB / 1-in-10,000 selectivity | **1.02× — neutral** |
| The same measurement with a compressible payload | 0.74× — *slower*, an artefact of the fixture |
| The same measurement, fixture written by the product's writer (2026-08-28) | 1.06×, 0.94×, 0.96× across runs — noise around neutral |

**This corrected a claim rather than confirming one.** Earlier drafts of the
requirements and architecture documents asserted that filter pushdown was worth roughly
an order of magnitude. It is not, on this shape. The setting is still pinned — neutral
is not harmful and the benefit is expected on wider payloads — but the documents now
record the measurement instead of the assumption.

The first attempt showed pushdown *slower*, which was a fixture artefact: a repetitive
padding string dictionary-encodes so well that decoding it is nearly free, so avoiding
that decode saves nothing while the row-selection bookkeeping still costs. Worth
recording, because a benchmark whose data is unrepresentative produces confident wrong
numbers rather than obviously wrong ones.

The companion claim about the Parquet page row-count limit has **not** been measured and
should be read as unverified.

**The fixture was writing its own Parquet, and that was the third thing wrong with it.**
Found 2026-08-28 while auditing test code for reimplemented infrastructure. The benchmark
configured its own `ArrowWriter`: it duplicated three of `WriterConfig`'s six decisions --- zstd,
page statistics, the page row limit --- and silently dropped `max_row_group_row_count`, the
statistics truncation length and the commit-LSN encoding. Pushdown is only as good as the page
index and the page index is emitted by writer settings, so the measurement was of a file
Sankhya would never produce, and a change to the product's layout could not have moved the
number.

Routed through `sankhya_table::write_parquet` the figure is unchanged in substance --- noise
around neutral, which is what the TPC-H measurement in `session.rs` already concluded from
better evidence --- so nothing downstream of it moves. What changed is that the number is now
about the product. The test asserts only what the measurement supports: pushdown does not
change the answer, and is not materially slower.

The shape is also the reason, and the file's own comment had it backwards. `needle` is
`id % 10_000` over sequential ids, so every 20,000-row page spans the column's whole range: the
predicate eliminates 99.99% of *rows* and no *pages* at all, and decoding is per page. The
comment called this "the ordinary shape of an analytical table". It is not, and the claim went
unchallenged for as long as the fixture was also choosing its own encoding.

### Compaction

400 fragments totalling 20,000,000 rows, merged into one file.

| | 400 files | 1 file | Ratio |
|---|---|---|---|
| Short query — one narrow range | 16.9 ms | 3.8 ms | **4.42×** |
| Long query — full aggregation | 121.0 ms | 97.3 ms | **1.24×** |
| On-disk size | 61.0 MB | 27.4 MB | **2.23×** |

**This confirmed a claim, having first failed to test it.** The architecture asserts that
small files cost query *planning* rather than scanning, which predicts a roughly fixed
per-query penalty — dominant on short queries, amortised away on long ones. The
measurement bears that out: the absolute overhead stays in the same order (13 ms to
24 ms) across a query doing thirty times more work, while the ratio collapses from 4.42×
to 1.24×.

The first run used 1,000,000 rows and produced 4.43× and 3.77× — apparently uniform, and
readable only as "more files are slower". The long query was not long enough for
planning to amortise against. The fixture was scaled until the two hypotheses gave
different answers; before that it was not evidence for either.

The practical consequence is that fragmentation is an **interactive-latency** problem
rather than a throughput one, which is the reason it is worth a first-class subsystem.

### Query planning

400 fragments and up, planned but not executed, best of three.

| Files | Provider | Directory listing | |
|---|---|---|---|
| 50 | 0.54 ms | 1.15 ms | **2.2×** |
| 200 | 0.63 ms | 3.02 ms | **4.8×** |
| 800 | 1.37 ms | 10.33 ms | **7.5×** |

The advantage widens with file count, which is what the metadata-only claim predicts:
the provider reads no Parquet footers, so its cost does not scale with the number of
files it is planning over.

**It does not scale with nothing, though.** Sixteen times the files costs the provider
about 2.5× more planning, because it replays the table log and the log grows with commit
count. The cost has moved from one seek per file to one sequential read — which is a much
better shape and is not the same as free. It is also the argument for log checkpoints,
which are not built.

### Where the memory limit bites

The gate is that a hostile query is *refused* rather than fatal, so the first thing to
establish is where refusal actually begins.

| Pool | An aggregation that cannot reduce anything |
|---|---|
| 64 KiB – 1 MiB | Refused: `Resources exhausted … SingleHashAggregateStream` |
| 8 MiB and above | Succeeds, 400,000 groups |

The operator **errors rather than spilling**, which is what makes admission worth having:
without it a query gets far enough to fail, having consumed the machine on the way.

**The first version of this test used two megabytes and passed while proving nothing** —
just above the line, so the query succeeded and the assertion never ran. The threshold is
now measured rather than guessed, and the test asserts the *operator* that ran out of
room as well as the fact that something did, so an aggregation that quietly started
spilling would be visible rather than silently making the gate meaningless.

### Replaying the table log

One add per commit, replayed from the first.

| Commits | Before | After |
|---|---|---|
| 100 | 0.32 ms | 0.34 ms |
| 1,000 | 1.99 ms | 3.58 ms |
| 10,000 | 81.5 ms | 20.5 ms |
| 50,000 | **1.96 s** | **117 ms** |

**The first column is a defect, not a cost.** The replay searched the accumulated file
list for every add and filtered it for every remove — quadratic in the number of files,
invisible in every test, and two seconds per query plan at fifty thousand commits. A
table committing every ten seconds reaches that inside a week.

Positions are now held in an index. What remains is linear and is dominated by opening
one file per commit, which is what checkpoints exist to fix rather than anything an
algorithm can.

The regression guard measures the **ratio** between two sizes rather than elapsed time,
so it means the same thing on any machine. Its first version put the actions in thousands
of separate commits and passed against the very implementation it was written to catch —
the quadratic term is per action, not per commit, so file I/O buried it. Concentrating
the actions into ten commits separates them: 16.1× for four times the files against about
4× for the fixed version.

### Checkpointing the log

A cold reader — an external engine, or a process that has just started.

| Commits | Replay | From a checkpoint | | Checkpoint size |
|---|---|---|---|---|
| 1,000 | 1.90 ms | 0.39 ms | **5×** | 32 KiB |
| 10,000 | 28.7 ms | 3.21 ms | **9×** | 269 KiB |
| 50,000 | 141 ms | 13.9 ms | **10×** | 1.3 MiB |

This is the number that matters to **other engines**, which have no cache and start cold
every time. A Spark job reading a fifty-thousand-commit table opened fifty thousand files
before reading a row.

The kernel reads a checkpoint this system writes by hand — nested structs, maps and lists
in Parquet, against a protocol it does not own. And it is shown to *use* it rather than
merely tolerate it: the commits the checkpoint covers are deleted, leaving it as the only
record of those files, and the table still resolves. Without that step both checkpoint
tests would have passed whether the kernel read the file or ignored it and replayed,
which is precisely the shape of a test that proves nothing.

**Nothing depends on a checkpoint being present, correct, or parseable.** It holds exactly
what replay produces, so every failure — a missing file, a corrupt pointer, one left
behind by a table that was dropped and recreated — falls back to the log and costs a
replay rather than an answer. That is what makes it defensible to write this by hand: the
worst a bad checkpoint can do is be ignored.

### Caching a table's file set

The same table, replanned by a long-running process.

| Commits | Cold replay | Cache, unchanged | Cache, one new commit |
|---|---|---|---|
| 1,000 | 1.66 ms | 23 µs | 43 µs |
| 10,000 | 22.1 ms | 224 µs | 371 µs |
| 50,000 | 140 ms | **1.20 ms** | **1.88 ms** |

**117× on an unchanged table, 75× after a commit**, and both are now proportional to what
changed rather than to the table's history. The residual 1.2 ms is copying fifty thousand
file entries into the answer, which is what the caller asked for.

Two things had to change to get there, and each was worth about an order of magnitude:

- **Asking is a probe, not a listing.** Versions are contiguous — enforced at commit
  rather than assumed — so a reader that knows the state at version *n* asks whether
  *n+1* exists. Listing instead made every lookup proportional to the whole history, which
  is the cost the cache existed to remove.
- **The index is kept, not rebuilt.** Resuming from a bare file list rebuilds the position
  index over every live file, swapping *linear in the history* for *linear in the table* —
  better, and still not right.

The cache cannot go stale, and that is structural rather than careful: it never trusts its
own version, there is no invalidation, no expiry and no notification to miss. A table
dropped and recreated at the same path is detected and replayed from the beginning, rather
than resumed from a base describing files that no longer exist.

### Computing statistics at compaction

5,000,000 rows across three columns, merged.

| | |
|---|---|
| Merge including statistics | 357 ms |
| The statistics alone | 83 ms — **23% of the merge** |
| The same, before using vectorised kernels | 164 ms — 36% |

**This corrected a claim.** The first version of the module said bounds came from Arrow's
vectorised aggregates and were "close to free". They did not and were not: bounds were
computed in a scalar loop costing 29% of the merge on its own, five times what the
cardinality sketch costs. The comment was written before the measurement and was false
when written.

With the kernels actually used, bounds and widths are close to free and the remaining
23% is almost entirely the sketch, which has to hash every value. That cost is paid
because a cardinality estimate is the one statistic neither the file format nor the table
log carries.

23% on top of a merge is not nothing. It is still much cheaper than the alternative,
which is a second full pass over storage — and it means statistics arrive without anyone
running an analysis command, which matters on a system where tables onboard themselves
from a replication stream and the tables nobody thought about are exactly the ones that
would have none.

### File pruning

200 files of 2,000 rows, the same query with and without the catalogue.

| Predicate selects | With statistics | Without | |
|---|---|---|---|
| One file in 200 | 1.26 ms | 9.21 ms | **7.3×** |
| A tenth of the table | 3.89 ms | 10.81 ms | **2.8×** |
| Everything | 17.52 ms | 17.71 ms | **1.0×** |

The gradient is the point. Pruning helps in proportion to what a predicate can exclude,
so a single figure would be meaningless — and the last row matters as much as the first:
when nothing can be pruned, consulting the catalogue costs nothing measurable.

Every row of that table was checked to return the same answer with and without the
statistics, because a pruning measurement that does not verify the answer is measuring
how fast it can be wrong.

### Deterministic reduction

Reduction sums in a canonical order with Neumaier compensation on top. Searching which
of the two actually delivers order-independence produced a correction rather than a
confirmation.

| | |
|---|---|
| Randomised inputs tried, spanning 120 orders of magnitude | 3,000 |
| Permutations where compensation *alone* gave a different total | **0** |
| Hand-built adversarial cases where it did | **0** |

**Compensation is what makes the total order-independent in practice.** The canonical
sort — which the first draft of the documentation credited with the property — changes
no behaviour that could be observed. What it contributes is a *guarantee*: Neumaier's
error bound bounds the error without proving bit-identity across permutations, and its
compensation term is itself one `f64` that can lose bits when corrections span extreme
ranges. Sorting makes the result a function of the multiset by construction.

The sort is therefore defence in depth, at `n log n` against `n`. It is kept because the
figures it exists for have to be defended later, and *"we could not find a
counterexample"* is a weaker thing to say than *"there cannot be one"* — but the
documentation now says which of the two mechanisms is doing the work.

### Capture at scale

One million rows across all ten tables, in five interleaved transactions.

| | |
|---|---|
| Rows written | 1,000,000 in 2.0 s (~500k rows/s) |
| Messages decoded | 1,000,060 |
| **Rows captured through the pipeline** | **1,000,000 in 3.5 s (~285k rows/s)** |
| Retained write-ahead log during the run | 1,053 MB |
| Files published | 10 |
| Bytes published | 6.3 MiB |

**On the compression figure**, because it would otherwise be misleading: 6.7 bytes per
row reflects *this* data, which is deliberately regular — sequential identifiers,
patterned labels, timestamps clustered within a single run. Dictionary encoding,
delta-encoded monotonic columns and zstd all do unusually well on it. Do not read it as
a general ratio; the honest range against a realistic schema is closer to 3×–15× the
source's footprint, driven mostly by cardinality and index count. See
`REQUIREMENTS.md` §7.4 on estimates versus measurements.

Reproduce with:

```bash
SANKHYA_PG_BIN=$PWD/.build/pg-install/bin SANKHYA_E2E_SOCKET=/tmp/sankhya-sock \
  cargo test -p sankhya-ingest --test scale --release -- --ignored --nocapture
```

---

## How to check this document is honest

```bash
cargo xtask check-all            # every repository invariant: layers, file length, doc
                                 # links, version claims, feature pins, clippy with the
                                 # workspace's denied lints across every target, and that
                                 # no mutation is still applied to the source
cargo test --workspace           # 1,762 tests, none of which needs a database
cargo xtask check-performance    # the NFR-PERF objectives, as a gate that can fail
python3 tools/mutation-audit.py  # 408 specific defects, applied one at a time
crates/sankhya-cdc-apply/tests/run_e2e.sh   # capture against a live database
```

The performance gate needs a quiet machine and several minutes, which is why it is not in
`check-all`. Everything else runs unattended.

[`QUICKSTART.md`](QUICKSTART.md) walks through building it from nothing.
