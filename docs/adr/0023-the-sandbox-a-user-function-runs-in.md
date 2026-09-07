<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>


# ADR-0023 — The sandbox a user's function runs in

**Status:** Accepted · **Date:** 2026-09-03 · **Version:** 0.1.0 · **Milestone:** M18 — the design gate, before any implementation
**Status of the system:** Implementation — M0, M1, M3, M4, M7, M10 and M13 complete; M2 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Builds on:** [ADR-0010](0010-external-aggregations.md), [ADR-0012](0012-open-capabilities.md), [ADR-0022](0022-user-defined-functions.md), [ARCHITECTURE](../ARCHITECTURE.md) §5.7

## Context

[ADR-0022](0022-user-defined-functions.md) decided that a user may write a function in Python and
have it run over Arrow batches, out of process, on either tier. It left one thing open, by name:

> **Sandboxing and resource limits.** A user function is arbitrary code; what it may import, how
> long it may run, and how much memory it may take are a security question with its own answer,
> and `ADR-0010`'s *"inside the trust boundary of the data it sees"* is the start of it rather
> than the whole.

That open question is not a detail to settle during implementation. It decides **whether the
feature can be built at all**, because the alternative to deciding it is shipping a door through
which any principal who can issue DDL runs arbitrary code as the server user, with
policy-filtered rows in hand. `ADR-0010` already stated the requirement — *no network, no
filesystem, no subprocess; a declared, pinned set of importable modules; a wall-clock and memory
bound per call* — and stated no mechanism. A requirement with no mechanism is a wish.

So this ADR names the mechanism, and names what happens where the mechanism is not available.

## Decision 1 — The boundary is the operating system, never the interpreter

There is a tempting cheap answer and it must be refused explicitly, because it is what most
products in this position ship.

The cheap answer is to restrict Python from inside Python: strip `__builtins__`, install an audit
hook ([PEP 578](https://peps.python.org/pep-0578/)), pre-import an allowlist, or run the source
through `RestrictedPython`. It is attractive because it is a hundred lines and needs no
privileges.

**It is not a security boundary, and CPython's own maintainers say so.** The interpreter exposes
its own internals to the code it runs; every published attempt at in-interpreter sandboxing has
been escaped through some path back to `object.__subclasses__`, a C extension, a frame object, or
a codec. Choosing it would not be choosing a weak boundary. It would be choosing a **decoration**
that makes a reviewer believe there is a boundary, which is worse than having none — with no
boundary, nobody grants the capability lightly.

> **The isolation is enforced by the kernel, outside the process running the code.** What Python
> does inside that process is not part of the boundary and is not relied on for any property.

An audit hook may still be installed, and it may still be useful — for a *message* that says
which prohibited thing was attempted, which a bare `EPERM` does not. It is diagnostics, and it is
documented as diagnostics.

## Decision 2 — Four mechanisms, one for each prohibition, each named

`ADR-0010`'s three prohibitions and its two bounds are enforced as follows. Each row names what
would otherwise be possible, so a reviewer can check the mechanism against the threat rather than
against a word.

| Prohibition | Mechanism | What it stops |
|---|---|---|
| **No network** | The worker is placed in a network namespace with no interface but a disconnected loopback | Exfiltration. The reason this matters most: the function is handed rows a policy already filtered *for a principal*, and a socket turns "may read" into "may publish" |
| **No filesystem** | A mount namespace pivoted onto a read-only tree holding the interpreter, its standard library, and the declared modules — and nothing else | Reading the warehouse directly, which would bypass every policy; reading the server's configuration, keys and TLS material; and writing anything at all, which `ADR-0022` Decision 3 forbids because it would be a second writer |
| **No subprocess** | The mount namespace again — inside the jail there is nothing to exec but the interpreter — plus `PR_SET_NO_NEW_PRIVS` and a bound on process count | Escaping the two above by starting something that was not the worker |
| **Not the server's identity** | *Inside* the namespace, an identity map to `65534` with no supplementary groups, so `exec` drops the capability set. **Outside it, the worker is still the server's user** --- see the amendment below | Everything the server user may do by virtue of being the server user |
| **Bounded time** | `RLIMIT_CPU` for the compute, and a **wall-clock deadline enforced by the parent**, which kills the worker | An aggregation that never returns is an outage, not an error. CPU alone does not catch a sleep |
| **Bounded memory** | `RLIMIT_AS`, and a cap on the size of the batch returned | One query taking the machine down. The output cap matters separately: a function returning a gigabyte per batch is a denial of service with no loop in it |

**A `seccomp` filter cannot deliver the third row, and building it is what showed why.** The
mechanisms above are applied between `fork` and `exec`, and the child must `exec` exactly once
to become the worker at all — a filter denying `execve` denies that one, and there is no state
in a `seccomp` program with which to allow the first and refuse the second. So the prohibition
is delivered by the row above it: in a jail holding the interpreter and nothing else, there is
nothing to exec. That is a stronger guarantee than a syscall filter and a simpler one, and the
first draft of this table claimed the filter because the filter is what everybody writes down.

Path filtering deliberately comes from the **mount namespace and not from `seccomp`**. `seccomp`
filters on syscall numbers and scalar arguments; it cannot see the string a path points to, so a
filter written as "deny `openat` outside `/opt/sankhya/py`" cannot be written at all. Attempting
it produces a filter that looks like a rule and enforces nothing — the same failure mode as
Decision 1, one layer down.

## Decision 3 — Where the mechanism does not exist, the feature is off, not degraded

The mechanisms above are Linux's. On a platform that does not offer them — or a Linux with
unprivileged user namespaces disabled, which several distributions ship and several operators
turn off deliberately — there is no boundary to run behind.

> **`CREATE FUNCTION` is then refused by name, saying which mechanism is missing.** It is not run
> unsandboxed, and it is not run behind a weaker substitute.

This is the pattern the rest of the system already uses for a capability it cannot honour: the
parity soak reports the transactional path as **unavailable** rather than passing over three of
four paths; `check-concurrency` skips a measurement *and says it skipped it* rather than
measuring on a busy machine. A refusal that names the missing mechanism is something an operator
can act on. A feature that quietly runs without its boundary is one nobody knows to act on.

The startup probe runs the mechanism, once, against a trivial worker — it does not read a
capability flag and hope. A namespace that cannot be entered is discovered at startup, on the
machine, rather than at the first `CREATE FUNCTION` in production.

## Decision 4 — Creating one is a grant, not a right

Even behind the boundary above, a user function is arbitrary code that sees data. The sandbox
stops it reaching *out*; it does not stop it computing something the operator would not want
computed, and it cannot review the code.

So `CREATE FUNCTION` requires a capability that is **not granted by default**, in exactly the
sense [ADR-0012](0012-open-capabilities.md) uses when it says marking a cube maintained becomes
an authorization question and *"the answer will annoy somebody"*. The commitment here is larger:
a maintained cube commits an operator's storage, and a user function commits their trust.

Two consequences follow, and both are obligations on this system rather than on the operator:

- **The source is stored and shown.** `functions()` carries the body of a user-defined function,
  so the grant is reviewable and so a later reader can see what the column they are looking at
  was computed by. A function whose source cannot be read is a function nobody can audit.
- **The grant is per principal, not per server.** A capability that turns the feature on for
  everybody once is the flag somebody sets during an incident and nobody unsets.

### What is actually built, as of 2026-09-03

**This decision was written and not implemented.** A production-readiness audit found that
nothing granted the capability and nothing checked it: the `CREATE AGGREGATION` arm returned
before the zero-role refusal, so a caller holding **no roles at all** ran arbitrary Python, and
the code then wrote an audit entry asserting `allowed=true` for a decision that was never made.

What exists today is `server.user_functions`, a **server-wide switch that defaults to off**.
That is precisely the flag this decision's second bullet warns against, and it is not claimed to
satisfy it. It is the half of the decision that can be honest without the per-principal
authorization work: a closed door an operator must deliberately open, instead of an open door
nobody was told about. The per-principal grant remains owed, and until it lands, turning the
switch on grants every principal at once.

`crates/sankhya-server/tests/aggregations.rs` holds the closed-door test. Every other test in
that file opts in by name, because a suite where all servers have the capability is a suite that
can never notice it was never checked --- which is how this survived.

## Decision 5 — The module set is declared, pinned, and part of the function's version

`ADR-0010` requires that a function's version participate in the cache and materialisation keys,
because *a changed function is a changed answer*. The same argument reaches one step further and
the ADR did not take it:

> **A changed library is also a changed answer.** A function that calls `numpy.percentile` gives
> a different number when NumPy changes its interpolation default, and nothing about the
> function's own text moved.

So a function declares the modules it imports; the operator's installed versions of those modules
are resolved at declaration; and the **resolved versions are hashed into the function's version**
alongside its source. Upgrading NumPy under a materialised cuboid therefore invalidates it, which
is the correct and slightly expensive answer. Serving it would be the cheap and wrong one.

A module the operator has not installed is refused at declaration, naming it. Not at first call,
inside a scan, as a stack trace in a log.

## Decision 6 — A worker serves one statement, for one principal, and then exits

Reuse is where both of the remaining defects live, and they are different defects:

- **Across principals**, a reused interpreter is an exfiltration path that needs no socket. Module
  state, a cached global, a memoised dictionary — anything the first function left behind, the
  second can read, and the two were filtered for different people.
- **Across statements**, a reused interpreter breaks determinism. `ADR-0010` requires that the
  same input accumulated in one batch and in several agree **by bits**; a function that
  accumulates into a module-level global agrees with itself only until it is called a second
  time.

Within one statement a worker is reused, because that is where the cost argument lives — process
setup per batch would put a fork in the inner loop, and `ADR-0022`'s whole reason for passing
batches rather than rows is that the boundary is the expense.

The determinism check runs **in the sandbox**, under the same limits. A check that ran outside it
would certify behaviour the calls it certifies do not have.

## Decision 7 — A killed worker fails the query; it never returns what it had

When a bound is hit — wall clock, memory, output size — the partial result is discarded and the
statement is refused, naming the function and the bound.

This is worth stating because the alternative is easy to write by accident and is very hard to
notice: an aggregation whose worker was killed at the eighth of ten batches has a state, and that
state is a number. Returning it produces an answer that is wrong by however much the last two
batches held, on a query that reported success. Every failure this repository has found of that
shape — a snapshot that pinned nothing, a compaction that read as a data change in one direction
— cost more to find than it would have cost to refuse.

## Decision 8 — One crate calls the syscalls, and the build says which one

Every mechanism in Decision 2 is a syscall — `unshare`, `setrlimit`, `prctl`, `seccomp` — made
between `fork` and `exec`, which in Rust means `unsafe`. This workspace sets
`unsafe_code = "forbid"` for every crate, and that is not a stylistic preference: it is why a
data warehouse handling other people's numbers has no memory-safety surface to review.

Three ways out, and the choice matters:

| | |
|---|---|
| **Delegate to a helper on the machine** — `bubblewrap`, `systemd-run` | No `unsafe` here, and no boundary either: the security property becomes whatever version of somebody else's binary is installed, discovered at run time, on a machine we did not build. A sandbox is the last thing to hand to *whatever is on the PATH* |
| **Forbid it everywhere and drop the feature** | Coherent, and it makes `ADR-0022` undeliverable. Worth naming as the honest alternative rather than pretending there were only two |
| **One crate, named, with the exception enforced** | Chosen |

> **`sankhya-sandbox` is permitted to write `unsafe`, and `check-unsafety` fails the build for
> any crate that opts out and is not on the list.** The exception is declared in that crate's
> own manifest, which is where a reviewer would look, and it is *checked* rather than trusted —
> because an exception that is only a convention becomes two exceptions and then a policy.

It is the **second** entry on that list, not the first, and writing the check is what showed
that. `sankhya-alloc` has held one since the counting allocator was written, and both its
manifest and its module header said it was *the only* crate in which unsafe code is permitted.
That sentence was true when it was written and had quietly stopped being true the moment a
second was needed. The list now lives in one place, `xtask/src/unsafety.rs`, and the build reads
it — which is the whole difference between a rule and a habit.

Within the crate the permission is narrower still: the manifest says `deny` rather than the
workspace's `forbid`, and exactly one module allows it. `forbid` cannot be relaxed locally,
which is precisely why the workspace uses it.

The crate is deliberately small and depends on nothing but the standard library and `libc`. It
holds no warehouse types, no Arrow, no query engine. Everything above it — the worker protocol,
the batches, the determinism check — is ordinary safe Rust talking to a process this crate
started.

## Consequences

**The feature is Linux-first, and says so.** That is a narrower claim than "runs everywhere" and
it is the true one. `check-package` already asserts a platform baseline; the sandbox probe joins
it.

**There is a real chance an operator cannot use this at all**, because their platform has
unprivileged user namespaces disabled and they are unwilling to run the server with the privilege
that would substitute. That is a legitimate outcome, and the honest way to present it is a
refusal at `CREATE FUNCTION` naming the mechanism — not a footnote in a manual.

**The built-in catalogue is the answer for everybody else**, and this ADR sharpens why it is worth
its size. A built-in is compiled, in process, over borrowed slices, with no boundary to cross and
no trust to extend — two to three orders of magnitude faster than a worker, and available to a
principal who will never be granted `CREATE FUNCTION`.
[ADR-0020](0020-the-built-in-function-catalogue.md)'s breadth is not only a convenience; it is
what keeps the number of people who need this feature small. [historical: an order-of-magnitude comparison from the literature on process spawn versus in-process dispatch; nothing in this repository times a worker]

**Nothing here is a substitute for a container or a VM.** An operator running the server inside
one has a second boundary, and it composes with this one. What it does not do is remove the need
for this one: inside a single container, one principal's function and another's data are on the
same side.

## What this does not decide

- **Languages beyond Python.** The boundary is a process boundary and a syscall filter, so it is
  language-neutral by construction; a Rust or Java worker runs behind the same one. What differs
  per language is the *module set* of Decision 5, which has no general answer.
- **Whether a granted function may be shared between principals.** A function is authored by
  somebody and may be useful to others; who may call one they did not write is an authorization
  question this does not answer.
- **Metering.** A bound stops a runaway; it does not attribute cost. Charging a principal for
  the CPU their functions burn is `M12`'s question, alongside where a worker runs when there is
  more than one node.
- **The wire format's evolution.** Arrow IPC is decided; what happens when a worker built for one
  Arrow version meets a server built for another is a compatibility question, and the answer is
  probably that the worker ships with the server rather than being found on the machine.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>


---

## Amendment, 2026-09-05 --- what Decision 2 promised and what an unprivileged namespace can do

`SEC-09`. The row above read *"a distinct unprivileged uid and gid"*, and the implementation
mapped the namespace's root to the server's own user. Two separate things were wrong with that,
and only one of them was fixable in code.

**The fixable half.** Mapping to `0` made the worker root *inside* its namespace, and `execve` of
a file with no file capabilities only drops the capability set when the effective user id is not
zero. So the interpreter began with every capability the namespace had, for no reason: all of the
setup that needs a capability happens before `exec`. The map is now to `65534`.

**The half that is not.** Outside the namespace the worker remains the server's user, and no
unprivileged mechanism changes that. The kernel permits an unprivileged writer exactly one line in
`uid_map`, and its parent-side id must be the writer's own effective id. A genuinely distinct id
requires `newuidmap` installed setuid and a `/etc/subuid` range allocated to the account the server
runs as --- a deployment decision, made by whoever installs the software, that this process cannot
make for itself and must not pretend to have made.

So Decision 2 is **amended rather than closed**: the row states what is delivered, and the
consequence an operator has to know is that a worker shares the server's identity as far as the
rest of the machine is concerned. What stops that mattering is Decision 2a below.

## Decision 2a --- the worker is inside a PID namespace, not beside one

`SEC-10`. `unshare(CLONE_NEWPID)` places the calling process's *children* in the new namespace and
leaves the caller behind. The caller here was the process that then `exec`ed into the worker, so
the worker was never in the namespace at all: it could see every process on the machine, and
because of the identity above it could signal them. `os.kill(os.getppid(), 9)` stopped the server.

The boundary therefore forks once more after the namespaces are entered. The worker is PID 1 of a
namespace holding nothing else; the process left outside waits for it and exits the way it exited,
so nothing upstream can tell there were ever two.

Two consequences follow, and both are load-bearing rather than incidental:

- The process left outside **closes every descriptor above the standard streams**. `Command::spawn`
  does not return until every copy of its close-on-exec pipe is closed, so one held open blocks the
  caller for the whole run --- which starts the wall-clock deadline after the function has already
  finished.
- The worker is **tethered** with `PR_SET_PDEATHSIG`. PID 1 of a namespace is reaped by nobody, so
  killing the process the caller has a handle on would otherwise leave the function running after
  the query that started it was told it timed out. A worker orphaned in the window between the fork
  and that call still ends at `RLIMIT_CPU`, so the leak is bounded rather than absent.

## Amendment --- Decision 3, and a probe that runs fifteen mechanisms rather than one

`SEC-14`. Decision 3 says the probe *runs the mechanism, once, against a trivial worker --- it does
not read a capability flag and hope*. It forked, called `unshare`, and stopped there. A machine
where `unshare` succeeds and `pivot_root` fails --- one whose `/` cannot be made private, or whose
temporary directory is on a filesystem that cannot host a mount --- passed the probe and failed at
the first `CREATE AGGREGATION` in production, which is exactly the outcome the decision exists to
prevent.

The probe now calls the same function a spawn calls, against an empty readable set, and reports
which of the fifteen steps refused and what the system said about it. There is no second
implementation to drift from the first.
