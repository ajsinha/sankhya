# 9. Capture and ingest

> Data arrives two ways: streamed from PostgreSQL's write-ahead log by a decoder this project
> wrote, and read from declared file feeds under a YAML contract. This chapter argues that both
> paths reduce to the same three obligations — a batch is a set of *whole* transactions, the
> position is committed with the rows, and a record that does not fit is neither dropped nor
> coerced — and shows the specific defects each obligation prevents. It also gives the
> reconciliation harness that turns "zero data loss" into a measured quantity, and names the
> transport that does not yet exist.

## 9.1 The shape of the capture path

```
  PostgreSQL WAL  ── START_REPLICATION ... LOGICAL (CopyBoth) ──▶
  transport (vendored, behind a trait) ── bytes ──▶
  decoder (ours, pure, fuzzed) ── events ──▶
  apply planner (ours, pure) ──▶ arrival buffer + landing writer (append-only)
```

SANKHYA speaks the database's streaming replication protocol directly, in-process, using the
built-in `pgoutput` logical decoding plugin. There is no message broker, no connector framework
and no external process.

**The decoder is ours.** The transport may be vendored — the available crates are young, pre-1.0
and thinly maintained — but the decoder parses untrusted bytes from a network socket and is
therefore both the largest attack surface and the most correctness-critical component in the
ingest path. It lives in a pure crate at layer 0, is property-tested for round-trip fidelity, and
is continuously fuzzed. Neither of the mainstream Rust PostgreSQL client crates offers
replication support, so this was never optional.

**The apply planner is pure**: decoded event stream in, table mutation plan out.

That seam is described in this project's own documents as *the highest-leverage testability
decision in the design*, and the reason is arithmetic. It allows thousands of randomised crash
and interleaving scenarios to run in milliseconds against an in-memory table — which is the
only practical way to gain confidence in exactly-once behaviour. The alternative, one
integration test per scenario against a real database, buys single-digit coverage per minute.

## 9.2 The transaction invariant

> A batch is a set of **whole** transactions. A transaction is sealed only by its commit, and
> only sealed transactions are eligible to flush; an in-flight transaction is carried forward.

Splitting one would publish half a transaction, which for any multi-table write is a torn read
that no downstream consumer could detect. The invariant has two counterparts elsewhere in the
system: on the storage side, a batch spanning partitions becomes several files in **one commit**,
because a reader must never see half a batch; and on the read side, a source transaction carries
one commit position, so a single target position either includes all of it or none (Chapter 8,
§8.3).

Shutdown honours it explicitly: the capture source stops, **the in-flight batch finishes**, and a
partial batch is rolled back entirely rather than half-committed.

## 9.3 Exactly-once, and the ordering that is the guarantee

Delivery is at-least-once; application is idempotent; the composition is effectively
exactly-once. Two rules carry it.

1. **Every commit records its log position in the table's own commit metadata.** On restart the
   applier reads the last committed position from the table's history. No external state is
   consulted, so there is nothing to fall out of sync.
2. **The slot position is advanced only after the corresponding commit is durable.** Reversing
   this ordering is silent data loss, and it is the most common defect in hand-built capture
   pipelines.

> **Key idea**
> Rule 2 is not a precaution around the guarantee. **It *is* the guarantee.** If the worker dies
> between publish and checkpoint, the source resends and idempotence absorbs the duplicate; if
> the checkpoint were written first, the range would be silently missing and nothing downstream
> could tell.

### Idempotence is per row, not per batch

This is the part most likely to be got wrong, and it was got wrong here first.

After a crash the source resends everything since the last confirmed position, so
already-published work arrives again. Publishing it twice duplicates rows — and row counts alone
still look plausible against a source that has itself grown.

A resent stream does **not rebatch identically**: the restarted pipeline sees a different message
boundary, so a batch routinely spans both already-published and new positions. Skipping only
*wholly-old* batches would therefore republish every row in such a batch, which is exactly what
the crash tests found. The filter compares **positions rather than content**, because positions
are monotonic and content is not, and it is applied per row:

```
keep row  ⟺  commit_lsn > published_through
```

with counters for batches and rows skipped as duplicates, so the behaviour is observable rather
than assumed.

The same defect appears one layer up in the arrival buffer, where a segment straddling the
durable frontier must be filtered per row rather than returned whole (Chapter 8, §8.5). Two
occurrences of one mistake in two subsystems is the signature of a rule that needs stating
rather than a bug that needs fixing.

## 9.4 Adaptive batching

The apply loop flushes on first-to-fire across a size trigger, a **size-gated** time trigger, a
hard freshness backstop, a row bound and a transaction-count bound.

The size gate on the time trigger is the part that is easy to omit and expensive to omit: without
it, a table receiving a trickle emits hundreds of tiny commits per day, spending more on metadata
than on data. The defaults make the shape concrete:

| Bound | Default |
|---|---|
| Rows | 50,000 |
| Transactions | 5,000 |
| Age before a flush is considered | 120 ticks |
| Minimum rows for an age-triggered flush | 1,000 |
| Hard age backstop | 900 ticks |

The reason for the flush is reported, so the adaptive control loop has a signal and an operator
can tell a healthy cadence from a pathological one.

Four overrides apply in strict precedence, and two of them move in opposite directions:

| Condition | Action | Why |
|---|---|---|
| Source log pressure rising | **Shorten** | Shorter batches drain faster, advancing the replication position sooner. Deliberately accepts analytical damage to protect the source |
| Shared storage is the bottleneck | **Lengthen** | Fewer, larger writes are more efficient when the store is slow |
| Query planning latency measurably regressing | **Lengthen**, and raise a named signal | *"Do not overload the analytical tier"* expressed as a control law rather than an aspiration |
| Partition fan-out excessive | Do **not** shorten; engage fan-out guards | Shortening makes fan-out worse |

Hysteresis is required — a minimum dwell and a two-window persistence rule — or the loop
oscillates against its own effect on the signal it is reading.

### The ratio gate

```
publish partition P when
     accumulated_change_bytes(P) >= ratio × base_bytes(P)     ← bounds cost
  OR age_of_oldest_unpublished_change(P) >= publish_interval  ← bounds staleness
  OR P has been sealed and not yet finalized
```

**Both conditions are required.** The ratio bounds cost, the interval bounds lag, and neither
alone is sufficient. A moderate default reduces write amplification by roughly two orders of
magnitude relative to a fixed-cadence merge.

The honest limit is stated rather than configured around: **a workload with uniformly-distributed
updates across a very large base and a tight external-freshness requirement is fundamentally
unsuited to copy-on-write storage.** Such a table should accept a longer publish interval, be
served from the transactional tier directly, or wait for delete-vector write support. Saying so
is more useful than implying a configuration exists that fixes it.

### Fan-out

Four guards: a per-batch partition cap with deferral, a minimum file size, an alarm, and a bulk
bypass that sorts by partition first. The first two convert fan-out into per-partition batching.

> **Key idea**
> **The alarm is the important one.** Sustained high fan-out is a *symptom* that the partition
> scheme violates the minimum-partition-size guardrail. The guards buy time; the alarm gets the
> design fixed. Silently absorbing it would be the failure.

The arithmetic behind the guards is in Chapter 7: a 5,000-row append spread over ninety days
becomes ninety files of fifty-five rows, and a measured run produced 32,279 live files across
ten tables in four minutes, averaging 37 KB against a 256 MB target.

## 9.5 Why the landing zone is append-only

Three library limitations force it, and each is a fact about a released version rather than a
preference:

| Library | Limitation |
|---|---|
| Delta | Reads and preserves deletion vectors but **cannot emit them**; update and delete are copy-on-write, rewriting whole files. Updating a thousand rows scattered across a thousand large files rewrites a billion rows to change a thousand |
| Iceberg (equality deletes) | Anti-joined against every earlier data file, and the format is itself moving away from them |
| Iceberg Rust | Append-only, and **cannot compact at all** |

So the applier writes an **append-only change log**: key, position, operation, payload. No
deletion vectors, no delete files. Current state is produced by SANKHYA's own merge-on-read
(Chapter 8, §8.4), and a background job compacts the log into a clean published table by bulk
partition rewrite — efficient precisely because it is bulk rather than scattered.

The strategic effect is worth naming: it reduces both formats to versioned file containers with
metadata, which is what makes the format choice reversible and the arrival buffer
format-independent.

### Two published surfaces

```
  <warehouse_root>/<schema>/<table>/            merged current state   — correct standalone
  <warehouse_root>/<schema>/<table>__changes/   append-only change log — correct standalone
  ${SANKHYA_DATA}/hotwal/, spill/               in-flight, node-local, never on shared storage
```

Publishing the change log beats hiding it on three counts. Each byte reaches shared storage
**once** rather than twice. **No external reader can obtain a wrong answer from either path**,
because each is exactly what its name declares — whereas a hidden staging area relies on external
readers not finding it, which is a convention rather than a guarantee. And the change log gives
external consumers a genuinely fresh path, since appending requires no merge.

The two coverage ranges are disjoint **by construction** — the base covers up to its high-water
mark, the delta covers strictly beyond it — so double-counting is impossible rather than
unlikely. Append-only and keyless tables have no second stage at all: with no primary key there
is no "current row", so the base *is* the append target and its freshness equals the batch
interval at zero merge cost.

Three contracts result, and the third row is the disclosure that must not be discovered during an
integration:

| Contract | Reads | Freshness |
|---|---|---|
| Simple | Base alone | Publish cadence — always correct, zero knowledge required |
| Fresh | Base ∪ change log, via the published merge | Batch interval — seconds |
| SANKHYA's own readers | Base ∪ change log ∪ arrival buffer | Sub-second |

**External readers taking the simple path see *mutable* tables at publish cadence — minutes,
not seconds.** This is not configurable away; it is copy-on-write mutation meeting the
correct-standalone requirement.

## 9.6 Onboarding and schema change

Automatic onboarding has three parts: a publication covering all tables, so tables created later
are captured automatically; onboarding triggered by the first relation-metadata message for an
unknown relation, which fires exactly when the user first writes to the table; and a schema-change
log written by a database event trigger, which is itself replicated and therefore arrives in-band
through the same stream — catching changes that produce no row events.

Replica identity is a hazard and is handled explicitly: a table with no primary key and default
replica identity causes *the database* to reject updates and deletes. Onboarding detects this and
either remediates with a documented write-amplification cost, or onboards the table append-only
with a clear diagnostic.

Incompatible schema changes **quarantine the affected table**: the applier stops applying to it,
the last consistent version remains queryable, a named error and remediation are surfaced, and an
explicit operator action resolves it. The reason is that intent is genuinely unknowable from the
change alone — a dropped column may mean "stop capturing this" or "erase it from history", and
guessing wrong is either a data-loss incident or a compliance breach. A renamed column is
indistinguishable from a drop-and-add without tracking attribute numbers. A narrowing type change
silently loses data.

The coupled requirement that is easy to miss: **with a single replication slot there is one
cursor.** If a quarantined table stalls it, retained log grows without bound and fills the
source database's volume — INV-2, arriving through the feature that was meant to be careful.
Quarantined events are therefore routed to a durable dead-letter store and the cursor is
advanced, with replay on resolution.

The same shape recurs for renames. Ingest continues throughout a rename quarantine, and only
*publication* pauses — which is possible only because ingest is keyed by a stable table identity
rather than by name. Without that property the feature would trade a naming problem for an
availability problem.

## 9.7 Reconciliation: turning "zero data loss" into a number

Comparing two datasets row by row requires both in memory and in the same order. A digest lets
each side be computed independently, in streaming fashion, in any order, and compared as a single
value.

> **Pitfall**
> The obvious way to combine per-row hashes order-independently is to XOR them. **That is wrong
> here, and subtly so: XOR cancels duplicate pairs.** A pipeline that wrote every row exactly
> twice would produce a digest identical to one that wrote each row once — and duplication from
> at-least-once delivery is precisely the defect this harness exists to detect. Wrapping addition
> is order-independent *and* duplicate-sensitive.

The second requirement is independence. If the expected side were produced by querying the source
through the same reader the pipeline uses, a defect in that reader would appear on both sides and
cancel out: the harness would pass while the data was wrong. **So the expected side comes from a
source that shares no code with the pipeline.**

Measured, at one million rows across ten tables in five interleaved transactions:

| Quantity | Value |
|---|---|
| Rows written | 1,000,000 in 2.0 s (~500k rows/s) |
| Messages decoded | 1,000,060 |
| Rows captured through the pipeline | 1,000,000 in 3.5 s (**~285k rows/s**) |
| Retained write-ahead log during the run | 1,053 MB |
| Files published | 10 |
| Bytes published | 6.3 MiB |

A caveat travels with the last row: **6.7 bytes per row reflects *this* data**, which is
deliberately regular. It is not a general compression ratio; the honest range against a realistic
schema is closer to 3×–15× the source's footprint.

And the read-your-own-writes demonstration, which is the whole point of the arrival tier: a
session wrote a row, capture caught up in **6 ms**, and the query returned it.

## 9.8 Crash recovery

A restart resets in-memory state, and two counters are *derived* rather than remembered — so they
must come from the log, which is the only thing that survives.

Without recovering them, the pipeline would restart at sequence zero and write `00000000.parquet`
over a file that is still live, and restart at version zero and be told the table already exists.
The sequence recovered is the **highest ever committed, not the highest still live**: a
compacted-away file's name must not be reused while readers holding an older snapshot can still
resolve it.

Partitioning reintroduced exactly this defect after it had been fixed. The path became relative
to the table root and carried the partition directory — `sank_data_date=2024-03-01/0000.parquet`
— parsing the whole thing yielded nothing, the sequence restarted at zero, and the next write
would have overwritten a live file. **The recovery existed and the change walked around it.**

The ordering rule that governs this is stated as an invariant: **the log lags the filesystem,
never leads it.** A file on disk with no log entry is invisible and reclaimable; a log entry with
no file makes every query fail.

## 9.9 The freshness floor nobody owns

Under PostgreSQL's `synchronous_commit = off`, a transaction returns to the client once its
commit record is in the WAL *buffer*, before that buffer reaches disk. **Logical decoding reads
flushed WAL only.** There is therefore a window in which a transaction is durable enough to be
visible to ordinary queries on the primary and entirely invisible to change capture.

The measurement that settled it:

```
after insert:  pg_current_wal_lsn       = 4/1137C000
               pg_current_wal_flush_lsn = 4/11375CC8
```

The write position had advanced; the flush position had not. Under a quiet workload the window
closes only when the WAL writer next runs, so it is bounded by `wal_writer_delay` rather than by
anything the capture loop controls.

Four consequences are recorded in [ADR-0002](../../adr/0002-async-commit-and-decoding-visibility.md):
asynchronous commit is documented as *reducing the recovery-point objective* and surfaced in the
diagnostic rather than left to be discovered; the capture loop measures lag against the **flush**
position, not the write position, because comparing against the write position would report
permanent lag that no amount of consumption could close; tests that need to observe a change
immediately wait on the flush position; and the bulk-load path may still use asynchronous commit,
since nothing is capturing during it.

The freshness budget gains a term that is not under SANKHYA's control, and the honest form of
the objective names it. Tuning it away trades durability for latency **at the source**, which
is the operator's decision to make and not this system's.

## 9.10 Declared feeds

The second ingest path is configuration rather than replication: a directory of newline-delimited
JSON documents, described by one YAML file per feed. It is complete as of M13, and its design
gate is [ADR-0018](../../adr/0018-a-record-that-does-not-fit.md).

```yaml
name: orders                    # appears in metrics, in quarantined records, and in refusals
from: /var/spool/sankhya/orders # a directory of newline-delimited JSON, read in name order
schema: sales
table: orders                   # sales.orders must already exist; a feed never creates it

date: ingest
# date: { column: booked_on }   # ...or a declared column, which must be a date and not null

columns:
  - name: id
    type: int64                 # never inferred from the data
  - name: amount
    from: total                 # the key in the document, when it differs from the column
    type: decimal(18,2)         # decimals arrive as *strings*: a JSON number is a double
  - name: note
    type: utf8
    nullable: true
    missing: null               # a missing key means null — said by name, on a nullable column

unknown: refuse                 # a source that grew a field is news

microbatch:
  rows: 10000
  seconds: 30

quarantine:
  retain_days: 30               # zero is refused
  window: 100                   # the recent records the stop rate is measured over
  stop_above: 0.2               # above this fraction, the feed stops and waits for a person
```

The `date:` key is required, because the date is declared per table and never defaulted
(Chapter 7). The microbatch has **both** bounds because either alone stalls: by size, the last
records of a quiet hour are never published; by time, a busy feed writes a file per tick.

### What a configuration may not do

Each of these turns a defect at the source into published data that looks fine, which is the
failure mode that is never noticed at the time.

| It refuses | Rather than |
|---|---|
| `"42"` into an `int64` | parsing it, and hiding the day the source sends `"forty-two"` |
| `3.0` into an `int32` | converting it, and then having to decide about `3.5` |
| a JSON *number* into a `decimal` | accepting it, when `0.1` is not `0.1` in binary floating point |
| a missing key | inventing a value indistinguishable from a measurement |
| a key no column claims | discarding a field the source just grew |
| `"31/08/2026"` as a date | guessing between day-first and month-first |

Everything above is a **validation** failure — refused when the configuration is loaded, naming
**every** rule that failed rather than the first, for the same reason the cube's measure
validation gives: fixing them one build at a time is how a person gives up.

### A record that does not fit

> **Key idea**
> A stream cannot refuse the way a statement can. There is nobody to tell: the producer wrote the
> record and moved on, and the connection that carried it is closed. And the obvious alternative
> is worse — stopping the pipeline for one malformed document turns one bad record into an
> outage, which is how ingest systems come to be run with every validation switched off.

So a record that does not fit is neither dropped nor coerced. It is written to a quarantine
**exactly as it arrived**, alongside five things:

| Field | Why |
|---|---|
| The payload, verbatim | A record reduced to an error message cannot be replayed, and replay is the only actual remedy |
| The reason, as a code | So a client can count kinds rather than parse sentences |
| The reason, as a sentence | So a person can act on one without a lookup table |
| The configuration version that refused it | *"Why did this fail in March"* is otherwise unanswerable |
| When it arrived, and from where | Which file, which offset — the coordinates a replay needs |

**The quarantine is a table**, `sank.sank_quarantine`, not a directory of rejected files beside
the warehouse. A side directory would be outside everything this system has built: nothing sweeps
it, nothing backs it up, no policy governs who can read it — and it holds *source data*, which is
the most sensitive thing here. As a table it inherits durability, backup, retention, tiering and
policy for free, and it carries `sank_data_date` like everything else. It is queryable:

```sql
SELECT source, position, reason_code, reason, payload
FROM sank_quarantine
WHERE feed = 'orders';
```

Its schema is fixed and independent of any pipeline's, because the whole point is that these
records do not fit the pipeline's schema. And its expiry is **mandatory**: a configuration without
a quarantine retention is refused at validation, in the same way a rehydration without an expiry
is refused. Expiry is built as **partition detach rather than row deletion** — the immutability
rule gets no exception, and a detach stays reversible until retirement's grace period runs.

### One bad record is an incident; a run of them is an outage

The control is a **rate over a recent window, not a total**. A total accumulates over the life of
a feed and eventually trips for reasons that are historical; a rate says what is happening now.
Above the configured fraction, the feed **stops**: it halts, says why, names the position of the
last record it accepted, and waits for a person.

It does not retry on a timer. A source whose shape changed produces all-bad records, and a
pipeline that sidelines them one at a time turns a schema change into a **silent data outage** —
everything green, nothing arriving. Auto-resume is how the same outage is rediscovered every five
minutes and acted on by nobody.

The threshold has a default, because a control an operator must invent a number for is one that
ships switched off. And the boundary is read as written: two in ten is *at* a fifth and not above
it.

```sql
SHOW FEEDS;
```

One row per declared feed: its name, whether it is `running` or `halted`, when it halted and why,
and how much it has published, quarantined and skipped. A feed that has never managed to run is
listed too — that is the case an operator most needs to see. Resuming is an act somebody performs:

```sql
RESUME FEED orders;
```

and **resuming does not forget**: the halt count survives it, because a feed that halted twice for
the same reason is not the same situation as one that halted once.

### The position is committed with the rows

A microbatch pipeline restarts. If where-we-got-to is recorded separately from what-was-published,
a crash between the two produces either duplicated rows or a silent gap, and which one depends on
the order somebody chose. So **the position is part of the same commit as the rows** — either both
are visible or neither is, which makes a restart a question with an answer rather than a
reconciliation exercise.

The design's first statement of this claimed more than it could support, and the soak found it
in under a minute. The position is a **high-water mark**: the last source finished, in name
order. That is what keeps it O(1) — the alternative, a set of every source ever read, grows for
as long as the feed runs. But a mark records *where a feed got to*, not *which files it read*,
so a source sorting below the mark is **indistinguishable** from one finished last week. The
first implementation reported everything below the mark as a late arrival, and in a spool
directory that keeps its files, that meant every previously-finished source was announced as
late on every run after the third.

The decision is which error to prefer, and it is: **never re-ingest.** A source that genuinely
arrives out of order is skipped rather than read. Duplication is silent and permanent — every row
twice, in a table somebody reconciles against, with nothing in the result saying so — where a
skipped source is a file still sitting in a directory, findable and replayable. What replaces the
refusal is a **count**: every run reports how many sources it skipped as already read. The number
is steady for a spool that accumulates and grows when sources start arriving behind the mark,
which is what turns an undetectable event into one an operator can notice.

Distinguishing the two cases properly requires remembering which sources were read, and no
bounded structure does it: a *recent* set answers "in the set" but cannot tell "long since done"
from "never seen" for anything that has fallen out of it.

## 9.11 The defect that made the case for front-door testing

`SHOW FEEDS` returned **one row with one empty column**. The command surface was built, wired,
unit-tested and mutation-tested, and none of that could see it, because the statement never
reached the code that implements it: the wire layer's lenient `SHOW <anything>` recogniser treated
`FEEDS` as a session setting named `feeds` and answered from the catalogue.

The fix inverts precedence — a handler may **claim** a statement it defines itself.

> **Key idea**
> **This is the fourth time a whole SQL surface has turned out to be unreachable from the thing
> that serves SQL.** The maintenance surface, the cube surface and the OLAP function set were the
> others. The pattern does not vary: a surface's own tests call the surface, and the layer above
> it is where the statement goes missing. That is the entire argument for executing through the
> front door with clients that know nothing about the code, and Chapter 23 develops it.

## 9.12 What is not built

| Not built | Note |
|---|---|
| **The streaming transport** | Changes are drained through a SQL function rather than a replication connection. The decoder and pipeline are transport-agnostic by design, but the transport itself is unwritten — and neither mainstream Rust PostgreSQL client supports the replication protocol, so this is real work rather than a wiring exercise. M2's remainder |
| **The slot lifecycle driver** | No creation policy and no position advancement on a timer. The decision functions exist; it is a decision function without a caller |
| **The backfill reader** | The handoff *contract* is built and verified against a live slot, but nothing yet reads the existing rows. **Only changes after a slot exists are captured** |
| The arrival tier as the architecture describes it | What exists is a retention contract. Missing: the epoch ring, the per-epoch key digests that let a historical query skip the tier at no cost, and per-tenant sub-caps. Nothing wires the tier into the ingest path either, so read-your-own-writes still waits for publication in practice |
| Partitioning on the ingest path | `sankhya-ingest` creates tables with no partition columns and writes flat. There is also no timestamp to derive a date from: the declared commit-timestamp system column is written as literal `0` for every row |
| Ingest on a timer | Nothing drives ingest in a running server, so everything it serves is already published |
| Automatic table onboarding on a timer | Built; nothing drives it |
| Streaming ingest from a client, and Kafka | M15, not started. Gated on a pin-set decision and on deciding how a consumer is tested without a broker — *a fake is the easy answer and the one that proves least* |

The honest summary, and it applies to the whole of this chapter: **the correctness contracts are
built and tested, and the machinery that runs them continuously is not.** Every capability above
is exercised by the test suite; the parts marked here are not yet exercised by a process you can
start.
