# 13. Security, tenancy and policy

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> This chapter specifies how SANKHYA decides what a caller may see, and its central claim is
> structural: **it is impossible to reach a table without a security context, because the type
> system offers no other way to construct one.** From that follow a single choke point shared by
> the SQL, graph and tiering engines, a row predicate enforced where no provider can decline it,
> a catalogue that makes an unreadable table indistinguishable from a missing one, a hash-chained
> audit that records what was *seen* rather than what was asked, and one arithmetic disclosure
> channel — the aggregate — that is closed by putting completeness on every row. It also states,
> by name, the half that is not built.

---

## 13.1 One choke point, and why there is exactly one

Three engines answer questions in this system: the SQL engine, the graph engine and the tiering
engine. Each of them resolves a table through the same catalog, and the catalog's resolution
function takes a `SecurityContext`. There is no other constructor for a table provider.

```
  request ──▶ authenticate ──▶ Principal + SecurityContext
                                      │
                                      ▼
                            ┌──────────────────────┐
                            │      CATALOG         │  ← the ONLY path to a table
                            │  policy rewrite:     │
                            │   • row filter       │
                            │   • column mask      │
                            │   • projection limit │
                            └──────────┬───────────┘
                                       │
                   ┌───────────────────┼───────────────────┐
                   ▼                   ▼                   ▼
              SQL engine          graph engine        tiering engine
```

The alternative — three engines each remembering to consult a policy — fails in a specific and
unrecoverable way. It does not fail by enforcing the wrong policy; it fails by a code path that
never consulted one, which produces no error, no log line and no wrong-looking number. Chapter 5,
*Architecture*, states this as principle **P8**: structural prevention over procedural care. This
is that principle applied to the highest-consequence path in the system.

The mechanism is a value called `Guard`, and its properties are chosen for one purpose:

Property | Consequence
---|---
No public constructor | It cannot be made without a policy decision
No public fields | It cannot be forged from parts
No `Default` | It cannot appear by omission

Anything taking a `Guard` in its signature cannot be called until a decision has been made. That
is why the enforcement is stated as a compile-time fact rather than as a review item.

> **Key idea** — The defect a security architecture must prevent is not *the wrong policy*. It is
> *no policy*, on one path, on one day. A wrong policy is visible in a test; a path that never
> asked is visible in nothing at all.

Enforcement happens **once, at plan construction**, not three times in three engines. The graph
tier resolves through the same catalog, so an edge a tenant may not see is never materialised into
that tenant's epoch — it is not filtered out of the traversal, it is absent from the adjacency the
traversal walks.

## 13.2 The predicate is enforced where nothing can decline it

The first implementation of row security handed the policy predicate to the underlying table
provider as a pushdown filter. That is the obvious design and it is the one that reads best. It
was also a complete failure of the control, and the way it failed is worth the paragraph.

A provider may *decline* a filter. `MemTable` does. When it declined, every row came back — with
no error raised anywhere, by anything. The table was secured in name only, and the only evidence
was the row count.

The predicate is now offered to the provider as an optimisation *and*, unless the provider
promises exactness, conjoined above the scan where nothing can decline it. The presence of the
predicate in the **final physical plan** is asserted by test, not the presence of the intent in
the logical one.

This has a pleasant consequence a caller can observe. A tautology in the query cannot widen the
policy, because the policy is not part of the query:

```sql
-- with a policy of  region = 'north'  on this table:
SELECT count(*) FROM orders WHERE region = 'south' OR 1 = 1;
-- returns only the northern rows
```

> **Pitfall** — Correctness that depends on a cooperating component is not correctness. It is a
> convention with a green test suite, and it will hold until somebody swaps the component for a
> faster one that declines the same call.

## 13.2a A mask that is applied, and the question it also has to refuse

Column masks were declared in the policy, merged into the guard, hashed into the visibility
scope so that two principals seeing different values could not share a cached result, reported
by `masked_columns()` and `mask_for()`, and documented in three places. Nothing read them.
`SecuredTable::scan` conjoined the row predicate and returned every column exactly as the
provider produced it, so every masked column returned its real value to every principal.

That is worse than having no masking at all, because the documentation is what an operator
decides on. Somebody reads *"column masks"* in a policy reference, writes `masking("email",
Mask::Partial { keep: 4 })`, sees the mask reported back by the API, and concludes that the
support desk cannot read customer addresses. `SEC-02`.

The scan now ends in a projection that replaces each masked column with the masked value, and
the three masks mean:

Mask | What comes back | What happens to a null
---|---|---
`Null` | nothing, at the column's own type | it was already null
`Constant { value }` | the constant, in every row | **it becomes the constant too**
`Partial { keep }` | all but the last `keep` characters replaced by `*` | **it stays null**

The two null rules differ deliberately. A constant mask that left nulls alone would publish which
rows have no value, and *"this customer has no email address"* is a fact about that customer. A
partial mask that turned a null into `***` would be inventing a value where there is none, and a
reader could not tell the two apart.

The masks are built from Arrow directly rather than from `concat`, `repeat` and `right`, and the
reason is the null rule above: **`concat` treats a null as the empty string**, so the obvious
implementation turns a missing address into `***`. A security control whose semantics are
inherited from a function library's opinion about nulls is a control that changes when the
library does.

### The question the mask also has to refuse

A mask hides a value from being *printed*. It does not, by itself, stop the value being *asked
about*:

```sql
-- with  masking("region", Partial { keep: 2 })  on this table:
SELECT count(*) FROM orders WHERE region = 'north';
```

Nothing here prints a region. If that predicate is pushed into the provider it is evaluated
against the real column, below the mask, and the count answers the question as loudly as
printing it would — a non-zero answer means yes, there are northern orders. Repeated against a
list of candidate values, it reads the column out one value at a time; against a masked email
address and a list of customers, it reads out who banks here.

So `SecuredTable` declares any filter over a masked column **unsupported**, whatever the
provider would have accepted, which keeps the predicate above the scan where the column it reads
is the masked one. A filter over an unmasked column still pushes down and still prunes; this
withholds one predicate, not pushdown.

> **Pitfall** — A disclosure control that only governs what is displayed governs nothing. Every
> aggregate, predicate and join is a way of asking about a value without showing it.

### A mask that cannot be applied is refused when the table is opened

`Partial` and `Constant` produce text, so a policy asking for either over an integer column, or
over a column the table does not have, is refused when the table is opened — the same rule, and
for the same reason, as a policy predicate that does not parse. Discovering it on the first query
that happens to select that column is a policy that is wrong for months and looks right. `Null`
is refused nowhere, because a null is meaningful at every type.

## 13.2b A name in a statement is not a path

Three statement families built a file path as `warehouse.join(DIRECTORY).join(format!("{name}\
.json"))` from a name a client typed, constrained only to be non-empty and free of whitespace.
`Path::join` **replaces the entire path when the component is absolute** and honours `..` when
it is not, so a name was a way to write and delete files anywhere the server's user could reach:

```
CREATE AGGREGATION /var/tmp/x LANGUAGE PYTHON AS $$…$$   -- writes there
DROP SNAPSHOT ../_cubes/regional                          -- deletes that
```

The worst target is a snapshot document, and not because a snapshot is precious. **An absent
snapshot pins nothing, so deleting one releases the files the sweeper was holding back** — the
deletion the whole retention mechanism exists to prevent, reached through the name of a `DROP`.
`SEC-06`.

A name that may become part of a path is now checked in one place, `sankhya-atomicfs::name`, and
the three path builders return a `Result` rather than a `PathBuf`. That is the load-bearing part:
a check that can be forgotten is a check that will be, and there were already four copies of this
path-building line and one copy of the restriction — in the crate that needed it least.

### An allow-list, because the deny-list has no end

The rule is: letters, digits, `_`, `-` and `.`, ASCII only, with `.` and `..` refused outright.

The alternative is a list of dangerous shapes somebody has to keep complete, and it is longer
than it looks: `..`, a leading `/`, a NUL byte, a Windows drive letter, a trailing dot or space
that Windows strips, a reserved device name, a name differing from another only in case on a
case-insensitive filesystem, a Unicode character that normalises to a separator. Saying what a
name **may** contain is one line and has no tail. Non-ASCII is refused rather than normalised for
the last of those: two names differing only in normalisation form are one file on macOS and two
on Linux, which shows up as a document silently overwritten rather than as an error.

### Where a quoted identifier carries it

`CREATE CUBE ../x` is a syntax error, because the cube tokenizer builds a bare word out of
alphanumerics and `_`. That makes the statement look safe and it is not: a **quoted** identifier
is copied verbatim — which is what makes a cube called `"Level"` expressible — so
`CREATE CUBE "../x"` reached the path builder with the separator intact. A tokenizer that
restricts one spelling of a name and not the other has restricted nothing.

### The fourth site, which the audit did not name

`CREATE TABLE … CLONE` places a clone beside its origin, and it does not escape upward. The
reason is an accident: `..` holds a dot, so a name containing one is read as `schema.table` and
refused for naming the wrong schema rather than for traversing. That is a proof about
`split_once('.')` and it stops holding the day the qualified form gets smarter. What is wrong
without a check even today is `sub/dir` — no dot, so the clone lands in a directory the
catalogue does not scan, and the result is a table that exists and cannot be found.

> **Pitfall** — Reasoning that a path *cannot* escape is not a control. It is a comment that
> was true when it was written, attached to code somebody else will change.

## 13.2c A statement runs as somebody, and three of them did not

Three surfaces reached state without ever asking who was asking.

### Flight ran every request as a literal

`let _user = Self::user_of(request_metadata)?;` — the name was read, checked non-empty, and
**discarded**. Every Flight request then executed as the subject `"flight"`, whose roles came out
of the same default branch as any unknown name's. A user an operator had deliberately left out of
`server.users` connected to that port and read. The port is always bound. `SEC-03`.

The module's own comment explained why this was safe: *"today loses nothing… every user of a
tenant gets the same roles"*. That was true when it was written. It stopped being true the day
roles became per-subject, and **nothing changed and nothing failed** — which is the failure mode
that comment shape has, and the reason this book prefers a check to an explanation.

The ticket now carries the subject as well as the tenant, and redemption checks both. The
consequence of checking only the tenant was not merely that a leaked ticket was usable: it was
usable *at the entitlements of the person it was issued to*, because the plan inside it was made
under their roles and Flight deliberately does not re-authorize at redemption.

A `skhyft1` ticket — the version before the subject existed — no longer decodes. Honouring one
would mean choosing a subject for it, and every available choice is the hole this closes. A
client holding one is told it is not a ticket this server issued and plans again.

### `DROP SNAPSHOT` took no principal

So any caller could drop any snapshot. A snapshot's whole job is to hold files back from the
sweeper, so dropping one releases them — and `docs/INVARIANTS.md` claims the maintenance
scheduler is *structurally incapable* of destroying retained history. It is. A statement was
doing it instead. `SEC-04`.

The rule now is: the subject who took it, or a caller who may read every table it pins. The
second half is what keeps an operator able to clean up after somebody who has left, and it is the
rule `DROP CUBE` already followed — a principal who cannot read what a thing is built on has no
business removing it. A snapshot document that cannot be *read* is refused rather than dropped:
not being able to tell what it pins is not permission to release it.

### `RESUME FEED` took no principal either

`SHOW FEEDS` reports what the server is doing and stays ungated: names an operator configured,
counts this process moved, and why something stopped. There is no table to check a scope against.

`RESUME FEED` is a different thing that was treated the same way. It restarts an ingest that
`ADR-0018` halted **because its source changed shape** — so resuming one is deciding that records
of an unknown shape should start landing in a table again. It is now authorized against the table
the feed writes into.

> **Pitfall** — All three refusals are the same sentence as *"there is no such thing"*. Saying
> "you may not touch that" confirms it exists, and the name of a snapshot or a feed is something
> somebody chose.

## 13.2d What a refusal says, and what a listing says

None of the three below returns a row the caller may not see. Each of them *tells* the caller
something about rows they may not see, which is the half a row-level control does not reach.

### A misspelt column was answered with the list of the real ones

`SELECT nosuchcol FROM orders` produced *"Schema error: No field named nosuchcol. Valid fields are
orders.id, orders.region, orders.email, …"*, and the whole of it reached the client. Every column
of every table in the plan's scope, to anybody who could name one table and guess one column
wrong. The only mention of that phrase in this repository sniffed for it to choose a SQLSTATE and
passed it on. `SEC-16`.

The half the caller typed is kept, because telling somebody they misspelt a name is the entire
usefulness of the message. The half they did not type is gone, and they are pointed at
`information_schema.columns`, which is subject to the same policy everything else is.

> **Pitfall** — The first attempt at this fixed the wrong function. The message that reaches a
> client is built from the engine's string a *second* time, in `plan_failure`, not from the
> detail the classifier carries. A leak fixed on the path nobody takes is not fixed, and the
> tests were what said so.

### A name was contested by tables the caller could not see

Bare-name claims were counted over **every** servable table, and the authorization ran afterwards.
That leaked twice, and the second one is the one worth fearing.

The visible half: a refusal telling a caller to qualify an ambiguous name listed the qualified
names it could mean — enumerating the schemas of a warehouse to somebody with no grant on them.

The half with no string in it at all: because the count included tables the caller cannot read, a
hidden `payroll.orders` made the caller's own `sales.orders` stop resolving under its bare name.
Anybody could ask whether a table of a given name existed somewhere they could not look, and read
the answer off whether their own query planned. `SEC-17`.

Authorizing first and counting afterwards closes both, and it also makes the word mean what it
says: a name is contested when **this caller** could mean two things by it. Two tables one of
which they may not read is not an ambiguity they can act on.

### Five listings were unfiltered

`cubes()`, `derived()`, `SHOW SNAPSHOTS`, `SHOW FEEDS`, `SHOW AGGREGATIONS`. `derived()` emits the
**SQL text** of every derived definition and the tables it reads; a snapshot row names the
qualified tables it pins; `SHOW AGGREGATIONS` publishes every user function's Python source.
`register_derived` was already gating on scope four lines away in the same file, which is what
makes this an omission rather than a decision. `SEC-18`.

Each is filtered by the rule that already governed the thing being listed:

Listing | Shown to
---|---
`cubes()`, `derived()` | whoever may read the fact table the cube is built on
`SHOW SNAPSHOTS` | the subject who took it, or whoever may read every table it pins
`SHOW FEEDS` | everybody, by name and state; the **halt reason** to whoever may read the table it fills
`SHOW AGGREGATIONS` | everybody, by name; the **source** only where `server.user_functions` is on

The aggregation rule needs its reason stated, because the source is there on purpose:
`ADR-0023` Decision 4 makes creating one a grant, and a grant nobody can review is a grant nobody
should give. The mistake was that *"somebody"* was every caller. The switch is the one that
decides whether these can be created at all, so the people who can review a grant are the people
who could make one — and the **names** stay visible to everybody, because an operator who has
just closed that door needs to see what came in while it was open.

`SHOW FEEDS` is the one that took two attempts, and the wrong attempt is instructive. Filtering
the **rows** by the same rule as the others removed a feed whose target table does not exist — and
a feed that halted *because its table is missing* is precisely what an operator opens the
statement to find. A control that hides the thing it is meant to report is not a control. So the
name and the state are shown to everybody, because somebody configured that feed and it is not
tenant data, and the **reason** is what is withheld, because `ADR-0018` halts a feed when a record
does not fit and saying so means saying which file and what was in it. Where the table does not
exist at all the reason is shown, since there are no rows to withhold anything about.

> **Correction** — §13.2c said `SHOW FEEDS` had no table to check a scope against. That stopped
> being true in the same change that wrote it: recording which table each feed fills, so `RESUME`
> could be authorized, gave `SHOW` exactly the table it was said to lack.

## 13.2e The policy the binary could not be configured with

Everything above this section describes a policy engine: a row predicate conjoined into the scan
where nothing can decline it, column masks applied above it, a denial that beats any grant. All of
it is built, and §13.8 is about how carefully it is tested.

**No configuration key loaded a policy set.** `start()` — the only path the shipped binary takes —
built a policy granting `reader` read on every discovered table, with no filter and no mask. So
the row-predicate enforcement, which is the best-tested code in this repository, had never run
outside a test, and the binary could express *everything* or *nothing* and nothing in between.
`SEC-15`.

That is a different kind of finding from the rest of this chapter. Nothing was wrong. The thing
was unreachable, and every page describing it described a capability nobody could configure — a
feature that ships unreachable has been paid for and not delivered.

```yaml
policy:
  rules:
    analysts_read_northern_orders:
      role: analyst
      table: sales.orders
      action: read
      where: "region = 'north'"
      mask:
        email: null
        phone: partial:4
```

Four decisions in that shape are worth stating.

- **Each rule is named, and the name is the operator's.** A list would be shorter to write and
  impossible to talk about: *"rule 3 does not parse"* is a refusal somebody has to count to, and a
  policy is a file people review line by line.
- **The table must be qualified.** A policy is the one place an ambiguous name is fatal: a bare
  `orders` means one table today and two the day somebody adds a schema, and the rule would then
  apply to *neither*, because a contested bare name resolves nowhere.
- **A rule that does not parse stops the server.** A policy with a rule quietly dropped permits
  more than it says, and the person who wrote the rule believes it is in force. That is the worst
  available outcome for a file whose entire purpose is to be reviewed.
- **A denial is spelled out**, not inferred from an absent grant. Forbidding is a decision
  somebody made and should read as one.

An absent policy is not an error — a warehouse somebody is trying out should still answer. What
must not happen is that it answers *the same way* as one that has been configured and says nothing
about it, so the startup line names which posture is in force:

```
  tenant …, password verified for 2 user(s), 3 policy rule(s), 4 table(s) known
  tenant …, NO AUTHENTICATION, NO POLICY CONFIGURED — every authenticated user may read every
  one of the 4 table(s) below, 4 table(s) known
```

> **Pitfall** — A control that cannot be configured is not a weaker control. It is documentation.
> Every claim in this chapter about row filters and masks was true of the code and false of any
> deployment, and nothing in the test suite could tell the difference, because the tests build
> their own policy.

## 13.3 A table you may not read does not exist

Only the tables a principal may read are registered into the session. Naming one that is not
registered therefore fails to resolve, and it fails with the same code, the same SQLSTATE and the
same words as naming a table that was never created.

Run against the review server in this chapter's session, a missing table answers:

```
ERROR:  [SNK-C0001] Error during planning: table 'datafusion.common.no_such_table' not found
DETAIL:  Correct the statement. The detail names the offending element.
```

An unreadable table answers identically, and that identity is the control. *"You may not read
that"* confirms the table exists, and existence is frequently the secret — a table called
`acquisitions_2027` discloses something before a single row is read. The difference between the
two messages is a working enumeration oracle, and there is no configuration that turns it on.

The same rule governs the catalogue. `information_schema.tables` is filtered server-side, so a
schema browser cannot enumerate what a query cannot read. A client that listed everything and
filtered locally would have reintroduced the oracle through the front door of a GUI.

## 13.4 The disclosure that has no error and no log line

Row filtering closes the direct channel. It does not close the arithmetic one, and the arithmetic
one is invisible.

An aggregate computed over rows a caller may not read is a real number, correctly calculated,
disclosing information about rows that were withheld. Nothing about it looks wrong. There is no
refusal to notice and nothing in an audit log to find.

Two mechanisms close it, and both are described in Chapter 10, *Multidimensional analysis*, from
the modelling side. Here is what they do for security.

**Every cube answer states how much of its input it saw.** `completeness` and `withheld` are
columns on the result row, not metadata beside it — because metadata beside a result is dropped by
the first projection that does not mention it, and a filtered total then looks exactly like a
complete one. Measured on the review server:

```
 region | amount | completeness | withheld
--------+--------+--------------+----------
 north  |  15687 |        0.668 |       83
 south  |  15438 |        0.668 |       83
```

167 of that table's 250 rows carry a region; 83 do not. `0.668` is `167/250`, and `withheld` names
the 83 directly. Two callers with different permissions ask the same question, correctly get
different totals, and each can *see* that they did. Most systems make an operator choose between a
true total and a visible one. This one publishes the fraction.

**A stored cuboid may only serve a caller it was computed for.** A background refresh has no
principal — nobody is logged in at four in the morning — so it builds the *unrestricted* cuboid: an
aggregate over every row. That cuboid may serve only a caller whose own policy withholds nothing.
The operational consequence is worth knowing rather than discovering: background materialisation
helps dashboards and service accounts and does **nothing** for a restricted analyst, whose cuboids
can only be built by their own queries.

> **Key idea** — The cache key for a materialised aggregate includes the caller's visible scope,
> and two scopes are two *tables*. A bug in the lookup therefore cannot serve one principal's rows
> to another, because the rows are not in the file being read.

## 13.5 The audit records what was seen

An audit that records *"a query happened, and here is who ran it"* answers no question anybody
asks after an incident. The record here carries:

Field | Why it is not optional
---|---
The row filter applied | The same statement returns different rows under different policy
The column masks applied | A masked column and an absent one are different disclosures
The table snapshot | The same query returns different rows a day later
The graph epoch | A traversal is reproducible only against the adjacency it walked

Without the last two, a record reproduces nothing, and an audit that cannot reproduce what a
principal saw is a log file with ceremony.

### None of those four was ever populated

The table above described a record the code did not write. The only append site hardcoded *no row
filter and no column masks*, never recorded the version, the statement or the rows returned, and
passed **the first two words of the statement** where a table belongs. `SEC-07`.

The restrictions field is the one worth dwelling on, because it was not merely empty: it said
`allowed, no filter, no masks` on statements where a filter *was* applied. An empty field is a
record that does not answer a question. A field filled with a false value is a record that answers
it wrongly, and an audit is read as evidence.

A read now writes one entry **per table the plan scanned** — taken from the plan, not from the
session and not by looking for table names in the SQL. One per table because a restriction is a
property of a table rather than of a statement: two tables in one query can be filtered
differently and a single row cannot say so. From the plan because recording every table the
session *authorized* would attribute a row count to tables nobody read, and a substring search
over the statement — which is good enough for deciding whether to hydrate a cube, where a false
positive costs a cache lookup — would put a table in somebody's audit trail because its name
appeared in a string literal.

The graph epoch stays `None` and is not guessed. This read path does not traverse a graph, and a
field that is always present and never true is worse than one that is absent and says so.

**The statement is recorded by shape and never by text** — `select region`, not
`SELECT region FROM orders WHERE national_id = '123-45-6789'`. A statement carries the values a
query filtered on, and copying those into a durable log makes the audit a second place the data
lives, with different retention and different access control from the table it came from. The
audit file is also the one most likely to be shipped somewhere else wholesale. That is a decision
this repository had already made and asserted with a test; what was wrong is that the shape was
being recorded **as the table**, which is a different field.

### And a restart erased all of it

`Chain` was a `Vec`. The hash-linked, tamper-evident audit — the one this section describes and
`docs/STATUS.md` marked as a met criterion — lived in memory and was lost when the process
stopped, which is the event most likely to accompany the incident an audit exists for.

It is written to `_audit/chain.jsonl` under the warehouse, one JSON object per line, synced before
the statement is answered. A file opened for append is the shape where *append-only* is enforced
by the thing doing the writing rather than by the code that means to; a row store would let a
later version issue an `UPDATE`. It is also readable by `jq` and by a person, which a chain only
this binary can read is not.

Every sync, and not a buffer: the missing records would be precisely the ones written in the
seconds before whatever made the audit interesting. This is one line per table per statement, not
one per row.

A write that fails does not stop the statement — that is a decision, and the alternative of
refusing to answer is defensible — but it is counted in `sankhya_audit_unwritten_total` and
logged, because the one thing it must not do is fail silently. A server with nowhere to write says
`IN MEMORY ONLY` in its startup line, capitalised like the authentication postures and for the
same reason.

### The chain in memory is a window onto the chain on disk

Making it durable did not make it bounded. The records were a `Vec` that only ever grew, appended
on every statement **and every catalogue listing** — every `\dt`, every JDBC metadata call, every
tab-completion — with no cap and no rotation: roughly 3 to 5 GB a day at a hundred statements a
second, and 26 GB a day at a thousand. `OPS-04`.

A running process now holds the most recent 1,024 records. **The file is the chain**; memory holds
enough to show somebody what just happened and to link the next digest. Two things deliberately
still describe the whole chain rather than the window:

- `len` counts everything ever appended. A count that shrank as records aged out is one nobody
  could compare against what they mirrored — and comparing it is the only way a truncated chain is
  ever noticed.
- `head` is the real head, kept separately from the record that carries it, because that record
  may have aged out.

Verification splits the same way. A windowed chain checks the links it still has, and reports so
in those words; verifying the whole of it means reading the file, which is a deliberate act an
investigation performs rather than something a boot does. Startup reads a window too — loading a
year of audit into memory before answering anything would turn unbounded growth into an unbounded
boot.

### And the timestamp was a counter

Every record's `at` was `*clock += 1`. An audit's timestamps were `1, 2, 3`, restarting at 1 on
every boot, under a comment saying *"a real deployment supplies wall-clock time here"* — and no
deployment did, because no deployment could. An audit that cannot say **when** answers none of the
questions an audit is opened for.

The comment was protecting something real: a component that reads a clock cannot be replayed, and
the audit is the one thing that must reproduce exactly. The reproducible *ordering* was never the
timestamp's job — that is the record's `sequence`, which is what the chain links and what
verification checks — so `at` is wall-clock microseconds now.

**Something was given up, and it is worth naming.** The digest covers the time, so two runs of the
same statements no longer produce the same head. A test relied on that: it compared the audit head
across two fresh servers on the grounds that a difference must therefore be the subject. That
reasoning held only while the clock was fake, and the test now asserts the property it was always
about — that each record names and hashes the user who ran the statement. Byte-identical replay
across processes was never a property of an audit; it was a property of a counter standing in for
a clock, and keeping it would have meant keeping an audit that cannot say when.

The chain is hash-linked with SHA-256 and detects alteration, reordering and insertion. It does
**not** detect truncation of the tail: an attacker who removes the last *n* records leaves a chain
that verifies perfectly. Only publishing the head somewhere append-only makes the true length
knowable. That limitation is asserted by a test rather than left to be discovered, and the head is
printed on every start:

```
  audit chain head 0000000000000000000000000000000000000000000000000000000000000000 (0 record(s))
```

The counterpart on the metrics side is `sankhya_audit_records_total`, whose stated purpose is the
negative case: *a count that stops rising while queries continue means the audit is not recording
them.* Chapter 14, *Observability*, covers what to alert on.

## 13.6 Transport, and the four postures

`FR-SEC-03` asks for three things on the wire: federated identity tokens, mutual TLS, and scram.
**Transport security is built. Identity is not**, and the difference matters more than the shared
requirement number suggests.

One certificate serves both doors — the wire protocol and Arrow Flight SQL — loaded once by
`sankhya-tls`, with each door naming only its own ALPN. Two loaders would mean two sets of
refusals and two answers to *"is this key the one for this certificate?"*, and the divergence
would surface on whichever door is used less.

The wire protocol **negotiates rather than wraps**: a PostgreSQL client opens a plain socket, asks
in eight bytes whether encryption is available, and reads one byte back before any handshake
exists. So the decision belongs to the same state machine that decodes everything else. A GSSAPI
request is declined out loud for the same reason — `psql` with `gssencmode=prefer` is a default on
several Linux distributions, and a server that says nothing leaves the most ordinary client
waiting.

Situation | Answer
---|---
A certificate with no key, or a key with none | **Startup stops**, naming the missing setting
A key that is not the certificate's | Refused at load, not at the first connection
A key file named as the certificate | Named as such — the commonest first-day mistake
A client trust bundle with no anchor | Refused; trusting nobody rejects every client it was meant to accept
A plain client on a door that requires TLS | `28000`, *"connect with sslmode=require"*, before authentication
A peer that connects and never handshakes | Dropped on a deadline

The four postures are `unencrypted`, `TLS offered`, `TLS required`, and `TLS required, and a client
certificate with it`. Requiring is the default once a certificate is configured: an operator who
went to the trouble did not do it so a client could decline.

**The posture is named in words on every start.** The review server this chapter was written
against prints:

```
SANKHYA 0.1.0
  tenant tenant:00000000-0000-0000-0000-000000000001, NO AUTHENTICATION — every connection is accepted, 10 policy rule(s), 10 table(s) known
  listening on 127.0.0.1:55432
  wire protocol unencrypted — passwords cross the network in plain text
```

That is the honest line for a loopback development server, and it is the line the design exists to
force. A server that fell back to plain text because its key was missing is not nearly encrypted:
it is a server whose operator believes it is encrypted, and the belief survives until somebody
captures a packet.

## 13.7 What is not built, by name

The gaps below are stated here rather than in a closing section, because a security chapter that
buries its exclusions is doing the thing this book exists not to do. Chapter 26, *Roadmap and
status*, carries the authoritative version.

**Federated identity, and scram.** A `Principal` is a fixed tenant established at the edge, not
something a certificate or token establishes. Mutual TLS puts the client's certificate where a door
can see it — that is the hook the work will hang from — and nothing yet derives an identity from it.
`M14`. On a loopback this is tolerable and honest; for a client whose entire purpose is connecting
from somewhere else, it is credential exposure, which is why `M14` is gated on transport security
rather than treating it as work inside the milestone.

~~**TLS in the Python binding.**~~ **Built 2026-09-03.** `sslmode` takes PostgreSQL's own
vocabulary — `disable`, `prefer`, `require`, `verify-ca`, `verify-full` — and the default is
`prefer`, which is libpq's. The honesty is elsewhere: `db.connection.encrypted` says what
actually happened, so a caller can assert on their posture rather than assume it. `require`
against a server that declines **refuses**, because a client that asks for encryption, is told
no, and continues has already sent the password it was protecting and cannot un-send it.

**Envelope encryption and per-tenant graph epochs** are built and tested and have **no path through
the front door** — no running process reaches them.

**The external-reader boundary is a hole, and it is documented rather than obscured.** External
engines reading the published warehouse directly — Spark, Trino, DuckDB — bypass row- and
column-level enforcement entirely, because they are reading Parquet files with no SANKHYA process
in the path. This is a product decision (Chapter 6, *Storage and the open table log*), and the
compensating controls are storage-level: object-store credentials, prefix policy, and the fact that
the published tier is the only tier they can reach. A security model with an unmentioned hole is
worse than one with a documented boundary.

**Personal data** is handled by design rather than by deletion. Direct identifiers live only in the
transactional store, with surrogate keys downstream, so an erasure request becomes a transactional
delete plus a vault purge and leaves analytical history, time travel and retention untouched. Where
an identifier must exist downstream, per-subject encryption keys permit cryptographic erasure.

> **Key idea** — The ordinary maintenance scheduler is structurally incapable of destroying
> retained history. Erasure is not a priority level of expiry; it is a different job class with a
> different authorization path. Anything less, and a misconfigured retention default eventually
> deletes records that were legally required to persist. Chapter 15, *Maintenance, tiering and the
> data lifecycle*, specifies the ladder that keeps them apart.

## 13.6a Who a user is, and what that decides

A connection presents a user, and `authenticate` refuses an empty one: an unattributable
connection cannot be audited, and an audit chain that cannot say who is a log with extra steps.
That subject travels inward on the `Caller`, and the audit chain's head differs when the same
statement is run by two people.

Until 2026-09-03 it decided nothing else. **Every user was handed the same role** — a literal
`reader` — so the identity travelled a path nothing distinguished on. The plumbing was finished
and the feature was not, and `STATUS` said so in as many words rather than implying otherwise.

Roles are now the operator's:

```yaml
server:
  users:
    alice: reader, analyst
    bob: reader
```

Rules grant by role (§13.1), so this is the whole of the connection between a person and what
they may read. One line is worth reading twice:

> **The presence of the map is the switch.** An operator who has written down no users has not
> decided anything about roles, and gets the old behaviour: one role for everybody. An operator
> who has written down *one* has decided that the list is the list — so a user absent from it
> holds **no** role, and every rule that grants by role passes them by.

A separate flag would be a flag somebody forgets, and forgetting it in this direction grants
access. Adding a user is a change somebody notices; silently granting one is not.

### And what a password is checked against

Until 2026-09-04 the answer was **nothing**. `authenticate` refused an empty user and then
checked that a password was *present and non-empty*. There was no credential store, no hash and
no comparison anywhere in the workspace, and because the username above is self-asserted, that
means any client connected as any user — including one this server had never heard of — by
sending any byte string. `SEC-01`, and the single most serious finding in the audit.

It was disclosed before it was repaired: the startup line has said `PASSWORD UNVERIFIED` in
capitals since Phase 0, beside the sentence explaining that an operator reading "password
required" opposite "NO AUTHENTICATION" would conclude the first one authenticates.

A password is now checked against a stored verifier:

```yaml
server:
  credentials:
    alice: pbkdf2-sha256$600000$<salt>$<key>
```

Four decisions in that line are worth stating.

**The same switch as roles.** An empty list is the old behaviour, because an operator who has
configured nothing has decided nothing and a server that began refusing every connection on
upgrade is a server nobody upgrades. Naming one user decides the list is the list, and a user
absent from it is refused.

**The count is in the file.** A verifier says how it was made, so raising the default does not
invalidate every credential already written down — which is what makes the default movable at
all. Old verifiers keep working at the count they were made with; new ones are made at the
current default.

**One refusal for both failures.** "No such user" and "wrong password" are the same message.
Telling them apart turns the login into a directory of who exists here, which is the first thing
an attacker asks for and the last thing this door should answer.

**The primitive lives in one crate.** `sankhya-credential` is the only crate that reaches for
`ring`, the same way `sankhya-sandbox` is the only one that reaches for `libc`, and for the same
reason: a second crate deriving its own key material is a second chance to get an iteration
count or a comparison wrong.

> **Key idea**
> This is not SCRAM. PostgreSQL's challenge-response never sends the password, and it is the
> right destination; this verifies a password the client sent in cleartext, which is why the
> transport posture is printed beside it. Getting from *never verified* to *verified against a
> stored key* closes `SEC-01`. Getting from *cleartext over TLS* to *challenge-response* is a
> protocol change, and calling it done here would be the same overstatement this chapter exists
> to avoid.

What this is still not is **federated identity**. The names here are an operator's list, not an
assertion from an identity provider, and mutual TLS puts a client's certificate where a door can
see it with nothing yet deriving a subject from it. That is `FR-SEC-03` and it is unbuilt.

## 13.7a The boundary a user-supplied function runs behind

A user may write a function in Python and have SANKHYA compute it
([ADR-0022](../../adr/0022-user-defined-functions.md)). That is arbitrary code, handed rows a
policy has already filtered **for a particular principal** — so the question is not whether it is
convenient but what stops it from publishing them.

[ADR-0023](../../adr/0023-the-sandbox-a-user-function-runs-in.md) answers it, and the first
decision is the one that decides all the others:

> **The isolation is enforced by the kernel, outside the process running the code.** What Python
> does inside that process is not part of the boundary and is not relied on for any property.

The cheap alternative — stripping `__builtins__`, an audit hook, a source rewriter — is refused by
name. It is not a weak boundary; it is a **decoration**, and a decoration is worse than nothing,
because with no boundary nobody grants the capability lightly.

| Prohibition | Mechanism | What it stops |
|---|---|---|
| No network | A network namespace with no interface but a disconnected loopback | Exfiltration — a socket turns *may read* into *may publish* |
| No filesystem | A mount namespace pivoted onto a read-only tree holding the interpreter and nothing else | Reading the warehouse directly, which bypasses every policy; reading the server's keys; writing anything at all |
| No subprocess | The same mount namespace: no shell, no `/bin`, and nothing in `/usr/bin` but the interpreter | Escaping the two above by starting something that was not the worker |
| No sight of this server | A PID namespace the worker is *inside*, so `getppid()` is `0` | Signalling the process that started it, whose user it shares |
| Not root in its own namespace | An identity map to `65534`, so `exec` drops the capability set | A capability held inside the namespace being turned on the namespace |
| Bounded time | `RLIMIT_CPU`, and a wall-clock deadline the parent enforces by killing | A function that never returns is an outage, not an error |
| Bounded memory and output | `RLIMIT_AS`, `RLIMIT_FSIZE` of zero, and a cap on the bytes returned | One query taking the machine down |

Three things about this are worth reading twice, because each is a place the obvious design is
wrong:

- **A `seccomp` filter cannot deliver "no subprocess".** The mechanisms are applied between `fork`
  and `exec`, and the child must `exec` once to become the worker at all; a filter denying `execve`
  denies that one, and `seccomp` has no state with which to allow the first and refuse the second.
  What delivers the prohibition is the empty jail.
- **Where the mechanism does not exist, the feature is refused rather than degraded.** Several
  distributions ship unprivileged user namespaces disabled and several operators turn them off
  deliberately. `CREATE FUNCTION` then fails, naming the mechanism. It does not fall back.
- **Creating one is a grant, not a right.** The sandbox stops the code reaching out; it cannot
  review it. `CREATE FUNCTION` needs a capability that is not granted by default, and the source is
  stored and shown so the grant is reviewable.

Each prohibition is proved by trying it — a process that opens a socket, reads a file it was not
given, writes, loops forever — because a test that asserted which flags were passed would pass for
a mechanism this kernel does not honour, and that is precisely the case worth detecting.

### Six ways the table above was ahead of the code

An audit read this chapter against the crate and found that four of the five rows were describing
something stronger than what ran, and that the startup probe was checking something other than
what it claimed. Every one of them is the same shape: a mechanism named correctly, applied
incompletely, and asserted by nothing.

**The worker was `root` in its own namespace.** The identity map read `0 <server uid> 1`, so the
namespace's root was the server's user. `execve` of a file with no file capabilities only drops
the capability set when the effective user id is *not* zero — so the interpreter started holding
every capability the namespace had. It now maps to `65534`, and the capabilities go at `exec`.
`SEC-09`.

**Outside the namespace it is still the server's user, and this chapter used to imply otherwise.**
`ADR-0023` Decision 2 promised *"a distinct unprivileged uid and gid"*, and the table here quietly
dropped that row rather than flagging it. An unprivileged user namespace cannot deliver it: the
kernel permits one map line and its parent-side id must be the writer's own. A distinct id needs a
`newuidmap` helper installed setuid and a `/etc/subuid` range allocated to the server's user,
which is a deployment decision the server cannot make for itself. The row is now stated as what it
is rather than omitted.

**A user function could kill the server.** `unshare(CLONE_NEWPID)` places the caller's *children*
in the new PID namespace and leaves the caller behind — and the caller was the process that then
`exec`ed into the worker. So the worker sat in the host PID namespace, could see every process on
the machine, and shared the server's uid: `os.kill(os.getppid(), 9)` worked. It now forks once
more, so the worker is PID 1 of a namespace holding nothing else and `getppid()` is `0`. `SEC-10`.

That fork brought two problems of its own, and both are worth naming because neither is obvious.
The process left outside must **close every descriptor above the standard streams**: `spawn` does
not return until every copy of its close-on-exec pipe is closed, so holding one blocks the caller
for the whole run and starts the deadline clock after the function has already finished. And the
worker must be **tethered** with `PR_SET_PDEATHSIG`, because PID 1 of a namespace is not reaped by
anybody and would otherwise go on running after the query that started it was told it timed out.

**The jail held every binary on the machine.** *No subprocess* is delivered entirely by the empty
jail, and the jail bound `sys.base_prefix` — which on a system interpreter is `/usr`. The comment
three lines above claimed it avoided binding `/usr` wholesale. It now asks `sysconfig` for the
standard library by name, binds the interpreter **as a file** rather than the directory it sits in,
and refuses any answer that is one of the directories a machine keeps its programs in. `SEC-11`.

The honest form of the promise is narrower than *"there is nothing to exec"*: the shared-object
directories have to be there for the dynamic linker, and some of them hold executables. What the
jail delivers is that there is no shell, no `/bin`, no `/usr/bin` beyond the interpreter itself,
and nothing a `subprocess` call names by habit.

**A forked grandchild blocked the parent for ever.** The parent read the child's output only once
`try_wait` said it was gone, and a pipe's write end is held by every process that inherited it —
so a grandchild that outlived the child meant the read never saw end-of-file, with the deadline
loop already exited. This runs inside a DataFusion accumulator on a Tokio worker thread, so a
handful of such queries stop the server. Output is now read on threads of its own from the moment
the process exists. `SEC-12`.

**The output cap could not fire at its shipped value.** For the same reason: a child writing more
than a pipe holds (~64 KiB) blocked on the write, was killed at the deadline, and was reported as
having run out of *time*. With the cap at 64 MiB the `OutOfRoom` arm was unreachable, and the test
that proved the cap used a cap of 64 bytes. Counting while the run is going is what makes the
bound a bound. `SEC-13`.

**And the probe checked one mechanism out of fifteen.** `ADR-0023` Decision 3 says the probe *runs
the mechanism, once, against a trivial worker — it does not read a capability flag and hope*. It
forked, called `unshare`, and stopped. A machine where `unshare` succeeds and `pivot_root` fails
passed it and failed at the first `CREATE AGGREGATION` in production, which is the outcome the
decision exists to prevent. It now calls the same function a real spawn calls, and names the step
that refused. `SEC-14`.

> **Pitfall** — Every one of these was a mechanism that was *present*. Reviewing a sandbox by
> checking that the right syscalls appear in the file is reviewing the table of contents.

## 13.8 How this is tested, and why mutation testing is not optional here

A negative test suite is a first-class deliverable: for every policy fixture it asserts that
forbidden rows, columns and edges are absent from results, absent from the physical plan, and
absent from graph memory. Absent from three places, because a row filtered at the last step was
still read, and a row present in graph memory is one traversal away from being a result.

**Mutation testing is applied to the policy component**, and the reason is arithmetic rather than
rhetorical: a surviving mutant means a test that passes for the wrong reason, and on this component
that is a data breach with a green build. Eleven of eleven mutations are caught, including removing
the tenant comparison, letting grants outvote a denial, and treating absent grants as permission.

Three defects found this way are worth recording, because each passed review first:

- **The secured table was secured in name only** — §13.2's pushdown, caught on the first run of the
  test written to check it.
- **A mutation survived twice before the test was honest.** *Push the limit below the security
  filter* kept passing, because the test ran against `MemTable`, which ignores limits too. A test
  whose subject ignores the thing under test proves nothing. It now runs against a provider that
  honours a limit, with the forbidden rows ordered first so the cut bites.
- **Removing the audit chain's previous-digest check left every test green.** Every test that broke
  a link also broke the sequence number, which fires first — so the link check was never the thing
  catching anything. A competent attacker renumbers after a deletion. That test exists now, along
  with one for a spliced record.

Chapter 23, *How this is tested*, treats the method in general. What belongs here is the reason it
is mandatory on this component and merely valuable elsewhere: everywhere else, a test passing for
the wrong reason costs a defect. Here it costs the property the whole system is sold on.
