# Invariants and the Gate

**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress

> This chapter covers the rules SANKHYA holds about itself and the machinery that
> enforces them. Its central claim is that a rule which lives only in somebody's head has
> a measurable failure rate, and that this repository has the evidence — every invariant
> here was written after the corresponding mistake had already been made. So each rule
> names where it is enforced, `cargo xtask check-all` runs twenty checks that fail the
> build when one is broken, a further check verifies that the document and the tooling
> cannot disagree about what exists, and the rules that are *not* enforced are listed
> separately as intent rather than guarantee.

## 22.1 Why the rules are mechanical

The workspace holds fifty-four crates under `crates/`, three domain packs under `packs/`,
and the enforcement tool itself. Nobody holds that in their head. The consequence is not
that mistakes get made — mistakes get made in a three-crate project too — but that a
particular *class* of mistake becomes invisible: the one where the code is correct, every
test passes, and something structural about the repository has quietly stopped being true.

Four examples, each of which happened here and none of which any test could see:

- A crate registered a whole SQL surface that no server depended on, so every function it
  exposed answered `Invalid function` while its own unit tests passed.
- The Parquet writer's default compression was never enabled, because the workspace pin
  omitted `zstd` and a *dev*-dependency supplied it. The crate's entire test suite passed
  while the library panicked for every real consumer.
- The workspace lint policy — `unwrap`, `expect`, `panic` and unchecked indexing denied in
  library code — was declared in `Cargo.toml` from the start and run by nothing. Six crates
  had accumulated violations, including a wire decoder indexing attacker-supplied bytes.
- `target/` grew to 482 GB across three days of ordinary work and took the disk to 95%,
  which is how a forty-five-minute soak died at t+2833s and wrote a zero-byte report
  explaining why: it ran out of room to say what had gone wrong.

None of those is a bug in a function. Each is a property of the repository, and a property
of the repository is exactly what a test suite is not organised to notice.

> **Key idea**
> A rule stated in a document is a rule with a decay rate. A rule that fails the build is
> a rule. `docs/INVARIANTS.md` is written so that the difference is visible on every line:
> its third column names the check or the test that enforces the rule, and where the answer
> is *nothing yet*, it says so.

## 22.2 The shape of the invariant document

`docs/INVARIANTS.md` is organised as six tables — the shape of the system, storage,
answers, documentation, operability, configuration and evidence — and every row has three
columns: **the rule**, **why it exists**, and **what enforces it**. The middle column is
not decoration. It is almost always a specific past failure, stated in the past tense,
because a rule whose reason has been forgotten is a rule that gets argued away by the next
person who finds it inconvenient.

The last table, §6, is the one that makes the rest believable. It lists what is *not*
enforced:

- The project-root rule — nothing may be created or deleted outside the repository — binds
  the soak and nothing else. Any other tool could still write outside it.
- `sankhya-maintenance` must preserve the layout `sankhya-publish` established. It is an
  allowed writer and nothing checks that its output is still partitioned correctly.
- Compaction targets 256 MB and the soak's harness sets its own thresholds. Nothing
  reconciles the two, so a soak can be healthy under settings production never uses.

> **Pitfall**
> A document that lists twenty enforced rules and silently omits the three unenforced ones
> reads as though everything in it is guaranteed. The omission is the lie, not any
> individual sentence.

## 22.3 The gate

`cargo xtask check-all` runs the checks below in order. Slow is not a reason for a check to
be absent; it is a reason for it to be last — so the test suite runs first (it is the
longest), and the build-tree sweep runs at the end, when the superseded generation of
artefacts exists and is identifiable.

| Check | The defect it exists to catch |
|---|---|
| `check-tests` | The suite not running at all. Thirteen static checks once passed while a maintenance test failed, and the reported test count came from *counting test functions* — a number equally correct whether they pass or not |
| `check-concurrency` | ADR-0013's C1–C3 measurements taken on a machine saturated by the rest of the suite. Run alone, one at a time, as the only cargo process |
| `check-invariants` | The document and the tooling disagreeing about which checks exist — in both directions |
| `check-surfaces` | A crate nothing reaches. A capability nothing reaches is indistinguishable from one that was never built |
| `check-atomic-writes` | A writer making a file visible by writing to the path a reader will open, or claiming a name by first checking it is free |
| `check-lock-order` | Two locks held at once without the pair being declared with its order — the precondition of a deadlock, not the deadlock |
| `check-writers` | A second writer to a warehouse. While the CDC pipeline wrote its own files, its tables had no partition columns and violated `FR-STORE-20` |
| `check-layers` | An upward or cyclic dependency, a core crate depending on a pack, a pack reaching outside its allowance |
| `check-loc` | A file past 1,500 lines. A file nobody will read in one sitting is a file whose invariants nobody knows |
| `check-vocabulary` | A domain noun in a core crate. Risk and AML are *use cases*; a domain word in the engine is the first step to an engine that serves one industry |
| `check-dupes` | Two versions of anything in the Arrow/Parquet/DataFusion/`object_store` family. Two Arrow majors make identically named types incompatible |
| `check-docs` | Broken links, stale version claims, status lines that disagree, a source path named in prose that does not exist |
| `check-features` | A feature our own defaults require that only a dev-dependency supplies |
| `check-lints` | Clippy across every target under the workspace's denied set: `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, indexing, float comparison |
| `check-mutations` | A mutation-catalogue entry that no longer matches its source, and a deliberate defect left applied in the tree by an interrupted audit |
| `check-catalogues` | Metric and error documentation drifting from the declarations that produce it; a declared metric nothing records; a generated file edited by hand |
| `check-logging` | A log line or trace attribute carrying caller data — especially `#[instrument]`, which records every argument by default |
| `check-build-tree` | `target/` taking the machine hostage |
| `check-package` | A release binary requiring a `glibc` symbol version the oldest supported platform does not have; an orchestrator grace period shorter than the server's drain deadline |
| `check-doc-numbers` | A figure quoted in prose that the repository no longer has |

Three things are deliberately outside `check-all`. `write-catalogues` writes rather than
checks, and a check that silently rewrote a document would be making a decision that is not
a check's to make. `check-performance` generates a scale-factor-1 dataset and needs a quiet
machine; it belongs to the performance pipeline. `sweep` deletes build artefacts and is run
explicitly.

`check-concurrency` is the interesting boundary case, because it went the other way. Its
four measurements interfere with the parallel suite badly enough that they are `#[ignore]`d
and run separately — but they stay *inside* `check-all` rather than beside it, on the
principle that **a measurement moved out of the gate is a measurement that stops being
taken**.

## 22.4 The check that checks the checks

`check-invariants` reads `docs/INVARIANTS.md`, extracts every token beginning `check-`, and
compares that set against `KNOWN_CHECKS` — the single list from which the dispatch is also
driven. It fails in both directions:

- **`UNKNOWN CHECK`** — the document names a check `xtask` does not run. A document that can
  name a check nobody runs turns every rule in it into an apparent guarantee.
- **`UNDOCUMENTED`** — a check runs on every build and the document does not say what it
  protects. A rule nobody can find is a rule nobody keeps.

This is not ceremony. When `check-concurrency` was added, `check-invariants` rejected two
drafts before accepting the third: one added a check nobody had documented, and one wrote
prose naming a check that does not exist.

The same principle applies one level down. `xtask` has its own tests, and the rule is that
**a check is tested by a case it must reject** — a test asserting a check passes on clean
input passes just as happily when the check never reports anything.

## 22.5 A check added because something got through

### `check-surfaces`, and the day four SQL surfaces turned out to be unreachable

The narrow version of this check looked for crates that register SQL functions and required
each to be reachable from the server. It was written after four such crates were found
unreachable **in a single day**, each by accident:

| Crate | What was unreachable | How it was found |
|---|---|---|
| `sankhya-maintenance` | A working, tested library nothing in production called, so a running server compacted nothing and retired nothing | A soak dying of a full disk |
| `sankhya-cube-sql` | Depended on by no crate at all; the cube surface existed and could not be reached | Reading a `Cargo.toml` for an unrelated reason |
| `sankhya-olap` | Every vector, matrix and statistics function the guide documents answered `Invalid function` | A guide example failing |
| `sankhya-graph-sql` | The same, for five graph functions the guide names | The same |

That check would have caught all four. It did not catch what came next. About 2,600 lines
fell straight through it, because they were not SQL surfaces: a REST surface, a capture
source, a counting allocator nothing installed, a set of port traits, and an entire
declarative pack tier. They were found by reading manifests by hand.

So the rule was widened from *every SQL surface is reachable from the server* to **every
crate is reachable from something that ships, or listed with a milestone**. Widening it
immediately found Arrow Flight SQL, which `GUIDE.md` §7a documents and which no binary could
reach.

The excuse list is where the design lives. An entry in `UNREACHED` needs a milestone, not a
sentence: *"we will get to it"* is how ten crates came to hold one line of source each while
being named nowhere in the plan. The current entries each name where the work belongs —
`sankhya-tiering` to M9 and its drills, `sankhya-api-rest` to M8 §12.2 with criterion 8,
`sankhya-pack` to M4 §8.6, `sankhya-cdc-pg` to M2's carried remainder, and `sankhya-ports`
to a recorded decision to **delete** it, listed rather than gone only because the removal
needs an owner's hand on it.

> **Key idea**
> A crate is a claim the repository makes about itself. A milestone is what makes the claim
> keepable. The check does not forbid unreachable code; it forbids *undated* unreachable
> code.

### `check-atomic-writes`, and a rename that replaced its destination

The second story is shorter and sharper. The technique for making a file appear atomically —
write to a temporary path, then link or rename it into place — was already implemented
correctly three times in this repository and wrongly four, because a three-line technique
gets retyped rather than reused.

The wrong half cost a commit. `commit` checked that a version was absent and then renamed
onto it; `rename(2)` replaces its destination silently. Two committers both saw the version
free, and the second overwrote the first with no error to either. **Seventeen hundred tests
could not see it, because every one of them had a single writer.**

The fix separates two operations that look identical and are not: `publish` makes a file
visible all at once, and `claim` does that *and fails if the name is taken*, through
`link(2)`, which returns `EEXIST`. The distinction is the whole of the protocol's
concurrency control. `check-atomic-writes` now refuses `std::fs::write`, `File::create` and
their relatives anywhere outside a short allowlist, and each allowlist entry carries its
reason.

One of those reasons is worth reading, because it shows what a mechanical check cannot see.
`write_parquet` creates its file and streams into it, which is safe for a reason the check
has no way to know: no log names the path until the file is closed, so no reader can ask for
it, and a partial file left by a crash is unreferenced and collected by the orphan sweep.
Safety there comes from **ordering**, not from atomicity. It is excused with that reasoning
rather than changed — routing it through `publish` would have meant buffering a whole Parquet
file in memory.

## 22.6 Two checks that exist because a document lied

`check-doc-numbers` and `check-catalogues` are about prose, and both were added after prose
went wrong in a way nobody noticed.

Seven documents claimed a test count that was two hundred short. A figure quoted in prose is
derived data, and deriving it by hand is the rot: every commit that adds a test turns a green
build red in seven documents, and the fix is a careful edit across several spellings of the
same number. Done often enough, the tempting move becomes *not adding the test*. So there are
two halves — `check-doc-numbers` fails a build whose prose has drifted, and `sync-doc-numbers`
rewrites the drift away. The check stays, because running the fixer is a deliberate act and a
build must still fail if somebody skipped it.

`check-catalogues` generates `METRICS.md` and `ERRORS.md` from the declarations and compares.
It found something worse than drift: it had been **failing on `develop`**, because
`PLATFORMS.md` — a generated file carrying a header saying it must not be edited — had an
owner decision written into it by hand. Two failures at once, and the second is the dangerous
one. The gate was red, so nobody could tell what else was red; and the next regeneration would
have deleted an owner decision silently, leaving no trace of which paragraph went missing.

The catalogue check also carries an admission about its own limits, which is the shape every
generated-documentation check should have. Generating documentation from a catalogue proves
the documentation matches the catalogue. It says nothing about whether the catalogue matches
reality — so a metric declared and never recorded would be documented, dashboarded, and
permanently absent. That is checked separately, by requiring every declared metric to appear
somewhere in the source outside the catalogue. It is a grep, and a grep is a blunt instrument;
it is also the difference between a catalogue and a wishlist.

> **Pitfall**
> `check-docs` catches the mechanically checkable half of documentation rot: broken links,
> stale version claims, references to crates and decision records that do not exist. Whether
> the prose still *describes* what the code does is not mechanically checkable and remains a
> review responsibility. A check that implied otherwise would be worse than no check.

## 22.7 What the gate cannot do

Three limits are stated in the tooling itself, because a checker that oversells its reach is
the same failure as a document that oversells its guarantees.

**`check-lock-order` is syntactic and single-function.** It finds a lock acquired while a
guard binding is still in scope, within one function body. It cannot see a lock taken inside
a function called while a guard is held — that needs a call graph, and a lint that is half a
call graph reports confidently about the half it has. What it catches is the shape that
appeared here twice in one day: `QueryLog::record` held the map's read lock across the ring's
lock, under a comment claiming it did not, because in edition 2021 a temporary in an `if let`
scrutinee lives to the end of the block; and `CubeCatalog::resolve` held `cubes` across
`declared`.

**`check-vocabulary` catches leakage, not shape.** A core can be immaculately neutral in its
naming and still be bent toward one industry. Only the reference packs catch that. The word
list is also deliberately narrow: two entries have been *removed* — `claim`, which is the
natural verb for "these two tiers claim the same positions", and `diagnosis`, which is the
natural noun in "that would delay the diagnosis" — because a lint that fires on ordinary
prose gets worked around or switched off, which is worse than a narrower lint that is always
obeyed.

**`check-loc` bounds cognitive load and nothing else.** The 1,500-line ceiling is not
satisfied structurally: a split that widens visibility, or separates an invariant from its
enforcement, is a violation of the rule rather than compliance with it, and must be rejected
in review.

> **Key idea**
> Every check in this chapter names the defect it catches, and several name the defect they
> *cannot* catch. The second list is what makes the first one worth having — a gate whose
> coverage is unstated is a gate whose green is unbounded.

---

**Where to go next.** Chapter 23 covers the other half: the test suite, the mutation audit,
the soak and the adversarial review — and the recurring failure they exist to find, which is
a surface that is built, tested, and unreachable through the front door.
