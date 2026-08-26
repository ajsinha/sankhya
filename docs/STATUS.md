# SANKHYA — Build Status

**Updated:** 2026-08-26 · Tracks what is *actually built* against
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).

This document exists because a plan describes intent and a roadmap describes ambition;
neither tells you what runs today. Where the two disagree, this one is right.

---

## Milestone progress

| Milestone | Planned | State |
|---|---|---|
| **M0** Foundations, spikes, walking skeleton | 10–12 ew | **Complete**, merged to `main` |
| **M1** Zero-configuration sync and read-your-own-writes | 14–18 ew | **Complete** |
| **M2** Ingest correctness and durability | 24–28 ew | In progress — batching invariants, the source-safety ladder, reconciliation, idempotence and crash safety exist; slot lifecycle, backfill and schema evolution do not |
| **M3** Query engine and storage performance | 28–34 ew | A vertical slice only |
| **M4**–**M8** | — | Not started |

---

## What runs today

| Capability | Evidence |
|---|---|
| The pinned dependency set compiles with no critical duplicates | `cargo xtask check-dupes`, [ADR-0001](adr/0001-dependency-pin-set.md) |
| PostgreSQL 17.11 builds from vendored, checksum-verified source | `vendor/postgresql/build.sh` |
| The wire decoder handles a real replication stream | Conformance suite against captured bytes |
| The decoder never panics on arbitrary or corrupted input | Property tests plus single-byte corruption of real messages |
| A transaction is never split across batches | Property test over randomised interleavings |
| Types round-trip exactly or are refused with a reason | 73 real columns across 10 tables |
| Naming mirrors the source; collisions are refused | All 10 tables map with no transformation |
| A query is answered from tiers covering its span exactly once | Property test asserting exact cover |
| Tables onboard from the stream alone | All 10 tables, schema derived not declared |
| Captured data becomes queryable Parquet | Exact decimal sum matches the source |
| Several tables capture independently from one interleaved stream | 4 tables, each reconciling against the source |
| Capture holds up at scale | 1,000,000 rows across all 10 tables at ~285k rows/s, every table reconciling |
| A session sees its own write analytically | Wrote, capture caught up in 6 ms, the query returned the row |
| Capture cannot endanger its own source | Five-rung ladder escalating strictly below the database's own limit, validated against a real slot |
| Captured data provably matches the source | 3,000 rows digested independently on both sides, no discrepancies |
| A restart cannot duplicate data | Replay is filtered per row; every crash point across a constructed stream yields each row exactly once |

---

## What does not exist

Stated plainly, because a status document that omits this is marketing.

- **No server.** The binary is a stub; there is no daemon, no listener, no lifecycle.
- **No streaming transport.** Changes are drained through a SQL function rather than a
  replication connection. The decoder and pipeline are transport-agnostic by design, but
  the transport itself is unwritten. Note that neither mainstream Rust PostgreSQL client
  supports the replication protocol, so this is real work rather than a wiring exercise.
- **No slot lifecycle.** No creation policy and no position advancement. The
  source-safety ladder now exists and is tested against a real slot, but nothing drives
  it on a timer yet — it is a decision function without a caller.
- **No backfill.** Only changes occurring after a slot exists are captured.
- **No arrival buffer.** The tiered read path is planned and property-tested but has
  only one tier to plan over, so read-your-own-writes currently waits for publication
  rather than for an in-memory tier. The waiting *contract* is right; the tier that
  would make the wait shorter does not exist yet.
- **No catalog, no table provider, no compaction, no maintenance.**
- **No graph engine, no API surfaces, no multi-tenancy, no security.**

---

## Known defects found and fixed

Recorded because the interesting information is usually in what went wrong.

| Defect | How it surfaced |
|---|---|
| Rows of the transaction that first introduces a table were silently refused | Only visible with several tables interleaved; a single-table test cannot see it |
| The conformance fixture's central assertion passed vacuously — the "large" value compressed 80× and stayed inline, so it was never withheld | Caught by making the generator fail loudly if the value does not exceed the out-of-line threshold |
| The fixture capture tool injected newline separators into a binary stream | The decoder was right and the capture was wrong; found at the first byte after a transaction boundary |
| The layer rule forbade same-layer dependencies, which was wrong rather than strict | A vocabulary crate legitimately building on another |
| The documentation-rot check found a stale version claim on its first run | Its own first execution |
| **Zone offsets were stripped rather than applied, shifting a whole timestamp column by four hours** | End-to-end reconciliation against the source. Every value stayed internally consistent, so nothing looked wrong until the two sides were compared |
| **Duplicate suppression worked per batch rather than per row, so a batch spanning the restart boundary republished its already-durable half** | Crash-safety tests sweeping every possible interruption point. A resent stream does not rebatch identically, which a single hand-picked crash point would not have revealed |

---

## Measurements

On a 24-core machine with NVMe storage.

| | |
|---|---|
| Cold `cargo check`, full critical dependency family | 32.9 s |
| Vendored PostgreSQL build | ~2 min, 35 MB installed |
| Synthetic generation | ~147 MB/s |
| Bulk load, 10 tables | 99,235,351 rows / 10 GiB in 188.7 s (~526k rows/s) |
| On-disk size after load | 14 GB |

### Capture at scale

One million rows across all ten tables, in five interleaved transactions.

| | |
|---|---|
| Rows written | 1,000,000 in 2.0 s (~500k rows/s) |
| Messages decoded | 1,000,060 |
| **Rows captured through the pipeline** | **1,000,000 in 3.5 s (~285k rows/s)** |
| Retained write-ahead log during the run | 1,053 MB |
| Files published | 10 |
| Bytes published | 6.3 MiB |

**On the compression figure**, because it would otherwise be misleading: 6.7 bytes per
row reflects *this* data, which is deliberately regular — sequential identifiers,
patterned labels, timestamps clustered within a single run. Dictionary encoding,
delta-encoded monotonic columns and zstd all do unusually well on it. Do not read it as
a general ratio; the honest range against a realistic schema is closer to 3×–15× the
source's footprint, driven mostly by cardinality and index count. See
`REQUIREMENTS.md` §7.4 on estimates versus measurements.

Reproduce with:

```bash
SANKHYA_PG_BIN=$PWD/.build/pg-install/bin SANKHYA_E2E_SOCKET=/tmp/sankhya-sock \
  cargo test -p sankhya-ingest --test scale --release -- --ignored --nocapture
```

---

## How to check this document is honest

```bash
cargo xtask check-all          # every repository invariant, including doc links and version claims
cargo test --workspace         # everything that needs no database
crates/sankhya-cdc-apply/tests/run_e2e.sh   # capture against a live database
```

[`QUICKSTART.md`](QUICKSTART.md) walks through building it from nothing.
