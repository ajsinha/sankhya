# 13. Security, tenancy and policy

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
| No subprocess | The same mount namespace: inside the jail there is nothing to exec | Escaping the two above by starting something that was not the worker |
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
