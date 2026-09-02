# 21. Extensions and packs

> This chapter covers how a domain reaches a general-purpose engine: the published extension API, the
> three packaging tiers a pack may take, and the contract for an aggregation the system did not write.
> Its central claim is a constraint rather than a capability — **never let a fast-moving upstream type
> into a slow-moving contract** — and the constraint is what makes everything else here possible. The
> chapter also holds the sentence that governs third-party code in this system: once outsiders author
> packs, the extension API is a security boundary and must be tested as one. That is the difference
> between a plugin system and a remote code execution feature.

---

## 21.1 The boundary, and the single most important constraint on it

```
   ┌──────────────────────────────────────────────────────────┐
   │  packs/    risk · financial-crime · telemetry · logistics │
   │            (and anything a third party writes)            │
   └───────────────────────────┬──────────────────────────────┘
                               │ may depend ONLY on:
                               ▼
   ┌──────────────────────────────────────────────────────────┐
   │  sankhya-ext   the published extension API                │
   │  — SANKHYA's OWN function traits                          │
   │  — a curated, pinned Arrow subset                         │
   │  — the logical-type registry                              │
   └───────────────────────────┬──────────────────────────────┘
                               ▼
   ┌──────────────────────────────────────────────────────────┐
   │  the core — knows nothing about any domain                │
   └──────────────────────────────────────────────────────────┘
```

The extension API defines **its own** function traits and re-exports only a curated Arrow subset.
Re-exporting the query engine's traits directly would break every pack in existence on every engine
upgrade, several times a year.

> **Key idea** — This is the same principle as the metadata-only storage coupling in Chapter 6,
> *Storage and the open table log*, applied a second time: never let a fast-moving upstream type into
> a slow-moving contract. `sankhya-ext` is the only crate in this workspace carrying a stable-version
> commitment while everything else is pre-1.0, and it is under a thousand lines. Both facts are
> deliberate and the second protects the first.

A pack may depend on `sankhya-ext`, `sankhya-types` and `sankhya-error`, and on nothing else in the
workspace. That is enforced by `check-layers`, which refuses any pack dependency outside those three
and refuses any core dependency on a pack in the other direction. Packs **self-register**: no core
crate contains a dispatch on pack identity, so the rule cannot be quietly circumvented by
"temporarily" adding a branch.

## 21.2 What a pack may contribute

An enumerated set, and nothing outside it:

- table and schema definitions
- logical types
- scalar, aggregate and window functions
- graph algorithms
- view and materialized-view definitions
- rules and detectors
- named parameterized endpoints
- policy vocabulary

The **logical-type registry** resolves an otherwise intractable tension. The core forbids bare
primitives in public signatures, but a pack must be able to define its own types, which the core
cannot name. The resolution is that the core moves Arrow arrays paired with an **opaque logical-type
identifier**, and the pack owns validation, coercion and formatting. The extension surface therefore
never passes bare scalars, and `Value` and `LogicalType` are SANKHYA's own types rather than the
engine's.

## 21.3 Keeping the API from rotting

An extension API rots by accretion rather than by breaking, so the mechanisms are structural rather
than editorial:

Mechanism | What it stops
---|---
**A hard size budget** | Crude, and the only mechanism that reliably survives to year three
**The two-domain rule** | Nothing enters until two packs *from different domains* need it. One pack's need is a pack-local helper
**No escape hatches** | Type-erased downcasting, free-form document values and open-ended string maps are prohibited — these are how interfaces rot without ever changing shape
**A restricted dependency allowance** | When a pack legitimately needs more, the build fails, and that failure *is* the signal that the API has a gap. It is an API design task, never grounds to widen the allowance
**Compiling examples on every public item** | Bloat acquires a visible recurring cost
**Use it or lose it** | Anything the reference packs do not exercise is removed at the next major version
**Mechanical breaking-change detection** | Not merely a reviewed diff

The two-domain rule is the one that does the most work and is the hardest to hold, because the
request always arrives as *"just this one accessor"*.

## 21.4 Three tiers, and the one that was rejected

Tier | Form | Sandboxed | Hot-reload | Build cost
---|---|---|---|---
**Declarative** | Signed bundle: schemas, views, materialized views, SQL functions, rules, policy vocabulary, endpoints. **No code** | It is data | Yes | None
**Sandboxed module** | Compiled to a portable sandboxed target, for logic the declarative form cannot express | Full: fuel metering, memory cap, deadline interruption, no ambient authority | Yes | None to the server
**Compiled** | Built into the binary behind a feature | **None** — pack code is core code | No | The only tier that adds build time

**The declarative tier is expected to express the substantial majority of a real pack**, because most
of what a domain *is* consists of schemas, views, aggregations, rules and thresholds. Building that
tier well is what keeps the other two exceptional rather than routine, and it is what allows a domain
analyst rather than a systems engineer to deliver a pack.

Its expression language has **no loop, no recursion, no call and no I/O**, so a declarative function
*cannot* be the one that hangs a query. That restriction is the point: an expression language with
loops is a programming language, and one loaded from a configuration file is a remote code execution
feature with extra steps.

Reloading a bundle set is **atomic or it does not happen**. A reload parses, verifies and validates
every bundle before replacing any of them, and a single bad file leaves the previous set entirely in
place — because a reload that half-applies means some queries see the new definitions and some the
old, with which one depending on timing. The outcome names the files that were refused, so an
operator fixes them rather than guessing.

**Dynamically-loaded native extensions are rejected**, and the reasons are recorded so the decision is
not relitigated annually: the language has no stable binary interface, so the crate that would be
required is effectively unmaintained; a version mismatch is undefined behaviour rather than an error;
a fault kills the process with no isolation; the entire Arrow type surface would have to be projected
across the boundary; and every extension would need a per-compiler-version build matrix. The only
benefit over the sandboxed tier is a modest constant factor.

### Cancellation, and what it costs

A pack function in a deliberate infinite loop that *ignores* the cancellation flag is stopped, and the
query fails with an error naming the pack rather than hanging. The mechanism is worth stating plainly
because it is not free.

The call runs on its own thread and is **abandoned** when the bound passes. Rust has no safe way to
kill a thread and this repository has no unsafe code, so a genuinely non-terminating function leaks
one thread until the process ends. `Sandbox::abandoned()` counts them, so a pack that does this is
**visible rather than suspected**.

> **Key idea** — The alternatives are worse: hanging the query for ever, or killing a thread
> mid-allocation and corrupting the allocator for everything else. A bounded, observable, attributable
> leak is an operational problem with an obvious fix. The other two are outages.

## 21.5 Proving the core is actually general

The claim that the engine knows nothing about any industry is tested rather than asserted, by four
mechanisms of increasing strength:

1. **A naming lint** rejecting domain vocabulary in core identifiers, filenames and documentation.
2. **A pack-free build** of the full core test suite, preventing a core test from depending on pack
   fixtures.
3. **Two reference packs, deliberately opposite** — `pack-ref-telemetry`, high-volume, narrow,
   time-series-shaped with essentially no graph; and `pack-ref-logistics`, entity-heavy with a physical
   network graph and string-heavy joins. **Neither is financial.** The acceptance test is mechanical:
   the change that adds a reference pack must touch **zero core files**.
4. **An adversarial pack** attempting what packs must not be able to do — read another tenant's data,
   escape its sandbox, register a non-terminating or panicking function, exceed its budget, shadow a
   core name, claim an API version the engine does not offer. Seven attempts, seven named refusals.

> **Key idea** — The lint catches leakage; the reference packs catch shape. A core can be immaculately
> neutral in its naming and still be structurally bent toward one domain — which is exactly what
> happened to the graph model during review, and exactly what no lint would have caught. Both
> mechanisms are needed and only the second is hard.

The adversarial pack matters because **once third parties author packs, the extension API is a
security boundary and must be tested as one.**

### Signing, and what digest pinning actually proves

The requirement asks for pack bundles to be signed and verified. What ships is **digest pinning**: an
operator pins the digests of bundles they have reviewed and anything else is refused — including
everything when nothing is pinned, because a trust policy that defaults to trusting is not a policy.

That is a real control and it is deliberately not called a signature. **A digest proves the bytes are
the bytes you pinned; it proves nothing about who wrote them.** Public-key signing is the better
answer and needs a cryptographic dependency, which this repository adds by decision record rather than
alongside a feature. The verifier is a trait, so adding it later changes no caller.

## 21.6 Aggregations the system did not write

The most-requested extension point is also the most dangerous one, and the reason is the same: **the
aggregation rule is the one thing in a cube that decides whether an answer is correct.**

Chapter 10, *Multidimensional analysis*, establishes what a rule does: it decides which roll-ups are
legal, whether a materialised ancestor may answer a query, and whether a total means anything. `Mean`
exists so that averaging averages can be *refused*; `None` exists for ratios and distinct counts, where
there is no operation over the parts that yields the whole.

A bare Python callable cannot answer the question the model asks. Given `def f(values): …`, the system
cannot know whether `f(f(a) + f(b))` equals `f(a + b)`. It has two choices and both are bad: assume it
composes, and produce plausible wrong numbers from materialised ancestors; or assume it does not, and
give up roll-up, the lattice and materialisation entirely for that measure.

### The contract, which is the literature's

Method | What it means here
---|---
`accumulate(state, batch)` | Fold a **batch** of values into the state
`merge(a, b)` | Combine two partial states. **Its presence is the composability declaration**
`finish(state)` | The state as a number
`state()` | The partial state, serialisable — what a materialised cuboid stores

That shape is not borrowed from a vendor. It is an implementation of the classification in Gray,
Chaudhuri, Bosworth *et al.*, *Data Cube* (ICDE 1996), which sorts aggregates into three kinds:

Gray's term | Definition | This system's rule
---|---|---
**Distributive** | Computable from partitions by applying the function to each and combining | `Sum`, `Min`, `Max`, `First`, `Last`
**Algebraic** | Computable from a **bounded** intermediate of *M* distributive aggregates | `Mean` — sum and count
**Holistic** | No constant bound on the intermediate exists | `None` — ratios, distinct counts, percentiles

Adopting the contract is therefore not only about Python: **it closes a gap in the built-in rules.**
`composes()` is currently true for exactly Gray's distributive set and false for `Mean`, so the model
treats algebraic and holistic the same and sends both to base data. But `Mean` is algebraic — it
composes perfectly well given the intermediate `(sum, count)`, which is precisely what a `state` plus a
`merge` is. With the contract in place the refusal narrows from *"not distributive"* to *"genuinely
holistic"*, which is where it belongs.

Three properties follow, and each closes a specific hole:

- **`merge` earns the lattice.** A declared `merge` means the measure may be rolled up, answered from a
  materialised ancestor, and combined across partitions. **No `merge` means `Rule::None`** — usable,
  computed from base data every time.
- **The state is what materialises, not the number.** A Python function returns a float, and a float
  cannot be rolled up further without the rounding that bit-identity forbids. So an external measure
  materialises its *state*, and `finish` runs once when the answer is read.
- **Determinism is exercised, not trusted.** The same input, accumulated in one batch and in several,
  merged in two groupings, compared by bits. A function that fails is refused at declaration with the
  two answers side by side. This is cheap, it runs once, and it catches the class of defect — set
  iteration order, floating-point accumulation order, a stray seed — that would otherwise appear as two
  reports disagreeing by a penny.

An aggregation also occupies a **rule slot, not a measure**. A measure here declares a rule *per
dimension* — a closing balance is `Last` along time and `Sum` along entity — so there is no single
"does this compose" answer. A function may legitimately merge along one dimension and not along
another, and the model already has somewhere to put that. This is the one place the borrowed contract
does not fit unchanged.

### It runs out of process

Decided 2026-08-31: **out of process, behind Arrow IPC.** The argument is the correctness argument
applied to blast radius. A user's aggregation is the one part of a query this system did not write.
In-process it shares an address space with the audit chain, with every other tenant's data, and with a
runtime whose threads it can stall; a panic is a server, and an infinite loop is an outage. A sidecar
that panics is a sidecar that dies, and the query fails with a typed error naming the aggregation.

Three obligations follow, none optional:

- **A registered aggregation names its sidecar**, and a query planned against it fails closed when that
  sidecar is absent — rather than silently falling back to computing something else.
- **A sidecar that dies mid-query fails the query.** Partial state is not an answer, and the cube
  model's whole point is that a materialised cell must be exactly what a full recomputation would have
  produced.
- **The determinism exercise runs before the aggregation is trusted**, not on first use in anger.

The function also runs **inside the trust boundary of the data it sees**: an aggregate is computed over
the rows a principal may read, so an external function is handed policy-filtered data, and a function
that can open a socket is an exfiltration path with a legitimate-looking name. No network, no
filesystem, no subprocess; a declared, pinned set of importable modules; and a wall-clock and memory
bound per call. The function's **version participates in the cache and materialisation keys** — a
changed function is a changed answer, and serving a cuboid computed by the previous version is the same
defect as serving one from the previous snapshot.

`accumulate` takes a batch rather than a row, because per-row calls across millions of cells spend
their time in the interpreter boundary, and the data is already Arrow on both sides.

**The contract says nothing about where it runs, which is what keeps the decision reversible.** An
embedded interpreter stays available later, justified by a measurement rather than a preference — and
the same contract admits WebAssembly, which would answer the sandboxing question far more convincingly
than a restricted interpreter. Python is the ecosystem people have; WASM is the isolation nobody has to
trust.

## 21.7 What is built, and what is not

Everything in §21.1 to §21.5 is **built and tested**: the extension API with its own traits, the
declarative tier, the two reference packs, the adversarial pack and its seven refusals, cancellation
inside pack code, digest pinning, and the layer checks that make the neutrality claim mechanical.

**The loader is not wired into the server.** Packs load into a registry; nothing in a running process
does that. `sankhya-pack` sits on the workspace's `UNREACHED` list with `M4 §8.6` named against it —
the declarative tier is a *planned* tier that the architecture expects to express the substantial
majority of a real pack, so deleting it would discard a milestone's work, and the loader that reads a
bundle directory into a running server is the piece that was never finished.

The consequence is directly observable. Against a live server:

```console
$ psql … -c "SELECT logistics_check_digit('abc');"
ERROR:  [SNK-C0001] Error during planning: Invalid function 'logistics_check_digit'.
Did you mean 'to_timestamp_seconds'?
```

That function exists, is tested, and is not reachable from any door. A reader who takes this chapter as
a guide to *deploying* a pack will get no further than the registry.

**Out-of-process aggregations are `M14` and are not built.** The contract is decided, the sidecar is
not; there is no `register` call to make and no Arrow IPC channel to make it over. What exists today is
the built-in rule set (Chapter 19, *The SQL surface*, §19.5), and Chapter 19 also records that the
non-`SUM` members of that set do not yet return the value they declare — which is worth reading before
concluding that an external `merge` is the missing piece.

> **Pitfall** — Two things in this chapter are easy to conflate and are a milestone apart. **A pack**
> contributes definitions and functions and is loaded from a bundle; that mechanism is built and
> unwired. **An external aggregation** contributes a rule slot in a cube and runs in a sidecar; that
> contract is decided and unbuilt. Neither is reachable from a running server today, and the reasons
> they are not are different reasons.

Chapter 26, *Roadmap and status*, is authoritative, and the repository's
[`STATUS.md`](../../STATUS.md) is the continuously updated version.
