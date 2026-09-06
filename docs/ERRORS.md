<!-- GENERATED FILE — DO NOT EDIT.
     Produced by `cargo xtask write-catalogues` from crates/sankhya-error/src/lib.rs.
     `cargo xtask check-catalogues` fails the build if this file and that source disagree. -->

# SANKHYA — Error catalogue

Every error code this build defines, with what to do about it. Twelve of them are marked **not produced by this build**, and that marking is checked: `cargo xtask check-catalogues` fails when a code nothing constructs is not declared, and fails again when a declared one starts being produced and the note is left behind. An alert rule written from an undeclared code will fire; one written from a declared code will not, and now says so.

**Codes are permanent.** Removing or renumbering one breaks every runbook, alert rule and support script that references it, so this catalogue only ever grows.

The class is the load-bearing part: one classification drives retry policy, protocol status, SQL state, log level, metric labelling and alerting. Without it each call site decides independently, and the decisions drift until an operator cannot tell from a log line whether to wake somebody.

## The caller's request was wrong

Do not retry unchanged, and do not page. The detail names what to fix.

### `SNK-C0001`

the query is malformed or references something that does not exist

Correct the statement. The detail names the offending element.

### `SNK-C0002`

a source column type has no faithful representation

Exclude the column, or convert it in the source. SANKHYA refuses an approximate mapping because a silently lossy column cannot be reconciled afterwards.

### `SNK-C0003`

an approximate aggregate was used where exactness is required

**Not produced by this build.** nothing carries an exactness requirement for a statement to breach. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

Use the exact equivalent named in the detail, or clear the exactness requirement for this session if approximation is genuinely acceptable.

### `SNK-C0004`

the statement targets a range that has been archived

**Not produced by this build.** there is no archived tier to target: `sankhya-tiering` plans and does not run. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

Archived data is immutable. Record a compensating entry in the live tier, or rehydrate the range read-only for inspection.

### `SNK-C0006`

the statement uses a feature this build does not implement

The detail names the construct. It is refused rather than approximated: a statement that silently means something slightly different from what it says is worse than one that is rejected.

### `SNK-C0007`

the statement failed during execution

The detail names what failed --- usually a cast, a division, or a value outside the range of its type. If the statement should have worked, this is worth reporting with the detail attached.

### `SNK-C0005`

two distinct source identifiers map to the same storage path

**Not produced by this build.** the source identifiers that could collide arrive on the ingest path, and `ING-00` records that there is no change-capture runtime. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

Rename one in the source, declare an explicit mapping, or exclude one. SANKHYA refuses to disambiguate automatically because a generated suffix destroys the naming relationship it exists to preserve.

## A limit was reached

Shed load and apply backpressure. Retrying immediately makes it worse.

### `SNK-R0001`

the query was refused because its estimated cost exceeds available capacity

Retry when load falls, narrow the predicate, or raise the tenant's limit. Refusal is deliberate: admitting it would risk terminating the process.

### `SNK-R0002`

a tenant quota was reached

**Not produced by this build.** tenant quotas are `sankhya-governor`, which is called with a zeroed request against `u64::MAX` ceilings and decides nothing. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

The detail names the quota. Raise it or reduce consumption.

### `SNK-R0003`

the arrival buffer has no room

**Not produced by this build.** the arrival buffer is part of the change-capture runtime (`ING-00`). The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

Capture is ahead of publication. The system is already lengthening its commit interval; if this persists, publication throughput is the bottleneck.

## A concurrent writer won

Re-plan against the new state and retry. A blind retry loses again.

### `SNK-F0001`

a concurrent writer committed first

**Not produced by this build.** commit conflicts do occur, and `sankhya-publish` reports them as its own `CommitError`, which nothing maps onto this code. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

Re-plan against the new snapshot and retry. Maintenance yields to the applier; the applier never yields.

## Transient

Retry after the hint. Persistent failure means the underlying resource is genuinely unavailable.

### `SNK-T0001`

object storage is unreachable or returned a transient failure

*Retry after 250 ms.*

Retried automatically within the request deadline. Persistent failure indicates a storage or credential problem.

### `SNK-T0002`

the transactional store is unreachable

*Retry after 1000 ms.*

**Not produced by this build.** a source that could be unavailable is the change-capture runtime (`ING-00`). The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

Check the database is running and reachable. In managed mode the supervisor restarts it with backoff.

### `SNK-T0003`

the requested freshness could not be met within the deadline

*Retry after 500 ms.*

**Not produced by this build.** staleness against a freshness objective needs the replication a change-capture runtime would provide (`ING-00`). The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

Retry, relax the freshness requirement, or investigate capture lag. Returning stale data silently would be worse than failing.

## Abandoned deliberately

No action. A deadline expired, a client disconnected, or the server is draining.

### `SNK-X0001`

the work was cancelled

**Not produced by this build.** cancellation does occur, as `sankhya_governor::Stopped` and as a statement timeout, and nothing maps either onto this code. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

No action. The deadline expired, the client disconnected, or the server is draining.

## An invariant does not hold

**These page.** Each has a runbook. Fail fast is deliberate: continuing past a broken invariant turns a detectable fault into a silent wrong answer.

### `SNK-S0001`

no tier covers part of the requested range

**Pages.** Runbook: [`snk-s0001`](runbooks/snk-s0001.md)

A correctness event, not a performance one. The query was refused rather than answered partially. Investigate capture continuity and retention immediately.

### `SNK-S0002`

the archival registry disagrees with the live catalogue

**Not produced by this build.** an archive to conflict with is `sankhya-tiering`, which does not run. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

**Pages.** Runbook: [`snk-s0002`](runbooks/snk-s0002.md)

Usually a restore that resurrected purged rows. Queries on the affected table are refused until an operator re-purges or re-adopts the range.

### `SNK-S0003`

archive verification did not match

**Not produced by this build.** backup verification does run, and reports through `sankhya-backup`’s own types rather than raising this code. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

**Pages.** Runbook: [`snk-s0003`](runbooks/snk-s0003.md)

Terminal until a human acts. There is no automatic retry: a mismatch means a defect exists, and retrying would be the wrong response.

### `SNK-S0004`

continuing would threaten the availability of the transactional store

**Not produced by this build.** an endangered source is the change-capture runtime (`ING-00`). The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.

**Pages.** Runbook: [`snk-s0004`](runbooks/snk-s0004.md)

Retained log has reached its limit. The analytical tier is being sacrificed to protect the source. A gap marker is recorded and re-snapshot begins automatically.

### `SNK-S0005`

an internal invariant does not hold

**Pages.** Runbook: [`snk-s0005`](runbooks/snk-s0005.md)

A defect. Capture a diagnostic bundle and report it. The detail names the invariant.

### `SNK-S0006`

configuration is not valid

**Pages.** Runbook: [`snk-s0006`](runbooks/snk-s0006.md)

The detail names the key, the value and its origin. An unknown key is an error rather than a warning, because silently ignored typos are a leading cause of production incidents.

