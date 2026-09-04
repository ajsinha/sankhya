<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# Production-readiness audit

**Date:** 2026-09-03 · **Commit audited:** `710848b` · **Verdict:** not production-ready
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> **The status line above was itself a finding, and has been corrected.** See `CLM-01`. The line
> this report was written against read *"M0–M8, M10 and M13 complete"*, which four rows of
> STATUS's own table contradicted. Correcting it was item 0.7 of [`REMEDIATION.md`](REMEDIATION.md),
> and widening `check-docs` to see partial completion --- it had looked only for the literal
> *"in progress"* --- immediately caught a **fifth** contradiction none of the twelve audits
> found: M13 is *substantially built*, and every document called it complete.

## What this document is

The consolidated findings of twelve independent audits, each given one axis and told to read
deeply, cite file and line, separate what it traced from what it suspected, and **not to fix
anything**. It exists to be turned into a roadmap: every finding carries a stable identifier, so
a plan can reference `COR-08` rather than restate it.

Findings are grouped by **what happens if it is not fixed**, not by which auditor found them.
Several appear under one identifier and were found independently by two or three auditors; that
corroboration is noted, because it is evidence.

**How to read a finding.** `CONFIRMED` means an auditor traced it to the load-bearing lines.
`EXECUTED` means it was run and the wrong output observed. `SUSPECTED` means the mechanism looks
wrong and could not be fully verified. `VERIFIED BY LEAD` marks the fifteen I checked myself
before publishing, because a wrong critical finding costs more than a missed one.

## The verdict, in one paragraph

The system is not production-ready, and the reason is not the defect count. It is that **a green
gate did not see any of this**: 2,642 tests, 741 mutations and twenty checks, against silent data
loss on three production paths, a door with no lock, and a summation kernel that returns zero for
a real number. The engineering underneath is unusually careful — the row-filter enforcement, the
commit protocol, the lease machinery and the metrics catalogue are all better than they need to
be. The failure is not of care. It is that the checks verify what they were built to verify while
the claims drifted outside their scope, and the tests that should have caught the rest were
written against shapes the production caller never produces.

## The audits

| # | Axis | State |
|---|---|---|
| 1 | Correctness and data safety | Complete |
| 2 | Security, authorization and disclosure | Complete |
| 3 | Operability and failure modes | Complete |
| 4 | Client-facing surfaces | Complete |
| 5 | Claims versus reality | Complete |
| 6 | Feature completeness against intent | Complete |
| 7 | First-run experience | Complete |
| 8 | Performance claims and benchmark integrity | Complete |
| 9 | Dependencies, licensing and supply chain | Complete |
| 10 | Format evolution and upgrade safety | Complete |
| 11 | Open-format conformance | Complete |
| 12 | Ingest, change capture and reconciliation | Complete |

Audits 6–12 will be appended as sections below when they report. **Nothing in the roadmap built
from this document should be considered final until they are in** — a licence blocker or an
unreadable-on-upgrade format changes what "fix first" means.

---

# Tier 0 — Data is lost, silently and unrecoverably

Every finding here destroys data that was successfully written and acknowledged. None of them
produces an error. All are CONFIRMED.

### `COR-01` A clone's pins never reach its origin's sweeper
`crates/sankhya-maintenance/src/service.rs:393`, `crates/sankhya-clone/src/family.rs:157`

The sweeper derives the table name from the directory (`"orders"`); the lineage records the
qualified name (`"sales.orders"`). The comparison is false **in every deployment**, not
sometimes — `discover` only ever walks `<warehouse>/<schema>/<table>`, so the qualified form is
always used. A clone's files are therefore invisible to the retirement guard and are deleted
after the grace period. `SELECT` from the clone then reads short, with no error.

This is the exact M10 failure `ADR-0016` exists to prevent. The test at
`crates/sankhya-maintenance/tests/service.rs:279` builds the table at the warehouse root and
records a bare name — the one shape in which the mismatch cannot appear. Mutation entry
`tools/mutation-audit.py:1500` is killed by that same test, so **the catalogue reports coverage
of a line whose defect no test can reach**.

### `COR-03` The orphan sweep ignores snapshot pins
`crates/sankhya-maintenance/src/service.rs:324`

`retire_due`, a hundred lines below, correctly unions clone pins *and* snapshot pins under a
comment reading *"Reclamation has exactly one question… Two rules disagree eventually, and the
one that loses deletes a file somebody is reading."* The orphan sweep is the second rule.

Deterministic, not racy: retirement correctly declines a pinned file forever, which **guarantees**
it crosses the orphan sweep's seven-day threshold. Any snapshot older than a week loses its files.

### `COR-02` Compaction output names restart at zero on restart
`crates/sankhya-maintenance/src/service.rs:157`, `crates/sankhya-table/src/write.rs:95`

The output sequence is a per-process tick starting at zero; the writer is a truncating
`File::create` with no staging. After a restart, tick 1 recomputes a name tick 1 already used —
and the compaction planner selects any live file under the target size, so the old file is
selected as its own output. It is truncated, then `Add(path)` followed by `Remove(path)` in one
commit removes the merged partition from the live set entirely.

The identical defect was found and fixed on the ingest path, with two mutation entries. Nothing
on the maintenance path recovers a sequence from the log.

### `COR-06` Two publishers collide on a data-file name
`crates/sankhya-publish/src/publish.rs:666`, `:423`

Both writers recover the same sequence; the loser rebases and re-commits **the actions it built
before the race**, never re-checked against the winner. A second `Add` on one path replaces in
place, so the winner's acknowledged rows vanish from disk and from the log. The concurrency test
gives each writer a distinct filename and cannot see it.

### `OPS-09` Backup manifests pin nothing
`crates/sankhya-server/src/backup.rs:41-122`

`sankhya_backup::protect::Protection` implements the retention guarantee correctly and is
**never called outside its own crate**. The server records `(table, version)` pairs and creates
no snapshot, so nothing holds the files back. Back up on Monday, let compaction run, and by
Tuesday the inputs that version depends on are gone. **This fires on schedule, not on a crash.**

### `COR-14` There is no `fsync` anywhere
`grep -rn "sync_all\|sync_data\|fsync" crates/*/src` returns nothing

`atomicfs` delivers visibility — no reader sees a torn write, and `tests/races.rs` proves it —
but not durability. No directory handle is opened after any rename or link. Power loss after
`commit()` returns can present a commit with a short or zero-length Parquet behind it.
`docs/INVARIANTS.md:85` reads as a durability claim. Documented as deliberate in
`docs/book/part3/16-durability.md:298`; the **consequence** is not documented.

### `OPS-15` A crash mid-commit reads as a successful *empty* commit
`crates/sankhya-atomicfs/src/lib.rs:85-92`, `crates/sankhya-table-delta/src/log.rs:500-509`

`hard_link` is not covered by ext4's `auto_da_alloc` heuristic, so the directory entry can be
durable while the data blocks are not. Replay reads empty text as **zero actions, no error**, and
the commit is accepted as one that added nothing — dropping every Parquet file it named from the
live set. It is cemented rather than transient: a retry is refused as `VersionTaken` and the next
version commits on top. **The commit format has no checksum, no length prefix and no
end-of-record marker**, so a short body is undetectable by construction.

### `COR-15` Nothing prevents two servers on one warehouse
No lock file, pid file or advisory lock exists. `atomicfs::claim` serialises commits *at a
version* and nothing else — which is what `COR-02` and `COR-06` leak through.

### `OPS-13` Three further silent paths to deleting pinned data
`crates/sankhya-server/src/snapshots.rs:380`, `:387`, `crates/sankhya-maintenance/src/service.rs:401`

A corrupt snapshot document, a snapshot naming an ambiguous table, and a read error on the pin
lookup each **drop the pin with the complaint discarded**. Compounded by `main.rs:496`, where a
poisoned mutex silently stops the pin-refresh loop for ever — the maintenance side handles poison
correctly, the writer side does not. The grace window is 12 minutes and `grace_ticks` is not
configurable.

### `OPS-14` A leaked lease stops reclamation for ever
`crates/sankhya-maintenance/src/service.rs:442`

The code states that the grace period *"remains as a backstop for the case where a lease is
leaked… because a registry with a leak and no backstop reclaims nothing for ever"*, and then
writes `if old_enough && unreachable` — an **and**. The backstop is not a backstop. `pending`
grows unbounded in RAM while disk fills with both copies of every compacted partition.

---

# Tier 1 — Anyone can get in, and do anything

### `SEC-01` No password is ever verified — VERIFIED BY LEAD
`crates/sankhya-server/src/wiring.rs:913`

The entire check is that the password is **present and non-empty**. There is no credential store,
no hash and no comparison anywhere in the workspace. `require_password: true` means *"send any
non-empty byte string"*. The username is self-asserted, so **any client connects as any user and
receives that user's roles** — which makes the per-subject authorization built on 2026-09-03
decorative on the wire door.

The startup banner prints `password required`, so an operator reads the opposite of the truth,
and the principal is stamped `Authentication::Password` in the audit chain. `docs/GUIDE.md:108`
describes a client "refused at the handshake — before it can send a password".

The test that names this behaviour asserts the defect: `tests/wiring.rs:193` is
`assert!(with.authenticate(&user, Some(b"anything")).is_ok())`. The real test
(`crates/sankhya-api-pg/tests/connection.rs:334`) runs against a test-local handler, not the
server's.

Found independently by three auditors. **This is what makes `SEC-06` remotely reachable.**

### `SEC-06` Path traversal from three statement names — VERIFIED BY LEAD
`crates/sankhya-server/src/aggregations.rs:43`, `crates/sankhya-server/src/snapshots.rs:41`, `crates/sankhya-cube/src/catalogue.rs:344`

`warehouse.join(DIRECTORY).join(format!("{name}.json"))`. The only constraint is non-empty and
whitespace-free, and `Path::join` **replaces the whole path on an absolute component**. So
`CREATE AGGREGATION /var/tmp/x LANGUAGE PYTHON AS $$…$$` writes to an arbitrary absolute path,
and `DROP SNAPSHOT ../_cubes/regional` deletes a cube definition.

The worst target is a snapshot document: an absent snapshot pins nothing, so **deleting it
releases the files the sweeper was holding back** — the module's own comment names that as the
deletion the mechanism is gated on. `sankhya-clone` restricts identifiers correctly for exactly
this job; the other three do not.

### `SEC-05` `CREATE AGGREGATION` is unauthenticated arbitrary code execution
`crates/sankhya-server/src/wiring.rs:2359`, `crates/sankhya-server/src/aggregations.rs:207`

`ADR-0023` Decision 4 states in as many words that it *"requires a capability that is not granted
by default"* and that *"the grant is per principal, not per server."* **Nothing grants it and
nothing checks it.** The arm returns before the zero-role refusal, so a caller holding no roles
reaches it — and the code then writes an audit entry asserting `allowed=true` for a decision never
made.

### `SEC-04` Two more statement families reach state with no authorization
`crates/sankhya-server/src/snapshots.rs:287` (snapshots), `crates/sankhya-server/src/feeds.rs:227` (feeds)

`DROP SNAPSHOT` by any caller releases the retention pins other readers depend on — defeating the
claim that *"the ordinary maintenance scheduler is structurally incapable of destroying retained
history"* by a caller rather than a scheduler. The feed commands do not receive a principal at all.

### `SEC-03` Arrow Flight authorizes as a literal — VERIFIED BY LEAD
`crates/sankhya-server/src/flight.rs:118`

`let _user = Self::user_of(request_metadata)?;` — the user is read, checked non-empty, and
**discarded**. Every Flight request executes as subject `"flight"`. The port is always bound. The
user an operator deliberately left out of `server.users` connects there and gets `reader`.

The module's own comment says this *"today loses nothing… every user of a tenant gets the same
roles"* — a sentence that stopped being true the day per-subject roles landed. Flight also
bypasses the statement deadline, the quota and the read-only guard (`OPS-19`).

### `SEC-02` Column masks are never applied — VERIFIED BY LEAD
`crates/sankhya-catalog/src/secured.rs`

`mask_for()` and `masked_columns()` have **zero non-test callers**. `SecuredTable::scan` never
reads them. A policy written `.masking("email", Mask::Null)` returns `email` in the clear to
every principal. §13.1 lists column masking as one of the choke point's three rewrites; two exist,
and §13.8 claims the negative suite asserts forbidden **columns** are absent from results.

### `SEC-15` The shipped binary can only express "everything" or "nothing"
`crates/sankhya-server/src/wiring.rs:2637`, `:2598`

`start()` — the only path the binary takes — builds `permissive_policy`, granting `reader` read
on every discovered table with no row filter and no mask. **No configuration key loads a policy
set.** So the row-predicate enforcement, which is the best code in the repository, has never run
outside a test.

## The user-function sandbox

`sankhya-sandbox` uses real kernel mechanisms and is careful about the hard parts. The failures
are all at the seams, and together they mean **`ADR-0023` describes a stronger boundary than the
one that exists**.

### `SEC-09` The worker runs as the server's own user
`crates/sankhya-sandbox/src/jail.rs:169`

Namespace-uid 0 maps to `real_uid()`. `ADR-0023` Decision 2 has a row promising *"a distinct
unprivileged uid and gid"*. Outside the namespace the worker **is** the server user. The book's
§13.7a table silently drops that row rather than flagging it.

### `SEC-10` A user function can kill the server
`crates/sankhya-sandbox/src/jail.rs:29`

`unshare(CLONE_NEWPID)` places the caller's *children* in the new namespace and leaves the caller
behind — and the caller is the process that then `exec`s into the worker. So the worker is in the
host PID namespace, and because its credentials map to the server's uid, `os.kill(os.getppid(), 9)`
succeeds.

### `SEC-11` "No subprocess" is not delivered — VERIFIED BY LEAD
`crates/sankhya-udf/src/worker.rs:332`

The jail binds whatever `sys.base_prefix` names. **On this machine that is `/usr`** — so every
binary on the box is inside the jail. `ADR-0023` delivers this prohibition *entirely* through
"in a jail holding the interpreter and nothing else, there is nothing to exec". The comment three
lines above claims it avoids binding `/usr` wholesale.

There is **no test for this prohibition**, and the sandbox's own fixture deliberately binds
`/bin` and `/usr/bin` so it can run a shell.

### `SEC-12` A forked grandchild blocks the parent for ever
`crates/sankhya-sandbox/src/lib.rs:196`

The parent reads stdout only after `try_wait` returns, and a forked grandchild still holds the
write end — so `read_to_end` never sees EOF, with the deadline loop already exited. This runs
inside a DataFusion accumulator on a Tokio worker thread. A handful of such queries stop the
server.

### `SEC-13` The output cap cannot fire at its shipped value
`crates/sankhya-sandbox/src/lib.rs:193`, `crates/sankhya-sandbox/src/limits.rs`

Output is read only after the child exits, so a child writing more than the pipe capacity
(~64 KiB) blocks and is reported as `OutOfTime`. With the shipped cap at 64 MiB the `OutOfRoom`
arm is **dead code**. The test that "proves" the cap uses 64 bytes.

### `SEC-14` The probe does not run the mechanism it claims to
`crates/sankhya-sandbox/src/jail.rs:44`

`ADR-0023` Decision 3: *"The startup probe runs the mechanism, once, against a trivial worker — it
does not read a capability flag and hope."* It forks and calls `unshare`, and nothing else — no
uid map, no `pivot_root`, no `setrlimit`, no worker. A machine where `unshare` succeeds and
`pivot_root` fails passes the probe and fails at the first `CREATE AGGREGATION` in production,
which is the outcome the decision exists to prevent.

## Disclosure

### `SEC-07` The audit record is structurally empty, and volatile
`crates/sankhya-server/src/wiring.rs:871`, `crates/sankhya-audit/src/chain.rs:217`

The only append site hardcodes *no row filter and no masks*, never records the version, the
duration or the rows returned, and passes the first two words of the statement instead of a table.
§13.5 lists four fields as "not optional"; **none is ever populated**, and the record positively
asserts that no filter was applied on statements where one was. `Chain` is a `Vec` with no
persistence — the hash-chained tamper-evident audit is erased by a restart. `docs/STATUS.md:3119`
marks the criterion **met**.

### `SEC-08` The metrics endpoint enumerates every table, and a test asserts the leak
`crates/sankhya-server/src/scrape.rs:16`, `crates/sankhya-server/tests/observability.rs:375`

The endpoint is justified on the grounds that *"no label may carry tenant data — the metric
catalogue enforces it structurally"*. It does not: the label is bounded by **cardinality**, not
content, and is filled with the fully-qualified name of every servable table, with no principal in
scope. The test **requires** the label to be present.

### `SEC-16` DataFusion's "Valid fields are …" reaches the client verbatim
`crates/sankhya-server/src/execute.rs:663`

`SELECT nosuchcol FROM readable_table` dumps every column in the plan's scope. Nothing suppresses
it; the only mention of the phrase in the repository **sniffs for it to choose a SQLSTATE** and
passes it on. Since masks are not applied (`SEC-02`), those are the real column names.

### `SEC-17` `explain_contested` names tables the caller may not read
`crates/sankhya-server/src/execute.rs:210`

`claims` and `contested` are built from **all** servable tables; the guard filter runs afterwards.
There is also a string-free oracle: because `claims` counts unreadable tables, a hidden
`payroll.orders` makes the caller's own `sales.orders` stop resolving under its bare name.

### `SEC-18` Five listings are unfiltered
`cubes()` and `derived()` (`crates/sankhya-cube-sql/src/describe.rs:33`), `SHOW SNAPSHOTS`,
`SHOW FEEDS`, `SHOW AGGREGATIONS`. `derived()` emits the **SQL text** of every derived definition
and the tables it reads; `SHOW AGGREGATIONS` publishes every user function's Python source;
`SHOW FEEDS` emits filesystem paths. `register_derived` **does** gate on scope four lines away in
the same file.

---

# Tier 2 — Wrong answers that look right

The failure mode this project names as its enemy.

### `COR-08` `deterministic_sum` returns approximations, and its proof is wrong — EXECUTED, VERIFIED BY LEAD
`crates/sankhya-math/src/reduce.rs:146`

`exact_sum`'s proof states that a term more than 100 places under the largest is *"more than 47
places below anything the returned `f64` can represent"*. The premise is false: the returned
value is the **sum**, which cancellation makes arbitrarily smaller than the largest term.
Accuracy holds for about **48 bits of cancellation, not 100**. `deterministic_sum` tries this
route first, so it inherits the error.

Executed against the shipped binary:

```text
[1e13, -1e13, 0.01]   -> 9.999999999999995e-3     (correct: 0.01)
[1e18, -1e18, 0.01]   -> 9.999999999763531e-3     (relative error 2.4e-11)
[1e30, -1e30, 1e-5]   -> 0                        (correct: 1e-5)
[1.0, -1.0, 1e-25]    -> 9.999995265034156e-26
```

The module's own text rejects a candidate fast path because it *"still differed from the exact
total in 57% of cases, worst relative error 5e-11. Small, which is the problem: that is precisely
the figure that will not tie out and nobody can explain."* The shipped path produces 2.4e-11 on a
three-element input, and `exact_sum`'s docstring says **"Never an approximation."**

**This is a regression introduced on 2026-09-02.** The sorted-Neumaier route it replaced gets all
four cases right. It is the foundation of every aggregate in the system.

The property test compares the two routes over 20,000 vectors but spans only ~27 bits within a
vector, and random data never cancels 48 bits. The one mutation entry changes the constant
100→20, which the random test does catch — so **the catalogue protects the constant and not the
proof**.

### `COR-04` A materialised cuboid's key has no measure
`crates/sankhya-cube/src/materialise.rs:51`, `crates/sankhya-server/src/wiring.rs:1261`

The first measure writes each shape; every later measure sees `exists()` and skips. Reads build
the same measure-free key and label the result with the requested measure's name. On the shipped
fixture, a maintained `sales` cube returns `amount`'s numbers as `ratio` — **and answers a
`Rule::None` measure out of a stored aggregate**, which the ancestor-answerability machinery
exists to prevent. Materialisation converts an honest refusal into a plausible number.

The in-memory catalog had this exact defect and was fixed by keying on `(cube, measure)`, with a
comment explaining why. The on-disk key never got the same treatment.

### `COR-05` Materialisation stores the sum whatever the rule was
`crates/sankhya-cube/src/store.rs:175`

`to_batch` computes `contributions.exact_sum()` and uses the `rule` parameter only in a fallback
branch. `from_batch` reads back with `add_reduced`, and `Contributions::reduce` early-returns the
stored value for **every** rule. So `MEASURE amount (MAX ALONG region) … MAINTAINED` over facts
`30.0, 40.0` answers **70.0** where the live path answers 40.0; `MEAN` answers 70.0 against 35.0.

This is the 2026-09-01 defect that `crates/sankhya-server/tests/cube_rules.rs` was written to pin,
**resurrected one layer down**. Every `store` test passes `Rule::Sum`; every `cube_rules` test
declares its cube without `MAINTAINED`.

### `CLI-01` Every timestamp on the wire is a raw integer — VERIFIED BY LEAD
`crates/sankhya-server/src/execute.rs:546`

Microsecond timestamps — this project's own canonical unit — are rendered with `.to_string()`
under OID 1114/1184. psql prints `1756545242000000`; JDBC and psycopg raise. The other two units
render with `T`/`Z` where PostgreSQL's text format uses a space and an offset. `bytea` renders as
bare hex with no `\x` prefix. **Zero tests touch a timestamp.**

The catalogue and the result set also disagree about the same column's type: `information_schema`
maps every timestamp to `TIMESTAMP`, the result set maps tz-aware to `TIMESTAMPTZ`.

### `CLI-05` A null inside an array reads as `0.0`
`crates/sankhya-functions/src/rows.rs:119`, `crates/sankhya-functions/src/multi.rs:254`

Both copy the raw child buffer, ignoring the element null mask, where Arrow wrote `0.0`.
`sankhya-olap` refuses the same input with *"a vector contains a null element. There is no
reading of that: it is not zero"* — **two conventions in one catalogue, and the silent one is the
default** on exactly the composition the documentation advertises:
`ts_max_drawdown(ts_rolling_mean(prices, 3))`, whose leading nulls are deliberate.

The parity soak cannot see it because all three of its paths call the same kernel.

### `COR-07` `irr` returns a garbage rate instead of refusing — EXECUTED
`crates/sankhya-math/src/finance.rs:117`

From period 55 the bracketing divisor underflows to zero, so late flows become `±inf` and mixed
signs give `NaN`. Both the "no rate brings this to zero" refusal and the bisection's bracket test
are false for `NaN`, so the search marches through an interval that need not contain a root.

100 flows `[-1000, 110 × 98, -500]` — a project with a terminal decommissioning cost — returns
**`Ok(10.0)`**, i.e. 1000%. The true rate is 11.0%; NPV at the returned rate is −989. Any monthly
series over five years exceeds the threshold. The test uses four elements.

### `COR-09` `drawdown` flips sign on a non-positive peak — EXECUTED
`crates/sankhya-math/src/timeseries.rs:248`

`drawdown([-100, -200])` returns `[0.0, 1.0]` — a **positive** drawdown against a docstring
saying *"reported as a negative proportion"* — and `max_drawdown` returns **0.0** for a series
that doubled its loss. `max_drawdown([0, -50, -100])` is also 0.0: a default substituted where a
refusal was intended. A cumulative P&L curve crossing zero is the ordinary input.

### `COR-10` `f_test` reports `p = 0` by the subtraction its own module forbids — EXECUTED
`crates/sankhya-math/src/inference.rs:252`

The module header states *"Every tail here is taken from `chisq_sf`, `f_sf` or `t_two_sided`
rather than as `1 - cdf`."* The lower tail is taken as `1.0 - upper`. Every lower-tail p below
~1e-16 is reported as zero — in the direction that makes a finding look **stronger** than it is.
The mutation catalogue names this exact line and tests a different property.

### `COR-11`–`COR-13`, `COR-24`–`COR-29` Further arithmetic and cube findings
`jarque_bera` uses bias-corrected estimators where the statistic is defined on moment estimators
(3× wrong on 8 observations, +63% on 20); `least_squares` reports negative R² and negative F for a
model without an intercept; the two least-squares fitters disagree on constant data; an overlay
contributes nothing when served from a coarser cuboid while still being labelled with the
scenario; the definition fingerprint is blind to which supplied aggregation a measure names, so
redefining it serves the old cuboid; a supplied measure read from a cuboid is fed per-cell sums
rather than facts; `First`/`Last` at the base grain read fact-scan arrival order; the overlay
hard-codes `Rule::Sum`; `consolidate_along` demands a member order and ignores it.

### `CLI-04` A timed-out SDK query poisons the connection
`sdk/python/sankhya/wire.py:365`

`_read_exactly` raises mid-frame and keeps the partial buffer. Nothing marks the connection dead
and nothing resynchronises, so **the next query returns the previous one's rows** — reproduced
against a fake server. The default timeout is 30 seconds; a 30-second analytical query is
ordinary. `stream()` desynchronises a second way, on `break`, leaving the connection permanently
one query behind.

### `CLI-06` `SET SNAPSHOT` is silently lost on the extended protocol
`crates/sankhya-api-pg/src/session.rs:588`

`remember_setting` is called only from the simple-`Query` arm. A client using Parse/Bind/Execute
gets a success tag, the snapshot is validated, and **every subsequent query reads the present**.
pgjdbc, psycopg3, asyncpg and SQLAlchemy all use the extended protocol by default. This is exactly
the failure `ADR-0019` Decision 6 is quoted against, one layer above where the check lives.

### `CLI-07` `SET SNAPSHOT='eod'` with no spaces is accepted as a no-op
Both the protocol layer and the server take the second whitespace-delimited word as the setting
name, so the statement falls through to the generic `SET` arm and is acknowledged. No validation,
no effect, no symptom. `SET VERSION OF sales.orders=2` breaks the same way.

### `CLI-09` `cube_rollup(…, 'by=<unknown>')` silently changes the grain
`crates/sankhya-cube-sql/src/functions.rs:252`

The `by` **value** is never checked against the cube's dimensions, and the match is
case-sensitive while every keyword in the file is not. `by=regoin` rolls the axis away and returns
a subtotal labelled as a breakdown. `where` is checked; `by` is not. The test named
`a_misspelled_option_is_refused_rather_than_taking_its_default` carries the `by=regoin` case in a
comment and then tests a misspelled **key**.

### `COR-19`–`COR-22` Snapshot and cache semantics
The hydration cache key omits the grain, so `by=region` then `by=region|period` in one session
returns region-level totals with no `period` column and no error; a pinned session's answer is
cached under the present version and served to unpinned sessions; the snapshot read path lacks
the out-of-range guard `SET VERSION OF` has; and `CREATE SNAPSHOT` reads each table's version in
a loop, so it does not capture one instant — which is the crate's central claim.

---

# Tier 3 — Cannot be operated

### `OPS-01` The shipped configuration prevents the server from starting — VERIFIED BY LEAD
`config/application.yaml:52`, `crates/sankhya-config/src/lib.rs:423`

`read_as_of: 18446744073709551615` is `u64::MAX`; the config crate parses integers as `i64`.
Running the binary from the repository root, exactly as `docs/QUICKSTART.md` instructs, produces:

```text
sankhya: `warehouse.read_as_of` must be an integer and holds `18446744073709551615` …
```

The refusal message is excellent and the file it refuses is the one we ship. **§4, §7 and §9 of
QUICKSTART are unrunnable as printed**, and so are `doctor`, `backup`, `drill` and `attest`.

No test catches it because cargo sets the working directory to the *package* root, where that path
does not exist and is silently skipped. **No test in the workspace loads the shipped
configuration.**

### `OPS-03` The systemd unit cannot read the configuration at all
`packaging/systemd/sankhya.service`

No `WorkingDirectory=` and no `SANKHYA_CONFIG=`, so systemd's default CWD of `/` makes the config
path absent and silently skipped. Only five `SANKHYA_*` names reach the process. **`server.tls.*`
has no environment mapping, so TLS cannot be enabled in the systemd deployment shape at all** —
nor can Flight, cube budgets, maintenance cadences or `server.users`. The unit binds
`0.0.0.0:5433`; combined with `SEC-01`, the shipped unit is an all-interfaces, no-auth,
read-everything server.

The two documented start procedures fail in opposite ways.

### `OPS-04` The audit chain is an unbounded in-memory `Vec`
Appended on **every** statement and every catalogue listing — every `\dt`, every JDBC metadata
call, every tab-completion. No cap, no rotation, no persistence. Roughly 3–5 GB/day at 100 qps,
26 GB/day at 1,000. Each append SHA-256s a JSON serialisation of the whole record while holding
one global mutex on the query hot path. The timestamp is `*clock += 1` — a sequence number that
restarts at 1 on every boot, under a comment saying *"a real deployment supplies wall-clock time
here."* No deployment does.

### `OPS-05`, `OPS-06`, `OPS-07` Nothing bounds memory
`SELECT *` is **fully materialised before** the row limit is checked, then rendered a second time
into a `Vec::with_capacity(total)`. DataFusion runs on the unbounded default pool — there is no
`MemoryPool`, `FairSpillPool` or `DiskManager` anywhere in the workspace. `sankhya-governor`
states the risk precisely — *"hash joins do not spill… it exhausts memory and the operating system
terminates the process"* — and is dead code: admission is called with a zeroed request against
`u64::MAX` ceilings, and `observe()` is never called outside its own tests.

The operator cannot determine which query did it: there is no query log, and the audit is in RAM
and dies with the process.

### `OPS-08` An `accept()` error kills the server
`crates/sankhya-api-pg/src/listener.rs:159`

`EMFILE`, `ECONNABORTED` (routine behind a load balancer) or `ENOBUFS` propagates out of the serve
loop and `main` exits. The comment three lines below says *"a failed connection is that
connection's problem, not the server's. Logging and continuing is the only correct response"* — it
describes the *serve* error, and the accept path does the opposite. The metrics listener in the
same codebase gets it right. No connection cap and no `LimitNOFILE=` make it reachable.

### `OPS-12` "I could not look" is recorded as "there is nothing"
Fourteen sites of `let Ok(entries) = read_dir(x) else { return empty }`. Three matter:

- An unreadable warehouse — an unmounted NFS, a wrong path — returns **no tables and no
  complaints**. The server starts and serves an empty catalogue.
- **`sankhya-server doctor` on a missing or unmounted warehouse prints "0 table(s)", "Nothing to
  report", and exits 0 = CLEAN.** The tool built for the moment the server will not start gives a
  clean bill of health, and the documented hourly cron stays green through a dropped mount.
- An unreadable `_snapshots/` is identical to "no snapshots were ever taken", and the sweeper is
  told nothing is pinned every 30 seconds.

### `OPS-10`, `OPS-11` Maintenance is blind and partial
Tables created after startup are **never maintained** — the table list is a startup snapshot,
and `tables_under`'s own doc warns about exactly this. Maintenance failures are discarded with
`Err(_) => continue`: **7,747 lines of `sankhya-maintenance` contain zero `tracing` calls**. A
table whose compaction fails every 30 seconds is invisible, and the aggregate reclaimed-bytes
figure keeps rising from other tables so it looks healthy.

### `OPS-21`, `OPS-22`, `OPS-23` Degradation
Checkpoints are **never written** — `checkpoint_if_due` is called only from tests — so every log
replay is from version 0, one `exists()` plus one read plus a JSON parse per commit. Every
statement re-runs `discover()` over every table and replays each log up to three times, because
`live_files` is called free-standing rather than through the `LogCache` that was built for it. And
each column of each file carries a 4 KiB **provably-zero** HyperLogLog sketch: 80 GB of heap at a
million files, merged with 8×10⁹ byte comparisons per plan, twice.

Net: **`SELECT 1` costs the same as a full scan**, and scales as tables × commits.

### `OPS-24`, `OPS-25`, `OPS-26` Observability and runbooks
The whole system emits **six** `tracing` events; everything else is `println!`/`eprintln!` with no
timestamp, level or target — so an operator cannot determine when a feed halted. **There is no
query log at all.** Twelve of twenty-one documented error codes are never emitted, including four
of the six pageable ones, so alerting on them yields permanently silent alerts. The only
remediation in the only alert that can page invokes `sankhya maintenance compact`, and there is no
`sankhya` binary — the CLI is a 23-line stub. `check::storage_headroom` exists, is tested, and is
never called, so **`doctor` will never warn about a filling disk**.

---

# Tier 4 — The gate did not see any of this

The findings above are recoverable. This one explains why they exist.

### `CLM-21` There is no CI
No workflow, no Makefile, no pipeline. `docs/FUNCTIONS.md` claims the parity soak *"runs on each
build"*; there is no build to run it on.

### `CLM-16` Fifteen PostgreSQL end-to-end tests skip to green, invisibly
They print lowercase `skipping:` to **stderr**; the gate greps stdout for uppercase `SKIPPED`.
**M1's headline property — read-your-own-writes — is entirely inside this bucket**, and M1 is
marked complete and asserted by nine documents.

### Tests that cannot fail

| ID | Test | Why it cannot fail |
|---|---|---|
| `CLM-02` | `crates/sankhya-ingest/tests/crash_safety.rs:230` | Runs the crash-and-restart pipeline, **discards the result**, and digests a hardcoded `for id in 1..=20` loop. `digest(x) == digest(y)` for every `x`, `y`; deleting the pipeline body leaves it green. The only content-equality crash test in the crate. |
| `CLM-03` | Four authorization tests | Assert that *denied* is indistinguishable from *not found*, in fixtures where **both objects are absent** — comparing an error with itself. Each passes with the authorization check deleted. |
| `CLM-04` | `crates/sankhya-server/tests/cube_from_query.rs:79` | `assert!(… \|\| true)` — the only one in the workspace, written 2026-09-02. |
| `CLM-06` | The parity soak | `wire` is `connection.execute(sql)`; `sdk_sql` is `db.sql(sql)`, which **is** `connection.execute(sql)`. Half the reported comparisons are a tautology, all three paths share one kernel, and the harness assertion matches the coverage line at **0 of 155**. It is the only thing exercising 95 of the 155 catalogue entries. |
| `CLM-05` | The arity test | Skips 54% of the catalogue silently, and never checks `elsewhere()` at all — **which is where the arities are wrong**: `functions` and `cubes` declare arity 1 and take none; `graph_cycles` declares 1 and takes three. A sibling test *forces* a zero-argument function to declare a false arity. |
| `CLM-07` | Nine numerical SQL-surface tests | Assert with `String::contains` over a pretty-printed table. The matmul test passes for the **transpose**; the transpose test passes for a no-op; `contains('5')` passes for `15`, `0.5`, `−5` and `10.0`; one test passes with its two answers **swapped**; `vec_norm_l2` would pass if wired to `norm_l1`. `mat_inverse` and `mat_solve` have **no numeric assertion anywhere in the repository**. |

### `CLM-08` Eighteen crates have no mutation entry at all
About 19,000 lines. Including the whole of **M4's graph algorithms, its SQL surface and the
published extension API** — M4 is marked *"Complete. Every exit criterion met."* — and
`sankhya-cdc-model`, the `pgoutput` decoder the README singles out as validated against a real
PostgreSQL stream.

### `CLM-11`–`CLM-15`, `CLM-17` The checks themselves
`check-unsafety`'s opt-out detection is a substring test that every manifest satisfies, so a crate
declaring `unsafe_code = "allow"` is classified as inheriting and never checked. `check-kernels`
counts a test file as a surface, contradicting the principle stated in the same file.
`check-lock-order` cannot see 26% of guard bindings, and reports "0 declared nested acquisitions"
partly because it is not looking. `check-doc-numbers` recognises exactly four markers — every
other number in every document is unchecked, which is where all the stale ones live.
`check-concurrency` treats a skipped measurement as a pass. `NOT_YET_EMITTED` has no staleness
ratchet and one entry is already stale.

### `CLM-01` Ten documents and the README assert milestones complete that STATUS contradicts
All carry `M0–M8, M10 and M13 complete`. STATUS's own table says M2 is *"substantially complete"*
with the streaming partition path **not started**, M5 *"closed"* on four of five criteria, and M8
*"complete on six of eight"*. Eighteen milestones in the table are never named in that line.

`check-docs` misses it because it selects only rows containing the literal `"in progress"` — so
*"substantially complete"*, *"closed"* and *"complete on six of eight"* pass through. Its own
comment claims *"agreement is not accuracy"*.

### `CLM-09`, `CLM-10`, `CLM-18`, `CLM-19`, `CLM-20` Documentation against code
`FUNCTIONS.md` contains four contradictions, including a claim that every shipped function has *"a
runnable example gated as a test"* — 28 of 155 appear in any example — and the figure `192` for
engine functions, **which appears nowhere else in the repository**. STATUS's header reads
`Updated: 2026-08-29` and has eight rows dated 2026-09-03. Six code comments assert properties the
code no longer has, including *"the only place the state changes"* (eight assignment sites in four
functions) and *"the one place a `main` may exist"* (four). Crate counts appear as 55, 52 and 51;
there are 58. `INVARIANTS.md` names one writer where the check permits two, and states a
partitioning invariant with *"no exemption"* that STATUS says is unmet on the path where most data
lands.

---

# The patterns worth naming

**1. A guard exists, is documented at length, and is bypassed because a key is wrong.**
`COR-01` (a bare name against a qualified one), `COR-02` (a tick counter against a durable
sequence), `COR-04` (a key missing its measure), `COR-19` (a key missing the grain), `COR-25` (a
fingerprint missing the function name). Five findings, one shape.

**2. A rule stated in one function and violated by its sibling twenty lines away.**
`COR-03` (orphan sweep versus `retire_due`), `COR-05` (`to_batch` versus `reduce`), `SEC-18`
(`derived()` versus `register_derived`), `OPS-08` (accept loop versus the metrics accept loop).

**3. The test tests the pure function, hand-built with a shape the production caller never
produces.** `COR-01`'s test uses bare names; `COR-05`'s tests pass `Rule::Sum`; `COR-03`'s test
proves the pure function honours the pin set and nothing tests who supplies it. In `COR-01`'s case
the mutation catalogue reports coverage of a line whose defect **no test can reach**.

**4. Machinery built, reasoned about, never called.** `sankhya-governor`, `backup::protect`,
`check::storage_headroom`, `check::replication_lag`, `checkpoint_if_due`, `Configuration::explain`,
`Quotas::observe`, the column masks, `LogCache::live_files` on the hot path. The reasoning in each
is accurate about this system's failure modes. The wiring is the gap — and the doc comments
describe the wiring as done.

**5. Documentation that was true when written.** Not carelessness: every instance is a sentence
that was accurate and was overtaken. The system has no mechanism that notices.

---

# Tier 5 — What the product actually is

Audit 6 inventoried every feature the system planned, promised, imagined or built, against the
stated intent. Its verdict changes how everything above should be read.

> **SANKHYA today is a single-tier, read-only analytical warehouse**: DataFusion over Parquet,
> a PostgreSQL-wire door and a Flight door, with a genuinely strong cube engine, a wide scalar
> function catalogue, and real cloning, snapshots and maintenance. Around that sit roughly
> **15,000 lines of built, tested capability that nothing can reach**.

**31** built and reachable · **14** partial · **17** built and unreachable · **26** promised and
absent · **8** imagined and never specified · **7** silently dropped.

### `FEA-01` There is no write path — VERIFIED BY LEAD
`crates/sankhya-server/src/execute.rs:412`

Every DML and DDL statement is refused, and the refusal directs the user to write to a
transactional store **that is not running**. `sankhya-oltp-pg` is a **dev-dependency** of the
server, with a comment saying so; `Settings` has no OLTP field. "Two tiers, one experience" has
one tier.

### `FEA-02` The graph engine can never answer — VERIFIED BY LEAD
`crates/sankhya-server/src/execute.rs:181`

All five graph functions are registered against `GraphCatalog::new()` — and **nothing anywhere
populates it**; that constructor appears exactly once in the whole server. `sankhya-graph` is a
dev-dependency. The test harness excludes the GUIDE's graph examples, so **they have never run**.
The README calls the graph engine "Working".

The comment above the registration argues, correctly, that registering an empty catalogue is not
pretending. It is right about the mechanism and wrong about this instance: a cube's catalogue can
be populated, and this one cannot.

### `FEA-03` Packs cannot be loaded
`crates/sankhya-server/Cargo.toml`

The server's manifest references neither `sankhya-ext` nor `sankhya-pack`. The extension
mechanism — one of the two stated differentiators alongside cubing — is not a deployable
capability. `M4` is marked *"Complete. Every exit criterion met."*

### `FEA-04` Cube hierarchies are declared, validated, and ignored
`PARENT` and `ROLLUP` are parsed, checked and never reach the query path. That is `FR-CUBE-06`
and `FR-CUBE-08`, both mandatory, and ragged hierarchies are the marketing's headline example of
what native cubing gives you.

### `FEA-05` QR, SVD and eigendecomposition shipped while six documents say they are deliberately absent
Including the maths crate's own header, which argues that *"a subtly wrong SVD produces plausible
singular values"* as the reason not to have one. `mat_singular_values` forms the Gram matrix,
squaring the condition number, with a documented resolution floor of ~1e-8 that never reaches the
user. Compare `COR-08`: the same crate, the same failure — a documented reason to be careful,
overtaken by an implementation nobody re-read the reasoning against.

### `FEA-06` No benchmarks exist at all — VERIFIED BY LEAD
`criterion` is declared in the pin set. There is no `benches/` directory anywhere in the
workspace. The owner directive is that performance is *"measured rather than claimed"*, and
`ADR-0020` Decision 3 says every claim about speed ships with a number.

### `FEA-07` The catalogue drift test is tautological
It compares `functions()` against the list `functions()` is built from. It has already missed one
addition. (Same defect family as `CLM-06`; recorded separately because it guards a different
claim.)

### `FEA-08` Black-Scholes and the Greeks are registered in a core crate
Contradicting `NG-07` and `FR-EXT-01`, which say domain functions belong in a pack.
`check-vocabulary`'s twenty-two-noun list contains no quantitative-finance term, so the gate
passes. The rule exists, the check exists, and the check's vocabulary was never extended to the
domain that arrived.

### The pattern the feature audit adds

**Gaps are disclosed honestly in STATUS and the book, and contradicted in the README, GUIDE and
QUICKSTART — the three documents a buyer reads.** Twelve mutually contradicting documentation
pairs were tabulated. This is the same mechanism as `CLM-01`, seen from the product side rather
than the milestone side.

---

# Tier 6 — Format evolution and the first upgrade

Audit 10 found **19 persisted formats**. `sankhya-version` declares four, and its compatibility
gate is called at **two** sites. Its top finding is not an upgrade problem at all.

> **The system is in the worst configuration for a first upgrade: versions are written almost
> everywhere and read almost nowhere.** A reader that sees a stamp it ignores is strictly worse
> than one with no stamp, because the stamp is what convinces the next person the question was
> handled.

### `FMT-01` A schema change is adopted in memory and never written to the log — VERIFIED BY LEAD
`crates/sankhya-ingest/src/pipeline.rs:322`, `:527`, `crates/sankhya-publish/src/publish.rs:584`

**This is live data loss today, one binary reading its own files.** A `Compatible` change updates
the in-memory schema and increments a success metric. `Publication::create` — the **only** writer
of `schema_string` — is gated on `next_version == 0`, so it never runs again. The append path
commits `Action::Add` and nothing else, and the write-path check passes a column the table does
not declare.

The new column's data is encoded, written into Parquet, and is **unreachable by every query,
permanently**, while `schema_changes_applied` reports success. The comment at
`warehouse.rs:266` — *"A schema evolution writes a new one"* — describes a path that does not
exist.

Knock-on: compaction refuses inputs that do not share a schema, so after one added column that
partition's compaction **fails on every tick for ever**, silently (`OPS-11`).

Two smaller holes beside it: adding a `NOT NULL` column classifies as `Compatible` because
nullability is dropped when the change is built — surfacing later as a scan-time error, which is
precisely what the `ColumnTightened` refusal exists to prevent; and `ColumnsReordered` is **never
constructed anywhere**, so a pure reorder returns *"compatible, and nothing changed"*.

### `FMT-02` `Action::Protocol` is written and never read
`crates/sankhya-table-delta/src/log.rs:711`, `:685`

No comparison against a ceiling exists anywhere. A table declaring `minReaderVersion: 3` with
deletion vectors is served whole — **returning deleted rows as live**. With column mapping, every
column reads NULL. The write side has no ratchet either, and the checkpoint writer **hardcodes**
the protocol versions, so checkpointing a foreign table silently *downgrades* its declared
protocol for every downstream reader.

`docs/VERSIONS.md` states that *"a table whose protocol this build does not fully support is
read-only, never written"*. Neither half is implemented.

### `FMT-03` Checkpoints erase partitioning and configuration
`crates/sankhya-table-delta/src/checkpoint.rs:209`, `:210`, `:231`

`partitionColumns`, `configuration` and every file's `partitionValues` are written empty.
`publish.rs:695` states the consequence in advance: *"A table declaring a partition column whose
files carry no value for it is malformed: an external engine reads the column as null for every
row, and prunes nothing."* The checkpoint produces exactly that — and a checkpoint exists **for**
foreign readers. `configuration` carries clone lineage, feed positions, table class and key
columns; key columns absent means a mutable table reads as append-only, resolving distinct rows
into one.

SANKHYA itself survives only by accident: metaData comes from a full replay, and **there is no
log retention anywhere**, so commit files are never deleted. Add log cleanup and this becomes
data loss for SANKHYA too.

The kernel oracle cannot catch it: it builds metadata whose partition columns and configuration
are empty by construction, so the oracle is exercised only against unpartitioned, property-free
tables — including in the one test that deletes commit 0 to prove the checkpoint is used.

### `FMT-04` An unknown action variant kills the whole table
`crates/sankhya-table-delta/src/log.rs:503`

Four variants, no `#[serde(other)]`. `commitInfo` — which Spark and delta-rs write on essentially
every commit — makes the table unreadable, as do `txn`, `domainMetadata` and `cdc`. **A future
SANKHYA that adds a fifth action makes every table it touches unreadable by the previous binary:
a one-way door, undeclared anywhere.**

Note the inversion against `FMT-02`: **fatal on a benign unknown action, silent on a
semantics-changing unknown field.**

### `FMT-05` Aggregation documents are read by substring scan
`crates/sankhya-server/src/aggregations.rs:155`, `:159`

`composes` is decided by `text.contains("\"composes\":true")` — **a whole-document search that
includes the author's Python source**, so a body containing that literal flips the flag. Since
`composes` decides whether partial aggregates may be merged, a false positive produces wrong
numbers with no error. The field readers take the *first* occurrence of a key anywhere in the
document, so a future writer reordering keys returns the wrong name and the wrong source.

The `"format":1` stamp is written and never read.

### `FMT-06` Feed positions parse "never run" from any JSON object
`crates/sankhya-feed/src/progress.rs:46`

No version, and both fields `#[serde(default)]`. `{}` — or any restructured document — yields
"never run", and the feed re-reads every source from the beginning. The module documents this
hazard four lines away (*"starting from the beginning … is how a table acquires every row
twice"*) and then makes it undetectable.

### `FMT-07` `deny_unknown_fields` appears nowhere in the workspace
Per format: a dropped `pinned` silently unpins cuboids the writer required; a dropped snapshot
pinning axis under-pins, and the sweeper reclaims files a reader holds; a misspelled key in a feed
declaration is a feed with no retention. The backup manifest is the **one** format done properly —
a lenient stamp pre-parse, a compatibility gate before the full parse, a distinct `FromTheFuture`
error, and an upgrade corpus. It is the pattern the other eighteen need.

### `FMT-08` There is no migration mechanism, and rollback is documentation only
No `migrate`, `upgrade` or `downgrade` subcommand. Nothing at runtime reads `reversible()`, and no
startup marker records which version last opened a warehouse — so nothing would stop or even warn
on a downgrade. `Compatibility::ReadOnly` is never acted on: `FR-OPS-12`'s read-only degradation
is declared, not implemented, on either the manifest or the table protocol.

`docs/VERSIONS.md`'s rollback procedure ends *"run `sankhya-server doctor`. It reads every
artefact and reports what it could not."* `doctor` never opens a snapshot, a cube, an
aggregation, a backup manifest, a feed position or a protocol action.

### `FMT-09` The matrix shape metadata does survive — and an unmarked matrix is guessed
Checked end to end and it holds: Delta's `schemaString` carries arbitrary field metadata, the read
path uses the log schema, Parquet round-trips it, compaction and checkpoints preserve it. A
**width** change is properly refused.

But `mat_determinant` on a column with no shape metadata takes the integer square root, so a 2×8
is read as a 4×4 — a number from values that were never in the same row, reported as success. The
assertion that would have refused it was **reversed on 2026-09-02** on the premise that stored
metadata does not survive. That premise is now false; the reversal was not revisited. Compare
`COR-08` and `FEA-05`: the same shape, a third time.

An older reader that does not know `sankhya.fixedLength` gets a `List` rather than a
`FixedSizeList`, so every vector and matrix function fails to find a width — a refusal rather
than a wrong answer. That part is right.

---

# Tier 7 — The path data arrives by

### `ING-00` There is no change-capture runtime — VERIFIED BY LEAD
`crates/sankhya-ports/src/lib.rs:170`

`trait CaptureSource { start / next_batch / confirm / lag }` is declared, and a workspace grep
returns **exactly one hit — the declaration**. No implementation exists. There is no PostgreSQL
client in the workspace; nothing issues `START_REPLICATION`, nothing advances
`confirmed_flush_lsn`. The INV-2 source-safety ladder is called only from its own tests. The
server does not depend on `sankhya-ingest` at all.

> **The streaming path has neither exactly-once nor at-least-once delivery, because it has no
> delivery.** STATUS frames this as the last 10% ("what remains is the slot lifecycle driver");
> in fact every position and high-water-mark property below is unreachable and therefore untested
> against reality.

The path data *does* travel today is `sankhya-feed`, and that is where the live defects are.

### `ING-01` A feed resume duplicates one row per preceding quarantined or blank line — VERIFIED BY LEAD
`crates/sankhya-feed/src/source.rs:98`, `crates/sankhya-feed/src/progress.rs:122`

The saved position counts **records published**; the resume skips by **line index**. They are the
same number only if every line so far fitted and there were no blanks. A source of
`[good, bad, good]` publishes two, records position 2, and on restart skips lines 0–1 and resumes
at line 2 — **which was already published**.

The test that exists **asserts the buggy semantics**: `tests/progress.rs:111` is named
`a_partial_carries_the_count_of_what_is_published_not_what_is_read` and comments *"skip ten, read
the eleventh"*. The name identifies the exact conflation and the assertion locks it in.

`ADR-0018`'s amendment chose "never re-ingest" over "never duplicate", because *"duplication is
silent and permanent"*. This produces exactly the outcome the ADR ruled out.

### `ING-02` Quarantined records are committed separately from the position
`crates/sankhya-feed/src/run.rs:162`, `:224`

Rows and position commit atomically per batch — correct. Refusals accumulate across the **entire
source** and are written **once, after the loop**. A crash at 99,000 of 100,000 records loses
every refusal held in memory, with the position already advanced past them. `ADR-0018` Decision 1
is explicit that dropping is refused, *"because a pipeline that discards what it cannot parse is
one whose correctness claim is 'everything I kept was fine'"*. This is a drop. The accumulator is
also unbounded, so an all-bad source is an OOM before it is a quarantine.

### `ING-03` A file still being written is marked finished; its remainder is never read
`crates/sankhya-feed/src/source.rs:9`, `crates/sankhya-feed/src/progress.rs:179`

The module states the intent — a truncated last line is *"quarantined whole, and replayable once
the file is finished"*. What happens: the run quarantines the tail, marks the file finished, and
on the next tick the file sorts *at* the mark and is skipped. **Everything appended after the
first read is never ingested.** There is no file-stability control at all — no minimum age, no
required suffix, no atomic-rename convention — so a producer that appends rather than renames
loses data by default.

### `ING-04` A halted feed silently resumes on restart
`crates/sankhya-server/src/wiring.rs:707`, `crates/sankhya-server/src/main.rs:587`

`ADR-0018` Decision 4: a stopped pipeline *"waits for a person. It does not retry on a timer …
Auto-resume is how the same outage is rediscovered every five minutes and acted on by nobody."*
Feed standing is process memory, reconstructed as `Running` on every boot. **A restart is the
auto-resume the ADR forbids**, and the halt count that exists to preserve "halted twice for the
same reason" resets to zero.

### `ING-05` `TRUNCATE` is decoded and then silently discarded — VERIFIED BY LEAD
`crates/sankhya-cdc-apply/src/batch.rs:165`

The match ends in `_ => {}`, swallowing `Truncate`. The source table is emptied; the analytical
copy keeps every row. **Permanent, silent divergence**, no counter. A test asserts the decoder
*parses* truncate; nothing asserts anything downstream acts on it.

### `ING-06` A TOASTed unchanged value drops the whole row update
`crates/sankhya-ingest/src/pipeline.rs:261`

`TupleValue::Unchanged` exists as a distinct variant precisely to force this decision — and
`current_rows` is hard-coded `None` at **every** call site, so the mutation is discarded entirely.
Not just the column: the whole update. There is no row-lookup path into published Parquet
anywhere, so the pipeline has no way to supply it. This is the hazard the crate's own module doc
says it exists to prevent, defeated by its only consumer.

### `ING-07` A float becomes `inf` after the binder said it fitted — VERIFIED BY LEAD
`crates/sankhya-feed/src/shape.rs:136`

The binder refuses string→number, number→date, float→int and over-scaled decimals — it is
genuinely strict. Then `*value as f32` turns `1e308` into `f32::INFINITY`, silently. The comment
shows the narrowing was moved here **deliberately** and the consequence was not followed through.
The capture path has the same defect via `parse::<f32>()`, which returns `Ok(inf)` rather than an
error.

### `ING-08` Reconciliation compares nothing
`crates/sankhya-ingest/src/reconcile.rs`

The mechanism is sound — FNV-1a over a canonical null-distinguishing encoding, combined with
**wrapping addition rather than XOR** specifically so a doubled dataset is detectable. And
`compare` takes both digests as arguments: it reads nothing and computes nothing. Its only
callers are one test that needs a live PostgreSQL and never runs, and the soak, which compares the
warehouse **against itself**.

M11 is not merely "not schedulable" — **the scheduling is the whole of it**. There is no job, no
command and no server surface for reconciliation.

### `ING-09` The analytical copy is an unfolded change log
`crates/sankhya-table/src/encode.rs:75`

Every mutation is appended with an `_sankhya_op` of `I`/`U`/`D`. `WriteStrategy::Mergeable` is
computed at onboarding and **never read**. There is no merge, no upsert, no key-based fold and no
reader that applies the ops — so a table receiving updates is represented as every historical
version of every row plus tombstones, and `SELECT *` returns all of them. **Nothing in the
repository states this boundary.** `_sankhya_commit_ts` is hard-coded to the Unix epoch for every
captured row.

### `ING-10` An added column mid-stream destroys the buffered rows
`crates/sankhya-ingest/src/pipeline.rs:325`

The comment says *"Publish what was captured under the old shape before adopting the new one, so
no batch spans two schemas."* **The publish call is not there.** The schema is swapped while the
batcher holds old-arity rows; the next publish flushes them, fails on arity, and they are gone
(`ING-11`). Every evolution test publishes *before* the change, so the buffered case has never
been exercised — which is why the missing call was never caught. Compare `FMT-01`: the same crate,
the same day, two different consequences of the same missing write.

### `ING-11` Further position defects
`published_through` is **never recovered on restart** — the dedupe floor is always zero, so every
row between the last confirmed position and the crash is republished, while the counter that
exists to make replay visible stays at zero. `applied_through` is a **maximum** across tables
where a safe confirm needs the **minimum**, so the missing driver will be handed a number that
confirms away every quiet table's unpublished rows. A flushed batch that fails to encode is
destroyed. The decoder advertises streaming support it cannot parse, and an unknown or two-phase
tag halts capture outright — `ADR-0018`'s "must not stop for one record" has **no analogue on the
CDC path**.

---

# Tier 8 — Supply chain and licensing

The one axis that returns a **clean** answer to its central question, and one blocker beside it.

### `DEP-01` No third-party attribution file exists — BLOCKS DISTRIBUTION
No `NOTICE`, `THIRD-PARTY` or `ATTRIBUTION` file anywhere in the tree.

426 third-party crates ship under MIT, Apache-2.0, BSD, ISC and Unicode-3.0. **Every one of those
licences requires reproducing the copyright notice with binary distribution**, and Apache-2.0 §4(d)
additionally requires propagating upstream `NOTICE` files — which `arrow`, `parquet`, `datafusion`
and `object_store` all ship. Shipping today breaches ~426 permissive licences at once.

This is a *may we ship it as-is* problem, not a *may we use it* problem, and it is cheap to fix.
**It is the only thing in the dependency graph that blocks shipping.**

### `DEP-02` Licence compatibility is genuinely clean
488 lock entries, 426 third-party, **every one with a licence field, none unknown**. Zero GPL,
AGPL, LGPL-only, SSPL, BUSL or CC-BY-SA. The one LGPL appearance is a disjunction on a UEFI-only
crate that never links. `license-file` is used correctly for the proprietary crates — with a note
for later: a future licence gate needs `private.ignore = true`, or it fails on the project's own
crates and gets switched off.

### `DEP-03` Three advisories, none reachable — but the clean result is contingent
Two `quick-xml` denial-of-service advisories and a yanked `chacha20`. All three are **absent from
the shipped graph**, because `object_store` resolves without cloud features. `sankhya-tiering` is
designed around `s3://` archive URIs. **The day any cloud feature is enabled, all three land in
the binary** — along with two complete HTTP client stacks, since `reqwest` 0.12 *and* 0.13 are
both allowlisted as benign duplicates. Fix now, while it costs a `cargo update`.

### `DEP-04` No CI, no toolchain pin, no `--locked`, no advisory or licence scanning
`ADR-0001` states *"the gate runs on every pull request"* and is re-verified quarterly; there is no
CI and the graph has grown ~40% in nine days unreviewed. `rust-version` is a floor, not a pin
(the machine runs 1.97.1 against a declared 1.90). `--locked` appears nowhere. Twenty-five xtask
checks and **none is advisory or licence** — `IMPLEMENTATION_PLAN.md` §4.1 lists licence and
advisory scanning as delivered M0 work; it was not built. No SBOM, no release signing.

**The build is not reproducible.**

### `DEP-05` Two concentrations worth recording as accepted risk
`serde_yaml_ng` — exact-pinned, **single owner, last release 27 months ago**, parses operator
config, and pulls a C-derived YAML parser into the binary. The workspace comment calls it *"the
maintained fork"*; by release cadence that is no longer true. And `ring` — **one maintainer**,
18 months since release — is the sole crypto provider under both doors. Choosing it over
`aws-lc-rs` to avoid two crypto providers in one process is defensible, and I confirmed the claim
holds; the bus factor should be recorded rather than implicit.

Bundled C is invisible to every scanner here: the zstd inside `zstd-sys`, and the vendored
PostgreSQL. `NFR-QUAL-21` acknowledges the responsibility; no mechanism discharges it.

### What is exemplary
**The vendored PostgreSQL is the strongest supply-chain artefact in the repository** — upstream
URL, retrieval date, an upstream-published checksum verified at every build, the licence recorded,
and configured without Perl, Python or Tcl to shrink attack surface. The auditor verified the
checksum and the licence independently; both check out, and 17.11 is the current 17.x release. The
only gap is that nothing watches for the next one.

Also: zero git dependencies, zero alternate registries, **zero workspace-owned build scripts**, no
committed binaries beyond the documented tarball, and a hand-rolled PostgreSQL wire protocol with
no third-party client — which removes an entire class of supply-chain exposure from the most
exposed surface.

---

# Tier 9 — The open-format claim

Audit 11 **ran `delta_kernel` against tables built with this crate's own API**. Every finding
below is a reproduced kernel error, not an inference.

> **A partitioned SANKHYA table stops being readable by Spark, Trino and DuckDB the first time
> the shipped maintenance thread compacts it.** The security chapter accepts an enforcement hole
> in exchange for external readability. That trade is currently paying for a benefit that
> survives until the first maintenance tick.

### `CNF-01` Compaction writes empty `partitionValues` — live in the shipped server — VERIFIED BY LEAD
`crates/sankhya-maintenance/src/driver.rs:417`, `crates/sankhya-table-delta/src/log.rs:77`

The compacted output lands **inside** the partition directory, and its `add` action carries no
partition value — while `metaData.partitionColumns` names the column and the column is
**non-nullable**. `sankhya-publish` gets this right four files away, and its comment names the
exact failure: *"a table declaring a partition column whose files carry no value for it is
malformed."*

Reproduced against the kernel:

```text
sank_data_date=2024-03-01/part-0000.parquet  ->  {"sank_data_date": "2024-03-01"}
sank_data_date=2024-03-02/part-0001.parquet  ->  {}
ROW ERROR: Found unmasked nulls for non-nullable StructArray field "sank_data_date"
```

Kernel-based readers **hard error mid-scan**. Spark's more forgiving reader produces `NULL` for
the partition column instead — so `WHERE sank_data_date = '…'` **prunes the compacted file away
and silently returns short results.** That is the worst outcome available, and *it grows with how
well maintenance is working.*

Invisible internally because SANKHYA's own retention parses the **path**, not `partitionValues`,
and the data files also carry the column physically. Only a partition-value-driven reader notices.
**No test anywhere asserts `partition_values` on a compaction add.**

### `CNF-02` The oracle test cannot prove what the README says it proves
`crates/sankhya-table-delta/tests/oracle.rs`

`README.md:176` and `docs/QUICKSTART.md:115` claim it *"reads every log this crate writes back
with `delta_kernel`"*. Four structural reasons it cannot:

- The schema is a **hand-written constant** of two `long` columns — `schema_string()`, the entire
  Arrow-to-protocol type mapping, **is never called**.
- **Every data file is zero bytes**, so the kernel never reads a row and schema/data agreement is
  out of reach by construction.
- **No test uses a partitioned table.**
- **Neither production writer is under test** — actions are hand-built.

Everything that broke when an independent implementation was finally pointed at these tables had
never been looked at.

### `CNF-03` Three type mappings are loud failures
`UInt64` is declared `long` — the kernel refuses the file entirely (`Expected Int64, got UInt64`),
and `UInt8/16/32` are correctly *refused* at create, which makes accepting `UInt64` an
inconsistency rather than a widening. Its statistics are also actively wrong: the bound clamps
with `unwrap_or(i64::MAX)`, which **narrows a max**, so a predicate above it skips the file —
silently wrong rows. `FixedSizeBinary(n)` is declared `binary`; the kernel refuses it. And
`Granularity::Month`/`Year` write `"2024-03"` into a column typed `date`
(`Failed to parse value '2024-03' as 'date'`) — with a test **asserting the malformed layout as
correct**.

Timestamps round-trip correctly but the **zone is lost on read-back**, and naive timestamps are
mapped to the protocol's UTC-adjusted `timestamp` rather than `timestamp_ntz` — harmless only
because SANKHYA happens to store UTC, which STATUS records as a *finding*, not a guarantee.

### `CNF-04` Checkpoints are not partition-aware — armed, not yet wired
Corroborates and sharpens `FMT-03`. Reproduced: after a checkpoint, partition pruning is silently
lost for every external reader, and `configuration` — table class, key columns, **clone lineage**,
which STATUS says lives there *precisely so foreign readers keep it* — is destroyed. Rows still
read only because `partitionColumns` is *also* emptied: **two bugs cancelling**.

Add one `metaData`-carrying commit after a checkpoint and they stop cancelling — the table becomes
**entirely unreadable**. Feeds commit their position that way, so it is the normal shape for an
ingest table.

### `CNF-05` An external `VACUUM` would delete every superseded file immediately
`crates/sankhya-maintenance/src/service.rs:243`

`deletionTimestamp` is a tick counter — 1, 2, 3 — where the protocol defines epoch milliseconds.
`deletionTimestamp = 3` is three milliseconds after 1970, so **every superseded file is instantly
past any retention interval**. A conformant `VACUUM RETAIN 168 HOURS` deletes them at once,
defeating the retention check and pulling files from under SANKHYA readers holding leases — the
two mechanisms cannot see each other. `DRY RUN` would report them safe.

### `CNF-06` The openness is one-directional
`crates/sankhya-table-delta/src/log.rs:503`

Spark and delta-rs write `commitInfo` on **every** commit. One external `INSERT`, `OPTIMIZE` or
`VACUUM` renders the table permanently unreadable to SANKHYA — including the log files SANKHYA
later needs for schema recovery. Corroborates `FMT-04`.

### What is genuinely well built
**Statistics.** Bounds are produced *only* for the types whose Delta serialization is a plain JSON
number or string, and the types that would be wrong if guessed — `date`, `timestamp`, `decimal` —
get **no bound at all**. NaN, infinities and non-UTF-8 are refused rather than approximated; the
NaN-propagation trap in Arrow's min/max kernel is detected and worked around; negative zero is
handled correctly. The auditor could not fault the restraint.

**Both non-standard extensions are benign.** `sankhya.fixedLength` round-trips exactly through the
kernel, and the tensor metadata is carried through opaquely. The reasoning in the comment that
justifies the first could not be broken.

**The refusal-by-name of inexpressible types** works, and it is the right design. Also: STATUS's
claim that the ingest path writes flat and unpartitioned is now **stale** — that path routes
through `Publication` and is partitioned and readable.

---

# Tier 10 — Performance claims

Audit 8 rebuilt the benchmarks that the documentation's numbers came from, and ran them.

### `PERF-01` The 24.9× wrapper claim does not reproduce — measured **2.2×**
`docs/adr/0020-the-built-in-function-catalogue.md:283`, restated in `rows.rs:16` and STATUS

**No code anywhere in git history reproduces it** — the auditor searched the working tree, every
branch, `git log -S` on each figure, deletions and stashes. The figures appear only in prose.

Reconstructed faithfully, best-of-9 with warm-up and `black_box`, at load 2.6:

| Width | ADR claims | Measured |
|---|---|---|
| 8 | 24.9× | **2.2×** |
| 64 | 7.2× | 1.2× |
| 512 | 2.1× | 1.1× |

The shape of the claim is right — narrow rows win most — and the magnitude is off by roughly ten.
The tell is an internal inconsistency: against the fresh run, the ADR's *copying* arm is 2.4×
faster, but its *borrowing* arm is 27× faster. A smaller dataset would scale both together. Only
the fast arm is anomalous, it is **non-monotone**, and 0.85 ms for a scalar fixed-point reduction
implies ~39 GB/s — above memcpy bandwidth. **The borrowing arm was almost certainly optimised
away**: its result was unused and LLVM deleted the loop, while the copying arm survived because
heap allocation has side effects. That is precisely the *benchmark that optimises away* failure.

The same applies to every other speed table in ADR-0020 Decision 3 — the reduction speedups, the
per-kernel figures, "10 to 15 times faster", the 200,000-vector experiment. **None exists in the
repo.** The exactness claims in that ADR *are* well backed; every speed claim is not.

Decision 3 rule 5 reads: *"Every claim about speed carries its number… this repository does not
ship claims."*

### `PERF-02` There are no benchmarks at all
`criterion` is declared in the pin set and used by **no crate**. Zero `benches/` directories, zero
`[[bench]]` targets. `[profile.bench]` is configured for a profile no target uses, and
`check-performance` runs `--release` against a workspace with no `[profile.release]` — so the gate
measures cargo defaults, no LTO.

### `PERF-03` The 29× is a fixture constant, printed and never asserted
The example asserts only that the two routes **agree**; the ratio is printed. A regression
collapsing it to 1.2× passes silently. And the number is a property of the fixture's 64-wide
vectors: at 10,000 outcomes it would be ~4,500×, at 4 outcomes ~2×. The example's own text says
*"the ratio does not improve with size"* — **that is false in both directions**. It is also
inflated by the text wire protocol; on a binary protocol the same comparison is ~12–15×.

### `PERF-04` "Two to three orders of magnitude" for a user function was never measured
`crates/sankhya-udf/` contains **no timing code at all**. ADR-0022 Decision 5 is titled *"The cost
is stated"*, and the cost is asserted.

### `PERF-05` Fifteen of eighteen `NFR-PERF` objectives are unmeasured
And **seven are missing from the table that reports on them** — not listed as unmet, not listed at
all. Including **`NFR-PERF-06`**, which is the function catalogue's own requirement and literally
the value-at-risk workload the 29× claim is about.

### `PERF-06` The gate measures the engine, not the server
`the_performance_objectives_are_met` calls DataFusion directly and never crosses `execute.rs` or
the wire. So every per-statement cost in `OPS-22` — the double full log replay per table, the
checkpoint decode per table, a fresh `SessionContext` with ~150 UDF registrations, and a
`SessionState` **deep clone per table** — is outside the measured path. And `check-performance` is
deliberately outside `check-all`, so the only gate that can fail on slowness runs only when
someone chooses to run it.

### `PERF-07` The kernels are scalar, and the price is documented nowhere
`exact_sum` accumulates into an `i128`, which **cannot be autovectorised**, and makes three passes
where one would do. `dot`, `norm_l1`, `norm_l2` and `euclidean` each **allocate a full-length
`Vec` before reducing** — so `rows.rs` removed the wrapper's per-row allocation and the kernel
immediately makes one of the same size.

Measured against an ordinary sum: **32× slower at width 8**, 6× at 4096. The tradeoff is
defensible and well argued. But the documentation states only that the new reduction is 1.5–2.7×
faster **than the project's own previous code** — a reader comes away believing the kernels got
fast. Against an owner directive for "very high performance", the absolute price of the guarantee
is stated nowhere.

### What is exemplary
**The scaling benchmarks are the best in the repository** and could not be faulted: both arms in
one process on one machine in one run, a control arm that is the exact defect being tested for, a
warm-up arm, a mutex so two throughput tests cannot measure each other, and a capacity window that
reads `/proc/stat` before *and after* — counting `iowait` as busy, with a documented incident
explaining why.

And `pushdown_benefit.rs` is the policy working exactly as intended: a claimed optimisation was
measured, found to be a **cost**, the assertion that flattered it was found to be asserting its own
fixture, and **both the finding and the retraction were published**.

The measurement culture is unusually strong. The failures are concentrated in the newest work —
the function catalogue — where the documented numbers ran ahead of the harnesses that were
supposed to produce them.

---

# Tier 11 — The first run

Audit 7 followed `README.md`, `docs/QUICKSTART.md`, `sdk/python/QUICKSTART.md`, `docs/GUIDE.md` and
`docs/tutorials/` **literally**, executing every command on a clean machine. It is the only audit
that measures what a stranger experiences rather than what the code contains.

Its verdict: **a new user stops in the first ninety seconds, at the first command that runs a
binary.**

### `RUN-01` Every entry point dies, including `--version` — BLOCKS
`config/application.yaml:52` · `crates/sankhya-server/src/main.rs:112` · `:421`

`OPS-01` recorded that the shipped configuration prevents the server from starting. It is worse
than that. `read_as_of: 18446744073709551615` is `u64::MAX` parsed as `i64`, and `main.rs:421`
dispatches subcommands **after** config load — so `doctor`, `backup`, `drill`, `attest`,
`--version` and `--help` all fail identically:

```
$ ./target/release/sankhya-server doctor
sankhya: `warehouse.read_as_of` must be an integer and holds `18446744073709551615` …
exit=1
```

`doctor`'s own docstring says it must work when `start` would not. It doesn't. And the refusal
carries **no `SNK-` code and no remediation**, contradicting QUICKSTART's promise that every error
a client sees carries a permanent code and the catalogue's own remediation.

### `RUN-02` The one document that explains the fix is unreachable — BLOCKS
`docs/book/part4/18-getting-started.md:67` carries this exact pitfall, and
`docs/book/part3/17-packaging.md:147` is the **only** table of `SANKHYA_*` environment variables —
the sole place `SANKHYA_CONFIG` is documented. `grep -c "docs/book"` returns **0** for README,
QUICKSTART, GUIDE, `docs/tutorials/README.md` and STATUS. The README's two "Start here" tables list
seventeen documents and omit the book's twenty-seven chapters entirely.

The fix for `RUN-01` is written down and cannot be found.

### `RUN-03` The tutorials are unreproducible: **3 of 21** SQL blocks run — BLOCKS
`docs/tutorials/01-your-first-cube.md:43` opens with `cube_dimensions('sales')`. That cube exists
only in `crates/sankhya-server/tests/common/mod.rs:262`. The documented warehouse recipe does not
create it:

```
ERROR:  [SNK-C0001] Error during planning: no cube named 'sales' — this server serves []
```

Executed against a live server: **3/21 blocks succeed.** After issuing one `CREATE CUBE sales …` by
hand: **15/21**. The remaining six need a `margin_pct` column that exists in no documented
warehouse — so Tutorial 1's headline lesson and all of Tutorial 4 are unreachable.

`docs/tutorials/README.md` promises *"Every SQL example in every tutorial is executed by a test …
so an example that stops working breaks the build rather than misleading you."* True of the
fixture; false of the reader's warehouse. **The gate is green and the reader is stuck** — and the
promise makes the failure read as the reader's mistake.

### `RUN-04` Startup says "password required" and accepts any password from any user
`crates/sankhya-server/src/wiring.rs:913` — the independent confirmation of `SEC-01`, from the
outside:

```
user=alice pw='wrong'   -> connected
user=bob   pw='hunter2' -> connected
user=root  pw='wrong'   -> connected
user=alice pw=''        -> refused
```

The check is *non-empty*. Nothing is compared. QUICKSTART frames `SANKHYA_NO_PASSWORD` as an
opt-out whose danger is announced in capitals — so the **absence** of `NO AUTHENTICATION` reads as
"authentication is on." No user-facing document says otherwise.

### `RUN-05` QUICKSTART's own query fails against QUICKSTART's own warehouse
The recipe is documented as writing *"one table of 1,000 rows"*. It writes **eleven tables across
seven schemas** — `make_warehouse.rs` grew a second `#[ignore]`d test and `-- --ignored` runs both.
Three are named `orders`, so the documented query fails on ambiguity. Qualify it `sales.orders` and
the output matches the documentation **exactly, null row and all**. One word.

### `RUN-06` `--help` and `--version` start a server
`main.rs:436` is `_ => {}`, which falls through to serving. With a valid config,
`sankhya-server --help` binds ports and listens until killed. **Any typo'd subcommand silently
starts a production server.** There is no help text anywhere in the binary.

### `RUN-07` Trailing comments break SANKHYA's own documented statements — one returns a wrong answer
`without_leading_comments` (`wiring.rs:934`) handles *leading* comments. The GUIDE writes its
examples with trailing ones. `docs/GUIDE.md:498` verbatim refuses. Worse, silently:

```
$ psql -c "SHOW SNAPSHOTS;   -- what is held"
 snapshots; -- what is held      <- one bogus column, one empty row
```

It falls through to PostgreSQL's `SHOW <guc>` path. A reader pasting the GUIDE's own commented line
gets a **plausible empty result instead of their snapshot list** — precisely the confidently-wrong
failure this product exists to prevent.

### `RUN-08` The doctor transcript labelled "the correct output for a first run" is not
QUICKSTART §7 shows a `[note]`. A real first run gives `[critical] backup — no restore drill has
ever passed`, exit 1. The transcript presupposes a warehouse *and* a passing drill history.

### `RUN-09` The backup-corruption demo silently proves nothing
The documented path `warehouse/sales/orders/part-0000.parquet` does not exist — tables partition
under `sank_data_date=`. At the corrected path the drill **still** says "Proven", correctly, because
maintenance had already compacted those files away. **The reader corrupts a dead orphan, sees
"Proven", and concludes the drill is theatre.**

It is not. Corrupting the live file gives `NOT PROVEN. 1 of 12 table(s) did not verify`, exit 1,
exactly as documented. The product is right and the instruction is stale twice over.

### `RUN-10` Arrow Flight cannot be moved, and Kubernetes does not expose it
There is no `SANKHYA_FLIGHT_LISTEN` (`grep -c` = 0), so a second instance always collides on 5434.
`packaging/kubernetes/deployment.yaml` declares only 5433 and 9464 — **Flight SQL, a headline
README feature, is unreachable in the shipped manifest and unconfigurable from either.**

### `RUN-11` The Kubernetes manifest cannot be applied
It names `image: ghcr.io/ajsinha/sankhya:0.1.0`. There is **no Dockerfile, Containerfile or compose
file anywhere in the repository.** It references a `persistentVolumeClaim` no manifest defines and
ships no Service, so nothing routes to 5433. `packaging/` contains exactly two files.

### `RUN-12` The systemd unit produces the unconfigured, unauthenticated server
`packaging/systemd/sankhya.service` sets no `WorkingDirectory=` and no `SANKHYA_CONFIG`, and config
resolution is relative to cwd — which systemd sets to `/`. A missing file is skipped rather than
fatal, **so it starts**: no users, no roles, no policy, no TLS, no maintenance, and
`configuration_dir()` pointing at a non-existent `/config` so feeds never run. This is the server in
`RUN-04` that accepts every password.

### `RUN-13` The doc-number gate is structurally blind to the drift it exists to catch
Three different counts of one set of scripts, twelve of them on the day of the audit: *"eight"*, *"ten"* (three lines above its
own table of twelve), *"twelve"*.

The root cause is exact. `xtask/src/docnumbers.rs:150` recognises four markers and walks back over
**digits**. Every count that drifted is spelled as an **English word** — eight, ten, twelve, "eleven
invariants" (there are 21), "one table". The gate cannot see any of them.

### `RUN-14` The SDK example that says it adapts, doesn't
11 of 12 examples ran clean. `12_your_own_aggregation.py:96` hardcodes `margin_pct` and never calls
`a_table()`, while `sdk/python/examples/README.md:55` states *"Nothing here hard-codes a table."*
Same root cause as `RUN-03`.

### `RUN-15` Smaller stumbles
`pip install -e` dirties the tree (`sankhya.egg-info/` is not ignored) against a document that says
to run the audit on a clean tree · QUICKSTART says "any PostgreSQL client: `psql`" before `PGBIN`
exists and the prerequisites install no client · the README badge says Rust 1.97+, `Cargo.toml` says
1.90, and there is no `rust-toolchain.toml` · `check-all` is described as building "in seconds" when
it opens with the full test suite · `cargo build --workspace` emits **68 warnings** against a
document emphasising clippy cleanliness.

### The credits — and they are real
Several were exact to the character:

- **The wire protocol.** Real `psql` connected first try; `SELECT version()` returned the documented
  string character-for-character; `\dt` listed all eleven tables.
- **Capture end-to-end** printed `933 messages decoded, 922 mutations across 4 transactions,
  reconciled against source` — an **exact** match for the documented output.
- **`datagen plan --gb 10`** → 99,236,189 rows, matching "99.2 million rows across ten tables."
- **`backup`, `drill`, `doctor`, `attest`** all behaved exactly as documented once reachable, and
  the drill **caught real corruption** with the documented message and exit code.
- **The vendored PostgreSQL build** was idempotent exactly as promised — 0.12s on the second run.
- **The error messages, where they carry codes, are the best thing here.** The ambiguous-`orders`
  refusal names all three candidates in `DETAIL` and `HINT`. The cube refusals explain *why*.

### The pattern behind almost every debit
**The gates verify against fixtures the documentation never hands the reader.** `guide.rs`,
`sdk_examples.rs` and the doc-number check are all green while the documented path is broken,
because the test fixture and the documented recipe have silently diverged.

The documents are not careless — the auditor called them unusually candid. **They are validated
against the wrong warehouse.**

---

# Appendix — the state of this report

**All twelve audits are complete.** 129 distinct findings carry stable identifiers; 18 were
re-verified personally by the lead before publication, including four that indict the lead's own
work. No auditor was permitted to fix anything, so every finding below is open by construction.

What this report is *not*: it is not a measurement of the system under production load, on a second
machine, or over time. `check-performance` and the 741-entry mutation audit were not run during it.
The absence of a finding is therefore not evidence of correctness — twelve auditors reading for
three days found 129 things, and the honest reading of that number is that it is a lower bound.

The sequenced remediation plan derived from this report is `REMEDIATION.md`.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
