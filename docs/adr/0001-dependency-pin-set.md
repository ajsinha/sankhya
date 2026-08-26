# ADR-0001 — Dependency pin set and the read-path strategy

**Status:** Accepted · **Date:** 2026-08-25 · **Milestone:** M0
**Implements:** `DEC-06` (own the table provider; storage libraries supply metadata only)

## Context

The released `deltalake-core` (0.32.4) and `iceberg` (0.10.1) crates pin `arrow ^58`
and `datafusion ^53.1.0`, two majors behind head. Two Arrow majors cannot coexist in
one process — identically-named types become distinct and incompatible, and the
trait-identity problem is worse: a provider implementing one generation's trait
cannot be registered with the other generation's session at all.

A further hazard: `parquet` pulls native compression libraries. A duplicate `links`
key is a **hard build failure**, not a cost. This had to be settled by an actual
resolution, not by reading manifests.

## Decision

Pin to the head generation and use the storage library for **metadata only**:

```
datafusion 55 · arrow 59 · parquet 59 · object_store 0.13 · delta_kernel 0.27 (arrow-59)
```

`delta_kernel` is the load-bearing choice: it has **no DataFusion dependency at all**
and offers an `arrow-59` feature, so it imposes zero version drag. `deltalake-core`
and `iceberg-datafusion` are absent from the read path entirely.

## Verification (2026-08-25, this machine)

Resolved and compiled a spike containing the full critical family. Evidence retained
as `spikes/pinset-evidence.lock` and `spikes/pinset-evidence.toml`.

| Check | Result |
|---|---|
| Resolution | 329 packages, no conflict |
| `arrow` / `arrow-array` / `arrow-schema` | **59.2.0**, single version |
| `parquet` | **59.2.0**, single version |
| `datafusion` | **55.0.0**, single version |
| `object_store` | **0.13.2**, single version |
| `delta_kernel` | **0.27.1**, single version |
| Critical-family duplicates | **NONE** |
| `links`-key collision | **NONE** — no duplicate `zstd-sys` / `lz4-sys` |
| Cold `cargo check` | **32.9 s** wall, 594 MB peak RSS, 24 cores |
| Compile | **Exit 0** |

Benign duplicates exist (`base64`, `hashbrown`, `syn`, `rand`, `getrandom`,
`itertools`, `foldhash`, `windows-sys`, `r-efi`). None cross a SANKHYA API boundary,
so none can cause type incompatibility. The duplicate-version CI gate therefore
allowlists exactly these and denies the critical family unconditionally.

## Consequences

- The read path is ours: `SankhyaTableProvider` over DataFusion's own Parquet
  machinery. Roughly 3,000–5,500 lines, budgeted in M3.
- We track the query engine at **head**, not one behind. Owning the read path is
  what makes that both possible and beneficial.
- `object_store` is **not** a skew axis: DataFusion 55 itself requires the 0.13 line,
  matching `delta_kernel`. One object store, one credential provider, one cache,
  shared across every layer.
- The upstream storage library has already moved to Arrow 59 / DataFusion 55 on its
  main branch but has not released for months. We accommodate a permanently-lagging
  storage library rather than waiting.

## Re-verification

The gate runs on every pull request. This ADR is re-verified quarterly against the
ecosystem baseline in `REQUIREMENTS.md` §7.
