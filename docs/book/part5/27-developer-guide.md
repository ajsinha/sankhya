# Developer Guide

> This chapter covers how to work on SANKHYA: the repository layout, the layer model the
> fifty-eight workspace members live in, how to build and test, how the gate and the mutation
> audit are run and extended, the coding standards and what each one prevents, and the
> procedures for the three things most likely to go wrong — adding a crate, adding a SQL
> surface, and merging a milestone. Its central claim is that almost every rule here exists
> because the corresponding mistake has already been made in this repository, and that the
> checklist form is what stops it being made twice.

## 27.1 Repository layout

```
sankhya/
  crates/          54 library and binary crates
  packs/            3 domain packs — two reference, one adversarial
  xtask/            the enforcement tooling: `cargo xtask <check>`
  sdk/              client bindings: python/ (M14), sql/, java+rust to follow in M16
  tools/            mutation-audit.py, review-server.sh
  vendor/           PostgreSQL source, checksum-verified, built into .build/pg-install
  packaging/        release artifacts and deployment manifests
  config/           server configuration, including config/feeds/*.yaml
  docs/             the working documents this book is assembled from
    adr/            eighteen architecture decision records
    runbooks/       one per alert that can page
    tutorials/      executed by the test suite
    book/           this book
  spikes/           verification spikes kept for provenance
```

Four directories carry rules a newcomer would not guess.

**`sdk/` is at the root, not under `crates/`.** Only one of the three bindings is a Rust crate,
and putting the other two under a Cargo workspace directory would be a lie about what builds
them. Each subdirectory carries its own quickstart — the root `QUICKSTART.md` starts a server,
`sdk/python/`'s assumes one is running and starts from `pip install`. The duplication is
deliberate: the alternative is a binding whose documentation lives somewhere its user is not.

**`vendor/postgresql` is source, not a binary.** `vendor/postgresql/build.sh` verifies a checksum
and builds into a private prefix in roughly two minutes. Nothing is installed system-wide and no
existing PostgreSQL is touched.

**`docs/tutorials/` is executed.** Every file in it must appear in the test that runs the guide's
SQL, and a second test asserts that — so a tutorial cannot be added and quietly left unverified.

**`target/` is not yours to let grow.** See §27.10.

## 27.2 The layer model

Every crate declares its layer in its manifest:

```toml
[package.metadata.sankhya]
layer = 2
```

`check-layers` enforces four rules: dependencies point **downward only**; same-layer dependencies
are permitted but the graph must stay **acyclic**; **no core crate may depend on a pack**; and
**nothing may depend on tooling**.

| Layer | Crates | What lives here |
|---|---|---|
| **0** | `types`, `error`, `schema`, `version`, `atomicfs`, `leases`, `cdc-model`, `alloc`, `ports` | Vocabulary and primitives with no dependencies. Deterministic summation, the error taxonomy, atomic publication, the reader registry, the wire decoder's model |
| **1** | `plan`, `math`, `metrics`, `stats`, `config`, `governor`, `cube-algo`, `graph-algo`, `testkit`, `tls`, `cdc-apply` | Pure algorithms and policy. `cube-algo` and `graph-algo` have **zero dependencies**, which is what makes their property tests fast enough to run thousands of cases on every build |
| **2** | `table`, `table-delta`, `table-memory`, `readpath`, `catalog`, `session`, `authz`, `audit`, `clone`, `objectstore`, `oltp-pg`, `cdc-pg` | Storage, the read path, and the security choke point |
| **3** | `publish`, `ingest`, `maintenance`, `cube`, `graph`, `olap`, `mv`, `tiering`, `diagnostic`, `backup`, `datagen`, `feed` | Services. Data **in** is `ingest`; data **stored** is `publish`; nothing else writes to a warehouse |
| **4** | `api-pg`, `api-flight`, `api-grpc`, `api-rest`, `cube-sql`, `graph-sql` | Surfaces — the doors and the SQL function registrations |
| **5** | `server`, `cli` | Composition roots. The only place an erased error type is permitted |
| **15** | `ext` | The extension API — the one component carrying a stability commitment |
| **99** | `packs/*` | Domain packs. May depend on **only** `sankhya-ext`, `sankhya-types` and `sankhya-error` |
| **100** | `xtask` | Tooling. Nothing depends on it |

The pack allowance is a feedback mechanism rather than hygiene. When a pack legitimately needs a
fourth crate the build fails, and **that failure is the signal that the extension API has a gap**.
The rule is: widen the API, never the allowance.

> **Key idea**
> Same-layer dependencies were once forbidden, and that was wrong rather than strict — a
> vocabulary crate legitimately builds on another. The check now permits them and detects cycles
> separately, which is defence in depth: cargo rejects a cyclic *normal* dependency before this
> runs, but tolerates one through dev-dependencies.

## 27.3 Building and testing

```bash
git clone https://github.com/ajsinha/sankhya.git && cd sankhya
cargo build --workspace          # several minutes on a first build
vendor/postgresql/build.sh       # ~2 min, idempotent, 35 MB installed
cargo test --workspace           # 2,570 tests, none of which needs a database
```

Nothing is mocked. The Parquet is real Parquet, the Delta logs are read back by an independent
kernel, the TPC-H data is generated rather than fixtured, and the PostgreSQL supervisor's tests
run against the vendored 17.11 — when that build is absent they **skip loudly and by name**
rather than passing quietly.

Five gates run outside or alongside the suite:

```bash
cargo xtask check-all                        # every repository invariant (§27.4)
cargo xtask check-concurrency                # ADR-0013's measurements, alone (also inside check-all)
python3 tools/mutation-audit.py              # 733 deliberate defects, one at a time
cargo xtask check-performance                # the NFR-PERF objectives, as a gate that can fail
SANKHYA_RELEASE=1 cargo xtask check-package  # the release artifact's platform baseline
crates/sankhya-cdc-apply/tests/run_e2e.sh    # capture against a live database
```

`check-performance` is deliberately outside `check-all`: it generates a scale-factor-1 dataset and
needs a machine that is not otherwise busy. `check-package` warns on a development build and fails
under `SANKHYA_RELEASE=1`, because a binary built on a current distribution silently requires
symbol versions the customer's enterprise distribution does not have, and the build machine cannot
tell you so. Today this build needs `GLIBC_2.34` against a declared baseline of `2.28`.

## 27.4 The gate

`cargo xtask check-all` runs twenty checks. Chapter 22 gives each one and the defect it exists to
catch; this section is about running it.

**Order matters and is deliberate.** `check-tests` runs first because it is the longest, and
*slow is not a reason for a check to be absent; it is a reason for it to be last within its
class*. `sweep` runs at the very end, after the workspace has been rebuilt, because that is the
moment the superseded generation of artefacts exists and is identifiable — a sweep before the
build sweeps the wrong thing.

**A skipped measurement is reported.** `check-tests` prints its pass count, then lists every
`SKIPPED` line, then *"green means the rest"*. That exists because skips were once invisible on
both ends: `eprintln!` inside a *passing* test goes into libtest's per-test capture and is printed
only on failure, and the gate printed a bare count.

**Two subcommands write rather than check** and are therefore outside `check-all`:
`write-catalogues` regenerates `METRICS.md`, `ERRORS.md`, `PLATFORMS.md` and `VERSIONS.md` from
their declarations, and `sync-doc-numbers` rewrites stale figures in prose. Running either is a
deliberate act, and the corresponding check stays so that a build still fails if somebody skipped
it.

**When `check-catalogues` fails on a generated document**, the fix is in the generator, not the
document. A hand edit to a generated file is deleted silently by the next regeneration, leaving no
trace of which paragraph went missing — which happened to an owner decision in `PLATFORMS.md`.

## 27.5 The mutation audit

```bash
python3 tools/mutation-audit.py            # the whole catalogue: 733 sequential cargo test runs
python3 tools/mutation-audit.py splice     # only entries whose label matches
```

Run it on a clean tree. It edits source files in place and restores each one afterwards, verifying
every touched file byte-identical at the end. A `finally` does not survive a kill, so the same
restore is a signal handler, and every mutation is also recorded in `tools/.mutation-in-flight`,
which a later run finds and undoes before doing anything else. `tools/.mutation-lock` holds the
owning process id, because two overlapping runs restore each other's originals.

A catalogue entry is a tuple:

```python
("splice: accept an off-by-one gap between tiers",
 "crates/sankhya-plan/src/splice.rs",
 "if tier.coverage.start_exclusive() != position {",
 "if tier.coverage.start_exclusive().get() + 1 < position.get() {",
 "sankhya-plan"),
```

Four rules govern adding one.

1. **The defect must be plausible.** Not an arbitrary operator flip — a mistake a person could
   make in this code. An entry nobody would ever write teaches nothing.
2. **The entry must be able to fail.** One equivalent mutant has been produced here — a change to
   a duplicated guard that left the second copy still refusing — and it is recorded rather than
   quietly deleted, because *an entry that cannot fail teaches you to read `SURVIVED` as noise*.
   Where a guard is deliberately duplicated, the optional sixth element sets how many occurrences
   to replace.
3. **The `find` text must be exact and current.** `cargo xtask check-mutations` asserts it on
   every build — no compilation, milliseconds — because four entries had already drifted through
   ordinary refactoring, and because an audit killed hard enough to defeat the in-flight record
   once left a deliberate defect in a commit.
4. **A `SURVIVOR` is a gap in the tests, not necessarily a bug in the code.** The correct response
   is usually a better test.

## 27.6 Coding standards

### Comments say why, and name the failure mode

Module headers are not summaries. The convention throughout is a `# Why this exists` section that
names the defect the module prevents, in the past tense where it has already happened. From
`sankhya-error`:

> One enum drives six behaviours: retry policy, protocol status, SQL state, log level, metric
> labelling and alerting. Without it each call site decides independently, and the decisions drift
> until an operator cannot tell from a log line whether to page someone.

Two habits follow from that. **Record negative results**: a backoff was tried on the rebase loop,
moved the mean from 5.0 to 4.5 and the p99 from 25 to 35 — it made the tail worse — and the
paragraph saying so is worth more than the code would have been. And **do not let a comment
outlive its truth**: a comment claiming `QueryLog::record` did not hold two locks was false, and a
comment claiming statistics bounds were "close to free" was written before the measurement and was
false when written.

### No `unwrap` in a server

The workspace lint policy is declared once and enforced by `check-lints` across **every** target:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
unreachable_pub = "warn"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
unwrap_used = "deny"   expect_used = "deny"      panic = "deny"
todo = "deny"          unimplemented = "deny"
indexing_slicing = "deny"                        float_cmp = "deny"
```

`unsafe_code` is `forbid` rather than `deny` on purpose: `forbid` cannot be relaxed by an
`#![allow]` inside a crate, so the only way out is for a crate to stop inheriting the workspace
lints — which shows in its own manifest, where a reviewer looks. **Two crates do**, and
`check-unsafety` holds the list:

| Crate | Why |
|---|---|
| `sankhya-alloc` | A `GlobalAlloc` implementation cannot be written in safe Rust, and the crate is short enough to read in a sitting |
| `sankhya-sandbox` | [ADR-0023](../../adr/0023-the-sandbox-a-user-function-runs-in.md) Decision 8 — the namespace, mount and `rlimit` syscalls that isolate a user-supplied function are made between `fork` and `exec` |

The check fails the build both ways: for a crate that opts out and is not on the list, and for a
listed crate that no longer writes any `unsafe` — because a permission that outlives its reason is
how an exception becomes a habit. Writing the check found that both `sankhya-alloc`'s manifest and
its module header still said it was *the only* crate in which unsafe code is permitted, which had
stopped being true.

The denied set is a **safety policy, not a style preference**: a server must not abort on data it
did not choose. This policy was declared from the first commit and enforced by nothing for five
milestones, during which library code accumulated violations in six crates — including a wire
decoder indexing attacker-supplied bytes.

Test targets allow the same lints, stated **file by file rather than globally**, because a test
panicking is how a test fails.

`unsafe` is forbidden workspace-wide. It appears in exactly one crate, `sankhya-alloc`, only to
delegate `GlobalAlloc`, and any further exception needs a written decision and a designated
reviewer.

### Errors are typed, classified, and permanent

`sankhya-error` defines a `Class` that drives six behaviours at once:

| Class | Meaning |
|---|---|
| `User` | The request was wrong. Do not retry, do not page |
| `Retryable { after }` | Transient. Retry after the hint |
| `Conflict` | A concurrent writer won. **Re-plan** and retry — a blind retry will lose again |
| `Resource` | A limit was reached. Shed load; do not retry immediately |
| `Cancelled` | Abandoned deliberately: deadline, disconnect, shutdown |
| `Fatal` | An invariant was violated. Fail fast and page |

Every variant must be classifiable and an exhaustive test asserts it — without that test a newly
added variant silently inherits the fallback, which is how a fatal condition ends up being retried
forever.

Error **codes are permanent**. Removing or renumbering one breaks every runbook, alert rule and
support script referencing it, so the catalogue is snapshot-tested and a change is a visible diff
rather than a customer's discovery. Library crates use typed errors; only the composition root
(`server`, `cli`) may use an erased error type.

A refusal that crosses the wire carries four fields — `code`, `sqlstate`, `remediation`,
`subjects` — and never relies on the sentence. `subjects` is the one that is easy to omit and
expensive to add later: without it, a client wanting to show *"three clones read this table"* must
parse the message, and the message becomes an API nobody meant to publish and nobody may reword.

### Logging carries no caller data

`check-logging` refuses field names that carry what a user supplied — `sql`, `statement`, `query`,
`text`, `row`, `value` and their relatives. `table` and `tenant` are deliberately absent: an
identifier is not data, and forbidding them would make the check useless for what it exists to
catch by making it fire constantly.

The dangerous case is the one nobody writes: `#[instrument]` logs **every argument of the function
it decorates** by default. Put it on `fn query(&self, sql: &str)` and every statement any client
ever sends is in the log, including the predicate values, with nothing at the call site saying so.
Use `skip_all`.

**There is no suppression comment, deliberately.** A prohibition with an escape hatch becomes a
prohibition with escapes in it. Where a field is genuinely needed, the answer is a hash, a shape or
a count — `statement_shape` logs the first two words of a statement rather than the statement.

### Writes are published or claimed, never written

Two operations, and using the first where the second was meant loses the loser's work in silence:

- **`atomic::publish`** makes a file visible all at once.
- **`atomic::claim`** does that *and fails if the name is taken*, through `link(2)`, which returns
  `EEXIST`.

`check-atomic-writes` refuses `std::fs::write`, `File::create` and their relatives outside a short
allowlist, and each allowlist entry carries its reason. If your new code needs an entry, the
reason has to explain why safety comes from somewhere else — as `write_parquet`'s does, where no
log names the path until the file is closed.

### Locks are declared, and never held across I/O

`check-lock-order` requires any place holding two locks at once to be declared with its order. The
`NESTED` list is currently **empty and should stay that way**: an entry is not a failure, but it is
a commitment, and every other path holding both must then take them in the same order.

Three practices, in priority order:

1. **Do not hold a lock across I/O.** Look, release, do the reading, re-acquire to install, and
   resolve a concurrent install by version comparison. Striping a lock held across a disk read only
   reduces how many threads wait; removing the I/O from the critical section changes what they are
   waiting for.
2. **Stripe per table or per entry, never per warehouse.** A `Mutex<HashMap<_, _>>` over every
   table is a GIL with a filesystem accent.
3. **On a hot read path, prefer no lock at all.** `Server::servable` uses `ArcSwap`, so the read
   path *loses* a lock rather than gaining a faster one.

Beware edition 2021's `if let` scoping: a temporary in the scrutinee lives to the end of the whole
block, so `if let Some(x) = self.map.read()...` holds the guard through everything inside. Binding
to a `let` first is not a style preference.

### Files, and what a legal split is

A file fails the build above 1,500 code lines and warns above 800 — the warning exists so a file
does not arrive at 1,499 overnight. The rule carries its own anti-pattern, stated in the tooling:

> A split that requires widening visibility, or that separates an invariant from the code enforcing
> it, is a violation of this rule, not compliance with it, and must be rejected in review.

Natural compliance: move test modules to sibling files (tests are typically 40–60% of a well-tested
file), express large mapping tables as data, and keep generated code in an excluded directory.

### Vocabulary

No core crate may name a domain concept. The list is deliberately narrow — two entries have been
*removed* because they were ordinary English a domain also happens to use, and a lint that fires on
ordinary prose gets switched off. Where a domain sense genuinely needs catching, use a compound
that cannot occur by accident: `claim_id`, not `claim`.

`sank_` is a reserved column-name prefix; a colliding source column is refused at onboarding rather
than shadowed.

## 27.7 How to add a crate

1. **Pick the layer** from §27.2 and put it in `[package.metadata.sankhya]`. If you cannot decide,
   the crate is probably two crates.
2. `[lints] workspace = true`. Without it the crate is outside the safety policy.
3. **Add it to a dependency path that ships**, or add it to `UNREACHED` in
   `xtask/src/surfaces.rs` **with a milestone**. Not a sentence — a milestone. *"We will get to
   it"* is how ten crates came to hold one line of source each while being named nowhere in the
   plan. Every existing entry names where the work belongs, and one names a decision to delete.
4. **Write the module header before the code**, naming what the crate prevents.
5. **Add the crate to `QUICKSTART.md`'s suite table** if it has a suite worth reading, and update
   any prose figure — or run `cargo xtask sync-doc-numbers`.
6. **Add mutation entries** for the guards that are load-bearing, and check each one fails.
7. Run `cargo xtask check-all`. `check-layers`, `check-surfaces`, `check-loc`, `check-vocabulary`
   and `check-lints` all have something to say about a new crate.

## 27.8 How to add a SQL surface without it becoming unreachable

This is the procedure with the worst track record in the repository: **four whole SQL surfaces
turned out to be unreachable from the thing that serves SQL, in one day**, and three more
capabilities followed. Chapter 23 has the list. The checklist exists because the failure is
invisible from inside the crate.

1. **Register the functions** — `register_udf`, `register_udtf`, or `impl TableFunctionImpl`.
2. **Make a shipping binary depend on the crate.** `check-surfaces` finds crates that register SQL
   functions and are not reachable from the server. This is the step that was missed four times.
3. **Document an example in `GUIDE.md` or a tutorial**, and let the guide test execute it. That
   catches the case where the function is reachable but wrong.
4. **If the statement is a new keyword rather than a function, claim it.** The wire layer answers
   catalogue and settings queries itself, *before* the handler, and its recogniser is deliberately
   lenient — it treats `SHOW <anything>` as a session setting. A handler must **claim** a statement
   it defines, and a claimed statement bypasses the shortcut. `SHOW FEEDS` was unit-tested,
   mutation-tested and answered as an empty setting for its entire life before this existed.
5. **Demonstrate it through the front door.** Not through the library the criterion is about —
   through a real client over a real socket. That is what caught `SHOW FEEDS`, and the guide test
   would *not* have, because it asserts an example is not refused and an empty answer is not a
   refusal.
6. **Check the fixture's shape, not just its writer.** A fixture built through the product's own
   writer can still encode a layout no deployment has: thirteen clone tests passed against tables
   at the warehouse root while `CREATE TABLE ... CLONE` had never worked against a real
   `<schema>/<table>` warehouse.

> **Pitfall**
> A surface's own tests call the surface. That is the one place this defect cannot be. Every step
> above moves the caller further from the implementation, because distance is the only variable
> that matters here.

## 27.9 How to add a check to the gate

1. Write it as a module under `xtask/src/`, with a header naming the defect and — this is the part
   people skip — **what it cannot catch**. `check-lock-order` says it is syntactic and
   single-function; `check-vocabulary` says it catches leakage, not shape; `check-docs` says the
   half that is not mechanically checkable remains a review responsibility.
2. Add it to `KNOWN_CHECKS` and to the dispatch. Both read from the same list so they cannot
   disagree.
3. **Document it in `docs/INVARIANTS.md`**, naming the rule it protects. `check-invariants` fails
   the build otherwise, in both directions — an undocumented check and a documented check that does
   not exist are both errors. It rejected two drafts of `check-concurrency` on the way in.
4. **Test it with a case it must reject.** A test asserting a check passes on clean input passes
   just as happily when the check never reports anything.
5. Decide whether it belongs in `check-all`. The default is yes. The bar for staying out is high:
   `check-performance` is out because it needs a quiet machine and minutes;
   **`check-concurrency` stays in despite needing a quiet machine, because a measurement moved out
   of the gate is a measurement that stops being taken.**

## 27.10 Documentation, and the machine

**Documentation is updated in the same commit as the code.** A change that alters behaviour and
not the documentation is incomplete. `check-docs` and `check-doc-numbers` catch the mechanically
checkable half — broken links, stale version claims, status lines that disagree, quoted figures
that have drifted. The other half is review.

Four documents are **generated** and carry a header saying so: `METRICS.md`, `ERRORS.md`,
`PLATFORMS.md`, `VERSIONS.md`. Prose that has to survive belongs in the generator.

**The build tree is not allowed to consume the machine.** Cargo names every artefact by an input
hash and never removes the one a rebuild supersedes; nothing collects them. Three days of ordinary
work grew `target/` to 482 GB and took the disk to 95%, which is how a forty-five-minute soak died
at t+2833s and wrote a zero-byte report explaining why — it ran out of room to say what had gone
wrong. `check-build-tree` says the number out loud on every run and fails when the tree has taken
the machine hostage; `cargo xtask sweep` collects superseded generations, keeping the newest two of
each artefact. Sweep proactively at safe breaks.

**This is a one-machine project.** A workspace build takes minutes and tens of gigabytes, and
several at once take the box down. During a review, builds are serialised through one person and
**no reviewer runs `cargo` at all** — a review that kills the machine it is reviewing has proved
nothing.

## 27.11 The git drill

Branches: work happens on `develop`; `main` holds verified milestones.

The milestone drill, every milestone, in this order:

1. **Verify.** `cargo xtask check-all` green, the mutation audit clean, the milestone's exit
   criteria demonstrated — and demonstrated **through the front door a user has**, not through the
   library the criterion is about.
2. **Walk the exit criteria one at a time** and assess each rather than assuming it. Walking M10's
   list found two criteria that were not met, and both were built rather than reinterpreted.
   Walking M7's found that eight passing criteria had each supplied their own cells.
3. **Update `STATUS.md` in the same commit as the code.** It is the authority; where the plan and
   the roadmap disagree with it, it is right.
4. Merge `develop` → `main`, then merge back to `develop`, so the two never diverge.
5. If a criterion cannot be met, **record it as unmet rather than reinterpreting it** — and if it
   belongs to a milestone that can answer it, move the clause there. M3's cancellation criterion
   moved to M4 because there is no user code until M4; M8's recovery criteria moved whole to M12
   because they need a second machine.

Commit messages are narrative sentences naming what was found, not what was edited — *"A schema is
a name, and half the system had thrown it away"*, *"A clone that read as empty, and an omission in
my own ADR amendment"*. The habit is not decoration: a message that names the defect makes
`git log` a readable record of what went wrong, which is the same material Chapter 23 is built
from.

## 27.12 Picking up work cold

Read in this order:

| Document | What it tells you |
|---|---|
| `docs/STATUS.md` | What actually runs today, and what was found wrong. Where the plan or roadmap disagrees, this is right |
| `docs/INVARIANTS.md` | Every rule and where it is enforced — including §6, the rules that are only intent |
| `xtask/src/surfaces.rs` | The `UNREACHED` list: every crate nothing reaches, and the milestone that owns it. It is the most honest inventory of unfinished work in the repository |
| `docs/IMPLEMENTATION_PLAN.md` | The milestone breakdown, entry and exit criteria |
| `docs/adr/` | Eighteen decisions and, more usefully, the alternatives they rejected |

And one warning, because it applies to `STATUS.md`'s *"what does not exist"* list: that list has
accreted. Several entries were written against M1 and answered since without being removed. Treat
an unmarked entry as unaudited rather than current, and check the code.

---

**A closing rule, which subsumes most of the above.** Every check, standard and procedure in this
chapter was added after something got through the ones that preceded it. When a rule here seems
excessive, the corresponding paragraph in Chapter 23 usually explains what it cost to learn — and
the honest expectation is that this chapter is not finished either.
