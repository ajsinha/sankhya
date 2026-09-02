# Decisions

> This chapter digests all eighteen architecture decision records to the question each
> answered, the decision, and the reasoning that would be lost if only the decision survived.
> Its central claim is that the reasoning is the valuable part: a decision recorded without
> its rejected alternatives reads as an arbitrary preference, and an arbitrary preference is
> something a later engineer feels free to change. Three records here were design gates
> cleared before any code was written, and three carry amendments made when implementation
> falsified what the design had said.

## 25.1 The form

Every record answers four questions: what was asked, what was decided, what the alternatives
cost, and what would reopen it. The third decays fastest and matters most.

Three records are **design gates** — an accepted ADR was a precondition for writing code at
all. ADR-0016 exists because *"the failure mode is not a failed query — it is silent data loss
in a table nobody was touching, discovered when somebody reads a clone months later."*
ADR-0017 because *"a contract written once and implemented three times produces three clients
that agree; three clients written separately and reconciled later produce a contract that is
whatever the first one happened to do — including its accidents, which by then are somebody's
production code."* ADR-0018 because a stream has nobody to refuse to.

| # | Title | Status |
|---|---|---|
| 0001 | Dependency pin set and the read-path strategy | Accepted |
| 0002 | Asynchronous commit delays visibility to logical decoding | Accepted |
| 0003 | A cryptographic hash for the audit chain | Accepted |
| 0004 | `sank_data_date`: one date axis on every table | Accepted |
| 0005 | Array columns, and kernels that are deterministic | Accepted, amended |
| 0006 | Arrow Flight SQL as the bulk data plane | Accepted |
| 0007 | Cubes are declared views, not a second store | Accepted, amended twice, corrected once |
| 0008 | Serving cubes under policy: the scope is part of the key | Proposed |
| 0009 | The cube lifecycle: three lifetimes, one model | Proposed |
| 0010 | External aggregations: a measure may bring its own rule, if it can merge | Accepted, amended |
| 0011 | SDAF: an extension declares what it needs, and is given it | Proposed |
| 0012 | Open capabilities: what a standing artefact must declare | Proposed |
| 0013 | Concurrency and data safety, end to end | Accepted, amended four times |
| 0014 | Materialized views, and whether they are a cube lifetime | Proposed, deliberately |
| 0015 | What a table reference resolves to, and the shard-set seam | Accepted — design only |
| 0016 | Who may delete a file that more than one table names | Accepted — design gate |
| 0017 | What a client may assume, and what it may never decide | Accepted — design gate |
| 0018 | A record that does not fit | Accepted — design gate, amended |

## 25.2 Foundations

**ADR-0001 — the pin set, and why SANKHYA owns its read path.** *Which Arrow generation, when
the released storage crates lag two majors behind head?* Pin head — DataFusion 55, Arrow and
Parquet 59 — and use the storage library for **metadata only**, with `deltalake-core` and
`iceberg-datafusion` absent from the read path entirely. Two Arrow majors cannot coexist:
identically named types become distinct and incompatible, and the trait-identity problem is
worse — a provider implementing one generation's trait cannot be registered with the other's
session at all. A second hazard is not a cost but a hard build failure: `parquet` pulls native
compression libraries, and a duplicate `links` key does not compile, which is why the question
had to be settled by an actual resolution rather than by reading manifests. `delta_kernel` is
the load-bearing pick precisely because it has **no DataFusion dependency at all**, so it
imposes zero version drag. Benign duplicates are allowlisted exactly, because none crosses a
SANKHYA API boundary; the critical family is denied unconditionally.

**ADR-0002 — async commit delays visibility to logical decoding.** *Why did a committed,
queryable workload produce no pending changes on the replication slot?* Measure capture lag
against the **flush** position, never the write position. Under `synchronous_commit = off` a
transaction returns once its commit record is in the WAL *buffer*, and logical decoding reads
flushed WAL only — so there is a window in which a transaction is durable enough for ordinary
queries and entirely invisible to change capture. What makes it dangerous is that every usual
symptom is healthy: the source is up, the slot is active and valid, rows are queryable, no
error is reported. It simply sees nothing. Comparing against the write position instead would
report permanent lag that no amount of consumption could close.

**ADR-0003 — a cryptographic hash for the audit chain.** *Which hash, in a repository with no
hashing dependency?* `sha2 0.10`. Tamper-evidence comes entirely from collision resistance: a
chain built on a fast non-cryptographic hash is not tamper-evident at all, because an attacker
who can write the file can compute a colliding record in milliseconds and the chain still
verifies. **It would look like a control and be none.** Each rejection is costed — `sha2 0.11`
brings three duplicates where 0.10 brings one; `blake3` is faster but larger, with more
`unsafe` in a workspace that forbids it, to solve a throughput problem that does not exist;
`ring` and `openssl` bring a `links` key. And a precedent is named rather than followed:
`sankhya-pack` met the same problem and answered it honestly with digest *pinning* rather than
signing. Audit has no equivalent dodge — either the hash is cryptographic or the chain is
decoration.

**ADR-0004 — one date axis on every table.** *What single guaranteed time axis unblocks
partitioning, retention and tiering, which are all stalled on the same missing thing?*
`sank_data_date` of type `DATE`, declared **per table**, never defaulted per row. Two checks a
design document would not think to run decide the type. The partition path: `DATE` gives
`sank_data_date=2024-03-01`, which Spark and Trino parse as a date, where `yyyymmdd` gives a
string every pruning query must know the encoding of. And arithmetic: `20240301 - 7 =
20240294`, which is not a date, raises no error, and users will write it. The rejected design
is the tempting *use the supplied value, else today* — under which a backfill lands in today's
partition and `WHERE sank_data_date = '2024-03-01'` returns a **mixture** of rows meaning *this
happened that day* and *we received this that day*, inseparable afterwards because the
distinction was never recorded. So a null in a declared source column is an error, not a
fallback. The cost is stated rather than hidden: a backfill must say what date its data is
about, which is a real imposition on the loader and is the point.

**ADR-0005 — array columns and deterministic kernels.** *How do we compute on vector-shaped
data without reintroducing the export-compute-import split?* `FixedSizeList<Float64, N>`,
kernels in pure Rust, and **every reducing kernel goes through `deterministic_sum`**. Every
useful vector kernel ends in a floating-point sum, and non-associativity means the same
aggregate returns different values run to run: *"the difference is small — that is what makes
it expensive: too small to notice and too large to reconcile."* Not Polars: a dataframe
*engine* carrying its own Arrow layer and execution engine, a substitute for DataFusion rather
than a companion. Not BLAS: a `links` key, a workspace-wide `forbid(unsafe_code)` that a dot
product is not a reason to breach, air-gapped packaging — and decisively, **BLAS offers no
ordering guarantee at all**; reordering freely for speed is its entire design. Two details show
the standard: LU's pivot tie-break uses `>` rather than `>=`, so two rows of equal magnitude
cannot be chosen differently by two builds; and a matrix column with no shape metadata is
refused rather than assumed square, because the guess produces numbers from values that were
never in the same row, every one of which looks ordinary.

**ADR-0006 — Flight SQL as the bulk data plane.** *How do bulk extracts avoid the row-by-row
conversion the wire protocol forces at the last step?* Flight SQL over gRPC, read-only,
streaming, authorized at `GetFlightInfo` with the ticket carrying the outcome. The data is
columnar on disk, in memory and through every operator, and is then taken apart to be put back
together by the receiver — nothing for a screenful, the whole cost for an extract. Adding it
brought **zero new duplicate crates**, measured rather than assumed; had it required a second
Arrow generation, ADR-0001 would have ruled it out. Two consequences are named because they are
easy to get wrong. **A query's error can arrive mid-stream**, reported as a `Status` rather than
a clean close, because a truncated stream that ends cleanly is indistinguishable from a complete
one. And the ticket does not re-authorize on redemption, so it is bound to its tenant. `DoPut`
is absent — a third write path with its own semantics would be a way for the other two to
disagree.

> **Key idea**
> Four of these six are decided by something measured: a resolution spike, a flush LSN, a
> duplicate count, a pivot tie-break. A dependency decision taken from manifests is a decision
> taken from documentation about a resolver rather than from the resolver.

## 25.3 The cube family

**ADR-0007 — cubes are declared views, not a second store.** A cube holds no data; materialised
cuboids are ordinary published tables keyed by *(definition version, snapshot, cuboid)*; every
measure declares a rule **per dimension** or is refused. `GROUP BY ROLLUP(year, quarter,
month)` does not make a time dimension — it makes one query that knows three column names, and
the user then reconstructs the structure in a client, which is how the work ends up in a
spreadsheet nobody can reconcile.

The **revision** is the instructive part. The first version forbade automatic materialisation,
because a precomputed aggregate is a second copy that can disagree. *"That reasoning is right
about every other product in this category and wrong about this one"* — files are immutable and
every key embeds a snapshot, so a new commit cannot produce a stale hit; the key simply misses.
Materialisation is a **cache**. Hence the rule the rest of the system uses: *what may be
automatic is anything whose being wrong costs latency; what may not is anything whose being
wrong changes an answer.*

Two amendments and a correction followed. The **fingerprint**: the guarantee requires the
definition version to move when the definition does, and a hand-edited version does not —
producing a cuboid built under the old meaning of a measure, served against the new one, as a
real number computed correctly from a definition that no longer exists. The **one ULP**:
bit-identical results failed, and not from a defect — `round(round(a+b) + round(c+d))` is not
`round(a+b+c+d)`, and a materialised cuboid is precisely a re-association of the same addition.
One ULP is the worst possible size: large enough for two reports to disagree by a penny, small
enough that nobody can point at a defect. Materialised aggregates now store an unrounded
Shewchuk expansion. And the **correction**: an earlier paragraph called a semi-additive measure
*worse* than a non-additive one, which describes what a naive implementation does rather than
what the rule is — and taking it literally cost a set of tests that refused valid roll-ups.
**Additive** is about the operator; **composable** is about permission. A closing balance is not
additive over time and *is* composable over time, provided the executor applies `last` and not
`sum`. The danger of a semi-additive measure is the operator, not the axis.

Three further positions are load-bearing. Answering from an ancestor is the most dangerous
operation in the engine, because a wrong number there is plausible, derived from real data, and
reconciles against nothing — nobody reconciles a subtotal. Ragged hierarchies are native rather
than padded, because padding **invents members that do not exist**. And a measure with no
declared rule is refused rather than defaulted to `SUM`, because summing twelve month-end
balances gives a number that means nothing and looks exactly like a number that does.

**ADR-0008 — the scope is part of the key.** Three tiers, with the caller's scope digest in the
cache key. Both obvious placements are wrong, and the second is wrong in a way no test finds
unless it goes looking: hydrating per statement reads the whole fact table every query, making
a cube slower than the `GROUP BY` it was meant to accelerate; hydrating once at startup hands
every principal the same totals whatever policy says, with nothing in the result to indicate
it. The survey is memorable — SSAS solves it with `VisualTotals`, documented as **off by
default, for performance**, with Microsoft's own warning that it *"could create a security
issue"*; AtScale and Kylin fall back to base tables. The common shape: **a pre-aggregate is
usable only when the scope it was built under is at least as narrow as the caller's
entitlement.** Teaching the cube engine about policy would be a second implementation of the
authorization rule, and two implementations disagree eventually — when they do, the cube is the
one that leaks, because it answers with a number rather than with rows somebody might notice
were missing.

**ADR-0009 — three lifetimes, one model.** Ephemeral (the default), Declared, Maintained — so a
cube is promoted or demoted rather than rebuilt. Making exploration durable is paperwork nobody
asked for; making the dashboard ephemeral means recomputing it for every viewer. Snowflake's
`TARGET_LAG` is documented as *a staleness target, not a refresh interval*, and the difference
matters both ways: a schedule refreshes when nothing changed and fails to refresh when a build
outlasts its interval. Freshness here is **exact rather than estimated** — the distance between
the cuboid's keyed snapshot and the table's current version, not a wall-clock guess about when
a job last ran. Refresh reuses the existing maintenance tick, so it cannot starve compaction,
and never takes the cube away.

**ADR-0010 — a measure may bring its own rule, if it can merge.** An external aggregation is a
**contract, not a function**: `accumulate`, `merge`, `finish`, `state`, and **the presence of
`merge` is the composability declaration**. Given a bare `def f(values)` the system cannot know
whether `f(f(a) + f(b))` is `f(a + b)`, and both fallbacks are bad — assume it composes and
produce plausible wrong numbers from ancestors, or assume it does not and surrender roll-up and
materialisation. The method names are Snowflake's in spelling but the shape is Gray *et al.*,
*Data Cube* (1996): distributive, algebraic, holistic. Adopting it **closes a gap in the
built-ins** — `Mean` composes given a (sum, count) intermediate, which is exactly what a state
plus a merge is, so the refusal narrows from *not distributive* to *genuinely holistic*. The
*state* rather than the number is materialised, because a float cannot be rolled up further
without the rounding ADR-0007 forbids. Determinism is exercised rather than trusted: the same
input accumulated in one batch and several, merged in two groupings, compared **by bits**. The
2026-08-31 amendment puts it out of process — an aggregation supplied by a user is the one part
of a query this system did not write, and in-process it shares an address space with the audit
chain and every other tenant. *A sidecar that panics is a sidecar that dies.* The contract is
unchanged, which is what makes the decision reversible.

**ADR-0011 — an extension declares what it needs, and is given it.** Injection, not connection:
a `needs` manifest, narrowly typed accessors, and **no `execute`**. Snowflake does not pass a
session to a UDF; Spark's UDFs have no context. The reasons: a function that may issue queries
is re-entrant so it can recurse, its answer depends on data nobody declared so it cannot be
cached correctly, and it runs where the security context has already been decided. **The
precedent says do not hand an aggregate a connection; it does not say the capability is wrong —
it says the mechanism is.** Dependencies resolve through the *caller's* guard, because an SDAF
reading a table its caller cannot is disclosure through arithmetic with an extra step — the
caller sees only a number, and a number carries no evidence of where it came from. And an
unresolvable dependency is a **planning error, not an empty result**: an SDAF silently seeing an
empty rate table returns numbers wrong by a factor nobody notices.

**ADR-0012 — openness in proportion to what is declared.** *Openness comes from a
well-specified contract, not from the absence of one.* Three failure modes. **Authority**: SQL
has argued definer's versus invoker's rights for thirty years, and the classic failure is that
the author leaves and the artefact keeps handing out data under permissions nobody holds. This
system gets a better answer for free, because `scope_digest` hashes what a guard *permits* and
deliberately not who is asking — if policy changes, the digest changes and the old cuboid is
never found. There is no stale-permission window because there is no lookup that can succeed.
**That property arrived by accident of a caching decision and must now be treated as
load-bearing**: any change making the digest coarser silently reopens the hole. **Population**:
cuboids multiply as cubes × cuboids × scopes × snapshots, and snapshots are unbounded — a
superseded cuboid is a whole table no log mentions, so the orphan sweep cannot see it. *This is
a leak, and it is the same shape as the one that filled a disk in the soak.* Cuboid retirement
must therefore exist before named scopes do. **Cost**: marking a cube maintained should be a
grant, for the same reason nobody grants themselves storage quota.

**ADR-0014 — materialized views, and the crate that stays empty.** *"The tempting answer is
that M7 already built this. It is nearly right, which is what makes it dangerous."* A maintained
cube is a materialized view in every respect that costs effort — catalogue, key, target lag,
tick, reclamation, scope digest. What does not settle it: **a materialized view is not
necessarily an aggregate**. It can be a join or a filter, with no measures and no grain, for
which the lattice and roll-up are meaningless. The decisive argument against a separate crate is
a pattern this repository keeps finding: the failure mode is not that it will not work, but that
it will grow a second refresh loop, a second staleness rule and a second reclamation path, and
the two will drift. **This warehouse has already paid for that twice** — a soak with its own
writer produced flat warehouses reporting `PASS`, and an ingest pipeline with its own writer
produced tables with no partition columns. The record stays *Proposed* deliberately: the nine
empty crates resolved beside it had no design question left, and this one does.

> **Pitfall**
> Three of the seven cube records were corrected by implementation. The ADR-0007 correction had
> already cost a set of tests that refused valid roll-ups. A design document is a hypothesis
> until something runs.

## 25.4 Safety, shape and lifetime

**ADR-0013 — concurrency and data safety, end to end.** Two properties: **P1**, any file a
reader can name becomes visible all at once or not at all; **P2**, a claim fails rather than
replaces. Plus one mechanism: reclamation waits for readers, not for a proxy.

The audit's division is the useful part: *"nothing in the compute layers needs fixing. The type
system did that work. It is worth stating explicitly, because the instinct on being asked 'is it
concurrency safe' is to harden everything, and hardening what is already safe adds contention
and hides the parts that are not."*

The four defects are one shape. `commit` claimed a version by `exists()` then `rename`, and
`rename(2)` replaces its destination silently — so two committers both see the version free, the
second overwrites the first, and the rebase loop never runs because the `VersionTaken` it waits
for is never returned. **The technique was already known here and applied unevenly**: three
places right, four wrong, nothing checking which. That is why the rule became a gate rather than
a paragraph. Three reclamation paths guarded against proxies — 24 ticks, seven days, a hundred
table versions — and version-space is weakest, because a hundred versions can pass in seconds
while a scan runs for minutes. Not theoretical: the failure was observed during M7 as an error
naming a Parquet file the caller had never mentioned.

**Safe is not concurrent**, which is why three criteria are measurements. A global lock delivers
safety perfectly and would pass every safety test. The owner's instruction was unprompted:
*"Don't make one global lock — we don't want a Python GIL-like handicap."* The reader registry
is where a GIL would have appeared — a `Mutex<HashSet<PathBuf>>` per warehouse is a GIL with a
filesystem accent, and it satisfies every safety criterion. Three rules keep it out: per-table
never per-warehouse; readers must not take a lock a sweeper can hold; and the read path may not
grow a lock it did not have.

The throughput choke points were invisible to the earlier audit, which asked *can this corrupt?*
and got the answer no. `LogCache` held one mutex over every table, **across filesystem I/O**.
The ordering matters: *striping a lock held across a disk read only reduces how many threads
wait; removing the I/O from the critical section changes what they are waiting for. Do this
first.* And on a hot read path, `ArcSwap` means the path *loses* a lock rather than gaining a
faster one — **the best lock on a hot read path is the one that is not there.**

**ADR-0015 — the shard-set seam, refused rather than deferred.** Two documents carried the same
sentence — a table reference resolving to a shard set is *near-free now and an expensive retrofit
later* — and it had never been designed. Reading the code dissolved the premise: `plan_splice`
already resolves one reference to several sources and proves they cover the span exactly once,
and `AddFile` already records partition values. Shards as file groups beneath one log are
**already built**, costing nothing because they were never absent. Shards as independently
committed logs require claiming a version in each atomically — a distributed commit protocol,
landing on a different exit criterion, and costing safety to buy nothing, since the cheapest
correct cross-shard commit is a lock over the shard set, which is the forbidden design arrived at
from another direction. **The most valuable output is that the seam was mislabelled.** It was
recorded as a *resolution* seam in two documents for months; resolution was never the cost.
Recording the mislabelling matters more than recording the decision.

**ADR-0016 — who may delete a file that more than one table names.** `reachable` becomes the
union of the live sets of every table in the **clone family**; a clone's log names none of its
origin's files. The walk-through is the argument: clone at version 40, and a week later the
origin's sweep lists a file, finds it unnamed by the origin's own log, is passed an empty
reachable set, sees it is older than seven days, and removes it. *Nothing failed. No query
errored.* **From the origin's point of view a file only the clone still names is
indistinguishable from debris.**

Reference counting was refused on an asymmetry: drift high loses disk, drift low loses **data,
silently, in a table nobody was touching** — the exact sentence the gate exists to prevent.
Copy-on-maintenance was refused because it *quietly* gives up constant space: clone a quiet table
and pay nothing, clone one that compacts tonight and pay for the whole table by morning; refusing
cloning outright would be more honest than that. Reachability is affordable because the scan is
over the clone family, making it a **no-op for every table nobody has cloned**, and it fails safe
— a stale lineage record makes the reachable set *larger*.

The clone naming none of its origin's files rejects an absolute URI (spec-legal, and a restore
into a different directory silently produces a table whose files are all missing) and a `../`
path (undefined, so a bet on every reader agreeing). Its **cost was listed incompletely when
written**, and the omission is recorded rather than fixed away: had the clone's log named the
origin's files, the existing read path would have served clones unchanged. Deciding otherwise
meant this engine must splice too, and until that existed a clone was a table that read as
**empty** — found while planning the soak that would have exercised it.

**ADR-0017 — what a client may assume, and what it may never decide.** Four decisions carry the
weight. A binding contains **no logic the server does not also enforce** — the test being *delete
every SDK and nothing about what the system permits, refuses or audits changes*; otherwise, if
Python rejects a cube whose measure declares no rule and Java does not, the rule lives in Python
and the second binding is a documented way around a correctness rule. A refusal crosses the wire
as **data** — `code`, `sqlstate`, `remediation`, `subjects` — and `subjects` is the field easy to
omit and expensive to add later, because without it the message becomes an API nobody meant to
publish and nobody may reword. Results stream, and no binding collects on the caller's behalf.
And **there is no job registry**: a registry is a second durable state machine needing its own
reclamation, authorization and answer to *what happens when the server restarts mid-job?* This
system already has exactly one durable record of what happened, and it is the log — so a
disconnected client asks the warehouse what is there rather than asking the server what it was
doing. The price is two obligations: every long operation is idempotent under re-issue or refuses
naming what it found, and no operation leaves a state only the disconnected client could
describe.

Two lines generalise. **If the client is thin, its language does not matter; if its language
matters, it is not thin enough.** And on gated examples: *an example that does not run is
documentation that lies, and it lies to the person least able to tell* — somebody meeting the
product for the first time, who cannot distinguish *this is wrong* from *I am holding it wrong*.

**ADR-0018 — a record that does not fit.** Quarantine it whole, into a **table** rather than a
side directory, with a **mandatory expiry**; one bad record is an incident and a **rate over a
recent window** is an outage that halts the pipeline and waits for a person. *A stream cannot
refuse the way a statement can* — there is nobody to tell, and the obvious alternative is worse,
since stopping for one malformed document turns one bad record into an outage, which is how
ingest systems come to run with every validation switched off. A side directory is outside
everything this system has built — nothing sweeps it, nothing backs it up, no policy governs it,
and it holds source data. The stop is a rate rather than a total, because a total eventually
trips for historical reasons, and it waits for a person because **auto-resume is how the same
outage is rediscovered every five minutes and acted on by nobody.** The five refused
configuration behaviours each turn a defect at the source into published data that looks fine —
widening a type, inventing a value for a missing key (*a default indistinguishable from a
measurement is a measurement nobody made*), accepting unseen keys, and coercing between strings,
numbers and dates.

**The amendment is a design corrected by a soak in under a minute.** The position is a high-water
mark, which keeps it O(1) — but a mark records *where a feed got to*, not *which files it read*,
so a source sorting below the mark is indistinguishable from one finished last week, and the
first implementation announced every previously finished source as a late arrival on every run.
The decision is which error to prefer, and it is **never re-ingest**: duplication is silent and
permanent, where a skipped source is a file still sitting in a directory, findable and
replayable. What replaces the refusal is a **count** of sources skipped as already read.
*Refusing individually is not available at this cost; noticing is.*

## 25.5 What the register shows

**A decision is preferred to a deferral.** ADR-0015 refuses a reading rather than scheduling it;
ADR-0014 leaves a crate empty rather than reserving a name for a capability nobody asked for;
ADR-0005 defers SVD for a stated reason. Each refusal is cheaper to reverse than a half-built
mechanism.

**The rejected alternative is costed, not dismissed.** Reference counting, copy-on-maintenance,
BLAS, Polars, `blake3`, MDX, a job registry, a `pyo3` core — each appears with what it would buy
and what it would cost, which is what makes the record survive its author.

**Amendments are recorded in place.** ADR-0007 carries two and a correction, ADR-0013 four, and
ADR-0016 records an omission in its own cost list. A register edited to look prescient is one
nobody can learn from.

**A design gate is a gate.** Three records were preconditions for code, and the one that mattered
most was written because the alternative was silent data loss in a table nobody was touching.
Gates cost days; the failure they prevent costs a table.
