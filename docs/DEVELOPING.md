# SANKHYA — Developing

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> How to build it, how to add to it, and what the repository will refuse. Written for somebody
> making their first change.
>
> **Every count in this document is derived rather than typed.** The chapter this replaces had
> six of them and every single one was wrong — fifty-eight members where there are 64,
> fifty-four crates where there are 60, eighteen architecture decisions where there
> are 24, sixteen server test binaries where there are 43, "`unsafe` appears in
> exactly one crate" where it is two and the same document said so correctly nine pages later.
> None of them was wrong when written. That is the point: a hand-typed count of a moving set is
> a claim with an expiry date and no alarm.

## 1. Build it

```bash
cargo build --workspace
cargo test --workspace
cargo run -p xtask -- check-all
```

The toolchain is pinned in `rust-toolchain.toml` to **1.97.1** — pinned rather than a floor,
because a lint that fires on one version and not another turns the gate into a property of
whoever ran it. `Cargo.toml`'s `rust-version` states the same number.

`check-all` is the thing to run before pushing. It takes minutes; `docs/TESTING.md` says what
each of its checks refuses and which two checks deliberately run outside it.

**`check-fast` is the thing to run while you work.**

```bash
cargo run -p xtask -- check-fast     # about two seconds
```

Twenty-one of the checks read files and decide; six build something. The cheap ones total
**about two seconds**, and they are where most failures actually are — a stale figure, a file
over the length ceiling, a banned word in a comment, a link to a document that moved.

That split was not free to discover. `check-tests` used to be dispatched **first**, so three
failures whose combined computation is under five seconds were reported twenty-five minutes
into a run — three times in one sitting. Cheapest-first does not make the gate faster; it makes
the loop faster, which is the thing that was slow. The answer now arrives while the person who
caused it is still looking at what they changed.

The linker is `rust-lld`, configured in `.cargo/config.toml`. It ships inside the toolchain
directory, so nothing has to be installed and `rust-toolchain.toml` pins it along with the
compiler — which matters more than the speed does, because a linker installed separately is a
build that behaves differently on the machine that has it.

## 2. The layer graph

A crate may depend on a lower layer and never on a higher one. `check-layers` enforces the
direction; the layer is declared in each crate's own manifest under
`[package.metadata.sankhya]`, so this table is derived from the manifests rather than
maintained here.

| Layer | Crates |
|---|---|
| 0 | `sankhya-accept`, `sankhya-alloc`, `sankhya-atomicfs`, `sankhya-cdc-model`, `sankhya-error`, `sankhya-leases`, `sankhya-ports`, `sankhya-sandbox`, `sankhya-schema`, `sankhya-types`, `sankhya-version` |
| 1 | `sankhya-cdc-apply`, `sankhya-config`, `sankhya-credential`, `sankhya-cube-algo`, `sankhya-governor`, `sankhya-graph-algo`, `sankhya-math`, `sankhya-metrics`, `sankhya-plan`, `sankhya-stats`, `sankhya-testkit`, `sankhya-tls`, `sankhya-udf` |
| 2 | `sankhya-audit`, `sankhya-authz`, `sankhya-catalog`, `sankhya-cdc-pg`, `sankhya-clone`, `sankhya-objectstore`, `sankhya-oltp-pg`, `sankhya-readpath`, `sankhya-session`, `sankhya-snapshot`, `sankhya-table`, `sankhya-table-delta`, `sankhya-table-memory` |
| 3 | `sankhya-backup`, `sankhya-cube`, `sankhya-datagen`, `sankhya-diagnostic`, `sankhya-feed`, `sankhya-functions`, `sankhya-graph`, `sankhya-ingest`, `sankhya-maintenance`, `sankhya-mv`, `sankhya-olap`, `sankhya-publish`, `sankhya-tiering` |
| 4 | `sankhya-api-flight`, `sankhya-api-grpc`, `sankhya-api-pg`, `sankhya-api-rest`, `sankhya-cube-sql`, `sankhya-graph-sql` |
| 5 | `sankhya-cli`, `sankhya-server` |
| 15 | `sankhya-ext` |
| 16 | `sankhya-pack` |

## 3. What the repository will refuse

- **`unsafe`**, everywhere except `sankhya-alloc` and `sankhya-sandbox`. Both are excused by
  name, both are short enough to read in a sitting, and `check-unsafety` fails if a third
  appears — or if an excuse outlives its reason.
- **`unwrap`, `expect`, `panic` and unchecked indexing in library code.** A server must not
  abort on data it did not choose. Test and benchmark targets allow them, stated file by file
  rather than globally, because a test panicking is how a test fails.
- **A source file past 1,500 code lines.** `check-loc`. A file nobody reads in a sitting is a
  file whose review is a formality. It has split `wiring.rs` three times.
- **A domain concept in a core crate.** `check-vocabulary`. A storage engine that knows what a
  trade is has a customer it cannot lose.
- **A second writer to a warehouse.** `check-writers`. `sankhya-publish` and
  `sankhya-maintenance` are the only two.
- **A log statement recording something a caller supplied.** `check-logging`, and it catches
  `#[instrument]` without `skip_all` — which logs every argument of the function it decorates,
  and on a function taking SQL puts every statement any client sends into the log.

## 4. Adding a check

1. Write it in `xtask/src/`, returning `bool`.
2. Dispatch it in `main.rs` as `if run_all || task == "check-yours"`. That arm is what
   `known_checks()` parses, so nothing else needs telling.
3. Give it a row in `docs/INVARIANTS.md` naming the check. `check-invariants` fails if a
   document names a check nobody runs, and `check-docs` fails if `docs/TESTING.md`'s generated
   gate table has not been regenerated.
4. Add a line to `purpose_of()` in `main.rs` saying what it refuses. A check without a stated
   purpose is one nobody can decide to keep.
5. **Test that it rejects something.** Nine of the existing gate modules have no such test, and
   a check that has never been shown to fail is one nobody has shown to work.

## 5. Adding a mutation

`tools/mutation-audit.py` holds a tuple per entry: label, path, the text to find, the text to
replace it with, the crate, and optionally a count and a test target.

Three rules, each learned by breaking one:

- **The mutation must remove the property, not sit beside it.** An entry that added a
  `println!` while leaving the `tracing` call in place survived, because nothing was taken away.
- **The test it names must fail *because of the property*.** An entry that changed a log
  message's wording survived, because the assertion matched a substring of both.
- **A mutation that hangs the build is worse than one that survives.** Give the entry a test
  target rather than letting it run the whole crate, and check the timing.

Run `python3 tools/mutation-audit.py --check` after any refactor: it proves every entry still
names real code, in seconds, and a mutation that no longer applies passes silently.

## 6. Adding a benchmark

Benchmarks are build targets, so one that stops compiling fails `check-all`. They are not run
by it — a timing taken on a machine doing something else describes the machine.

```toml
[[bench]]
name    = "yours"
harness = false
```

Both arms must consume their results through `black_box`. An unused result is a loop the
optimiser may delete, and that is how a scalar reduction came to imply 39 GB/s and a published
figure came to be 24.9× rather than 14.3×.

`[profile.release]` and `[profile.bench]` declare the same optimisation settings on purpose: a
benchmark and the gate that guards it must measure the same build, or a regression appears in
one and not the other.

## 7. Where things are

- `crates/` — 60 crates. `sankhya-server` is the composition root; `sankhya-types`,
  `sankhya-error` and `sankhya-ports` are the bottom.
- `packs/` — reference extension packs.
- `xtask/` — the gates.
- `tools/` — the mutation catalogue, the logo generator, the deck generator.
- `sdk/` — Python and SQL clients, with examples that run as tests.
- `docs/adr/` — 24 decision records. An archive: append, amend, never rewrite.
- `docs/runbooks/` — one per pageable condition. `check-catalogues` requires it.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
