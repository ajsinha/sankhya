# 14. Observability

> This chapter covers what SANKHYA tells you about itself: the metric catalogue, the error
> catalogue, the runbooks they index, and the diagnostic. Its central claim is that observability
> fails by *inversion* — the usual arrangement makes a metric a string and its documentation an
> optional artefact, and the predictable result is a dashboard carrying series with no meaning, no
> unit and no owner, on which somebody builds an alert. Here the declaration is the call site, so
> an undeclared metric cannot be typed, a label cannot carry caller data, and the diagnostic
> reports a **date** rather than a value — which forces it to keep a history, and to refuse a date
> it cannot justify.

---

## 14.1 The catalogue is the API

Recording a metric takes the metric's **declaration**, not its name. There is no
`counter("some_name")` in this codebase, so an undeclared metric is not refused at runtime — it
cannot be typed. Every exported series therefore carries a documented meaning, a unit, a group and
a bound on its cardinality, because those are fields on the thing the call site had to pass.

The consequence is that [`METRICS.md`](../../METRICS.md) is generated from the declarations and
regenerated and diffed on every build. A metric absent from that document is not merely
undocumented: it is unrecordable.

**Two checks, and they are different checks.** That the published catalogue matches the
declarations is one. That the declarations match reality is the other — every declared metric must
be recorded *somewhere in the source*, or the catalogue is a wishlist published as documentation.

> **Key idea** — Generating documentation from a catalogue proves the document matches the
> catalogue. It says nothing about whether the catalogue matches the program. Both checks are
> needed and only the second is uncomfortable, because it is the one that fails.

## 14.2 A label is one of two things, and there is no third

`ARCHITECTURE` §17.1 forbids tenant data in any log line, trace attribute or metric label. Here
that prohibition is a **type** rather than a review item. A label declares either:

- a **closed set** of permitted values, where anything else is refused and counted; or
- a **deployment-scoped identifier** — a table, a tenant — under a **cap**.

There is deliberately no third variant, so a label that varies per row, per query or per user has
no way to be declared. Putting a value where a dimension belongs is simultaneously the tenant-data
leak and the cardinality explosion, and one construct prevents both.

Past the cap, new series are **refused and counted** rather than created. Three behaviours were
available and only one is defensible:

Behaviour | Outcome
---|---
Grow without bound | The process dies
Drop silently | The dashboard is quietly wrong
Refuse and report | The metric is visibly incomplete

A gap gets noticed; a quiet inaccuracy does not. `sankhya_metrics_rejected_total` is the series to
watch, and non-zero means either a call site disagrees with the catalogue or something has outgrown
its cap.

The same argument extends to logs, where it is easier to break by accident than on purpose.
`#[instrument]` records **every argument of the function it decorates**, so three words on
`fn query(&self, sql: &str)` put every statement any client sends into the log — predicate values
included — with nothing at the call site saying so. `cargo xtask check-logging` closes that, and
there is deliberately no suppression comment: a prohibition with an escape hatch is a prohibition
with escapes in it.

## 14.3 What a scrape actually contains

Its own port, one route, exact match, loopback by default — a metrics endpoint on every interface
is a small permanent disclosure of the deployment's shape, and the safe choice should be the one an
operator gets by not deciding.

Scraped from the review server this chapter was written against:

```
$ curl -s http://127.0.0.1:55433/metrics
# HELP sankhya_queries_total Statements that reached execution, by how they ended.
# TYPE sankhya_queries_total counter
sankhya_queries_total{outcome="error"} 50
sankhya_queries_total{outcome="ok"} 89
# TYPE sankhya_query_duration_seconds histogram
sankhya_query_duration_seconds_bucket{outcome="ok",le="0.025"} 81
sankhya_query_duration_seconds_sum{outcome="ok"} 1.0876959490000002
sankhya_query_duration_seconds_count{outcome="ok"} 89
sankhya_rows_returned_total 316
sankhya_connections_active 0
sankhya_audit_records_total 138
sankhya_table_live_files{table="common.orders"} 1
sankhya_table_live_files{table="probe_e.scratch"} 1
sankhya_memory_in_use_bytes 804242
sankhya_memory_peak_bytes 3272022
sankhya_metrics_rejected_total{reason="over_cap"} 0
```

Four things in that are load-bearing.

**`refused` is not `error`.** A quota held and a permission enforced are the system working.
Counting them with genuine failures makes a healthy system under load indistinguishable from a
broken one, which is how an error-rate alert comes to fire on correct behaviour. The four outcomes
are `ok`, `error`, `refused` and `cancelled`, and `refused` is derived from the SQLSTATE class —
`28000`, `28P01`, `42501`, `53200`, `53400`. A malformed statement is an `error`, which is right: a
typo is the caller's fault and not the system holding a limit.

**`sankhya_metrics_rejected_total` appears at zero on all four of its reasons**, so a dashboard can
tell *no events* from *not wired up*.

**Memory is counted at the global allocator**, not at the query engine's pool. The pool tracks what
its operators reserve, which is most of what a query uses and not all of it — decode buffers,
network buffers and every third-party allocation sit outside it. A query can stay inside its
reservation and still exhaust the machine. Two atomic loads at scrape time is cheaper than any
timer, and a timer would report the previous era.

**`sankhya_table_live_files` is the compaction-debt series**, and it is the only one in this build
that pages. Chapter 15, *Maintenance, tiering and the data lifecycle*, explains what it measures.

> **Pitfall** — This gauge is built from the table set the server resolved at boot. In the session
> that produced the scrape above, two clones were dropped and their series continued to be exported
> at zero. A dropped table leaving a gauge behind is a small thing; a dropped table still answering
> queries, which the same cause produces, is not. §14.7 records it.

## 14.4 The metrics that page, and the three that are absent

`ARCHITECTURE` §17.1 names four metrics that receive paging alerts, because each precedes a
user-visible failure by a predictable interval. **A metric that may page must name a runbook**, and
the field is not an `Option`: the build check requires the file to exist *and* to carry its
*Symptom* / *What is actually wrong* / *What to do* sections. Seven runbooks exist.

Of the four, **one is emitted**:

Metric | State | Lead time
---|---|---
`sankhya_table_live_files` | Emitted, pages, runbook `compaction-debt` | Days at ordinary write rates
Retained log volume | **Not emitted** — no ingest runs in this process, so no slot retains anything | —
Transaction-identifier freeze age | **Not emitted** — a property of PostgreSQL, read by a supervisor that is not wired in | —
Archive jobs awaiting attention | **Not emitted** — archival is gated; see Chapter 15 | —

The three absent ones are listed in `METRICS.md` rather than declared, and that is the decision
worth defending. **A gauge permanently reading zero is indistinguishable from a healthy
subsystem.** Publishing three of them would produce a dashboard on which the replication lag is
always fine, the freeze age is always young and the archive queue is always empty — for a
deployment where none of those things is being measured at all.

The interval by which a metric precedes user-visible failure is recorded alongside it, because that
interval is the entire justification for paging. An alert with no lead time fires when the user
notices, which makes it a notification.

## 14.5 Every error carries a code, a class and a remediation

The error catalogue drives six behaviours from one classification — retry policy, protocol status,
SQL state, log level, metric labelling and alerting — and it is generated into
[`ERRORS.md`](../../ERRORS.md) from the same declarations. **Codes are permanent.** Removing or
renumbering one breaks every runbook, alert rule and support script that references it, so the
catalogue only ever grows.

The class is the letter after `SNK-`, and it is what a client actually branches on:

Class | Meaning | What a caller should do
---|---|---
`C` | The caller's request was wrong | Do not retry unchanged; do not page
`R` | A limit was reached | Shed load; retrying immediately makes it worse
`F` | A concurrent writer won | Re-plan against the new state and retry
`T` | Transient | Retry after the hint
`X` | Abandoned deliberately | No action — a deadline, a disconnect, a drain
`S` | An invariant does not hold | **Pages.** Each has a runbook

A failed statement on the review server:

```
psql> SELECT * FROM common.ordres;
ERROR:  [SNK-C0001] Error during planning: table 'datafusion.common.ordres' not found
DETAIL:  Correct the statement. The detail names the offending element.
```

`DETAIL` is the catalogue's own remediation, so the client and `ERRORS.md` cannot say different
things. The SQLSTATE comes from the error's **class**, never from its wording: every driver in this
ecosystem branches on those five characters, and a plausible message with the wrong ones produces a
client that connects, appears to work, and mishandles every failure.

> **Key idea** — The path a person actually takes has to go through the catalogue, and that is the
> part that gets missed. A catalogue can be complete, classified and published while the wire path
> returns the engine's own message with a status guessed from substrings — so the errors a user
> meets most often are precisely the ones with nothing to look up. That was true here until `M6`,
> and exit criterion 6 was false while the catalogue sat correct and unreachable.

Mapping engine failures onto catalogue entries is matched on the failure's **variant**, never on
its text. Substring matching is a mapping that changes silently when a dependency rewords a
message, and the symptom is a client that stops retrying something it should retry. This system has
the scar: DataFusion 55 began wrapping plan errors in a `Diagnostic` to attach a source span, the
match on `Plan` stopped firing, and *table not found* — the commonest error there is — fell through
to the catch-all.

**A statement the system will not honour is refused, never accepted and discarded.** `CREATE TABLE`
once returned a success tag and did nothing durable: the table existed for the rest of that
connection and was gone on reconnect. Not an error, not a wrong number — *a confirmation of
something that did not occur*, which is the worst shape available, because it produces no evidence
at all. It now answers:

```
ERROR:  [SNK-C0006] the statement uses a feature this build does not implement: data modification
        is not served over this connection; this server is a read path over a published warehouse
DETAIL:  Write to the transactional store and let capture publish it, or publish an external table
         with `sankhya-publish`. See GUIDE.md §3.
```

### Where the catalogue does not yet reach

Verified on the running server: the statement paths added in `M10` and `M13` — cloning, lineage,
feeds — refuse **without a `SNK-` code and with an empty `DETAIL`**, carrying a generic SQLSTATE:

```
DROP TABLE probe_e.frozen
  sqlstate  22000
  message   `probe_e.frozen` is still read by probe_e.audit. Removing it is the deletion cloning
            is gated on, arriving through the front door --- materialise them first, or drop them
  detail    (empty)
```

The message is excellent and the envelope is not. A client cannot dispatch on it, `ERRORS.md` does
not index it, and it is counted as an `error` rather than a `refused`. It is the same shape as the
`M6` defect above, one milestone later and on a newer surface, and it is recorded here rather than
smoothed over. Chapter 20, *The client contract and the SDKs*, §20.5 shows what that costs a binding.

## 14.6 The diagnostic reports a time, and refuses to invent one

`FR-OPS-17` requires the diagnostic to report **time until a problem becomes user-visible** rather
than its current value, on the grounds that *"compaction debt is 400 GB"* is far less actionable
than *"query latency on this table will double in about nine days."*

The architectural consequence is not in the requirement and is easy to build around: **a time
cannot be computed from one sample.** It needs a rate; a rate needs observations separated in time;
observations separated in time need somewhere to live between runs. A diagnostic that computes
projections beautifully and keeps no history satisfies the requirement in code and never once in
operation, because every run is the first run.

So `sankhya-server doctor` owns a small append-only observation history, and three properties of it
are deliberate:

- **Beside the warehouse, not inside it.** The warehouse is the thing being diagnosed, and may be on
  storage that is full or unwritable — which may itself be the finding.
- **Not a table in this system.** A diagnostic that needs a healthy database to report an unhealthy
  one is decoration. For the same reason `doctor` reads the warehouse directly and **does not start
  the server**: the day you want a diagnostic is frequently the day the server will not start.
- **Text, and damage is expected.** A process killed mid-append leaves a torn line. That line is
  skipped and counted, and the count is reported. Refusing to start over a truncated line removes
  the tool at the moment somebody reaches for it; hiding the count lets a history quietly losing
  half its lines produce confident dates.

Run against the review warehouse for this chapter:

```
$ sankhya-server doctor
SANKHYA doctor 0.1.0
  warehouse .../review/warehouse
  12 table(s)

  [critical] backup — no restore drill has ever passed; this threshold has already been crossed.
         Run a restore drill: `sankhya-server drill`. If it fails, the backup is not a backup and
         this is an incident rather than a maintenance task. See docs/runbooks/restore-drill.md.

12 check(s) clean, 1 finding(s) of which 1 have a date, 0 check(s) could not run
$ echo $?
1
```

That finding is **Already** — past the threshold now, an incident rather than a warning — and it is
the one check in the crate that gives a firm date on a *first* run. Everything else needs two
samples because a value alone implies no rate. Backup staleness rises at exactly one second per
second and always has, so it needs no observing. The instinct is to feed it through the same trend
machinery as everything else, which would collect a week of samples to estimate a rate that is
already known exactly, and report *too few observations* in the meantime about the one thing that
needs none.

### The four answers, and the four refusals

Answer | Meaning
---|---
**Already** | Past the threshold now
**Crossing** | A date, with a confidence — two observations give `Weak`, five or more `Firm`
**Receding** | Moving away, or flat. **Not reported** — reporting it teaches an operator to skim
**Beyond / Unknown** | It will not say, and names why

It refuses a date in four distinct situations, each named in the output:

- **Too few observations.** Fewer than two. The first run, always.
- **Not linear.** A sawtooth — debt accumulating and being compacted away — fits a line badly *by
  construction*, and a date drawn through one reports where in the cycle the samples happened to
  fall.
- **Beyond the horizon.** Four days of samples projecting six months out is arithmetic, not
  evidence. The horizon is three times the observed span.
- **No elapsed time.** Every observation shares an instant.

A measure that is *near* the threshold still speaks up without a date, at `note` severity. Silence
at 990 of 1,000 files reads as health, and it is not.

> **Key idea** — Findings are ordered by *when*, not by severity. Severity orders a list by how
> loudly each item shouts; time orders it by which one must be dealt with first. A warning that
> becomes an outage tomorrow outranks an error that has been stable for a month. Reading top-down
> should be reading a schedule.

### "Could not run" is not "found nothing"

Both produce an empty finding list and they are opposite facts. The exit status keeps them apart:

Exit | Meaning
---|---
`0` | Clean
`1` | Findings
`2` | At least one check could not run

**Alert on `2`.** A monitoring system that treats *I could not look* as *nothing found* reports
all-clear for a subsystem nobody examined, which is the specific failure the whole crate is
arranged against.

Hourly from cron is what makes the projections real:

```cron
17 * * * * SANKHYA_WAREHOUSE=/srv/sankhya/warehouse /usr/local/bin/sankhya-server doctor
```

## 14.7 What is checked, and what is not

Check | State
---|---
`compaction-debt` | Built end to end, threshold 1,000 live files per table
Backup staleness | Built, and firm on a first run
Archive attestation age | Built, and reported only for a deployment that archives anything
`storage-headroom` | Built as a check with **nothing feeding it observations** — reading free space needs a platform call this workspace's `forbid(unsafe_code)` will not permit, so the caller passes the number in
`replication-lag` | Built as a check, **not wired** — nothing in this process advances a replication position
Conformance, replica identity, archival consistency | **Not built.** `FR-OPS-16` names them

**Distributed tracing is not built.** Metrics and the error catalogue exist; spans do not. When they
arrive they are per stage rather than per operator — per-operator spans on a plan with thousands of
batches cost more than the query — and query text is data, so a normalised plan hash is logged by
default with full text only under explicit policy and routed to the audit store.

One behaviour of the running server was observed while writing this chapter and belongs here rather
than in a defect list somebody else keeps, because the metric surface is where an operator would
have to notice it and does not.

**A table that was present when the server started, and is dropped afterwards, stays served.** The
`DROP TABLE` succeeds and removes the table's directory from the warehouse. Afterwards, verified on
the running server:

```
$ ls <warehouse>/probe_e/
scratch

psql> SELECT table_name FROM information_schema.tables WHERE table_schema='probe_e';
 audit | frozen | scratch

psql> SELECT count(*) FROM probe_e.frozen;
 250

psql> SHOW LINEAGE OF probe_e.frozen;
ERROR:  there is no table called `probe_e.frozen` on this server
```

The clone records agree with the warehouse; the served table set does not, and the served set is
what answers. A `sankhya_table_live_files` series is still exported for both ghosts, at zero — which
is indistinguishable from an empty table rather than from a table that is gone. A table created
*and* dropped within one server's lifetime disappears correctly, so the fault is in what boot
registers rather than in the drop. Chapter 12, *Cloning and lineage*, covers the splice that supplies
those 250 rows; Chapter 15, *Maintenance, tiering and the data lifecycle*, §15.3 covers the
re-resolution that maintenance already forced for the *advancing* case and that the *removed* case
does not yet get.
