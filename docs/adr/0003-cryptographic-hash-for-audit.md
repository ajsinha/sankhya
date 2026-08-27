# ADR-0003 — A cryptographic hash for the audit chain

**Status:** Accepted · **Date:** 2026-08-27 · **Milestone:** M5
**Implements:** `9.5` (audit hash-chained and mirrored to immutable storage)

## Context

The audit log has to be **tamper-evident**: an operator who can write to it must not be
able to remove or alter a record without that being detectable. The mechanism is a hash
chain — each record carries the digest of the one before it, so altering any record
invalidates every digest after it.

That property comes entirely from the hash being **collision- and preimage-resistant**. A
chain built on a fast non-cryptographic hash is not tamper-evident at all: an attacker who
can write the file can compute a colliding record in milliseconds and the chain still
verifies. It would look like a control and be none.

This repository had no hashing dependency. `sankhya-pack` had already met the same problem
for bundle verification and answered it honestly — digest *pinning* rather than signing,
named as such — because a pinned list is a real control that needs no cryptography. Audit
has no equivalent dodge. Either the hash is cryptographic or the chain is decoration.

## Decision

Add **`sha2 0.10`** to the workspace pin, `default-features = false`.

Chosen over the alternatives:

| Option | Why not |
|---|---|
| `sha2 0.11` | A generation ahead of the RustCrypto stack already resolved here. Brings three duplicates — `digest`, `crypto-common`, `block-buffer` — where 0.10 brings one |
| `blake3` | Faster, and a larger surface: SIMD backends, `rayon`, more `unsafe`. This workspace forbids `unsafe` in its own code and has no throughput problem to solve here — an audit record per query is not a hot path |
| `ring` / `openssl` | Native code, a `links` key, and a build dependency. ADR-0001 records what a duplicate `links` key costs |
| A non-cryptographic hash | Not tamper-evident. See above |

`sha2 0.10` pulls `cpufeatures 0.2`, where `chacha20` in the existing tree pulls `0.3`.
That duplicate is **allowlisted deliberately** in `xtask`: `cpufeatures` is build-time CPU
feature detection with no state, no wire format and nothing crossing an API boundary. Two
copies cost a few kilobytes and can differ in no observable way.

## Consequences

- The audit chain is genuinely tamper-evident, and `verify()` detects alteration,
  reordering, insertion and truncation.
- The dependency is available for other integrity needs. **Pack bundle signing is still not
  implemented**: signing needs a signature scheme and a key, not a hash, and that is a
  further decision about key custody rather than about a crate. `sankhya-pack` continues to
  offer digest pinning and to say plainly that it is not a signature.
- One more allowlisted duplicate, recorded above with its reason.

## Revisit if

The RustCrypto v2 generation (`digest 0.11`) becomes the majority in this tree, at which
point `sha2 0.11` becomes the low-duplicate choice and this should flip.
