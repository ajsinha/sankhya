<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Security

**Document ID:** SNK-SD-001
**Version:** 0.1.0
**Status:** Implementation — M0, M1, M3, M4, M7, M10 and M13 complete; M2 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Date:** 2026-09-06
**Companions:** [`ARCHITECTURE.md`](ARCHITECTURE.md) — why the choke point is shaped this way. [`OPERATIONS.md`](OPERATIONS.md) — every setting and how to change it. [`REMEDIATION.md`](REMEDIATION.md) — the record of what was found and when.

---

## 1. Why this document exists

Every fact in §2 is documented honestly somewhere in this repository. That is the problem. They are
documented in five different places, each beside the mechanism it belongs to, each with the
reasoning that makes it defensible in context — and a security reviewer does not read a system one
mechanism at a time. They read for the shape of the exposure, and the shape only appears when the
five sit on one page.

So this page is the one place they sit together. It is short on purpose: the design arguments are in
[`ARCHITECTURE.md`](ARCHITECTURE.md), the settings are in [`OPERATIONS.md`](OPERATIONS.md), and
repeating either here is how a fact comes to be true in one document and stale in the other.

Everything below names the file that decides it. Nothing here is a claim about intent.

---

## 2. The posture you get by not deciding

This is a shipped default table, not a recommendation. Every row is what a server does when its
operator has configured nothing.

| | Ships as | Consequence | Decided in |
|---|---|---|---|
| **TLS** | **Off**, both doors | Passwords and results cross the network as typed | `crates/sankhya-server/src/main.rs`, `transport_security` |
| **Password demanded** | `server.require_password: true` | A connection must present a non-empty password | `crates/sankhya-server/src/main.rs` |
| **Password verified** | **No** — `server.credentials` is empty | **Any non-empty password from any username, including one this server has never heard of, connects** | `crates/sankhya-server/src/wiring.rs`, `Handler::authenticate` |
| **Roles** | `server.users` empty | Every authenticated connection is granted the single role `reader` | `crates/sankhya-server/src/wiring.rs`, `Server::principal` |
| **Policy** | `policy.rules` absent | `permissive_policy` — role `reader` may read **every discovered table**, with no row filter and no mask | `crates/sankhya-server/src/wiring.rs` |
| **Arrow Flight SQL identity** | An unverified request header | Any client that completes the handshake may assert any username | `crates/sankhya-server/src/flight.rs` |
| **`/metrics`** | Unauthenticated, no TLS, loopback | Anyone who can reach the port scrapes it | `crates/sankhya-server/src/scrape.rs` |
| **Per-table metric labels** | `server.metrics_detail: false` | The warehouse is not enumerable through the scrape endpoint | `crates/sankhya-server/src/main.rs` |
| **User-defined functions** | `server.user_functions: false` | `CREATE AGGREGATION` is refused | `crates/sankhya-server/src/main.rs` |
| **Listeners** | Loopback in every case | Nothing is reachable off-host until somebody says so | `config/application.yaml` |

The two rows in bold are the ones that surprise people, and the server says both of them out loud
on every start rather than leaving them to be discovered:

```
SANKHYA 0.1.0
  tenant tenant:0000…0001, PASSWORD UNVERIFIED — no credentials are configured, so any non-empty
  password is accepted from any user, NO POLICY CONFIGURED — every authenticated user may read
  every one of the 10 table(s) below, 10 table(s) known
  listening on 127.0.0.1:5433
  wire protocol unencrypted — passwords cross the network in plain text
```

`Server::describe` in `crates/sankhya-server/src/wiring.rs` builds that line, and the capitalisation
is the point. An operator reading *"password required"* opposite *"no authentication"* concludes the
first one authenticates.

---

## 3. The five findings a reviewer needs together

### 3.1 TLS is off by default, and cannot be turned on by environment

There is no `server.tls.enabled`. **The presence of the certificate pair is the switch**, and both
halves are required: a certificate with no key, or a key with no certificate, **stops startup**
rather than falling back to plaintext (`crates/sankhya-server/src/main.rs`). A server that started
in the clear because a path was wrong would be one whose operator believes it is encrypted, and the
belief survives until somebody captures a packet.

The consequence worth stating separately: **TLS has no `SANKHYA_*` environment variable.**
`legacy_environment()` in `crates/sankhya-server/src/main.rs` maps no TLS key, so a deployment
configured entirely by environment — which is how the shipped container is configured — **cannot
turn TLS on.** It needs a mounted configuration file. That is a gap in the configuration surface
rather than in the transport, and it is recorded here because the two read identically from outside.

### 3.2 `require_password: true` ships with an unverified empty credential list

`Handler::authenticate` in `crates/sankhya-server/src/wiring.rs` does two things in order. It
refuses an empty *username*, because an unattributable connection cannot be audited and an audit
chain that cannot say who is a log with extra steps. Then it checks that a password is present and
non-empty — and if `settings.credentials` is empty it returns success.

So the shipped posture is: a password is **demanded** and **not checked**. Nothing at startup
refuses an empty credential list; the only signal is the banner. This is deliberate, and the
argument for it is that a server which began refusing every connection on upgrade is a server nobody
upgrades — the behaviour is pinned by
`a_server_with_no_credentials_keeps_the_behaviour_it_had` in
`crates/sankhya-server/tests/wiring.rs`.

Naming **one** user decides that the list is the list: a user absent from it is then refused. The
same rule governs `server.users`, and it runs in the safe direction — forgetting to add somebody
denies them, and there is no separate flag to forget.

Until 2026-09-04 there was no credential store at all: `authenticate` checked that a password was
non-empty and nothing else, workspace-wide. That is `SEC-01`, the most serious finding of the audit,
and the banner said `PASSWORD UNVERIFIED` in capitals before the repair rather than after it.

### 3.3 `/metrics` is unauthenticated

`crates/sankhya-server/src/scrape.rs` is a hand-rolled HTTP responder on its own listener. It serves
exactly one target — `is_metrics_request` compares the path **whole**, not by prefix — caps a
request at 8 KiB, and performs no authentication of any kind. There is no TLS on that listener
either.

That follows Prometheus's convention and it is a real exposure, so the disclosure it can make is
bounded instead. `server.metrics_detail` defaults to `false`, which withholds the per-table
`sankhya_table_live_files` gauge whose label is a fully-qualified table name — a breakdown that
enumerates the warehouse to anybody who can reach the port (`SEC-08`). What is exported either way
is `sankhya_table_live_files_max`, which carries no label: an alert fires on the fact that *some*
table has too many files, and **which one** is a question `sankhya-server doctor` answers to
somebody who has authenticated.

Turn `metrics_detail` on only where the metrics interface is one clients cannot reach.

### 3.4 Arrow Flight SQL takes its identity from an unverified header

`crates/sankhya-server/src/flight.rs` derives the caller from gRPC request metadata under the key
**sankhya-user** (the constant `USER_KEY`). It is trimmed and refused if empty. **It is not verified
in any way** — there is no
password exchange on that door, no token, and no binding to a client certificate. Even with
`server.tls.client_ca` set, so that the handshake is mutual, the certificate's subject is never
compared against the header.

Anyone who can complete the handshake may assert any username and receive that username's roles.

The consequence reaches the ticket, too. A Flight ticket (`crates/sankhya-api-flight/src/ticket.rs`)
is a length-prefixed blob, deliberately unsigned, admitted on tenant, subject and a five-minute
expiry. Its own comment reasons that a client cannot forge the identity it authenticated as. On this
door **that premise does not hold**, because the identity was never authenticated.

There is a second consequence that is easy to miss: **a Flight query writes no audit record.**
`audit::record_read` is called from the wire-protocol statement path in
`crates/sankhya-server/src/wiring.rs` and from nowhere else. `crates/sankhya-server/src/flight.rs`
mentions the audit only in prose.

Until federated identity exists (`FR-SEC-03`, `M14`), **treat the Flight port as reachable only from
where every caller is already trusted.**

### 3.5 The Flight door does not call the write refusal the psql door calls

The wire-protocol door refuses anything that is not a read. `refuse_if_not_a_read` in
`crates/sankhya-server/src/execute.rs` inspects the **planned logical plan** — `Ddl`, `Dml` and
`Copy` are rejected with SQLSTATE `0A000` — rather than sniffing keywords, and `execute::run` calls
it on every statement.

`crates/sankhya-server/src/flight.rs` does not go through `execute::run`. It plans with
`create_logical_plan` and executes with `SessionContext::sql`, so `refuse_if_not_a_read` never runs.

The asymmetry matters more than "one door checks and the other does not", and the reason is written
into `execute.rs` beside the call: `SessionContext::sql` runs a data-definition statement **during
planning** and hands back a frame over the empty result. A check applied to the returned plan
therefore fires after the table has already been created. The psql door is a read-only surface by
construction; the columnar door is not one at all.

> **This is the one item on this page that reads as a code change rather than a documentation
> change**, and it is recorded as such in §9.

---

## 4. The choke point

The design argument is in [`ARCHITECTURE.md`](ARCHITECTURE.md). What belongs here is what the
control is and where a reviewer verifies it.

Three engines answer questions — SQL, graph, and (when it runs) tiering. Each resolves a table
through the same catalogue, and the catalogue's resolution takes a decision value called `Guard`
(`crates/sankhya-catalog/src/guard.rs`). Its properties are chosen so that the failure a security
architecture must prevent is not *the wrong policy* but *no policy, on one path, on one day*:

| Property | Consequence |
|---|---|
| No public constructor | It cannot be made without a policy decision |
| No public fields | It cannot be forged from parts |
| No `Default` | It cannot appear by omission |
| `from_decision` returns `Option`, `None` on denial | A denial cannot be turned into a guard |

`SecuredTable::new` in `crates/sankhya-catalog/src/secured.rs` takes a `Guard` **by value** and is
the only constructor. `execute::session_reaching` in `crates/sankhya-server/src/execute.rs` is where
both doors register tables: it filters the servable set through `Guard::authorize(…, Action::Read)`
and registers nothing it did not get a guard for.

Four properties follow, and each is asserted by a test in
`crates/sankhya-catalog/tests/enforcement.rs`:

- **A table you may not read does not exist.** It is never registered, so naming it fails to resolve
  with the same code, SQLSTATE and words as naming a table that was never created. The difference
  between the two messages would be a working enumeration oracle, and there is no configuration that
  turns it on. `information_schema.tables` is filtered server-side for the same reason.
- **The row predicate is conjoined above the scan**, where nothing can decline it. The first
  implementation handed it to the provider as a pushdown filter; `MemTable` declines filters, so
  every row came back with no error raised anywhere. Presence in the **final physical plan** is what
  is asserted now, not presence of the intent in the logical one.
- **A tautology in the query cannot widen the policy**, because the policy is not part of the query.
- **A `LIMIT` does not short-circuit the filter.**

Masks are applied by a projection at the end of the scan (`crates/sankhya-catalog/src/mask.rs`),
built from Arrow directly rather than from `concat`, `repeat` and `right` — `concat` treats a null
as the empty string, so the obvious implementation turns a missing address into `***`.

### Two documented bypasses

Stated here rather than left to be found, because a choke point with an unlisted exception is not a
choke point.

**`Server::hydrate_unrestricted`** (`crates/sankhya-server/src/wiring.rs`) builds a fresh session and
registers providers **unsecured**. That is what makes it the *unrestricted* scope rather than one
principal's: a background cuboid refresh has no principal, because nobody is logged in at four in
the morning. Its safety rests on the resulting cuboid being keyed `unrestricted` and on
`Guard::withholds_nothing()` gating who may read it (ADR-0008) — which is a *procedural* invariant
where everything else on this page is a type-level one.

**`Action::Insert`, `Action::Update` and `Action::Delete` rules parse and are consulted by nothing.**
Every authorization call in the server passes `Action::Read`. A policy author who writes
`action: insert` gets a file that loads cleanly, a server that starts, and a rule that enforces
nothing. `crates/sankhya-server/src/feeds.rs` is the clearest case: `RESUME FEED` is *authorized* as
a read and *audited* as an insert.

---

## 5. The policy reference

### 5.1 There is no policy file

`crates/sankhya-authz` has no `serde` dependency and no deserializer. There is no `policy.yaml`, no
`--policy` flag and no `SANKHYA_POLICY` variable. **The policy is a section of the main
configuration file**, read by a hand-written parser in `crates/sankhya-server/src/policy.rs` over
flattened dotted keys.

| | |
|---|---|
| Default location | `config/application.yaml`, relative to the process working directory |
| Overridden by | `SANKHYA_CONFIG` — a **comma-separated list** of files, lowest precedence first |
| A named file that is missing | **Startup is refused.** A missing *default* file is skipped silently, which is the difference between "I did not configure this" and "I configured it and you cannot find it" |
| Section | `policy.rules` |
| Absent section | `permissive_policy` — see §2 |
| A rule that does not parse | **Startup stops**, naming the rule. A policy with a rule quietly dropped permits more than it says, and the person who wrote the rule believes it is in force |

### 5.2 Grammar

```yaml
policy:
  rules:
    analysts_read_northern_orders:   # the rule's name, and it is yours
      role: analyst                  # required
      table: sales.orders            # required, and must be qualified
      action: read                   # required
      effect: allow                  # optional; defaults to allow
      where: "region = 'north'"      # optional; defaults to every row
      mask:                          # optional; defaults to no mask
        email: null
        phone: partial:4

    interns_may_not_read_payroll:
      role: intern
      table: hr.payroll
      action: read
      effect: deny
```

Each rule is **named**, and the name is the operator's, because *"rule 3 does not parse"* is a
refusal somebody has to count to and a policy is a file people review line by line.

The table must be **qualified**. A policy is the one place an ambiguous name is fatal: a bare
`orders` means one table today and two the day somebody adds a schema, and the rule would then apply
to *neither*, because a contested bare name resolves nowhere.

A denial is **spelled out**, never inferred from an absent grant. Forbidding is a decision somebody
made and should read as one — even though an absent grant also denies (§5.5).

### 5.3 Every field

| Field | Required | Default | Accepted | Refused |
|---|---|---|---|---|
| `role` | yes | — | Any string, taken verbatim and **case-sensitive** | empty |
| `table` | yes | — | `<schema>.<table>` | Anything with no `.`, and anything with an empty half |
| `action` | yes | — | `read`, `insert`, `update`, `delete` — lower-cased before matching | anything else |
| `effect` | no | `allow` | `grant`, `allow`, `deny`, `forbid` — lower-cased before matching | anything else |
| `where` | no | every row | A SQL scalar expression — see §5.4 | See §5.4 |
| `mask.<column>` | no | no mask | See §5.5 | anything else |

Only `read` is enforced today (§4).

### 5.4 The `where` expression, and what it may contain

The predicate is parsed by `parse_predicate` in `crates/sankhya-catalog/src/secured.rs`: `sqlparser`
with the generic dialect, then converted against the **table's real schema** through a context whose
table sources, scalar functions, aggregates, window functions and variables all resolve to nothing.

The practical grammar is therefore narrower than "SQL":

- **Permitted** — column references, literals, comparison and boolean operators, `IN`, `BETWEEN`,
  `IS NULL`, arithmetic on columns of the table itself.
- **Not permitted** — function calls, aggregates, window functions, session variables, subqueries,
  and any reference to another table.

A predicate naming a column the table does not have is **refused when the table is opened**, not
silently matched against zero rows. The distinction matters: a filter that matches nothing looks
like a policy working perfectly.

### 5.5 Masks: every accepted spelling

Parsed by `policy::mask` in `crates/sankhya-server/src/policy.rs`, which splits on the first `:`.
The kind is matched case-insensitively; the argument is not.

| Written | Mask | What comes back | What happens to a null |
|---|---|---|---|
| `null` (any case) | `Mask::Null` | Nothing, at the column's own type | It was already null |
| *(empty)* — YAML `email:` with no value | `Mask::Null` | Same as above | Same as above |
| `constant:<text>` | `Mask::Constant` | `<text>`, in every row | **It becomes the constant too** |
| `partial:<n>` | `Mask::Partial` | All but the last `n` characters replaced by `*`. A value of `n` characters or fewer is returned **unchanged** | **It stays null** |

**There is no `hash` and no `redact`.** `hash` appears in the test suite as an example of a mask that
must be *refused*. `partial:some` — a non-numeric argument — is refused.

The two null rules differ deliberately. A constant mask that left nulls alone would publish which
rows have no value, and *"this customer has no email address"* is a fact about that customer. A
partial mask that turned a null into `***` would be inventing a value where there is none, and a
reader could not tell the two apart.

A mask a column cannot carry — a text mask over an integer, or a column the table does not have — is
refused **when the table is opened**, not on the first query that happens to select it.

`Mask::Partial` documents its own limit in `crates/sankhya-authz/src/policy.rs`: a partially masked
value still leaks. It is a usability concession and must not be read as anonymisation.

### 5.6 How rules combine

`PolicySet::decide` in `crates/sankhya-authz/src/policy.rs`, in order:

1. **Filter by tenant first.** The tenant boundary is the one thing that must not depend on anybody
   having written the right row.
2. **Any denial wins**, evaluated before any grant.
3. **No grant is a denial.** Absence is refusal, not permission.
4. **Row filters across several grants are OR-ed**, sorted and de-duplicated, each parenthesised. A
   grant with **no** filter absorbs the others — being granted the whole table cannot be narrowed by
   also being granted part of it.
5. **Column masks are intersected.** A column is masked only if *every* applicable grant masks it,
   and the spelling taken is the first grant's.
6. *"No grant"* and *"explicitly denied"* render **identical text**, so a refusal cannot be used to
   probe the policy.

### 5.7 What a change needs: `SIGHUP` or a restart

The signal handler is in `crates/sankhya-server/src/main.rs`, on `tokio`'s `SignalKind::hangup()`.
On `SIGHUP` the **whole** configuration is re-read — and then exactly three keys are applied.

| Setting | Takes effect on |
|---|---|
| `maintenance.interval` | **`SIGHUP`** |
| `maintenance.compact_every` | **`SIGHUP`** |
| `maintenance.orphan_sweep_every` | **`SIGHUP`** |
| `policy.rules.*` | Restart |
| `server.credentials.*` | Restart |
| `server.users.*` | Restart |
| `server.require_password` | Restart |
| `server.tls.*` | Restart |
| `server.listen`, `server.flight_listen`, `server.metrics_listen` | Restart |
| `server.metrics_detail`, `server.user_functions` | Restart |
| `warehouse.*` | Restart |

Two behaviours of the handler are worth knowing before you rely on it, both in
`crates/sankhya-server/src/main.rs`:

- **The handler is installed only when maintenance is enabled.** With `maintenance.interval: 0` there
  is no handle to reconfigure, so the signal is not handled at all — and an unhandled `SIGHUP`
  terminates the process. **On a server with maintenance disabled, `SIGHUP` is a restart with extra
  steps.**
- **Asking to disable maintenance over `SIGHUP` is not honoured.** The thread keeps running under its
  previous settings and a line is printed. Disabling maintenance is a restart.

**A revoked grant does not take effect until the process restarts.** That is the operationally
important half of this table and it is stated plainly rather than left to be inferred from the
absence of a row.

---

## 6. Authentication, and what a password is checked against

### The wire-protocol door

One method: `AuthenticationCleartextPassword` (`crates/sankhya-api-pg/src/session.rs`). There is no
MD5, no SASL, no SCRAM-SHA-256 and no GSSAPI — a GSSAPI encryption request is **declined out loud**
rather than ignored, because `psql` with `gssencmode=prefer` is a default on several Linux
distributions and a server that says nothing leaves the most ordinary client waiting.

Cleartext is only defensible behind TLS. `Encryption::Required` is the mechanism that makes it so,
and a plain client on a door that requires TLS is refused with `28000` and *"connect with
sslmode=require"* **before authentication** — a password sent to discover the requirement would
already have crossed the wire.

A verifier is `pbkdf2-sha256$<iterations>$<salt>$<key>`, PBKDF2-HMAC-SHA256 over `ring`, 600,000
iterations by default and 32 bytes of salt and key (`crates/sankhya-credential/src/lib.rs`).
Verification is constant-time. Four properties are deliberate:

- **The count is in the line.** A verifier says how it was made, so raising the default does not
  invalidate every credential already written down.
- **One refusal for both failures.** *"No such user"* and *"wrong password"* are the same message.
  Telling them apart turns the login into a directory of who exists here.
- **The primitive lives in one crate.** `sankhya-credential` is the only crate that reaches for
  `ring`, the same way `sankhya-sandbox` is the only one that reaches for `libc`. A second crate
  deriving its own key material is a second chance to get an iteration count or a comparison wrong.
- **A malformed verifier refuses startup**, naming the user.

Write one with `sankhya-server hash-password`, which reads the password from standard input rather
than taking it as an argument — an argument is in shell history and in `ps` output for every user on
the machine. It is answered **before** the configuration is read, so it still works on a server whose
configuration is what you are trying to fix.

### The columnar door

Nothing. See §3.4.

### What identity is not

A `Principal` is a fixed tenant established at the edge plus a self-asserted subject, not something a
certificate or a token establishes. `FR-SEC-03`'s federated identity tokens and SCRAM are **not
built**; mutual TLS puts the client's certificate where a door can see it, and that is the hook the
work will hang from. `M14`, and it is gated on transport security rather than treated as work inside
the milestone.

---

## 7. Audit

`<warehouse>/_audit/chain.jsonl`, one JSON object per line, hash-linked, `sync_data`'d on every
append (`crates/sankhya-audit/src/journal.rs`). The directory is `_`-prefixed so warehouse discovery
skips it.

A record carries: `sequence`, `at` (microseconds, wall clock), `tenant`, `subject`,
`authentication`, `table`, `action`, `decision` (allowed, the row filter that applied, the column
masks that applied), `data_version`, `statement`, `rows_returned`, `previous` and `digest`
(`crates/sankhya-audit/src/chain.rs`).

Four things about it are load-bearing:

- **`statement` is the statement's *shape*, never its text.** `audit::statement_shape` in
  `crates/sankhya-server/src/audit.rs` takes the first word stripped to ASCII letters, plus a second
  word **only if it is in a closed keyword list**. `select 'a-secret'` records as `select`, and
  `select nosuchcolumn` records as `select`. That closed list is the repair for `SEC-16`: taking the
  first two words unconditionally put caller data in the audit, and two audit tests were codifying
  the leak.
- **One record per table the *plan* scanned**, taken from the plan rather than from the session, so
  a table registered and not read is not recorded as read.
- **It is not tamper-proof, and says so.** The chain detects alteration and splicing; it cannot
  detect truncation of the tail. The head hash is printed at every start so an operator can pin it
  somewhere this server cannot reach.
- **Flight statements are not audited at all** (§3.4).

If the journal cannot be opened the server still starts and says so — `IN MEMORY ONLY — it will be
lost at the next restart`. `sankhya_audit_unwritten_total` is above zero when a record was made and
did not reach disk; [`runbooks/audit-unwritten.md`](runbooks/audit-unwritten.md) is the runbook.

Envelope encryption lives in `crates/sankhya-audit/src/keys.rs`. It is built and tested, the shipped
key provider **does not encrypt** and its name says so, and nothing in the server reaches it.

---

## 8. Transport, and the four postures

One certificate serves both doors, loaded once by `sankhya-tls`, each door naming only its own ALPN.
Two loaders would mean two sets of refusals and two answers to *"is this key the one for this
certificate?"*, and the divergence would surface on whichever door is used less.

The wire protocol **negotiates rather than wraps**: a PostgreSQL client opens a plain socket, asks in
eight bytes whether encryption is available and reads one byte back before any handshake exists — so
the decision belongs to the same state machine that decodes everything else
(`crates/sankhya-api-pg/src/session.rs`), and only the handshake happens outside it.

| Situation | Answer |
|---|---|
| A certificate with no key, or a key with none | **Startup stops**, naming the missing setting |
| A key that is not the certificate's | Refused at load, not at the first connection |
| A key file named as the certificate | Named as such — the commonest first-day mistake |
| A client trust bundle with no anchor | Refused; trusting nobody rejects every client it was meant to accept |
| A plain client on a door that requires TLS | `28000`, *"connect with sslmode=require"*, before authentication |
| A peer that connects and never handshakes | Dropped on a deadline |

The four postures are `unencrypted`, `TLS offered`, `TLS required`, and `TLS required, and a client
certificate with it`. Requiring is the default once a certificate is configured: an operator who went
to the trouble did not do it so a client could decline. `server.tls.require: false` asks out of it by
name, and applies **only to the wire door** — a Flight client either handshakes or gets nothing.

**Certificate expiry is never checked at load.** `crates/sankhya-tls/src/lib.rs` states that
deliberately: a certificate that expires while the process runs would not be caught by a check at
startup anyway, and the check that matters is the one your monitoring does. Read it as a gap you
must cover elsewhere, not as a property this server provides.

---

## 9. A user function's boundary

`server.user_functions` is **off** by default, and `CREATE AGGREGATION` is refused while it is. It
runs code the caller supplied over rows a policy has already filtered *for a particular principal*,
so the question is not convenience but what stops that code publishing them.

The first decision decides all the others: **the isolation is enforced by the kernel, outside the
process running the code** (ADR-0023, `crates/sankhya-sandbox/src/lib.rs`). Stripping
`__builtins__`, an audit hook or a source rewriter is refused by name — that is not a weak boundary,
it is a decoration, and a decoration is worse than nothing because with no boundary nobody grants
the capability lightly.

| Prohibition | Mechanism |
|---|---|
| No network | A network namespace with no interface but a disconnected loopback |
| No filesystem | A mount namespace pivoted onto a read-only tree holding the interpreter and nothing else |
| No subprocess | The same empty jail — no shell, nothing in `/usr/bin` but the interpreter |
| No sight of this server | A PID namespace the worker is inside, so `getppid()` is `0` |
| Not root in its own namespace | An identity map to `65534`, so `exec` drops the capability set |
| Bounded time | `RLIMIT_CPU`, and a wall-clock deadline the parent enforces by killing |
| Bounded memory and output | `RLIMIT_AS`, `RLIMIT_FSIZE` of zero, and a cap on the bytes returned |

Two consequences of that choice are worth stating. **A `seccomp` filter cannot deliver "no
subprocess"** — the child must `exec` once to become the worker at all, and `seccomp` has no state
with which to allow the first `execve` and refuse the second; what delivers the prohibition is the
empty jail. And **where the mechanism does not exist, the feature is refused rather than degraded**:
several distributions ship unprivileged user namespaces disabled, and `CREATE AGGREGATION` then
fails naming the mechanism rather than falling back.

Each prohibition is proved by trying it — a worker that opens a socket, reads a file it was not
given, writes, loops for ever — because a test asserting which flags were passed would pass for a
kernel that does not honour them, which is precisely the case worth detecting. An audit found four
of five rows of an earlier version of this table describing something stronger than what ran; that
record is in [`REMEDIATION.md`](REMEDIATION.md).

---

## 10. The boundary that is a hole, on purpose

External engines reading the published warehouse directly — Spark, Trino, DuckDB — **bypass row- and
column-level enforcement entirely.** They are reading Parquet with no SANKHYA process in the path.

This is a product decision, not an oversight: open storage is what makes SANKHYA a participant in a
data estate rather than a replacement for one, and a table nobody else can read is a table locked in.
The compensating controls are storage-level — filesystem or object-store credentials, prefix policy,
and the fact that the published tier is the only tier an external reader can reach. The arrival tier,
the transactional store and the audit chain are not under the warehouse root.

**A security model with an unmentioned hole is worse than one with a documented boundary.** If
row-level policy must hold against every reader, the warehouse root must not be readable by them.

Personal data is handled by design rather than by deletion: direct identifiers live only in the
transactional store, with surrogate keys downstream, so an erasure request is a transactional delete
plus a vault purge and leaves analytical history untouched. That is the design;
[`ARCHITECTURE.md`](ARCHITECTURE.md) marks how much of it runs.

---

## 11. How this is tested

A negative suite asserts, for every policy fixture, that forbidden rows, columns and edges are absent
from the **result**, from the **physical plan**, and from **graph memory** — three places, because a
row filtered at the last step was still read, and a row in graph memory is one traversal away from
being a result.

**Mutation testing is mandatory on the policy component** rather than merely valuable, and the reason
is arithmetic: a surviving mutant is a test that passes for the wrong reason, and on this component
that is a data breach with a green build. Eleven of eleven are caught, including removing the tenant
comparison, letting grants outvote a denial, and treating an absent grant as permission.

Three defects found this way are worth carrying, because each passed review first:

- **The secured table was secured in name only** — the pushdown of §4, caught on the first run of the
  test written to check it.
- **A mutation survived twice before the test was honest.** *Push the limit below the security
  filter* kept passing because the test ran against `MemTable`, which ignores limits too. A test
  whose subject ignores the thing under test proves nothing.
- **Removing the audit chain's previous-digest check left every test green.** Every test that broke a
  link also broke the sequence number, which fires first — so the link check was never what caught
  anything. A competent attacker renumbers after a deletion.

What is **not** tested: there is no check that enumerates every table registration in the workspace
and asserts each one carries a `Guard`. `a_secured_table_cannot_be_built_without_a_guard` in
`crates/sankhya-catalog/tests/enforcement.rs` says of itself that it is *"not a test of behaviour but
of the API's shape"*. The type does the work; no test proves the type is the only door.

---

## 12. What is not built, by name

| | State |
|---|---|
| Federated identity tokens, SCRAM | Not built. `FR-SEC-03`, `M14`, gated on transport security |
| Any authentication on the Arrow Flight door | Not built. §3.4 |
| The write refusal on the Arrow Flight door | Not built. §3.5 — this is a code gap, not a design one |
| Auditing of Flight statements | Not built. §3.4 |
| Authorization on `insert`, `update` and `delete` | Rules parse; nothing consults them. §4 |
| Envelope encryption | Built and tested; no path through the front door |
| Per-tenant graph epochs | Built and tested; no path through the front door |
| TLS configurable by environment | Not built. §3.1 |
| Reload of policy or credentials without a restart | Not built. §5.7 |
| Certificate expiry checking | Not built, deliberately. §8 |
| Row- and column-level enforcement against external readers | Not possible by construction. §10 |

The authoritative, dated version of this list is [`STATUS.md`](STATUS.md); the sequenced plan that
closes the items is [`REMEDIATION.md`](REMEDIATION.md).

---

## 13. Reporting

A defect in anything on this page is a defect in the product, not in its documentation. Findings go
to the repository owner. `SNK-S0005` is the error class for *"an invariant this system asserts does
not hold"*, and [`runbooks/snk-s0005.md`](runbooks/snk-s0005.md) says what to do with one.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
