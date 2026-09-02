# 2. The thesis

> This chapter argues that the three-engine data estate is not a set of tools that happen to
> sit beside each other but a single architectural mistake made three times, and that the
> mistake is now correctable. The central claim is that one governed copy of the data, held in
> one address space, with semantics declared rather than inferred, is cheaper *and* more
> correct than three copies reconciled — and that the second half of that sentence matters
> more than the first. Five clauses follow, each defended by mechanism and arithmetic, and
> each of which degrades to marketing if any of the other four is dropped.

## 2.1 The claim, in one page

SANKHYA's design reduces to five clauses. Every later chapter in Part II is the defence of one
of them.

1. **One governed copy of the data.** There is one row for a fact. The transactional store
   holds it, the analytical tier is a derived and versioned view of it, and the graph tier is
   hydrated from that same view. An edge exists because a row exists. No tier holds private
   state that another tier could contradict.

2. **One address space.** The transaction, the aggregate and the traversal happen inside one
   process, over one memory representation — Arrow — with no wire hop, no serialisation
   boundary and no second security model between them. This is a performance claim second and
   a correctness claim first.

3. **Semantics are declared, never inferred.** What a date column *means*, whether a measure
   may be summed along a dimension, what a feed does with a field it has not seen before —
   each is a declaration a person committed to, refused at definition time when it is absent,
   rather than a default the system chose on the user's behalf.

4. **A system that cannot represent an answer refuses it.** Refusal is the product's dominant
   verb. A type that cannot round-trip makes a table ineligible; a roll-up of an undeclared
   measure is rejected at planning time; a record that does not fit the declared shape is
   quarantined whole rather than coerced. The refusal names what to do about it.

5. **Every claim is tested rather than asserted.** "Zero data loss" is a measured metric with
   an alarm, not a sentence. The open-storage claim is exercised by a foreign reader in the
   test suite. Concurrency numbers are taken twice in the same run — once as the code stands,
   once with the same work forced through one mutex — because a single global lock satisfies
   every *safety* property while destroying the property being claimed.

> **Key idea**
> The five clauses are not independent. One copy is only safe if the semantics are declared,
> because a single copy with an inferred meaning is a single copy of an ambiguity. Declared
> semantics are only enforceable if the system refuses. Refusal is only tolerable if the
> refusals are true, which is what clause 5 is for. And none of it survives if the three
> engines are three processes, because then clause 1 is a synchronisation problem rather than
> a fact about storage.

## 2.2 Defending clause 1: one copy, and the arithmetic of the alternative

The standard estate keeps the same fact in an operational database, in a warehouse, in a graph
store, and in however many extracts sit between them. The usual defence is that each store is
specialised for its access pattern, which is true and beside the point. The cost is not
storage. The cost is that the copies are taken at different instants against different
definitions, and the difference is then a permanent function of the business.

The arithmetic is unforgiving and it is worth doing once.

Take a population of 10,000,000 open positions, arriving at a steady 1,000 per minute. Two
downstream systems extract it, and their cut-offs differ by three minutes — not because anyone
was careless, but because two schedulers were configured independently, four years apart, by
two teams.

| Quantity | Value |
|---|---|
| Population | 10,000,000 rows |
| Arrival rate | 1,000 rows/minute |
| Cut-off skew | 3 minutes |
| Rows in one system and not the other | 3,000 |
| Discrepancy as a fraction | 0.03% |

Three thousand rows is the worst possible size. It is small enough that it looks like a
rounding artefact and gets an exception filed against it, and large enough that if those rows
are concentrated — a single large counterparty booking at the close, say — the aggregate they
feed is materially wrong. Nothing in either system is broken. Both extracts are correct
extracts. The discrepancy exists because there are two.

Add the second axis and it compounds. Each copy carries its own definition of a filter. If
"open" means *not settled* in one system and *not settled and not cancelled* in the other, the
0.03% acquires a second term that does not shrink with better scheduling, because it is not a
timing defect at all.

> **Key idea**
> The reconciliation function is a pure cost with no analytical output. It is caused by the
> architecture rather than by carelessness, and no vendor sells a cure, because curing it
> collapses three licences into one. Eliminating it is worth more than any performance number
> in this book.

## 2.3 Defending clause 2: one address space, and what a hop costs

Clause 2 is where the engineering claim lives, and it is the one most likely to be dismissed as
an implementation detail. It is not. Each hop between engines costs three things, and only the
first is a performance cost.

**The representation cost.** A bulk extract that begins as columnar Arrow on disk and ends as
columnar Arrow in a client's buffer pays nothing for its shape. A single row-oriented hop
anywhere in that path — a JDBC `ResultSet`, an ORM, a JSON gateway, or the PostgreSQL wire
protocol itself — imposes a per-value cost that dominates everything else at extract scale.

The arithmetic is a property of the protocol rather than of any implementation. PostgreSQL's
`DataRow` message carries a four-byte length prefix per column value. Take a modest extract:

| Quantity | Value |
|---|---|
| Rows | 50,000,000 |
| Columns | 20 |
| Values | 1,000,000,000 |
| Length prefixes at 4 bytes each | 4.0 GB |

Four gigabytes of framing, before a single byte of data, on a path where the data was columnar
on disk, columnar in memory, and columnar through every operator — and is then taken apart so
the receiver can put it back together. This is why Chapter 5 puts Arrow Flight SQL beside the
wire protocol rather than instead of it: the row protocol is the right answer for a screenful
and the wrong answer for a night's extract, and the difference is arithmetic rather than
taste. [ADR-0006](../../adr/0006-flight-sql.md) records the decision.

**The lag cost.** Every hop has a scheduler, and every scheduler has a window. Risk sees a
position hours after the trade is booked because four windows were placed end to end, each
sized by a team that could only see its own.

**The divergence cost.** This is the expensive one and it is the one clause 2 exists to
remove. A hop is a place where two systems can disagree about a schema, a null, a time zone or
a rounding convention, and each such disagreement is discovered in production, once, by
somebody who cannot fix it. Removing the hop does not make the disagreement less likely to be
noticed; it makes it *unrepresentable*, because there is only one schema.

> **Pitfall**
> The tempting relaxation is "one logical copy, several physical stores, kept in sync". It
> sounds like clause 1 and it is clause 1's opposite. Synchronisation is a protocol, protocols
> have failure modes, and the failure mode here is the copies diverging while every component
> reports healthy. The whole point of one address space is that there is no protocol to fail.

## 2.4 Defending clause 3: declared, not inferred

Three of this system's more distinctive decisions are the same decision made in three places,
and the pattern is worth naming because it looks like pedantry until the failure arrives.

**A date's meaning is a property of the table.** Every table carries `sank_data_date`, of type
`DATE`. The obvious design — take the supplied value, else today — is rejected. A default taken
from write time makes the column's meaning vary per row without saying so, and then
`WHERE sank_data_date = '2024-03-01'` returns a *mixture* of rows meaning "this happened that
day" and rows meaning "we received this that day", with no query able to separate them
afterwards because the distinction was never recorded. So provenance is declared once per
table: either a named source column supplies it, in which case a null in that column is an
error rather than a fallback, or the table records that its dates are ingest dates.
Chapter 7 develops this, and [ADR-0004](../../adr/0004-the-date-axis.md) is the decision.

**A measure declares how it may be combined, per dimension.** A measure with no declared rule
is refused at definition time, not defaulted to `SUM`. Summing twelve month-end closing
balances gives a number of the right magnitude, the right sign and no meaning. Averaging an
average is right only when every group is the same size, and groups are never the same size.
Chapter 10 develops this, and [ADR-0007](../../adr/0007-the-cube-model.md) is the decision.

**A feed declares what it does with what it has not seen.** Silently widening a type, inventing
a value for a missing key, accepting keys it has never seen, coercing a string to a number —
each is refused at configuration validation, and the validation names *every* rule that failed
rather than the first, because fixing them one build at a time is how a person gives up.
Chapter 9 develops this, and [ADR-0018](../../adr/0018-a-record-that-does-not-fit.md) is the
decision.

The common structure: in each case the inferred alternative produces data that is wrong and
*looks right*, with nothing null, nothing missing and no error raised. That class of defect is
what this system is organised against, and the price is that somebody must make a declaration
at definition time who would rather be querying.

## 2.5 Defending clause 4: refusal as the dominant verb

A system that refuses is unpleasant to use for exactly as long as it takes to meet the first
number it would otherwise have got wrong. Three properties make refusal usable rather than
merely obstructive.

**A refusal is data, not a sentence.** It carries a stable `code` a client dispatches on, a
`sqlstate` a generic driver understands, a `remediation` saying what to do, and `subjects` —
the names the refusal cites, as a list. Without `subjects`, a client that wants to display
"three clones read this table" must parse the message, and the message becomes an API nobody
meant to publish and nobody may reword.
[ADR-0017](../../adr/0017-the-client-contract.md) is the decision.

**A refusal fails closed.** The alternative to refusing is warning, and a warning defers the
decision to whoever reads the log, which in practice is nobody.

**A stream cannot refuse the way a statement can.** This is the honest limit of the rule, and
it produces the one place where the system deliberately does something other than refuse.
There is nobody to tell: the producer wrote the record and moved on, and the connection that
carried it is closed. Stopping the pipeline for one malformed record turns one bad record into
an outage, which is how ingest systems come to be run with every validation switched off. So a
record that does not fit is quarantined *whole*, with the reason as a code and as a sentence,
the configuration version that refused it, and the coordinates a replay needs — and the
quarantine is an ordinary governed table with a mandatory expiry, not a directory of rejects
beside the warehouse. One bad record is an incident; a *rate* of them over a recent window is
an outage, and it halts the feed and waits for a person rather than retrying on a timer.

## 2.6 Defending clause 5: measured against a control

The fifth clause is the one that decides whether the other four are true or merely intended,
and it has a specific methodological shape: **a claim is measured against the design it
forbids.**

The concurrency work is the clearest instance. Every safety property — no lost commit, no
partial read, no file deleted while read — is satisfied perfectly by a single lock over the
warehouse, which is also the design that destroys the throughput the system exists to provide.
A throughput measurement taken without a serialized arm beside it therefore cannot distinguish
the design from the one it forbids, and a threshold chosen without both states is taste. So
every concurrency claim is taken twice in the same run on the same machine.

| Property | As built | Serialized control |
|---|---|---|
| Commits to eight tables, relative to one table's rate | 4.82× | 0.91× |
| Reader's share of its idle rate under four writers | 0.59–0.80, p99 227 µs | 0.00–0.07, waits seconds |
| Sixteen writers on one contested version | all commit, worst rebase count 11 | — |

Those figures and the method that produced them are recorded in
[ADR-0013](../../adr/0013-concurrency-and-data-safety.md), including four successive
corrections to the measurement itself — the arms interfering with each other, a capacity probe
that counted `iowait` as idle, a window that held at both ends but not in the middle. The
corrections are part of the claim. A measurement whose failure modes are undocumented is an
assertion with a number attached.

The same discipline appears elsewhere in shapes worth naming now and developing later: a
materialised cube must return **bit-identical** answers to a computed one, not close ones,
which forced materialised aggregates to be stored unrounded as exact expansions
(Chapter 10); an adversarial review is run through the front door by clients that know nothing
about the code, because a surface's own tests call the surface and the layer above it is where
the statement goes missing (Chapter 23).

> **Pitfall**
> "Tested" is not the same as "tested by a test that would have failed". Between 2026-08-31 and
> 2026-09-01, four defects were found in this system and **every one of them had passing
> tests**, several had passing mutation tests, and none was found by the suite. The pattern did
> not vary: a surface's own tests call the surface, and the statement goes missing one layer
> up. That is why Chapter 23 spends its length on how the tests are checked against the defects
> they claim to catch, rather than on how many there are.

## 2.7 What the thesis costs

An honest thesis states its own bill.

- **One process is one blast radius.** Three engines fail independently; one engine does not.
  The mitigations — bounded cancellation, a hostile aggregation refused rather than taking the
  process down, capability starvation for extensions — are described in Chapters 5 and 21, and
  they are mitigations rather than a denial of the cost.
- **One node is a ceiling.** The largest single query is bounded by one node's memory and cores.
  [ADR-0015](../../adr/0015-the-shard-set-seam.md) decides that a table reference resolves to
  file groups beneath exactly *one* log, and refuses independently-committed shards rather than
  deferring them, on the grounds that the cheapest correct cross-shard commit is a lock over
  the shard set — which is the forbidden design arrived at from a different direction. Chapter
  4 states the boundary plainly.
- **Declaring is work.** Aggregation rules, date provenance and feed shapes are all work at
  definition time, on somebody who would rather be querying.
- **Determinism is slower.** Every reducing kernel goes through a compensated, ordered sum. The
  elementwise half vectorises; the reduction does not. That is the price of the guarantee, and
  [ADR-0005](../../adr/0005-array-columns-and-numeric-kernels.md) records it as a knowing
  trade rather than an oversight.
- **The system is under construction.** Chapter 26 and the repository's `STATUS.md` are
  authoritative; Chapter 4 names the boundaries that are permanent, and every chapter in
  Part II marks the milestone where a designed-but-unbuilt capability lands.

## 2.8 Why now

The thesis is not new. Collapsing the estate has been proposed and attempted repeatedly, and it
failed for reasons that were real at the time: a single engine could not be both transactional
and vectorised without one half being a toy; columnar storage was proprietary, so consolidating
meant accepting a silo; and the analytical tier was a JVM cluster whose operational estate
dwarfed the problem it solved.

Four things changed, and the bet is that together they are sufficient.

| Change | What it removes |
|---|---|
| Arrow as a shared in-memory representation | The conversion between engines, which was most of the cost of talking to more than one |
| A vectorised query engine as a *library* (DataFusion), not a cluster | The scheduler, the broker, the connector fleet and the JVM |
| Open lakehouse table formats with a transaction log | The proprietary-silo objection: another engine reads these tables directly, with no SANKHYA process in the path |
| PostgreSQL logical replication as a native protocol | The Kafka/Connect/Debezium tier between the system of record and everything downstream |

None of the four is this project's invention, and the thesis does not depend on any of them
being novel. It depends on their being simultaneously available and mutually compatible in one
process, in one language, with no JVM anywhere — which is a claim about a moment rather than
about a technology, and it is checkable.
