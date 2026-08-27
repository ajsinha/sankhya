<!-- GENERATED FILE — DO NOT EDIT.
     Produced by `cargo xtask write-catalogues` from xtask/src/package.rs.
     `cargo xtask check-catalogues` fails the build if this file and that source disagree. -->

# SANKHYA — Platforms

Where the server runs, where it does not, and what a build has to satisfy.

**The number of build targets is the number of things that can silently break.** Each one is declared here once and the packaging tooling iterates this table, rather than a script per platform drifting from its siblings until an artifact behaves unlike the rest for a reason nobody can find.

| Platform | Target | Support | Baseline | Published as |
|---|---|---|---|---|
| Linux (x86-64) | `x86_64-unknown-linux-gnu` | **server** | `GLIBC_2.28` | tarball, rpm, deb |
| Linux (ARM64) | `aarch64-unknown-linux-gnu` | **server** | `GLIBC_2.28` | tarball, rpm, deb |
| Linux (x86-64, static) | `x86_64-unknown-linux-musl` | **server** | static (musl) | tarball |
| macOS (Apple silicon) | `aarch64-apple-darwin` | **server** | macOS 12.0 | tarball |
| Windows | `x86_64-pc-windows-msvc` | client only | — | — |

## Linux (x86-64)

The self-contained artifact. `glibc` 2.28 is RHEL 8 and Debian 10 — the oldest an enterprise is plausibly still running. The bundled PostgreSQL is dynamically linked, so this baseline is set by how *it* was built, not by the Rust binary.

## Linux (ARM64)

Same baseline, same reasoning. Graviton and Ampere are ordinary deployment targets now rather than a special case.

## Linux (x86-64, static)

The artifact that *downloads* database binaries rather than bundling them. Statically linked, so it starts on any Linux at all — and it is only achievable because the thing that cannot be static, PostgreSQL, is not in this artifact.

## macOS (Apple silicon)

Development and evaluation. The warehouse layout is portable to a case-insensitive filesystem because every path segment is already case-folded — see `sankhya-schema`'s naming rules — so a warehouse written on Linux opens here.

## Windows

Any PostgreSQL driver connects to a SANKHYA server from Windows today — that is the wire protocol, and it is the thing most Windows users actually need. The *server* is not built for Windows, and the reason is the vendored PostgreSQL build and service integration rather than the storage layer: path segments are already restricted to lower-case ASCII, digits and underscores, and the platform device names (`aux`, `con`, `nul`, `com1`…) are already reserved, so a warehouse is already Windows-path-safe. Run the server under WSL2 or a container until this is built.

---

## How an old baseline is met

A binary built on a current distribution silently acquires that distribution's symbol versions. The symbols are present locally, so it links, runs and tests clean, and the failure appears the first time somebody on an enterprise distribution tries to start it — which is why `cargo xtask check-package` reads what the binary *requires* rather than trusting what the build intended.

Three ways to hit an older one, and they are not equivalent:

| | |
|---|---|
| **`cargo-zigbuild`** | Targets a chosen `glibc` directly — `--target x86_64-unknown-linux-gnu.2.28`. No container, no sysroot to maintain. The simplest answer for the Rust half |
| **A build container or sysroot** | The only answer for the *bundled PostgreSQL*, which is a C build and acquires its baseline the same way. A container is excluded from **running** this system, never from building it |
| **musl, statically linked** | Removes the question entirely, and is only available to the artifact that does not bundle PostgreSQL. A static binary containing a database is not achievable |

**So the baseline of the self-contained artifact is set by PostgreSQL, not by the Rust binary.** That is worth stating plainly, because tuning the Rust build alone and declaring victory is the obvious mistake.

## One artifact or one per distribution

Both, answering different questions. A **tarball built at the oldest baseline** is one file that runs everywhere newer, which is what an air-gapped install needs. **Native packages** integrate with the distribution — the service unit, the user, the upgrade path — at the cost of a build and a test per distribution. The matrix above is what keeps that cost visible.
