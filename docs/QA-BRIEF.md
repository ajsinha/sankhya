<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — What to know before testing it by hand

**Status:** Implementation — M0, M1, M3, M4, M7, M10 and M13 complete; M2 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Document ID:** SNK-QA-01 · **Version:** 0.1.0

---

> Written for somebody about to test this system by hand, so that the first day is spent on
> defects nobody has found rather than on the ones fourteen reviewers found on 2026-09-07.

Read §2 before touching a client. It is the list of statements this server **accepts and does
nothing with**, and a tester who does not have it will conclude that features work.

---

## 1. What actually runs

One engine of the three, and the README says so. What a client can reach today:

- **A PostgreSQL wire door on 5433** — not 5432. Read-only: `SELECT`, the catalogue, cubes,
  clones, named snapshots, `SHOW` statements.
- **An Arrow Flight SQL door on 5434**, which streams properly and is the only surface that
  can return more than 10,000 rows.
- **A Prometheus scrape on 9464**, unauthenticated by design.

What does **not** run, whatever the documentation elsewhere may suggest:

| | State |
|---|---|
| Writes over SQL | `INSERT`/`UPDATE`/`DELETE`/`COPY`/DDL are refused with `0A000` and a clear sentence. This is correct behaviour, not a defect |
| The graph engine | No running process builds a graph epoch. Every `graph_*` call returns *"no graph named '…' is registered; known graphs are []"*. There is no configuration that changes this |
| Change capture | There is no replication client. `sankhya-cdc-pg` has no dependents |
| The transactional tier | Not wired into the server |
| Cube functions over Flight | Registered on the PostgreSQL path only. `cube_rollup` over Flight fails to resolve |

---

## 2. The trap list — accepted, and silently ignored

**These are the ones that will cost you a day.** Each is a statement the server answers
successfully while doing nothing. None of them errors, so a test that only checks for an
exception will pass.

| Statement | What actually happens |
|---|---|
| `BEGIN` / `COMMIT` | No-ops. There is no transaction object. Two `SELECT`s inside one `BEGIN`/`COMMIT` can be answered at two different table versions — a **demonstrable non-repeatable read** |
| `ROLLBACK` | Refused with `25P01`. This is deliberate, and it interacts badly with the row below |
| The `ReadyForQuery` status byte | Hardcoded to `'I'`. After an error the server reports idle rather than `'E'`, so a client's standard recovery — send `ROLLBACK` — meets the `25P01` refusal and raises a **second** exception from the error-recovery path |
| `SHOW TRANSACTION ISOLATION LEVEL` | Returns an empty string |
| `server_version` | Reports **17.0**, which tells every client library that MVCC, savepoints, `COPY` and typed prepared parameters are available |
| Bound parameters | String-substituted. pgjdbc sends `int4` in **binary** by default, and those four bytes become a quoted text literal — a cast error on a value you never typed, or silently zero rows against a text column |
| A suspended portal | Has no cursor position. `setFetchSize(10)` over 1,000 rows returns rows 1–10 **forever** |
| `Flush` (protocol message `H`) | Not decoded. Kills the connection with `08P01`. psycopg3 pipeline mode hits this |
| `PRIMARY KEY` / `UNIQUE` | Declarable, **never enforced**. Nothing checks for a duplicate |
| `FOREIGN KEY`, `CHECK`, `DEFAULT`, sequences | Do not exist |

**Do not file these individually.** They are known, they are recorded with file and line in
`docs/AUDIT_REPORT.md` and the 2026-09-07 review, and the remediation for them is scheduled.
File a defect if you find one that is *not* on this list.

---

## 3. Known-broken, not yet fixed

Found by review, verified against source, **not** repaired at the time of writing. Testing these
will reproduce them; that is expected.

**Storage and durability**
- A checkpoint blanks `partitionColumns`, `configuration` and every file's `partitionValues`
  (`sankhya-table-delta/src/checkpoint.rs:209,210,231`). After ~10 commits plus a checkpoint,
  Spark or delta-rs reads the table as **unpartitioned** and a partition filter returns zero
  rows with no error.
- `write_checkpoint` hardcodes protocol `(1, 2)` (`sankhya-maintenance/src/driver.rs:484`),
  silently downgrading a table that declares a higher writer version.
- A log that does not start at version 0 replays as an **empty table with `Ok`**
  (`sankhya-table-delta/src/log.rs:601-626`). This also defeats clone pins, so the origin can
  delete files its clone is the only reader of.
- A zero-copy clone contains **no `add` actions**, so every non-SANKHYA reader resolves it as a
  valid, well-formed, **zero-row** table.

**Correctness**
- Cube hierarchies are declared, validated and **ignored**. `LEVEL` reaches the query path;
  `PARENT` and `ROLLUP` do not. There is no dimension-table join — member keys come from the
  fact table.
- A row with a NULL key on **any** dimension is excluded from **every** cell, so a cube's
  breakdown along one dimension can silently omit rows because a different dimension was null.
  The `completeness` column is the only signal.
- No partition pruning exists in the read path. Statistics cover no temporal or decimal type,
  so a date-filtered query reads every file.

**Numerics** (the function catalogue)
- `jarque_bera` uses adjusted Fisher–Pearson moments where the test needs method-of-moments;
  it crosses the 5% line in the wrong direction on small samples.
- Every `*_inv` collapses in the upper tail past about `1 − 1e-9`; there is no inverse-survival
  function for χ² or F.
- `irr` **refuses every cash flow whose IRR is exactly zero** — `[-100, 100]` is refused.
- `gammaln` returns `NaN` for every negative non-integer.
- `uniform_cdf` returns `0.0` (a plausible probability) for a very wide interval.
- QR and SVD accept **square matrices only**, so a design matrix cannot be decomposed.
- Every finance function **refuses a `Decimal` column** — which is what a declared `NUMERIC`
  maps to, and what money is stored in.

**Operability**
- The Kubernetes manifest sets no `SANKHYA_CONFIG` and mounts no configuration; the fallback
  path is the one the PVC overmounts, and a missing file there is silently skipped. The pod runs
  with any-password-accepted and no policy.
- The Flight door writes **no audit record**, records **no metrics**, and takes **no lease**.
- `runbooks/maintenance-stalled.md` says an absent `maintenance:` block disables maintenance.
  It does not — only `interval: 0` does.

---

## 4. Repaired on 2026-09-07, and worth re-testing

If any of these reproduces, it is a regression and worth a defect immediately.

| Fixed | How to see it |
|---|---|
| A materialised cuboid folded under the wrong measure rule | Declare `MEASURE m (SUM ALONG a, MAX ALONG b)`, query `by=a` with materialisation on and off. The two answers must be identical |
| A user aggregation served from a cuboid was fed pre-summed cells | Same shape with `CREATE AGGREGATION`; a non-distributive function must give the base-data answer or refuse |
| A pinned session answered from the present | `SET VERSION OF t = <old>` then a cube query. It must agree with plain SQL in the same session |
| A cube over `schema.table` froze its snapshot at 0 | Query a maintained cube, publish, query again. The number must move |
| A quarter's closing balance was October's | Any `LAST` measure consolidated over months |
| Two spellings of one user in a startup packet | Now refused |
| The audit said `password` for unverified logins | With `require_password` and no credentials, the chain must say `unverified` |
| Every standard error and p-value | `regress_stderr`/`regress_tstat`/`regress_pvalue` |
| `R²` for a constant response | Now **NULL**, on both `vec_regression_r2` and `regress_r2` |
| `SELECT … FOR UPDATE` | Now refused with `0A000`. It used to return rows and take no lock |
| `SET TRANSACTION ISOLATION LEVEL`, `SET SESSION CHARACTERISTICS`, `SET ROLE`, `SET SESSION AUTHORIZATION` | Now refused with `0A000`. Every other `SET` is still a no-op, deliberately: refusing broadly would break the handshake of every driver |

---

## 5. What the gates prove, and what they do not

`cargo run -p xtask -- check-all` runs 27 checks and the full test suite. It is genuinely
load-bearing for the static half. Know its limits before treating a green gate as evidence:

- **The mutation catalogue has never been run by automation** until the nightly added on
  2026-09-07. `check-all` invokes only `--check`, which is string matching.
- **CI runs a weaker gate than its name.** `check-concurrency` returns success on zero
  measurements when `SANKHYA_CI` is set, and ~15 PostgreSQL end-to-end tests "pass without
  running" unless `SANKHYA_REQUIRE_E2E=1`.
- **TPC-H has no correctness oracle.** `every_query_runs_and_returns_rows` asserts only that
  the row count is above zero.
- **76 of 156 catalogued SQL functions are named in no test.** A function bound to the wrong
  kernel would not be caught.
- The Python SDK's own test suite is run by nothing.

---

## 6. Setting up

- Port **5433**, not 5432. `sdk/python/examples/README.md` says 5432 and is wrong.
- `SANKHYA_CONFIG` must be set explicitly. If it is unset the server looks for
  `config/application.yaml` relative to its working directory and **silently continues** if it
  is not there — which is how the k8s pod ends up unauthenticated.
- `SANKHYA_NO_PASSWORD` disables authentication **on its presence**, whatever the value.
  `SANKHYA_NO_PASSWORD=false` opens the server.
- With `require_password: true` and an **empty** credential map, any password is accepted. The
  banner says `PASSWORD UNVERIFIED` in capitals; the audit now says `unverified` too.
- `docs/QUICKSTART.md` §3's transcript names a table `sank.risk` that does not exist. The
  fixture calls it `risk.positions`. Only one of the file's 44 code blocks is gated, so the
  console transcripts can and do rot.
