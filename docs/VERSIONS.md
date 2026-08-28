<!-- GENERATED FILE — DO NOT EDIT.
     Produced by `cargo xtask write-catalogues` from crates/sankhya-version/src/lib.rs.
     `cargo xtask check-catalogues` fails the build if this file and that source disagree. -->

# SANKHYA — Versions and rollback

Four things version independently, and every artefact this system writes says which format it is in.

## The four axes

`FR-OPS-11` requires them managed independently, and *independently* is the load-bearing word. One product version covering all four means every change to any of them is a change to all of them — so an upgrade that only touches the wire protocol reads as a storage-format change and gets the caution one deserves, and, worse, the reverse: a genuine storage break hides inside a release that looked like a wire change.

| Axis | What moves it |
|---|---|
| internal schema | This system's own on-disk artefacts — the table below |
| database major version | PostgreSQL. Moving it needs `pg_upgrade` and both binaries present |
| table format protocol | The Delta reader and writer versions. `FR-OPS-12`: a table whose protocol this build does not fully support is **read-only**, never written |
| wire API | The PostgreSQL wire protocol and Flight SQL |

## On-disk formats

| Format | Where | Writes | Reads from | Rollback |
|---|---|---|---|---|
| backup manifest | `<data-dir>/backup-manifest.json` | 1 | 1 | **safe** — the previous release reads it unchanged |
| restore-drill evidence | `<data-dir>/restore-drills.jsonl` | 1 | 1 | **safe** — the previous release reads it unchanged |
| diagnostic history | `<data-dir>/diagnostic-history.tsv` | 1 | 1 | tolerated — a build with no version header treats the file as format 1, which it is --- the header was added after the format, and its absence means the original |

## What a reader does with an artefact it did not write

| It found | What happens |
|---|---|
| A newer format | **Refused, by name.** Not attempted |
| An older but supported format | Read, and **not written back** |
| Older than the floor | Refused. Migrate it with a release that still understood it |
| The current format | Read and written |

**A parse error and "this is from the future" are different facts, and only one of them says what to do.** An artefact from a newer release read by an older one otherwise fails somewhere in the middle of parsing — an unknown field, a number that will not fit — and the error reads as *corruption*. An operator goes looking for a damaged disk. The answer was "upgrade the binary", and nothing in front of them said so.

So the version sits first in every file, is read before anything else is understood, and a refusal names both versions.

## Rolling back

**Backwards is the direction that decides whether you can roll back.** A new release reading old data is the easy direction and the one everybody tests. Whether the *old* release can read what the new one wrote is the question, and the moment to answer it is not after the upgrade.

The procedure, when every format above says **safe**:

1. **Stop the new binary.** It drains in-flight connections; see the termination grace in `packaging/`.
2. **Prove the backup first.** `sankhya-server drill`. A rollback with an unproven backup is two unknowns at once.
3. **Start the previous binary against the same data directory.** No migration step, because none of these formats moved.
4. **Run `sankhya-server doctor`.** It reads every artefact and reports what it could not — which is how a format problem surfaces as a sentence rather than as a failed query later.

When any format says **ONE-WAY**, steps 3 and 4 do not apply and the only route back is a restore. That is why the column exists: the decision has to be visible *before* the upgrade, not discovered during the rollback.

## What is not tested

**Running the previous binary.** One release exists, so there is no earlier one to run. What is tested is the thing that does not need it: a corpus of artefacts as earlier releases wrote them, checked into the repository and read by every build. A fixture is an old binary's behaviour preserved — and unlike the binary it never stops building and is legible in a diff. The fixtures are hand-written rather than generated, because a generated fixture regenerates when the format changes, agrees with the current code by construction, and proves nothing.
