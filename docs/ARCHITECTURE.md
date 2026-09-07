<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — System Architecture

**Document ID:** SNK-AD-001
**Version:** 0.2.0
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Date:** 2026-09-06
**Companions:** [`REQUIREMENTS.md`](REQUIREMENTS.md) — what it must do. [`INVARIANTS.md`](TESTING.md) — the rules and what enforces each. [`OPERATIONS.md`](OPERATIONS.md) — running it. [`SECURITY.md`](SECURITY.md) — the posture and the policy. [`STATUS.md`](STATUS.md) — the dated record of what is built.

---

## 1. What this document is, and how to read it

This describes how SANKHYA is built. It does not restate what it must do, which is
[`REQUIREMENTS.md`](REQUIREMENTS.md), and it does not restate what is finished on any given day, which
is [`STATUS.md`](STATUS.md).

**Its organising rule is that a design document which cannot distinguish the designed from the built
is a brochure.** The previous version of this file could not. It described four runtimes where one
runs, resource governance that governs nothing, and a plan hash nothing computes — in the present
tense, beside sections describing mechanisms that do run, with nothing to tell a reader which was
which. That is the failure the 129-finding audit was mostly about, and this rewrite is organised
against it.

So the document is in two parts.

**Part I is the system that exists.** Every section in it describes something a running process does,
and it starts with what happens when somebody types a query, because that is the shortest path to
understanding what this actually is.

**Part II is designed and not running.** Every section in it is marked, and each names what has to
exist first. None of it is written in the present tense.

Two things make the split checkable rather than a matter of my care in writing, and §2 is about
those.

### Reading order

1. **§2** — how the repository distinguishes built from designed.
2. **§3** — what a query actually does. Read this before anything else.
3. **§4** — what bounds a query, and the large gap in it.
4. **§5–§14** — the rest of what runs.
5. **Part II** — what does not.

---

## 2. Built and designed, and how this repository tells them apart

Three mechanisms decide it, and none of them is prose.

### 2.1 `UNREACHED` — crates no binary reaches

`xtask/src/surfaces.rs` computes reachability from every root that ships — `sankhya-cli`, plus every
pack — and requires every crate under `crates/` to be either reachable or **listed with a milestone**.
The list is self-pruning in both directions: an unlisted unreachable crate fails the build, and a
listed crate that has *become* reachable or been deleted fails it as a `STALE EXCUSE`, so the list
cannot grow into a place where things go to be forgotten. A reason shorter than thirty characters is
a panic.

Ten crates are on it, and they are the honest shape of what is designed rather than delivered:

| Crate | Why it is unreached |
|---|---|
| `sankhya-cdc-pg` | M2's carried remainder. The slot lifecycle, the lag thresholds and the source-safety ladder are built and tested; the **driver** that runs them on a timer is not |
| `sankhya-oltp-pg` | The PostgreSQL supervisor is built and tested against the vendored 17.11; `Settings` has no transactional-store configuration. M8 §12.2, beside leader election |
| `sankhya-tiering` | M9, explicitly gated on the drills |
| `sankhya-objectstore` | M8 §12.1 — where the version claim lands on an object store, as a conditional put |
| `sankhya-api-rest` | The route table and the size decision are built and tested; serving them needs an HTTP listener, HTTP authentication and a pre-materialisation row estimate |
| `sankhya-mv` | Undecided by [ADR-0014](adr/0014-materialized-views-and-the-cube-lifetime.md); listed rather than deleted because the design question is open |
| `sankhya-pack` | M4's remainder — the declarative pack tier is built; the loader that reads a bundle directory into a running process was never finished |
| `sankhya-ports` | Decided: **delete.** Nothing implements a single trait in it, and its header asserts a property the workspace does not have |
| `sankhya-datagen`, `sankhya-testkit` | Reached only from dev-dependencies, which the traversal ignores on purpose — a *surface* reachable only from a test is the defect; a generator of test data is not one |

### 2.2 `UNREACHABLE` and `MAPPED_BUT_UNREACHABLE` — codes nothing produces, and codes no query reaches

`xtask/src/catalogues.rs` reads every `.rs` file under `crates/` except `sankhya-error` itself and
asks, of every documented code, whether anything constructs it. A code that nothing constructs must
be **declared unreachable with a reason**, and the reason is printed into [`ERRORS.md`](ERRORS.md) as
*"Not produced by this build."*

Ten codes are on it. Seven wait on a subsystem that does not exist. **Three are worse**: the
condition happens today and is reported through a crate-local type nothing maps onto the code, so an
alert rule written from the catalogue is permanently silent while the failure it names occurs ---
commit conflicts (`sankhya-publish` reports its own `CommitError`), cancellation, and backup
verification.

A second list, `MAPPED_BUT_UNREACHABLE`, holds three more, and it exists because the first list
answered the wrong question. `UNREACHABLE` asks *does anything construct this?*; an operator asks
*can this fire?* Those were the same question until the crate-local conditions were mapped onto
their codes --- `SpliceError::CoverageGap` onto `SNK-S0001`, `Unservable::NotReconciled` onto
`SNK-S0002`, `SpliceError::BeyondFrontier` onto `SNK-T0003`. The conversions exist and are tested,
so a caller that meets the condition now reports it correctly; nothing in this build meets it. The
two lists are guarded in opposite directions: an `UNREACHABLE` entry that becomes constructible
fails the build, and a `MAPPED_BUT_UNREACHABLE` entry that stops being constructible fails it too,
because that entry claims a mapping exists.

`SNK-S0001` is the one worth reading twice, because it states a fact about the read path more
precisely than any paragraph in this document does: the tier splice that would raise it is **not in
the server's read path**, which synthesises a coverage range rather than composing one. §3.8. It
had also read as *produced* for a while, because `SpliceError::CoverageGap` contains the substring
`Error::CoverageGap`; the check requires a word boundary now, which is what made the real state
visible.

`SNK-S0002` was the sharper find of the two. `FR-TIER-23` requires a conflict to make unified
queries on the affected table fail **with a typed error**, and what it produced was `Option::None`
--- which carries no code, no remediation and no name for what went wrong. `Unservable::NotReconciled`
was declared, documented in `unify::plan`'s `# Errors` as returned *"when the witness is for another
table"*, and constructed nowhere; `plan` takes the table *from* the witness, so it could not detect
the case it documented. The refusal now belongs to `Registry::servable_or_refuse`, where the witness
is obtained.

### 2.3 The enforced-by column

[`INVARIANTS.md`](TESTING.md) states every structural rule with a third column naming what enforces
it — a build check, a test, or *nothing yet*, marked as such. `cargo xtask check-invariants` verifies
that every check named there exists. This document does not restate those rules; where one is
load-bearing here, it is named and the reader is sent there.

### 2.4 What this document does with all that

Every section below carries a marker: **[Built]**, **[Built, with a named gap]**, or, in Part II,
**[Designed]**. Where a section says something is built, a file is named where a reader can go and
look. `cargo run -p xtask -- check-docs` verifies that every such path exists; it cannot verify that
the prose still describes what the code does, and saying so is the point rather than an apology.

---

# Part I — The system that exists

## 3. What a query actually does — **[Built]**

A single process. One tenant, fixed at startup. Three listeners on one Tokio runtime.

```
  psql / JDBC ─┐                                  ┌─▶ audit chain (durable, hash-linked)
               ├─▶ door ─▶ authenticate ─▶ Caller ┤
  Flight SQL ──┘                │                 └─▶ query log (one line, stderr)
                                ▼
                     policy decision ──▶ Guard ──▶ SecuredTable
                                                      │
                     session registered with ONLY the tables this caller may read
                                                      │
                                                      ▼
                     plan ──▶ table provider ──▶ live set from the log ──▶ prune by statistics
                                                      │
                                                      ▼
                     execute on the shared pool ──▶ stream ──▶ row cap ──▶ rows
```

### 3.1 What the server knows before anybody connects

At boot it walks the warehouse — `<schema>/<table>/` directories, each with its own `_delta_log` —
and reads each table's schema **out of its own log** rather than inferring it from a Parquet footer.
Two reasons, and both are ordinary rather than exotic: a table with no files yet has no footer, and a
table whose files predate a column would be missing it.

Underscore-prefixed directories are SANKHYA's own — `_snapshots`, `_cubes`, `_audit` — and are
skipped. A foreign object under the warehouse root is refused rather than ignored.

**A table the server cannot open is named on stderr rather than omitted.** A server that starts with
three tables of four and says nothing produces an outage that looks, to whoever queries it, like a
table nobody ever created. The same instinct fixed fourteen places that read a directory as
`let Ok(entries) = read_dir(x) else { return empty }` — which answers *"there is nothing here"* to the
question *"what is here?"* whenever the true answer is *"nobody could tell"*. The worst of them was
the diagnostic: an unreadable warehouse produced no tables and no complaints, and `doctor` printed
`Nothing to report` and exited clean.

The table set is **re-read every maintenance cycle**, not frozen at boot. A configured list goes stale
the first time somebody creates a table; so does a discovered list that is only discovered once, and
a table created after the server came up was maintained by nobody for the life of the process.

### 3.2 The door, and who the caller is

Both doors converge on the same `Caller`: a fixed tenant plus a subject. `authenticate` refuses an
empty subject, because an unattributable connection cannot be audited and an audit chain that cannot
say who is a log with extra steps.

How each door establishes that subject, and what it verifies, is materially different, and it is
[`SECURITY.md`](SECURITY.md) §3 rather than a footnote here. In one line: the wire door demands a
password and verifies it against `server.credentials`; the columnar door reads an unverified header.

### 3.3 Where security happens, and why there is exactly one place

Three engines answer questions in this system — SQL, graph, and, when it runs, tiering. Each resolves
a table through the same catalogue, and the catalogue's resolution takes a `Guard`
(`crates/sankhya-catalog/src/guard.rs`), which has no public constructor, no public fields and no
`Default`. Anything taking a `Guard` in its signature cannot be called until a decision has been made.

> **The defect a security architecture must prevent is not *the wrong policy*. It is *no policy*, on
> one path, on one day.** A wrong policy is visible in a test; a path that never asked is visible in
> nothing at all — no error, no log line, no wrong-looking number.

`execute::session_reaching` in `crates/sankhya-server/src/execute.rs` is where both doors register
tables, and it registers nothing it did not obtain a guard for. Two consequences follow that are
easier to state than to build:

- **A table you may not read does not exist.** It is never registered, so naming it fails to resolve
  with the same code, SQLSTATE and words as naming a table that was never created. The difference
  between those two messages is a working enumeration oracle, and there is no configuration that
  turns it on.
- **The policy predicate is conjoined above the scan**, where nothing can decline it. The first
  implementation handed it to the provider as a pushdown filter — the obvious design, and the one
  that reads best. A provider may *decline* a filter; `MemTable` does; and when it declined, every
  row came back with no error raised anywhere. The table was secured in name only, and the only
  evidence was the row count.

> **Correction to an earlier version of this document.** It said the tenant predicate is *"injected by
> an analyzer rule **and** independently asserted by the provider"*. **There is no analyzer rule.**
> `AnalyzerRule` appears nowhere in this repository, and enforcement is entirely the provider wrapper.
> `assert_filter_present` — the "independently asserted" half — is a `#[must_use] -> bool` helper in
> `crates/sankhya-catalog/src/secured.rs` whose only call site is
> `crates/sankhya-catalog/tests/enforcement.rs`. It is a test's assertion, not a second runtime
> control, and describing it as one described a defence in depth that has one layer.

### 3.4 Planning does no file I/O

The table provider is SANKHYA's own. The table-format library says **which files exist and what is in
them**; it does not read them, does not decode them, and does not appear in the execution plan. Scan
execution is the query engine's Parquet source, unmodified. §6.7 gives the version-skew argument that
forces this and the five capabilities that make it better than a workaround.

The measurable consequence is that planning does no file I/O: row counts come from the log, which
already records them. The alternative is one footer read per file before a single row is read — the
small-file penalty, moved somewhere compaction cannot help.

| Files | Provider | Directory listing | |
|---|---|---|---|
| 50 | 0.54 ms | 1.15 ms | 2.2× |
| 200 | 0.63 ms | 3.02 ms | 4.8× |
| 800 | 1.37 ms | 10.33 ms | **7.5×** |

*Measured; the run and its conditions are recorded in [`STATUS.md`](STATUS.md), §Measurements.*

The honest caveat is that the provider is **not flat**: sixteen times the files costs about 2.5× more
planning, because replaying the log grows with commit count. The cost has been moved from one seek per
file to one sequential read of a log, not abolished — which was the argument for checkpoints (§6.3).

Four details the provider gets right, and a naive one would not:

**Statistics are marked exact only when nothing can be filtered out.** A query pinned below what the
tiers hold has an *upper bound*, not a count. Reporting it as exact lets the optimiser order joins on a
number that is simply wrong — a slow plan chosen confidently, which is harder to notice than a slow
plan chosen for want of information.

**The commit-position column is read only when the query pins a position.** Time travel genuinely
costs a column the caller did not ask for, and hiding that would be dishonest about its price. When no
tier holds anything past the target the filter provably removes nothing, and **neither the filter nor
the column read is planned at all** — which matters because the cost is per table and compounds with
join arity.

**The scan reports its own statistics, not the table's.** These are different numbers arriving at
different times: the table's during logical planning, the scan's during physical planning, and join
selection reads the second. A provider supplying only the first leaves every table looking
unmeasurable at the moment the engine decides how to join it — so it repartitions tables it could
broadcast, and because tables reporting no size are ordered against tables that do, **one absent
figure moves every join in the query.** The scan's figures are also counted over the files that
survived pruning, so a selective predicate is reflected in the number the decision actually uses.

**File grouping is left to the engine above its own threshold.** The engine splits file groups by byte
range, which balances on size and beats anything a provider can do by counting files — but only for
scans large enough to be worth splitting, below which it leaves a single group alone, and a single
group is a single partition. So the provider deals files out only *below* that threshold. Doing both
is worse than either.

### 3.5 Pruning, and the measurement that overturned two of this project's assumptions

Pruning pays in proportion to what it eliminates and costs nothing when it eliminates nothing: over
200 files, a predicate selecting one file ran 7.3× faster with statistics than without, a predicate
selecting a tenth of the table 2.8× faster, and a predicate selecting everything at parity. Recorded
in [`STATUS.md`](STATUS.md), §Measurements.

Filter pushdown and late materialisation are a different story and the most instructive measurement in
this repository. Both ship **disabled by default** in the query engine, so SANKHYA asserts its required
configuration at startup and fails loudly on unexpected values rather than setting it once and
trusting it — an upstream default that changes between versions would otherwise be an invisible
regression.

Late materialisation is widely described as the single largest scan optimisation available. An earlier
version of this document said it was worth roughly an order of magnitude. Measured on a synthetic scan
it was 1.02× — neutral — and it was pinned on anyway, on the reasoning that neutral is not harmful.
Measured on TPC-H at scale factor 1 it is a **cost** at every selectivity tried, and with filter
reordering compounding it, TPC-H Q6 went from 357 ms to 917 ms at eight clients: **2.6× slower**.
(Recorded in [`STATUS.md`](STATUS.md), §A required setting that was costing 2.6×.)

> Late materialisation saves the decode of payload columns for rows a predicate eliminates. On this
> data those rows have already been eliminated, by row-group and page statistics, before any decoding
> begins. **Pushdown cannot save work that is not being done; what it adds is per-row bookkeeping on
> the scan that remains.** The two mechanisms are not complementary here — the cheaper one has already
> won.

It is left at the engine's default rather than pinned off, because pinning a setting off is still
pinning it and the evidence supports *not always* rather than *never*. **What this says about the
practice matters more than the setting.** Both errors came from a mechanism with a good reputation,
asserted on reasoning rather than on a measurement of this system's own data. A setting worth asserting
at startup is worth measuring on something somebody else designed.

One more default in the same family, and it is **not** measured: the Parquet writer's page row-count
limit is effectively unlimited, and with no row cap a narrow column packs enormous row counts into one
page — a boolean can fit tens of millions — so the page index degenerates to a single entry covering
everything and page pruning silently does nothing. That reasoning is sound and the claim is
**unverified**; read it as a hypothesis this project has not yet measured, which is the correction the
paragraph above earns.

### 3.6 Executing, and the shape of the answer

Execution runs on the process's single shared memory pool (§4). Results are **streamed** and stopped
at the row cap rather than materialised and then checked — the check used to run after
`frame.collect()`, so a statement returning ten million rows against a limit of ten thousand allocated
all ten million first and the refusal arrived after the damage. **A bound enforced by a check that runs
afterwards is not a bound.**

Refusing rather than truncating is the older decision and stands. A truncated answer that looks
complete is the failure this whole system is arranged against.

### 3.7 What is written down

Two records, deliberately different, because they answer different questions and merging them would
serve neither.

**The audit chain** is the evidence: durable, hash-linked, `sync_data`'d on every append, one entry per
table the *plan* scanned, with the principal, the decision, the row filter and column masks that
applied, and the statement's **shape** — never its text. [`SECURITY.md`](SECURITY.md) §7.

**The query log** is one `tracing` line per statement carrying who ran it, its shape, how many tables
it scanned, rows returned, milliseconds and whether it was refused
(`crates/sankhya-server/src/audit.rs`). It exists because *"which statements are slow?"* had no answer
anywhere: reading the audit means reading a chain rather than grepping a log, and the chain carries no
duration. [`OPERATIONS.md`](OPERATIONS.md) §7 has the fields and how to configure it.

> **There is no plan hash.** An earlier version of this document said *"a normalized plan hash is
> logged by default"*. Nothing in this build computes one. What is logged is `statement_shape`, and
> the two are not substitutes: a shape groups `select` with every other `select`, where a plan hash
> would group a query with its own repetitions. If plan-level grouping is wanted it is work, not
> configuration.

### 3.8 What the read path does *not* do

The tier splice — the mechanism §17 describes, which composes an in-memory arrival tier with published
Parquet under a proof of exact coverage — **is not in the server's read path.** The planner
synthesises a coverage range rather than composing one, and `SNK-S0001`, the coverage-gap refusal that
splice exists to raise, cannot be raised by this build. That is not an inference; it is the reason
recorded against `SNK-S0001` in `xtask/src/catalogues.rs`. The *mapping* now exists — a
`SpliceError::CoverageGap` converts to the code, with the positions in the detail — so the gap is
one call path rather than a call path and a conversion. `SNK-S0001` moved from `UNREACHABLE` to
`MAPPED_BUT_UNREACHABLE` accordingly, and an alert rule on it is still permanently silent.

The splice itself is built and property-tested in `sankhya-plan` and `sankhya-readpath`, and is
exercised end to end in tests. What is missing is that no capture runs, so there is no second tier to
splice, so the server resolves one.

Also not in the read path: a catalogue proper. The provider resolves mutable tables correctly and
nothing maps a table *name* to one automatically, so the caller assembles the two. The result cache
does not exist — **its key does**, because key correctness is a security property and the right time
to fix it is before anything caches (§6.6).

**And no served table folds a change log.** This is the boundary a reader is most likely to walk
into. The write path appends every mutation with an `_sankhya_op` of `I`, `U` or `D`, so a table
that receives updates holds every historical version of every row plus a tombstone per delete, and
a plain scan returns all of them.

The fold itself **exists and is correct**: `ResolvedTable` in `crates/sankhya-readpath/src/merge.rs`
wraps a raw scan as `DISTINCT ON (key) … ORDER BY key, position DESC`, then filters keys whose
latest version is a deletion — in that order, deliberately, because dropping tombstones first
leaves the *previous* version to win the distinct and a deleted row comes back holding the values
it had before it was deleted, which looks like data rather than like duplication. `ING-09` said
there was *"no reader that applies the ops"*, and that is not the state: what is true is narrower
and is the same shape as the splice above. **Nothing outside its own tests constructs one.** The
server resolves a table name to a raw `SankhyaTable`, and the caller who wants the fold has to
assemble it, which is the sentence two paragraphs up saying nothing maps a name to a provider
automatically.
`WriteStrategy::Mergeable`, computed at onboarding, does not close the gap either. It records that
the *source* **can** emit updates and deletes for the table — which is why onboarding warns when
it cannot — and **nothing downstream reads it**, including the fold: `ResolvedTable::new` takes a
bare slice of column names, and nothing checks where the caller got them. That is a weaker guard
than a typed one and it is the one that exists; this paragraph named a `Capability` guard on the
constructor, and there is none. Its own doc comment used to say the
value *"lets the storage layer skip merge machinery it will never need"*, which reads as though the
other branch selects some. Neither branch selects anything.

This costs nothing today, because nothing captures (`ING-00`) so no table receives updates through
this path — the fold is unreached rather than missing, and the work to close it is one wiring
decision rather than an algorithm. It is stated here rather than left to be discovered because the day a capture runtime
exists is the day a table silently returns its whole history to a `SELECT *`, and a reader who
learned that from the data rather than from this document has already believed a wrong number. The
fold arrives with the runtime, not before it.

One related value is now **null rather than wrong**: `_sankhya_commit_ts`. A commit time lives on
the transaction's `BEGIN` in a replication stream and a mutation does not carry one, so the column
was declared non-null and the writer stamped `1970-01-01T00:00:00Z` on every captured row. A reader
cannot tell that from a real instant, and a `WHERE _sankhya_commit_ts > …` excludes every row while
looking like a filter that found nothing. The column is nullable and the writer writes null, which
is what a capture runtime would later fill.

---

## 4. What bounds a query — **[Built, with a named gap]**

Six bounds exist. One is shared, one is a process-wide constant, and four apply to only one of the two
doors. [`OPERATIONS.md`](OPERATIONS.md) §6 is the table with the knobs; this section is why they are
shaped that way and what is missing.

### 4.1 One pool, shared, and fair

`shared_runtime()` in `crates/sankhya-server/src/execute.rs` builds one `FairSpillPool`, sized from
`SANKHYA_QUERY_MEMORY_BYTES` at one gibibyte by default, and holds it in a `OnceLock`. Every statement
in the process, on both doors, allocates from it.

That it is **one** pool is the correction that made it a bound at all. `bounded_session` used to build
a fresh runtime, and therefore a fresh pool, on every call — so each statement got its own gibibyte and
ten concurrent statements got ten, while the setting, its help text and the remediation plan all said
the bound was what a server's queries may use *between* them. **A pool that is not shared is not a
bound; it is a per-statement allowance wearing a bound's name, which is worse than none because it
reads as solved.** Fairness was the whole argument for a fair pool, and with a pool each there is
nothing to be fair about.

The test written to prove it did not: two sessions and two hash joins against a megabyte pass either
way, because a per-statement pool refuses each against a megabyte of its own. It asks directly now —
the two runtimes, and the pools inside them, must be the same object.

**A sort or a grouping past the bound spills; a hash join past it is refused**, because the engine's
hash join does not spill. That asymmetry is the difference between a slow query and a failed one and
is stated wherever the setting is.

**Spill goes to the operating system's temporary directory.** `DiskManagerBuilder::default()` is what
the runtime is given, and its default is the OS temp directory with a 100 GB ceiling. The architecture
requires I/O isolation to be **physical first, quota second** — the write-ahead log, query spill and
cache on separate filesystems, so a query that fills the spill volume is structurally incapable of
filling the log volume. **That separation is not built**, and until it is, `TMPDIR` is the only lever.

### 4.2 The other five

The streamed row cap (10,000), the statement deadline (thirty minutes, read once into a `OnceLock`, so
changing it needs a restart), the connection cap (1,024), and the wire message cap (16 MiB) all live on
the **PostgreSQL door**. The metrics door caps a request line at 8 KiB.

**Arrow Flight has none of them.** It streams by design, and its only bound is the shared pool.

The connection cap is not a refusal: past it the accept branch is **disabled**, so callers wait in the
kernel backlog rather than costing a descriptor. That is what makes descriptor exhaustion unreachable
rather than merely survivable, and the shipped unit's `LimitNOFILE=65535` is the other half of the
number and says so.

A deadline and a cancellation are told apart, with a `retryable` flag: a deadline may succeed with
longer to run; a cancellation is a decision somebody made. A client that cannot tell them apart cannot
decide whether to retry.

### 4.3 What does *not* bound a query, and the sentence that was false

`sankhya-governor` contains admission control that estimates from plan cardinality and queues or
refuses, a bounded queue so a refusal arrives immediately rather than after a timeout, tenant floors
and caps, a memory brake, and a five-rung pressure ladder evaluated centrally from a typed signal bus.
All of it is built and property-tested.

**None of it is called.** `admission::admit`, `assess` and `assess_memory` have no callers outside
their own crate's tests. The one governor call on the query path passes a zeroed `Request::default()`
against `Quota::generous()`, whose scan, row and storage ceilings are `u64::MAX`; `Quotas::observe` is
never called, so the concurrency ceiling can never bind either; and there is one tenant, fixed at
startup, so the tenancy dimension exists in the type system and nowhere in a deployment.

> **The whole of §8.5 of the previous version of this document was false**, and it was false in the
> most expensive direction: it described admission control, per-tenant floors and caps, a brake and a
> ladder in the present tense. An operator reading it would have believed a runaway query was bounded
> by something other than a one-gibibyte pool. The repository already said otherwise —
> `SNK-R0002`'s entry in `UNREACHABLE` reads *"tenant quotas are `sankhya-governor`, which is called
> with a zeroed request against `u64::MAX` ceilings and decides nothing"* — and the document did not.

The design is not discarded. It is §18, in Part II, where it belongs.

---

## 5. The doors — **[Built]**

Two client planes and one scrape endpoint. That is a decision, not a stage: §5.4 is why there is no
third.

### 5.1 `sankhya-api-pg` — the PostgreSQL wire protocol

The door for tools nobody wrote for this system: `psql`, a notebook's existing driver, a BI product.
No shim, no driver, no adapter. It is five modules and about three thousand lines, and no document
before this one described it.

| Module | What it holds |
|---|---|
| `crates/sankhya-api-pg/src/message.rs` | The codec. Frontend and backend messages, and PostgreSQL's real type OIDs |
| `crates/sankhya-api-pg/src/session.rs` | The protocol state machine — pure, no I/O, no engine |
| `crates/sankhya-api-pg/src/listener.rs` | The socket: accept loop, TLS handshake, backpressure, drain |
| `crates/sankhya-api-pg/src/catalog.rs` | `pg_catalog` and `information_schema` emulation |
| `crates/sankhya-api-pg/src/setting.rs` | One parser for `SET` and `RESET`, shared by the wire layer and the server |

**The framing is hand-written on purpose.** The message length is an `i32` that includes itself and
not the type byte, which is the classic trap; and the declared length is attacker-controlled, so a
client exceeding `MAX_MESSAGE_BYTES` is disconnected rather than accommodated.

**The state machine is pure, and that is the load-bearing decision.** `Startup → Handshaking →
Authenticating → Ready → Closed`, with one mutator, and a `Handler` trait as the entire seam to the
engine. The protocol layer has no session, no catalogue and no planner in it, which is what makes it
testable byte-in, byte-out — and what let three separate protocol defects be found by tests that
never started a server.

Four behaviours are worth stating because a reader would otherwise assume the opposite:

**TLS negotiation is *inside* the state machine.** A client asks in eight bytes whether encryption is
available and reads a single byte back before any handshake exists, so the decision belongs to the
same state machine that decodes everything else; only the handshake happens outside it. A GSSAPI
request is **declined out loud** for the same reason — `psql` with `gssencmode=prefer` is a default on
several Linux distributions, and a server that says nothing leaves the most ordinary client waiting.

**The extended query protocol is fully implemented** — `Parse`, `Bind`, `Describe`, `Execute`,
`Close`, `Sync` — and `Bind` parameters are *decoded*, not skipped. A missing prepared statement
answers SQLSTATE `26000`, which drivers use to re-prepare rather than to reconnect. A portal
**caches its answer**, because `Describe` and `Execute` both need a result and re-running would read
two different warehouse snapshots. The consequence is honest and unusual: **a statement executes at
`Describe` time**, earlier than real PostgreSQL. Planning without executing would need a second
planner that could disagree with the first.

**Catalogue queries are recognised by *shape*, not by exact text.** That is the difference between
*"a command-line client connects"* and *"a BI tool works"*: both spellings of the same question are
matched — `information_schema`, which JDBC uses, and `pg_catalog`, which `psql`'s `\d` uses — because
matching only one works for the client it was written against and fails for the next. Projections
preserve the order of columns the tool asked for. **An unrecognised catalogue query produces a named
error, never an empty result**, because an empty result is indistinguishable from *"you have no
tables"*.

**A refusal carries the names it cites.** `QueryFailure` has a `subjects` field
([ADR-0017](adr/0017-the-client-contract.md)), sent in the protocol's hint field, because PostgreSQL
has no list field and an unknown field type may not survive a driver. §5.5.

Not implemented, and it is a design position rather than a gap: **`COPY`**. There are no COPY messages
at all. Bulk transfer is Arrow Flight.

The version string begins `PostgreSQL 17.0` because every client parses the major version out of it
before it will proceed, and then says what this actually is so the prefix does not mislead anybody
reading it.

### 5.2 Arrow Flight SQL — the bulk plane

Arrow-native end to end, streaming by construction, carrying a result's schema without a second
description of it ([ADR-0006](adr/0006-flight-sql.md)). `sankhya-api-grpc` is the transport and
deliberately nothing else — a Flight service *is* a gRPC service, so the crate is a socket, a TLS
option and a shutdown, with no protocol in it.

It was complete and tested for a milestone with **nothing serving it**, found by widening
`check-surfaces` from *"crates that register SQL functions"* to plain reachability. That is the
clearest single argument for §2.1 existing at all.

Three asymmetries with the wire door are real and are not oversights of this document:

- It has **no authentication**, and its identity is an unverified header. [`SECURITY.md`](SECURITY.md)
  §3.4.
- It does **not** call the write refusal the wire door calls. [`SECURITY.md`](SECURITY.md) §3.5.
- It has no row cap, no deadline and no connection cap. §4.2.

Both doors present **one certificate**, loaded once by `sankhya-tls`, each naming only its own ALPN.
Two loaders would mean two sets of refusals and two answers to *"is this key the one for this
certificate?"*, and the divergence would surface on whichever door is used less.

### 5.3 One answer to a failed `accept()`

`sankhya-accept` classifies an accept error: a routine per-connection failure continues, a descriptor
shortage pauses before retrying, and anything unrecognised **stops** — the default is stop, so an
unclassified error becomes a crash with the error in it rather than a silent spin.

It is a crate rather than three `match` arms because it *was* three `match` arms, and they disagreed.
The wire door propagated the error out of `main`, so `ECONNABORTED` — which is what a load balancer
produces every time a health check opens a connection and closes it before the handshake — exited the
process. The metrics and columnar doors did the opposite: `let Ok(..) = accepted else { continue }`,
which looks safe and is a hot loop, so a descriptor shortage never cleared and a genuinely broken
listener failed the same way for ever — a process that is up, answering nothing, and reporting nothing.
**The disagreement was visible only to somebody reading all three at once.**

### 5.4 Why there is no third door

A REST/JSON API is deliberately not a third door. The engine is columnar and typed; a row-oriented
JSON surface converts twice, loses the type distinctions the type mapping spent effort preserving — a
`Decimal(38,9)` becomes a double or a string, and both are wrong in different ways — and would need its
own pagination, its own error shape and its own authorization path. That is a second product surface
maintained for ever to avoid a dependency the client already has.

What `sankhya-api-rest` actually contains is a **route table and a size decision**: which shapes exist,
and the rule that anything past a byte or row cap returns a Flight ticket rather than a body
(`crates/sankhya-api-rest/src/size.rs`). `deliver` refuses to be given a row count taken *after* the
rows exist, which is the whole point of it. Serving it needs an HTTP listener, HTTP authentication and
a pre-materialisation estimate — a feature, not hygiene, which is why it is sized rather than pending.
Its `/health` and `/ready` routes are declared there and **served by nothing**; do not point a probe at
them.

### 5.5 A refusal has to survive the wire

This system's dominant verb is refusal, and a refusal that says *"drop it first"*, *"materialise them
first"*, *"the archive is the copy"* is only useful if the words and the names reach the client. So a
refusal carries a code, a SQLSTATE, a remediation and the **subjects** it names, and the subjects
travel in a field every driver already surfaces rather than one an unknown driver may drop.

---

## 6. Storage — **[Built]**

### 6.1 The layout, and why the name is the interface

```
<warehouse_root>/
  <schema>/                    mirrors the source schema name
    <table>/                   self-contained; the unit of external readability
      _delta_log/
      sank_data_date=YYYY-MM-DD/
        <data files>
```

One name spans four naming domains — source identifier, object path, catalogue namespace, and the name
a user types — so a table's origin is identifiable without a lookup table. Because PostgreSQL folds
unquoted identifiers to lower case, for the large majority of tables all four are the **same string
with no transformation at all**.

Three properties are engineered rather than assumed. Escaping is human-legible rather than hashed, and
the separator chosen is not legal in an unquoted source identifier — **so its presence is itself a
signal that a transformation occurred.** Collisions are refused loudly at onboarding, never silently
merged; an earlier proposal to disambiguate by hash suffix was withdrawn, because it guaranteed
uniqueness by destroying the readability that was the entire point. And identity is recoverable from
the table directory alone, with no catalogue and no SANKHYA process running, because relatability must
survive the system being switched off.

The warehouse path is a **published interface**. Additive schema changes are backward-compatible and
applied automatically; a table rename is a breaking change to consumers SANKHYA cannot see and requires
a human. A column rename and a table rename therefore have opposite policies — same word, different
contracts.

A user writes `sales.orders` and never writes anything else. There is no `warehouse.sales.orders` and
deliberately never will be: putting the tier into the name would encode a *physical* fact in a
*logical* identifier, and the physical fact moves.

### 6.2 Why there is a log at all

This is the argument that decides the chapter, and it is concrete rather than philosophical.

Between a merge and the retirement of its inputs, the directory holds **both** — the file that was
written and the files it replaced, *the same rows twice*. That window lasts at least a full grace
period and exists by design (§8). So anything answering *"which files belong to this table"* by listing
the directory is wrong for the whole of it: a planner given a listing plans a merge whose inputs include
files an earlier merge already superseded, and the result contains those rows twice — **permanently**,
this time; and a reader given a listing double-counts every merged row for the duration of the window.

> **"Which files are live" is not answerable from the filesystem once compaction has run.** A directory
> of Parquet files is a storage layout; it is not a table.

The live set is therefore a first-class value carried across maintenance ticks, not derived from
storage, and the published tier names its files individually rather than pointing at a directory. Both
behaviours are tested, **including the negative one**: a query registered against the *directory*
returns the merged rows twice, while the same query against the *live set* returns them once.

### 6.3 The log, written by hand, and validated by somebody else's reader

SANKHYA emits the table log directly — a few hundred lines covering `protocol`, `metaData`, `add` and
`remove`, one JSON object per line, staged and renamed so a reader never observes a partial commit
(`crates/sankhya-table-delta/src/lib.rs`).

**A commit says how long it is.** The first line of every commit is a seal carrying the number of
actions that follow; the reader counts what it reads and refuses the commit when the two disagree. The
reason is that the alternative is indistinguishable from success: a commit body truncated by a crash
replays as a *shorter commit*, and one whose lines are all missing replays as a commit that did
nothing. Neither is an error to a reader that parses the lines it finds — and the state is cemented,
because a retry at the same version is refused as `VersionTaken`, so the next commit lands on top of
the truncated one and every `add` the crash swallowed is gone from the live set for good.

Two properties come along with the seal. Lines are parsed as generic JSON before their kind is read, so
an action from another engine is *counted and passed over* rather than turned into a parse failure that
renders the table permanently unreadable — which is the protocol's own forward-compatibility rule. And
the seal is itself an action for counting purposes, so a reader cannot satisfy the count by mistaking
the header for data.

**The kernel is a dev-dependency, used as an oracle.** It reads the log SANKHYA wrote and must agree
about the schema, the version and the live set. Keeping it test-only keeps eighty-four packages and a
duplicated HTTP client out of the shipped binary, and that it stays test-only is checked mechanically
by `cargo xtask check-features` rather than left to review.

The oracle earned its place on its first run. The log this system wrote was **invalid**: the `add`
action's `partitionValues` field is non-nullable and had been omitted. It round-tripped through
SANKHYA's own reader perfectly, because a reader ignores a field it never writes.

> **Two implementations agreeing is worth nothing when the same author wrote both sides.**

**Checkpoints.** Every ten versions the reconciled state is written as a single Parquet file with a
pointer, and readers start from it — worth roughly 10× at fifty thousand commits, and the beneficiary
is mostly *other engines*, which have no cache and start cold on every query. A checkpoint holds
exactly what replay produces, which makes it safe in a specific way: **it can always be discarded.** A
missing file, a corrupt pointer, or one left behind by a table dropped and recreated at the same path
all fall back to the log and cost a replay rather than an answer. Nothing is permitted to depend on a
checkpoint being present or even parseable, which is what makes writing the format by hand defensible
rather than reckless. Writing one is a *maintenance* job, not part of committing: a commit that had to
checkpoint could fail for a reason that does not matter.

> **Correction, and it is recent.** Checkpoints were written only by tests, so every replay in this
> system ran from version zero for the life of a warehouse. The mechanism was built; nothing called it.

### 6.4 Atomic publication, and the claim that used to be a comment

Concurrency control is the protocol's own: a writer picks the next version and **fails if somebody took
it**, and the loser rebases, because its decisions were made against a state that no longer exists.

> **Correction.** *"Fails if somebody took it"* is the property the design requires and, until M8, not
> the one the code delivered. `commit` claimed a version by checking the file was absent and then
> renaming a staging file over it — and `rename(2)` **replaces its destination silently**. Two
> committers could both see the version free, and the second would overwrite the first with no error to
> either. The rebase loop never ran, because the conflict it waits for was never returned. It went
> unseen because **every test had a single writer per version**.
> [ADR-0013](adr/0013-concurrency-and-data-safety.md) is the record.

The claim is now a link, which fails when the name is taken, and **that refusal is the whole of the
protocol's concurrency control.** It eliminates deployment targets rather than merely preferring some:
ext4, xfs, btrfs, zfs, APFS and NTFS support hard links; **FAT and exFAT do not and are not
supported.** On an object store the equivalent primitive is a conditional put, and a store that does
not offer one cannot host a warehouse safely — which is checked at startup, with multi-writer mode
**refused** rather than degraded.

**A data file name is used once.** A second write to a live name truncates rows some log still refers
to. The publisher names a file from the version it is attempting *and a per-write token* — the version
alone is shared by two writers racing for it — and the writer refuses a name that exists rather than
truncating it.

**One server per warehouse.** The version claim serialises committers at a version and nothing else.
Two servers would each retire files against a lease registry that cannot see the other's readers.

### 6.5 Statistics in the log — a recorded reversal

Bounds and null counts are written into the log alongside the row count. **This reverses an earlier
decision in this document, and the reversal is recorded rather than quietly made.**

They were withheld on the grounds that a wrong bound silently drops rows and that bounds go wrong
quietly under type coercion. That is true, and it is why every bound written comes from code that
refuses to produce one it cannot justify: an unrecognised type gets no bound, an unorderable value gets
no bound, a merge that would narrow a bound drops it instead, and a value the protocol cannot represent
exactly — a non-finite float, bytes that are not text — is omitted rather than approximated.

What the original reasoning did not weigh is the cost of withholding them. **An external engine can
prune only on what the log tells it.** Keeping bounds private to SANKHYA means every other reader scans
everything, which undercuts the reason for choosing an open format at all. The bar is higher now rather
than lower: a malformed statistic costs *other people* answers, in engines that cannot be fixed from
here.

The distinct-value estimate is a HyperLogLog sketch — 4,096 registers per column, merging by
register-wise maximum so a merged file's sketch equals the sketch of its inputs' union exactly, which
is what makes statistics maintainable at compaction with no value re-read. Accuracy was measured within
5% from 10 to 100,000 distinct values, and the sketch is reproducible across processes, because a
per-process hash seed would make two nodes disagree about a plan and the disagreement would look like
an optimiser bug. (Recorded in [`STATUS.md`](STATUS.md), §Measurements.)

**The sketch is computed at write and then discarded**, because the protocol has nowhere to put it. A
column read back from the log therefore reports **zero distinct values**. Nothing reads that figure
today; it is a trap for whatever reads it first, and it is listed rather than left to be discovered.

> **There is no quantile sketch.** An earlier version of this document described one. Exact order
> statistics exist, with three conventions named and shown to disagree at the 99th percentile — which
> is the only place anybody asks for one — but they **buffer their input**, so every observation must
> be resident. `FR-QUERY-08` asks for a bounded-memory algorithm over large inputs and this is not one.
> Exact and bounded are independent properties, and only the first is delivered.

One detail that is silent if wrong: a compaction's `remove` actions declare `dataChange: false`.
Compaction rewrites files without changing rows, and a reader streaming changes would otherwise see
every compacted row as a deletion followed by a re-insertion — a flood of spurious changes proportional
to how well maintenance is working.

### 6.6 Caching is free, and one key is not

Data files are immutable and never rewritten in place. Therefore:

> **A cache keyed by object path requires no invalidation protocol.** Entries never go stale; they only
> become unreferenced.

Correctness is free; only eviction policy remains, and eviction policy is a performance question. This
is stated explicitly because engineers who have built caches over mutable stores will otherwise design
an invalidation protocol this system does not need.

**One mutable key exists in the entire design**: the mapping from a table to its latest version. It is
named because it is the single place a stale cache produces a stale answer.

Two security requirements on cache keys, both breach mechanisms if omitted: the **policy version** must
be in the plan-cache key, or a revocation does not take effect for any query whose plan is already
cached — data served after it was forbidden, with a passing test suite; and the **evaluated entitlement
set** must be in the result-cache key. Both are constructor *arguments* rather than fields, because a
field can be left at its default and an argument has to be passed, and the keys are byte-identical
across processes and pinned so a change to the hash is deliberate.

**The result cache does not exist. Its key does**, and that ordering is the point: key correctness is a
security property, and the right time to fix it is before anything caches.

The file-set cache that *does* exist resumes rather than replays, and it **cannot go stale because it
never trusts its own version** — asking costs one filesystem probe rather than a directory listing.

### 6.7 Why the storage library is metadata-only

The released Delta and Iceberg libraries pin an Arrow generation two majors behind the query engine's.
Two Arrow majors cannot coexist in one process — identically-named types become incompatible, and the
**trait-identity problem is worse than the type problem**, since a table provider implementing one
generation's trait cannot be registered with the other generation's session at all.

Using the storage library **only for metadata** — snapshot resolution, file lists, per-file statistics,
schema and partition specification — and running scan execution on the query engine's own Parquet
machinery means **no bulk data crosses a version boundary; only small metadata structures, which
convert trivially.** The kernel-level library has no query-engine dependency and supports the current
Arrow generation behind a feature flag, so the dependency graph is internally consistent with zero
duplicate versions. `cargo xtask check-dupes` is the gate;
[ADR-0001](adr/0001-dependency-pin-set.md) is the record.

**The skew is chronic rather than transient**, so the architecture accommodates a permanently-lagging
storage library rather than waiting for a release.

Owning the provider is better than a workaround, and five things depend on it: injecting our own
distinct-value statistics into the optimiser — neither vendor provider supplies them, which is the root
cause of poor join ordering; partition-transform inversion and derived-column correlation; wiring the
Parquet reader to our own cache; ordering files by statistics so top-N queries stop early; and turning
delete vectors into a plan-time row selection rather than a post-filter. The last two are not built.

### 6.8 What is not built here

| Not built | Note |
|---|---|
| Object-store backend | Everything published goes to a local filesystem. M12. The conditional-put property it must have is written down |
| Bloom filters | Off by design where they do not pay; not built where they would. Measured neutral on TPC-H, which has no query of that shape — untested here rather than shown worthless |
| Deletion vectors, column mapping, partition values in the log | Deliberately absent. A reader requiring any of them refuses these tables, which is the correct outcome — refusing is visible, and a partially-implemented protocol feature is not |
| Multi-part and V2 checkpoints, and log cleanup | Nothing deletes the commits a checkpoint subsumes, so the log directory grows without bound |
| The result cache | §6.6 |
| The persisted cardinality sketch | §6.5 |
| File ordering by statistics for early termination | Inside the M3 work breakdown, deliberately unbuilt |
| Delete resolution into plan-time row selections | Same |

---

## 7. The date axis — **[Built on the publish path; not on the ingest path]**

**Every table carries `sank_data_date`, of type `DATE`, and is partitioned on it.** No exemption for
size or purpose. `sank_` is a reserved column-name prefix, and a source column already so named is a
collision refused at onboarding rather than silently shadowed.
[ADR-0004](adr/0004-the-date-axis.md).

Three requirements were blocked on the same absence — partitioning, time-based retention, and hot/cold
tiering — and each was individually solvable in a way that would have been wrong. Writing each per
table means writing it many times, differently, and being wrong somewhere.

**Why `DATE` and not an encoded integer**, since the integer form is the common choice: the row that
decides it is arithmetic. `20240301 - 7 = 20240294` is a plausible-looking expression that produces a
value which is not a date, raises no error anywhere in the stack, and will be written by somebody. That
is exactly the class of defect this system is organised against. The Hive-convention partition path
`sank_data_date=2024-03-01` is also what Spark and Trino parse as a date; the integer form is a string
to them, so every pruning query must know the encoding.

**A partition key with a fixed granularity is a small-file generator**, and the arithmetic is
unforgiving: the fan-out is per *batch*, not per day, so a 5,000-row append spread over ninety days
becomes ninety files of fifty-five rows. That is not hypothetical — a measured run produced 32,279 live
files across ten tables in four minutes, averaging 37 KB each, against a compaction policy targeting
256 MB. Four orders of magnitude below target, and the ordinary consequence of correct partitioning
meeting a wide batch. Two mechanisms answer it: declarable granularity (`day`, `month`, `year`; an
unrecognised value is refused rather than defaulted, because a monthly table silently becoming daily is
repartitioned on its next write — a full rewrite for a typo), and a fan-out guard on the writer.

**A partition column must be in three places** — the schema, the path, and the add action. A column
present in only one of them reads as null for every row in Spark and Trino.

**Managed tables get the column; attached tables are not altered.** `ALTER TABLE` on somebody's schema
breaks `INSERT` without column lists, changes `SELECT *`, touches ORM mappings, and may exceed the
privileges granted. The column is derived during ingest and exists on the analytical side, which is
where partitioning happens anyway.

> **The gap, stated where the capability is described rather than in a footnote.** The batch publish
> path is partitioned; **the streaming arrival path is not.** `sankhya-ingest` creates tables with no
> partition columns and writes flat. There is a sharper gap behind it: there is no timestamp on an
> ingested row to derive a date from. A commit-timestamp system column is declared on every ingested
> table and written as the literal `0` for every row — its comment says the value is recorded for human
> reading only, which it is not; it is recorded for nothing. Since no ingest runs in a server today
> (§16), this affects the path that will matter most when it does.

---

## 8. Maintenance — **[Built]**

### 8.1 Tiered compaction

Lakehouse compaction has the write-amplification shape of a log-structured merge tree, and the naive
approach is catastrophic: recompacting a whole large partition every hour while it receives a small
increment rewrites the entire partition per hour.

```
  L0   micro-batch files, arrival order, small
        │  merge many
  L1   sorted within file, medium
        │  merge several
  L2   sorted across the partition, full statistics
        │
  SEALED — never rewritten again
```

Each byte is written once at each level, giving roughly **3× total write amplification instead of two
orders of magnitude**. When a partition's newest data falls behind a watermark it is compacted once to
the top level and sealed. This bounds total compaction work to a function of data volume rather than of
volume multiplied by elapsed time.

**What it is worth was measured, and the measurement is the interesting part.** The claim is specific:
small files cost query *planning* rather than scanning, which predicts a roughly fixed penalty per
query — dominating short queries and amortising away on long ones. Over 20,000,000 rows, 400 fragments
against the single file they merge into: a short query 4.42× slower with an absolute overhead of
13.1 ms, a long full aggregation 1.24× slower with an overhead of 23.7 ms. The overhead stays within
the same order across a query doing thirty times more work while the *ratio* collapses. **Fragmentation
is an interactive-latency problem, not a throughput one.** Merging also reduced the data 2.23×, largely
through better compression across a larger block. (Recorded in [`STATUS.md`](STATUS.md),
§Measurements.)

That required scaling the fixture before it was a real test: an earlier run over 1,000,000 rows showed
4.43× and 3.77× — apparently uniform, and it would have been read as *"more files are slower"*. The
long query simply was not long enough for planning to amortise against. **A measurement that cannot
distinguish the hypothesis from its negation is not evidence.** §Measurements.

### 8.2 Compaction adds; a separate operation removes

The rule that makes frequent compaction safe is that **a merge never deletes anything.** It writes a
new file and leaves its inputs in place, so a reader holding a snapshot continues reading files that
are still there. There is no window in which a file under a reader disappears.

Deleting the inputs is a distinct operation with three preconditions, all of which must hold for a
given file:

1. **The replacement verifies.** Its row count is re-read from its footer *at retirement time*, not
   trusted from the merge, which may have completed hours earlier.
2. **No retained snapshot, lease or clone can resolve to the input.** A file a pinned reader may reach
   is kept however old it is.
3. **The grace period has elapsed.** A reader that listed files a moment before the merge is entitled
   to open them and has no way to announce that it is doing so, so the grace must exceed the longest
   query the deployment permits.

An input failing any precondition is **retained with a reason**, which is a correct outcome rather than
a failure — retirement is an optimisation and declining it costs only disk. The one case that is an
error is a missing or short replacement: that means the compaction did not happen, and nothing may be
removed at all.

Separating the two means the frequent, cheap operation carries essentially no risk and the dangerous
one runs rarely and under stricter conditions.

**Reclamation waits for readers, not for a proxy.** Readers announce themselves through an epoch-based
lease registry (`crates/sankhya-leases/src/lib.rs`), and every imprecision in it is arranged to
**delay** reclamation and never to permit it early: a reader that could not announce makes the registry
report that something is active, and a slot collision reports the older announcement. Two rules from
[`INVARIANTS.md`](TESTING.md) follow — a pin that cannot be read stops reclamation, because
contributing nothing is indistinguishable from protecting nothing; and a leaked lease does not stop
reclamation for ever, because a registry with a leak and no backstop reclaims nothing, for ever, and
says nothing about why. The backstop overrides the *lease* check only.

**Orphan collection** removes files the log has never named, and age is the only thing separating an
orphan from a file mid-commit — which is why the threshold is a week.

### 8.3 One scheduler, one budget, and a class that does not exist

Maintenance work is arbitrated against one budget across both sides of the system, by a strict class
ladder (`crates/sankhya-maintenance/src/schedule.rs`): safety, availability, performance, housekeeping,
optional. Safety and availability may **preempt a query** and ignore the duty cycle; nothing below them
does. A job that cannot checkpoint is refused rather than started. **Waiting never promotes a job out
of its class** — starvation is counted and reported, never fixed by ageing, because ageing is how a
housekeeping job comes to preempt a query.

> **The maintenance scheduler is structurally incapable of destroying retained history.** There is no
> erasure class to configure: the guard is on the type, and an exhaustive match makes adding a variant
> a compile error. Erasure is not a priority level of expiry; it is a different job class with a
> different authorization path. Anything less and a misconfigured retention default eventually deletes
> records that were legally required to persist.

Clustering is **declared, never inferred**: the engine cannot tell a meaningful query boundary from a
merely low-cardinality column, and guessing sorts a table for queries nobody runs, at every compaction,
for ever. A partition still receiving writes is merged **without** sorting, because ordering it produces
a layout correct until the next append at the cost of a full sort every pass.

Sorting is what turns row-group bounds into an index. Measured on a range query at eight clients, the
same data sorted by the filtered column answered in 31 ms against 234 ms unsorted — **7.8×**, entirely
from row groups skipped on their statistics before any decoding, and the difference between missing an
objective and meeting it. It was also the **only one of that objective's three named preconditions
that turned out to matter**, which is why it is recorded rather than folded into a list.
(Recorded in [`STATUS.md`](STATUS.md), §Clustering, worth 7.8× on a range scan.)

Operationally, all of this is three settings and a `SIGHUP`: [`OPERATIONS.md`](OPERATIONS.md) §10.

---

## 9. Multidimensional analysis — **[Built]**

SQL's `GROUP BY CUBE` and `ROLLUP` are *grouping constructs*: they enumerate combinations of the columns
you name. There is no dimension, no hierarchy, no declared measure, no consolidation rule and no notion
of a member. The operations people actually perform — take this slice, dice it by those two dimensions,
roll it up that hierarchy, drill into the outlier — are **navigation of one structure**, and a system
that cannot represent the structure makes the user reconstruct it in a client, which is how the work
ends up in a spreadsheet nobody can reconcile. [ADR-0007](adr/0007-the-cube-model.md).

Three properties SANKHYA already had are the three a cube engine most needs: parent-child hierarchies
are graphs and **a consolidation path *is* a traversal**, so hierarchy walking is the general case that
already exists rather than a special case reimplemented; consolidation is a large floating-point
reduction, which is the reduction at its worst and the first thing a finance function asks about; and
every table already carries a date axis, so a time dimension exists before anybody declares one.

### 9.1 The rule that decides whether an answer is correct

> **A measure with no declared aggregation rule is refused at definition time. Not defaulted to `SUM`.**

The default is wrong for an entire class of measures and wrong *invisibly*. Twelve monthly closing
balances summed across time give a figure with the right magnitude for a balance-sheet line, the right
sign, four significant figures, and no meaning whatsoever — it reconciles against nothing, because
nobody reconciles a subtotal.

The precise statement separates two things a loose one runs together. **Additive** is about the
*operator*: may this measure be summed along this axis? **Composable** is about *permission*: can the
whole be built from partial aggregates along this axis at all? A closing balance is not additive over
time and it **is** composable over time, because `last(last(a, b), c) == last(a, b, c)` given an order.
An earlier version of this design said a semi-additive measure was *worse* than a non-additive one, and
taking that literally cost a set of tests that refused *valid* roll-ups.

> The danger of a semi-additive measure is **the operator, not the axis**, and it lives in the executor.
> So the operator is the *measure's*, never the caller's: a roll-up reduces eagerly, under the rule the
> measure declares for the dimension being rolled away, and each cell of the result holds one value.
> There is nothing left for a later call to reduce differently. The exit criterion asks that summing a
> semi-additive measure across time be *rejected at planning time*. **It is not rejected — it is not
> expressible.**

Underneath that is a trap that survives testing. `FIRST` and `LAST` name a *position*, which is
meaningless over an unordered bag; the obvious implementation takes contributions in visit order, which
for a sorted address map is lexicographic by member name. `"feb" < "jan"`, so the closing balance of
the first quarter is January's. ISO-8601 dates sort correctly, so a system tested with `2026-01` never
exhibits it and the first wrong number appears against member names somebody chose for a report. The
order is a **parameter**, and a `FIRST` or `LAST` roll-up without one is refused.

### 9.2 Completeness is a column, not metadata

Row filtering closes the direct disclosure channel. It does not close the arithmetic one, and the
arithmetic one is invisible: an aggregate computed over rows a caller may not read is a real number,
correctly calculated, disclosing information about rows that were withheld — with no refusal to notice
and nothing in an audit log to find.

So every cube answer states how much of its input it saw. `completeness` and `withheld` are **columns
on the result row**, not metadata beside it, because metadata beside a result is dropped by the first
projection that does not mention it, and a filtered total then looks exactly like a complete one. Two
callers with different permissions ask the same question, correctly get different totals, and each can
*see* that they did. ([ADR-0008](adr/0008-serving-cubes-under-policy.md).)

The second half: **a stored cuboid may only serve a caller it was computed for.** The cache key for a
materialised aggregate includes the caller's visible scope, and two scopes are two *tables* — so a bug
in the lookup cannot serve one principal's rows to another, because the rows are not in the file being
read.

The operational consequence is worth knowing rather than discovering. A background refresh has no
principal, so it builds the **unrestricted** cuboid, which may serve only a caller whose own policy
withholds nothing. **Background materialisation helps dashboards and service accounts and does nothing
for a restricted analyst**, whose cuboids can only be built by their own queries. Pre-building named
scopes is a decision nobody has made and is not taken by implication.

### 9.3 Three crates, three lifetimes, and a cache that is not a copy

The engine has the same shape as the graph engine and for the same reason: `sankhya-cube-algo` at layer
1 holds the algebra and depends on **nothing**, which is what makes its property tests fast enough to
run thousands of cases on every build; `sankhya-cube` holds construction, hydration and the
specification; `sankhya-cube-sql` holds the table functions.

**Materialisation is a cache, not a second copy of the truth**, and it cannot be stale: a cuboid's key
includes the data version and the definition, so a definition edited under a cached cuboid produces a
miss rather than an answer from the old shape. Three lifetimes — ephemeral, session, maintained — with
the collection each one needs ([ADR-0009](adr/0009-the-cube-lifecycle.md)).

### 9.4 What is not there

Cuboids are pre-built only for the unrestricted scope (§9.2). The ephemeral lifetime is M14. A cube
definition is a JSON file under `_cubes/` rather than a row in a system table, because the
catalogue-backed form waits on the catalogue proper. The hydration trigger scans statement text for cube
function names — deliberately crude, because a false positive costs a cache lookup and a false negative
costs a query that fails to resolve a cube it named. Pivot and hierarchy drill exist in the library layer
and are not registered as table functions. **MDX is deliberately not planned.** Automatic rewriting of
arbitrary queries onto cuboids is not built: explicit addressing only, revisited after 1.0.

---

## 10. The graph engine — **[Built; one exit criterion carried unmet]**

**There is no graph write path.** An epoch is hydrated by scanning published tables and records the
snapshot it was built from. Four consequences follow, and each answers a question a graph database has
to keep answering: the graph cannot be behind the tables in a way the snapshot does not record; the
graph and SQL cannot disagree about an entity, because an edge exists because a row exists; its
durability contract is that it has none and needs none; and on restart it is rebuilt, because derived
state never blocks shutdown.

The cost is real — no graph writes, no persistent graph-native indexes, and a rebuild after restart —
and the trade is that the class of defect a graph store most often produces, the relational half seeing
an entity the graph half does not, is **unrepresentable** rather than defended against.

`sankhya-graph-algo` has **zero dependencies at all**. Nothing in it allocates a graph: every algorithm
takes an adjacency by reference. The adjacency is typed vertices and typed edges with per-edge-type
adjacency in **both** directions and half-open validity intervals stored sorted by source and by time,
so *"the edges of this vertex as of time t"* is a binary search plus a contiguous slice rather than a
filter over everything.

**The graph carries one number per edge**, and that is a limit rather than an omission: an ownership
percentage or a haircut, not both. Amounts belong in the tables and are joined to the traversal result,
which keeps the traversal a traversal and the arithmetic in the engine that has completeness and
additivity rules.

**Time-respecting traversal is a separate function, not a flag**, and the argument is asymmetric rather
than aesthetic:

> Static reachability over a temporal graph **over-reports** — it finds routes that time forbids — and
> *always in that direction*. A flag defaulting to off would hand the optimistic answer to everyone who
> forgot it, and **the optimistic answer looks exactly like the correct one.**

Two further bounds a naive temporal traversal omits travel with it. A conservation bound requires each
onward edge to carry at least some fraction of the one before, without which a large transfer chaining
onto a negligible one is reported as a route. A dwell bound caps how long a path may pause at a vertex,
without which two unrelated events years apart join into one path and the resulting chain is an artefact
of the data set's length rather than of anything that happened.

**Every bound travels on the row.** `epoch`, `snapshot`, `truncated` and `truncation_reason` are
columns, because a flag beside the result gets dropped by the first projection that does not mention it
— **and a short list looks exactly like a short answer.** Every algorithm is bounded by construction and
reports its own truncation through a common wrapper, which is why *bounded* is a property of the crate
rather than a convention each function observes. The cube's completeness columns were built on this
precedent, with the argument strengthened, because a partial *total* is worse than a partial *list*.

### What is not built

**A measured benchmark against a public suite** is the one exit criterion carried forward as **unmet**
rather than reinterpreted. The primitives are correct against brute force and bounded by construction;
memory per vertex and per edge is published and measured from a real epoch rather than estimated from
type sizes — but throughput is not timed at scale. It would have been easy to reinterpret the criterion,
and the milestone would have closed clean. It is carried as unmet instead, which is what an exit
criterion is for.

Also unbuilt: a timer driving hydration; per-tenant epochs *through the front door*, which are built and
tested and reach no door; the pack bundle loader in a running server; and the structured graph API on the
control plane. A durable graph database is not a gap but a non-goal, and a bespoke query language is not
planned — a structured API and SQL functions now, the ISO standard later as a rewrite onto those
functions rather than a second engine.

---

## 11. Cloning, lineage and dependents — **[Built]**

```sql
CREATE TABLE q3_frozen CLONE sales.orders;
SHOW LINEAGE OF q3_frozen;
SHOW DEPENDENTS OF sales.orders;
```

A clone reads exactly what its origin read at a version, in constant time and constant space, by
referencing the same files rather than copying them. It lands beside its origin; naming another schema
is refused, because the right to read a clone derives from the right to read what it references — a
clone under another schema would have its *name* governed by one policy and its *data* by another.

### The premise it breaks

Three mechanisms decide that a file may be removed — retirement, orphan collection, purge — and **each
consults one table's log**, correct today for the same reason.

> **A file belongs to exactly one table.** Under cloning that premise is false, and each of the three
> becomes a way to delete data a clone is the only remaining reader of.

Orphan collection is the most dangerous, and it is worth walking. The sweeper lists files under one
table's root, builds `named` from that table's live set, and passes `reachable` as an empty set. Clone
a table; the clone's log names the origin's files; the files stay under the origin's root. A week later
the origin's sweeper runs: the file is listed, it is not `named` because the origin has compacted past
it, it is not `reachable`, and it is older than the threshold. **Removed.** Nothing failed. No query
errored. The clone is missing rows, and the first evidence arrives whenever somebody next reads that
range of it.

> **From the origin's point of view, a file only the clone still names is indistinguishable from
> debris.** That is the whole hazard in one sentence, and it is why this feature was design-gated: no
> code was written before [ADR-0016](adr/0016-zero-copy-cloning.md) was accepted.

### The mechanism, and the two refused

`reachable` becomes the union of the live sets of every table in the **clone family** — the transitive
closure of the origin and everything cloned from it, walked through a lineage record each clone writes
at creation.

**Not reference counting.** It is exact, and exact in the way that matters least: a count is derived
state maintained across clone, drop, compaction, retirement and crash, and derived state that disagrees
with reality is the failure this project keeps finding elsewhere. The asymmetry settles it — a count
that drifts high loses disk; a count that drifts low deletes a file a clone still reads, **silently, in
a table nobody was touching.** The second is the exact sentence the design gate exists to prevent.

**Not copy-on-maintenance.** It is simple, it is safe, and it *quietly* gives up the constant-space
property that motivated the feature. The word doing the work is *quietly*: a clone's cost would depend
on maintenance activity its owner cannot see — clone a quiet table and pay nothing, clone one that
compacts tonight and pay for the whole table by morning. Refusing the feature outright would be more
honest.

The cost of reachability is bounded three ways: the scan is over the clone family, so it is a **no-op
for every table that has never been cloned**; reading *n* logs is what the sweep already does, *n*
times; and execution is on one node. And it fails in the safe direction — a stale or unreadable lineage
record makes the reachable set *larger*, so a file is kept that could have been reclaimed.

### A clone's log names none of the origin's files

The two obvious ways to name a foreign file are both worse than they look. An absolute URI embeds a
filesystem path, so **restore into a different directory silently produces a table whose files are all
missing** — and restore-to-a-different-path is an operation this system has. A `../`-relative path bets
on every reader resolving `..` the same way, which the specification leaves undefined.

So the clone's log names **none** of them. It records its origin and version as properties and contains
only the files it writes afterwards; a read splices the origin's live set at that version with the
clone's own log. This makes the lifetime question *simpler*: the origin's sweeper asks *"which versions
of me does a clone still read?"*, which is a question about its own log.

**What it costs, stated plainly: a foreign reader pointed at a clone's directory sees only the files the
clone wrote, not the rows it inherited.** The open-storage claim holds for ordinary tables and **not for
clones**. A clone that must travel is *materialised*, which produces an ordinary self-contained table.

> **The decision also cost a read path, and the document that made it failed to say so.** If a clone's
> log named the origin's files, the existing read path would have served a clone with no changes at all.
> Deciding that it names none of them means *this* engine must splice too — and until that existed, **a
> clone was a table that read as empty.** The omission is recorded rather than quietly fixed: the
> decision was argued on portability and on the lifetime question, both of which it wins; the cost it
> did not name was a piece of work. *A decision whose costs are listed incompletely is one somebody
> re-reads and mis-weighs.*

Purge **refuses** on a clone-referenced table rather than adapting, and the refusal names the clones. A
backup is taken at warehouse scope and records lineage, and **restoring a clone without its origin is
refused** rather than restored into a table with missing files (§11 of [`OPERATIONS.md`](OPERATIONS.md)).

---

## 12. Security architecture — **[Built; posture in `SECURITY.md`]**

The mechanism is §3.3 and the argument for its shape is there. What belongs in an architecture document
beyond that is four structural facts and a pointer.

**Enforcement happens once, at plan construction**, not three times in three engines. The graph tier
resolves through the same catalogue, so an edge a tenant may not see is never materialised into that
tenant's epoch — it is not filtered out of the traversal, it is absent from the adjacency the traversal
walks.

**A name in a statement is not a path.** Three statement families built a file path from a name a client
typed, constrained only to be non-empty and free of whitespace — and `Path::join` replaces the entire
path when the component is absolute. The worst target is a snapshot document, and not because a snapshot
is precious: **an absent snapshot pins nothing, so deleting one releases the files the sweeper was
holding back** — the deletion the whole retention mechanism exists to prevent, reached through the name
of a `DROP`. A name that may become part of a path is checked in one place now
(`crates/sankhya-atomicfs/src/lib.rs`) and the path builders return a `Result` rather than a `PathBuf`.
The rule is an **allow-list** — letters, digits, `_`, `-`, `.`, ASCII only — because a deny-list has no
end and saying what a name *may* contain is one line and has no tail.

> **Reasoning that a path *cannot* escape is not a control.** It is a comment that was true when it was
> written, attached to code somebody else will change.

**The external-reader boundary is a hole, and it is documented rather than obscured.** External engines
reading the published warehouse directly bypass row- and column-level enforcement entirely, because they
are reading Parquet with no SANKHYA process in the path. This is a product decision — open storage is
what makes SANKHYA a participant in a data estate rather than a replacement for one — and the
compensating controls are storage-level. **A security model with an unmentioned hole is worse than one
with a documented boundary.**

**Personal data is handled by design rather than by deletion.** Direct identifiers live only in the
transactional store with surrogate keys downstream, so an erasure request becomes a transactional delete
plus a vault purge and leaves analytical history, time travel and retention untouched. Where an
identifier must exist downstream, per-subject encryption keys permit cryptographic erasure. The envelope
encryption that would carry that is built and tested and **has no path through the front door**.

Everything else — what ships on by default, what a policy file may say, what needs a restart, and the
five findings a reviewer needs together — is [`SECURITY.md`](SECURITY.md).

---

## 13. Consistency, determinism and failure — **[Mixed; marked per row]**

### 13.1 Read modes

| Mode | Sees | State |
|---|---|---|
| Pinned snapshot | Published tables only, at a stated version | Built |
| Fresh | Published, plus the arrival tier where one exists | The splice is built; **no arrival tier runs** (§3.8) |
| Strong | The transactional tier | **Not built** — no transactional tier is wired in |

**Pinned reads exclude the arrival tier by definition, which is exactly why they are deterministic and
replayable.** Reproducible outputs must use pinned mode, and they are reproducible *because* of the
exclusion. That is a definition rather than an optimisation.

Read-your-own-writes is a property of the arrival tier and therefore does not hold today: a write is
visible analytically once it is published.

### 13.2 Determinism

A deterministic mode fixes the clock, seeds identifier generation, sorts listings and pins reduction
order, such that:

> The same scenario run twice produces byte-identical committed metadata and byte-identical query
> output.

One test, enormous coverage: it detects hash iteration order leaking into results, wall-clock creeping
into metadata, unsorted directory listings, and non-deterministic parallel reduction. It is only
possible because clock and identifier generation are injected seams, which is why that decision is
mandatory rather than stylistic.

Determinism also constrains arithmetic. Every vector reduction is bit-deterministic under permutation,
asserted by a test whose fixture is itself proven adversarial — a naive sum fails on it. LU
factorisation breaks pivot ties on the lowest row index, without which two builds could factor
differently. 

> **Correction.** QR, SVD and eigendecomposition **ship**, and this
sentence used to say they were deliberately absent. The refusal was real when written — an
in-house SVD that is subtly wrong produces plausible singular values, which is worse than none
— and it was lifted rather than forgotten: `crates/sankhya-math/src/decompose.rs` implements
them by Jacobi rotation on symmetric input, refusing a non-symmetric matrix rather than
symmetrising it, and they are registered as `mat_qr_q`, `mat_qr_r`, `mat_singular_values`,
`mat_cholesky`, `mat_eigenvalues` and `mat_eigenvectors`. What was not done was retracting the
refusal in the eight places that stated it. **A stated refusal silently reversed is the worst
class of claim in this repository**, because a refusal is the one thing a reader is entitled to
treat as permanent.

### 13.3 Where the two engines disagree, and the one that is dangerous

The system presents one copy of the data through two engines, so the same question asked of the
transactional tier and of the analytical tier is expected to get the same answer. It usually does. **That
is what makes the exceptions dangerous: nobody re-checks a figure that has agreed a thousand times.**

The differences are enumerated in a test that runs both engines and pins the agreements as well as the
divergences — a list of differences is only trustworthy if somebody checked the rest, and without the
agreements pinned a *new* divergence is a discovery later rather than a failure now.

**Three of them return a wrong number rather than an error.**

| | Transactional | Analytical |
|---|---|---|
| Summing past a 64-bit integer | exact, widened accumulator | **wraps to a large negative** |
| Multiplying past a 64-bit integer | refuses | **returns zero** |
| Summing decimals past 38 digits | exact | **loses exactness** |

The third contradicts a stated principle. Fixed-point decimal is used *because* money must be exact, and
on overflow the analytical tier returns a number close to the right one instead of refusing. An error is
recoverable; a plausible wrong number in a report is not.

> **This is a real limitation of the current design, not a note about an edge case.** The tier that
> exists to answer questions about money can answer one wrongly, silently, and the tier of record would
> have refused the same question.

**The mitigation is predictive rather than detective**, because detection is not on offer: by the time
the wrong number exists it is already in a result set. The statistics catalogue bounds the total from
the column's range and row count and reports whether an overflow is *possible*, erring toward
"possible" — a false alarm costs a refused query, a missed one costs a wrong figure nobody notices. Its
limit is that bounds are held as 64-bit integers, so a decimal column beyond about nineteen digits has
no representable bound and the check answers *"unknown"*. **The columns most able to overflow a 38-digit
decimal are exactly the ones it cannot reason about.** Widening the bound type closes it.

Three further differences change precision or ordering without making a figure wrong: `avg` over integers
is arbitrary-precision against a 64-bit float; division to a repeating fraction gives twenty significant
digits against sixteen; and text orders by the database's collation against byte order, so a paged or
ranked result over text appears in a different order in the two tiers.

### 13.4 Shutdown ordering

The drain order is a correctness property and is specified normatively: report not-ready while liveness
stays healthy; stop accepting new queries and let in-flight ones run to their deadline; stop the capture
source but **finish applying the in-flight batch**, rolling a partial batch back entirely rather than
half-committing it; **persist the applied position strictly after the commit is durable**, which
ordering *is* the exactly-once guarantee; flush and close writers and release leases; drop graph epochs,
because derived state never blocks shutdown; stop the database gracefully; flush telemetry, because an
unflushed exporter loses the traces of the incident being debugged.

Steps three, four, seven and eight have no subsystem in this build. What runs is: stop accepting, drain
in-flight connections to a bounded deadline, then shut the metrics listener down after the wire door so
the last scrape completes.

**The drain has to be bounded and it has to exist.** An unbounded drain hangs a shutdown on one stuck
client until the orchestrator's patience runs out and kills the process anyway, with the difference that
nobody chose the moment. And a shutdown that does not wait at all cannot be given a correct grace,
because there is nothing to wait for — it abandons work instantly, which reads as fast and is the
failure the grace exists to prevent. That was the state of this server until M6, and the doc comment
above the function described behaviour it did not have.

> Two numbers decide whether a shutdown is orderly and **they live apart**: how long the server needs to
> finish work already in flight, and how long the orchestrator will wait before `SIGKILL`. They are
> edited by different people, in different files, for different reasons — and when the second is the
> shorter, every deploy severs connections mid-result and clients see something indistinguishable from a
> crash. So the relationship is **checked mechanically**: `xtask/src/package.rs` reads the drain
> deadline out of `crates/sankhya-api-pg/src/listener.rs` and compares it against every deployment
> manifest's grace.

That comparison was correct and exercised by nothing until a mutation shortening the Kubernetes grace
below the drain **survived**: it lived only in a command, and a check that is only a command is a check
that is only sometimes made. It is a test as well now.

### 13.5 The failure model

| Failure | Behaviour | State |
|---|---|---|
| Compaction interrupted | Resumes from checkpoint; at worst unreferenced files, reclaimed after an age threshold | Built |
| Compaction conflicts with a concurrent committer | Compaction rebases and retries; **the applier never backs off** | Built |
| Storage lacks a conditional write or `link(2)` | Detected at startup; multi-writer mode **refused** | Built |
| Applier crash mid-batch | Batch rolled back; resume from the last durable position; idempotent replay | Built and tested; no applier runs |
| Slot invalidated | Gap marker recorded; automatic re-snapshot; stale data served with explicit provenance | Designed; §16 |
| Incompatible schema change | Table quarantined; last consistent version stays queryable; events dead-lettered so the cursor advances | Built and tested; no applier runs |
| Restored backup resurrects purged rows | Hot extent wins; inconsistency flagged; unified queries on that table refused until resolved | Designed; §20 |
| Node loss (executor) | Transparent; stateless | Designed; M12 |
| Node loss (coordinator) | Election; database failover | **Not built**, M12 |

---

## 14. Observability, and the two things it is easy to get wrong — **[Built]**

The mechanics are [`OPERATIONS.md`](OPERATIONS.md) §8 and [`METRICS.md`](METRICS.md). What is
architectural is two decisions.

**The catalogue is the API, and it is checked twice.** Recording a metric takes the metric's
*declaration* — meaning, unit, group, cardinality bound — so an undeclared metric is not refused at
runtime, it cannot be typed. That [`METRICS.md`](METRICS.md) matches the declarations is one check.
That every declared metric is **recorded somewhere in the source** is the other.

> Generating documentation from a catalogue proves the document matches the catalogue. It says nothing
> about whether the catalogue matches the program. Both checks are needed and only the second is
> uncomfortable, because it is the one that fails.

**A label is one of exactly two things**: a closed set of permitted values, or a deployment-scoped
identifier under a cap. There is deliberately no third variant, so a label that varies per row, per
query or per user has no way to be declared — putting a value where a dimension belongs is
simultaneously the tenant-data leak and the cardinality explosion, and one construct prevents both. Past
a cap, new series are **refused and counted** rather than created: a gap gets noticed and a quiet
inaccuracy does not.

The same argument extends to logs, where it is easier to break by accident. `#[instrument]` records
*every argument of the function it decorates*, so three words on `fn query(&self, sql: &str)` put every
statement any client sends into the log — predicate values included — with nothing at the call site
saying so. `cargo xtask check-logging` closes that, and there is deliberately **no suppression
comment**: a prohibition with an escape hatch is a prohibition with escapes in it.

### The diagnostic reports a time, which forces it to keep a history

`FR-OPS-17` asks for **time until a problem becomes user-visible** rather than a current value. The
architectural consequence is the part that is not in the requirement and is easy to build around: **a
time cannot be computed from one sample.** It needs a rate; a rate needs observations separated in time;
and those need somewhere to live between runs. A diagnostic that computes projections beautifully and
keeps no history satisfies the requirement in code and never once in operation, because every run is the
first run.

So the diagnostic owns a small append-only observation history, and three properties of it are
deliberate. **It is beside the warehouse, not inside it**, because the warehouse is the thing being
diagnosed and may itself be the finding. **It is not a table in this system**, because a diagnostic that
needs a healthy database to report an unhealthy one is decoration — which is also why `doctor` reads the
warehouse directly rather than starting the server. And **it is text, and damage is expected**: a
process killed mid-append leaves a torn line, which is skipped and *counted*, and the count is reported;
refusing to start over a truncated line would remove the tool at the moment somebody reaches for it, and
hiding the count would let a history quietly losing half its lines still produce confident dates.

**`Unknown` is a first-class outcome**, and **findings are ordered by *when*, not by severity** —
severity orders a list by how loudly each item shouts, time orders it by which must be dealt with first,
and an operator reading top-down should be reading a schedule. **"Could not run" is structurally
separate from "found nothing", including in the exit status**, because a monitoring system that treats
*"I could not look"* as *"nothing found"* reports all-clear for a subsystem nobody examined.

### Backup: three architectural rules

The procedure is [`OPERATIONS.md`](OPERATIONS.md) §11. Three rules are design rather than operation.

**There are two positions, and a manifest recording one has recorded the wrong one.**
`source_restores_to` is where the transactional store lands; `queryable_at` is the highest position at
which *every* table is complete — the minimum over their coverage, because a query joining two tables
can only be answered where both reach. They are rarely equal, and the difference is not noise: it is how
much re-capture a restore implies. The rule enforced when the manifest is **built** is that no table may
cover a position past where the source restores to, because afterwards it is **not detectable from
either side alone**.

**A drill reads the data back, because presence checks pass on the failures that happen.** A
file-presence check passes on a truncated Parquet, and on a file whose bytes were replaced with another
table's. What goes wrong with a backup is almost never that a file is missing — a missing file is loud.
Both sides compute the digest through **one** implementation, because two would eventually differ on a
null convention and every drill would then fail on data that is perfectly fine, and after the third
false alarm the drills would stop being run.

**Evidence that omits failures is not evidence.** The drill record is append-only and a failure is
written with the same ceremony as a pass. A history with no failures across three years describes either
a very good system or a drill that does not really run, and nothing in the history distinguishes them.
*"When did we last prove we could restore?"* is answered with the last **pass**, never the last attempt.

**Expiry and removal are separate, and the gap is the point.** Deleting a backup does not release the
snapshots it protects; a grace period follows. It costs storage that could have been reclaimed sooner and
buys a window in which a mistake is still a mistake. **That mechanism is library code with no caller in
the server** — [`OPERATIONS.md`](OPERATIONS.md) §11.1 says so where an operator will meet it.

### A platform baseline nobody checks is a baseline nobody meets

Bundled database binaries are dynamically linked, so a fully static artifact is not achievable and the
alternative is a **declared platform baseline** — the oldest system the artifact runs on, as a maximum
symbol version and a set of shared objects.

The declaration is not the interesting part. **The check is.** A binary built on a current distribution
silently acquires symbol-version requirements from it; the symbols are present locally, so it links, runs
and tests clean, and the failure appears the first time somebody on an enterprise distribution starts it.
**Nothing on the build machine can surface this by construction** — the machine is the reason it happens.
So the check reads what the binary *requires* rather than what the build intended, and fails only on a
release build, because a check that fails every local build is a check everybody learns to ignore.

It is currently **not met** by the shipped container image, by two glibc versions, and the gap is written
down and printed rather than rounded off. [`OPERATIONS.md`](OPERATIONS.md) §3.2.

---

# Part II — Designed, and not running

## 15. Why this part exists at all

Everything below is a decision that is expensive to change after the thing exists, taken before it does.
That is a defensible reason to write a design down and an indefensible reason to write it in the present
tense, which is what the previous version of this document did.

Each section states what has to exist first. Where the repository already tracks that dependency —
`UNREACHED`, `UNREACHABLE`, or a milestone in [`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md) — the
tracker is named, so this document cannot drift into claiming something is close when the build says it
is not.

---

## 16. Capture and ingest — **[Designed; the correctness contracts are built and nothing drives them]**

```
  PostgreSQL WAL ── START_REPLICATION … LOGICAL ──▶
  transport (vendored, behind a trait) ── bytes ──▶
  decoder (ours, pure, fuzzed) ── events ──▶
  apply planner (ours, pure) ──▶ arrival buffer + landing writer (append-only)
```

**The decoder is ours.** The transport may be vendored — the available crates are young and thinly
maintained — but the decoder parses untrusted bytes from a network socket and is therefore both the
largest attack surface and the most correctness-critical component in the path. It lives in a pure crate
at layer 0, is property-tested for round-trip fidelity, and is continuously fuzzed. Neither mainstream
Rust PostgreSQL client offers replication support, so this was never optional.

**The apply planner is pure**: decoded event stream in, table mutation plan out. That seam is the
highest-leverage testability decision in the design, and the reason is arithmetic — it allows thousands
of randomised crash and interleaving scenarios to run in milliseconds against an in-memory table, where
one integration test per scenario against a real database buys single-digit coverage per minute.

**The transaction invariant.** A batch is a set of **whole** transactions; a transaction is sealed only
by its commit, and an in-flight one is carried forward. Splitting one publishes half a transaction, which
for any multi-table write is a torn read no downstream consumer could detect. It has two counterparts
elsewhere: a batch spanning partitions becomes several files in one commit, and a source transaction
carries one commit position so a single target position includes all of it or none.

**Exactly-once is a composition**: delivery is at-least-once, application is idempotent. **Idempotence is
per row, not per batch** — suppressing duplicates per batch is the same defect one layer down as
declaring buffer coverage per segment (§17), and this system has made it once already.

**Reconciliation turns "zero data loss" into a number**: rows are digested independently on both sides
and compared, rather than asserted. The harness is deliberately not the pipeline's own code, for the same
reason the Delta kernel is an oracle (§6.3).

What is built and tested: the decoder against captured bytes and against single-byte corruption; the
transaction property under randomised interleavings; type round-tripping across 73 real columns; naming
and collision refusal; onboarding from the stream alone; reconciliation at 3,000 rows with no
discrepancies; capture at 1,000,000 rows across ten tables at roughly 285k rows/s with every table
reconciling; the five-rung source-safety ladder validated against a real slot; and crash replay filtered
per row at every crash point.

**What does not exist**: the streaming transport (and neither mainstream Rust client supports the
replication protocol, so it is real work rather than wiring); the slot lifecycle driver; the backfill
reader, so **only changes after a slot exists would be captured**; partitioning on the ingest path (§7);
and anything that drives ingest in a running server. `sankhya-cdc-pg` is on `UNREACHED` with M2's
remainder named against it.

> The honest summary, and it applies to the whole of this section: **the correctness contracts are built
> and tested, and the machinery that runs them continuously is not.** Every capability above is exercised
> by the test suite; none is exercised by a process you can start.

---

## 17. The arrival buffer and the splice — **[Designed; the splice is built and not in the server's read path]**

### 17.1 The problem, with the arithmetic

Committing more often produces small files, inflates version counts and grows metadata, and the cost
lands on **planning** — a fixed price paid before a single row is read. At one commit per second
snapshot resolution begins to cost; at ten, metadata dominates and planning becomes the tail latency. The
impact is inversely proportional to query size, which is what makes it insidious: **a high commit rate is
a tax that is invisible on the queries nobody watches and severe on the queries everybody watches.**

> **Freshness is a read-path property, not a write-path property.** The commit interval is tuned for
> storage efficiency; freshness comes from an in-memory tier spliced in at query time. That single
> inversion is what would let the backpressure ladder lengthen the commit interval under load without the
> system becoming stale.

### 17.2 The splice, and its correctness rule

> The planner selects, for each table, a set of tiers whose coverage intervals are **contiguous,
> non-overlapping, and collectively cover `[0, target]`.**

Because every committed snapshot records the exact log position it contains — the same metadata that
provides exactly-once semantics — and every buffer epoch records its range, the boundary is **exact
rather than approximate**. Intervals are half-open at the start, so adjacent tiers abut exactly.

Three properties follow, and the third is the one that makes it safe for a ledger: no double counting and
no gaps, by construction; provenance is exact, so every response reports which tiers served it and over
which intervals; and **transactional atomicity survives the splice**, because one target position is
applied to every tier and every table in the request. That third property is the reason the axis is log
position rather than wall-clock time — **a design splicing on wall-clock time could show one leg of a
transaction without the other.**

If a required interval cannot be covered, the planner **fails the query with a typed error**. It never
returns a partial answer. *An incomplete answer that looks complete is the worst outcome the system can
produce, because nothing downstream can detect it.*

Merge strategy is selected by **declared table capability, never by heuristic**: union for append-only,
latest-version-per-key for mutable, shaped as an anti-join because the buffer is tiny relative to
published data. Inferring *"this table looks append-only because no update has arrived yet"* is correct
until the first update, at which point every query silently starts double-counting — which was a real
defect, and the reason the suite never caught it is that every test reading captured data used an
append-only fixture.

### 17.3 Retention, not eviction

> **A segment may be released only once a durable tier covers it.** Not when it is old, not when memory
> is tight, not when it has been read.

This inverts the usual cache relationship, and the inversion is the point: a cache evicts under pressure
and takes a miss, and this tier has nothing to miss *to* until publication has happened. Evicting under
pressure does not degrade an answer — it destroys one, and releasing from the middle of the interval
opens a coverage gap the splice cannot be talked into approximating.

So when memory runs short and nothing is releasable, the only correct response is to push back on
ingest, reported as a distinct condition rather than absorbed, because it is a **publication problem
wearing a memory problem's clothes**: the tier is full because publication has stalled, and adding memory
treats the symptom.

**Coverage is trimmed; data is not.** The buffer retains segments the published tier already covers but
*declares* coverage from the durable frontier, so the two tiers abut exactly. The consequence is that a
scan must filter **per row**, not per segment — a segment straddling the frontier is half durable and
half not, and returning it whole would double-count its durable half.

### 17.4 What is built, and what the gap is

The coverage contract, the per-row filter at the frontier, the release rule and the exact-cover proof are
built and property-tested, including the negative cases: a genuine gap, a target past every tier, and a
tier that started mid-stream. Two defects worth carrying were found there — a tier that declared coverage
from the durable frontier rather than from its own oldest segment (so the splice found an exact cover
that did not exist), and the property test written to catch that defect, whose generator made the two
values incapable of diverging and whose assertion encoded the buggy expectation.

**What is missing**: the epoch ring, the per-epoch key digests that let a historical query skip the tier
at no cost, per-tenant sub-caps, and any wiring into an ingest path. And, per §3.8, **the splice is not in
the server's read path**: the planner synthesises a coverage range rather than composing one, which is why
`SNK-S0001` is on `MAPPED_BUT_UNREACHABLE` — the conversion onto the code exists and is tested, and
no query reaches it.

---

## 18. Resource governance and the pressure ladder — **[Designed; built and called by nothing]**

§4.3 states the current reality. This is the design, and it is retained because the shape is right and
the wiring is the work.

**Admission** estimates from plan cardinality and either queues or refuses; the queue is bounded, so a
refusal arrives immediately rather than after a timeout. **A client can tell a permanent refusal from a
temporary one** — *"too large for the pool"* and *"too large right now"* are distinct answers with a
`retryable` flag, and collapsing them is how a client retries for ever.

**Tenancy** is a floor and a cap: a floor honoured under global pressure so one tenant cannot starve
another, and a cap that binds even when nothing else is running so one cannot take an idle pool.

**Pressure escalates through one ladder, evaluated centrally** from a typed signal bus:

| Level | Trigger | Action |
|---|---|---|
| Normal | — | Full admission; maintenance at normal duty |
| Watch | Lag or compaction debt above warning | Defer optional maintenance; increase batch size |
| Constrain | Lag high, or buffer filling | Reduce admission; suspend re-clustering; lengthen the commit interval — freshness still served by the buffer |
| Protect | Retained log or buffer critical | **Stop admitting new queries**; existing queries run to deadline; all resources to the applier; page |
| Sacrifice | Retained log or freeze age near the limit | **Sacrifice the analytical tier to save the source**: advance the slot with a recorded gap marker, mark affected tables for re-snapshot, begin it automatically, and report the gap in provenance until closed |

The first and highest-leverage action is to **lengthen the commit interval**, which attacks the cause
rather than the symptom — fewer, larger files reduce compaction load, metadata volume and planning
latency at once. It is safe precisely because the arrival buffer preserves freshness as the commit rate
falls, and it is bounded by buffer memory, so the two parameters **must be tuned together**: owned by
different configuration sections they will drift, and the failure will occur under exactly the load that
triggered the backpressure.

The last rung is the only one that loses continuity, and it is property-tested to be reachable by its two
source signals and nothing else.

**What has to exist first**: something that publishes signals. No memory pool reports its occupancy, no
subsystem publishes a signal, and no query passes through admission on its way to running. They are
decision functions without callers — the same state the maintenance scheduler was in before its driver
was written, which is the encouraging half of the comparison.

---

## 19. The runtime and pool split — **[Designed; one runtime and one pool run]**

**Four runtimes, not one**, because the sync path and the query path are natural enemies — both want
processor time, memory and I/O — and the failure mode is asymmetric: a stalled applier stops log
reclamation, which can take down the source database.

| Runtime | Sizing | Why isolated |
|---|---|---|
| Control | Small | Must stay responsive when everything else is saturated, or an orchestrator kills a healthy node |
| Capture | **Reserved cores** | Reservation, not prioritisation — **priority schemes fail under sustained saturation and reservations do not** |
| Network | Proportional | Latency-sensitive, not compute-bound |
| Execution | Remainder | Compute-bound; tolerates queuing |

Memory would be four disjoint pools with no lending between them. I/O isolation would be **physical
first, quota second** — write-ahead log, query spill and cache on separate filesystems or devices, so a
query that fills the spill volume is *incapable* of filling the log volume. Connection pools would be
separate and individually capped for transactional writes, replication, analytical reads and maintenance,
with the replication slot reserved and never shared, so a runaway analytical workload is structurally
unable to lock out the transactional writer.

> **None of that is built.** There is one bare `#[tokio::main]`, one `FairSpillPool`, one connection cap
> on one door, and spill in the operating system's temporary directory. §4.

**What has to exist first**: a capture path to isolate. Reserved cores for a runtime with nothing to run
is a partition of the machine in exchange for nothing, so this follows §16 rather than preceding it.

---

## 20. Data tiering and the lifecycle — **[Designed; `sankhya-tiering` is on `UNREACHED`]**

Archival adds a second, orthogonal axis to the same planner, and the symmetry is exact — which is what
keeps the planner comprehensible as it grows:

| Axis | Coverage rule | Authority |
|---|---|---|
| **Log position** (freshness) | Contiguous, non-overlapping, covering `[0, target]` | Commit metadata and buffer epochs |
| **Key range** (archival) | Hot and cold extents disjoint, together covering the declared domain | The catalogue for hot; the archival registry for cold |

**Three gates, and only one is irreversible.** A purge detaches, quarantines, and only then removes; the
quarantine is a partition detach rather than a row deletion, which is why the date axis (§7) is
load-bearing rather than tidy — expiry by row deletion would need a delete path against published data,
which the immutability argument forbids.

**Structural prevention of an accidental purge** is the principle applied at its highest-consequence
point: where a mistake would be catastrophic, make it impossible to *express* rather than forbidden by
convention. The scheduler has no erasure class (§8.3), and a table any clone still references is
**ineligible** for purge with the refusal naming the clones (§11).

**The attestation drill** is built and runnable and belongs to this section's argument even though it
serves backups: it proves a write-once store still refuses writes **by trying to break it**, because
reading a configuration flag would pass in exactly the case it exists to catch — a retention policy that
still reports `enabled` and no longer applies. [`OPERATIONS.md`](OPERATIONS.md) §11.6.

**M9's eleven work items are built and its exit criteria were demonstrated**: purge end to end with
verification, quarantine and rollback; the anomaly guard halting an intentionally defective policy;
nineteen refusal paths shown to fail closed. **The gate is not cleared, and that is not a formality.**
One criterion needs the attestation drill run against a real non-production archive, which cannot be
produced from development; and a separate decision holds the *arming* of destructive purge. **Building
the purge path and arming it are two decisions**, and `sankhya-tiering` stays on `UNREACHED` with that
milestone named against it.

Beyond it, one milestone is named and unbuilt: **the data lifecycle policy**, one declaration governing
how data ages across *both* tiers. The reframe that makes it safe is the one this section has been making
throughout — **nothing moves.** Capture already published it, so ageing rows out of the transactional
store is a **release** gated on reconciliation's proof that the analytical copy exists. Detach, never
delete; reversible for a grace period; and a read of released data **refused by name** rather than
answered short, which is the failure nearly every product ships.

---

## 21. The client contract and the SDKs — **[Partly built; the correction below matters]**

> **Correction.** The previous version of this section opened *"Planned, M14. Nothing in this section is
> built."* That is false, and it was false in a way that undersold the repository: `sdk/python/` exists,
> with a package, tests, a soak harness and **twelve runnable examples**, each gated the way a test is —
> by owner directive, because an example that does not run is documentation that lies. `sdk/sql/` carries
> the SQL-side equivalents. What is not built is the Java and Rust bindings, and federated identity,
> which is what M14 actually turns on.

**The contract is the product; a binding is not.** The temptation with three SDKs is to write the good one
first and port it, which produces three clients that each decided for themselves what to validate, and the
divergence surfaces as *"it worked in Python"* — a sentence somebody then has to debug across two languages
and a wire.

> **An SDK contains no logic the server does not also enforce.**

A client may *anticipate* a refusal to give a better message, and it may never *be* the refusal. If one
binding rejects a cube whose measure declares no merge rule and another does not, then the rule lives in
that binding and the server is not enforcing it — and the second binding is a way around a correctness
rule. This is the same argument §22 makes about packs.

**Two doors, and not a third** (§5.4). **A session, not a permission**: an SDK holds a session and must
never cache an authorization decision, because a grant revoked between two calls has to take effect on the
second.

**Identity on the wire is the gating dependency.** Transport security is built on both doors and identity is
not: a principal is a fixed tenant established at the edge, not something a certificate or token
establishes. On a loopback that is tolerable and honest — the server has been a local thing. **For a client
whose entire purpose is connecting from somewhere else it is credential exposure**, which is why M14 is
gated on transport security rather than treating it as work inside the milestone. An SDK shipped in front of
it would be a feature whose first use is a mistake. [`SECURITY.md`](SECURITY.md) §6.

---

## 22. Extensions and packs — **[Designed; the declarative tier is built and no server loads a bundle]**

The engine knows about tenants, tables, columns, edges, versions and policies. **It knows nothing about any
industry**, and that is tested rather than asserted: `cargo xtask check-vocabulary` fails a core crate that
names a domain concept, and `cargo xtask check-layers` fails a pack that depends outside the allowed set with
the message *"this failure means the extension API has a gap — widen the API, not the allowance."*

**The extension API is a security boundary**, which is why it defines its own function traits rather than
re-exporting the query engine's: never let a fast-moving upstream type into a slow-moving contract. A pack
that could reach the engine directly could reach a table provider without a `Guard`.

Packs get **digest pinning rather than signatures**. An operator pins the digests of bundles they have
reviewed and anything else is refused — **including everything when nothing is pinned, because a trust policy
that defaults to trusting is not a policy.** A digest proves the bytes are the bytes you pinned; it proves
nothing about who wrote them, and calling it a signature would be the overclaim.

**What has to exist first**: the loader. The declarative tier is built and `sankhya-pack` is on `UNREACHED`
with M4's remainder named against it — the piece that reads a bundle directory into a running process was
never finished.

---

## 23. Scale-out, and the one honest leak — **[Designed; M12, and it needs a second machine]**

One artifact, one configuration schema, one process per node. The **role is a configuration value, not a
build variant**: coordinator (exactly one active — the transactional connection, the applier, the maintenance
scheduler, the catalogue), executor (nothing durable, caches only; any node serves any query), graph
(hydrated epochs, partitioned by tenant, rebuildable).

> **The honest statement about the single-binary constraint.** There is no configuration in which several
> nodes share writable state with zero coordination. Either the object store is the coordinator, through
> atomic conditional writes, or the transactional database is. The constraint is satisfied in **packaging** —
> one artifact, one configuration file, one process per node — and it cannot be satisfied in **topology**,
> where a multi-writer cluster has exactly one logical coordinator by definition. SANKHYA's answer is that
> the coordinator is a *role of the same binary*.

Two independent lines of analysis arrived at that — commit serialisation for the table format, and
coordination of maintenance jobs — and convergence from unrelated directions is good evidence the conclusion
is correct.

**Managed mode is single-node**, and that is a documented product boundary rather than a defect: a highly
available multi-node deployment requires a highly available transactional tier, which means an externally
managed cluster.

**Ordered scaling limits**, so that the next thing to break is known rather than discovered: a single-node
warehouse is bounded first by local storage throughput, then by planning latency as commit count grows,
then by the coordinator's serialisation of commits. The seams designed now and built later are the shard-set
seam ([ADR-0015](adr/0015-the-shard-set-seam.md) — a table reference resolves beneath exactly one log, so
resolving it beneath several later is a change to one function) and the object-store backend, whose
conditional-put requirement is already written down.

**What has to exist first**: a second machine. Leader election, executor scale-out, failover and replication
are all M12 for that reason, and it is recorded as the reason rather than dressed up as sequencing.

---

## 24. The transactional tier — **[Designed; the supervisor is built and unwired]**

`sankhya-oltp-pg` supervises a stock PostgreSQL cluster as a child process — `initdb`, start, readiness with
bounded backoff, stop — built and tested against a vendored, checksum-verified 17.11, and **on `UNREACHED`**
because `Settings` has no transactional-store configuration.

Three failure modes are handled explicitly because each is fatal if missed: **two processes over one data
directory**, prevented by a directory lock taken before anything else; **an orphaned database process**,
adopted if live and healthy, restarted if live and unhealthy, cleared if stale — getting this wrong produces
either corruption or a boot loop; and **a supervisor that dies leaving its child running**, handled by
parent-death signalling *plus* the boot-time check, because signalling does not survive every termination
path.

PostgreSQL 17 or later is required, and the reason is one feature: **failover-capable logical replication
slots.** Without them a routine database failover destroys the slot and forces a full re-snapshot of every
replicated table — a multi-hour analytical outage triggered by an ordinary availability event.

What SANKHYA changes about a cluster, what it will never do to one, and the settings capture needs — above
all `max_slot_wal_keep_size`, the setting that stops an analytical query from taking down the transactional
store — is [`POSTGRES.md`](POSTGRES.md), which marks each item built, applied or designed.

---

## 25. Where these decisions are recorded

| Decision | Record |
|---|---|
| The exact-pinned dependency family | [ADR-0001](adr/0001-dependency-pin-set.md) |
| A cryptographic hash for the audit chain | [ADR-0003](adr/0003-cryptographic-hash-for-audit.md) |
| The date axis | [ADR-0004](adr/0004-the-date-axis.md) |
| Array columns and numeric kernels | [ADR-0005](adr/0005-array-columns-and-numeric-kernels.md) |
| Flight SQL as the bulk data plane | [ADR-0006](adr/0006-flight-sql.md) |
| The cube model | [ADR-0007](adr/0007-the-cube-model.md) |
| Serving cubes under policy | [ADR-0008](adr/0008-serving-cubes-under-policy.md) |
| The cube lifecycle | [ADR-0009](adr/0009-the-cube-lifecycle.md) |
| External aggregations | [ADR-0010](adr/0010-external-aggregations.md) |
| Atomic publication, and claim-fails-rather-than-replaces | [ADR-0013](adr/0013-concurrency-and-data-safety.md) |
| Materialized views and the cube lifetime | [ADR-0014](adr/0014-materialized-views-and-the-cube-lifetime.md) |
| A table reference resolves beneath exactly one log | [ADR-0015](adr/0015-the-shard-set-seam.md) |
| Zero-copy cloning | [ADR-0016](adr/0016-zero-copy-cloning.md) |
| The client contract, and what an SDK may not contain | [ADR-0017](adr/0017-the-client-contract.md) |
| A record that does not fit | [ADR-0018](adr/0018-a-record-that-does-not-fit.md) |
| Named snapshots | [ADR-0019](adr/0019-named-snapshots.md) |
| The built-in function catalogue | [ADR-0020](adr/0020-the-built-in-function-catalogue.md) |
| Vectors and matrices across the tiers | [ADR-0021](adr/0021-vectors-matrices-across-the-tiers.md) |
| User-defined functions | [ADR-0022](adr/0022-user-defined-functions.md) |
| The sandbox a user function runs in | [ADR-0023](adr/0023-the-sandbox-a-user-function-runs-in.md) |
| What a difference between two versions is | [ADR-0024](adr/0024-what-a-difference-between-two-versions-is.md) |

---

## 26. Open architectural questions

Recorded rather than resolved, because a design document that presents every question as answered is one
whose author stopped asking.

1. **Whether a table may have two partitioned time axes.** The current answer is that a business date and an
   arrival date are one partition key and one ordinary column. If that stops being enough, the declaration
   becomes a list rather than a column — a schema change to the declaration rather than to the tables (§7).
2. **What replaces `Quota` when there is more than one tenant.** The type is tenant-keyed and the deployment
   has one tenant fixed at startup, so the model has never met the case it was designed for (§4.3).
3. **Whether the result cache is worth building at all**, given that its key is the hard part and is already
   built, and that the pinned-snapshot mode is the only one where caching pays (§6.6).
4. **How the exactness gate reaches a session.** `check_exactness` is a function with no caller: nothing
   carries a session's exactness setting and nothing attaches the watermark to a result.
5. **Whether bounded-memory exact order statistics are achievable at the required sizes**, or whether the
   requirement should be split into *exact* and *bounded* and answered separately (§6.5).
6. **Whether the overflow bound should be widened past 64 bits**, which is the only thing standing between
   the analytical tier and a silent wrong number on a wide decimal (§13.3).

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
