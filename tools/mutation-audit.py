#!/usr/bin/env python3
"""Check that the tests guarding an invariant actually fail when it is broken.

Why this exists
---------------

A test written to catch a defect is not evidence that it catches it. This project has
now shipped two tests that were reviewed, passed, and would not have failed on the very
defect they were written for:

  * A property test asserting the arrival tier's coverage started publication at the
    same position as the stream's origin, so the two could never diverge -- and its
    assertion asserted the buggy value.
  * `is_exact_cover`, the oracle every splice test leans on, had no tests of its own.
    It could be changed to tolerate gaps between tiers and the whole suite stayed green,
    which would have made every splice test pass over a planner returning partial
    covers.

Neither was found by reading. Both were found by breaking the code on purpose and
noticing that nothing complained.

So this script keeps a catalogue of specific, meaningful defects -- not arbitrary
operator flips, but the mistakes a person could plausibly make in this code -- applies
each one, runs the tests that claim to cover it, and reports whether anything failed.

A SURVIVOR is a gap in the tests, not necessarily a bug in the code.

Safety
------

Mutations edit source files in place. Each file is read before it is edited and rewritten
from that copy in a `finally`, and every file the run touched is verified byte-identical
at the end.

A `finally` does not run when the process is killed, and a run interrupted mid-mutation
would otherwise leave a deliberate defect in the working tree looking like ordinary
uncommitted work — which is exactly what happened once, and cost a confusing half hour of
tests failing for no visible reason. So the same restore is installed as a signal
handler, and every mutation is also recorded in a sidecar file that a later run finds and
undoes before doing anything else.

Two runs must never overlap, so a lock file holds the owning process id. Overlapping runs
mutate the same files and restore each other's originals, which produces a tree carrying
several deliberate defects at once and no record of where they came from — that also
happened, and was considerably harder to work out than the first case. A lock whose owner
is gone is taken over rather than respected, so a crashed run does not block the next one
forever.

The check is on the files this run mutates, not on the whole tree. An earlier version
refused to run on any uncommitted change, which sounded safer and was worse: it forced a
commit before every audit, so the history filled with placeholder commits and the audit
became something done *after* deciding the work was finished rather than before.

Usage
-----

    python3 tools/mutation-audit.py            # the whole catalogue
    python3 tools/mutation-audit.py splice     # only entries whose label matches
"""

import glob
import io
import os
import signal
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Records the file currently under mutation and its original contents, so an interrupted
# run can be undone by the next one. Inside the repository on purpose: a sidecar in a
# temporary directory is one reboot away from being the thing that made the defect
# permanent.
IN_FLIGHT = os.path.join(ROOT, "tools", ".mutation-in-flight")

# Holds the process id of the run that owns the working tree.
LOCK = os.path.join(ROOT, "tools", ".mutation-lock")

# (label, file, find, replace, crate whose tests should catch it)
#
# An optional sixth element is how many occurrences to replace; it defaults to one.
# Needed where a guard is deliberately duplicated: removing either copy alone leaves the
# other still refusing, so the mutation is *equivalent* -- behaviour is unchanged and no
# test can possibly catch it. An entry that can never fail is worse than no entry,
# because it trains you to read "SURVIVED" as noise.
CATALOGUE = [
    ("splice: accept an off-by-one gap between tiers",
     "crates/sankhya-plan/src/splice.rs",
     "if tier.coverage.start_exclusive() != position {",
     "if tier.coverage.start_exclusive().get() + 1 < position.get() {",
     "sankhya-plan"),

    ("splice: stop requiring the cover to reach the target",
     "crates/sankhya-plan/src/splice.rs",
     "    position == target\n}",
     "    position <= target\n}",
     "sankhya-plan"),

    ("fixed: drop the scale check on addition",
     "crates/sankhya-types/src/fixed.rs",
     "    pub fn add(self, other: Self) -> Result<Self, FixedError> {\n        self.same_scale(other)?;",
     "    pub fn add(self, other: Self) -> Result<Self, FixedError> {",
     "sankhya-types"),

    ("fixed: use saturating instead of checked addition",
     "crates/sankhya-types/src/fixed.rs",
     ".checked_add(other.units)",
     ".checked_add(other.units).or(Some(0))",
     "sankhya-types"),

    ("lsnrange: treat abutting ranges as overlapping",
     "crates/sankhya-types/src/position.rs",
     "pub const fn overlaps(self, other: LsnRange) -> bool {",
     "pub const fn overlaps(self, other: LsnRange) -> bool { if true { return true; }",
     "sankhya-types"),

    ("batcher: publish rows of a transaction that never committed",
     "crates/sankhya-cdc-apply/src/batch.rs",
     "        let Some(mut rows) = self.open.remove(&xid) else {\n            return;\n        };",
     "        let mut rows: Vec<_> = self.open.values().flatten().cloned().collect();\n        self.open.clear();",
     "sankhya-cdc-apply"),

    ("batcher: stamp the commit position as zero instead of the seal position",
     "crates/sankhya-cdc-apply/src/batch.rs",
     "            row.commit_lsn = end_lsn;",
     "            row.commit_lsn = Lsn::ZERO;",
     "sankhya-cdc-apply"),

    ("batcher: swallow an unresolvable row instead of counting it",
     "crates/sankhya-cdc-apply/src/batch.rs",
     "            None => self.unresolvable = self.unresolvable.saturating_add(1),",
     "            None => {}",
     "sankhya-cdc-apply"),

    ("reconcile: combine digests with XOR, so duplicates cancel",
     "crates/sankhya-ingest/src/reconcile.rs",
     "        self.checksum = self.checksum.wrapping_add(digest.get());",
     "        self.checksum ^= digest.get();",
     "sankhya-ingest"),

    ("compaction: narrow the merged file's declared coverage",
     "crates/sankhya-maintenance/src/compaction.rs",
     "    let covers_through = inputs",
     "    #[allow(unused)]\n    let covers_through = Lsn::ZERO;\n    let _unused = inputs",
     "sankhya-maintenance"),

    ("arrival: release a segment publication has not covered",
     "crates/sankhya-table-memory/src/lib.rs",
     "            if front.coverage.end_inclusive() > self.durable_through {",
     "            if false {",
     "sankhya-table-memory"),

    ("arrival: let memory pressure evict the oldest segment",
     "crates/sankhya-table-memory/src/lib.rs",
     "        if self.bytes.saturating_add(bytes) > self.budget.hard_limit {",
     "        if self.bytes.saturating_add(bytes) > self.budget.hard_limit {\n            if let Some(s) = self.segments.pop_front() { self.bytes -= s.bytes; }",
     "sankhya-table-memory"),

    ("arrival: declare coverage from the frontier alone, ignoring what is held",
     "crates/sankhya-table-memory/src/lib.rs",
     "        let from = self.held_from().max(self.durable_through);",
     "        let from = self.durable_through;",
     "sankhya-table-memory"),

    ("scheduler: let the duty cycle bound safety work",
     "crates/sankhya-maintenance/src/schedule.rs",
     "        !self.may_preempt_queries()",
     "        true",
     "sankhya-maintenance"),

    ("scheduler: age a starved job into a higher class",
     "crates/sankhya-maintenance/src/schedule.rs",
     "        a.class\n            .cmp(&b.class)",
     "        b.ticks_deferred\n            .cmp(&a.ticks_deferred)\n            .then_with(|| a.class.cmp(&b.class))",
     "sankhya-maintenance"),

    ("scheduler: start a job that cannot checkpoint and cannot finish",
     "crates/sankhya-maintenance/src/schedule.rs",
     "            if budget == 0 || job.estimated_ticks > budget {",
     "            if job.resumable && (budget == 0 || job.estimated_ticks > budget) {",
     "sankhya-maintenance"),

    ("scheduler: run optional work outside a maintenance window",
     "crates/sankhya-maintenance/src/schedule.rs",
     "        if job.class.windows_only() && !state.in_maintenance_window {",
     "        if false {",
     "sankhya-maintenance"),

    ("scheduler: order a job with no visible consequence first",
     "crates/sankhya-maintenance/src/schedule.rs",
     "                    .unwrap_or(u64::MAX)\n                    .cmp(&b.ticks_to_visible.unwrap_or(u64::MAX))",
     "                    .unwrap_or(0)\n                    .cmp(&b.ticks_to_visible.unwrap_or(0))",
     "sankhya-maintenance"),

    ("driver: treat a degrading partition as merely a performance problem",
     "crates/sankhya-maintenance/src/driver.rs",
     "            CompactionUrgency::Urgent => Some(Class::Availability),",
     "            CompactionUrgency::Urgent => Some(Class::Performance),",
     "sankhya-maintenance"),

    ("driver: leave superseded inputs in the live set",
     "crates/sankhya-maintenance/src/driver.rs",
     "        live.retain(|f| !superseded.contains(&f.name));",
     "",
     "sankhya-maintenance"),

    ("driver: let a merge estimate round down to free",
     "crates/sankhya-maintenance/src/driver.rs",
     "        (bytes / self.bytes_per_tick.max(1)).max(1)",
     "        bytes / self.bytes_per_tick.max(1)",
     "sankhya-maintenance"),

    ("driver: stop a tick at the first partition that fails",
     "crates/sankhya-maintenance/src/driver.rs",
     "            Err(e) => report\n                .failed\n                .push((pending.job.name.clone(), e.to_string())),",
     "            Err(e) => return Err(e),",
     "sankhya-maintenance"),

    ("driver: reuse one output name for every tick",
     "crates/sankhya-maintenance/src/driver.rs",
     'format!("compacted-{sequence:06}-{index:04}.parquet")',
     'format!("compacted-{index:04}.parquet")',
     "sankhya-maintenance"),

    ("log: let an add of an existing path duplicate it",
     "crates/sankhya-table-delta/src/log.rs",
     "            Action::Add(add) => match self.position.get(&add.path).copied() {\n                Some(index) => {\n                    if let Some(slot) = self.files.get_mut(index) {\n                        *slot = Some(add);\n                    }\n                }\n                None => {\n                    self.position.insert(add.path.clone(), self.files.len());\n                    self.files.push(Some(add));\n                }\n            },",
     "                Action::Add(add) => {\n                    self.position.insert(add.path.clone(), self.files.len());\n                    self.files.push(Some(add));\n                }",
     "sankhya-table-delta"),

    ("log: list commits in directory order rather than version order",
     "crates/sankhya-table-delta/src/log.rs",
     "    out.sort_by_key(|(v, _)| *v);",
     "",
     "sankhya-table-delta"),

    # Both checks, not just the first. The first is a fast path; the second closes the
    # race between checking and renaming. Removing only the fast path is an *equivalent
    # mutant* -- behaviour is unchanged, so no test can catch it, and an entry that can
    # never fail is noise that trains you to ignore survivors.
    # The count was `2` and there is one site left: the commit seal replaced the other. Nothing
    # noticed, because replacing fewer occurrences than declared is silent --- which is why the
    # count is now checked rather than trusted.
    ("log: allow a second commit to overwrite an existing version",
     "crates/sankhya-table-delta/src/log.rs",
     "    if path.exists() {",
     "    if false {",
     "sankhya-table-delta"),

    ("log: skip a malformed line instead of reporting it",
     "crates/sankhya-table-delta/src/log.rs",
     "            let action: Action =\n                serde_json::from_str(line).map_err(|e| CommitError::Malformed {\n                    version,\n                    detail: e.to_string(),\n                })?;",
     "            let Ok(action) = serde_json::from_str::<Action>(line) else { continue };",
     "sankhya-table-delta"),

    ("log: omit the required partitionValues field from an add",
     "crates/sankhya-table-delta/src/log.rs",
     '    #[serde(rename = "partitionValues")]\n    pub partition_values: BTreeMap<String, String>,',
     '    #[serde(rename = "partitionValues", skip_serializing)]\n    pub partition_values: BTreeMap<String, String>,',
     "sankhya-table-delta"),

    ("log: read an absent row count as zero",
     "crates/sankhya-table-delta/src/log.rs",
     "        let stats = self.stats.as_ref()?;",
     "        let Some(stats) = self.stats.as_ref() else { return Some(0) };",
     "sankhya-table-delta"),

    ("driver: publish a compaction removal as a data change",
     "crates/sankhya-maintenance/src/driver.rs",
     "            actions.push(Action::Remove(RemoveFile::rewritten(name(input), now)));",
     "            actions.push(Action::Remove(RemoveFile::deleted(name(input), now)));",
     "sankhya-maintenance"),

    ("driver: commit a merge without its row count",
     "crates/sankhya-maintenance/src/driver.rs",
     "            sankhya_table_delta::from_column_stats(outcome.rows, &outcome.column_stats);",
     "            sankhya_table_delta::from_column_stats(0, &outcome.column_stats);",
     "sankhya-maintenance"),

    ("ingest: commit to the log before the file is written",
     "crates/sankhya-ingest/src/pipeline.rs",
     "            state.published_through = plan.covers_through;",
     "",
     "sankhya-ingest"),

    ("ingest: restart the file sequence from zero rather than from the log",
     "crates/sankhya-ingest/src/pipeline.rs",
     "                        state.sequence = state.sequence.max(highest.saturating_add(1));",
     "",
     "sankhya-ingest"),

    ("ingest: resume the sequence from the earliest committed file, not the latest",
     "crates/sankhya-ingest/src/pipeline.rs",
     "                        .max();",
     "                        .min();",
     "sankhya-ingest"),

    ("schema: publish a millisecond timestamp as a microsecond one",
     "crates/sankhya-table-delta/src/schema.rs",
     "        DataType::Timestamp(TimeUnit::Microsecond, _) => \"timestamp\".to_string(),",
     "        DataType::Timestamp(_, _) => \"timestamp\".to_string(),",
     "sankhya-table-delta"),

    ("schema: publish a decimal as a double",
     "crates/sankhya-table-delta/src/schema.rs",
     "            format!(\"decimal({precision},{scale})\")",
     "            let _ = (precision, scale);\n            \"double\".to_string()",
     "sankhya-table-delta"),

    ("provider: report an upper-bound row count as exact",
     "crates/sankhya-readpath/src/provider.rs",
     "        splice,\n        !published_overshoots,",
     "        splice,\n        true,",
     "sankhya-readpath"),

    ("provider: treat a file with no row count as empty",
     "crates/sankhya-readpath/src/provider.rs",
     "                    let rows = file.rows().ok_or_else(|| {",
     "                    let rows = Some(file.rows().unwrap_or(0)).ok_or_else(|| {",
     "sankhya-readpath"),

    ("provider: drop the commit-position column before the target is enforced",
     "crates/sankhya-readpath/src/provider.rs",
     "        let mut indices = requested.clone();\n        let added = !indices.contains(&lsn);",
     "        let mut indices = requested.clone();\n        let added = false;",
     "sankhya-readpath"),

    ("provider: claim filters are evaluated exactly, so the engine may drop them",
     "crates/sankhya-readpath/src/provider.rs",
     "TableProviderFilterPushDown::Inexact; filters.len()",
     "TableProviderFilterPushDown::Exact; filters.len()",
     "sankhya-readpath"),

    ("provider: treat an unreadable log as an empty table",
     "crates/sankhya-readpath/src/provider.rs",
     "                    None => live_files(table_root)?,",
     "                    None => live_files(table_root).unwrap_or_default(),",
     "sankhya-readpath"),

    ("quantile: place an unorderable value instead of refusing",
     "crates/sankhya-math/src/quantile.rs",
     "    if let Some(at) = values.iter().position(|v| v.is_nan()) {\n        return Err(QuantileError::NotOrderable { at });\n    }",
     "",
     "sankhya-math"),

    ("quantile: answer an empty input with zero",
     "crates/sankhya-math/src/quantile.rs",
     "    if values.is_empty() {\n        return Err(QuantileError::Empty);\n    }",
     "    if values.is_empty() {\n        return Ok(0.0);\n    }",
     "sankhya-math"),

    ("quantile: collapse linear interpolation onto the lower observation",
     "crates/sankhya-math/src/quantile.rs",
     "            Ok(lower + (upper - lower) * fraction)",
     "            let _ = upper;\n            Ok(lower)",
     "sankhya-math"),

    ("quantile: sum across the wrong element, so vectors stop lining up by scenario",
     "crates/sankhya-math/src/quantile.rs",
     ".filter_map(|v| v.get(element).copied())",
     ".filter_map(|v| v.get(0).copied())",
     "sankhya-math"),

    # Deliberately absent: "sum in arrival order rather than a canonical one".
    #
    # It was written, it compiled, and it survived — and chasing it produced a better
    # answer than a new test would have. Neumaier compensation alone is order-independent
    # across 3,000 randomised inputs spanning 120 orders of magnitude, so removing the
    # sort changes no observable behaviour that could be tested for. The sort is there to
    # make order-independence a *guarantee* rather than an observation, and a guarantee
    # about inputs nobody can construct is not something a test can distinguish.
    #
    # Recorded here rather than silently dropped, because the reasoning is the useful
    # part and someone will otherwise write the entry again.

    ("exactness: stop descending into subquery plans",
     "crates/sankhya-olap/src/exactness.rs",
     "    let _ = plan.apply_with_subqueries(|node| {",
     "    let _ = plan.apply(|node| {",
     "sankhya-olap"),

    ("exactness: permit approximation by default",
     "crates/sankhya-olap/src/exactness.rs",
     "    #[default]\n    Required,",
     "    Required,\n    #[default]",
     "sankhya-olap"),

    ("exactness: stay silent about approximation in the permissive mode",
     "crates/sankhya-olap/src/exactness.rs",
     "    Ok(Watermark {\n        approximate_functions: functions,\n    })",
     "    Ok(Watermark::default())",
     "sankhya-olap"),

    ("stats: skip a file whose bounds are unknown",
     "crates/sankhya-stats/src/prune.rs",
     "fn compare_min(stats: &ColumnStats, target: &Bound) -> Option<Ordering> {\n    stats.min.as_ref()?.compare(target)",
     "fn compare_min(stats: &ColumnStats, target: &Bound) -> Option<Ordering> {\n    let Some(min) = stats.min.as_ref() else { return Some(Ordering::Greater) };\n    min.compare(target)",
     "sankhya-stats"),

    ("stats: treat an inclusive bound as exclusive when skipping",
     "crates/sankhya-stats/src/prune.rs",
     "        Predicate::LessOrEqual(target) => {\n            matches!(compare_min(stats, target), Some(Ordering::Greater))\n        }",
     "        Predicate::LessOrEqual(target) => matches!(\n            compare_min(stats, target),\n            Some(Ordering::Greater | Ordering::Equal)\n        ),",
     "sankhya-stats"),

    ("stats: inherit one side's bounds when the other is unbounded",
     "crates/sankhya-stats/src/column.rs",
     "            match (mine, theirs) {\n                (Some(a), Some(b)) => Bound::min_of(&a, &b),\n                _ => None,\n            }",
     "            match (mine, theirs) {\n                (Some(a), Some(b)) => Bound::min_of(&a, &b),\n                (Some(a), None) => Some(a),\n                (None, b) => b,\n            }",
     "sankhya-stats"),

    ("stats: record a NaN as a bound",
     "crates/sankhya-stats/src/column.rs",
     "        if matches!(&bound, Bound::Float(f) if f.is_nan()) {\n            return;\n        }",
     "",
     "sankhya-stats"),

    # Both guard sites -- min and max. Replacing one leaves the other refusing, which is
    # an equivalent mutant.
    ("stats: merge incomparable bounds instead of refusing",
     "crates/sankhya-stats/src/column.rs",
     "                return Err(MergeError::IncomparableBounds);",
     "                self.min = None;",
     "sankhya-stats",
     2),

    ("stats: drop the small-cardinality correction",
     "crates/sankhya-stats/src/sketch.rs",
     "        if raw <= 2.5 * m && zeros > 0 {",
     "        if false {",
     "sankhya-stats"),

    # Treats Or exactly as And. It has to replace the And arm rather than adding an Or
    # arm afterwards: the generic binary arm below matches every operator, so anything
    # added after it is unreachable and the mutation is inert. That mistake cost a round
    # of "the test does not cover this" before the mutation was checked by hand.
    # --- audit: a chain that cannot detect tampering is decoration ---

    ("audit: stop checking that each record carries the previous digest",
     "crates/sankhya-audit/src/chain.rs",
     "            if record.previous != expected_previous {\n                return Err(Broken::LinkMismatch { at: position });\n            }",
     "",
     "sankhya-audit"),

    ("audit: stop recomputing the digest, so an altered record verifies",
     "crates/sankhya-audit/src/chain.rs",
     "            if record.compute_digest() != record.digest {\n                return Err(Broken::Altered { at: position });\n            }",
     "",
     "sankhya-audit"),

    ("audit: stop checking the sequence, so a reordered log verifies",
     "crates/sankhya-audit/src/chain.rs",
     "            if record.sequence != position {\n                return Err(Broken::OutOfOrder {\n                    at: position,\n                    claims: record.sequence,\n                });\n            }",
     "",
     "sankhya-audit"),

    ("audit: match history by subject alone, returning another tenant's records",
     "crates/sankhya-audit/src/chain.rs",
     "            .filter(|r| r.tenant == tenant && r.subject == subject)",
     "            .filter(|r| r.subject == subject)",
     "sankhya-audit"),

    ("audit: leave the row filter out of the record",
     "crates/sankhya-audit/src/chain.rs",
     "            row_filter,\n            column_masks: masks",
     "            row_filter: None,\n            column_masks: masks",
     "sankhya-audit"),

    # --- enforcement: the predicate must reach the plan whatever the provider does ---

    ("secured: trust the provider's pushdown instead of enforcing the predicate",
     "crates/sankhya-catalog/src/secured.rs",
     "        if exact {\n            // A limit must still not be pushed past the predicate unless the provider\n            // applies the predicate before counting, which `Exact` is precisely the promise\n            // of. So it may go down.\n            return self.inner.scan(state, projection, &all, limit).await;\n        }",
     "        if true {\n            return self.inner.scan(state, projection, &all, limit).await;\n        }",
     "sankhya-catalog"),

    ("secured: push the limit below the security filter",
     "crates/sankhya-catalog/src/secured.rs",
     "        let scan = self.inner.scan(state, widened.as_ref(), &all, None).await?;",
     "        let scan = self.inner.scan(state, widened.as_ref(), &all, limit).await?;",
     "sankhya-catalog"),

    ("secured: do not widen the projection, so the predicate cannot bind",
     "crates/sankhya-catalog/src/secured.rs",
     "        let widened = widen_projection(projection, &needed, &table_schema);",
     "        let widened = projection.cloned();",
     "sankhya-catalog"),

    ("guard: hand out a guard for a denied decision",
     "crates/sankhya-catalog/src/guard.rs",
     "        let Decision::Allowed {\n            row_filter,\n            column_masks,\n        } = decision\n        else {\n            return None;\n        };",
     "        let (row_filter, column_masks) = match decision {\n            Decision::Allowed { row_filter, column_masks } => (row_filter, column_masks),\n            Decision::Denied { .. } => (&None, &BTreeMap::new()),\n        };",
     "sankhya-catalog"),

    ("guard: take the storage prefix from somewhere other than the tenant",
     "crates/sankhya-catalog/src/guard.rs",
     "        sankhya_authz::principal::storage_prefix(&self.tenant)",
     '        format!("{}/", self.subject)',
     "sankhya-catalog"),

    # --- policy: the component where a surviving mutant is a breach, not a weak test ---

    ("policy: stop comparing the tenant, so a rule reaches across tenants",
     "crates/sankhya-authz/src/policy.rs",
     "                r.tenant == *principal.tenant()\n                    && r.table == *table",
     "                r.table == *table",
     "sankhya-authz"),

    ("policy: let grants outvote an explicit denial",
     "crates/sankhya-authz/src/policy.rs",
     "        if let Some(denial) = applicable.iter().find(|r| r.effect == Effect::Deny) {",
     "        if let Some(denial) = applicable.iter().find(|r| r.effect == Effect::Allow) {",
     "sankhya-authz"),

    ("policy: treat the absence of a grant as permission",
     "crates/sankhya-authz/src/policy.rs",
     "        if grants.is_empty() {\n            return Decision::Denied {\n                reason: DenialReason::NoGrant,\n            };\n        }",
     "        if false {\n            return Decision::Denied {\n                reason: DenialReason::NoGrant,\n            };\n        }",
     "sankhya-authz"),

    ("policy: stop checking the role, so any principal matches any rule",
     "crates/sankhya-authz/src/policy.rs",
     "                    && principal.has_role(&r.role)",
     "",
     "sankhya-authz"),

    ("policy: stop checking the action, so a read grant permits a delete",
     "crates/sankhya-authz/src/policy.rs",
     "                    && r.action == action\n",
     "",
     "sankhya-authz"),

    ("policy: combine row filters with AND, so a second role narrows the first",
     "crates/sankhya-authz/src/policy.rs",
     '                        .join(" OR "),',
     '                        .join(" AND "),',
     "sankhya-authz"),

    ("policy: emit a filter even when an unrestricted grant applies",
     "crates/sankhya-authz/src/policy.rs",
     "        let row_filter = if grants.iter().any(|r| r.row_filter.is_none()) {",
     "        let row_filter = if grants.iter().all(|r| r.row_filter.is_none()) {",
     "sankhya-authz"),

    ("policy: mask a column any grant masks, so a role can take visibility away",
     "crates/sankhya-authz/src/policy.rs",
     "                let masked_by_all = grants.iter().all(|r| r.column_masks.contains_key(column));",
     "                let masked_by_all = grants.iter().any(|r| r.column_masks.contains_key(column));",
     "sankhya-authz"),

    ("policy: list a table the principal cannot actually read",
     "crates/sankhya-authz/src/policy.rs",
     "                r.effect == Effect::Allow\n                    && matches!(\n                        self.decide(principal, &r.table, Action::Read),\n                        Decision::Allowed { .. }\n                    )",
     "                r.effect == Effect::Allow",
     "sankhya-authz"),

    # Deliberately absent: "accept a tenant identifier that can traverse a path".
    # There is nothing to mutate. The identifier is a UUID, so a path separator cannot
    # occur in it — the property is held by construction rather than by a check, and a
    # mutation audit can only remove checks.

    ("principal: distinguish 'no rule' from 'a rule forbids you' in the message",
     "crates/sankhya-authz/src/policy.rs",
     "            Self::NoGrant | Self::ExplicitDeny { .. } => {\n                f.write_str(\"this principal is not permitted to perform this action\")\n            }",
     "            Self::NoGrant => f.write_str(\"no rule grants this\"),\n            Self::ExplicitDeny { .. } => f.write_str(\"a rule forbids this\"),",
     "sankhya-authz"),

    ("predicate: split a disjunction and apply one side",
     "crates/sankhya-readpath/src/predicate.rs",
     "            op: Operator::And,",
     "            op: Operator::And | Operator::Or,",
     "sankhya-readpath"),

    ("predicate: do not flip a reversed comparison",
     "crates/sankhya-readpath/src/predicate.rs",
     "        (other, Expr::Column(c)) => (c, literal(other)?, flip(op)?),",
     "        (other, Expr::Column(c)) => (c, literal(other)?, op),",
     "sankhya-readpath"),

    ("provider: skip a file the catalogue knows nothing about",
     "crates/sankhya-readpath/src/provider.rs",
     "                .is_some_and(|stats| can_skip(stats, predicate))",
     "                .is_none_or(|stats| can_skip(stats, predicate))",
     "sankhya-readpath"),

    ("provider: skip a file when any one predicate is merely unproven",
     "crates/sankhya-readpath/src/provider.rs",
     "        predicates.iter().any(|(column, predicate)| {",
     "        !predicates.is_empty() && predicates.iter().all(|(column, predicate)| {",
     "sankhya-readpath"),

    ("table: narrow a bound instead of widening it",
     "crates/sankhya-table/src/stats.rs",
     "            Some(std::cmp::Ordering::Greater) => Some(bound.clone()),",
     "            Some(std::cmp::Ordering::Less) => Some(bound.clone()),",
     "sankhya-table"),

    ("table: record a NaN as a bound at compaction",
     "crates/sankhya-table/src/stats.rs",
     "    if matches!(&bound, Bound::Float(f) if f.is_nan()) {\n        return;\n    }",
     "",
     "sankhya-table"),

    ("delta-stats: write a bound the protocol cannot carry",
     "crates/sankhya-table-delta/src/stats.rs",
     "        Bound::Float(v) if v.is_finite() => serde_json::Number::from_f64(*v).map(Into::into),\n        Bound::Float(_) => None,",
     "        Bound::Float(v) => serde_json::Number::from_f64(*v)\n            .map(Into::into)\n            .or(Some(serde_json::Value::from(0.0))),",
     "sankhya-table-delta"),

    ("delta-stats: lose the null count on the way back from the log",
     "crates/sankhya-table-delta/src/stats.rs",
     "                nulls: stats.null_count.get(name).copied().unwrap_or(0),",
     "                nulls: 0,",
     "sankhya-table-delta"),

    ("driver: commit a merge without the bounds it computed",
     "crates/sankhya-maintenance/src/driver.rs",
     "        let mut add = AddFile::rewritten(output.clone(), outcome.bytes, now, &statistics);",
     "        let mut add = AddFile::with_rows(output.clone(), outcome.bytes, now, outcome.rows);",
     "sankhya-maintenance"),

    # Removing this alone leaves a file sitting inside a partition directory while telling
    # every reader it belongs to no partition. A kernel reader stops; Spark reads the column
    # as null and prunes the file out of exactly the queries that filter on it, so the answer
    # is short rather than refused --- and gets shorter the better maintenance is working.
    # Removing the filter makes the loader read every file in the feed directory --- so
    # `config/feeds/README.md`, which explains what a declaration is, is parsed as one, and
    # the shipped server complains about it on every startup. A complaint that is always
    # there hides the next one.
    # The kernel oracle's own guard: if the row count after compaction is not asserted, the
    # test degrades to listing paths --- which is what the old oracle did, and why
    # `partitionValues: {}` survived. Paths and counts are unaffected by that defect.
    # The de-tautologised catalogue check reads the engine's registry, and must read all of
    # it. Dropping the table functions hides `cube_rollup`, `cube_slice`, `functions` and every
    # `graph_*` entry --- eleven of the catalogue's most prominent surfaces --- which is the
    # state this check was in when it was first written.
    # There are deliberately **no mutations for the fsync calls**.
    #
    # Three were written --- drop the file sync, drop the directory sync, drop the Parquet
    # sync --- and all three survived, correctly. `fsync` cannot be observed from inside the
    # process that calls it: a test can prove no reader sees a torn write, and nothing short
    # of cutting the power distinguishes bytes that reached the medium from bytes the kernel
    # has merely accepted.
    #
    # `check-durability` asserts the source property instead, which is the same shape
    # `check-atomic-writes` already takes and for the same reason. A mutation nothing can
    # catch is not evidence of coverage; it is a permanent survivor that trains people to
    # ignore the survivor list.

    # --- a name is used once ------------------------------------------------------------------

    # The tick counter starts at zero on every start, so after a restart it reissues a name
    # the log already holds. The planner then selects that file as its own input, the writer
    # truncates it, and the commit adds and removes the same path --- taking the merged
    # partition out of the live set.
    ("maintenance: name compaction output from a counter that restarts at zero",
     "crates/sankhya-maintenance/src/service.rs",
     "            let sequence = next_compaction_sequence(table_root);",
     "            let sequence = tick;",
     "sankhya-maintenance"),

    # Two flushes of one logical name inside one publisher. Without the version in the name
    # the second write truncates the first while the first's `add` is still in the log, so
    # rows that were acknowledged are gone from disk and the live set says they are there.
    ("publish: write every flush under the caller's name",
     "crates/sankhya-publish/src/publish.rs",
     "            let unique = versioned_name(file_name, version);",
     "            let unique = file_name.to_string();",
     "sankhya-publish"),

    # The version alone is not unique across writers: two publishers read the same
    # `next_version`, so both intend the same version and both compute the same name from the
    # same caller-supplied one. Dropping the token puts them back on a collision course, which
    # `create_new` then turns into a spurious failure on a contended table.
    ("publish: name a file from the version alone, which two writers can share",
     "crates/sankhya-publish/src/publish.rs",
     "    let mark = format!(\"v{version:07}-{:x}{token:x}\", std::process::id());",
     "    let _ = token;\n    let mark = format!(\"v{version:07}\");",
     "sankhya-publish"),

    # The safety net under it. `File::create` truncates; a data file whose name already
    # exists belongs to rows some log still refers to.
    ("table: truncate an existing data file instead of refusing the name",
     "crates/sankhya-table/src/write.rs",
     "    let file = std::fs::File::create_new(&path).map_err(|e| {",
     "    let file = std::fs::File::create(&path).map_err(|e| {",
     "sankhya-table"),

    # --- who is allowed to delete a file -----------------------------------------------------

    # A lineage records `sales.orders`; a sweeper has a directory and knows only `orders`. With
    # a bare `==` the comparison is false for every clone in every warehouse, the pin is
    # invisible, and the clone reads short after the grace period with no error anywhere.
    ("clone: compare a qualified pin against a bare directory name",
     "crates/sankhya-clone/src/family.rs",
     "            .filter(|lineage| same_table(&lineage.origin, table))",
     "            .filter(|lineage| lineage.origin == table)",
     "sankhya-clone"),

    # The other direction, and the reason the fix is not "match on the leaf". Two schemas may
    # each hold an `orders`, and a clone of one must not hold the other's files --- that trades
    # a deletion defect for a warehouse that never reclaims.
    ("clone: treat any two tables with the same leaf name as one table",
     "crates/sankhya-clone/src/family.rs",
     "        (Some(_), Some(_)) | (None, None) => false,",
     "        (Some((_, a)), Some((_, b))) => a == b,\n        (None, None) => false,",
     "sankhya-clone"),

    # Retirement unions clone pins and snapshot pins; the orphan sweep read half the union. Not
    # a race: retirement correctly declines a pinned file for ever, which *guarantees* it
    # crosses the sweep's age threshold. Every snapshot older than the threshold lost its files.
    ("maintenance: answer the reclamation question from clone pins only",
     "crates/sankhya-maintenance/src/service.rs",
     '                resolve(*version, "a snapshot", &mut pinned);',
     "                let _ = version;",
     "sankhya-maintenance"),

    # And the other half of the same union.
    ("maintenance: answer the reclamation question from snapshot pins only",
     "crates/sankhya-maintenance/src/service.rs",
     '                    resolve(version, "a clone", &mut pinned);',
     "                    let _ = version;",
     "sankhya-maintenance"),

    # A pinned version that cannot be read used to be skipped, and skipping was justified by
    # the orphan sweep's age threshold --- which retirement does not have. Retirement saw no
    # paths, so it saw no reason to keep the file.
    ("maintenance: skip a pinned version that cannot be read",
     "crates/sankhya-maintenance/src/service.rs",
     "                Err(error) => holes.push(format!(",
     "                Err(error) => drop(format!(",
     "sankhya-maintenance"),

    # A pin that could not be read contributes nothing, and nothing is what a table with no
    # snapshots also contributes. Without this gate the sweeper reclaims exactly the files whose
    # protection it failed to read.
    ("maintenance: reclaim while the pin set is unknown",
     "crates/sankhya-maintenance/src/service.rs",
     "        if !self.blind.is_empty() || !holes.is_empty() {",
     "        if false {",
     "sankhya-maintenance"),

    # The backstop, written as it was: an `and`. One leaked lease then holds both copies of
    # every compacted partition on disk for ever and one entry per merge in memory for ever.
    ("maintenance: require the leases to drain before the leak backstop can fire",
     "crates/sankhya-maintenance/src/service.rs",
     "            if (old_enough && unreachable) || presumed_leaked {",
     "            if old_enough && unreachable {",
     "sankhya-maintenance"),

    # And the direction that costs rows rather than disk: a backstop that ignores the grace
    # period fires on a reader that is still inside the warehouse.
    ("maintenance: presume a leak the moment a lease is seen",
     "crates/sankhya-maintenance/src/service.rs",
     "            let presumed_leaked = self.policy.retention.leak_ticks > 0\n                && age >= self.policy.retention.leak_ticks",
     "            let presumed_leaked = self.policy.retention.leak_ticks > 0\n                && age >= 1",
     "sankhya-maintenance"),

    # The report a publisher hands back must name the file it wrote. Reporting the caller's
    # name while the write, the add action and the error path all use the versioned one hands
    # back a path that does not exist --- harmless to a caller that counts, and a missing file
    # to any caller that opens it. Compaction is such a caller.
    ("publish: report the name the caller asked for rather than the one on disk",
     "crates/sankhya-publish/src/publish.rs",
     '                // it builds its merge inputs from these names, and the merge failed.\n                file: format!("{directory}/{unique}"),',
     '                // it builds its merge inputs from these names, and the merge failed.\n                file: format!("{directory}/{file_name}"),',
     "sankhya-publish"),

    # --- one server per warehouse -------------------------------------------------------------

    # `create` replaces, so the second server writes its own name over the first's and both run.
    # Each then retires files against a lease registry that cannot see the other's readers.
    ("atomicfs: let a second server take a warehouse lock that is already held",
     "crates/sankhya-atomicfs/src/exclusive.rs",
     "        match std::fs::File::create_new(file) {",
     "        match std::fs::File::create(file) {",
     "sankhya-atomicfs"),

    # The lock type working and the server never calling it look identical from inside the type.
    ("server: start without taking the warehouse lock",
     "crates/sankhya-server/src/main.rs",
     "    let held = match sankhya_atomicfs::WarehouseLock::take(&lock_file) {\n        Ok(lock) => lock,\n        Err(why) => {\n            eprintln!(\"sankhya: {why}\");\n            std::process::exit(3);\n        }\n    };",
     "    let held = ();",
     "sankhya-server", 1, "one_server"),

    # The lock guards the **warehouse**, and lived in the data directory --- which is a
    # per-process setting, so two servers differing only in `SANKHYA_DATA_DIR` both started
    # and permanently corrupted the audit chain between them, silently.
    ("atomicfs: key the warehouse lock on something a second server can miss",
     "crates/sankhya-atomicfs/src/exclusive.rs",
     "        warehouse.join(\"_locks\").join(\"server.lock\")",
     "        std::path::PathBuf::from(\"/tmp\").join(\"server.lock\")",
     "sankhya-server", 1, "one_server"),

    # A pid is reused. Checking only that *something* has that pid calls a crashed server live,
    # so the lock is never broken and every restart is refused until somebody deletes the file
    # by hand --- which teaches an operator to delete it on sight, which is no lock at all.
    ("atomicfs: decide a lock holder is alive from the pid alone",
     "crates/sankhya-atomicfs/src/exclusive.rs",
     "    start_time(holder.pid).is_some_and(|started| started == holder.started)",
     "    start_time(holder.pid).is_some()",
     "sankhya-atomicfs"),

    # --- what a timestamp means --------------------------------------------------------------

    # The tick counter again. `deletionTimestamp: 3` is three milliseconds after 1970, so
    # every superseded file is instantly past any retention interval and a conformant external
    # VACUUM deletes the lot --- out from under readers holding leases, and reported safe by
    # DRY RUN first.
    ("maintenance: date a removal by the tick number instead of the clock",
     "crates/sankhya-maintenance/src/service.rs",
     "                let now = epoch_millis();",
     "                let now = i64::try_from(tick).unwrap_or(i64::MAX);",
     "sankhya-maintenance"),

    # --- a commit that is all there ---------------------------------------------------------

    # Without the seal, a body truncated by a crash is a commit that silently did less than it
    # said --- and it is cemented, because a retry is refused as VersionTaken and the next
    # version lands on top of the wrong state.
    ("delta: write a commit with no seal to check it against",
     "crates/sankhya-table-delta/src/log.rs",
     "    let mut body = seal.to_string();\n    body.push('\\n');",
     "    let mut body = String::new();",
     "sankhya-table-delta"),

    # An empty body and a body truncated to nothing are the same bytes. Reading one as "a
    # commit that added nothing" drops every file the missing lines named.
    ("delta: replay an action-less commit as a commit that did nothing",
     "crates/sankhya-table-delta/src/log.rs",
     "        if read == 0 {\n            return Err(CommitError::Malformed {",
     "        if false {\n            return Err(CommitError::Malformed {",
     "sankhya-table-delta"),

    # The protocol's rule is that a reader passes over actions it does not understand. Being
    # strict here made one Spark commit enough to render a table permanently unreadable.
    ("delta: refuse a table because another engine wrote an action we lack",
     "crates/sankhya-table-delta/src/log.rs",
     "            if !matches!(kind.as_str(), \"protocol\" | \"metaData\" | \"add\" | \"remove\") {\n                continue;\n            }",
     "            if !matches!(kind.as_str(), \"protocol\" | \"metaData\" | \"add\" | \"remove\") {\n                return Err(CommitError::Malformed { version, detail: kind });\n            }",
     "sankhya-table-delta"),

    # --- the gRPC transport -----------------------------------------------------------------

    # A transport configured with a certificate that serves in the clear anyway. The failure
    # is quiet by construction: the port listens, TCP connections open, and only the *answer*
    # reveals which door it is --- which is why the test asserts on the reply and not on the
    # socket.
    #
    # There is deliberately no mutation for the `while` in `wait_for_shutdown`. One was
    # written --- `while` to `if` --- and it survived, correctly: the watch channel is created
    # inside `serve_until` and only ever carries a single `send(true)`, so the spurious
    # wake-up the loop guards against cannot be produced through the public surface. It is
    # defensive code for a case this crate does not reach, and a mutation of unreachable code
    # is a test nobody can write.
    ("api-grpc: serve in the clear despite being given a certificate",
     "crates/sankhya-api-grpc/src/lib.rs",
     "        self.encryption = Some(acceptor);\n        self",
     "        let _ = acceptor;\n        self",
     "sankhya-api-grpc"),

    # --- the graph SQL surface --------------------------------------------------------------

    # A seed key that names no vertex is a question about an entity this graph has never
    # heard of. Skipping it silently answers as though that entity existed and was connected
    # to nothing --- which is a plausible answer, and wrong.
    ("graph-sql: skip unknown seed keys instead of refusing",
     "crates/sankhya-graph-sql/src/functions.rs",
     "    if !missing.is_empty() {\n        return plan_err!(",
     "    if false {\n        return plan_err!(",
     "sankhya-graph-sql"),

    # --- the measurement harness ------------------------------------------------------------

    # `enough` decides whether a throughput measurement means anything. Reading it
    # permissively makes every capacity window hold on any machine, so the numbers describe
    # the load rather than the code --- and the skip that says so never happens.
    ("testkit: call any machine idle enough to measure on",
     "crates/sankhya-testkit/src/lib.rs",
     "        idle >= wanted",
     "        idle >= 0.0 && wanted >= 0.0",
     "sankhya-testkit"),

    # --- the source-safety ladder ----------------------------------------------------------
    #
    # `sankhya-cdc-pg` decides when SANKHYA starts hurting the PostgreSQL it reads from. Every
    # rung is a bound on somebody else's disk, and the crate had no mutation entry at all.

    # `Sacrifice` is the rung where the unread log is abandoned deliberately rather than let
    # the source remove the slot. Reached one byte late, it is reached after the source has
    # already made the decision for us.
    ("cdc-pg: enter the sacrifice rung one byte late",
     "crates/sankhya-cdc-pg/src/safety.rs",
     "    if retained >= policy.threshold(policy.sacrifice_fraction_percent)\n        || state.status == WalStatus::Unreserved",
     "    if retained > policy.threshold(policy.sacrifice_fraction_percent)\n        || state.status == WalStatus::Unreserved",
     "sankhya-cdc-pg"),

    # An unusable slot means capture is not reading; the ladder must escalate on the *status*
    # and not only on the byte count, because a stalled slot grows the source's log with no
    # local symptom at all.
    ("cdc-pg: ignore an unreserved slot unless the bytes also say so",
     "crates/sankhya-cdc-pg/src/safety.rs",
     "        || state.status == WalStatus::Unreserved",
     "        || false",
     "sankhya-cdc-pg"),

    # --- generated data ---------------------------------------------------------------------

    # The fixtures exist so a null is exercised on every nullable column. Generating none
    # means every downstream test runs on data with no nulls in it, and null handling is the
    # single most common source of a plausible wrong answer in this system.
    ("datagen: never generate a null in a nullable column",
     "crates/sankhya-datagen/src/generate.rs",
     "        if column.nullable && row % 11 == 3 {",
     "        if column.nullable && row % 11 == 3 && false {",
     "sankhya-datagen"),

    # --- Arrow Flight tickets -------------------------------------------------------------

    # A ticket is a bearer credential: it carries the authorization decision made when it was
    # issued, so it is never re-checked on redemption. Expiry is the only thing bounding how
    # long a stolen or stale one works.
    ("api-flight: redeem a ticket at the instant it expires",
     "crates/sankhya-api-flight/src/ticket.rs",
     "        if now >= self.expires_at {",
     "        if now > self.expires_at {",
     "sankhya-api-flight"),

    # --- pack loading ---------------------------------------------------------------------

    # `Trust` decides whether third-party bytes are executed. Anything other than `Allowed`
    # is a refusal, and reading it permissively loads code the deployment did not vouch for.
    ("pack: treat any trust verdict as permission to load",
     "crates/sankhya-pack/src/verify.rs",
     "        matches!(self, Self::Allowed)",
     "        !matches!(self, Self::Allowed)",
     "sankhya-pack"),

    # --- session leases -------------------------------------------------------------------

    # A lease that has expired is a lease whose pins have been released; renewing it hands
    # back a claim on data the sweeper may already have collected. Boundary, not a range:
    # `<=` at exactly `now` is the moment it stops being held.
    ("session: renew a lease that has already expired",
     "crates/sankhya-session/src/lease.rs",
     "        if existing.expires_at <= now {",
     "        if existing.expires_at < now {",
     "sankhya-session"),

    # The clamp is what stops a caller granting itself an unbounded lease, which is a pin
    # nothing can reclaim behind.
    ("session: grant whatever lifetime the caller asked for",
     "crates/sankhya-session/src/lease.rs",
     "    ) -> Lease {\n        let granted = wanted_micros.clamp(0, self.max_lifetime_micros);",
     "    ) -> Lease {\n        let granted = wanted_micros.max(0);",
     "sankhya-session"),

    # Renewal is the second site, and the one that matters more: a lease that cannot be
    # *granted* past the ceiling but can be *renewed* past it has no ceiling at all. It was
    # covered by nothing, because the entry above named text that appears at both and mutated
    # only the first.
    ("session: renew a lease for whatever lifetime the caller asked for",
     "crates/sankhya-session/src/lease.rs",
     "        let granted = wanted_micros.clamp(0, self.max_lifetime_micros);\n        let renewed = Lease {",
     "        let granted = wanted_micros.max(0);\n        let renewed = Lease {",
     "sankhya-session"),

    # --- the error protocol ---------------------------------------------------------------

    # A client retrying a `User` error retries a statement that will never succeed; a client
    # not retrying `Unavailable` gives up on one that would. The two protocols must agree,
    # which is why this decision is made once for a class rather than per error site.
    ("error: tell clients to retry anything that is not unavailable",
     "crates/sankhya-error/src/protocol.rs",
     "        matches!(self.grpc, GrpcStatus::Unavailable)",
     "        !matches!(self.grpc, GrpcStatus::Unavailable)",
     "sankhya-error"),

    # --- the published extension API ------------------------------------------------------

    # A pack built against a different API version has a different ABI. Loading it anyway
    # postpones the failure into a query, where the cause is no longer visible --- which is
    # what the refusal's own message says it exists to prevent.
    ("ext: load a pack built against another API version",
     "crates/sankhya-ext/src/registry.rs",
     "        if info.api_version != API_VERSION {",
     "        if false {",
     "sankhya-ext"),

    # A pack registering a core name changes what an existing query means without changing
    # its text. There is no error at the call site; the answer is simply somebody else's.
    ("ext: let a pack shadow a reserved engine name",
     "crates/sankhya-ext/src/registry.rs",
     "        if let Some(prefix) = RESERVED_PREFIXES.iter().find(|p| name.starts_with(**p)) {",
     "        if let Some(prefix) = RESERVED_PREFIXES.iter().find(|p| name == **p) {",
     "sankhya-ext"),

    # Two packs claiming one name resolved by load order makes the answer depend on start-up
    # ordering, which is the same statement returning different numbers on two nodes.
    ("ext: resolve a duplicate function name by load order",
     "crates/sankhya-ext/src/registry.rs",
     "        if let Some(existing) = self.scalars.get(name) {",
     "        if let Some(existing) = self.scalars.get(\"\") {",
     "sankhya-ext"),

    # --- graph overlays -------------------------------------------------------------------

    # The overlay is a delta over a built epoch. Past the threshold a rebuild is cheaper, and
    # never rebuilding means every query pays the whole accumulated delta, for ever.
    ("graph: never conclude that an overlay needs rebuilding",
     "crates/sankhya-graph/src/overlay.rs",
     "        if self.pending_edges >= self.threshold.max_edges {",
     "        if self.pending_edges > usize::MAX {",
     "sankhya-graph"),

    # --- schema evolution ----------------------------------------------------------------

    # A decimal whose scale moved is a rescale of every stored value. Calling it a widening
    # lets it through as compatible, and the column then reads a hundred times its value or a
    # hundredth of it, silently, on data already written.
    ("schema: treat a rescaled decimal as a widening",
     "crates/sankhya-schema/src/evolve.rs",
     "        (Decimal(a), Decimal(b)) => a.scale == b.scale && b.digits >= a.digits,",
     "        (Decimal(a), Decimal(b)) => b.digits >= a.digits,",
     "sankhya-schema"),

    # Narrowing a nullable column to NOT NULL is blocking because rows already written may
    # hold nulls. Accepting it makes the table's own declaration false about its contents.
    ("schema: accept tightening a nullable column",
     "crates/sankhya-schema/src/evolve.rs",
     "                        let c = SchemaChange::ColumnTightened {\n                            name: field.name.clone(),\n                        };\n                        blocking.push(c.clone());\n                        c",
     "                        SchemaChange::ColumnTightened {\n                            name: field.name.clone(),\n                        }",
     "sankhya-schema"),

    # A changed primary key changes what a row *is*. Every idempotence guarantee downstream
    # is keyed on it, so a silent identity change makes replays merge the wrong rows.
    ("schema: let an identity change through unblocked",
     "crates/sankhya-schema/src/evolve.rs",
     "                        changes.push(change.clone());\n                        blocking.push(change);",
     "                        changes.push(change);",
     "sankhya-schema"),

    # --- the pgoutput decoder ------------------------------------------------------------
    #
    # The README singles this crate out as validated against a real PostgreSQL stream, and it
    # had no mutation entry at all.

    # `u` means "this column was TOASTed and did not change, so it is not in the message".
    # Reading it as NULL blanks a column nobody touched --- the decoder's own comment calls
    # this "the dangerous one", and nothing asked whether a test would notice.
    ("cdc-model: read an unchanged TOAST value as NULL",
     "crates/sankhya-cdc-model/src/decode.rs",
     "                b'u' => TupleValue::Unchanged,",
     "                b'u' => TupleValue::Null,",
     "sankhya-cdc-model"),

    # A negative length is a corrupt or hostile stream. Trusting it turns into a `take` of a
    # length that wrapped, which is how a decoder reads past the end of what it was given.
    ("cdc-model: trust a negative text length",
     "crates/sankhya-cdc-model/src/decode.rs",
     "                b't' => {\n                    let len = c.i32()?;\n                    if len < 0 {\n                        return Err(DecodeError::NegativeLength {\n                            at: c.pos - 4,\n                            value: len,\n                        });\n                    }",
     "                b't' => {\n                    let len = c.i32()?;",
     "sankhya-cdc-model"),

    # An unrecognised tuple kind is a protocol version this decoder does not understand.
    # Guessing one is how a stream is silently misread rather than refused.
    ("cdc-model: guess at an unknown tuple kind instead of refusing",
     "crates/sankhya-cdc-model/src/decode.rs",
     "                other => {\n                    return Err(DecodeError::UnknownTupleKind {\n                        at: kind_at,\n                        kind: other,\n                    })\n                }",
     "                _ => TupleValue::Null,",
     "sankhya-cdc-model"),

    # --- M4's graph algorithms -------------------------------------------------------
    #
    # Eighteen crates had no entry at all, about nineteen thousand lines, and this was the
    # largest of them: 2,920 lines of algorithms in a milestone marked "Complete. Every exit
    # criterion met." Seven hundred mutations reads as thorough and said nothing about where
    # they were.

    # A negative weight makes Dijkstra's settled set wrong --- it may finalise a vertex whose
    # cheaper route has not been found yet. Refusing is the whole reason the precondition is
    # checked before the search rather than inside it.
    ("graph-algo: run Dijkstra over negative weights instead of refusing",
     "crates/sankhya-graph-algo/src/paths.rs",
     "    if graph.has_negative_weight() {\n        return Err(NegativeWeight);\n    }\n    Ok(dijkstra(graph, from, to, mask, budget))",
     "    Ok(dijkstra(graph, from, to, mask, budget))",
     "sankhya-graph-algo"),

    # A truncated search reported as complete is the failure this whole crate is shaped
    # against: an answer that is a subset of the truth, presented as the truth.
    ("graph-algo: call a suppressed-vertex search complete",
     "crates/sankhya-graph-algo/src/budget.rs",
     "        !self.by_results && !self.by_visits && !self.by_depth && self.suppressed.is_empty()",
     "        !self.by_results && !self.by_visits && !self.by_depth",
     "sankhya-graph-algo"),

    # Off by one on the visit budget: the search runs one vertex past what the caller allowed.
    ("graph-algo: exceed the visit budget by one",
     "crates/sankhya-graph-algo/src/paths.rs",
     "        if visits > budget.max_visits {",
     "        if visits > budget.max_visits + 1 {",
     "sankhya-graph-algo"),

    ("catalogue: read only the scalar kinds from the engine registry",
     "crates/sankhya-server/tests/catalogue.rs",
     "        out.extend(state.table_functions().keys().cloned());",
     "",
     "sankhya-server"),

    ("kernel oracle: accept any row count after compaction",
     "crates/sankhya-maintenance/tests/kernel_oracle.rs",
     "    assert_eq!(\n        read, 1000,",
     "    assert_eq!(\n        read, read,",
     "sankhya-maintenance"),

    ("feeds: read every file in the directory, not only declarations",
     "crates/sankhya-server/src/feeds.rs",
     "        .filter(|path| {\n            path.extension()\n                .is_some_and(|kind| kind.eq_ignore_ascii_case(\"yaml\") || kind.eq_ignore_ascii_case(\"yml\"))\n        })\n",
     "",
     "sankhya-server"),

    ("driver: commit a compacted file that declares no partition",
     "crates/sankhya-maintenance/src/driver.rs",
     "        add.partition_values = sankhya_table_delta::partition_values_from(&output);",
     "",
     "sankhya-maintenance"),

    ("publish: publish a file without the statistics it could have carried",
     "crates/sankhya-publish/src/publish.rs",
     "            let statistics = sankhya_table::column_stats(&part);",
     "            let statistics = sankhya_table::column_stats(&part.slice(0, 0));",
     "sankhya-publish"),

    ("publish: fail instead of rebasing on a version conflict",
     "crates/sankhya-publish/src/publish.rs",
     "                Err(sankhya_table_delta::CommitError::VersionTaken(_)) => {\n                    // The version this writer wanted is taken",
     "                Err(sankhya_table_delta::CommitError::VersionTaken(_)) if false => {\n                    // The version this writer wanted is taken",
     "sankhya-publish"),

    ("publish: retry a failure that is not a version race",
     "crates/sankhya-publish/src/publish.rs",
     "                    // then over the commits made since rather than over the whole history.\n                    self.remember(version);\n                    version = self\n                        .newest()\n                        .map_or(version.saturating_add(1), |v| v.saturating_add(1));\n                }\n                Err(error) => {\n                    return Err(PublishError::Commit {\n                        version,\n                        detail: error.to_string(),\n                    })\n                }",
     "                    // then over the commits made since rather than over the whole history.\n                    self.remember(version);\n                    version = self\n                        .newest()\n                        .map_or(version.saturating_add(1), |v| v.saturating_add(1));\n                }\n                Err(_) => {}",
     "sankhya-publish"),

    ("log: replay by scanning the file list instead of indexing it",
     "crates/sankhya-table-delta/src/log.rs",
     "            Action::Add(add) => match self.position.get(&add.path).copied() {\n                Some(index) => {\n                    if let Some(slot) = self.files.get_mut(index) {\n                        *slot = Some(add);\n                    }\n                }\n                None => {\n                    self.position.insert(add.path.clone(), self.files.len());\n                    self.files.push(Some(add));\n                }\n            },",
     "            Action::Add(add) => {\n                if let Some(existing) =\n                    self.files.iter_mut().flatten().find(|f| f.path == add.path)\n                {\n                    *existing = add;\n                } else {\n                    self.files.push(Some(add));\n                }\n            }",
     "sankhya-table-delta"),

    ("cache: trust the cached version instead of asking the log",
     "crates/sankhya-table-delta/src/cache.rs",
     "        let newest = newest_after(table_root, cached);",
     "        let newest = cached;",
     "sankhya-table-delta"),

    ("cache: resume from a stale base after the table was rebuilt",
     "crates/sankhya-table-delta/src/cache.rs",
     "        if rebuilt {\n            *replay = Replay::default();\n        }",
     "",
     "sankhya-table-delta"),

    ("log: allow a commit that leaves a gap",
     "crates/sankhya-table-delta/src/log.rs",
     "    if version > 0 && !commit_path(table_root, version - 1).exists() {",
     "    if false {",
     "sankhya-table-delta"),

    ("log: stop walking commits at the first gap without noticing",
     "crates/sankhya-table-delta/src/log.rs",
     "    let mut version = after.map_or(0, |v| v + 1);",
     "    let mut version = after.map_or(1, |v| v + 2);",
     "sankhya-table-delta"),

    ("admission: tell a permanently-too-large query to retry",
     "crates/sankhya-governor/src/admission.rs",
     "            Self::ExceedsPool { .. } | Self::ExceedsTenantCap { .. } => false,",
     "            Self::ExceedsTenantCap { .. } => false,\n            Self::ExceedsPool { .. } => true,",
     "sankhya-governor"),

    ("admission: queue a query that can never run",
     "crates/sankhya-governor/src/admission.rs",
     "    if demand.estimated_bytes > pool.total_bytes {",
     "    if false {",
     "sankhya-governor"),

    ("admission: queue without bound instead of refusing",
     "crates/sankhya-governor/src/admission.rs",
     "    if pool.queued >= pool.max_queue_depth {",
     "    if false {",
     "sankhya-governor"),

    ("admission: ignore the tenant floor under global pressure",
     "crates/sankhya-governor/src/admission.rs",
     "    if after <= limits.floor_bytes {\n        return Decision::Admit;\n    }",
     "",
     "sankhya-governor"),

    ("admission: let an idle pool override the tenant cap",
     "crates/sankhya-governor/src/admission.rs",
     "    if after > limits.cap_bytes {\n        return queue_or_refuse(pool);\n    }",
     "",
     "sankhya-governor"),

    ("admission: keep serving while the system sheds load",
     "crates/sankhya-governor/src/admission.rs",
     "    if posture == Posture::Shedding {",
     "    if false {",
     "sankhya-governor"),

    ("pressure: let compaction debt stop queries",
     "crates/sankhya-governor/src/pressure.rs",
     "    let debt = rung(signals.compaction_debt, thresholds, false).min(Level::Watch);",
     "    let debt = rung(signals.compaction_debt, thresholds, false);",
     "sankhya-governor"),

    ("pressure: let a full arrival buffer sacrifice continuity",
     "crates/sankhya-governor/src/pressure.rs",
     "    let buffer = rung(signals.arrival_buffer, thresholds, false);",
     "    let buffer = rung(signals.arrival_buffer, thresholds, true);",
     "sankhya-governor"),

    ("pressure: wait for the source to act before sacrificing",
     "crates/sankhya-governor/src/pressure.rs",
     "            sacrifice: 0.95,",
     "            sacrifice: 1.0,",
     "sankhya-governor"),

    ("pressure: keep admitting queries while protecting the source",
     "crates/sankhya-governor/src/pressure.rs",
     "        matches!(self, Self::Normal | Self::Watch | Self::Constrain)",
     "        !matches!(self, Self::Sacrifice)",
     "sankhya-governor"),

    ("checkpoint: trust a pointer to a checkpoint file that is not there",
     "crates/sankhya-table-delta/src/checkpoint.rs",
     "    if !checkpoint_path(table_root, version).exists() {\n        return None;\n    }",
     "",
     "sankhya-table-delta"),

    ("checkpoint: trust a checkpoint from a table that was rebuilt",
     "crates/sankhya-table-delta/src/checkpoint.rs",
     "    if !commit_path(table_root, version).exists() {\n        return None;\n    }",
     "",
     "sankhya-table-delta"),

    ("checkpoint: write the pointer before the checkpoint file",
     "crates/sankhya-table-delta/src/checkpoint.rs",
     "    std::fs::rename(&staging, &path)\n        .map_err(|e| CommitError::Io(format!(\"publishing {}: {e}\", path.display())))?;",
     "",
     "sankhya-table-delta"),

    ("checkpoint: include files that were removed",
     "crates/sankhya-table-delta/src/checkpoint.rs",
     "            if !adds.is_valid(row) {\n                continue;\n            }",
     "",
     "sankhya-table-delta"),

    ("driver: checkpoint a brand-new table on its first commit",
     "crates/sankhya-maintenance/src/driver.rs",
     "    let since = version.saturating_sub(latest_checkpoint(table_root).unwrap_or(0));",
     "    let since = latest_checkpoint(table_root).map_or(version + 1, |last| version - last);",
     "sankhya-maintenance"),

    ("key: leave the entitlement set out of a result key",
     "crates/sankhya-catalog/src/key.rs",
     "            &entitlements.fingerprint().to_be_bytes(),",
     "",
     "sankhya-catalog"),

    ("key: leave the policy version out of a plan key",
     "crates/sankhya-catalog/src/key.rs",
     "            &policy_version.to_be_bytes(),",
     "",
     "sankhya-catalog"),

    ("key: leave the snapshot version out of a result key",
     "crates/sankhya-catalog/src/key.rs",
     "            &snapshot_version.to_be_bytes(),",
     "",
     "sankhya-catalog"),

    ("key: concatenate components without length prefixes",
     "crates/sankhya-catalog/src/key.rs",
     "        buffer.extend_from_slice(&(part.len() as u64).to_be_bytes());",
     "",
     "sankhya-catalog"),

    ("key: use a process-seeded hash",
     "crates/sankhya-catalog/src/key.rs",
     "    let mut h: u64 = 0xcbf2_9ce4_8422_2325;",
     "    let mut h: u64 = 0xcbf2_9ce4_8422_2326;",
     "sankhya-catalog"),

    ("orphans: sweep a file that may still be mid-commit",
     "crates/sankhya-maintenance/src/orphans.rs",
     "        if file.age_ticks < policy.min_age_ticks {",
     "        if false {",
     "sankhya-maintenance"),

    ("orphans: sweep a file a retained snapshot still reaches",
     "crates/sankhya-maintenance/src/orphans.rs",
     "        if reachable.contains(&file.name) {",
     "        if false {",
     "sankhya-maintenance"),

    ("orphans: treat the log as data",
     "crates/sankhya-maintenance/src/orphans.rs",
     "        if file.name.starts_with('_') || file.name.contains(\"_delta_log\") {\n            continue;\n        }",
     "",
     "sankhya-maintenance"),

    ("orphans: stop the sweep at the first file that will not delete",
     "crates/sankhya-maintenance/src/orphans.rs",
     "            Err(e) => report.failed.push((name.clone(), e.to_string())),",
     "            Err(e) => {\n                report.failed.push((name.clone(), e.to_string()));\n                break;\n            }",
     "sankhya-maintenance"),

    ("cancel: report a deadline when the query was explicitly cancelled",
     "crates/sankhya-governor/src/cancel.rs",
     "        if self.cancel.is_cancelled() {\n            return Err(Stopped::Cancelled);\n        }\n        if self.deadline.expired_at(now) {",
     "        if self.deadline.expired_at(now) {",
     "sankhya-governor"),

    ("cancel: allow a check interval of zero",
     "crates/sankhya-governor/src/cancel.rs",
     "            check_every: check_every.max(1),",
     "            check_every,",
     "sankhya-governor"),

    ("cancel: let a relative deadline wrap instead of saturating",
     "crates/sankhya-governor/src/cancel.rs",
     "            at: now.saturating_add(ticks),",
     "            at: now.wrapping_add(ticks),",
     "sankhya-governor"),

    ("cancel: tell a client a cancellation can be retried",
     "crates/sankhya-governor/src/cancel.rs",
     "        matches!(self, Self::DeadlineExceeded { .. })",
     "        true",
     "sankhya-governor"),

    ("budgeted: end the stream quietly instead of failing",
     "crates/sankhya-readpath/src/budgeted.rs",
     "            return Poll::Ready(Some(Err(to_error(stopped))));",
     "            let _ = stopped;\n            return Poll::Ready(None);",
     "sankhya-readpath"),

    ("budgeted: never check the budget at all",
     "crates/sankhya-readpath/src/budgeted.rs",
     "        if let Err(stopped) = this.budget.check_periodically(this.batches, (this.clock)()) {",
     "        if let Err(stopped) = Ok::<(), Stopped>(()) {",
     "sankhya-readpath"),

    ("alloc: count a reallocation as a fresh allocation",
     "crates/sankhya-alloc/src/lib.rs",
     "            if new_size >= layout.size() {\n                self.record_growth(new_size - layout.size());",
     "            if new_size >= layout.size() {\n                self.record_growth(new_size);",
     "sankhya-alloc"),

    # Deliberately absent: "let the total wrap when a release exceeds it".
    #
    # The saturating subtraction is defence against a miscount somewhere else -- a
    # deallocation whose size disagrees with its allocation. Through the allocator's own
    # paths every release is paired with a growth, so the count cannot go negative and
    # saturating and wrapping are indistinguishable. Any test that could tell them apart
    # would have to construct the miscount, which is the defect the guard exists to
    # survive rather than one it should permit.
    #
    # Recorded here rather than silently dropped, because the reasoning is the useful
    # part and someone will otherwise write the entry again.

    ("alloc: let the peak fall back to the current total",
     "crates/sankhya-alloc/src/lib.rs",
     "        let mut seen = self.peak.load(Ordering::Relaxed);\n        while now > seen {",
     "        let mut seen = self.peak.load(Ordering::Relaxed);\n        while now != seen {",
     "sankhya-alloc"),

    ("alloc: discard the current total when the peak is reset",
     "crates/sankhya-alloc/src/lib.rs",
     "        self.peak.store(self.in_use(), Ordering::Relaxed);",
     "        self.peak.store(0, Ordering::Relaxed);",
     "sankhya-alloc"),

    ("alloc: report zero rather than nothing when no allocator was announced",
     "crates/sankhya-alloc/src/lib.rs",
     "    INSTALLED.get().map(|allocator| allocator.in_use())",
     "    Some(INSTALLED.get().map_or(0, |allocator| allocator.in_use()))",
     "sankhya-alloc"),

    # Deliberately absent: "let a second announcement replace the installed allocator".
    #
    # There is no one-line edit that produces it. `OnceLock` has no method that
    # overwrites, so the defect requires swapping the container for a mutable one and
    # rewriting both readers -- which the audit would report as a failure to compile
    # rather than as a surviving defect, and which is a redesign, not a slip.
    #
    # `a_second_announcement_does_not_replace_the_first` is kept anyway, because it pins
    # the property at the API rather than at the container: whoever makes that swap for a
    # reason that seems good at the time gets told what it costs.

    ("cube ddl: accept whatever follows a statement instead of refusing it",
     "crates/sankhya-cube-sql/src/ddl.rs",
     "        if self.peek().is_some() {",
     "        if false {",
     "sankhya-cube-sql"),

    ("cube ddl: check the fact table and let the dimension tables through",
     "crates/sankhya-server/src/cubes.rs",
     "    tables.extend(definition.dimensions.iter().map(|d| d.table.clone()));",
     "        let _ = &definition.dimensions;",
     "sankhya-server"),

    ("cube ddl: let a name already taken be created a second time",
     "crates/sankhya-server/src/cubes.rs",
     "    if server.cubes().iter().any(|cube| cube.name() == name) {",
     "        if false {",
     "sankhya-server"),

    ("cube ddl: drop a cube and leave its cuboids behind",
     "crates/sankhya-server/src/cubes.rs",
     "    let swept = sankhya_maintenance::cuboid::retire_cube(&server.settings.warehouse, name);",
     "        let swept = sankhya_maintenance::cuboid::Swept::default();",
     "sankhya-server"),

    ("cuboid: retire a cube's cuboids by prefix rather than by parsing the name",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "        if owner != cube {",
     "        if !name.contains(cube) {",
     "sankhya-maintenance"),

    ("attest: run against a store that never said it was non-production",
     "crates/sankhya-backup/src/attest.rs",
     "    if !store.is_non_production() {",
     "    if false {",
     "sankhya-backup"),

    ("attest: trust the reported refusals and skip reading the object back",
     "crates/sankhya-backup/src/attest.rs",
     "        Err(_) => Some(false),",
     "        Err(_) => Some(true),",
     "sankhya-backup"),

    ("attest: stop at the first violation instead of attempting every one",
     "crates/sankhya-backup/src/attest.rs",
     "    let attempts: Vec<(Forbidden, Outcome)> = Forbidden::ALL\n        .iter()",
     "    let attempts: Vec<(Forbidden, Outcome)> = Forbidden::ALL\n        .iter()\n        .take(1)",
     "sankhya-backup"),

    ("attest: an attestation with an untested violation still passes",
     "crates/sankhya-backup/src/attest.rs",
     "            && self.attempts.len() == Forbidden::ALL.len()",
     "            && !self.attempts.is_empty()",
     "sankhya-backup"),

    ("doctor: nag about attestation on a deployment that archives nothing",
     "crates/sankhya-diagnostic/src/check.rs",
     "    if !archived {",
     "    if false {",
     "sankhya-diagnostic"),

    ("tiering: let a float column be archived",
     "crates/sankhya-tiering/src/policy.rs",
     "        LogicalType::Float32 | LogicalType::Float64 => Err(NotCanonical::FloatingPoint),",
     "        LogicalType::Float32 => Err(NotCanonical::FloatingPoint),\n        LogicalType::Float64 => Ok(()),",
     "sankhya-tiering"),

    ("tiering: let a json column be archived",
     "crates/sankhya-tiering/src/policy.rs",
     "        LogicalType::Json => Err(NotCanonical::UnstableTextForm),",
     "        LogicalType::Json => Ok(()),",
     "sankhya-tiering"),

    ("tiering: call a table with no columns eligible",
     "crates/sankhya-tiering/src/policy.rs",
     "            return Eligibility { refusals: vec![Ineligible::NoColumns] };",
     "            return Eligibility { refusals: Vec::new() };",
     "sankhya-tiering"),

    ("tiering: stop at the first unarchivable column",
     "crates/sankhya-tiering/src/policy.rs",
     "        for column in &self.columns {",
     "        for column in self.columns.iter().take(1) {",
     "sankhya-tiering"),

    ("tiering: accept a retention basis that expires immediately",
     "crates/sankhya-tiering/src/policy.rs",
     "        if self.retention.basis.trim().is_empty() || self.retention.days == 0 {",
     "        if self.retention.basis.trim().is_empty() {",
     "sankhya-tiering"),

    ("tiering: treat unvaulted identifiers as acceptable",
     "crates/sankhya-tiering/src/policy.rs",
     "        if !self.identifiers_vaulted {",
     "        if false {",
     "sankhya-tiering"),

    ("tiering: let a purge skip a phase",
     "crates/sankhya-tiering/src/machine.rs",
     "        if next.requires() != Some(self.phase()) {",
     "        if false {",
     "sankhya-tiering"),

    ("tiering: resume a journal that skipped a phase",
     "crates/sankhya-tiering/src/machine.rs",
     "            if entry.phase.requires() != entered.last().copied() {",
     "            if false {",
     "sankhya-tiering"),

    ("tiering: ignore the kill switch",
     "crates/sankhya-tiering/src/machine.rs",
     "        if let Some(switch) = kill_switch {",
     "        if let Some(switch) = None::<&str> {",
     "sankhya-tiering"),

    ("tiering: call a detach non-destructive",
     "crates/sankhya-tiering/src/machine.rs",
     "        matches!(self, Self::Detached | Self::Dropped | Self::Recorded)",
     "        matches!(self, Self::Dropped | Self::Recorded)",
     "sankhya-tiering"),

    ("tiering: replay another purge's journal entries as this purge's",
     "crates/sankhya-tiering/src/machine.rs",
     "        for entry in journal.iter().filter(|entry| entry.purge == purge) {",
     "        for entry in journal.iter() {",
     "sankhya-tiering"),

    ("tiering: encode a value without its type tag",
     "crates/sankhya-tiering/src/encode.rs",
     "    let tag = tag_for(column, array.data_type())?;\n    out.push(tag);",
     "    let tag = tag_for(column, array.data_type())?;\n    let _ = tag;",
     "sankhya-tiering"),

    ("tiering: concatenate variable-length values without a length prefix",
     "crates/sankhya-tiering/src/encode.rs",
     "    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);\n    out.extend_from_slice(&length.to_be_bytes());",
     "    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);\n    let _ = length;",
     "sankhya-tiering"),

    ("tiering: fingerprint rows in scan order rather than key order",
     "crates/sankhya-tiering/src/verify.rs",
     "        self.rows.sort_by(|left, right| left.0.cmp(&right.0));",
     "        let _unsorted = &self.rows;",
     "sankhya-tiering"),

    ("tiering: report the first column that differs rather than every one",
     "crates/sankhya-tiering/src/verify.rs",
     "    for column in &source.columns {\n        match archive.columns.iter()",
     "    for column in source.columns.iter().take(1) {\n        match archive.columns.iter()",
     "sankhya-tiering"),

    ("tiering: trust the two scans and drop the plan's row count",
     "crates/sankhya-tiering/src/verify.rs",
     "    if source.rows != planned {",
     "    if source.rows != planned && planned == 0 {",
     "sankhya-tiering"),

    ("tiering: stop counting duplicate primary keys",
     "crates/sankhya-tiering/src/verify.rs",
     "            .filter(|pair| matches!(pair, [before, after] if before.0 == after.0))",
     "            .filter(|_pair| false)",
     "sankhya-tiering"),

    ("tiering: let the general transition journal a verification",
     "crates/sankhya-tiering/src/machine.rs",
     "        if next == Phase::Verified {\n            return Err(Halt::UnprovenVerification);",
     "        if next == Phase::Planned {\n            return Err(Halt::UnprovenVerification);",
     "sankhya-tiering"),

    ("tiering: hand out a proof whatever the verification found",
     "crates/sankhya-tiering/src/verify.rs",
     "        self.passed().then_some(Proof { _private: () })",
     "        Some(Proof { _private: () })",
     "sankhya-tiering"),

    ("tiering: admit a table with no declared primary key",
     "crates/sankhya-tiering/src/policy.rs",
     "        if !self.columns.iter().any(|column| column.key) {",
     "        if self.columns.iter().any(|column| column.key) {",
     "sankhya-tiering"),

    ("tiering: let two registry entries claim the same rows",
     "crates/sankhya-tiering/src/registry.rs",
     "            .find(|other| other.table == entry.table && other.range.intersects(&entry.range))",
     "            .find(|other| other.table == entry.table && other.range == entry.range)",
     "sankhya-tiering"),

    ("tiering: treat two adjacent archived ranges as overlapping",
     "crates/sankhya-tiering/src/registry.rs",
     "        !self.is_empty() && !other.is_empty() && self.from < other.until && other.from < self.until",
     "        !self.is_empty() && !other.is_empty() && self.from <= other.until && other.from < self.until",
     "sankhya-tiering"),

    ("tiering: report a coverage gap at the end of a range as covered",
     "crates/sankhya-tiering/src/registry.rs",
     "        if at < wanted.until && !wanted.is_empty() {",
     "        if at < wanted.from && !wanted.is_empty() {",
     "sankhya-tiering"),

    ("tiering: pin a snapshot only while a legal hold is set",
     "crates/sankhya-tiering/src/registry.rs",
     "        self.legal_hold || now < self.retained_until()",
     "        self.legal_hold",
     "sankhya-tiering"),

    ("tiering: let a pinned snapshot be expired",
     "crates/sankhya-tiering/src/registry.rs",
     "        if self.snapshots.contains(snapshot) {",
     "        if !self.snapshots.contains(snapshot) {",
     "sankhya-tiering"),

    ("tiering: keep pinning a snapshot after its retention basis lapses",
     "crates/sankhya-tiering/src/registry.rs",
     "                .filter(|entry| entry.binding_at(now))",
     "                .filter(|_entry| true)",
     "sankhya-tiering"),

    ("plan: report a coverage gap as a freshness problem",
     "crates/sankhya-plan/src/splice.rs",
     "            SpliceError::CoverageGap { from, to } => {\n                sankhya_error::Error::CoverageGap(",
     "            SpliceError::CoverageGap { from, to } => {\n                sankhya_error::Error::StatementFailed(",
     "sankhya-plan", 1, "splice"),

    ("plan: answer a position past the frontier with a retry delay",
     "crates/sankhya-plan/src/splice.rs",
     "            } => sankhya_error::Error::StatementFailed(format!(",
     "            } => sankhya_error::Error::StaleData(format!(",
     "sankhya-plan", 1, "splice"),

    ("tiering: report an unreconciled table as a coverage gap",
     "crates/sankhya-tiering/src/unify.rs",
     "            Unservable::NotReconciled { .. } => sankhya_error::Error::ArchiveConflict(detail),",
     "            Unservable::NotReconciled { .. } => sankhya_error::Error::CoverageGap(detail),",
     "sankhya-tiering", 1, "unification"),

    ("server: export a flat zero from a server that maintains nothing",
     "crates/sankhya-server/src/scrape.rs",
     "    let exported: Vec<&'static Metric> = if server.maintains() {",
     "    let exported: Vec<&'static Metric> = if true {",
     "sankhya-server", 1, "published"),
    ("server: stop publishing what maintenance has done",
     "crates/sankhya-server/src/main.rs",
     "    if let Some(handle) = maintenance.clone() {\n        let metrics = server.metrics();",
     "    if let Some(handle) = maintenance.clone().filter(|_| false) {\n        let metrics = server.metrics();",
     "sankhya-server", 1, "published"),

    ("server: publish the table count once and never again",
     "crates/sankhya-server/src/main.rs",
     "                        handle.maintaining() as f64,",
     "                        0.0,",
     "sankhya-server", 1, "published"),
    ("tiering: serve a table a reconciliation conflict names",
     "crates/sankhya-tiering/src/registry.rs",
     "            Conflict::InBothTiers { table: affected, .. } => affected != table,",
     "            Conflict::InBothTiers { table: affected, .. } => affected == table,",
     "sankhya-tiering", 1, "unification"),

    ("tiering: hand out a witness for a table nobody reconciled",
     "crates/sankhya-tiering/src/registry.rs",
     "        (looked && clean).then(|| Servable { table: table.to_string() })",
     "        clean.then(|| Servable { table: table.to_string() })",
     "sankhya-tiering", 1, "unification"),

    ("tiering: lose an archival entry across a restore without reporting it",
     "crates/sankhya-tiering/src/registry.rs",
     "            if !mine.contains_key(id) {",
     "            if mine.contains_key(id) {",
     "sankhya-tiering"),

    ("tiering: reconcile a hot range against another table's archives",
     "crates/sankhya-tiering/src/registry.rs",
     "                if *table == entry.table && hot.intersects(&entry.range) {",
     "                if hot.intersects(&entry.range) {",
     "sankhya-tiering"),

    ("tiering: admit a tiering key whose type has no ordinal",
     "crates/sankhya-tiering/src/policy.rs",
     "            if !has_ordinal(&column.logical) {",
     "            if has_ordinal(&column.logical) {",
     "sankhya-tiering"),

    ("tiering: skip a delete into an archived range rather than halting",
     "crates/sankhya-tiering/src/defence.rs",
     "                Some(extent) => alarm(Reason::DeleteInArchivedRange, Some(extent)),",
     "                Some(_extent) => Verdict::Apply,",
     "sankhya-tiering"),

    ("tiering: apply a delete whose key cannot be read",
     "crates/sankhya-tiering/src/defence.rs",
     "            (_, At::Unknown) => alarm(Reason::UnlocatableAgainstArchive, None),",
     "            (_, At::Unknown) => Verdict::Apply,",
     "sankhya-tiering"),

    ("tiering: let a truncate through when no key places it in an archive",
     "crates/sankhya-tiering/src/defence.rs",
     "            (Change::Truncate, _) | (_, At::WholeRelation) => {",
     "            (Change::Truncate, At::WholeRelation) | (_, At::WholeRelation) => {",
     "sankhya-tiering"),

    ("tiering: check the archival extent map against the wrong table",
     "crates/sankhya-tiering/src/defence.rs",
     "            .find(|range| range.contains(at))",
     "            .find(|range| !range.contains(at))",
     "sankhya-tiering"),

    ("tiering: admit a table whose publication would carry a delete",
     "crates/sankhya-tiering/src/policy.rs",
     "        if !self.publication_excludes_deletes {",
     "        if self.publication_excludes_deletes {",
     "sankhya-tiering"),

    ("tiering: answer short instead of reporting a coverage gap",
     "crates/sankhya-tiering/src/unify.rs",
     "        return Err(Unservable::CoverageGap { table, gaps });",
     "        let _unreported = &gaps;",
     "sankhya-tiering"),

    ("tiering: report only the first coverage gap",
     "crates/sankhya-tiering/src/unify.rs",
     "                gaps.push(piece);",
     "                if gaps.is_empty() { gaps.push(piece); }",
     "sankhya-tiering"),

    ("tiering: read a range both tiers claim from the archive instead of the source",
     "crates/sankhya-tiering/src/unify.rs",
     "                Read::Source\n            }\n            (true, None) => Read::Source,",
     "                Read::Archive { archive: entry.archive.clone(), extent: entry.range }\n            }\n            (true, None) => Read::Source,",
     "sankhya-tiering"),

    ("tiering: absorb a tier disagreement rather than reporting it",
     "crates/sankhya-tiering/src/unify.rs",
     "                inconsistencies.push(Conflict::InBothTiers {",
     "                let _absorbed = (Conflict::InBothTiers {",
     "sankhya-tiering"),

    ("tiering: merge two query segments read from different places",
     "crates/sankhya-tiering/src/unify.rs",
     "            Some(last) if last.read == read && last.range.until == piece.from => {",
     "            Some(last) if last.range.until == piece.from => {",
     "sankhya-tiering"),

    ("tiering: report no rows affected instead of refusing a mutation into an archive",
     "crates/sankhya-tiering/src/unify.rs",
     "        Some(entry) => Err(Immutable {\n            table: table.to_string(),\n            archive: entry.archive.clone(),\n            extent: entry.range,\n        }),",
     "        Some(_entry) => Ok(()),",
     "sankhya-tiering"),

    ("tiering: allow a quarantine grace period of zero days",
     "crates/sankhya-tiering/src/quarantine.rs",
     "        if days == 0 {\n            return Err(NoGrace);",
     "        if days > 0 && days == 0 {\n            return Err(NoGrace);",
     "sankhya-tiering"),

    ("tiering: reap a quarantined partition the registry no longer claims",
     "crates/sankhya-tiering/src/quarantine.rs",
     "            } else if archived(&held) {",
     "            } else if archived(&held) || true {",
     "sankhya-tiering"),

    ("tiering: reap a quarantined partition while its grace period runs",
     "crates/sankhya-tiering/src/quarantine.rs",
     "            if held.within_grace(now) {",
     "            if false && held.within_grace(now) {",
     "sankhya-tiering"),

    ("tiering: match a quarantined range against any archive of the table",
     "crates/sankhya-tiering/src/quarantine.rs",
     "                .any(|entry| entry.table == held.table && entry.range == held.range)",
     "                .any(|entry| entry.table == held.table)",
     "sankhya-tiering"),

    ("tiering: re-attach without withdrawing the archival entry",
     "crates/sankhya-tiering/src/quarantine.rs",
     "        let entry_withdrawn = registry.withdraw(&held.table, held.range).is_some();",
     "        let entry_withdrawn = registry.entries().iter().any(|e| e.range == held.range);",
     "sankhya-tiering"),

    ("tiering: re-attach a partition whose grace period has ended",
     "crates/sankhya-tiering/src/quarantine.rs",
     "        if !held.within_grace(now) {",
     "        if false && !held.within_grace(now) {",
     "sankhya-tiering"),

    ("tiering: allow a rehydrated copy that never expires",
     "crates/sankhya-tiering/src/rehydrate.rs",
     "        if days == 0 {\n            return Err(NotBounded::Never);",
     "        if days > 0 && days == 0 {\n            return Err(NotBounded::Never);",
     "sankhya-tiering"),

    ("tiering: grant a rehydration for longer than one decision should reach",
     "crates/sankhya-tiering/src/rehydrate.rs",
     "        if days > Self::MAXIMUM_DAYS {",
     "        if days > Self::MAXIMUM_DAYS * 10 {",
     "sankhya-tiering"),

    ("tiering: rehydrate into a schema a publication could capture",
     "crates/sankhya-tiering/src/rehydrate.rs",
     "        if !excluded_from_publications {",
     "        if excluded_from_publications {",
     "sankhya-tiering"),

    ("tiering: rehydrate into the live table's own schema",
     "crates/sankhya-tiering/src/rehydrate.rs",
     "        if schema == live_schema {",
     "        if schema.is_empty() && schema == live_schema {",
     "sankhya-tiering"),

    ("tiering: keep a rehydrated copy past its expiry",
     "crates/sankhya-tiering/src/rehydrate.rs",
     "            std::mem::take(&mut self.copies).into_iter().partition(|copy| copy.live_at(now));",
     "            std::mem::take(&mut self.copies).into_iter().partition(|_copy| true);",
     "sankhya-tiering"),

    ("tiering: report the newest rehydrated copy's age rather than the oldest",
     "crates/sankhya-tiering/src/rehydrate.rs",
     "            .max()\n            .unwrap_or(0);",
     "            .min()\n            .unwrap_or(0);",
     "sankhya-tiering"),

    ("tiering: allow a controlled rewrite that keeps no prior version",
     "crates/sankhya-tiering/src/rehydrate.rs",
     "        if prior_version.trim().is_empty() {",
     "        if !prior_version.trim().is_empty() && prior_version.trim().is_empty() {",
     "sankhya-tiering"),

    ("tiering: allow a controlled rewrite that records no amendment link",
     "crates/sankhya-tiering/src/rehydrate.rs",
     "        if amendment.trim().is_empty() {",
     "        if !amendment.trim().is_empty() && amendment.trim().is_empty() {",
     "sankhya-tiering"),

    ("tiering: migrate a table that is only partly archived",
     "crates/sankhya-tiering/src/migrate.rs",
     "    if !coverage.is_complete() {",
     "    if coverage.is_complete() && !coverage.is_complete() {",
     "sankhya-tiering"),

    ("tiering: report the first hole rather than every one when refusing a migration",
     "crates/sankhya-tiering/src/migrate.rs",
     "            gaps: coverage.gaps,",
     "            gaps: coverage.gaps.into_iter().take(1).collect(),",
     "sankhya-tiering"),

    ("tiering: migrate a table whose declared key domain covers nothing",
     "crates/sankhya-tiering/src/migrate.rs",
     "    if domain.is_empty() {",
     "    if !domain.is_empty() && domain.is_empty() {",
     "sankhya-tiering"),

    ("tiering: mark a migrated table writable",
     "crates/sankhya-tiering/src/migrate.rs",
     "    pub const fn writable(&self) -> bool {\n        false\n    }",
     "    pub const fn writable(&self) -> bool {\n        true\n    }",
     "sankhya-tiering"),

    ("tiering: leave the ranges out of the plan digest",
     "crates/sankhya-tiering/src/command.rs",
     "        for range in &self.moves {\n            hasher.update(range.from.to_be_bytes());\n            hasher.update(range.until.to_be_bytes());\n        }",
     "        for range in &self.moves {\n            let _unbound = (range.from, range.until);\n        }",
     "sankhya-tiering"),

    ("tiering: leave the cluster out of the plan digest",
     "crates/sankhya-tiering/src/command.rs",
     "        for part in [self.cluster.as_str(), self.policy.as_str(), self.table.as_str()] {",
     "        for part in [self.policy.as_str(), self.table.as_str()] {",
     "sankhya-tiering"),

    ("tiering: accept an invocation asserting another cluster",
     "crates/sankhya-tiering/src/command.rs",
     "    if invocation.cluster_asserted != this_cluster {",
     "    if invocation.cluster_asserted == this_cluster && false {",
     "sankhya-tiering"),

    ("tiering: accept a purge with no change-management reference",
     "crates/sankhya-tiering/src/command.rs",
     "    if invocation.change_reference.trim().is_empty() {",
     "    if !invocation.change_reference.trim().is_empty() && false {",
     "sankhya-tiering"),

    ("tiering: run a plan whose preconditions failed",
     "crates/sankhya-tiering/src/command.rs",
     "    if !proposal.refusals.is_empty() {",
     "    if proposal.refusals.is_empty() && false {",
     "sankhya-tiering"),

    ("tiering: accept an expired plan digest",
     "crates/sankhya-tiering/src/command.rs",
     "    if now >= proposal.expires_at() {",
     "    if now >= proposal.expires_at().saturating_mul(2) {",
     "sankhya-tiering"),

    ("tiering: accept ranges the plan digest was not taken over",
     "crates/sankhya-tiering/src/command.rs",
     "    if invocation.ranges != proposal.moves || invocation.digest != proposal.digest() {",
     "    if invocation.ranges != proposal.moves {",
     "sankhya-tiering"),

    ("tiering: let a policy's definer approve their own work",
     "crates/sankhya-tiering/src/permission.rs",
     "    if principal.name == policy.definer && permission.conflicts_with_defining() {",
     "    if principal.name != policy.definer && permission.conflicts_with_defining() {",
     "sankhya-tiering"),

    ("tiering: treat approval as compatible with having defined a policy",
     "crates/sankhya-tiering/src/permission.rs",
     "        matches!(self, Self::Approve | Self::Execute | Self::Purge)",
     "        matches!(self, Self::Execute | Self::Purge)",
     "sankhya-tiering"),

    ("tiering: grant a permission a principal does not hold",
     "crates/sankhya-tiering/src/permission.rs",
     "    if !principal.permissions.contains(&permission) {",
     "    if principal.permissions.contains(&permission) && false {",
     "sankhya-tiering"),

    ("tiering: run a schedule that was enabled but never approved",
     "crates/sankhya-tiering/src/schedule.rs",
     "        if self.approved.is_none() || !self.enabled {",
     "        if self.approved.is_none() && !self.enabled {",
     "sankhya-tiering"),

    ("tiering: ignore the tiering kill switch",
     "crates/sankhya-tiering/src/schedule.rs",
     "        if let Some(switch) = kill_switch {",
     "        if let Some(switch) = kill_switch.filter(|_| false) {",
     "sankhya-tiering"),

    ("tiering: enable a new tiering schedule by default",
     "crates/sankhya-tiering/src/schedule.rs",
     "            enabled: false,",
     "            enabled: true,",
     "sankhya-tiering"),

    ("tiering: let a new schedule default to purging",
     "crates/sankhya-tiering/src/schedule.rs",
     "            stage: Stage::Archive,",
     "            stage: Stage::Purge,",
     "sankhya-tiering"),

    ("tiering: raise the anomaly threshold out of reach",
     "crates/sankhya-tiering/src/schedule.rs",
     "            if median > 0 && candidates > median.saturating_mul(self.anomaly_factor) {",
     "            if median > 0 && candidates > median.saturating_mul(self.anomaly_factor * 100) {",
     "sankhya-tiering"),

    ("tiering: halt every run of a schedule whose history is all empty",
     "crates/sankhya-tiering/src/schedule.rs",
     "            if median > 0 && candidates",
     "            if median >= 0 && candidates",
     "sankhya-tiering"),

    ("tiering: take the sum rather than the midpoint of an even history",
     "crates/sankhya-tiering/src/schedule.rs",
     "            Some((low + high) / 2)",
     "            Some(low + high)",
     "sankhya-tiering"),

    ("tiering: ignore the per-run blast-radius limit",
     "crates/sankhya-tiering/src/schedule.rs",
     "        let by_run = wanted.min(self.ranges_per_run);",
     "        let by_run = wanted;",
     "sankhya-tiering"),

    ("tiering: ignore the daily blast-radius limit",
     "crates/sankhya-tiering/src/schedule.rs",
     "        let ranges = by_run.min(left_today);",
     "        let ranges = by_run;",
     "sankhya-tiering"),

    ("tiering: seal an evidence pack over its keys and not its values",
     "crates/sankhya-tiering/src/evidence.rs",
     "            for part in [key.as_bytes(), value.as_bytes()] {",
     "            for part in [key.as_bytes()] {",
     "sankhya-tiering"),

    ("tiering: report the first missing marker key rather than every one",
     "crates/sankhya-tiering/src/evidence.rs",
     "            .map(|key| (*key).to_string())\n            .collect();",
     "            .map(|key| (*key).to_string())\n            .take(1)\n            .collect();",
     "sankhya-tiering"),

    ("tiering: stop requiring an evidence pack to name its digest",
     "crates/sankhya-tiering/src/evidence.rs",
     "const REQUIRED: [&str; 6] = [\"table\", \"range\", \"archive\", \"snapshot\", \"rows\", \"keys\"];",
     "const REQUIRED: [&str; 5] = [\"table\", \"range\", \"archive\", \"snapshot\", \"rows\"];",
     "sankhya-tiering"),

    ("tiering: truncate an over-long seal key instead of hashing it",
     "crates/sankhya-tiering/src/evidence.rs",
     "    if key.len() > BLOCK {",
     "    if key.len() > BLOCK * 10 {",
     "sankhya-tiering"),

    ("tiering: use the same pad inside and outside the seal",
     "crates/sankhya-tiering/src/evidence.rs",
     "    let mut inner_pad = [0x36u8; BLOCK];",
     "    let mut inner_pad = [0x5cu8; BLOCK];",
     "sankhya-tiering"),

    ("tiering: let a purge reach a destructive phase by skipping to it",
     "crates/sankhya-tiering/src/machine.rs",
     "        if next.requires() != Some(self.phase()) {",
     "        if next.requires() < Some(self.phase()) {",
     "sankhya-tiering"),

    ("clone: treat a table with an unreadable lineage as an ordinary table",
     "crates/sankhya-clone/src/lineage.rs",
     "            return Some(Err(Malformed::NoOrigin));",
     "            return None;",
     "sankhya-clone"),

    ("clone: accept a clone that names no origin version",
     "crates/sankhya-clone/src/lineage.rs",
     "            return Some(Err(Malformed::NoVersion));",
     "            return Some(Ok(Self { origin: origin.clone(), version: 0, cloned_at: 0 }));",
     "sankhya-clone"),

    ("clone: read an unparseable origin version as zero",
     "crates/sankhya-clone/src/lineage.rs",
     "            return Some(Err(Malformed::UnreadableVersion { found: version.clone() }));",
     "            return Some(Ok(Self { origin: origin.clone(), version: 0, cloned_at: 0 }));",
     "sankhya-clone"),

    ("clone: sweep a table without consulting the clones beneath it",
     "crates/sankhya-clone/src/family.rs",
     "                reached.insert(candidate.clone());\n                frontier.push(candidate.clone());",
     "                reached.insert(candidate.clone());",
     "sankhya-clone"),

    ("clone: follow a lineage cycle instead of refusing it",
     "crates/sankhya-clone/src/family.rs",
     "            if !seen.insert(lineage.origin.clone()) {\n                return Err(Cycle { at: lineage.origin.clone() });\n            }",
     "            if false {\n                return Err(Cycle { at: lineage.origin.clone() });\n            }\n            if !seen.insert(lineage.origin.clone()) {\n                return Ok(chain);\n            }",
     "sankhya-clone"),

    ("clone: report a table as its own dependent",
     "crates/sankhya-clone/src/family.rs",
     "        readers.remove(table);",
     "        let _kept = table;",
     "sankhya-clone"),

    ("clone: let a sweep discard the versions a clone still reads",
     "crates/sankhya-clone/src/family.rs",
     "            .filter(|lineage| same_table(&lineage.origin, table))",
     "            .filter(|lineage| !same_table(&lineage.origin, table))",
     "sankhya-clone"),

    ("maintenance: sweep a table without consulting the versions its clones pin",
     "crates/sankhya-maintenance/src/service.rs",
     "        let (pinned, holes) = self.pins(table_root);",
     "        let (pinned, holes) = (BTreeSet::new(), Vec::new());",
     "sankhya-maintenance"),

    ("maintenance: retire an input a clone still reads",
     "crates/sankhya-maintenance/src/execute.rs",
     "        if referenced.cloned.contains(input) {",
     "        if false && referenced.cloned.contains(input) {",
     "sankhya-maintenance"),

    ("maintenance: pin every version of a table rather than the ones clones name",
     "crates/sankhya-maintenance/src/service.rs",
     "            match live_files_at(table_root, version) {",
     "            match live_files(table_root) {",
     "sankhya-maintenance"),

    ("clone: clone a table whose origin is being purged",
     "crates/sankhya-clone/src/refuse.rs",
     "    if origin.purge_in_flight {",
     "    if origin.purge_in_flight && false {",
     "sankhya-clone"),

    ("clone: clone a table belonging to another tenant",
     "crates/sankhya-clone/src/refuse.rs",
     "    if request.tenant != request.origin_tenant {",
     "    if request.tenant == request.origin_tenant && false {",
     "sankhya-clone"),

    ("clone: clone a table whose schema is mid-evolution",
     "crates/sankhya-clone/src/refuse.rs",
     "    if origin.schema_evolving {",
     "    if origin.schema_evolving && false {",
     "sankhya-clone"),

    ("clone: clone a version whose files have been retired",
     "crates/sankhya-clone/src/refuse.rs",
     "    if request.version < origin.earliest_retained_version {",
     "    if request.version < 0_u64 {",
     "sankhya-clone"),

    ("clone: clone a version the origin never had",
     "crates/sankhya-clone/src/refuse.rs",
     "    if request.version > origin.latest_version {",
     "    if request.version > u64::MAX {",
     "sankhya-clone"),

    ("clone: drop an origin its clones still read",
     "crates/sankhya-clone/src/refuse.rs",
     "    if dependents.is_empty() {\n        return Ok(());\n    }",
     "    if !dependents.is_empty() {\n        return Ok(());\n    }",
     "sankhya-clone"),

    ("clone: answer a clone for a moment before it existed",
     "crates/sankhya-clone/src/refuse.rs",
     "    if at < lineage.cloned_at {",
     "    if at < i64::MIN {",
     "sankhya-clone"),

    ("clone: add the origin's files to a clone's own log",
     "crates/sankhya-clone/src/action.rs",
     "    metadata.configuration.extend(lineage.to_properties());",
     "    let _unrecorded = lineage.to_properties();",
     "sankhya-clone"),

    ("clone: claim an ordinary CREATE TABLE as clone DDL",
     "crates/sankhya-clone/src/ddl.rs",
     "    if !words.iter().skip(at).any(|word| word.eq_ignore_ascii_case(\"CLONE\")) {",
     "    if false {",
     "sankhya-clone"),

    ("clone: claim any statement that mentions cloning",
     "crates/sankhya-clone/src/ddl.rs",
     "    if !matches_word(words.get(at), \"CREATE\") {",
     "    if false {",
     "sankhya-clone"),

    ("clone: ignore a clause the clone parser does not understand",
     "crates/sankhya-clone/src/ddl.rs",
     "    if let Some(found) = words.get(at) {\n        return Err(DdlError::Trailing { found: found.clone() });\n    }\n    Ok(Statement::Drop { table, if_exists })",
     "    if let Some(found) = words.get(at) {\n        let _ignored = found;\n    }\n    Ok(Statement::Drop { table, if_exists })",
     "sankhya-clone"),

    ("clone: read a clone with no version as version zero",
     "crates/sankhya-clone/src/ddl.rs",
     "    let mut version = None;",
     "    let mut version = Some(0);",
     "sankhya-clone"),

    ("clone: reserve the word CLONE as a table name",
     "crates/sankhya-clone/src/ddl.rs",
     'fn read(words: &[String], mut at: usize) -> Result<Create, DdlError> {\n    let table = identifier(words.get(at), "a table name")?;',
     'fn read(words: &[String], mut at: usize) -> Result<Create, DdlError> {\n    let table = identifier(words.get(at), "a table name").map(|name| name.to_ascii_lowercase())?;',
     "sankhya-clone"),

    ("server: leave CREATE TABLE ... CLONE unreachable from a client",
     "crates/sankhya-server/src/wiring.rs",
     "        if let Some(statement) = sankhya_clone::parse_ddl(dispatch) {",
     "        if let Some(statement) = None.map(|()| unreachable!()).or(sankhya_clone::parse_ddl(dispatch)).filter(|_| false) {",
     "sankhya-server"),

    ("server: clone a table the principal may not read",
     "crates/sankhya-server/src/wiring.rs",
     "        if !self.readable(principal, &origin, &lineages) {",
     "        if self.readable(principal, &origin, &lineages) && false {",
     "sankhya-server"),

    ("server: clone a version whose data files have been retired",
     "crates/sankhya-server/src/wiring.rs",
     "            Some(version) => sankhya_table_delta::live_files_at(origin_root, version)\n                .map(|at| at.files.iter().all(|file| origin_root.join(&file.path).exists()))\n                .unwrap_or(false),",
     "            Some(_version) => true,",
     "sankhya-server"),

    ("server: drop an origin its clones still read",
     "crates/sankhya-server/src/wiring.rs",
     "        if let Err(refused) = sankhya_clone::refuse::may_drop(table, &lineages) {",
     "        if let Err(refused) = sankhya_clone::refuse::may_drop(table, &Default::default()) {",
     "sankhya-server"),

    ("server: answer a DROP TABLE that is not a clone's",
     "crates/sankhya-server/src/wiring.rs",
     "            // Not a clone. Not ours.\n            return None;",
     "            return Some(Ok(acknowledged(\"DROP TABLE\")));",
     "sankhya-server"),

    ("server: give a clone no authority from what it references",
     "crates/sankhya-server/src/wiring.rs",
     "        let root = chain.last().map_or(table, String::as_str);",
     "        let root = table;",
     "sankhya-server"),

    ("server: clone over a name the warehouse already holds",
     "crates/sankhya-server/src/wiring.rs",
     "        if table_root.exists() {",
     "        if !table_root.exists() && false {",
     "sankhya-server"),

    ("backup: record a backup of a clone whose origin it does not contain",
     "crates/sankhya-backup/src/manifest.rs",
     "        if !orphaned.is_empty() {",
     "        if orphaned.is_empty() && false {",
     "sankhya-backup"),

    ("backup: name only the first clone whose origin is missing",
     "crates/sankhya-backup/src/manifest.rs",
     "                    .then(|| (table.table.clone(), origin.clone()))\n            })\n            .collect();",
     "                    .then(|| (table.table.clone(), origin.clone()))\n            })\n            .take(1)\n            .collect();",
     "sankhya-backup"),

    ("backup: look for a clone's origin among the clones rather than the tables",
     "crates/sankhya-backup/src/manifest.rs",
     "                (!tables.iter().any(|other| other.table == *origin))",
     "                (!tables.iter().any(|other| other.cloned_from.is_some() && other.table == *origin))",
     "sankhya-backup"),

    ("backup: record a clone as an ordinary table",
     "crates/sankhya-backup/src/manifest.rs",
     "            cloned_from: Some(ClonedFrom {\n                origin: origin.into(),\n                version: origin_version,\n            }),",
     "            cloned_from: { let _ = (origin.into(), origin_version); None },",
     "sankhya-backup"),

    ("readpath: read a clone without the rows it inherited",
     "crates/sankhya-readpath/src/provider.rs",
     "                    resolved.extend(\n                        at.files\n                            .into_iter()\n                            .map(|file| (inherited.origin_root.clone(), file)),\n                    );",
     "                    let _uninherited = at.files;",
     "sankhya-readpath"),

    ("readpath: read a clone against the origin as it stands rather than the cloned version",
     "crates/sankhya-readpath/src/provider.rs",
     "                    let at = sankhya_table_delta::live_files_at(\n                        &inherited.origin_root,\n                        inherited.version,\n                    )?;",
     "                    let at = sankhya_table_delta::live_files(&inherited.origin_root)?;",
     "sankhya-readpath"),

    ("readpath: serve a clone whose origin has gone",
     "crates/sankhya-readpath/src/provider.rs",
     "                    if !is_a_table {",
     "                    if is_a_table && false {",
     "sankhya-readpath"),

    ("readpath: resolve an inherited file against the clone's own root",
     "crates/sankhya-readpath/src/provider.rs",
     "                            root.join(&file.path).to_string_lossy().into_owned(),",
     "                            table_root.join(&file.path).to_string_lossy().into_owned(),",
     "sankhya-readpath"),

    ("brake: report a warning when the machine is about to be killed",
     "crates/sankhya-governor/src/memory.rs",
     "    if in_use >= limits.shed_bytes {",
     "    if in_use >= limits.warn_bytes {",
     "sankhya-governor"),

    ("brake: fire one byte late at each threshold",
     "crates/sankhya-governor/src/memory.rs",
     "    if in_use >= limits.warn_bytes {\n        return Pressure::Warning {",
     "    if in_use > limits.warn_bytes {\n        return Pressure::Warning {",
     "sankhya-governor"),

    ("overflow: treat an absent bound as safe",
     "crates/sankhya-stats/src/overflow.rs",
     "    let (Some(Bound::Int(min)), Some(Bound::Int(max))) = (&stats.min, &stats.max) else {\n        return SumRisk::Unknown;\n    };\n\n    // The widest the total can be in either direction. Both ends matter: a column of\n    // large negatives overflows just as readily as one of large positives.",
     "    let (Some(Bound::Int(min)), Some(Bound::Int(max))) = (&stats.min, &stats.max) else {\n        return SumRisk::Safe;\n    };\n",
     "sankhya-stats"),

    ("overflow: check only the maximum, ignoring large negatives",
     "crates/sankhya-stats/src/overflow.rs",
     "    let widest = i128::from(*min)\n        .saturating_mul(rows)\n        .abs()\n        .max(i128::from(*max).saturating_mul(rows).abs());\n\n    if widest <= i128::from(i64::MAX) {",
     "    let widest = i128::from(*max).saturating_mul(rows).abs();\n    let _ = min;\n\n    if widest <= i128::from(i64::MAX) {",
     "sankhya-stats"),

    ("overflow: count nulls as values when estimating",
     "crates/sankhya-stats/src/overflow.rs",
     "    let rows = i128::from(stats.rows.saturating_sub(stats.nulls));\n    if rows == 0 {\n        return SumRisk::Safe;\n    }\n\n    let (Some(Bound::Int(min)), Some(Bound::Int(max))) = (&stats.min, &stats.max) else {\n        return SumRisk::Unknown;\n    };\n\n    // The widest",
     "    let rows = i128::from(stats.rows);\n    if rows == 0 {\n        return SumRisk::Safe;\n    }\n\n    let (Some(Bound::Int(min)), Some(Bound::Int(max))) = (&stats.min, &stats.max) else {\n        return SumRisk::Unknown;\n    };\n\n    // The widest",
     "sankhya-stats"),

    ("overflow: report an unrepresentable precision as safe",
     "crates/sankhya-stats/src/overflow.rs",
     "    let Some(limit) = ten_to(digits) else {\n        return SumRisk::Possible { widest_total: None };\n    };",
     "    let Some(limit) = ten_to(digits) else {\n        return SumRisk::Safe;\n    };",
     "sankhya-stats"),

    ("clustering: sort a partition that is still receiving writes",
     "crates/sankhya-maintenance/src/execute.rs",
     "    let clustering: &[String] = if plan.settled { clustering } else { &[] };",
     "",
     "sankhya-maintenance"),

    ("clustering: merge unsorted when the key names an unknown column",
     "crates/sankhya-table/src/compact.rs",
     "        let column = batch.column_by_name(name).ok_or_else(|| {\n            Error::InvariantViolated(format!(\n                \"the clustering key names {name}, which is not a column of this table; \\\n                 merging unsorted would leave a partition that looks clustered and is \\\n                 not, and nothing downstream could tell\"\n            ))\n        })?;",
     "        let Some(column) = batch.column_by_name(name) else {\n            continue;\n        };",
     "sankhya-maintenance"),

    ("clustering: sort descending instead of ascending",
     "crates/sankhya-table/src/compact.rs",
     "                descending: false,",
     "                descending: true,",
     "sankhya-maintenance"),

    ("clustering: reorder only the first column and leave the rest",
     "crates/sankhya-table/src/compact.rs",
     "        .map(|column| arrow::compute::take(column, &indices, None))",
     "        .enumerate()\n        .map(|(i, column)| {\n            if i == 0 {\n                arrow::compute::take(column, &indices, None)\n            } else {\n                Ok(Arc::clone(column))\n            }\n        })",
     "sankhya-maintenance"),

    ("optimizer: report a cardinality estimate as exact",
     "crates/sankhya-readpath/src/provider.rs",
     "                    Precision::Inexact(usize::try_from(distinct).unwrap_or(usize::MAX))",
     "                    Precision::Exact(usize::try_from(distinct).unwrap_or(usize::MAX))",
     "sankhya-readpath"),

    ("optimizer: describe part of a table as though it were all of it",
     "crates/sankhya-readpath/src/provider.rs",
     "                let Some(stats) = file.stats.get(field.name()) else {\n                    // A file with nothing recorded makes the whole column unknown. Merging\n                    // only the files that happen to have statistics would produce bounds\n                    // that describe part of the table and claim to describe all of it.\n                    return ColumnStatistics::new_unknown();\n                };",
     "                let Some(stats) = file.stats.get(field.name()) else {\n                    continue;\n                };",
     "sankhya-readpath"),

    ("optimizer: hand it a bound it cannot represent exactly",
     "crates/sankhya-readpath/src/provider.rs",
     "        Some(Bound::Float(v)) if v.is_finite() => Precision::Exact(ScalarValue::Float64(Some(*v))),",
     "        Some(Bound::Float(v)) => Precision::Exact(ScalarValue::Float64(Some(*v))),",
     "sankhya-readpath"),

    ("provider: put every file in one group, so the scan uses one core",
     "crates/sankhya-readpath/src/provider.rs",
     "        let groups = partitions.max(1).min(files.len().max(1));",
     "        let groups = 1;",
     "sankhya-readpath"),

    ("provider: enforce the read position when it cannot remove anything",
     "crates/sankhya-readpath/src/provider.rs",
     "    const fn needs_target_filter(&self) -> bool {\n        !self.exact_counts\n    }",
     "    const fn needs_target_filter(&self) -> bool {\n        true\n    }",
     "sankhya-readpath"),

    ("provider: skip the read position when it can remove something",
     "crates/sankhya-readpath/src/provider.rs",
     "    const fn needs_target_filter(&self) -> bool {\n        !self.exact_counts\n    }",
     "    const fn needs_target_filter(&self) -> bool {\n        false\n    }",
     "sankhya-readpath"),

    ("merge: filter deletions before resolving instead of after",
     "crates/sankhya-readpath/src/merge.rs",
     "        let plan = scan\n            .distinct_on(on, select, Some(sort))?\n            // After the distinct, so a tombstone suppresses the row rather than being\n            // removed and letting the previous version win.\n            .filter(col(COMMIT_OP).not_eq(lit(DELETED)))?\n            .build()?;",
     "        let plan = scan\n            .filter(col(COMMIT_OP).not_eq(lit(DELETED)))?\n            .distinct_on(on, select, Some(sort))?\n            .build()?;",
     "sankhya-readpath"),

    ("merge: keep the earliest version of a key instead of the latest",
     "crates/sankhya-readpath/src/merge.rs",
     "        sort.push(col(COMMIT_LSN).sort(false, false));",
     "        sort.push(col(COMMIT_LSN).sort(true, false));",
     "sankhya-readpath"),

    ("merge: accept a key naming a column that does not exist",
     "crates/sankhya-readpath/src/merge.rs",
     "            if schema.index_of(name).is_err() {",
     "            if false {",
     "sankhya-readpath"),

    ("merge: resolve a table that never came through capture",
     "crates/sankhya-readpath/src/merge.rs",
     "            if schema.index_of(required).is_err() {",
     "            if false {",
     "sankhya-readpath"),

    ("merge: serve a raw scan when the planner does not inline the resolution",
     "crates/sankhya-readpath/src/merge.rs",
     "        Err(DataFusionError::Internal(",
     "        return self.raw.scan(_state, _projection, _filters, _limit).await;\n        #[allow(unreachable_code)]\n        Err(DataFusionError::Internal(",
     "sankhya-readpath"),

    ("merge: treat a relation with no row identity as mutable",
     "crates/sankhya-readpath/src/merge.rs",
     "        if key.is_empty() {\n            return Self::AppendOnly;\n        }\n        Self::Mutable { key }",
     "        Self::Mutable { key }",
     "sankhya-readpath"),

    ("readpath: read every offered tier rather than the selected ones",
     "crates/sankhya-readpath/src/lib.rs",
     "    for tier in &splice.tiers {",
     "    for tier in &offered {",
     "sankhya-readpath"),

    ("readpath: drop the target filter from the scan",
     "crates/sankhya-readpath/src/lib.rs",
     'format!("SELECT * FROM {t} WHERE {COMMIT_LSN} <= {}", target.get())',
     'format!("SELECT * FROM {t}")',
     "sankhya-readpath"),

    # Anchored on the line above, because the identical guard appears in `line()` first and
    # an unanchored find patches that one instead -- where it is equivalent, since a fit
    # through one point fails anyway. The entry SURVIVED for exactly that reason, which is
    # the failure mode this catalogue's own header warns about.
    ("diagnostic: project a date from a single observation",
     "crates/sankhya-diagnostic/src/projection.rs",
     """        if self.observations.len() < MINIMUM_OBSERVATIONS {
            return Projection::Unknown {""",
     """        if false {
            return Projection::Unknown {""",
     "sankhya-diagnostic"),

    ("diagnostic: read the direction of concern from the slope",
     "crates/sankhya-diagnostic/src/projection.rs",
     """        let already = match concern {
            Concern::RisingTo => latest.value >= threshold,
            Concern::FallingTo => latest.value <= threshold,
        };""",
     """        let already = match self.line().map_or(true, |fit| fit.slope >= 0.0) {
            true => latest.value >= threshold,
            false => latest.value <= threshold,
        };""",
     "sankhya-diagnostic"),

    ("diagnostic: project a date through a sawtooth",
     "crates/sankhya-diagnostic/src/projection.rs",
     "        if straightness < LINEAR_ENOUGH && self.observations.len() > MINIMUM_OBSERVATIONS {",
     "        if false {",
     "sankhya-diagnostic"),

    ("diagnostic: ask the direction before the fit, so a sawtooth reads as receding",
     "crates/sankhya-diagnostic/src/projection.rs",
     "        const LINEAR_ENOUGH: f64 = 0.80;",
     "        const LINEAR_ENOUGH: f64 = 0.0;",
     "sankhya-diagnostic"),

    ("diagnostic: extrapolate arbitrarily far past the observed window",
     "crates/sankhya-diagnostic/src/projection.rs",
     "        if seconds > horizon_seconds {",
     "        if false {",
     "sankhya-diagnostic"),

    ("diagnostic: sort findings by severity rather than by when",
     "crates/sankhya-diagnostic/src/check.rs",
     "        self.findings.sort_by_key(Finding::urgency);",
     "        self.findings.sort_by(|a, b| b.severity.cmp(&a.severity));",
     "sankhya-diagnostic"),

    ("diagnostic: go silent when near the line with no rate yet",
     "crates/sankhya-diagnostic/src/check.rs",
     "    if !matches!(projection, Projection::Unknown { .. }) {\n        return false;\n    }",
     "    if true {\n        return false;\n    }",
     "sankhya-diagnostic"),

    ("diagnostic: count a table that could not be read as a clean one",
     "crates/sankhya-diagnostic/src/collect.rs",
     'Err(why) => report.skipped(COMPACTION_DEBT, format!("table {}: {why}", table.name)),',
     "Err(_) => report.clean(COMPACTION_DEBT),",
     "sankhya-diagnostic"),

    ("diagnostic: report before recording, so every date is one run stale",
     "crates/sankhya-diagnostic/src/collect.rs",
     "                let observation = Observation::new(now, files as f64);",
     "                let observation = Observation::new(now, f64::from(0u8));",
     "sankhya-diagnostic"),

    ("diagnostic: accept a NaN from the history file",
     "crates/sankhya-diagnostic/src/history.rs",
     "    if !value.is_finite() {\n        return None;\n    }",
     "    if false {\n        return None;\n    }",
     "sankhya-diagnostic"),

    ("diagnostic: let a tab in a subject forge a field",
     "crates/sankhya-diagnostic/src/history.rs",
     "        measure.subject.replace('\\t', \" \"),",
     "        measure.subject,",
     "sankhya-diagnostic"),

    ("diagnostic: treat a damaged history line as if it had parsed",
     "crates/sankhya-diagnostic/src/history.rs",
     "                None => history.damaged_lines += 1,",
     "                None => {}",
     "sankhya-diagnostic"),

    # `R²` is `0/0` for a constant response and is now reported as undefined. The defect worth
    # pinning is no longer "which of the two numbers" --- it is answering with a number at all,
    # because that is what made `vec_regression_r2` and `regress_r2` disagree on one surface.
    ("math: answer an undefined R-squared with a number",
     "crates/sankhya-math/src/stats.rs",
     "        Err(VectorError::ZeroMagnitude) => None,",
     "        Err(VectorError::ZeroMagnitude) => Some(1.0),",
     "sankhya-math"),
    ("metrics: let a closed label accept any value",
     "crates/sankhya-metrics/src/metric.rs",
     "            Values::Closed(allowed) => allowed.contains(&value),",
     "            Values::Closed(_) => true,",
     "sankhya-metrics"),

    ("metrics: stop requiring every declared label to be supplied",
     "crates/sankhya-metrics/src/registry.rs",
     "        if labels.len() != metric.labels.len() {",
     "        if false {",
     "sankhya-metrics"),

    ("metrics: let an identifier label grow without bound",
     "crates/sankhya-metrics/src/registry.rs",
     "        if seen.len() >= cap {",
     "        if false {",
     "sankhya-metrics"),

    ("metrics: share one cardinality budget across every metric",
     "crates/sankhya-metrics/src/registry.rs",
     '            .entry((metric.name, declared.name))',
     '            .entry(("", declared.name))',
     "sankhya-metrics"),

    ("metrics: drop the +Inf bucket from a histogram",
     "crates/sankhya-metrics/src/registry.rs",
     '            let with_inf = render_labels(labels, Some("+Inf"));',
     '            let with_inf = render_labels(labels, Some("999999"));',
     "sankhya-metrics"),

    ("metrics: stop escaping label values, so one table name breaks the scrape",
     "crates/sankhya-metrics/src/registry.rs",
     '        .map(|(name, value)| format!("{name}=\\"{}\\"", escape(value)))',
     '        .map(|(name, value)| format!("{name}=\\"{value}\\""))',
     "sankhya-metrics"),

    ("metrics: omit a metric that has recorded nothing",
     "crates/sankhya-metrics/src/registry.rs",
     "            for labels in zero_combinations(metric) {\n                if !recorded.contains(&labels) {",
     "            for labels in Vec::<Vec<(String, String)>>::new() {\n                if !recorded.contains(&labels) {",
     "sankhya-metrics"),

    ("metrics: drop a closed label's other values once one of them is recorded",
     "crates/sankhya-metrics/src/registry.rs",
     "            for labels in zero_combinations(metric) {",
     "            for labels in if series.is_empty() { zero_combinations(metric) } else { Vec::new() } {",
     "sankhya-metrics"),
    ("metrics: emit a zero series for a label whose values are discovered",
     "crates/sankhya-metrics/src/registry.rs",
     "        let Values::Closed(values) = label.values else {\n            return Vec::new();\n        };",
     "        let Values::Closed(values) = label.values else {\n            continue;\n        };",
     "sankhya-metrics"),

    ("server: count a refusal as an error",
     "crates/sankhya-server/src/wiring.rs",
     '            state if state.starts_with("53") || state.starts_with("28") => "refused",',
     '            state if state.starts_with("53") || state.starts_with("28") => "error",',
     "sankhya-server"),

    ("server: time only the queries that succeeded",
     "crates/sankhya-server/src/wiring.rs",
     '        self.metrics.observe(\n            &catalogue::QUERY_DURATION_SECONDS,\n            &[("outcome", label)],\n            started.elapsed().as_secs_f64(),\n        );',
     '        if outcome.is_ok() {\n            self.metrics.observe(\n                &catalogue::QUERY_DURATION_SECONDS,\n                &[("outcome", label)],\n                started.elapsed().as_secs_f64(),\n            );\n        }',
     "sankhya-server"),

    ("server: decrement the connection gauge only on a clean exit",
     "crates/sankhya-api-pg/src/listener.rs",
     "    let _guard = ConnectionGuard(Arc::clone(&handler));",
     "    let _guard = ();",
     "sankhya-server"),

    ("server: accept a write and discard it",
     "crates/sankhya-server/src/execute.rs",
     "    refuse_if_not_a_read(&plan)?;",
     "    let _ = refuse_if_not_a_read(&plan);",
     "sankhya-server"),

    ("server: check the statement shape after the engine has already run the DDL",
     "crates/sankhya-server/src/execute.rs",
     "    let plan = context\n        .state()\n        .create_logical_plan(sql)\n        .await\n        .map_err(|error| plan_failure(&error))?;\n    refuse_if_not_a_read(&plan)?;",
     "    let frame = context.sql(sql).await.map_err(|error| plan_failure(&error))?;\n    refuse_if_not_a_read(frame.logical_plan())?;\n    let plan = frame.logical_plan().clone();",
     "sankhya-server"),

    ("server: stop unwrapping the engine's diagnostic wrapper",
     "crates/sankhya-server/src/execute.rs",
     "        E::Diagnostic(_, inner) | E::Context(_, inner) => classify(inner),",
     "        E::Context(_, inner) => classify(inner),",
     "sankhya-server"),

    ("server: send the engine's message with no catalogue code",
     "crates/sankhya-server/src/execute.rs",
     "            if said.contains(&format!(\"[{code}]\")) {",
     "            if true {",
     "sankhya-server"),

    ("server: drop the remediation before it reaches the client",
     "crates/sankhya-server/src/execute.rs",
     "        detail: Some(classified.remediation().to_string()),",
     "        detail: None,",
     "sankhya-server"),

    ("server: serve any path that starts with /metrics",
     "crates/sankhya-server/src/scrape.rs",
     '    path == "/metrics"',
     '    path.starts_with("/metrics")',
     "sankhya-server"),

    ("backup: record a manifest whose tables are ahead of the source",
     "crates/sankhya-backup/src/manifest.rs",
     "        if !ahead.is_empty() {",
     "        if false {",
     "sankhya-backup"),

    ("backup: report only the first table that is ahead",
     "crates/sankhya-backup/src/manifest.rs",
     "            .filter(|table| table.covers_to > source.restores_to)",
     "            .filter(|table| table.covers_to > source.restores_to)\n            .take(1)",
     "sankhya-backup"),

    ("backup: take the queryable position from the source rather than the slowest table",
     "crates/sankhya-backup/src/manifest.rs",
     "        let queryable_at = tables\n            .iter()\n            .map(|table| table.covers_to)\n            .min()\n            .unwrap_or(Lsn::new(0));",
     "        let queryable_at = source.restores_to;",
     "sankhya-backup"),

    ("backup: record a backup of no tables",
     "crates/sankhya-backup/src/manifest.rs",
     "        if tables.is_empty() {\n            return Err(InconsistentBackup::NoTables);\n        }",
     "",
     "sankhya-backup"),

    ("backup: leave the tables in the order they arrived",
     "crates/sankhya-backup/src/manifest.rs",
     "        tables.sort_by(|a, b| a.table.cmp(&b.table));",
     "",
     "sankhya-backup"),

    ("backup: write the checksum as a number and lose its low bits",
     "crates/sankhya-backup/src/manifest.rs",
     "            checksum: digest.checksum().to_string(),",
     "            checksum: (digest.checksum() as f64).to_string(),",
     "sankhya-backup"),

    ("backup: read an unparseable checksum as zero",
     "crates/sankhya-backup/src/manifest.rs",
     "        let checksum: u128 = self.checksum.parse().ok()?;",
     "        let checksum: u128 = self.checksum.parse().unwrap_or(0);",
     "sankhya-backup"),

    ("backup: release a deleted backup's files immediately",
     "crates/sankhya-backup/src/protect.rs",
     "        if now < expired.saturating_add(GRACE_MICROS) {\n            return false;\n        }",
     "",
     "sankhya-backup"),

    ("backup: restart the grace period on every expiry call",
     "crates/sankhya-backup/src/protect.rs",
     "        let entry = self.expired_at.entry(backup).or_insert(now);",
     "        let entry = self.expired_at.entry(backup).and_modify(|at| *at = now).or_insert(now);",
     "sankhya-backup"),

    ("backup: release a backup past its own horizon without a grace period",
     "crates/sankhya-backup/src/protect.rs",
     "            Some(until) if now >= *until => Standing::Grace {\n                until: until.saturating_add(GRACE_MICROS),\n            },",
     "            Some(until) if now >= *until => Standing::Released,",
     "sankhya-backup"),

    ("backup: call a drill over no tables a pass",
     "crates/sankhya-backup/src/drill.rs",
     "        self.could_not_start.is_none()\n            && !self.tables.is_empty()",
     "        self.could_not_start.is_none()",
     "sankhya-backup"),

    ("backup: treat a drill that could not start as a pass",
     "crates/sankhya-backup/src/drill.rs",
     "        let verdict = if self.could_not_start.is_some() {\n            \"could-not-start\"\n        } else if self.passed() {",
     "        let verdict = if self.passed() {",
     "sankhya-backup"),

    ("backup: stop at the first table that fails to verify",
     "crates/sankhya-backup/src/drill.rs",
     "        tables.push((snapshot.table.clone(), outcome));",
     "        let stop = !outcome.is_verified();\n        tables.push((snapshot.table.clone(), outcome));\n        if stop {\n            break;\n        }",
     "sankhya-backup"),

    ("backup: report the last drill attempt rather than the last pass",
     "crates/sankhya-backup/src/drill.rs",
     '        .filter(|line| line.contains("\\"verdict\\": \\"pass\\""))',
     "",
     "sankhya-backup"),

    ("backup: compare only the row count and not the checksum",
     "crates/sankhya-backup/src/drill.rs",
     "                Ok(found) if found == expected => TableOutcome::Verified {",
     "                Ok(found) if found.rows() == expected.rows() => TableOutcome::Verified {",
     "sankhya-backup"),

    ("backup: digest a null and an empty string identically",
     "crates/sankhya-backup/src/warehouse.rs",
     "                if batch.column(index).is_null(row) {\n                    None\n                } else {\n                    Some(text.as_str())\n                }",
     "                Some(text.as_str())",
     "sankhya-backup"),

    # Named against `sankhya-backup` rather than the crate the code lives in: time travel
    # has no test of its own in the log crate, and the test that actually notices is the one
    # asserting a backup still verifies after the table moves on. An entry pointed at the
    # wrong crate reports SURVIVED while the defect is caught, which trains you to read
    # survivors as noise.
    ("delta: replay past the requested version during time travel",
     "crates/sankhya-table-delta/src/log.rs",
     "        if at > version {\n            break;\n        }",
     "",
     "sankhya-backup"),

    ("diagnostic: treat a never-proven backup as merely approaching its objective",
     "crates/sankhya-diagnostic/src/check.rs",
     'docs/runbooks/restore-drill.md."\n        .to_string();\n    let Some(last) = last_pass else {',
     'docs/runbooks/restore-drill.md."\n        .to_string();\n    let Some(last) = last_pass.or(Some(now)) else {',
     "sankhya-diagnostic"),

    ("server: print the configured address rather than the one actually bound",
     "crates/sankhya-server/src/main.rs",
     "    let bound = listener\n        .local_addr()\n        .map_or_else(|_| settings_listen.clone(), |address| address.to_string());",
     "    let bound = settings_listen.clone();",
     "sankhya-server"),

    # This one hung the test rather than failing it, the first time it was run: the banner
    # was read with no deadline, so a server that printed nothing blocked forever and took
    # the build with it. A test that hangs is strictly worse than one that fails, because a
    # failure names what broke. The entry stays because it is the only thing that proved it.
    ("server: stop announcing the port at all",
     "crates/sankhya-server/src/main.rs",
     '    println!("  listening on {bound}");',
     "",
     "sankhya-server"),

    ("listener: return from shutdown without waiting for connections in flight",
     "crates/sankhya-api-pg/src/listener.rs",
     "        let drained = tokio::time::timeout(drain, async {\n            while connections.join_next().await.is_some() {}\n        })\n        .await;",
     "        let drained: Result<(), ()> = Ok(());",
     "sankhya-api-pg"),

    ("listener: drop the drain deadline and wait forever",
     "crates/sankhya-api-pg/src/listener.rs",
     "        let drained = tokio::time::timeout(drain, async {",
     "        let drained = tokio::time::timeout(Duration::from_secs(86_400), async {",
     "sankhya-api-pg"),

    ("packaging: give the orchestrator less grace than the server needs to drain",
     "packaging/kubernetes/deployment.yaml",
     "      terminationGracePeriodSeconds: 45",
     "      terminationGracePeriodSeconds: 20",
     "xtask"),

    ("package: compare glibc versions as strings",
     "xtask/src/package.rs",
     "        .filter_map(|token| token.strip_prefix(\"GLIBC_\"))\n        .filter_map(|version| {\n            let mut parts = version.split('.');\n            let major = parts.next()?.parse::<u32>().ok()?;\n            let minor = parts.next().unwrap_or(\"0\").parse::<u32>().ok()?;\n            Some((major, minor))\n        })\n        .max()",
     "        .filter_map(|token| token.strip_prefix(\"GLIBC_\"))\n        .max()\n        .and_then(|version| {\n            let mut parts = version.split('.');\n            let major = parts.next()?.parse::<u32>().ok()?;\n            let minor = parts.next().unwrap_or(\"0\").parse::<u32>().ok()?;\n            Some((major, minor))\n        })",
     "xtask"),

    ("package: match the whole symbol rather than the part after the @",
     "xtask/src/package.rs",
     "        .filter_map(|token| token.rsplit('@').next())\n",
     "",
     "xtask"),

    ("package: report only the first way an artifact misses its baseline",
     "xtask/src/package.rs",
     "    for object in &requires.shared_objects {",
     "    for object in requires.shared_objects.iter().take(0) {",
     "xtask"),

    ("version: read an artefact from a newer release instead of refusing it",
     "crates/sankhya-version/src/lib.rs",
     "        if found > self.current {",
     "        if false {",
     "sankhya-version"),

    ("version: read an artefact older than the supported floor",
     "crates/sankhya-version/src/lib.rs",
     "        if found < self.oldest_readable {",
     "        if false {",
     "sankhya-version"),

    ("version: write back an older format instead of degrading to read-only",
     "crates/sankhya-version/src/lib.rs",
     "        if found < self.current {",
     "        if false {",
     "sankhya-version"),

    ("version: call a one-way upgrade reversible",
     "crates/sankhya-version/src/lib.rs",
     "        !matches!(self.rollback, Rollback::OneWay { .. })",
     "        true",
     "sankhya-version"),

    ("backup: parse the whole manifest before looking at its version",
     "crates/sankhya-backup/src/manifest.rs",
     "        let stamped: Stamp = serde_json::from_str(text)",
     "        let stamped: Manifest = serde_json::from_str(text)",
     "sankhya-backup"),

    ("backup: report a manifest from the future as damage",
     "crates/sankhya-backup/src/manifest.rs",
     "            Compatibility::Refused { why } => Err(UnreadableManifest::FromTheFuture(why)),",
     "            Compatibility::Refused { why } => Err(UnreadableManifest::Malformed(why)),",
     "sankhya-backup"),

    ("backup: treat an unstamped manifest as format zero rather than the original",
     "crates/sankhya-backup/src/manifest.rs",
     "const fn one() -> u32 {\n    1\n}",
     "const fn one() -> u32 {\n    0\n}",
     "sankhya-backup"),

    # The whole guard, not just its condition. `if false` on an `if let` leaves the binding
    # unused and the mutation does not compile -- and a mutation that does not compile tests
    # nothing while looking in the catalogue exactly like one that does.
    ("diagnostic: read a history from a newer release anyway",
     "crates/sankhya-diagnostic/src/history.rs",
     """                    if let Compatibility::Refused { why } = DIAGNOSTIC_HISTORY.admits(found) {
                        return Err(HistoryError::FromTheFuture {
                            path: path.clone(),
                            why,
                        });
                    }""",
     "",
     "sankhya-diagnostic"),

    ("diagnostic: write the format header onto every append",
     "crates/sankhya-diagnostic/src/history.rs",
     "        let fresh = !path.exists();",
     "        let fresh = true;",
     "sankhya-diagnostic"),

    ("diagnostic: drop the format header when compacting",
     "crates/sankhya-diagnostic/src/history.rs",
     '        let mut buffer = format!("{HISTORY_HEADER_PREFIX}{}\\n", DIAGNOSTIC_HISTORY.current);',
     "        let mut buffer = String::new();",
     "sankhya-diagnostic"),

    ("diagnostic: count a comment line as damage",
     "crates/sankhya-diagnostic/src/history.rs",
     "                if text.starts_with('#') {\n                    continue;\n                }",
     "",
     "sankhya-diagnostic"),

    ("soak: extrapolate past what the run observed",
     "crates/sankhya-diagnostic/src/soak/judge.rs",
     "    if horizon > supported {",
     "    if false {",
     "sankhya-diagnostic"),

    ("soak: judge from a handful of samples",
     "crates/sankhya-diagnostic/src/soak/judge.rs",
     "    if samples.len() < fewest {",
     "    if false {",
     "sankhya-diagnostic"),

    ("soak: call an unjudgeable measure steady",
     "crates/sankhya-diagnostic/src/soak/report.rs",
     "        !self.verdicts.is_empty() && self.verdicts.iter().all(|(_, verdict)| verdict.passed())",
     "        self.verdicts.iter().all(|(_, verdict)| !matches!(verdict, Verdict::Growing { .. }))",
     "sankhya-diagnostic"),

    ("soak: skip the warm-up exclusion",
     "crates/sankhya-diagnostic/src/soak/report.rs",
     "    samples.get(WARM_UP_SAMPLES..).unwrap_or(&[])",
     "    samples",
     "sankhya-diagnostic"),

    ("soak: trend a sawtooth's raw samples rather than its peaks",
     "crates/sankhya-diagnostic/src/soak/judge.rs",
     "            let peaks = peaks_of(samples, PEAK_WINDOWS);",
     "            let peaks: Vec<Observation> = samples.to_vec();",
     "sankhya-diagnostic"),

    ("soak: take a sawtooth's entitlement from its peaks rather than from the run",
     "crates/sankhya-diagnostic/src/soak/judge.rs",
     "rising_to_at_least(measure, &peaks, limit, horizon, now, FEWEST_PEAKS, span_of(samples))",
     "rising_to_at_least(measure, &peaks, limit, horizon, now, FEWEST_PEAKS, span_of(&peaks))",
     "sankhya-diagnostic"),

    ("soak: count an empty peak window as zero",
     "crates/sankhya-diagnostic/src/soak/judge.rs",
     "        else {\n            continue;\n        };\n        peaks.push(Observation::new(peak.at, peak.value));",
     "        else {\n            peaks.push(Observation::new(start, 0.0));\n            continue;\n        };\n        peaks.push(Observation::new(peak.at, peak.value));",
     "sankhya-diagnostic"),

    ("soak: judge a per-unit-of-work measure by its total",
     "crates/sankhya-diagnostic/src/soak/judge.rs",
     "            let Some(ratios) = ratio(samples, reference) else {",
     "            let Some(ratios) = Some(samples.to_vec()) else {",
     "sankhya-diagnostic"),

    ("soak: record a reading that could not be taken as zero",
     "crates/sankhya-diagnostic/src/soak/sample.rs",
     "            None => *self.missed.entry(measure.to_string()).or_default() += 1,",
     "            None => self\n                .taken\n                .entry(measure.to_string())\n                .or_default()\n                .push(Observation::new(at, 0.0)),",
     "sankhya-diagnostic"),

    ("rest: encode a large result as JSON instead of handing back a ticket",
     "crates/sankhya-api-rest/src/size.rs",
     "    if estimate.rows > MAX_ROWS {",
     "    if false {",
     "sankhya-api-rest"),

    ("rest: ignore the byte estimate and judge only by row count",
     "crates/sankhya-api-rest/src/size.rs",
     "    if estimate.bytes as usize > MAX_BYTES {",
     "    if false {",
     "sankhya-api-rest"),

    ("rest: keep encoding after the response outgrew the cap",
     "crates/sankhya-api-rest/src/size.rs",
     "        if self.bytes > MAX_BYTES || self.rows > MAX_ROWS {\n            self.exceeded = true;\n            return false;\n        }",
     "",
     "sankhya-api-rest"),

    ("rest: let a small row reopen a budget that has already refused",
     "crates/sankhya-api-rest/src/size.rs",
     "        if self.exceeded {\n            return false;\n        }",
     "",
     "sankhya-api-rest"),

    ("rest: truncate an overrun response rather than abandoning it",
     "crates/sankhya-api-rest/src/size.rs",
     "        self.exceeded.then(|| {",
     "        false.then(|| {",
     "sankhya-api-rest"),

    ("rest: echo the whole statement into a message",
     "crates/sankhya-api-rest/src/size.rs",
     "    if statement.chars().count() <= KEEP {",
     "    if true {",
     "sankhya-api-rest"),

    ("rest: match routes by prefix",
     "crates/sankhya-api-rest/src/plane.rs",
     "        .find(|route| route.method == method && route.path == bare)",
     "        .find(|route| route.method == method && bare.starts_with(route.path))",
     "sankhya-api-rest"),

    ("rest: ignore the method when matching a route",
     "crates/sankhya-api-rest/src/plane.rs",
     "        .find(|route| route.method == method && route.path == bare)",
     "        .find(|route| route.path == bare)",
     "sankhya-api-rest"),

    ("rest: require a credential on the liveness probe",
     "crates/sankhya-api-rest/src/plane.rs",
     '        path: "/health",\n        shape: Shape::Document,\n        authenticated: false,',
     '        path: "/health",\n        shape: Shape::Document,\n        authenticated: true,',
     "sankhya-api-rest"),

    ("logging: stop catching an interpolated statement",
     "xtask/src/logging.rs",
     "    if !MACROS.iter().any(|macro_name| line.contains(macro_name)) {",
     "    if true {",
     "xtask"),

    ("logging: allow #[instrument] to record every argument",
     "xtask/src/logging.rs",
     "    !trimmed.contains(\"skip_all\") && !trimmed.contains(\"skip(\")",
     "    false",
     "xtask"),

    ("logging: ignore the word boundary after a forbidden field name",
     "xtask/src/logging.rs",
     "        let after_ok = !haystack",
     "        let after_ok = true || !haystack",
     "xtask"),

    # Counting a shared member once per path cannot be expressed here: `consolidates`
    # returns a `BTreeSet`, so the duplicate is unrepresentable rather than merely avoided —
    # the same shape as an undeclared metric. Changing the return type does not compile,
    # which is the type doing its job. So the mutation targets the other way this goes wrong:
    # consolidating over every descendant rather than over the leaves, which counts each
    # internal node as well as the grain beneath it.
    ("cube: consolidate over every descendant rather than over the leaves",
     "crates/sankhya-cube-algo/src/hierarchy.rs",
     "        let children = self.children_of(member);\n        if children.is_empty() {\n            reached.insert(member);\n            return Ok(());\n        }",
     "        let children = self.children_of(member);\n        reached.insert(member);\n        if children.is_empty() {\n            return Ok(());\n        }",
     "sankhya-cube-algo"),

    ("cube: stop detecting cycles during consolidation",
     "crates/sankhya-cube-algo/src/hierarchy.rs",
     "        if on_path.contains(&member) {",
     "        if false {",
     "sankhya-cube-algo"),

    ("cube: skip the members of a hierarchy that has no roots",
     "crates/sankhya-cube-algo/src/hierarchy.rs",
     "        if self.roots().is_empty() && !self.members().is_empty() {",
     "        if false {",
     "sankhya-cube-algo"),

    ("cube: default an undeclared aggregation rule to summation",
     "crates/sankhya-cube-algo/src/measure.rs",
     "            .map(|along| along.rule)",
     "            .map(|along| along.rule)\n            .or(Some(Rule::Sum))",
     "sankhya-cube-algo"),

    ("cube: report only the first dimension a measure fails to declare",
     "crates/sankhya-cube-algo/src/measure.rs",
     "        let missing: Vec<String> = dimensions",
     "        let missing: Vec<String> = dimensions\n            .iter()\n            .take(1)\n            .copied()\n            .collect::<Vec<_>>()",
     "sankhya-cube-algo"),

    ("cube: let a mean compose, so an average of averages is an average",
     "crates/sankhya-cube-algo/src/measure.rs",
     "        matches!(self, Self::Sum | Self::Last | Self::First | Self::Max | Self::Min)",
     "        !matches!(self, Self::None)",
     "sankhya-cube-algo"),

    ("cube: answer a query from a cuboid that lacks a dimension it needs",
     "crates/sankhya-cube-algo/src/ancestor.rs",
     "    if query.iter().any(|wanted| !materialised.contains(wanted)) {\n        return None;\n    }",
     "",
     "sankhya-cube-algo"),

    ("cube: treat an undeclared axis as a refusal rather than a definition error",
     "crates/sankhya-cube-algo/src/ancestor.rs",
     "            None => {\n                return Answerable::Undeclared {",
     "            None => {\n                #[allow(unused)]\n                return Answerable::No {\n                    measure: measure.name.to_string(),\n                    dimension: (*dimension).to_string(),\n                    rule: Rule::None,\n                };\n                #[allow(unreachable_code)]\n                return Answerable::Undeclared {",
     "sankhya-cube-algo"),

    ("cube: count benefit without asking whether the measure permits the roll-up",
     "crates/sankhya-cube-algo/src/lattice.rs",
     "        if !candidate.answers(query, measure) {\n            continue;\n        }",
     "        if rolled_away(&query.dimensions(), &candidate.dimensions()).is_none() {\n            continue;\n        }",
     "sankhya-cube-algo"),

    ("cube: credit every candidate with the full saving, ignoring what is already held",
     "crates/sankhya-cube-algo/src/lattice.rs",
     "        let current = already\n            .iter()\n            .map(|c| &c.cuboid)\n            .chain(std::iter::once(base))",
     "        let current = []\n            .iter()\n            .map(|c: &Chosen| &c.cuboid)\n            .chain(std::iter::once(base))",
     "sankhya-cube-algo"),

    ("cube: select past the budget",
     "crates/sankhya-cube-algo/src/lattice.rs",
     "            if spent.saturating_add(price) > budget_rows {\n                continue;\n            }",
     "",
     "sankhya-cube-algo"),

    ("cube: buy a cuboid that gains nothing",
     "crates/sankhya-cube-algo/src/lattice.rs",
     "            if gain == 0 {\n                continue;\n            }",
     "",
     "sankhya-cube-algo"),

    ("cube: compare benefit per row with integer division",
     "crates/sankhya-cube-algo/src/lattice.rs",
     "    u128::from(benefit) * u128::from(than_cost.max(1))\n        > u128::from(than_benefit) * u128::from(cost.max(1))",
     "    benefit / cost.max(1) > than_benefit / than_cost.max(1)",
     "sankhya-cube-algo"),

    ("cube: treat two cuboids naming the same dimensions as different",
     "crates/sankhya-cube-algo/src/lattice.rs",
     "        let unique: BTreeSet<String> = dimensions",
     "        let unique: Vec<String> = dimensions",
     "sankhya-cube-algo"),

    # --- the cube definition: the refusal that stops wrong numbers ---------

    ("cube: default an undeclared measure to summation instead of refusing",
     "crates/sankhya-cube/src/validate.rs",
     "        if let Err(undeclared) = measure.covers(&declared) {",
     "        if let Err(undeclared) = Ok::<(), sankhya_cube_algo::Undeclared>(()) {",
     "sankhya-cube"),

    ("cube: report only the first rejection, so a fix takes one build each",
     "crates/sankhya-cube/src/validate.rs",
     "    out.sort();\n    out.dedup();\n    out\n}",
     "    out.sort();\n    out.dedup();\n    out.into_iter().take(1).collect()\n}",
     "sankhya-cube"),

    ("cube: accept a cyclic hierarchy at definition time",
     "crates/sankhya-cube/src/validate.rs",
     "            if let Err(Cyclic { cycle }) = hierarchy.validate() {",
     "            if let Err(Cyclic { cycle }) = Ok::<(), Cyclic>(()) {",
     "sankhya-cube"),

    ("cube: let a validated cube be built from a rejected definition",
     "crates/sankhya-cube/src/model.rs",
     "        if !rejections.is_empty() {",
     "        if rejections.is_empty() && !rejections.is_empty() {",
     "sankhya-cube"),

    ("cube: ignore a measure's aggregation rules when fingerprinting the definition",
     "crates/sankhya-cube/src/version.rs",
     "            feed(&mut h, rule.rule.as_str().as_bytes());",
     "",
     "sankhya-cube"),

    ("cube: fingerprint fields without a length prefix, so two cubes share a key",
     "crates/sankhya-cube/src/version.rs",
     "    for byte in (bytes.len() as u64).to_le_bytes() {\n        *h = (*h ^ u64::from(byte)).wrapping_mul(PRIME);\n    }",
     "",
     "sankhya-cube"),

    ("cube: fingerprint levels as a set, losing the drill-down order",
     "crates/sankhya-cube/src/version.rs",
     "        for level in &dimension.levels {",
     "        for level in { let mut s: Vec<&crate::model::Level> = dimension.levels.iter().collect(); s.sort_by(|a, b| a.name.cmp(&b.name)); s } {",
     "sankhya-cube"),

    ("cube: skip the stray-rule check, so a misspelling reads as one problem",
     "crates/sankhya-cube/src/validate.rs",
     "            if !names.contains(rule.dimension.as_str()) {",
     "            if false {",
     "sankhya-cube"),

    ("cube: store a materialised cell rounded, so the fast path drifts from the slow one",
     "crates/sankhya-cube/src/store.rs",
     "        for component in stored.components() {\n            exact.values().append_value(*component);\n        }",
     "        exact.values().append_value(stored.to_f64());",
     "sankhya-cube"),

    ("xtask: pass a SQL surface no server can reach",
     "xtask/src/surfaces.rs",
     "        if served.contains(crate_name) {\n            continue;\n        }",
     "        if true {\n            continue;\n        }",
     "xtask"),

    ("xtask: count a dev-dependency as reaching a surface",
     "xtask/src/surfaces.rs",
     "            if line.trim_start().starts_with(\"[dev-dependencies]\") {\n                break;\n            }",
     "            if false {\n                break;\n            }",
     "xtask"),

    # --- the guide's examples, and whether they actually work ------------------

    ("server: let a refused guide example pass as a query that matched nothing",
     "crates/sankhya-server/tests/common/mod.rs",
     "    read_until_ready(&mut stream, &mut buffer);\n\n    if count_tags(&buffer, b'E') > 0 {",
     "    read_until_ready(&mut stream, &mut buffer);\n\n    if false {",
     "sankhya-server"),

    ("server: leave the analytical functions unregistered, so the guide documents nothing",
     "crates/sankhya-server/src/execute.rs",
     "    sankhya_olap::register_constructors(&context);",
     "    // sankhya_olap::register_constructors(&context);",
     "sankhya-server"),

    # --- cuboids nothing can ask for, and nothing was collecting ---------------

    ("maintenance: delete a directory the cuboid sweep cannot recognise",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "        let Some((cube, key)) = sankhya_cube::materialise::parse(&name) else {",
     "        let Some((cube, key)) = sankhya_cube::materialise::parse(&name).or_else(|| Some((String::new(), sankhya_cube::materialise::Key::unrestricted(0, 0, sankhya_cube::algo::Cuboid::of::<&str>(&[]))))) else {",
     "sankhya-maintenance"),

    ("maintenance: collect a cuboid whose cube has no known version, on a guess",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "        let Some(&version) = current.get(&cube) else {",
     "        let Some(&version) = current.get(&cube).or(Some(&u64::MAX)) else {",
     "sankhya-maintenance"),

    ("maintenance: collect a cuboid that is still within the tolerated drift",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "        if drift <= behind {\n            continue;\n        }",
     "        if false {\n            continue;\n        }",
     "sankhya-maintenance"),

    ("server: never sweep, so superseded cuboids accumulate for the life of the warehouse",
     "crates/sankhya-server/src/wiring.rs",
     "        self.retire_superseded_cuboids();",
     "        // self.retire_superseded_cuboids();",
     "sankhya-server"),

    ("server: refresh a cube nobody marked maintained, charging storage nobody asked for",
     "crates/sankhya-server/src/wiring.rs",
     "            let Some(_) = cube.target_lag() else {\n                // Declared, not maintained. Nothing to build, and building it anyway would\n                // charge an operator storage they did not ask for.\n                continue;\n            };",
     "            let _ = cube.target_lag();",
     "sankhya-server"),

    ("server: build a restricted cuboid from a refresh that has no principal",
     "crates/sankhya-server/src/wiring.rs",
     "                let key = sankhya_cube::materialise::Key::unrestricted(\n                    cube.version(),\n                    snapshot,",
     "                let key = sankhya_cube::materialise::Key::new(\n                    cube.version(),\n                    snapshot,\n                    0xdead_beef,",
     "sankhya-server"),

    # --- target_lag: a staleness target, not a schedule ------------------------

    ("maintenance: treat a cube that materialises nothing as always fresh",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "        Some(target) => lag <= target,\n        None => false,",
     "        Some(target) => lag <= target,\n        None => true,",
     "sankhya-maintenance"),

    ("maintenance: refresh a cube nobody asked to materialise",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "        Some(target) => lag > target,\n        None => false,",
     "        Some(target) => lag > target,\n        None => true,",
     "sankhya-maintenance"),

    ("maintenance: let a cuboid ahead of its table report an enormous lag",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "    table_version.saturating_sub(cuboid_snapshot)",
     "    table_version.wrapping_sub(cuboid_snapshot)",
     "sankhya-maintenance"),

    ("maintenance: say nothing about a target no refresh can meet",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "    if versions_during_refresh <= target {\n        return None;\n    }",
     "    if true {\n        return None;\n    }",
     "sankhya-maintenance"),

    ("cube: lose the staleness target when a definition is stored",
     "crates/sankhya-cube/src/catalogue.rs",
     "            target_lag: definition.target_lag,",
     "            target_lag: None,",
     "sankhya-cube"),

    ("maintenance: rewrite a materialised cuboid that already exists",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "    if exists(warehouse, key, cube) {\n        return Ok(false);\n    }",
     "    if false {\n        return Ok(false);\n    }",
     "sankhya-server"),

    ("maintenance: write an empty cuboid, so the next run skips the hydration that would find rows",
     "crates/sankhya-maintenance/src/cuboid.rs",
     "    if batch.num_rows() == 0 {\n        return Ok(false);\n    }",
     "    if false {\n        return Ok(false);\n    }",
     "sankhya-server"),

    ("server: discover the cube cache as a user table, so a hash appears in the catalogue",
     "crates/sankhya-server/src/warehouse.rs",
     "        if schema_name.starts_with('_') {\n            continue;\n        }",
     "        if false {\n            continue;\n        }",
     "sankhya-server"),

    ("cube: store every scope's cuboid in one table, so one principal reads another's",
     "crates/sankhya-cube/src/materialise.rs",
     "            self.snapshot,\n            self.scope,\n            // Length-prefixed",
     "            self.snapshot,\n            0,\n            // Length-prefixed",
     "sankhya-cube"),

    # --- a server reading a warehouse it is also maintaining -------------------

    ("server: never re-resolve a table, so a retired file breaks every later query",
     "crates/sankhya-server/src/warehouse.rs",
     "        if now == table.resolved_at {\n            continue;\n        }",
     "        if true {\n            continue;\n        }",
     "sankhya-server"),

    ("server: key a cube's cells on read_as_of, so the snapshot never moves",
     "crates/sankhya-server/src/wiring.rs",
     "            // correct and never reached.\n            let snapshot = self.snapshot_across(cube.reads());",
     "            // correct and never reached.\n            let snapshot = self.settings.read_as_of.get();",
     "sankhya-server"),

    # --- the query log, and selecting from it ---------------------------------

    ("cube: let the query log grow once per query and never trim",
     "crates/sankhya-cube/src/querylog.rs",
     "        if self.entries.len() < capacity {",
     "        if true {",
     "sankhya-cube"),

    ("cube: let one cube's asks crowd out another's",
     "crates/sankhya-cube/src/querylog.rs",
     "                asks.entry(cube.to_string())",
     "                asks.entry(String::new())",
     "sankhya-cube"),

    ("server: answer from any materialised cuboid, ignoring whether it can express the query",
     "crates/sankhya-server/src/wiring.rs",
     "        let chosen = sankhya_cube::materialise::plan(needed, measure, &usable, &base);\n        if !chosen.materialised {\n            return None;\n        }",
     "        let chosen = sankhya_cube::materialise::Plan {\n            from: usable.first().map_or_else(|| base.clone(), |first| (*first).clone()),\n            rolling_away: Vec::new(),\n            materialised: true,\n        };",
     "sankhya-server"),

    ("server: forget that a dice needs the dimension it restricts",
     "crates/sankhya-server/src/wiring.rs",
     "    for (option, take_dimension) in [(\"by=\", false), (\"where=\", true)] {",
     "    for (option, take_dimension) in [(\"by=\", false)] {",
     "sankhya-server"),

    ("server: read a cuboid at the cube's grain rather than the one its key names",
     "crates/sankhya-server/src/wiring.rs",
     "        let dimensions: Vec<String> = key\n            .cuboid\n            .dimensions()\n            .iter()\n            .map(ToString::to_string)\n            .collect();",
     "        let dimensions: Vec<String> =\n            cube.dimensions().iter().map(|d| d.name.clone()).collect();",
     "sankhya-server"),

    ("server: ignore a session that asked for the base data and serve it a cuboid",
     "crates/sankhya-server/src/wiring.rs",
     "        let usable: Vec<&sankhya_cube::algo::Cuboid> = policy.usable(&available, session);",
     "        let usable: Vec<&sankhya_cube::algo::Cuboid> = available.iter().collect();",
     "sankhya-server"),

    ("server: ignore the definition's pinned cuboids",
     "crates/sankhya-server/src/wiring.rs",
     "            for shape in policy.pinned() {\n                if !wanted.contains(shape) {\n                    wanted.push(shape.clone());\n                }\n            }",
     "",
     "sankhya-server"),

    ("server: spend a constant instead of the operator's configured budget",
     "crates/sankhya-server/src/wiring.rs",
     "                        policy.budget_rows(),",
     "                        CUBOID_ROW_BUDGET,",
     "sankhya-server"),

    # Two sites, deliberately. `cube_slice` used to pass a literal `false` here while
    # `cube_rollup` reported what happened; making the slice honest gave this entry a second
    # site, and both are pinned --- by `cube_queries.rs`'s roll-up assertions and by
    # `a_dice_is_not_answered_from_a_cuboid_that_rolled_its_dimension_away`, which now asserts
    # the column rather than comparing a constant against itself.
    ("cube-sql: report the caller's materialise argument instead of what happened",
     "crates/sankhya-cube-sql/src/functions.rs",
     "        let materialised = published.from_cuboid;",
     "        let materialised = false;",
     "sankhya-server",
     2),

    ("server: serve the unrestricted cuboid to a caller whose policy withholds rows",
     "crates/sankhya-server/src/wiring.rs",
     "                if cube\n                    .reads()\n                    .iter()\n                    .all(|table| self.withholds_nothing(principal, table))\n                {\n                    scopes.push(sankhya_cube::materialise::Key::UNRESTRICTED);\n                }",
     "                scopes.push(sankhya_cube::materialise::Key::UNRESTRICTED);",
     "sankhya-server"),

    ("table-delta: claim a commit version by renaming, losing one of two racing commits",
     "crates/sankhya-table-delta/src/log.rs",
     "    match sankhya_atomicfs::claim(&path, body.as_bytes()) {",
     "    match sankhya_atomicfs::publish(&path, body.as_bytes()) {",
     "sankhya-table-delta"),

    ("server: run a statement without announcing it, so a sweeper sees an idle warehouse",
     "crates/sankhya-server/src/wiring.rs",
     "        let _reading = self.leases.pin();",
     "",
     "sankhya-server"),

    ("oltp-pg: re-run initdb over an existing cluster, destroying the system of record",
     "crates/sankhya-oltp-pg/src/lib.rs",
     "        if !self.exists() {\n            self.initialise()?;\n        }",
     "        self.initialise()?;",
     "sankhya-oltp-pg"),

    ("oltp-pg: let the postmaster listen on the network instead of a private socket",
     "crates/sankhya-oltp-pg/src/lib.rs",
     "            \"-c listen_addresses='' -c unix_socket_directories='{}'\",",
     "            \"-c unix_socket_directories='{}'\",",
     "sankhya-oltp-pg"),

    ("oltp-pg: report readiness from bookkeeping rather than asking the cluster",
     "crates/sankhya-oltp-pg/src/lib.rs",
     "        Command::new(self.binaries.program(\"pg_isready\"))\n            .args([\"-h\", &self.socket_directory().to_string_lossy()])\n            .output()\n            .is_ok_and(|out| out.status.success())",
     "        self.running",
     "sankhya-oltp-pg"),

    ("oltp-pg: leave the child running when its supervisor goes away",
     "crates/sankhya-oltp-pg/src/lib.rs",
     "    fn drop(&mut self) {\n        let _ = self.stop();\n    }",
     "    fn drop(&mut self) {}",
     "sankhya-oltp-pg"),

    ("oltp-pg: accept a directory that holds only some of the programs",
     "crates/sankhya-oltp-pg/src/lib.rs",
     "        let complete = [\"initdb\", \"pg_ctl\", \"pg_isready\", \"postgres\"]\n            .iter()\n            .all(|program| directory.join(program).is_file());\n        complete.then_some(Self(directory))",
     "        Some(Self(directory))",
     "sankhya-oltp-pg"),

    ("cube: take the map's write lock on every recorded ask, serializing every cube",
     "crates/sankhya-cube/src/querylog.rs",
     "        let existing = self.asks.read().get(cube).map(Arc::clone);\n        if let Some(ring) = existing {\n            ring.lock().record(cuboid, self.capacity);\n            return;\n        }",
     "",
     "sankhya-cube"),

    ("table-delta: put back one lock over every table, held across the log probe and the replay",
     "crates/sankhya-table-delta/src/cache.rs",
     "        let entry = {\n            let mut shard = shard\n                .lock()\n                .unwrap_or_else(std::sync::PoisonError::into_inner);\n            Arc::clone(\n                shard\n                    .entry(table_root.to_path_buf())\n                    .or_insert_with(|| Arc::new(Mutex::new(Replay::default()))),\n            )\n        };",
     "        let _ = shard;\n        let mut one_lock_for_everything = self\n            .shards\n            .first()\n            .map(|shard| shard.lock().unwrap_or_else(std::sync::PoisonError::into_inner))\n            .expect(\"a shard\");\n        let entry = Arc::clone(\n            one_lock_for_everything\n                .entry(table_root.to_path_buf())\n                .or_insert_with(|| Arc::new(Mutex::new(Replay::default()))),\n        );",
     "sankhya-table-delta"),

    ("maintenance: retire a merge's inputs on the grace period alone, ignoring readers",
     "crates/sankhya-maintenance/src/service.rs",
     "            let unreachable = self\n                .leases\n                .as_ref()\n                .is_none_or(|leases| leases.drained(marked));",
     "            let unreachable = true;",
     "sankhya-maintenance"),

    ("maintenance: mark the epoch when retirement is considered rather than when it merged",
     "crates/sankhya-maintenance/src/service.rs",
     "        let marked = self.leases.as_ref().map_or(0, |leases| leases.mark());",
     "        let marked = 0;",
     "sankhya-maintenance"),

    ("leases: let a reader that could not announce go untracked, so a sweeper thinks it idle",
     "crates/sankhya-leases/src/lib.rs",
     "        if self.unannounced.load(Ordering::SeqCst) > 0 {\n            return Some(0);\n        }",
     "",
     "sankhya-leases"),

    ("atomicfs: claim a name by renaming, which replaces the winner instead of failing",
     "crates/sankhya-atomicfs/src/lib.rs",
     "    let claimed = std::fs::hard_link(&staging, final_path);",
     "    let claimed = std::fs::rename(&staging, final_path);",
     "sankhya-atomicfs"),

    ("atomicfs: share one staging name, so a writer can publish another's bytes",
     "crates/sankhya-atomicfs/src/lib.rs",
     "    let unique = NEXT.fetch_add(1, Ordering::Relaxed);",
     "    let unique = 0;",
     "sankhya-atomicfs"),

    ("atomicfs: publish by writing onto the live path, so a reader sees it half-written",
     "crates/sankhya-atomicfs/src/lib.rs",
     "pub fn publish(final_path: &Path, bytes: &[u8]) -> io::Result<()> {\n    let staging = staging_for(final_path);",
     "pub fn publish(final_path: &Path, bytes: &[u8]) -> io::Result<()> {\n    let staging = final_path.to_path_buf();",
     "sankhya-atomicfs"),

    ("server: never read a materialised cuboid, leaving materialisation write-only",
     "crates/sankhya-server/src/wiring.rs",
     "                if let Some(published) = scopes\n                    .into_iter()\n                    .filter(|_| may_use_a_cuboid)\n                    .find_map(|under| {\n                        self.from_a_cuboid(cube, measure, under, snapshot, session, &needed)\n                    })\n                {\n                    catalog.publish(cube.name(), published.clone());\n                    self.hydrated.put(key, published);\n                    continue;\n                }",
     "                let _ = (scopes, may_use_a_cuboid);",
     "sankhya-server"),

    ("cube: let a stored cuboid claim completeness it never measured",
     "crates/sankhya-cube/src/store.rs",
     "    let completeness = one_completeness(batch, &names)?;",
     "    let completeness = Completeness::complete(batch.num_rows() as u64);",
     "sankhya-cube"),

    ("server: store base cells under a coarser cuboid's key, at a grain it does not have",
     "crates/sankhya-server/src/wiring.rs",
     "                let Some(cells) = crate::cubes::roll_to(&base_cells, shape, measure) else {\n                    continue;\n                };",
     "                let cells = base_cells.clone();",
     "sankhya-server"),

    ("server: cost every cuboid the same, so selection can never choose one",
     "crates/sankhya-server/src/cubes.rs",
     "        ASSUMED_MEMBERS.saturating_pow(u32::try_from(cuboid.width()).unwrap_or(u32::MAX))",
     "        let _ = cuboid;\n        ASSUMED_MEMBERS",
     "sankhya-server"),

    ("server: materialise every shape in the lattice rather than the ones asked for",
     "crates/sankhya-server/src/wiring.rs",
     "            if !asked.is_empty() {",
     "            if false {",
     "sankhya-server"),

    # --- describing a cube, so a client need not hardcode it ------------------

    ("cube-sql: describe levels without their order, so a hierarchy draws alphabetically",
     "crates/sankhya-cube-sql/src/describe.rs",
     "                depth.push(u32::try_from(index).unwrap_or(u32::MAX));",
     "                depth.push(0);",
     "sankhya-server"),

    ("cube-sql: refuse an unknown cube without naming the cubes that are served",
     "crates/sankhya-cube-sql/src/describe.rs",
     "        let known: Vec<&str> = cubes.iter().map(Cube::name).collect();",
     "        let known: Vec<&str> = Vec::new();",
     "sankhya-server"),

    ("cube-sql: report every measure as composable, offering roll-ups that cannot work",
     "crates/sankhya-cube-sql/src/describe.rs",
     "                composes.push(along.rule.composes());",
     "                composes.push(true);",
     "sankhya-server"),

    # --- serving a cube at all ------------------------------------------------

    ("server: never register the cube functions, so a cube is loaded and unanswerable",
     "crates/sankhya-server/src/wiring.rs",
     "    sql.contains(\"cube_rollup\") || sql.contains(\"cube_slice\")",
     "    let _ = sql;\n    false",
     "sankhya-server"),

    # --- the hydration cache: every field of the key prevents something -------

    ("cube-sql: let the hydration cache grow without bound",
     "crates/sankhya-cube-sql/src/hydrated.rs",
     "        if entries.len() >= self.capacity && !entries.contains_key(&key) {",
     "        if false {",
     "sankhya-cube-sql"),

    ("cube-sql: forget every cube when one is invalidated",
     "crates/sankhya-cube-sql/src/hydrated.rs",
     "        self.entries.write().retain(|key, _| key.cube != cube);",
     "        let _ = cube;\n        self.entries.write().clear();",
     "sankhya-cube-sql"),

    # --- the scope digest: what one principal may be served of another's ------

    ("catalog: leave the row filter out of the scope digest",
     "crates/sankhya-catalog/src/guard.rs",
     "        self.row_filter.hash(&mut hasher);",
     "        // self.row_filter.hash(&mut hasher);",
     "sankhya-catalog"),

    ("catalog: leave the tenant out of the scope digest",
     "crates/sankhya-catalog/src/guard.rs",
     "        self.tenant.hash(&mut hasher);",
     "        // self.tenant.hash(&mut hasher);",
     "sankhya-catalog"),

    ("catalog: leave the column masks out of the scope digest",
     "crates/sankhya-catalog/src/guard.rs",
     "        self.column_masks.hash(&mut hasher);",
     "        // self.column_masks.hash(&mut hasher);",
     "sankhya-catalog"),

    ("catalog: put the subject in the scope digest, so no two principals ever share",
     "crates/sankhya-catalog/src/guard.rs",
     "        self.tenant.hash(&mut hasher);",
     "        self.tenant.hash(&mut hasher);\n        self.subject.hash(&mut hasher);",
     "sankhya-catalog"),

    # --- a cube that outlives its process, or quietly changes on the way back ---

    ("cube: default an unknown stored rule to Sum rather than refusing it",
     "crates/sankhya-cube/src/catalogue.rs",
     "                let Some(rule) = rule_named(&rule) else {",
     "                let Some(rule) = rule_named(&rule).or(Some(Rule::Sum)) else {",
     "sankhya-cube"),

    ("cube: load a catalogue and return none of what it holds",
     "crates/sankhya-cube/src/catalogue.rs",
     "        found.push(stored.into_definition()?);",
     "        if false { found.push(stored.into_definition()?); }",
     "sankhya-cube"),

    # --- consolidation: the three ways a total goes silently wrong ---------

    ("cube: consolidate to the leaves, dropping facts attached at an inner member",
     "crates/sankhya-cube/src/consolidate.rs",
     "    let members: BTreeSet<VertexId> = found.found.iter().map(|r| r.vertex).collect();",
     "    let members: BTreeSet<VertexId> = found.found.iter().filter(|r| graph.out_degree(r.vertex, child_edges) == 0).map(|r| r.vertex).collect();",
     "sankhya-cube"),

    ("cube: leave the root out of its own total",
     "crates/sankhya-cube/src/consolidate.rs",
     "    let members: BTreeSet<VertexId> = found.found.iter().map(|r| r.vertex).collect();",
     "    let members: BTreeSet<VertexId> = found.found.iter().filter(|r| r.via.is_some()).map(|r| r.vertex).collect();",
     "sankhya-cube"),

    ("cube: let a truncated consolidation be reported as a total",
     "crates/sankhya-cube/src/consolidate.rs",
     "        self.truncation\n            .explain()\n            .map(|why| Incomplete::Truncated { why })",
     "        None",
     "sankhya-cube"),

    ("cube: stop detecting a hierarchy that consolidates a member into itself",
     "crates/sankhya-cube/src/consolidate.rs",
     "                .any(|arc| arc.target == root)",
     "                .any(|arc| arc.target == root && false)",
     "sankhya-cube"),

    ("cube: deny a total whenever any cycle exists below the root",
     "crates/sankhya-cube/src/consolidate.rs",
     "                .any(|arc| arc.target == root)",
     "                .any(|arc| members.contains(&arc.target))",
     "sankhya-cube"),

    ("cube: consolidate along every edge type rather than the declared roll-up",
     "crates/sankhya-cube/src/consolidate.rs",
     "    let found = reachable(graph, &[root], child_edges, budget);",
     "    let found = reachable(graph, &[root], &graph.all_edge_types(), budget);",
     "sankhya-cube"),

    # --- the sparse cube and the navigation operations ---------------------

    ("cube: read an absent cell as zero",
     "crates/sankhya-cube/src/cells.rs",
     "        if self.values.is_empty() {\n            return None;\n        }",
     "        if self.values.is_empty() {\n            return Some(0.0);\n        }",
     "sankhya-cube"),

    ("cube: sum a cell without fixing the order, so two runs differ",
     "crates/sankhya-cube/src/cells.rs",
     "            Rule::Sum => Some(deterministic_sum(&self.values)),",
     "            Rule::Sum => Some(self.values.iter().sum()),",
     "sankhya-cube"),

    ("cube: report a cell of nothing but NaN as absent",
     "crates/sankhya-cube/src/cells.rs",
     "        best.or(Some(f64::NAN))",
     "        best",
     "sankhya-cube"),

    ("cube: pad an address of the wrong width instead of refusing it",
     "crates/sankhya-cube/src/cells.rs",
     "        if address.len() != self.dimensions.len() {\n            return Err(WrongWidth {",
     "        if false {\n            return Err(WrongWidth {",
     "sankhya-cube"),

    ("cube: give a non-composing measure a value from its partial aggregates",
     "crates/sankhya-cube/src/cells.rs",
     "            Rule::None => None,",
     "            Rule::None => Some(deterministic_sum(&self.values)),",
     "sankhya-cube"),

    ("cube: roll up a positional measure without a stated member order",
     "crates/sankhya-cube/src/navigate.rs",
     "    if positional && order == Ordered::Unstated {",
     "    if false {",
     "sankhya-cube"),

    ("cube: take contributions in visit order, so a closing balance is January's",
     "crates/sankhya-cube/src/navigate.rs",
     "        if positional {\n            values.sort_by_key(|(at, _)| *at);\n        }",
     "",
     "sankhya-cube"),

    # Two sites: `roll_up` had this refusal and `consolidate_along` was given it when the
    # order it demanded stopped being ignored. Both are pinned by tests in
    # `crates/sankhya-cube/tests/navigate.rs`.
    ("cube: place a member missing from the stated order rather than refusing",
     "crates/sankhya-cube/src/navigate.rs",
     "                    return Err(Refused::MemberNotOrdered {\n                        dimension: dimension.to_string(),\n                        member: member.to_string(),\n                    })",
     "                    0",
     "sankhya-cube",
     2),

    ("cube: treat an undeclared dimension as a refusal rather than a definition gap",
     "crates/sankhya-cube/src/navigate.rs",
     "        None => Err(Refused::Undeclared {",
     "        None => Err(Refused::NotComposable {",
     "sankhya-cube"),

    ("cube: keep the sliced axis, leaving a degenerate dimension behind",
     "crates/sankhya-cube/src/navigate.rs",
     "        .filter(|(index, _)| *index != axis)\n        .map(|(_, name)| name.clone())\n        .collect();\n\n    let mut out = Cells::over(remaining);\n    for address in cells.addresses() {\n        if address.get(axis).map(String::as_str) != Some(member) {",
     "        .map(|(_, name)| name.clone())\n        .collect();\n\n    let mut out = Cells::over(remaining);\n    for address in cells.addresses() {\n        if address.get(axis).map(String::as_str) != Some(member) {",
     "sankhya-cube"),

    ("cube: drop a dice restriction naming an absent dimension without saying so",
     "crates/sankhya-cube/src/navigate.rs",
     "            None => ignored.push((*dimension).to_string()),",
     "            None => {}",
     "sankhya-cube"),

    ("cube: lose the axes a partial pivot did not name",
     "crates/sankhya-cube/src/navigate.rs",
     "    for axis in 0..cells.dimensions().len() {\n        if !axes.contains(&axis) {\n            axes.push(axis);\n        }\n    }",
     "",
     "sankhya-cube"),

    ("cube: invent a parent for a member that has none, padding a ragged hierarchy",
     "crates/sankhya-cube/src/navigate.rs",
     "            if let Some(parent) = parents(member) {\n                if let Some(slot) = moved.get_mut(axis) {\n                    *slot = parent;\n                }\n            }",
     "            if let Some(slot) = moved.get_mut(axis) {\n                *slot = parents(member).unwrap_or_else(|| \"unknown\".to_string());\n            }",
     "sankhya-cube"),

    # --- completeness: a filtered total wearing a complete one's clothes ---

    ("cube: report an aggregate over no rows at all as complete",
     "crates/sankhya-cube/src/complete.rs",
     "        if considered == 0 {\n            return None;\n        }",
     "        if considered == 0 {\n            return Some(1.0);\n        }",
     "sankhya-cube"),

    ("cube: call an aggregate over nothing complete",
     "crates/sankhya-cube/src/complete.rs",
     "        self.withheld == 0 && self.contributed > 0",
     "        self.withheld == 0",
     "sankhya-cube"),

    ("cube: combine completeness by keeping one side and ignoring the other",
     "crates/sankhya-cube/src/complete.rs",
     "            contributed: self.contributed.saturating_add(other.contributed),\n            withheld: self.withheld.saturating_add(other.withheld),",
     "            contributed: self.contributed,\n            withheld: self.withheld,",
     "sankhya-cube"),

    ("cube: accept a completeness threshold that is not a fraction",
     "crates/sankhya-cube/src/complete.rs",
     "        if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {",
     "        if false {",
     "sankhya-cube"),

    ("cube: reject a threshold that is exactly met",
     "crates/sankhya-cube/src/complete.rs",
     "            .is_some_and(|seen| seen >= self.at_least)",
     "            .is_some_and(|seen| seen > self.at_least)",
     "sankhya-cube"),

    ("cube: return a partial total when the threshold is not met",
     "crates/sankhya-cube/src/complete.rs",
     "        if threshold.met_by(&self.completeness) {\n            return Ok(&self.value);\n        }",
     "        if true {\n            return Ok(&self.value);\n        }",
     "sankhya-cube"),

    ("cube: drop completeness when a value is mapped",
     "crates/sankhya-cube/src/complete.rs",
     "            completeness: self.completeness,\n        }\n    }\n}",
     "            completeness: Completeness::complete(self.completeness.contributed()),\n        }\n    }\n}",
     "sankhya-cube"),

    # --- exact summation, and where an answer comes from -------------------

    ("math: round an exact sum at every step instead of once at the end",
     "crates/sankhya-math/src/reduce.rs",
     "            let (high, low) = two_sum(carry, *component);\n            if is_nonzero(low) {\n                next.push(low);\n            }",
     "            let (high, low) = two_sum(carry, *component);\n            if false {\n                next.push(low);\n            }",
     "sankhya-math"),

    ("math: drop the error term from a two-sum, making the expansion merely careful",
     "crates/sankhya-math/src/reduce.rs",
     "    let error = (a - a_virtual) + (b - b_virtual);",
     "    let error = 0.0;",
     "sankhya-math"),

    # The find string used to be `if !value.is_finite() {` alone, and there are two of those
    # in this file --- so it matched `exact_sum`'s early scan instead of the expansion's, where
    # a second guard on the scaled value masks it. It survived for two years' worth of runs as
    # a mutation of a line nobody meant.
    ("math: hide a non-finite value inside an exact sum",
     "crates/sankhya-math/src/reduce.rs",
     "        if !value.is_finite() {\n            // An infinity or a NaN makes the expansion meaningless",
     "        if false {\n            // An infinity or a NaN makes the expansion meaningless",
     "sankhya-math"),

    ("math: keep zero components, so an expansion grows without bound",
     "crates/sankhya-math/src/reduce.rs",
     "        if is_nonzero(carry) {\n            next.push(carry);\n        }",
     "        next.push(carry);",
     "sankhya-math"),

    ("cube: store a materialised partial rounded, so the fast path drifts from the slow one",
     "crates/sankhya-cube/src/navigate.rs",
     "            let _ = out.add_reduced(coarser, rule, exact);\n            continue;",
     "            let _ = out.add(coarser, exact.to_f64());\n            continue;",
     "sankhya-cube"),

    # Named by the map it gathers into, because `consolidate_along` now carries the same
    # shape. Only the roll-up site is mutated here: nothing yet asserts that a partial
    # aggregate survives a *consolidation* unrounded, so mutating that one would record a
    # survivor rather than prove a defence.
    ("cube: roll a partial aggregate up as its rounded value",
     "crates/sankhya-cube/src/navigate.rs",
     "        let slot = gathered.entry(coarser).or_default();\n        if contributions.rule_used().is_some() {\n            slot.push((at, contributions.exact_sum()));",
     "        let slot = gathered.entry(coarser).or_default();\n        if contributions.rule_used().is_some() {\n            slot.push((at, Exact::of(&[contributions.exact_sum().to_f64()])));",
     "sankhya-cube"),

    ("cube: let a reduced cell answer with whatever rule is asked for",
     "crates/sankhya-cube/src/cells.rs",
     "        if self.reduced_under.is_some() {",
     "        if false {",
     "sankhya-cube"),

    ("cube: name a materialised table without length-prefixing its dimensions",
     "crates/sankhya-cube/src/materialise.rs",
     "            out.push_str(&format!(\"_{}_{dimension}\", dimension.len()));",
     "            out.push_str(&format!(\"_{dimension}\"));",
     "sankhya-cube"),

    ("cube: leave the snapshot out of a materialisation key",
     "crates/sankhya-cube/src/materialise.rs",
     "            self.definition,\n            self.snapshot",
     "            self.definition,\n            0",
     "sankhya-cube"),

    ("cube: let a session widen materialisation past what is configured",
     "crates/sankhya-cube/src/materialise.rs",
     "            Session::PinnedOnly => available\n                .iter()\n                .filter(|cuboid| self.pinned.contains(cuboid))\n                .collect(),",
     "            Session::PinnedOnly => available.iter().collect(),",
     "sankhya-cube"),

    ("cube: honour a session that asks for no materialisation by using it anyway",
     "crates/sankhya-cube/src/materialise.rs",
     "            Session::Off => Vec::new(),",
     "            Session::Off => available.iter().collect(),",
     "sankhya-cube"),

    ("cube: answer from an ancestor the measure does not permit",
     "crates/sankhya-cube/src/materialise.rs",
     "        if !permits(candidate, query, measure) {\n            continue;\n        }",
     "",
     "sankhya-cube"),

    ("cube: choose an ancestor by iteration order rather than by width",
     "crates/sankhya-cube/src/materialise.rs",
     "            (candidate.width(), candidate.dimensions())\n                < (current.width(), current.dimensions())",
     "            candidate.width() < current.width()",
     "sankhya-cube"),

    # --- the write-back overlay: keeping a what-if distinguishable ---------

    ("cube: serve an overlaid figure without saying which scenario it came from",
     "crates/sankhya-cube/src/overlay.rs",
     "            overlay: Some(self.name.clone()),",
     "            overlay: None,",
     "sankhya-cube"),

    ("cube: apply an overlay written against a different cube definition",
     "crates/sankhya-cube/src/overlay.rs",
     "        if definition != self.definition {",
     "        if false {",
     "sankhya-cube"),

    ("cube: show unadjusted children beneath an adjusted total",
     "crates/sankhya-cube/src/overlay.rs",
     "                Allocation::Refuse => {\n                    return Err(NotApplicable::FinerThanWritten {\n                        overlay: self.name.clone(),\n                        written_at: entry.grain.clone(),\n                        asked_at: dimensions.clone(),\n                    })\n                }",
     "                Allocation::Refuse => continue,",
     "sankhya-cube"),

    ("cube: divide an allocation equally when there is nothing to be proportional to",
     "crates/sankhya-cube/src/overlay.rs",
     "    if beneath.is_empty() || total == 0.0 {",
     "    if false {",
     "sankhya-cube"),

    ("cube: add an overlay entry into a total it is not part of",
     "crates/sankhya-cube/src/overlay.rs",
     "            if finer.is_empty() {\n                // The cube is *coarser* than the entry. Adding a leaf figure into a total\n                // it is not part of would double-count, so it is left alone.\n                continue;\n            }",
     "",
     "sankhya-cube"),

    ("cube: collapse a delta into a replacement, losing what the planner meant",
     "crates/sankhya-cube/src/overlay.rs",
     "        Adjustment::Delta(by) => existing.unwrap_or(0.0) + by,",
     "        Adjustment::Delta(by) => by,",
     "sankhya-cube"),

    # --- the SQL surface: provenance that survives a projection ------------

    ("cube-sql: refuse an unknown cube without naming the ones that exist",
     "crates/sankhya-cube-sql/src/catalog.rs",
     "                known: declared,",
     "                known: Vec::new(),",
     "sankhya-cube-sql"),

    ("cube-sql: report a cube awaiting its first load as a missing name",
     "crates/sankhya-cube-sql/src/catalog.rs",
     "            return Err(Unresolved::NothingPublished {\n                name: name.to_string(),\n            });",
     "            return Err(Unresolved::NoSuchCube {\n                name: name.to_string(),\n                known: Vec::new(),\n            });",
     "sankhya-cube-sql"),

    ("cube-sql: ignore an unrecognised option instead of refusing the query",
     "crates/sankhya-cube-sql/src/args.rs",
     "            if !KNOWN_OPTIONS.contains(&key.as_str()) {",
     "            if false {",
     "sankhya-cube-sql"),

    ("cube-sql: drop the overlay name from the rows",
     "crates/sankhya-cube-sql/src/functions.rs",
     "        Arc::new(StringArray::from(vec![overlay; rows])),",
     "        Arc::new(StringArray::from(vec![None::<&str>; rows])),",
     "sankhya-cube-sql"),

    ("cube-sql: leave the snapshot out of the rows",
     "crates/sankhya-cube-sql/src/functions.rs",
     "        Arc::new(UInt64Array::from(vec![published.snapshot; rows])),",
     "        Arc::new(UInt64Array::from(vec![0_u64; rows])),",
     "sankhya-cube-sql"),

    ("cube-sql: apply no overlay when one is named",
     "crates/sankhya-cube-sql/src/functions.rs",
     "    let Some(name) = args.string(\"overlay\") else {",
     "    let Some(name) = None::<String> else {",
     "sankhya-cube-sql"),

    ("cube-sql: accept a restriction naming a dimension the cube lacks",
     "crates/sankhya-cube-sql/src/functions.rs",
     "    if !diced.ignored.is_empty() {",
     "    if false {",
     "sankhya-cube-sql"),

    ("cube-sql: default an unknown measure rather than naming the ones that exist",
     "crates/sankhya-cube-sql/src/functions.rs",
     "    published.cube.measure(&name).cloned().ok_or_else(|| {",
     "    published.cube.measures().first().cloned().ok_or_else(|| {",
     "sankhya-cube-sql"),

    ("cube-sql: key published cells by cube alone, so a second measure evicts the first",
     "crates/sankhya-cube-sql/src/catalog.rs",
     "        self.cubes.write().insert((name, measure), published);",
     "        self.cubes.write().insert((name, String::new()), published);",
     "sankhya-cube-sql"),

    ("cube-sql: resolve cells by cube alone, ignoring which measure was asked for",
     "crates/sankhya-cube-sql/src/functions.rs",
     "    let measure = args.string_at(1, \"measure name\")?;\n    catalog\n        .resolve(&name, &measure)",
     "    let measure = args.string_at(1, \"measure name\")?;\n    let _ = &measure;\n    catalog\n        .resolve(&name, \"amount\")",
     "sankhya-cube-sql"),

    ("cube-sql: skip the completeness threshold a query asked for",
     "crates/sankhya-cube-sql/src/functions.rs",
     "    let Some(required) = args.number(\"min_completeness\")? else {",
     "    let Some(required) = None::<f64> else {",
     "sankhya-cube-sql"),

    # --- hydration: rows that could not be placed --------------------------

    ("cube: drop a row with a null dimension key instead of counting it",
     "crates/sankhya-cube/src/hydrate.rs",
     "        let Some(address) = address_of(&keys, row) else {\n            absorbed.unplaced = absorbed.unplaced.saturating_add(1);\n            continue;\n        };",
     "        let Some(address) = address_of(&keys, row) else {\n            continue;\n        };",
     "sankhya-cube"),

    ("cube: place a null member under the empty string, inventing a member",
     "crates/sankhya-cube/src/hydrate.rs",
     "        if column.is_null(row) {\n            return None;\n        }",
     "",
     "sankhya-cube"),

    ("cube: read a null measure as zero",
     "crates/sankhya-cube/src/hydrate.rs",
     "        let Some(value) = values.at(row) else {\n            absorbed.unplaced = absorbed.unplaced.saturating_add(1);\n            continue;\n        };",
     "        let value = values.at(row).unwrap_or(0.0);",
     "sankhya-cube"),

    ("cube: skip a dimension whose join column the fact table lacks",
     "crates/sankhya-cube/src/hydrate.rs",
     "        let column = batch.column_by_name(&dimension.joins_on).ok_or_else(|| {",
     "        let Some(column) = batch.column_by_name(&dimension.joins_on) else { continue }; let column = Ok::<_, NotHydratable>(column).map_err(|_: NotHydratable| {",
     "sankhya-cube"),

    ("cube-sql: compute completeness from the rows that survived",
     "crates/sankhya-cube-sql/src/functions.rs",
     "        let completeness = published.completeness;\n        check_completeness(&completeness, &args)?;\n\n        let rolled",
     "        let completeness = Completeness::complete(narrowed.len() as u64);\n        check_completeness(&completeness, &args)?;\n\n        let rolled",
     "sankhya-cube-sql"),

    # --- date partitioning: declared and applied ---------------------------

    ("publish: write files flat while declaring a partition column",
     "crates/sankhya-publish/src/publish.rs",
     "            add.partition_values\n                .insert(DATA_DATE_COLUMN.to_string(), partition.clone());",
     "",
     "sankhya-publish"),

    ("publish: put every row of a batch in one partition whatever its date",
     "crates/sankhya-publish/src/publish.rs",
     "                    let partition = self.date_axis.partition_of(days.value(row));",
     "                    let partition = self.date_axis.partition_of(days.value(0));",
     "sankhya-publish"),

    ("publish: file a row with no date under today instead of refusing",
     "crates/sankhya-publish/src/publish.rs",
     "                    if days.is_null(row) {",
     "                    if false {",
     "sankhya-publish"),

    ("publish: commit each partition of a batch separately",
     "crates/sankhya-publish/src/publish.rs",
     "        commit(&self.root, version, &actions).map_err(|error| PublishError::Commit {",
     "        for one in actions.chunks(1) { commit(&self.root, version, one).ok(); }\n        commit(&self.root, version, &[]).map_err(|error| PublishError::Commit {",
     "sankhya-publish"),

    ("publish: declare a partition column the schema does not contain",
     "crates/sankhya-publish/src/publish.rs",
     "        // reader written against the source schema still finds its columns where they were.\n        let stored = with_date_column(schema);",
     "        // reader written against the source schema still finds its columns where they were.\n        let stored = schema.clone();",
     "sankhya-publish"),

    # The same derivation on the evolution path, which is a second site and a quieter failure:
    # a table that loses the column it is partitioned on at the *first schema change* rather
    # than at creation, where nothing was watching for it.
    ("publish: drop the partition column when a schema change is adopted",
     "crates/sankhya-publish/src/publish.rs",
     "        // evolution and the table would stop declaring the column it is partitioned on.\n        let stored = with_date_column(schema);",
     "        // evolution and the table would stop declaring the column it is partitioned on.\n        let stored = schema.clone();",
     "sankhya-publish"),

    ("publish: leave the date column out of the file it partitions by",
     "crates/sankhya-publish/src/publish.rs",
     "            let part = stamped(&part, &partition, self.date_axis.granularity)?;",
     "",
     "sankhya-publish"),

    ("publish: stamp a row with its own date rather than its partition's",
     "crates/sankhya-publish/src/publish.rs",
     "        Granularity::Month => (read(0, 1970), read(1, 1), 1),",
     "        Granularity::Month => (read(0, 1970), read(1, 1), 2),",
     "sankhya-publish"),

    # --- fan-out: FR-CDC-14's guard ---------------------------------------

    ("publish: write a partition however small, restoring the tiny-file fan-out",
     "crates/sankhya-publish/src/fanout.rs",
     "            if entry.bytes >= self.fan_out.min_file_bytes {",
     "            if true {",
     "sankhya-publish"),

    ("publish: defer a partition for ever, so a slow one is never readable",
     "crates/sankhya-publish/src/fanout.rs",
     "            } else if entry.waited >= self.fan_out.max_deferred_batches {",
     "            } else if false {",
     "sankhya-publish"),

    ("publish: ignore the per-commit partition cap",
     "crates/sankhya-publish/src/fanout.rs",
     "        if !everything && ready.len() > self.fan_out.max_partitions_per_commit {",
     "        if false {",
     "sankhya-publish"),

    ("publish: absorb sustained fan-out silently instead of reporting it",
     "crates/sankhya-publish/src/fanout.rs",
     "        self.batches >= 8",
     "        false && self.batches >= 8",
     "sankhya-publish"),

    ("publish: report a fan-out of zero before any batch has arrived",
     "crates/sankhya-publish/src/fanout.rs",
     "        (self.batches > 0).then(|| self.partitions_touched as f64 / self.batches as f64)",
     "        Some(self.partitions_touched as f64 / self.batches.max(1) as f64)",
     "sankhya-publish"),

    ("publish: write each deferred batch as its own file, defeating accumulation",
     "crates/sankhya-publish/src/publish.rs",
     "        // than five. Writing them separately would defeat the accumulation entirely.\n        let schema = batches.first().map_or_else(\n            || Arc::new(arrow_schema::Schema::empty()),\n            RecordBatch::schema,\n        );\n        let combined = arrow_select::concat::concat_batches(&schema, batches)",
     "        // than five. Writing them separately would defeat the accumulation entirely.\n        let schema = batches.first().map_or_else(\n            || Arc::new(arrow_schema::Schema::empty()),\n            RecordBatch::schema,\n        );\n        let combined = Ok::<_, arrow_schema::ArrowError>(batches[0].clone())",
     "sankhya-publish"),

    ("publish: take the next version from the live set, missing an empty table's commits",
     "crates/sankhya-publish/src/publish.rs",
     "        self.newest().map_or(0, |version| version.saturating_add(1))",
     "        sankhya_table_delta::live_files(&self.root).ok().and_then(|s| s.version).map_or(0, |v| v.saturating_add(1))",
     "sankhya-publish"),

    ("publish: omit the protocol action from a creating commit",
     "crates/sankhya-publish/src/publish.rs",
     "        commit(&self.root, 0, &create(metadata)).map_err(|error| {",
     "        commit(&self.root, 0, &[Action::Metadata(metadata)]).map_err(|error| {",
     "sankhya-publish"),

    # --- a soak watches the resource it can exhaust ------------------------

    ("soak: stop watching what the warehouse consumes",
     "crates/sankhya-diagnostic/src/soak/measure.rs",
     "        name: \"warehouse_bytes\",",
     "        name: \"warehouse_bytes_unwatched\",",
     "sankhya-diagnostic"),

    ("soak: measure a directory tree as empty",
     "crates/sankhya-diagnostic/src/soak/sample.rs",
     "                Ok(metadata) => *total = total.saturating_add(metadata.len()),",
     "                Ok(metadata) => { let _ = metadata; }",
     "sankhya-diagnostic"),

    ("soak: count the log as part of what the data consumes",
     "crates/sankhya-diagnostic/src/soak/sample.rs",
     "    if !path.exists() {\n        return None;\n    }",
     "    if false {\n        return None;\n    }",
     "sankhya-diagnostic"),

    # --- compaction lands in the partition its rows belong to --------------

    ("maintenance: log a merged file by its bare name, losing its partition",
     "crates/sankhya-maintenance/src/driver.rs",
     "            p.strip_prefix(table_root)\n                .unwrap_or(p)\n                .to_string_lossy()\n                .into_owned()\n        };\n\n        // Everything the merge learned",
     "            p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())\n        };\n\n        // Everything the merge learned",
     "sankhya-maintenance"),

    ("maintenance: retire live files by bare name, so a partitioned one never leaves",
     "crates/sankhya-maintenance/src/driver.rs",
     "        let superseded: BTreeSet<String> =\n            outcome.inputs_retained.iter().map(|p| relative(p)).collect();",
     "        let superseded: BTreeSet<String> = outcome.inputs_retained.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();",
     "sankhya-maintenance"),

    ("maintenance: write a merged file outside the partition its rows belong to",
     "crates/sankhya-maintenance/src/driver.rs",
     "        let name = if directory_of_inputs.is_empty() {",
     "        let directory_of_inputs = String::new();\n        let name = if directory_of_inputs.is_empty() {",
     "sankhya-maintenance"),

    # --- clustering: declared, and applied only when settled ---------------

    ("maintenance: cluster a table nobody declared a clustering for",
     "crates/sankhya-maintenance/src/layout.rs",
     "    config\n        .list(&clustering_key(schema, table))\n        .unwrap_or_default()",
     "    config\n        .list(&clustering_key(schema, table))\n        .unwrap_or_else(|| vec![\"id\".to_string()])",
     "sankhya-maintenance"),

    ("maintenance: accept a clustering on a column the table does not have",
     "crates/sankhya-maintenance/src/layout.rs",
     "    if missing.is_empty() {",
     "    if true {",
     "sankhya-maintenance"),

    ("maintenance: sort a partition that is still receiving writes",
     "crates/sankhya-maintenance/src/execute.rs",
     "    let clustering: &[String] = if plan.settled { clustering } else { &[] };",
     "    let clustering: &[String] = clustering;",
     "sankhya-maintenance"),

    # --- configuration: precedence, resolution and refusal -----------------

    ("config: leave an unresolved reference in the value instead of refusing",
     "crates/sankhya-config/src/resolve.rs",
     "            return Err(Unresolved::NoSuchKey {",
     "            return Ok(format!(\"{out}${{{name}}}{rest}\"));\n            #[allow(unreachable_code)] return Err(Unresolved::NoSuchKey {",
     "sankhya-config"),

    ("config: resolve a circular reference until it runs out of stack",
     "crates/sankhya-config/src/resolve.rs",
     "        if visiting.contains(name) {",
     "        if false {",
     "sankhya-config"),

    ("config: let a file outrank an environment variable",
     "crates/sankhya-config/src/lib.rs",
     "        for (key, value) in environment {\n            settings.insert(key.clone(), Origin::from(value.clone(), Source::Environment));\n        }",
     "",
     "sankhya-config"),

    ("config: let the environment outrank a command-line argument",
     "crates/sankhya-config/src/lib.rs",
     "        for (key, value) in arguments {\n            settings.insert(key.clone(), Origin::from(value.clone(), Source::CommandLine));\n        }",
     "",
     "sankhya-config"),

    ("config: ignore a .local overlay",
     "crates/sankhya-config/src/lib.rs",
     "            if let Some(overlay) = local_overlay(&path) {",
     "            if let Some(overlay) = None::<PathBuf> {",
     "sankhya-config"),

    ("config: default an unparseable typed value instead of refusing it",
     "crates/sankhya-config/src/lib.rs",
     "        parse(&origin.value).map(Some).ok_or_else(|| ConfigError::NotA {",
     "        Ok(parse(&origin.value)).map_err(|_: ()| ConfigError::NotA {",
     "sankhya-config"),

    ("config: read a boolean that is neither true nor false as false",
     "crates/sankhya-config/src/lib.rs",
     "                _ => None,\n            }\n        })\n    }\n\n    /// A setting as a duration",
     "                _ => Some(false),\n            }\n        })\n    }\n\n    /// A setting as a duration",
     "sankhya-config"),

    ("config: print a secret",
     "crates/sankhya-config/src/secret.rs",
     "        f.write_str(\"<redacted>\")",
     "        f.write_str(&self.0)",
     "sankhya-config"),

    ("config: report a changed secret's old value",
     "crates/sankhya-config/src/lib.rs",
     "            if looks_secret(key) {\n                parts.push(format!(\"{key} changed\"));",
     "            if false {\n                parts.push(format!(\"{key} changed\"));",
     "sankhya-config"),

    ("config: replace a working configuration with a broken one on reload",
     "crates/sankhya-config/src/lib.rs",
     "        let fresh = Self::load_with(&files, environment, arguments)?;",
     "        let fresh = Self::load_with(&files, environment, arguments).unwrap_or_default();",
     "sankhya-config"),

    ("config: load part of a malformed file rather than none of it",
     "crates/sankhya-config/src/lib.rs",
     "            let parsed = parse::file(path).map_err(ConfigError::Unreadable)?;",
     "            let parsed = parse::file(path).unwrap_or_default();",
     "sankhya-config"),

    # --- the invariants document names every check, and no others ----------

    ("docs: name a check in INVARIANTS.md that does not run",
     "xtask/src/main.rs",
     "        if !known.contains(check.as_str()) {",
     "        if false {",
     "xtask"),

    ("docs: run a check that INVARIANTS.md documents nowhere",
     "xtask/src/main.rs",
     "        if !named.contains(check) {",
     "        if false {",
     "xtask"),

    # --- the soak writes nothing outside the project root ------------------

    ("soak: accept a warehouse outside the project root",
     "crates/sankhya-diagnostic/tests/soak_run.rs",
     "    if !resolved.starts_with(&root) {",
     "    if false {",
     "sankhya-diagnostic"),

    ("soak: check the unresolved path, so `..` walks out of the root",
     "crates/sankhya-diagnostic/tests/soak_run.rs",
     "    let anchor = absolute\n        .ancestors()\n        .find(|candidate| candidate.exists())\n        .unwrap_or(root);",
     "    let anchor = root;",
     "sankhya-diagnostic"),

    # --- the guide's examples are executed, or accounted for ---------------

    ("docs: name a source file in prose that does not exist",
     "xtask/src/main.rs",
     "            if !root.join(&named).exists() {",
     "            if false {",
     "xtask"),

            # --- M8 §12.1: the concurrency criteria, which a global lock would satisfy ------

    ("table-delta: serialize every commit in the warehouse behind one lock",
     "crates/sankhya-table-delta/src/log.rs",
     "    sankhya_atomicfs::create_dir_durably(&log_dir(table_root))\n        .map_err(|e| CommitError::Io(format!(\"creating the log directory: {e}\")))?;",
     "    static ONE_LOCK_FOR_THE_WAREHOUSE: std::sync::Mutex<()> = std::sync::Mutex::new(());\n    let _serialized = ONE_LOCK_FOR_THE_WAREHOUSE\n        .lock()\n        .unwrap_or_else(std::sync::PoisonError::into_inner);\n    sankhya_atomicfs::create_dir_durably(&log_dir(table_root))\n        .map_err(|e| CommitError::Io(format!(\"creating the log directory: {e}\")))?;",
     "sankhya-table-delta"),

    ("publish: probe for the newest version from zero on every append, not from the last seen",
     "crates/sankhya-publish/src/publish.rs",
     "        let floor = self.seen.load(Ordering::Relaxed).checked_sub(1);",
     "        let floor: Option<u64> = None;",
     "sankhya-publish"),

    # --- M14: transport security, and the mistakes it exists to name -----------------

    ("tls: accept a file with no certificate in it as a certificate",
     "crates/sankhya-tls/src/material.rs",
     "        if chain.is_empty() {",
     "        if false {",
     "sankhya-tls"),

    ("tls: leave a mismatched key and certificate to be discovered at the first connection",
     "crates/sankhya-tls/src/material.rs",
     "        material.server_config(&[])?;",
     "        let _ = &material;",
     "sankhya-tls"),

    ("tls: report a handshake that never arrived as one that was rejected",
     "crates/sankhya-tls/src/acceptor.rs",
     "            Err(_) => Err(Failed::TimedOut),",
     "            Err(_) => Err(Failed::Rejected(String::new())),",
     "sankhya-tls"),

    ("tls: advertise no application protocol on the columnar door",
     "crates/sankhya-tls/src/acceptor.rs",
     "            Self::Http2 => vec![b\"h2\".to_vec()],",
     "            Self::Http2 => Vec::new(),",
     "sankhya-tls"),

    ("tls: trust a client bundle that yields no anchor",
     "crates/sankhya-tls/src/material.rs",
     "        if anchors.is_empty() {",
     "        if false {",
     "sankhya-tls"),

    ("wire protocol: decline TLS on a door that has it",
     "crates/sankhya-api-pg/src/session.rs",
     "                if self.encrypts {",
     "                if false {",
     "sankhya-api-pg"),

    ("wire protocol: serve a plain client on a door that requires encryption",
     "crates/sankhya-api-pg/src/session.rs",
     "            (Phase::Startup, FrontendMessage::Startup { parameters }) if self.insists => {",
     "            (Phase::Startup, FrontendMessage::Startup { parameters }) if false => {",
     "sankhya-api-pg"),

    ("wire protocol: say nothing to a client asking about GSSAPI encryption",
     "crates/sankhya-api-pg/src/message.rs",
     "        GSS_REQUEST_CODE => Ok((FrontendMessage::GssEncRequest, length)),",
     "        GSS_REQUEST_CODE if false => Ok((FrontendMessage::GssEncRequest, length)),",
     "sankhya-api-pg"),

    # --- M13: a declared feed, and the coercions it will not make --------------------

    ("feed: accept a declaration that never says where its rows' date comes from",
     "crates/sankhya-feed/src/validate.rs",
     "        None => faults.push(Fault::NoDateAxis),",
     "        None => {}",
     "sankhya-feed"),

    ("feed: let a not-null column be filled with null when its key is absent",
     "crates/sankhya-feed/src/validate.rs",
     "        if column.missing == Missing::Null && !column.nullable {",
     "        if false {",
     "sankhya-feed"),

    ("feed: accept a stop rate that can never fire",
     "crates/sankhya-feed/src/validate.rs",
     "    if !(quarantine.stop_above > 0.0 && quarantine.stop_above < 1.0) {",
     "    if false {",
     "sankhya-feed"),

    ("feed: let two columns read the same key without saying so",
     "crates/sankhya-feed/src/validate.rs",
     "        if columns.len() > 1 {",
     "        if false {",
     "sankhya-feed"),

    ("feed: accept a key no column claims",
     "crates/sankhya-feed/src/bind.rs",
     "    if feed.unknown() == Unknown::Refuse {",
     "    if false {",
     "sankhya-feed"),

    ("feed: let a whole number too wide for its column through",
     "crates/sankhya-feed/src/bind.rs",
     "        LogicalType::Int16 => i16::try_from(whole).is_ok(),",
     "        LogicalType::Int16 => true,",
     "sankhya-feed"),

    ("feed: round a decimal with more places than the column declares",
     "crates/sankhya-feed/src/bind.rs",
     "    if fraction.len() > usize::from(precision.scale) {",
     "    if false {",
     "sankhya-feed"),

    ("feed: accept a null in a column that does not take one",
     "crates/sankhya-feed/src/bind.rs",
     "        return if column.nullable {",
     "        return if true {",
     "sankhya-feed"),

    ("feed: measure a stop rate before there is one to measure",
     "crates/sankhya-feed/src/stop.rs",
     "        if self.recent.len() < self.window as usize {",
     "        if false {",
     "sankhya-feed"),

    ("feed: let a source that produced nothing usable pass quietly",
     "crates/sankhya-feed/src/stop.rs",
     "        if read > 0 && unfit == read {",
     "        if false {",
     "sankhya-feed"),

    ("feed: fingerprint a declaration without length-prefixing its fields",
     "crates/sankhya-feed/src/quarantine.rs",
     "    for byte in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {",
     "    for byte in bytes {",
     "sankhya-feed"),

    ("feed: run a halted feed again on the next tick",
     "crates/sankhya-feed/src/state.rs",
     "        standing.get(name).is_none_or(|entry| !entry.is_halted())",
     "        standing.get(name).is_none_or(|_| true)",
     "sankhya-feed"),

    ("feed: forget that a feed has halted before",
     "crates/sankhya-feed/src/state.rs",
     "        if entry.is_halted() {\n            return;\n        }\n        entry.halts = entry.halts.saturating_add(1);",
     "        if entry.is_halted() {\n            return;\n        }",
     "sankhya-feed"),

    ("feed: report success for resuming a feed nobody declared",
     "crates/sankhya-feed/src/state.rs",
     "            None => false,",
     "            None => true,",
     "sankhya-feed"),

    ("feed: take a statement that merely begins like a feed command",
     "crates/sankhya-feed/src/command.rs",
     "    if first.eq_ignore_ascii_case(\"SHOW\") && second.is_some_and(|w| w.eq_ignore_ascii_case(\"FEEDS\"))",
     "    if first.eq_ignore_ascii_case(\"SHOW\")",
     "sankhya-feed"),

    ("feed: ignore words after a complete feed command",
     "crates/sankhya-feed/src/command.rs",
     "        if let Some(after) = words.get(3) {",
     "        if false {",
     "sankhya-feed"),

    ("maintenance: expire a partition whose date cannot be read",
     "crates/sankhya-maintenance/src/expire.rs",
     "        let Some(day) = day_of(partition) else {\n            continue;\n        };",
     "        let day = day_of(partition).unwrap_or(i32::MIN);",
     "sankhya-maintenance"),

    ("maintenance: expire a partition on the retention boundary",
     "crates/sankhya-maintenance/src/expire.rs",
     "        if day < cutoff {",
     "        if day <= cutoff {",
     "sankhya-maintenance"),

    ("maintenance: wrap the retention cutoff into the future",
     "crates/sankhya-maintenance/src/expire.rs",
     "    let cutoff = today.saturating_sub(i32::try_from(retain_days).unwrap_or(i32::MAX));",
     "    let cutoff = today.wrapping_sub(retain_days as i32);",
     "sankhya-maintenance"),

    ("readpath: give an empty scan the whole schema instead of the projection",
     "crates/sankhya-readpath/src/provider.rs",
     "                    Some(columns) => Arc::new(self.schema.project(columns)?),",
     "                    Some(_) => Arc::clone(&self.schema),",
     "sankhya-readpath"),

    ("feed: re-read a source the position says is already read",
     "crates/sankhya-feed/src/run.rs",
     "                ran.already_read = ran.already_read.saturating_add(1);\n                continue;",
     "                ran.already_read = ran.already_read.saturating_add(1);\n                0",
     "sankhya-feed"),

    ("feed: treat an unreadable recorded position as a feed that has never run",
     "crates/sankhya-feed/src/run.rs",
     "        Some(text) => Position::from_property(&text).map_err(|error| {",
     "        Some(text) => Ok(Position::from_property(&text).unwrap_or_default()).map_err(|error: serde_json::Error| {",
     "sankhya-feed"),

    ("feed: leave the position where it was when a source produced nothing usable",
     "crates/sankhya-feed/src/run.rs",
     "    if finishing {\n        position.finished(source);",
     "    if false {\n        position.finished(source);",
     "sankhya-feed"),

    ("feed: carry on to the next source after the feed has stopped",
     "crates/sankhya-feed/src/run.rs",
     "        if let Some(reason) = stopped {\n            ran.stopped = Some(reason);\n            return Ok(ran);\n        }",
     "        if let Some(reason) = stopped {\n            ran.stopped = Some(reason);\n        }",
     "sankhya-feed"),

    ("feed: skip a blank line by quarantining it",
     "crates/sankhya-feed/src/source.rs",
     "        if text.trim().is_empty() {",
     "        if false {",
     "sankhya-feed"),

    ("feed: read a source's records in whatever order the filesystem lists them",
     "crates/sankhya-feed/src/source.rs",
     "    found.sort();",
     "    found.reverse();",
     "sankhya-feed"),

    ("server: create a table a feed lands in rather than refusing",
     "crates/sankhya-server/src/feeds.rs",
     "    if !sankhya_publish::is_table(&table_root) {",
     "    if false {",
     "sankhya-server"),

    ("server: stop loading feeds at the first declaration that does not parse",
     "crates/sankhya-server/src/feeds.rs",
     "                complaints.push(format!(\"{}: {error}\", path.display()));\n                continue;\n            }\n        };\n        let from = PathBuf::from(&declaration.from);",
     "                complaints.push(format!(\"{}: {error}\", path.display()));\n                break;\n            }\n        };\n        let from = PathBuf::from(&declaration.from);",
     "sankhya-server"),

    ("server: start in the clear when a certificate is configured without its key",
     "crates/sankhya-server/src/main.rs",
     "            if present.is_none() || absent.is_none() {",
     "            if false {",
     "sankhya-server"),

    # The defect M13's exit demonstration found. `recognise` answers `SHOW <anything>` as a
    # session setting, so `SHOW FEEDS` was answered with one empty value and never reached the
    # handler that implements it. Nothing below the socket could notice.
    ("wire: let the catalogue answer a statement the handler defines itself",
     "crates/sankhya-api-pg/src/session.rs",
     '        // "no such table" error for a query the client considers routine.\n        if let Some(catalogue) = recognise(sql).filter(|_| !handler.claims(sql)) {',
     '        // "no such table" error for a query the client considers routine.\n        if let Some(catalogue) = recognise(sql) {',
     "sankhya-api-pg"),

    # The extended protocol's copy of the same decision, which was covered by nothing. Every
    # driver that binds parameters --- pgjdbc, psycopg3, asyncpg, SQLAlchemy --- takes this
    # path and not the simple one above.
    ("wire: let the catalogue answer an extended-protocol statement the handler defines",
     "crates/sankhya-api-pg/src/session.rs",
     "    ) -> Result<QueryResult, QueryFailure> {\n        if let Some(catalogue) = recognise(sql).filter(|_| !handler.claims(sql)) {\n            let result = answer(",
     "    ) -> Result<QueryResult, QueryFailure> {\n        if let Some(catalogue) = recognise(sql) {\n            let result = answer(",
     "sankhya-api-pg"),

    # And the other direction: a handler that claims one `SHOW` must not have claimed every
    # settings query a catalogue-browsing client sends on connection.
    ("wire: let a handler's claim swallow every catalogue query",
     "crates/sankhya-api-pg/src/session.rs",
     "    ) -> Result<QueryResult, QueryFailure> {\n        if let Some(catalogue) = recognise(sql).filter(|_| !handler.claims(sql)) {",
     "    ) -> Result<QueryResult, QueryFailure> {\n        if let Some(catalogue) = None::<crate::catalog::CatalogQuery> {",
     "sankhya-api-pg"),

    ("server: answer a feed command from the catalogue instead of the feed registry",
     "crates/sankhya-server/src/wiring.rs",
     "            || sankhya_feed::parse_command(sql).is_some()",
     "            || (false && sankhya_feed::parse_command(sql).is_some())",
     "sankhya-server"),

    # The adversarial review of 2026-09-01. Each of these is a defect it found.
    ("wire: let a value inside a literal choose which handler answers",
     "crates/sankhya-api-pg/src/catalog.rs",
     "    let structure = without_literals(&compact);",
     "    let structure = compact.clone();",
     "sankhya-api-pg"),

    ("wire: read a catalogue filter from the projection rather than the WHERE clause",
     "crates/sankhya-api-pg/src/catalog.rs",
     "    let mut from = 0usize;\n    while let Some(found) = text.get(from..)?.find(column) {",
     "    let mut from = 0usize;\n    while let Some(found) = text.get(from..).filter(|_| from == 0)?.find(column) {",
     "sankhya-api-pg"),

    ("wire: answer a column query without narrowing it to the schema it named",
     "crates/sankhya-api-pg/src/catalog.rs",
     "                .filter(|t| schema.as_ref().is_none_or(|named| &t.schema == named))",
     "                .filter(|t| schema.as_ref().is_none_or(|named| &t.schema != named) || true)",
     "sankhya-api-pg"),

    ("wire: discard a prepared statement's SQL, as the extended protocol did",
     "crates/sankhya-api-pg/src/session.rs",
     "                self.statements.insert(name, sql);",
     "                self.statements.insert(name, String::new());",
     "sankhya-api-pg"),

    ("wire: run a bound portal without the parameters bound to it",
     "crates/sankhya-api-pg/src/session.rs",
     "                        let sql = substitute(sql, &parameters);",
     "                        let sql = substitute(sql, &[]);",
     "sankhya-api-pg"),

    ("server: leave a newly created table out of the servable set until a restart",
     "crates/sankhya-server/src/adopt.rs",
     "        table.authorize_as = authority_for(&table.reference, &lineages);\n        servable.push(table);",
     "        table.authorize_as = authority_for(&table.reference, &lineages);\n        let _ = table;",
     "sankhya-server"),

    # No entry for "resolve a clone through the ordinary path", and the absence is deliberate.
    #
    # A clone is resolved in **two** places -- `warehouse::servable` when it is adopted, and
    # `warehouse::refresh` when its log moves -- and each masks the other. Break adoption and
    # the next statement's refresh repairs it; break refresh and adoption has already done it
    # right. A single-site mutation therefore survives while the defect it names is real, which
    # is the definition of a mutation that would pass without proving anything.
    #
    # The behaviour is guarded end to end instead, by `clone_questions.rs`:
    # `a_clone_reads_its_origins_rows_rather_than_none` and
    # `a_clone_of_a_clone_reads_the_same_rows_as_the_root`. Recorded here rather than left as a
    # gap, because the next person to notice the missing entry deserves the reason.

    ("server: splice a clone one level, so a clone of a clone reads as nothing",
     "crates/sankhya-server/src/warehouse.rs",
     "    while let Some(above) = lineage_at(&origin_root) {",
     "    while let Some(above) = lineage_at(&origin_root).filter(|_| false) {",
     "sankhya-server"),

    ("server: answer a user-class failure with the class code rather than its own",
     "crates/sankhya-server/src/execute.rs",
     "        sqlstate: specific_sqlstate(error)\n            .unwrap_or_else(|| statuses_for(classified.class()).sqlstate.as_str().to_string()),",
     "        sqlstate: statuses_for(classified.class()).sqlstate.as_str().to_string(),",
     "sankhya-server"),

    ("server: answer a statement whose sampling clause is parsed and ignored",
     "crates/sankhya-server/src/execute.rs",
     "    refuse_if_silently_ignored(sql)?;",
     "    let _ = refuse_if_silently_ignored(sql);",
     "sankhya-server"),

    ("server: refuse the statements a driver sends around a query",
     "crates/sankhya-server/src/wiring.rs",
     "        if let Some(answer) = crate::driver::run_session_statement(dispatch) {",
     "        if let Some(answer) = None::<Result<QueryResult, QueryFailure>> {",
     "sankhya-server"),

    ("server: answer ROLLBACK with success, which is the one lie that matters",
     "crates/sankhya-server/src/driver.rs",
     "        \"ROLLBACK\" | \"ABORT\" => {",
     "        \"ROLLBACK__never\" | \"ABORT__never\" => {",
     "sankhya-server"),

    ("server: give up on cube descriptions when the warehouse holds no cubes",
     "crates/sankhya-server/src/wiring.rs",
     "        let cubes = self.cubes();\n        // No early return on an empty set",
     "        let cubes = self.cubes();\n        if cubes.is_empty() { return; }\n        // No early return on an empty set",
     "sankhya-server"),

    # The sharpest defect the 2026-09-01 review found: a measure's declared rule was validated
    # and then ignored when the number was computed.
    # No entry for "reduce every cell by sum", and the absence is honest rather than an
    # oversight.
    #
    # `navigate::roll_up` already reduces along the dimension being rolled away, using the rule
    # `permits` reads from the measure. By the time `batch` runs, each cell of a rolled result
    # holds one value --- so the hardcoded `Rule::Sum` it used was **masked** there, and
    # replacing it with the declared rule changes nothing a roll-up can observe.
    #
    # The rule now read in `batch` is still the right one: it governs cells that hold several
    # contributions, which is the base grain and the slice path, and a hardcoded sum there was
    # arbitrary. But no test currently fails without it, and claiming a mutation is caught when
    # it is not is exactly the failure this tool exists to prevent.
    #
    # An adversarial review on 2026-09-01 reported `MEAN ALONG region` answering with the
    # total. That has **not been reproduced** and is recorded as open in `STATUS.md`.

    ("cube: take the kept dimension's rule rather than the one being rolled away",
     "crates/sankhya-cube-sql/src/functions.rs",
     "    let applying: Vec<&Along> = measure\n        .rules\n        .iter()\n        .filter(|along| !kept.contains(&along.dimension))",
     "    let applying: Vec<&Along> = measure\n        .rules\n        .iter()\n        .filter(|along| kept.contains(&along.dimension))",
     "sankhya-cube-sql"),

    ("cube: resolve disagreeing reduction rules by taking the first",
     "crates/sankhya-cube-sql/src/functions.rs",
     "        [only] => Ok(*only),",
     "        [only] => Ok(*only),\n        [first, ..] if true => Ok(*first),",
     "sankhya-cube-sql"),

    # `ADR-0017` Decision 2: the names a refusal cites travel as data, never only in prose.
    ("wire: leave the names a refusal cites out of the message it sends",
     "crates/sankhya-api-pg/src/message.rs",
     "            if !subjects.is_empty() {",
     "            if false {",
     "sankhya-api-pg"),

    ("wire: send a hint field even when a refusal cites nothing",
     "crates/sankhya-api-pg/src/message.rs",
     "            if !subjects.is_empty() {",
     "            if true {",
     "sankhya-api-pg"),

    ("server: flatten the clones a drop refusal names into its sentence alone",
     "crates/sankhya-server/src/wiring.rs",
     "                sankhya_clone::Refused::StillRead { by, .. } => by.clone(),",
     "                sankhya_clone::Refused::StillRead { by, .. } => { let _ = by; Vec::new() }",
     "sankhya-server"),

    ("server: let a statement run with no deadline, as the query path did",
     "crates/sankhya-server/src/execute.rs",
     "    let collected = tokio::time::timeout(statement_deadline(), async {",
     "    let collected = tokio::time::timeout(std::time::Duration::from_secs(86_400), async {",
     "sankhya-server"),

    # `ADR-0017` Decision 5: version skew is a connection-time refusal.
    ("wire: serve a client whose contract this server does not speak",
     "crates/sankhya-api-pg/src/session.rs",
     "                if let Err(failure) = admits_contract(&self.parameters) {",
     "                if let Err(failure) = Ok::<(), QueryFailure>(()) {",
     "sankhya-api-pg"),

    ("wire: refuse a generic driver, which declares no contract at all",
     "crates/sankhya-api-pg/src/session.rs",
     "    else {\n        return Ok(());\n    };",
     "    else {\n        return Err(QueryFailure { sqlstate: \"08004\".to_owned(), message: String::new(), detail: None, subjects: Vec::new() });\n    };",
     "sankhya-api-pg"),

    # `FR-SEC-02`: a principal is carried unchanged through planning, execution and audit. It
    # could not be --- the trait had no parameter for one.
    ("server: authorize every statement as a constant rather than as the caller",
     "crates/sankhya-server/src/wiring.rs",
     "        let user = caller.user();",
     "        let user = \"query\";",
     "sankhya-server"),

    # No entry for "answer the catalogue for a constant", and the absence is honest.
    #
    # The catalogue *is* now answered for whoever asked, and no test can tell: this build gives
    # every user the same role and the same tenant, so `visible_tables` returns the same list
    # whichever subject it is asked about. The plumbing is fixed; nothing yet varies along it.
    #
    # It becomes observable the day a policy distinguishes subjects --- which is `FR-SEC-03`'s
    # federated identity --- and the entry belongs there rather than here, claiming a guard that
    # does not exist yet.

    # `ADR-0019` Decision 6: `SET SNAPSHOT` is the first setting that would change an answer,
    # and accepting it as a no-op would serve the present to a caller who asked for one instant.
    ("server: accept a setting that would change an answer, as a no-op",
     "crates/sankhya-server/src/driver.rs",
     "    if matches!(first, \"SET\" | \"RESET\") {",
     "    if false {",
     "sankhya-server"),

    ("server: refuse every setting rather than the ones that change an answer",
     "crates/sankhya-server/src/driver.rs",
     "        if CHANGES_AN_ANSWER.contains(&named) {",
     "        if true {",
     "sankhya-server"),

    # M17, `ADR-0019`. A snapshot pins files, so every guard here is a guard on reclamation.
    ("snapshot: resolve a table the snapshot does not name to its current version",
     "crates/sankhya-snapshot/src/model.rs",
     "        self.tables.get(qualified).copied()",
     "        Some(self.tables.get(qualified).copied().unwrap_or(Pinned { version: u64::MAX }))",
     "sankhya-snapshot"),

    ("snapshot: accept a lifetime of zero days",
     "crates/sankhya-snapshot/src/expire.rs",
     "        if days == 0 {\n            return Err(Unaskable::Immediate);\n        }",
     "        if false {\n            return Err(Unaskable::Immediate);\n        }",
     "sankhya-snapshot"),

    ("snapshot: accept a lifetime longer than this system will hold storage for",
     "crates/sankhya-snapshot/src/expire.rs",
     "        if days > LONGEST_DAYS {",
     "        if false {",
     "sankhya-snapshot"),

    ("snapshot: expire a snapshot on its last day rather than after it",
     "crates/sankhya-snapshot/src/expire.rs",
     "    if today > snapshot.expires_on {",
     "    if today >= snapshot.expires_on {",
     "sankhya-snapshot"),

    ("snapshot: let an expiry wrap into the past on a distant day",
     "crates/sankhya-snapshot/src/expire.rs",
     "    taken_on.saturating_add(i32::try_from(self.days).unwrap_or(i32::MAX))",
     "    taken_on.wrapping_add(i32::try_from(self.days).unwrap_or(i32::MAX))",
     "sankhya-snapshot"),

    ("snapshot: supply a default lifetime where the statement gave none",
     "crates/sankhya-snapshot/src/statement.rs",
     "    if words.len() <= 3 {\n        return Err(NotAStatement::NoExpiry);\n    }",
     "    if words.len() <= 3 {\n        return Ok(Statement::Create { name, expiry: Expiry::days(30).unwrap_or_else(|_| unreachable!()) });\n    }",
     "sankhya-snapshot"),

    ("snapshot: claim every SHOW statement rather than SHOW SNAPSHOTS",
     "crates/sankhya-snapshot/src/statement.rs",
     "        && second.is_some_and(|word| word.eq_ignore_ascii_case(\"SNAPSHOTS\"))",
     "        && second.is_some()",
     "sankhya-snapshot"),

    ("server: keep pinning the files of a snapshot that has expired",
     "crates/sankhya-server/src/snapshots.rs",
     "    for snapshot in snapshots {\n        if standing(snapshot, today) == Standing::Expired {\n            continue;\n        }",
     "    for snapshot in snapshots {\n        if false {\n            continue;\n        }",
     "sankhya-server"),

    # The second site: the listing that tells a person *which* snapshot is holding a version.
    # An expired one appearing there sends somebody to drop a snapshot that is already gone.
    ("server: name an expired snapshot as a reason a version is still pinned",
     "crates/sankhya-server/src/snapshots.rs",
     "    for snapshot in &snapshots {\n        if standing(snapshot, today) == Standing::Expired {\n            continue;\n        }",
     "    for snapshot in &snapshots {\n        if false {\n            continue;\n        }",
     "sankhya-server"),

    ("server: replace a snapshot whose name is already taken",
     "crates/sankhya-server/src/snapshots.rs",
     "    if path.exists() {",
     "    if false {",
     "sankhya-server"),

    # No entry for "snapshot every table rather than the ones the caller may read", and the
    # absence is honest rather than an oversight.
    #
    # The filter is there and correct --- a snapshot recording a table its taker could not read
    # would disclose that table's existence to everyone who can list snapshots. It cannot be
    # *observed* in this build: every user has the same role and the same tenant, so the filter
    # admits every table whichever subject it is asked about.
    #
    # Same shape as the absent entry for `visible_tables`, and it becomes observable at the same
    # moment: when `FR-SEC-03`'s federated identity makes a policy distinguish subjects.

    ("readpath: resolve a table at its present rather than at the version asked for",
     "crates/sankhya-readpath/src/provider.rs",
     "                    Some(version) => sankhya_table_delta::live_files_at(table_root, version)?,",
     "                    Some(_version) => live_files(table_root)?,",
     "sankhya-readpath"),

    ("server: ignore a session's snapshot and answer from the present",
     "crates/sankhya-server/src/snapshots.rs",
     "    let Some(named) = caller.setting(\"snapshot\") else {",
     "    let Some(named) = None::<&str> else {",
     "sankhya-server"),

    # The property that makes a snapshot not a clone, and nothing demonstrated it until
    # 2026-09-02: the test that read as of one moved a single table, which proves the setting
    # is honoured and says nothing about many tables sharing one instant.
    ("server: pin each table at its own version rather than at the snapshot's",
     "crates/sankhya-server/src/snapshots.rs",
     "        let Some(at) = snapshot.pins(&qualified) else {",
     "        let Some(at) = snapshot.pins(&qualified).map(|_| Pinned { version: u64::MAX }) else {",
     "sankhya-server"),

    ("server: answer a table the snapshot does not name from the present",
     "crates/sankhya-server/src/snapshots.rs",
     "        let Some(at) = snapshot.pins(&qualified) else {\n            // Not named by this snapshot: it did not exist when the snapshot was taken, so it\n            // is left out and a statement naming it fails to resolve.\n            continue;\n        };",
     "        let Some(at) = snapshot.pins(&qualified) else {\n            pinned.push(table.clone());\n            continue;\n        };",
     "sankhya-server"),

    # A single leading `--` made the server fail to recognise its own statements, and every
    # script this repository ships as an example comments its statements.
    ("wire: dispatch on the raw text, so a leading comment hides the statement",
     "crates/sankhya-server/src/wiring.rs",
     "        let dispatch = sankhya_api_pg::catalog::without_leading_comments(sql);",
     "        let dispatch = sql;",
     "sankhya-server"),

    ("wire: stop at the first `*/`, so a nested block comment leaks its tail",
     "crates/sankhya-api-pg/src/catalog.rs",
     "                    (b'/', b'*') => {\n                        depth += 1;\n                        at += 2;\n                    }",
     "                    (b'/', b'*') => at += 2,",
     "sankhya-server"),

    # No entry for "treat an unterminated block comment as though it ended". The guard is there
    # and is correct --- an unterminated comment swallows the whole statement, so there is
    # nothing to dispatch on --- but it is **unobservable**, and an entry claiming otherwise
    # would be worse than none.
    #
    # Without it, the scan leaves the last one or two characters of the text as the dispatch
    # view: a fragment no handler claims, so the statement reaches the engine, whose tokenizer
    # reports the unterminated comment in its own words. With it, the view is empty and the
    # statement reaches the engine, which says the same thing. The engine sees the original
    # text on both paths, which is the whole point of this being a recogniser's view.
    #
    # The guard stays because a fragment offered to a future handler is a bug waiting for a
    # handler that matches short strings. It is defence, not behaviour, and the catalogue says
    # so rather than pretending a test covers it.

    # M21. A vector was sent as `text`, so a client received `[1.0, 2.0]` and had to parse it.
    ("server: send a vector as text rather than as an array of doubles",
     "crates/sankhya-server/src/execute.rs",
     "        DataType::List(item) | DataType::LargeList(item) | DataType::FixedSizeList(item, _)\n            if matches!(item.data_type(), DataType::Float64 | DataType::Float32) =>\n        {\n            (oid::FLOAT8_ARRAY, -1)\n        }",
     "",
     "sankhya-server"),

    ("server: announce an array type and send Arrow's rendering under it",
     "crates/sankhya-server/src/execute.rs",
     "            render_double_array(array, row)",
     "            format!(\"{:?}\", array.data_type())",
     "sankhya-server"),

    # M21. `publish_table` compared a batch against the schema it was HANDED; `append` --- the
    # path feeds, compaction and every test use --- compared nothing at all.
    ("publish: write a batch without checking it against the table's own schema",
     "crates/sankhya-publish/src/publish.rs",
     "        self.batch_agrees_with_the_table(batch)?;",
     "",
     "sankhya-publish"),

    ("publish: accept a vector of a width the column did not declare",
     "crates/sankhya-publish/src/publish.rs",
     "                Some((_, supplied)) if !same_to_the_format(field, supplied) => {",
     "                if false {",
     "sankhya-publish"),

    ("publish: accept a batch that omits a column the table says cannot be null",
     "crates/sankhya-publish/src/publish.rs",
     "                None if !field.is_nullable() => {",
     "                None if false => {",
     "sankhya-publish"),

    # Named against `sankhya-feed`: comparing raw Arrow types instead of rendered ones reports
    # a contradiction for every type richer than the format can express, and the quarantine
    # table --- which writes UTC-aware timestamps into a column its metadata calls naive --- is
    # what notices.
    ("publish: compare Arrow types rather than what the format records",
     "crates/sankhya-publish/src/publish.rs",
     "    match (render(declared), render(offered)) {\n        (Some(left), Some(right)) => left == right,",
     "    match (render(declared), render(offered)) {\n        (Some(_), Some(_)) => declared.data_type() == offered.data_type(),",
     "sankhya-feed"),

    # The schema check made the write path read the whole log per append --- quadratic in a
    # table's own history, which is the defect this repository was bitten by once already. It
    # showed up as `check-concurrency` failing its throughput floor within an hour.
    # No entry for "read the declared schema on every append rather than once". The caching is
    # **not observable in behaviour** --- both routes validate against the same schema and
    # every assertion passes either way. What differs is cost: reading the log per append is
    # quadratic in the table's own history.
    #
    # The property is held by `check-concurrency`, which is where it was caught: the throughput
    # floor stopped being met within an hour of the check being written. That measurement is
    # `#[ignore]`d in an ordinary test run --- deliberately, since a throughput number taken
    # while the workspace suite saturates the machine describes the machine --- so no crate's
    # default run notices, and a catalogue entry claiming otherwise would be false.

    # A stored matrix column carries no shape --- a `FixedSizeList` read back from Parquet has
    # no tensor metadata --- so requiring one made four functions unusable on real data.
    ("olap: refuse a square-only operation on a stored matrix column",
     "crates/sankhya-olap/src/matrices.rs",
     "        let deduced = self\n            .operation\n            .needs_square()\n            .then(|| first_length(&args).and_then(square_order))\n            .flatten();",
     "        let deduced = None;",
     "sankhya-olap"),

    ("olap: deduce a square shape for an operation where the guess is real",
     "crates/sankhya-olap/src/matrices.rs",
     "    const fn needs_square(self) -> bool {\n        matches!(self, Self::Determinant | Self::Trace | Self::Inverse | Self::Solve)\n    }",
     "    const fn needs_square(self) -> bool {\n        true\n    }",
     "sankhya-olap"),

    # M21. Time series, finance and risk. Every one is a wrong number somebody acts on.
    ("math: pad a rolling window's leading positions instead of reporting nothing",
     "crates/sankhya-math/src/timeseries.rs",
     "            if at + 1 < window {\n                return None;\n            }\n            values\n                .get(at + 1 - window..=at)\n                .map(|slice| deterministic_sum(slice) / divisor)",
     "            values\n                .get(at.saturating_sub(window - 1)..=at)\n                .map(|slice| deterministic_sum(slice) / divisor)",
     "sankhya-math"),

    # --- Phase 4: a password checked against something -----------------------------------------

    # `SEC-01`. Until this there was no credential store, no hash and no comparison anywhere in
    # the workspace: the entire check was that a password had been *presented* and was
    # non-empty, and the username is self-asserted.
    ("server: accept any password once credentials are configured",
     "crates/sankhya-server/src/wiring.rs",
     "        let verified = self\n            .settings\n            .credentials\n            .get(user)\n            .is_some_and(|verifier| verifier.verifies(presented));",
     "        let verified = !presented.is_empty();",
     "sankhya-server"),

    # A user with no credential must be refused, not waved through. Forgetting somebody in this
    # direction grants them access, which is the rule `server.users` already states.
    ("server: wave through a user who has no credential written down",
     "crates/sankhya-server/src/wiring.rs",
     "            .get(user)\n            .is_some_and(|verifier| verifier.verifies(presented));",
     "            .values()\n            .any(|verifier| verifier.verifies(presented));",
     "sankhya-server"),

    # Telling "no such user" apart from "wrong password" turns the login into a directory of
    # who exists here.
    ("server: say whether the user exists when refusing a password",
     "crates/sankhya-server/src/wiring.rs",
     "                \"password authentication failed for this user\",",
     "                &format!(\"no credential for `{user}`, or the password is wrong\"),",
     "sankhya-server"),

    # The derivation itself. A verifier that accepts anything is the defect wearing the fix's
    # clothes, and nothing above this line would notice.
    ("credential: accept any password against any verifier",
     "crates/sankhya-credential/src/lib.rs",
     "        .is_ok()\n    }",
     "        .is_ok()\n            || true\n    }",
     "sankhya-credential"),

    # The iteration count is read from the stored line so a raised default does not invalidate
    # existing credentials. Ignoring it makes every stored verifier fail to verify.
    ("credential: derive with the current default rather than the stored count",
     "crates/sankhya-credential/src/lib.rs",
     "            self.iterations,\n            &self.salt,",
     "            std::num::NonZeroU32::new(ITERATIONS).unwrap_or(self.iterations),\n            &self.salt,",
     "sankhya-credential"),

    # An empty salt makes every verifier of one password identical, which tells anybody holding
    # the file who shares a password before any of them is cracked.
    ("credential: make a verifier with no salt",
     "crates/sankhya-credential/src/lib.rs",
     "    if salt.is_empty() {\n        return None;\n    }",
     "    if false {\n        return None;\n    }",
     "sankhya-credential"),

    # --- Phase 3: snapshots, and what a cached answer is an answer to ---------------------------

    # There is deliberately **no mutation for the snapshot's second read** (`COR-22`).
    #
    # One was written --- replace the confirming pass with a copy of the first --- and it
    # survived, correctly. The confirmation is only observable while something else is
    # committing: on a quiet warehouse the two passes agree by construction, so a copy of the
    # first is indistinguishable from a second read of the same thing.
    #
    # Catching it needs a writer committing continuously through the snapshot, and the
    # assertion would then be "this eventually refuses" against five attempts --- which passes
    # or fails on how fast the machine is. A test contorted until it fails is worse than no
    # test, and a flaky gate is worse than an uncaught mutation.
    #
    # Same rule as the fsync calls: a mutation nothing can catch is not evidence of coverage,
    # it is a permanent survivor that trains people to ignore the list. `COR-21`'s entry below
    # covers the snapshot path that *is* observable.

    # `COR-21`. `live_files_at` replays up to a version and stops, so asking for one beyond the
    # log silently answers with the newest --- a version nobody has, served as though they had
    # it. `SET VERSION OF` checks this; the snapshot path did not.
    ("server: set a snapshot pinning a version its table no longer has",
     "crates/sankhya-server/src/snapshots.rs",
     "        if !commits.iter().any(|(at, _)| *at == pinned.version) {",
     "        if false {",
     "sankhya-server"),

    # `COR-20`. Without the pin in the key, a pinned session's cells go into the cache under
    # the *present* version and the next unpinned session is served them --- a read whose whole
    # promise is that it does not move, leaking into reads that promise the opposite.
    ("server: key a hydration without saying which position the session read from",
     "crates/sankhya-server/src/wiring.rs",
     "        self.register_cubes(&context, &principal, sql, caller.position_digest());",
     "        self.register_cubes(&context, &principal, sql, 0);",
     "sankhya-server"),

    # And the digest itself: folding rather than summing, because a cache key that collides
    # serves one session's position to another.
    ("session: treat every pinned session as reading the same position",
     "crates/sankhya-api-pg/src/session.rs",
     "            let pins = name.eq_ignore_ascii_case(\"snapshot\") || name.starts_with(\"version of \");",
     "            let pins = false;",
     "sankhya-api-pg"),

    # --- Phase 4: a mask that is applied ---------------------------------------------------------

    # `SEC-02`. `mask_for` and `masked_columns` reported what the policy said and `scan` read
    # neither, so every masked column returned its real value. Wrapping the scan is what makes
    # the control a control; without it the whole feature is a field in a struct.
    ("catalog: report the masks and return the values anyway",
     "crates/sankhya-catalog/src/secured.rs",
     "        if !self.masking.touches(&schema) {",
     "        if true {",
     "sankhya-catalog"),

    # A mask on a column of a type it cannot be applied to has to be refused when the table is
    # opened. Letting it through means finding out on the first query that selects the column,
    # which is a policy that is wrong for months and looks right.
    ("catalog: accept a text mask over a column that holds no text",
     "crates/sankhya-catalog/src/mask.rs",
     "            if !textual && !matches!(mask, Mask::Null) {",
     "            if false {",
     "sankhya-catalog"),

    # And a mask naming a column the table does not have. It obscures nothing and looks like a
    # control, which is the failure `SEC-02` was in the first place.
    ("catalog: accept a mask over a column that does not exist",
     "crates/sankhya-catalog/src/mask.rs",
     "            let Ok(index) = schema.index_of(name) else {",
     "            let Ok(index) = schema.index_of(name).or(Ok::<usize, ()>(0)) else {",
     "sankhya-catalog"),

    # The channel a mask closes only if pushdown is refused. A predicate on a masked column
    # evaluated below the mask asks the real value a question and answers it in the row count,
    # without ever printing the value.
    ("catalog: let a predicate on a masked column reach the provider",
     "crates/sankhya-catalog/src/secured.rs",
     "            if self.masking.reads_a_masked_column(&columns_of(filter)) {",
     "            if false {",
     "sankhya-catalog"),

    # A partial mask keeps the *last* `keep` characters. Keeping the first instead reveals the
    # local part of an address and the start of a name, which is the half that identifies.
    ("catalog: keep the head of a partially masked value rather than the tail",
     "crates/sankhya-catalog/src/mask.rs",
     "    out.extend(text.chars().skip(hidden));",
     "    out.extend(text.chars().take(keep));",
     "sankhya-catalog"),

    # A constant mask replaces the nulls too. Leaving them null publishes which rows have no
    # value, and \"this customer has no email address\" is a fact about that customer.
    ("catalog: leave a null unmasked under a constant mask",
     "crates/sankhya-catalog/src/mask.rs",
     "            Mask::Constant { value } => constant(value, &self.kind, values.len()),",
     "            Mask::Constant { .. } => Arc::clone(&values),",
     "sankhya-catalog"),

    # --- Phase 4: what the audit records, and whether it survives --------------------------------

    # `SEC-07`. `Chain` is a `Vec`, so the hash-linked tamper-evident audit was erased by a
    # restart --- and a restart is the event most likely to accompany the incident it exists for.
    ("server: keep the audit in memory and never write it down",
     "crates/sankhya-server/src/audit.rs",
     "            .zip(journal.as_mut())",
     "            .zip(journal.as_mut().filter(|_| false))",
     "sankhya-server"),

    # And reading it back, without which every restart starts a new chain that links to nothing.
    ("server: start a new chain at every restart",
     "crates/sankhya-server/src/audit.rs",
     "    let (chain, mut complaints) = sankhya_audit::journal::read_windowed(",
     "    let (chain, mut complaints) = (sankhya_audit::Chain::new(), Vec::new()); let _ = (",
     "sankhya-server"),

    # The record's contents. The only append site hardcoded no row filter and no masks, never
    # recorded the version, the statement or the rows returned, and passed the first two words of
    # the statement where a table belongs.
    ("server: record a read without saying what was asked",
     "crates/sankhya-server/src/audit.rs",
     "    let entry = Entry::by(principal, table, Action::Read, decision, at)\n        .running(statement_shape(sql))\n        .from_version(version);",
     "    let entry = Entry::by(principal, table, Action::Read, decision, at);",
     "sankhya-server"),

    # And the table it was asked of, taken from the plan rather than from the session. Recording
    # every authorized table attributes a row count to tables nobody read, and a wrong fact in an
    # audit is read as a fact.
    ("server: record every table the session could reach rather than the ones it did",
     "crates/sankhya-server/src/execute.rs",
     "    let touched = scanned(&plan);",
     "    let touched = Vec::new();",
     "sankhya-server"),

    # The restriction a statement was answered under, which §13.5 lists as not optional and which
    # was a hardcoded `None` --- so a query answered under a filter was recorded as one answered
    # under none.
    ("server: record every read as unrestricted",
     "crates/sankhya-server/src/execute.rs",
     "            guard.row_filter().map(str::to_owned),",
     "            None,",
     "sankhya-server"),

    # --- Phase 5: what a statement may spend -------------------------------------------------------

    # `OPS-05`. The whole result was materialised and *then* counted against the limit, so a
    # statement returning ten million rows against a limit of ten thousand allocated all ten
    # million first. A bound enforced by a check that runs afterwards is not a bound.
    ("server: count the rows against the limit once they are all in memory",
     "crates/sankhya-server/src/execute.rs",
     "            if total > max_rows {",
     "            if false {",
     "sankhya-server", 1, "disclosure"),

    # `OPS-06`, `OPS-07`. DataFusion runs on an unbounded pool unless it is given one, and there
    # was no `MemoryPool`, no `FairSpillPool` and no `DiskManager` anywhere in the workspace ---
    # so one statement could take the machine down and every other connection with it.
    ("server: run every query on an unbounded memory pool",
     "crates/sankhya-server/src/execute.rs",
     "    let context = bounded_session();",
     "    let context = SessionContext::new();",
     "sankhya-server", 1, "disclosure"),

    # And the bound itself, which a zero would make meaningless in the other direction.
    ("server: bound a query's memory at whatever the environment says, including nothing",
     "crates/sankhya-server/src/execute.rs",
     "        .filter(|bytes| *bytes > 0)",
     "        .filter(|_bytes| true)",
     "sankhya-server", 1, "disclosure"),

    # --- Phase 5: what a running process keeps -----------------------------------------------------

    # `OPS-04`. The records were a `Vec` that only ever grew, appended on every statement and
    # every catalogue listing --- roughly 3 to 5 GB a day at a hundred statements a second.
    ("audit: keep every record a running process has ever made",
     "crates/sankhya-audit/src/chain.rs",
     "        while self.records.len() > window {",
     "        while false {",
     "sankhya-audit"),

    # And the two figures that describe the whole chain rather than the part still held. A count
    # that shrank when records aged out is one nobody can compare against what they mirrored,
    # and comparing it is the only way a truncated chain is ever noticed.
    ("audit: report the window's length as the chain's",
     "crates/sankhya-audit/src/chain.rs",
     "        usize::try_from(self.total).unwrap_or(usize::MAX)",
     "        self.records.len()",
     "sankhya-audit"),

    # A windowed chain checks the links it has. Comparing against a position of zero reports the
    # first record it kept as out of order, which turns every long-running server's audit into
    # one that "does not verify".
    ("audit: verify a window as though it began the chain",
     "crates/sankhya-audit/src/chain.rs",
     "            .map_or(0, |record| record.sequence);",
     "            .map_or(0, |record| record.sequence * 0);",
     "sankhya-audit"),

    # The timestamp. `*clock += 1` gave an audit whose times were 1, 2, 3 and restarted at 1 on
    # every boot --- so no record could be placed against anything that happened.
    ("server: time an audit record by a counter rather than by the clock",
     "crates/sankhya-server/src/audit.rs",
     "    let at = server.now_micros();",
     "    let at = 0;",
     "sankhya-server", 1, "disclosure"),

    # --- Phase 4: a policy the binary can be configured with -------------------------------------

    # `SEC-15`. `start()` --- the only path the shipped binary takes --- built `permissive_policy`
    # and no configuration key loaded a policy set at all, so the row-predicate enforcement had
    # never run outside a test.
    ("server: ignore the policy an operator configured",
     "crates/sankhya-server/src/wiring.rs",
     "    let policy = settings\n        .policy\n        .clone()\n        .unwrap_or_else(|| permissive_policy(&settings.tenant, &tables));",
     "    let policy = permissive_policy(&settings.tenant, &tables);",
     "sankhya-server", 1, "disclosure"),

    # A rule that does not parse is refused rather than skipped. A policy with a rule silently
    # dropped permits more than it says, and whoever wrote it believes it is in force.
    ("server: skip a policy rule that names a table without its schema",
     "crates/sankhya-server/src/policy.rs",
     "    let Some((schema, bare)) = table.split_once('.') else {",
     "    let Some((schema, bare)) = table.split_once('.').or(Some((\"\", table.as_str()))) else {",
     "sankhya-server", 1, "disclosure"),

    # And the row predicate itself, which is the half that had never reached a deployment.
    ("server: read a rule's predicate and not apply it",
     "crates/sankhya-server/src/policy.rs",
     "    if let Some(predicate) = fields.get(\"where\") {",
     "    if let Some(predicate) = fields.get(\"where\").filter(|_| false) {",
     "sankhya-server", 1, "disclosure"),

    # And the masks beside it.
    ("server: read a rule's masks and not apply them",
     "crates/sankhya-server/src/policy.rs",
     "        let Some(column) = field.strip_prefix(\"mask.\") else {",
     "        let Some(column) = field.strip_prefix(\"\\u{0}\") else {",
     "sankhya-server", 1, "disclosure"),

    # --- Phase 4: what an unauthenticated endpoint says ------------------------------------------

    # `SEC-08`. `/metrics` is unauthenticated by convention and the per-table gauge's label is a
    # table's fully-qualified name, so the breakdown enumerated the warehouse to whoever could
    # reach the port. The module claimed the catalogue enforced this structurally; a label is
    # bounded by cardinality, not by content. A test asserted the label was present.
    ("server: name every table on an unauthenticated endpoint",
     "crates/sankhya-server/src/wiring.rs",
     "            if self.settings.metrics_detail {",
     "            if true {",
     "sankhya-server"),

    # And the figure that replaces it, which is what an alert fires on. A gauge that never moves
    # is an alert that never fires.
    ("server: report the largest table's file count as nothing",
     "crates/sankhya-server/src/wiring.rs",
     "            .set(&catalogue::TABLE_LIVE_FILES_MAX, &[], largest as f64);",
     "            .set(&catalogue::TABLE_LIVE_FILES_MAX, &[], 0.0);",
     "sankhya-server"),

    # --- Phase 4: what a refusal and a listing say ------------------------------------------------

    # `SEC-16`. `SELECT nosuchcol FROM orders` answered with every column of every table in the
    # plan's scope. The only mention of that phrase in the repository sniffed for it to choose a
    # SQLSTATE and passed it on.
    ("server: answer a misspelt column with the list of the real ones",
     "crates/sankhya-server/src/execute.rs",
     "            let said = without_the_column_list(&error.to_string());",
     "            let said = error.to_string();",
     "sankhya-server"),

    # And the marker itself. A dependency that reworded its message would stop matching and the
    # leak would return silently, which is why the test asks the planner for a real error rather
    # than handing this function a string.
    ("server: look for a column list nobody writes",
     "crates/sankhya-server/src/execute.rs",
     "    const ENUMERATION: &str = \"Valid fields are\";",
     "    const ENUMERATION: &str = \"\\u{0}\";",
     "sankhya-server"),

    # `SEC-17`. Counting bare-name claims over every servable table and authorizing afterwards
    # leaked twice: the refusal named schemas the caller cannot read, and --- with no string in
    # it at all --- a hidden table of the same name made the caller's own stop resolving.
    ("server: count a bare name's claims before deciding who may see them",
     "crates/sankhya-server/src/execute.rs",
     "    for (table, _) in &permitted {",
     "    for table in tables {",
     "sankhya-server"),

    # `SEC-18`. `cubes()` and `derived()` listed every cube to every caller, and `derived()`
    # emits the SQL text of each definition and the tables it reads.
    ("server: declare every cube to every caller",
     "crates/sankhya-server/src/wiring.rs",
     "            if self.scope_across(principal, cube.reads()).is_none() {\n                continue;\n            }\n            catalog.declare(cube.name());",
     "            catalog.declare(cube.name());",
     "sankhya-server"),

    # A snapshot row names the qualified tables it pins, so an unfiltered listing hands out the
    # shape of a warehouse.
    ("server: list every snapshot to every caller",
     "crates/sankhya-server/src/snapshots.rs",
     "                .filter(|snapshot| visible_to(server, principal, snapshot))",
     "                .filter(|_| true)",
     "sankhya-server"),

    # And a feed's halt reason, which `ADR-0018` fills with the file and the record that did not
    # fit. The rows are not filtered --- a feed hidden because its table is missing is the one an
    # operator opened the statement to find --- so the reason is where the rule lands.
    ("server: tell every caller why a feed halted",
     "crates/sankhya-server/src/feeds.rs",
     "                            Some(if readable {",
     "                            Some(if true {",
     "sankhya-server"),

    # And a user function's source, which is code its author wrote and whatever they put in it.
    ("server: publish every aggregation's source to every caller",
     "crates/sankhya-server/src/aggregations.rs",
     "                reviewable.then(|| aggregation.source.clone()),",
     "                Some(aggregation.source.clone()),",
     "sankhya-server"),

    # --- Phase 4: the boundary a user function runs behind ---------------------------------------

    # `SEC-09`. Mapping the namespace's root to the server's uid made the worker root *inside*
    # its namespace, and `execve` of a non-setuid file only drops capabilities when the
    # effective uid is not zero --- so the interpreter started holding every one of them.
    ("sandbox: make the worker root inside its own namespace",
     "crates/sankhya-sandbox/src/jail.rs",
     "const NOBODY: u32 = 65_534;",
     "const NOBODY: u32 = 0;",
     "sankhya-sandbox"),

    # `SEC-10`. `unshare(CLONE_NEWPID)` places the caller's *children* in the new namespace and
    # leaves the caller behind --- and the caller is the process that then execs into the
    # worker. Without the second fork the worker is in the host PID namespace, can see the
    # server, and shares its uid, so `os.kill(os.getppid(), 9)` succeeds.
    ("sandbox: leave the worker in the PID namespace it was started from",
     "crates/sankhya-sandbox/src/jail.rs",
     "    unsafe { establish(plan) }?;\n    unsafe { enter_pid_namespace() }",
     "    unsafe { establish(plan) }",
     "sankhya-sandbox"),

    # And the tether. Killing the intermediate is what the deadline does; without this the
    # worker is PID 1 of its own namespace and nothing reaps it.
    ("sandbox: let a killed worker outlive the process that started it",
     "crates/sankhya-sandbox/src/jail.rs",
     "    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0 {",
     "    if false {",
     "sankhya-sandbox"),

    # There is deliberately **no mutation for the descriptors the intermediate closes**.
    #
    # One was written --- keep them --- and it is caught: `Command::spawn` does not return until
    # every copy of its close-on-exec pipe is closed, so an intermediate holding one blocks the
    # spawn for the whole run, the deadline clock starts after the worker has finished, and
    # `a_function_that_never_returns_is_killed` reports `Failed` where it wants `OutOfTime`.
    #
    # It is not kept because of what it does to a *different* test.
    # `the_output_bound_fires_at_the_size_it_is_set_to` writes 12 MiB, and with the descriptors
    # held the parent is still inside `spawn` and reading nothing --- so the worker blocks on a
    # full pipe, burns no CPU, never trips `RLIMIT_CPU`, and never exits. The suite hangs rather
    # than failing, and the hang is inside `spawn`, where no deadline in this crate can reach it.
    #
    # A mutation that hangs the gate is worse than one that survives it: a survivor is a line on
    # a list somebody reads, and a hang is a build nobody can run. The behaviour is covered ---
    # by the test named above --- and this entry would only make the catalogue unrunnable.

    # `SEC-11`. The jail is what delivers *no subprocess*: there is nothing to exec because
    # nothing else is in it. Binding the interpreter's parent directory puts `/usr/bin` inside.
    ("udf: admit the directory the interpreter lives in rather than the interpreter",
     "crates/sankhya-udf/src/worker.rs",
     "    out.push(python.to_path_buf());",
     "    out.extend(python.parent().map(std::path::Path::to_path_buf));",
     "sankhya-udf"),

    # And the deny-list beside it, because the allow-list is built from what Python says it
    # loads from --- and Python says `/usr`.
    ("udf: admit a directory that holds a machine's programs",
     "crates/sankhya-udf/src/worker.rs",
     "        .filter(|path| !NEVER.iter().any(|never| path == Path::new(never)))",
     "        .filter(|_| true)",
     "sankhya-udf"),

    # There is deliberately **no mutation for the bounded wait on a stream** (`SEC-12`).
    #
    # One was written --- wait for ever instead --- and it survived, correctly, because the PID
    # namespace two files over makes the case it guards against impossible. The worker is PID 1
    # of a namespace holding nothing else, so when it exits the kernel kills everything else in
    # that namespace: there is no grandchild left to hold the write end of the pipe open.
    #
    # That is worth saying rather than deleting silently, because it is the *argument* for the
    # fork. The bounded wait is what the caller would need if a process could outlive the worker
    # and it cannot; the concurrent reading beside it is what fixed `SEC-12`, and the entry
    # below for the output bound covers that.

    # `SEC-13`. The output bound can only fire if the output is counted while it is being
    # written: a child writing past a pipe's capacity blocks, and is then killed at the
    # deadline and reported as having run out of time.
    ("sandbox: notice the output bound only once the run is over",
     "crates/sankhya-sandbox/src/lib.rs",
     "                    if overran(&answering) {",
     "                    if false {",
     "sankhya-sandbox"),

    # --- Phase 4: who a statement runs as --------------------------------------------------------

    # `SEC-03`. The subject was read from the metadata, checked non-empty, and discarded, so
    # every Flight request ran as one literal identity --- and a user an operator had
    # deliberately left out of the roles map connected there and was served.
    ("flight: run every request as the same identity",
     "crates/sankhya-server/src/flight.rs",
     "        let context = self.session_for(&caller.subject)?;",
     "        let context = self.session_for(\"flight\")?;",
     "sankhya-server"),

    # And at redemption, which is the half a leaked ticket travels through.
    ("flight: rebuild the session as whoever presents the ticket rather than whoever it names",
     "crates/sankhya-server/src/flight.rs",
     "        let context = self.session_for(ticket.subject())?;",
     "        let context = self.session_for(\"flight\")?;",
     "sankhya-server"),

    # A ticket carried a tenant and not a subject. Roles were once a property of the tenant, so
    # that named everything that mattered; roles became per-subject and the sentence stopped
    # being true with no code changing and no test failing.
    ("flight: admit a ticket on its tenant alone",
     "crates/sankhya-api-flight/src/ticket.rs",
     "        if self.tenant != presented_by.tenant || self.subject != presented_by.subject {",
     "        if self.tenant != presented_by.tenant {",
     "sankhya-api-flight"),

    # `SEC-04`. `DROP SNAPSHOT` took no principal at all. A snapshot holds files back from the
    # sweeper, so dropping one releases them --- the deletion `INVARIANTS.md` says the
    # maintenance scheduler is structurally incapable of performing.
    ("server: let anybody drop any snapshot",
     "crates/sankhya-server/src/snapshots.rs",
     "    if !mine && server.scope_across(principal, &pins).is_none() {",
     "    if false {",
     "sankhya-server"),

    # And a snapshot document nobody can read is refused rather than dropped: not being able to
    # tell what it pins is not permission to release it.
    ("server: drop a snapshot whose document cannot be read",
     "crates/sankhya-server/src/snapshots.rs",
     "    let held = read_one(&path).ok_or_else(|| {",
     "    let held = read_one(&path).or_else(|| Some(Snapshot::new(String::new(), String::new(), 0, 0, BTreeMap::new()))).ok_or_else(|| {",
     "sankhya-server"),

    # `RESUME FEED` restarts an ingest `ADR-0018` halted because its source changed shape. It
    # took no principal, so any caller could decide that records of an unknown shape should
    # start landing in somebody else's table again.
    ("server: let anybody resume any feed",
     "crates/sankhya-server/src/feeds.rs",
     "            if server.scope_for(principal, &table).is_none() {",
     "            if false {",
     "sankhya-server"),

    # --- Phase 4: a name in a statement is not a path -------------------------------------------

    # `SEC-06`. An allow-list is one line and has no tail; the deny-list it replaces has to stay
    # complete against `..`, a leading `/`, a NUL byte, a drive letter, a trailing dot Windows
    # strips, and a Unicode character that normalises to a separator.
    ("atomicfs: let a name hold whatever it likes",
     "crates/sankhya-atomicfs/src/name.rs",
     "    if let Some(character) = name.chars().find(|c| !permitted(*c)) {",
     "    if let Some(character) = name.chars().find(|_c| false) {",
     "sankhya-atomicfs"),

    # And the two that are a whole name rather than a character in one. `.` and `..` hold only
    # permitted characters and are the traversal.
    ("atomicfs: treat `..` as an ordinary name",
     "crates/sankhya-atomicfs/src/name.rs",
     "    if name == \".\" || name == \"..\" {",
     "    if false {",
     "sankhya-atomicfs"),

    # The check existing is not the check being reached. Each of the three statement families
    # had its own copy of `warehouse.join(DIRECTORY).join(format!(\"{name}.json\"))`, and the
    # crate that did restrict identifiers was the fourth one, which needed it least.
    ("server: build a snapshot's path from whatever the statement said",
     "crates/sankhya-server/src/snapshots.rs",
     "    let checked = sankhya_atomicfs::name::checked(name).map_err(|refused| {",
     "    let checked = Ok::<&str, sankhya_atomicfs::name::NotAName>(name).map_err(|refused| {",
     "sankhya-server"),

    ("server: build an aggregation's path from whatever the statement said",
     "crates/sankhya-server/src/aggregations.rs",
     "    let name = sankhya_atomicfs::name::checked(name)?;",
     "    let name = name;",
     "sankhya-server"),

    # The fourth site, which the audit did not name because it does not escape upward --- and
    # the reason is an accident. `..` holds a dot, so a name containing one is read as
    # `schema.table` and refused for naming the wrong schema. `sub/dir` holds no dot and lands
    # the clone in a directory the catalogue does not scan.
    ("server: place a clone wherever its name points",
     "crates/sankhya-server/src/warehouse.rs",
     "        if let Err(refused) = sankhya_atomicfs::name::checked(part) {",
     "        if let Err(refused) = Ok::<&str, sankhya_atomicfs::name::NotAName>(part) {",
     "sankhya-server"),

    ("cube: build a definition's path from whatever the statement said",
     "crates/sankhya-cube/src/catalogue.rs",
     "        sankhya_atomicfs::name::checked(name).map_err(|refused| CatalogueError::Name {",
     "        Ok::<&str, sankhya_atomicfs::name::NotAName>(name).map_err(|refused| CatalogueError::Name {",
     "sankhya-server"),

    # --- Phase 3: what a value looks like on the wire -------------------------------------------

    # `CLI-01`. Microseconds are this project's canonical unit and went out as a raw integer
    # under OID 1114/1184: `psql` printed 1756545242000000 and JDBC and psycopg raised. Zero
    # tests touched a timestamp.
    ("server: render a timestamp as the number of microseconds it is stored as",
     "crates/sankhya-server/src/execute.rs",
     "            raw.map_or_else(String::new, |micros| timestamp_text(micros, zone.is_some()))",
     "            raw.map_or_else(String::new, |micros| micros.to_string())",
     "sankhya-server"),

    # Floor division, so an instant before 1970 borrows from the day. Truncating toward zero
    # lands a day late with a negative time of day --- wrong on exactly one side of one
    # boundary, which is why nobody meets it.
    ("server: truncate a pre-epoch timestamp toward zero rather than toward the earlier day",
     "crates/sankhya-server/src/execute.rs",
     "    let days = micros.div_euclid(DAY);\n    let within = micros.rem_euclid(DAY);",
     "    let days = micros / DAY;\n    let within = micros % DAY;",
     "sankhya-server"),

    # `bytea` went out as bare hex. A driver strips `\x` and decodes the rest; given bare hex
    # it decodes the *characters*, so every byte comes back wrong and nothing errors.
    ("server: send bytea as bare hex, with no marker for a driver to strip",
     "crates/sankhya-server/src/execute.rs",
     "        DataType::Binary => format!(\"\\\\x{}\", hex(array.as_binary::<i32>().value(row))),",
     "        DataType::Binary => hex(array.as_binary::<i32>().value(row)),",
     "sankhya-server"),

    # `CLI-05`. Arrow writes 0.0 under a null, and this read the raw value buffer --- so the
    # composition the documentation advertises, whose leading nulls are deliberate, was
    # reduced against a price of nothing.
    ("functions: read a null element of a vector as the zero Arrow left in the buffer",
     "crates/sankhya-functions/src/rows.rs",
     "                if doubles.null_count() > 0 {\n                    return None;\n                }",
     "                if false {\n                    return None;\n                }",
     "sankhya-functions"),

    # The same, on the fixed-width path, where the child's nulls sit in this row's window.
    ("functions: ignore a null element in a fixed-width vector",
     "crates/sankhya-functions/src/rows.rs",
     "                    if (start..start + *width).any(|at| at < child.len() && child.is_null(at)) {",
     "                    if false {",
     "sankhya-functions"),

    # --- Phase 3: capture that lost or duplicated rows -----------------------------------------

    # `ING-01`. The position held records *published* and the resume skipped by *line index*.
    # Those agree only when every line so far fitted, so `[good, bad, good]` republished the
    # third line on restart --- one duplicate per preceding refusal or blank, silently.
    ("feed: record a resume position as rows published rather than lines read",
     "crates/sankhya-feed/src/run.rs",
     "            read_through = record.position.saturating_add(1);",
     "            read_through = skip + ran.published + fitted.len() as u64;",
     "sankhya-feed"),

    # `ING-05`. The batcher's match ended in `_ => {}` and `Truncate` fell into it: the source
    # table is emptied and the analytical copy keeps every row, permanently and silently.
    ("ingest: consume a truncation without acting on it",
     "crates/sankhya-ingest/src/pipeline.rs",
     "                    if state.quarantine.is_none() {",
     "                    if false {",
     "sankhya-ingest"),

    # And the other direction: a truncation names its own relations, so quarantining every
    # table stops feeding tables the source never truncated.
    ("ingest: quarantine every table when the source truncates one",
     "crates/sankhya-ingest/src/pipeline.rs",
     "                if let Some(state) = self.tables.get_mut(relation_id) {",
     "                let _ = relation_id;\n                for state in self.tables.values_mut() {",
     "sankhya-ingest"),

    # `ING-07`. The binder refuses string-to-number, number-to-date, float-to-int and an
    # over-scaled decimal --- and then `*value as f32` turned 1e308 into an infinity, one step
    # after it had said the value fitted.
    ("feed: narrow a float to a width that cannot hold it",
     "crates/sankhya-feed/src/shape.rs",
     "                        if value.is_finite() && !narrowed.is_finite() {",
     "                        if false {",
     "sankhya-feed"),

    # The same defect by the capture route: `parse::<f32>()` returns `Ok(inf)` rather than an
    # error for a value too large to represent.
    ("table: parse a float too large to represent as an infinity",
     "crates/sankhya-table/src/encode.rs",
     "            |s: &str| s.parse::<f32>().ok().filter(|value| {\n                value.is_finite()",
     "            |s: &str| s.parse::<f32>().ok().filter(|value| {\n                value.is_finite() || true",
     "sankhya-table"),

    # --- Phase 3: materialisation that changed the answer --------------------------------------

    # `COR-04`. Without the measure in the key the first measure materialised writes each
    # shape and every later one sees `exists()` and skips; reads build the same measure-free
    # key and label whatever comes back with the measure they asked for. A maintained `sales`
    # cube returned `amount`'s numbers under the name `ratio`.
    ("cube: key a materialised cuboid on its shape alone, not on which measure it holds",
     "crates/sankhya-cube/src/materialise.rs",
     "            self.measure.len(),\n            self.measure",
     "            0,\n            \"\"",
     "sankhya-cube"),

    # And the length prefix, for the reason the cube's own name has one: `a__b` on one cube and
    # `a` on a cube named `_b` must not render to the same table.
    ("cube: separate a measure name without prefixing its length",
     "crates/sankhya-cube/src/materialise.rs",
     "    let (length, rest) = rest.split_once('_')?;\n    let length: usize = length.parse().ok()?;\n    if rest.len() < length {\n        return None;\n    }\n    let (measure, mut rest) = rest.split_at(length);",
     "    let (measure, mut rest) = rest.split_once('_').map_or((rest, \"\"), |(a, b)| (a, b));",
     "sankhya-cube"),

    # `COR-05`. `to_batch` stored `exact_sum()` whatever the rule was, and `reduce`
    # early-returns a stored value for every rule --- so a measure declared `MAX ALONG region`
    # and maintained answered the sum. This is the 2026-09-01 defect `cube_rules.rs` was
    # written to pin, one layer down.
    ("cube: materialise every measure as a sum whatever its rule says",
     "crates/sankhya-cube/src/store.rs",
     "        let stored = if matches!(rule, Rule::Sum) {\n            contributions.exact_sum()\n        } else {\n            Exact::zero()\n        };",
     "        let stored = contributions.exact_sum();",
     "sankhya-cube"),

    # A rule with no reduction from partials has no value this layer can compute. Writing a
    # zero is materialisation turning a refusal into a number, which is the shape of the
    # finding.
    ("cube: materialise a measure that composes along nothing, as zero",
     "crates/sankhya-cube/src/store.rs",
     "        let Some(reduced) = contributions.reduce(rule) else {\n            continue;\n        };",
     "        let reduced = contributions.reduce(rule).unwrap_or(0.0);",
     "sankhya-cube"),

    # --- Phase 3: statements that were acknowledged and did nothing ----------------------------

    # `CLI-06`. `remember_setting` was called only from the simple-`Query` arm, so a client
    # using Parse/Bind/Execute got a success tag and every later statement read the present.
    # pgjdbc, psycopg3, asyncpg and SQLAlchemy all take this path by default.
    ("wire: acknowledge a setting on the extended protocol without recording it",
     "crates/sankhya-api-pg/src/session.rs",
     "                self.remember_setting(&portal.sql);",
     "",
     "sankhya-api-pg"),

    # `CLI-07`. A whitespace splitter cannot see the `=` inside `SNAPSHOT='eod'`, so the name
    # came out as `snapshot='eod'` and the value empty --- and the statement fell through to
    # the arm that accepts any `SET` as a no-op.
    ("wire: split a setting on whitespace before splitting it on its assignment",
     "crates/sankhya-api-pg/src/setting.rs",
     "    let (head, assigned) = match compact.split_once('=') {\n        Some((head, tail)) => (head.trim(), Some(tail.trim())),\n        None => (compact, None),\n    };",
     "    let (head, assigned) = (compact, None::<&str>);",
     "sankhya-api-pg"),

    # And the name, which decides whether two tables are two settings or one.
    ("wire: end a version setting's name at the first word",
     "crates/sankhya-api-pg/src/setting.rs",
     "        if let Some(table) = rest.get(1) {\n            name = format!(\"version of {}\", table.trim_matches('\"').to_lowercase());",
     "        if let Some(_table) = rest.get(1) {\n            name = \"version\".to_string();",
     "sankhya-api-pg"),

    # `CLI-09`. The `by` value was never checked against the cube's dimensions, so a misspelling
    # kept no dimension at all: the axis rolled away and a subtotal came back labelled as a
    # breakdown. `where` was checked; `by` was not.
    ("cube: roll up by a dimension the cube does not have",
     "crates/sankhya-cube-sql/src/functions.rs",
     "    if !unknown.is_empty() {",
     "    if false {",
     "sankhya-cube-sql"),

    # And the comparison, which was case-sensitive while every keyword in the file is not ---
    # so `by=Region` on a cube spelling it `region` collapsed the grain by a route nobody would
    # suspect of being a typo.
    ("cube: match a dimension name case-sensitively while every keyword is not",
     "crates/sankhya-cube-sql/src/functions.rs",
     "            .find(|dimension| dimension.eq_ignore_ascii_case(wanted))",
     "            .find(|dimension| *dimension == wanted)",
     "sankhya-cube-sql"),

    # --- Phase 3: a schema change that reached memory and not the log --------------------------

    # `FMT-01`. `Publication::create` is the only writer of `schemaString` and every caller
    # gates it on the table being new, so without this the added column's data is encoded,
    # written into Parquet, and unreachable by every query for ever --- while
    # `schema_changes_applied` says the change was applied. Compaction then refuses inputs that
    # do not share a schema, so that partition fails maintenance on every tick, silently.
    ("ingest: adopt a schema change in memory without writing it to the log",
     "crates/sankhya-ingest/src/pipeline.rs",
     "                if sankhya_publish::is_table(&directory) {",
     "                if false {",
     "sankhya-ingest"),

    # The rows captured under the old shape are published first, or a batch spans two schemas
    # and the metadata that follows describes files it does not match.
    ("ingest: adopt a new shape before publishing what the old one captured",
     "crates/sankhya-ingest/src/pipeline.rs",
     "        if matches!(classified, Compatibility::Compatible { .. }) {\n            self.publish(true)?;\n        }",
     "        if false {\n            self.publish(true)?;\n        }",
     "sankhya-ingest"),

    # Evolution writes the table's *current* metadata back with one field replaced. Building a
    # fresh one loses the id, the partition columns and every configuration entry --- clone
    # lineage, feed positions, table class, key columns.
    ("delta: read the first metaData rather than the last when asking what a table declares",
     "crates/sankhya-table-delta/src/log.rs",
     "        if let Action::Metadata(metadata) = action {\n            found = Some(metadata);\n        }",
     "        if let Action::Metadata(metadata) = action {\n            found = found.or(Some(metadata));\n        }",
     "sankhya-table-delta"),

    # A mandatory column arriving at the source has no value in any row already published.
    # Adopting it is the one route around `ColumnTightened`, which exists to refuse exactly
    # this, and the failure then surfaces as a scan error rather than at the change.
    ("schema: adopt an added column without asking whether it is mandatory",
     "crates/sankhya-schema/src/evolve.rs",
     "                if field.nullable {",
     "                if true {",
     "sankhya-schema"),

    # `ColumnsReordered` was declared and never constructed, so a pure reorder returned
    # "compatible, and nothing changed" and nothing rewrote the metadata.
    ("schema: report a reordering as no change at all",
     "crates/sankhya-schema/src/evolve.rs",
     "        if here != there {\n            changes.push(SchemaChange::ColumnsReordered);\n        }",
     "        if false {\n            changes.push(SchemaChange::ColumnsReordered);\n        }",
     "sankhya-schema"),

    # --- Phase 3: a plausible wrong number, rather than a refusal ------------------------------

    # `COR-08`. The fast path truncates every term at a scale set by the *largest* term, and
    # the old proof concluded from that that truncation could not move the answer. What is
    # returned is the **total**, which cancellation makes arbitrarily smaller. Removing the
    # post-condition puts back a route that is quietly approximate exactly where the input
    # cancels --- the figure that will not tie out and nobody can explain.
    ("math: return a fixed-point total that cancellation has taken below the truncation floor",
     "crates/sankhya-math/src/reduce.rs",
     "    if truncated > 0 && total.unsigned_abs() <= (truncated << 53) {\n        return None;\n    }",
     "    if false {\n        return None;\n    }",
     "sankhya-math"),

    # The other direction: a guard that counts every term rather than the ones that actually
    # lost bits declines on inputs that were exact, so every ordinary sum pays for an expansion
    # it does not need --- and the canonical hard case, which cancels hard and truncates
    # nothing, stops taking the fast path at all.
    ("math: count every term as truncated rather than the ones that lost bits",
     "crates/sankhya-math/src/reduce.rs",
     "        if lost {\n            truncated += 1;\n        }",
     "        truncated += 1;",
     "sankhya-math"),

    # There is deliberately **no mutation for the fallback being the exact expansion** rather
    # than sorted Neumaier compensation.
    #
    # One was written and it survived. Inputs were searched for that separate the two --- pairs
    # and triples cancelling across a hundred decimal orders, eight nested cancelling pairs,
    # residuals stacked at three scales --- and none was found: over a canonical magnitude
    # order, the small terms accumulate exactly before the large ones arrive and a single
    # compensation double captures what is left.
    #
    # So the change is a stronger *argument*, not an observable repair. It is kept because the
    # fallback is reached exactly when a fixed margin was not enough, and a module promising
    # "never an approximation" should not answer that with a second fixed margin. But an
    # uncatchable mutation is not evidence of coverage, and the entry that carries `COR-08` is
    # the post-condition above.

    # `COR-07`. Two endpoints and a sign test is only a bracket when the function crosses once
    # between them, and a sequence with more than one sign change does not. `[-1000, 110 x 98,
    # -500]` has a root at eleven per cent and another below zero; the extremes have the same
    # sign, so testing them concludes there is no root at all.
    ("math: bracket an internal rate of return between the extremes rather than scanning",
     "crates/sankhya-math/src/finance.rs",
     "    let bracket = scan(&value, 0.0, 1_000.0).or_else(|| scan(&value, 0.0, -0.999_999));",
     "    let bracket = match (value(-0.999_999), value(1_000.0)) {\n        (Some(low), Some(_)) => Some((-0.999_999, 1_000.0, low)),\n        _ => None,\n    };",
     "sankhya-math"),

    # And the convention, which is a choice rather than a derivation and so has to be pinned:
    # the non-negative rate is the one reported. Scanning from minus one upward finds the other
    # root first and returns it, which is a different number with the same name.
    ("math: report the first root above minus one rather than the one above zero",
     "crates/sankhya-math/src/finance.rs",
     "    let bracket = scan(&value, 0.0, 1_000.0).or_else(|| scan(&value, 0.0, -0.999_999));",
     "    let bracket = scan(&value, -0.999_999, 1_000.0);",
     "sankhya-math"),

    # There are deliberately **no mutations for two guards in `internal_rate_of_return`**: the
    # `is_finite` filter on an endpoint, and the check that the value at the returned rate is
    # actually zero.
    #
    # Both were written for `COR-07` and both survived, correctly. Once the bracket is found by
    # scanning, it spans a genuine sign change of a function that is continuous everywhere in
    # `(-1, infinity)` --- so bisection converges on a real root, the verification always
    # passes, and no input reaches the non-finite region with a bracket around it. They are
    # defence in depth against the domain boundary, not the load-bearing fix, and the two
    # entries above are.
    #
    # A mutation nothing can catch is not evidence of coverage; it is a permanent survivor
    # that trains people to ignore the survivor list. Same rule as the fsync calls.

    # `COR-09`. A proportion of a non-positive base is a different question: dividing by a
    # negative peak reports a fall as a rise, and substituting zero reports a series that only
    # fell as one that never did.
    ("math: report a drawdown against a peak that is not positive",
     "crates/sankhya-math/src/timeseries.rs",
     "        if peak <= 0.0 || !peak.is_finite() {",
     "        if false {",
     "sankhya-math"),

    # `COR-10`. The module forbids `1 - cdf` by name. A double near one has no bits below
    # about 1e-16, so every lower tail smaller than that is reported as zero --- in the
    # direction that makes a finding look stronger.
    ("math: take an F-test's lower tail by subtracting the upper one",
     "crates/sankhya-math/src/inference.rs",
     "        f_sf(1.0 / statistic, df_right, df_left)?",
     "        1.0 - upper",
     "sankhya-math"),

    ("math: measure a drawdown from the last value rather than the running peak",
     "crates/sankhya-math/src/timeseries.rs",
     "        if *value > peak {\n            peak = *value;\n        }",
     "        peak = *value;",
     "sankhya-math"),

    ("math: sum period returns rather than compounding them",
     "crates/sankhya-math/src/timeseries.rs",
     "    Ok(returns.iter().fold(1.0, |total, r| total * (1.0 + r)) - 1.0)",
     "    Ok(deterministic_sum(returns))",
     "sankhya-math"),

    ("math: discount a net present value from period zero under the period-one name",
     "crates/sankhya-math/src/finance.rs",
     "            let exponent = period as i32 + 1;",
     "            let exponent = period as i32;",
     "sankhya-math"),

    ("math: return the nearest iterate when no rate zeroes the cash flow",
     "crates/sankhya-math/src/finance.rs",
     "    if !(positive && negative) {",
     "    if false {",
     "sankhya-math"),

    ("math: report a value-at-risk positive, so it is added to a profit",
     "crates/sankhya-math/src/finance.rs",
     "    crate::quantile(&mut sorted, tail, crate::Convention::LinearInterpolation)\n        .map_err(|reason| VectorError::Refused(reason.to_string()))",
     "    crate::quantile(&mut sorted, tail, crate::Convention::LinearInterpolation)\n        .map(f64::abs)\n        .map_err(|reason| VectorError::Refused(reason.to_string()))",
     "sankhya-math"),

    ("math: count the whole deviation in a Sortino ratio rather than the downside",
     "crates/sankhya-math/src/finance.rs",
     "    let downside: Vec<f64> = excess.iter().filter(|v| **v < 0.0).map(|v| v * v).collect();",
     "    let downside: Vec<f64> = excess.iter().map(|v| v * v).collect();",
     "sankhya-math"),

    ("math: drop the variance term from the Black-Scholes drift",
     "crates/sankhya-math/src/finance.rs",
     "    let d1 = ((spot / strike).ln() + (rate + 0.5 * volatility * volatility) * years) / root;",
     "    let d1 = ((spot / strike).ln() + rate * years) / root;",
     "sankhya-math"),

    ("math: call a square non-symmetric matrix not square",
     "crates/sankhya-math/src/decompose.rs",
     "        return Err(MatrixError::NotSymmetric { size });\n    }\n\n    let mut lower = vec![0.0f64; size * size];",
     "        return Err(MatrixError::NotSquare { rows: size, columns: size });\n    }\n\n    let mut lower = vec![0.0f64; size * size];",
     "sankhya-math"),

    # M21. Reading a column of vectors. The borrowing path is 25x the copying one on a narrow
    # column, and a stride read from the wrong place returns numbers rather than failing.
    # No entry for "ignore a sliced column's offset". There is no offset to ignore: slicing a
    # `FixedSizeListArray` slices its child values too, so `offset()` is zero and `values()`
    # already begins at the slice's first row.
    #
    # The first version of this file carried an offset anyway, with a comment calling it a
    # hazard --- and the mutation was unobservable because the premise was false. The line is
    # gone rather than kept with a note, because a defensive line whose premise is false reads
    # as evidence somebody checked.

    ("functions: give a null row an empty series rather than a null one",
     "crates/sankhya-functions/src/rows.rs",
     "            Self::Strided { flat, width, list } => {\n                if list.is_null(row) {\n                    return None;\n                }",
     "            Self::Strided { flat, width, list } => {\n                let _ = list;",
     "sankhya-functions"),

    ("functions: allocate a buffer per row instead of reusing one",
     "crates/sankhya-functions/src/multi.rs",
     "                slot.clear();\n                if !fill(array, row, self.name, slot)? {",
     "                if !fill(array, row, self.name, slot)? {",
     "sankhya-functions"),

    # M21. The catalogue and the routing. Every one of these is a function a user is told
    # exists and cannot call, or a statement sent to a tier that cannot answer it.
    ("functions: match a bare name, so a column called `erf` routes a lookup analytically",
     "crates/sankhya-functions/src/routing.rs",
     "        let after = lowered[end..].trim_start();\n        let after_ok = after.starts_with('(');",
     "        let after_ok = true;",
     "sankhya-functions"),

    ("functions: match without a left boundary, so `my_erf(x)` is a call to `erf`",
     "crates/sankhya-functions/src/routing.rs",
     "        let before_ok = start == 0\n            || lowered[..start]\n                .chars()\n                .next_back()\n                .is_none_or(|c| !c.is_alphanumeric() && c != '_');",
     "        let before_ok = true;",
     "sankhya-functions"),

    ("functions: require the parenthesis to be adjacent, so `norm_cdf (x)` routes wrongly",
     "crates/sankhya-functions/src/routing.rs",
     "        let after = lowered[end..].trim_start();",
     "        let after = &lowered[end..];",
     "sankhya-functions"),

    ("functions: serve the catalogue unsorted, so two readings disagree",
     "crates/sankhya-functions/src/describe.rs",
     "        entries.sort_by_key(|entry| entry.name);",
     "",
     "sankhya-server"),

    # M21. Inference and regression. Every one of these is a wrong number that a person acts
    # on --- a p-value below a threshold, a slope reported as a finding.
    ("math: pool the variances, so unequal spreads reject too often",
     "crates/sankhya-math/src/inference.rs",
     "    let freedom = (se_left + se_right).powi(2)\n        / (se_left * se_left / (n_left - 1.0) + se_right * se_right / (n_right - 1.0));",
     "    let freedom = n_left + n_right - 2.0;",
     "sankhya-math"),

    ("math: pair two samples of different lengths by truncating to the shorter",
     "crates/sankhya-math/src/inference.rs",
     "    if left.len() != right.len() {\n        return Err(InferenceError::Unpaired { left: left.len(), right: right.len() });\n    }\n    let differences: Vec<f64> = left.iter().zip(right).map(|(a, b)| a - b).collect();",
     "    let differences: Vec<f64> = left.iter().zip(right).map(|(a, b)| a - b).collect();",
     "sankhya-math"),

    ("math: report only the upper tail of an F test, halving a small variance's p-value",
     "crates/sankhya-math/src/inference.rs",
     "    let p_value = (2.0 * upper.min(lower)).min(1.0);",
     "    let p_value = upper;",
     "sankhya-math"),

    ("math: divide a chi-squared term by an expected count of zero",
     "crates/sankhya-math/src/inference.rs",
     "        if *e == 0.0 {\n            return Err(InferenceError::ZeroExpected { at });\n        }",
     "        if false {\n            return Err(InferenceError::ZeroExpected { at });\n        }",
     "sankhya-math"),

    # No entry for "solve the normal equations instead of QR". A mutation of it would have to
    # replace the whole solve --- form `XᵀX`, invert it, substitute --- which is a rewrite
    # rather than a mutation, and an entry whose `replace` is a no-op (the first attempt here
    # added `let _ = &q;` and changed nothing) is a catalogue entry that claims coverage it
    # does not have.
    #
    # The property is held by a **test** instead: `a_regression_on_a_badly_conditioned_design`
    # fits a design whose condition number the normal equations could not survive, and checks
    # the coefficients against the ones written down. That guards the property whatever the
    # implementation, which is the stronger thing to have.

    ("math: report plain R-squared as the adjusted one, rewarding a useless predictor",
     "crates/sankhya-math/src/regression.rs",
     "        Some(1.0 - (rss / freedom) / (tss / (n - 1.0)))",
     "        Some(1.0 - rss / tss)",
     "sankhya-math"),

    ("math: fit a model with more parameters than observations",
     "crates/sankhya-math/src/regression.rs",
     "    if rows <= predictors {\n        return Err(InferenceError::TooFew { given: rows, needs: predictors + 1 });\n    }",
     "    if false {\n        return Err(InferenceError::TooFew { given: rows, needs: predictors + 1 });\n    }",
     "sankhya-math"),

    ("math: report the ridge penalty's own rows as unexplained variance",
     "crates/sankhya-math/src/regression.rs",
     "    let residuals = fit.residuals.into_iter().take(rows).collect();",
     "    let residuals = fit.residuals;",
     "sankhya-math"),

    ("math: accept a negative ridge penalty, rewarding large coefficients",
     "crates/sankhya-math/src/regression.rs",
     "    if penalty < 0.0 || !penalty.is_finite() {",
     "    if false {",
     "sankhya-math"),

    # M21. The distributions. Each of these is a wrong number rather than a failure, and the
    # wrong number is a critical value somebody compares a test statistic against.
    ("math: compute the error function by subtraction, losing the small tail",
     "crates/sankhya-math/src/special.rs",
     "    if x >= 0.0 {\n        gamma_p(0.5, square).unwrap_or(1.0)\n    } else {\n        -gamma_p(0.5, square).unwrap_or(1.0)\n    }",
     "    let value = 1.0 - erfc(x.abs());\n    if x >= 0.0 { value } else { -value }",
     "sankhya-math"),

    ("math: skip the Halley refinement on the normal quantile",
     "crates/sankhya-math/src/distribution.rs",
     "    let error = norm_cdf(x) - p;\n    let density = norm_pdf(x);",
     "    let error = 0.0;\n    let density = norm_pdf(x);",
     "sankhya-math"),

    ("math: take the upper tail by subtracting the lower one",
     "crates/sankhya-math/src/special.rs",
     "    if x < a + 1.0 {\n        gamma_series(a, x).map(|p| 1.0 - p)\n    } else {\n        Ok(gamma_continued(a, x))\n    }",
     "    Ok(1.0 - gamma_p(a, x)?)",
     "sankhya-math"),

    ("math: use the series everywhere instead of switching at the crossover",
     "crates/sankhya-math/src/special.rs",
     "    if x < a + 1.0 {\n        gamma_series(a, x)\n    } else {\n        Ok(1.0 - gamma_continued(a, x))\n    }",
     "    gamma_series(a, x)",
     "sankhya-math"),

    ("math: reflect the incomplete beta on the wrong side of its crossover",
     "crates/sankhya-math/src/special.rs",
     "    if x < (a + 1.0) / (a + b + 2.0) {",
     "    if x < 0.5 {",
     "sankhya-math"),

    # Not overflow --- `binomial_pmf` already works in logarithms, so the sum arrives. It
    # arrives having accumulated five hundred roundings, and after five hundred evaluations
    # where the identity needs one.
    ("math: sum a binomial cumulative term by term instead of using the identity",
     "crates/sankhya-math/src/distribution.rs",
     "    beta_i(n - k, k + 1.0, 1.0 - probability)",
     "    let mut total = 0.0;\n    for i in 0..=successes {\n        total += binomial_pmf(i, trials, probability)?;\n    }\n    Ok(total)",
     "sankhya-math"),

    ("math: lose a small exponential probability by subtracting from one",
     "crates/sankhya-math/src/distribution.rs",
     "    Ok(-(-rate * x).exp_m1())",
     "    Ok(1.0 - (-rate * x).exp())",
     "sankhya-math"),

    # The decompositions. A factor that is subtly wrong satisfies no identity, so these are
    # caught by the identity tests rather than by a table of expected entries.
    ("math: symmetrise a matrix rather than refusing an asymmetric one",
     "crates/sankhya-math/src/decompose.rs",
     "    if !is_symmetric(values, size) {\n        return Err(MatrixError::NotSymmetric { size });\n    }\n\n    let mut a = values.to_vec();",
     "    let mut a = values.to_vec();",
     "sankhya-math"),

    ("math: accept a non-positive Cholesky pivot, so an impossible matrix factors",
     "crates/sankhya-math/src/decompose.rs",
     "                if !(sum > 0.0) {\n                    return Err(MatrixError::Singular);\n                }",
     "                if sum < 0.0 {\n                    return Err(MatrixError::Singular);\n                }",
     "sankhya-math"),

    ("math: choose the Householder sign toward the head, cancelling the subtraction",
     "crates/sankhya-math/src/decompose.rs",
     "        let alpha = if head >= 0.0 { -norm } else { norm };",
     "        let alpha = if head >= 0.0 { norm } else { -norm };",
     "sankhya-math"),

    ("math: leave an eigenvector's sign to the arithmetic that produced it",
     "crates/sankhya-math/src/decompose.rs",
     "            put(&mut sorted, size, row, column, sign * at(&vectors, size, row, source));",
     "            put(&mut sorted, size, row, column, at(&vectors, size, row, source));",
     "sankhya-math"),

    ("math: sort eigenvalues without carrying their eigenvectors along",
     "crates/sankhya-math/src/decompose.rs",
     "    for (column, &source) in order.iter().enumerate() {",
     "    for (column, &source) in (0..size).collect::<Vec<_>>().iter().enumerate() {",
     "sankhya-math"),

    ("math: take the smaller Jacobi root in the form that cancels",
     "crates/sankhya-math/src/decompose.rs",
     "                let t = if theta >= 0.0 {\n                    1.0 / (theta + (1.0 + theta * theta).sqrt())\n                } else {\n                    -1.0 / (-theta + (1.0 + theta * theta).sqrt())\n                };",
     "                let t = 1.0 / (theta + (1.0 + theta * theta).sqrt());",
     "sankhya-math"),

    ("functions: round a fractional count rather than refusing it",
     "crates/sankhya-functions/src/distributions.rs",
     "    if value < 0.0 || value.fract() != 0.0 || !value.is_finite() {",
     "    if false {",
     "sankhya-functions"),

    # M21. Twelve kernels were written, tested and unreachable. These hold the surface, which
    # is the half that was missing --- the kernels themselves were never the problem.
    ("olap: register the scalar vector functions and not the series ones",
     "crates/sankhya-olap/src/vectors.rs",
     "    for function in series_functions() {\n        context.register_udf(function);\n    }",
     "",
     "sankhya-olap"),

    ("olap: give a null vector an empty series rather than a null one",
     "crates/sankhya-olap/src/vectors.rs",
     "                    None => {\n                        any_null = true;\n                        break;\n                    }\n                    Some(values) => operands.push(values),\n                }\n            }\n            let by = if self.scalar {",
     "                    None => break,\n                    Some(values) => operands.push(values),\n                }\n            }\n            let by = if self.scalar {",
     "sankhya-olap"),

    ("olap: return a fixed width from a kernel that changes the width",
     "crates/sankhya-olap/src/vectors.rs",
     "        Ok(DataType::List(Arc::new(Field::new(\n            \"item\",\n            DataType::Float64,\n            true,\n        ))))",
     "        Ok(DataType::FixedSizeList(\n            Arc::new(Field::new(\"item\", DataType::Float64, true)),\n            1,\n        ))",
     "sankhya-olap"),

    # M21. Every figure this system produces reduces through here, so a fixed-point route that
    # disagreed with the sorted one by a bit would move all of them at once.
    ("math: accumulate in floating point, losing the associativity the fixed point buys",
     "crates/sankhya-math/src/reduce.rs",
     "        total += whole;",
     "        total = ((total as f64) + (whole as f64)) as i128;",
     "sankhya-math"),

    ("math: keep fewer bits below the largest term than the answer can express",
     "crates/sankhya-math/src/reduce.rs",
     "const BELOW_THE_TOP: i32 = 100;",
     "const BELOW_THE_TOP: i32 = 20;",
     "sankhya-math"),

    # No entry for "approximate rather than decline when a term is not finite". The guard is
    # correct and **unobservable**, because a second guard downstream catches the same inputs.
    #
    # Remove the first and a `NaN` still declines: `NaN.abs() > largest` is false, so `largest`
    # stays finite, and the scaled term is `NaN`, which the `!scaled.is_finite()` check refuses.
    # An infinity declines a step earlier still --- it becomes `largest`, and its exponent puts
    # the scale outside the representable range.
    #
    # The guard stays because reading `largest` off a slice containing a `NaN` and reasoning
    # about what `>` does to it is not something a later reader should have to do. It is
    # clarity, not behaviour, and the catalogue says so rather than claiming a test covers it.

    # The cube surface could not name a table in a schema AT ALL: the qualified form failed to
    # parse and the bare form resolved only while one schema claimed the name.
    ("cube: read a table name as one word, so a schema cannot be named",
     "crates/sankhya-cube-sql/src/ddl.rs",
     "        let first = self.name(what)?;\n        if self.peek().map(|spanned| &spanned.token) != Some(&Token::Punct('.')) {\n            return Ok(first);\n        }\n        self.next += 1;\n        let second = self.name(what)?;\n        Ok(format!(\"{first}.{second}\"))",
     "        self.name(what)",
     "sankhya-cube-sql"),

    ("cube: accept a dot anywhere a name is read, not only in a table position",
     "crates/sankhya-cube-sql/src/ddl.rs",
     "        let name = self.name(\"a dimension name\")?;",
     "        let name = self.qualified_name(\"a dimension name\")?;",
     "sankhya-cube-sql"),

    # M17, items 1 and 2 --- the git-tag half. Every one of these was a live defect before the
    # test that names it, and three of them answered *something* rather than failing.
    # `kept_by` said the word `snapshot`, which is true and useless --- and it never mentioned
    # a clone at all, though a clone keeps a version alive in exactly the same way.
    ("server: report that something keeps a version without naming what",
     "crates/sankhya-server/src/snapshots.rs",
     "                    keepers\n                        .get(&change.version)\n                        .map(|names| {\n                            names.iter().cloned().collect::<Vec<_>>().join(\", \")\n                        })\n                        .unwrap_or_default(),",
     "                    if keepers.contains_key(&change.version) { \"snapshot\" } else { \"\" }\n                        .to_owned(),",
     "sankhya-server"),

    ("server: forget that a clone keeps a version alive too",
     "crates/sankhya-server/src/snapshots.rs",
     "        for (version, clones) in server.lineages().keepers_of(&qualified) {\n            keepers.entry(version).or_default().extend(clones);\n        }",
     "        let _ = qualified;",
     "sankhya-server"),

    ("server: serve the newest version when a caller asked for one the table does not have",
     "crates/sankhya-server/src/snapshots.rs",
     "    if !commits.iter().any(|(at, _)| *at == version) {",
     "    if false {",
     "sankhya-server"),

    ("server: answer a version whose files retirement has already taken",
     "crates/sankhya-server/src/snapshots.rs",
     "    if missing > 0 {",
     "    if false {",
     "sankhya-server"),

    # The dispatch order itself. `SET VERSION OF <table> = <n>` is a `SET`, and the generic
    # session handler accepts any `SET` as a no-op --- so ordered the other way this was
    # swallowed and the caller was served the present with no symptom at all.
    ("server: let the generic SET handler see a version statement first",
     "crates/sankhya-server/src/wiring.rs",
     "        if let Some(statement) = sankhya_snapshot::parse(dispatch) {\n            return crate::snapshots::run_statement(self, statement, &principal);\n        }\n\n        if let Some(answer) = crate::driver::run_session_statement(dispatch) {",
     "        if let Some(answer) = crate::driver::run_session_statement(dispatch) {",
     "sankhya-server"),

    ("delta: let a compaction declare that it changed rows",
     "crates/sankhya-table-delta/src/log.rs",
     "        Self {\n            data_change: false,\n            ..Self::with_statistics(path, size, modification_time, stats)\n        }",
     "        Self::with_statistics(path, size, modification_time, stats)",
     "sankhya-table-delta"),

    ("delta: report a metadata-only commit as having happened at the epoch",
     "crates/sankhya-table-delta/src/history.rs",
     "                    at: None,",
     "                    at: Some(0),",
     "sankhya-table-delta"),

    ("delta: describe a compaction as an ordinary rewrite",
     "crates/sankhya-table-delta/src/history.rs",
     "    } else if !change.changed_data && change.added > 0 && change.removed > 0 {",
     "    } else if false {",
     "sankhya-table-delta"),

    ("server: read as of a snapshot that has expired",
     "crates/sankhya-server/src/snapshots.rs",
     "    if standing(&snapshot, server.today()) == Standing::Expired {",
     "    if false {",
     "sankhya-server"),

    # Named against `sankhya-server`, not the crate the code lives in: a refused `SET` is only
    # observable where a handler *refuses* one, and only the server's does. The catalogue has
    # made the other mistake before and records it.
    ("wire: remember a setting the handler refused",
     "crates/sankhya-api-pg/src/session.rs",
     "                if !self.run(&sql, handler, output) {\n                    self.remember_setting(&sql);\n                }",
     "                self.run(&sql, handler, output);\n                self.remember_setting(&sql);",
     "sankhya-server"),

    ("server: accept a feed cadence of zero, which is a loop with no sleep in it",
     "crates/sankhya-server/src/main.rs",
     "            Ok(0) | Err(_) => {",
     "            Err(_) => {",
     "sankhya-server"),

    # M14. A session registered every table under its bare name and discarded the schema, so
    # `sales.orders` did not resolve and two tables of one name silently replaced each other.
    ("server: register a table under its bare name only, losing its schema",
     "crates/sankhya-server/src/execute.rs",
     "        if !schema.is_empty() && !in_the_default_schema {",
     "        if false {",
     "sankhya-server"),

    ("server: resolve a bare name two schemas claim to whichever came first",
     "crates/sankhya-server/src/execute.rs",
     "        if claims.get(table.reference.table.as_str()).copied().unwrap_or(0) == 1\n            || schema.is_empty()\n            || in_the_default_schema\n        {",
     "        if true {",
     "sankhya-server"),

    ("server: authorize a bare name two schemas claim against the first rule found",
     "crates/sankhya-server/src/wiring.rs",
     "        if matching.len() > 1 {\n            return None;\n        }",
     "        if false {\n            return None;\n        }",
     "sankhya-server"),

    # Cloning resolved `warehouse/<name>`, one level above where tables live, so it could only
    # ever name a table this server does not serve.
    ("server: resolve a clone's origin at the warehouse root rather than in a schema",
     "crates/sankhya-server/src/warehouse.rs",
     "    if let Some((schema, table)) = name.split_once('.') {\n        let root = warehouse.join(schema).join(table);",
     "    if let Some((_schema, table)) = name.split_once('.') {\n        let root = warehouse.join(table);",
     "sankhya-server"),

    ("server: put a clone in whatever schema its name asked for",
     "crates/sankhya-server/src/warehouse.rs",
     "            if origin_schema.is_some_and(|origin| origin == asked) {",
     "            if true {",
     "sankhya-server"),

    ("server: file a lineage under a name a second schema could later claim",
     "crates/sankhya-server/src/wiring.rs",
     "            let Some(name) = crate::warehouse::qualified_name(&self.settings.warehouse, &table.root)",
     "            let Some(name) = Some(table.reference.table.to_string())",
     "sankhya-server"),

    ("clone: claim every SHOW statement rather than the two this answers",
     "crates/sankhya-clone/src/ask.rs",
     "        } else {\n            return None;\n        };",
     "        } else {\n            (\"LINEAGE\", |table| Question::Lineage { table })\n        };",
     "sankhya-clone"),

    ("clone: hand back a question with no table instead of refusing it",
     "crates/sankhya-clone/src/ask.rs",
     "        None => return Some(Err(NotAQuestion::NoTableNamed { question: named })),",
     "        None => return None,",
     "sankhya-clone"),

    # A column's declared metadata. A matrix column carries its shape there and nowhere else,
    # so a writer or a reader that drops it stores a matrix that comes back not being one ---
    # and the table still reads, which is what made it invisible.
    ("delta: write only the fixed length, dropping every key the column declared",
     "crates/sankhya-table-delta/src/schema.rs",
     "        .filter(|(key, _)| key.as_str() != FIXED_LENGTH_KEY)\n        .map(|(key, value)| (key.as_str(), value.clone()))",
     "        .filter(|(_, _)| false)\n        .map(|(key, value)| (key.as_str(), value.clone()))",
     "sankhya-table-delta"),

    ("delta: read a column back without the metadata it was stored with",
     "crates/sankhya-table-delta/src/schema.rs",
     "            Ok(if carried.is_empty() {\n                restored\n            } else {\n                restored.with_metadata(carried)\n            })",
     "            Ok(restored)",
     "sankhya-table-delta"),

    # Simpson's rule. An even sample count has no answer under it, and both ways of pretending
    # otherwise --- dropping a sample, falling back to the trapezoid --- return a number.
    # A user's own aggregation. `ADR-0010`: a declared aggregation is *exercised* before it is
    # trusted, and a declared `merge` is a claim that partial results compose.
    ("udf: trust a declared aggregation instead of exercising it",
     "crates/sankhya-udf/src/worker.rs",
     "        self.exercise(&candidate)?;",
     "        let _ = &candidate;",
     "sankhya-udf"),

    ("udf: accept an aggregation whose answer depends on how the rows were batched",
     "crates/sankhya-udf/src/worker.rs",
     "        if !same(whole, in_pieces) {",
     "        if false {",
     "sankhya-udf"),

    ("udf: accept a merge that is not associative",
     "crates/sankhya-udf/src/worker.rs",
     "        if !same(whole, left) || !same(left, right) {",
     "        if false {",
     "sankhya-udf"),

    ("udf: compare a declaration's answers loosely rather than bit for bit",
     "crates/sankhya-udf/src/worker.rs",
     "    left.to_bits() == right.to_bits() || (left.is_nan() && right.is_nan())",
     "    (left - right).abs() < 1e-6 || (left.is_nan() && right.is_nan())",
     "sankhya-udf"),

    ("olap: let a null argument contribute to a user-supplied aggregation",
     "crates/sankhya-olap/src/supplied.rs",
     "                if column.is_null(row) {\n                    // The whole row, not the one argument.",
     "                if false {\n                    // The whole row, not the one argument.",
     "sankhya-olap"),

    ("olap: answer zero for a group a user-supplied aggregation saw no rows of",
     "crates/sankhya-olap/src/supplied.rs",
     "            return Ok(ScalarValue::Float64(None));",
     "            return Ok(ScalarValue::Float64(Some(0.0)));",
     "sankhya-olap"),

    # The declared rule reaching the value. An adversarial review on 2026-09-01 found a cube
    # declaring `MAX ALONG region` answering the sum --- 15,687 where the maximum was 373.5 ---
    # because the cell was read with a hardcoded summation one layer below where the model
    # checks additivity. It was fixed and nothing pinned it, and the book went on documenting it
    # as live for two days.
    ("cube: read every cell as a sum, so a declared rule never reaches the value",
     "crates/sankhya-cube-sql/src/functions.rs",
     "        cells\n            .addresses()\n            .map(|address| (address, cells.get(address, rule)))\n            .collect()",
     "        cells\n            .addresses()\n            .map(|address| (address, cells.get(address, Rule::Sum)))\n            .collect()",
     "sankhya-server"),

    # The difference between two versions. `M20` was deferred because the log records files and
    # a compaction rewrites files without changing a row, so the whole feature turns on that
    # distinction being honoured.
    ("delta: count a compaction as a change between two versions",
     "crates/sankhya-table-delta/src/history.rs",
     "        if declares {\n            changed.insert(*version);\n        }",
     "        if is_file {\n            changed.insert(*version);\n        }",
     "sankhya-table-delta"),

    ("delta: report a compaction as nothing at all rather than naming it",
     "crates/sankhya-table-delta/src/history.rs",
     "        compactions: touched.difference(&changed).count(),",
     "        compactions: 0,",
     "sankhya-table-delta"),

    ("delta: forget how many rows a removed file held",
     "crates/sankhya-table-delta/src/history.rs",
     "                difference.rows_removed += rows_in.get(&remove.path).copied().unwrap_or(0);",
     "                difference.rows_removed += 0;",
     "sankhya-table-delta"),

    ("delta: answer about the newest version when asked about one nobody has",
     "crates/sankhya-table-delta/src/history.rs",
     "        if wanted > newest {\n            return Err(NoDifference::NoSuchVersion { wanted, newest });\n        }",
     "        if false {\n            return Err(NoDifference::NoSuchVersion { wanted, newest });\n        }",
     "sankhya-server"),

    # Authorization varying by subject. The identity travelled inward correctly for two days
    # while every user was handed the same literal role, so nothing downstream could tell two
    # subjects apart --- a plumbing job finished and a feature that was not.
    ("server: hand every user the same role, whatever the operator wrote down",
     "crates/sankhya-server/src/wiring.rs",
     "        let held: Vec<Role> = if self.settings.roles.is_empty() {",
     "        let held: Vec<Role> = if true {",
     "sankhya-server"),

    ("server: give a user the operator did not name a role anyway",
     "crates/sankhya-server/src/wiring.rs",
     "                .map(|names| names.iter().map(|name| Role::new(name.clone())).collect())\n                .unwrap_or_default()",
     "                .map(|names| names.iter().map(|name| Role::new(name.clone())).collect())\n                .unwrap_or_else(|| vec![Role::new(\"reader\")])",
     "sankhya-server"),

    # The binding's transport security. The refusal is the load-bearing half: a client that
    # asks for encryption, is told no, and continues has already sent the password it was
    # protecting.
    ("sdk: continue in the clear when `sslmode=require` and the server declines",
     "sdk/python/sankhya/wire.py",
     "            if sslmode == \"prefer\":\n                # Declined, and the caller said they would take either.",
     "            if True:\n                # Declined, and the caller said they would take either.",
     "sankhya-server"),

    ("sdk: report a connection as encrypted when the handshake never happened",
     "sdk/python/sankhya/wire.py",
     "        self.encrypted = True",
     "        self.encrypted = False",
     "sankhya-server"),

    # A derived result: a query given a name. `ADR-0014` Option A.
    ("derived: accept a derived result over a bare table name",
     "crates/sankhya-cube/src/validate.rs",
     "        out.push(Rejection::DerivedFromATable);",
     "        let _ = Rejection::DerivedFromATable;",
     "sankhya-cube"),

    ("derived: accept a maintained derived result and materialise nothing",
     "crates/sankhya-server/src/cubes.rs",
     "    if is_derived && definition.target_lag.is_some() {",
     "    if false {",
     "sankhya-server"),

    ("derived: let `DROP CUBE` remove a derived result",
     "crates/sankhya-server/src/cubes.rs",
     "        .find(|cube| cube.name() == name && cube.definition().is_derived() == derived)",
     "        .find(|cube| cube.name() == name)",
     "sankhya-server"),

    ("derived: list a derived result among the cubes",
     "crates/sankhya-cube-sql/src/describe.rs",
     "            self.0.iter().filter(|cube| !cube.definition().is_derived()).collect();",
     "            self.0.iter().collect();",
     "sankhya-server"),

    # A user's own aggregation as a cube measure.
    ("cube: let a cube assert a composability its function never claimed",
     "crates/sankhya-server/src/cubes.rs",
     "            if let Some(held) = declared.iter().find(|held| held.name == named) {",
     "            if false {",
     "sankhya-server"),

    ("cube: let a cube name an aggregation this server has never heard of",
     "crates/sankhya-server/src/cubes.rs",
     "            if !declared.iter().any(|held| held.name == named) {",
     "            if false {",
     "sankhya-server"),

    ("cube: roll a user's aggregation up from reduced partials rather than from the facts",
     "crates/sankhya-cube/src/navigate.rs",
     "        if matches!(rule, Rule::Supplied { .. }) {",
     "        if false {",
     "sankhya-server"),

    ("cube: answer a user-supplied measure with a built-in reduction",
     "crates/sankhya-cube-sql/src/functions.rs",
     "    let rows: Vec<(&Vec<String>, Option<f64>)> = if let Rule::Supplied { .. } = rule {",
     "    let rows: Vec<(&Vec<String>, Option<f64>)> = if false {",
     "sankhya-server"),

    # The boundary a user-supplied function runs behind. Each entry removes one mechanism and
    # the test that proves that prohibition must fail --- which is the only way to know the
    # mechanism is the thing doing the work, rather than something else about this machine.
    #
    # There is deliberately **no entry for `RLIMIT_FSIZE`**. Writing is stopped twice, by the
    # read-only bind and by a file-size limit of zero, so removing either alone leaves writing
    # refused and the mutation survives. That is what defence in depth means and it is not a
    # gap in the tests.
    ("sandbox: leave the worker on the host's network",
     "crates/sankhya-sandbox/src/jail.rs",
     "    | libc::CLONE_NEWNET\n",
     "",
     "sankhya-sandbox"),

    ("sandbox: leave the worker on the host's filesystem",
     "crates/sankhya-sandbox/src/jail.rs",
     "    | libc::CLONE_NEWNS\n",
     "",
     "sankhya-sandbox"),

    ("sandbox: bind the allowed tree writable rather than read-only",
     "crates/sankhya-sandbox/src/jail.rs",
     "                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC,",
     "                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_REC,",
     "sankhya-sandbox"),

    ("sandbox: let a function that never returns keep running",
     "crates/sankhya-sandbox/src/lib.rs",
     "                    if started.elapsed() >= bounds.wall {",
     "                    if false {",
     "sankhya-sandbox"),

    ("sandbox: accept more output than the caller allowed",
     "crates/sankhya-sandbox/src/lib.rs",
     "            Ending::Ended(_) if answered.len() > bounds.output => {",
     "            Ending::Ended(_) if false => {",
     "sankhya-sandbox"),

    # A cube over a declared query. Its dependency list is what authorization, the cache key
    # and the invalidation are all decided against, and the query's *text* does not say what
    # they are.
    ("cube: accept a fact query whose answer can move on its own",
     "crates/sankhya-cube/src/validate.rs",
     "        if let Some(found) = non_deterministic(&definition.fact_table) {\n            out.push(Rejection::NotDeterministic { found });\n        }",
     "        if false {\n            out.push(Rejection::NotDeterministic { found: String::new() });\n        }",
     "sankhya-server"),

    ("cube: forget what a fact query reads when the catalogue is read back",
     "crates/sankhya-cube/src/catalogue.rs",
     "        if !self.reads.is_empty() {\n            definition.reads = self.reads.clone();\n        }",
     "        if false {\n            definition.reads = self.reads.clone();\n        }",
     "sankhya-server"),

    ("math: integrate an even sample count by Simpson's rule rather than refusing",
     "crates/sankhya-math/src/calculus.rs",
     "    if n % 2 == 0 {",
     "    if false {",
     "sankhya-math"),

    # --- Phase 5.2: what a door does when it cannot accept -----------------------------------------

    # `OPS-08`. `accept()` returning an error propagated out of the serve loop and out of
    # `main`, so `ECONNABORTED` --- routine behind any load balancer --- ended the process.
    # The classification is one crate so all three doors answer identically.
    ("accept: treat a descriptor shortage as a reason to stop serving",
     "crates/sankhya-accept/src/lib.rs",
     "            Some(EMFILE | ENFILE | ENOBUFS | ENOMEM) => Response::Pause(PAUSE),",
     "            Some(EMFILE | ENFILE | ENOBUFS | ENOMEM) => Response::Stop,",
     "sankhya-accept"),

    ("accept: treat an aborted connection as the server's failure",
     "crates/sankhya-accept/src/lib.rs",
     "        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset => Response::Continue,",
     "        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset => Response::Stop,",
     "sankhya-accept"),

    # The other direction, and the one a silent `continue` produces: a listener that will
    # never accept again, looped on for ever, in a process that is up and answering nothing.
    ("accept: loop on an error nobody has classified",
     "crates/sankhya-accept/src/lib.rs",
     "            _ => Response::Stop,",
     "            _ => Response::Continue,",
     "sankhya-accept"),

    # The wiring, at the door rather than in the policy: a real server must survive a real
    # descriptor shortage rather than exit on it.
    ("server: let a failed accept end the process, as it used to",
     "crates/sankhya-api-pg/src/listener.rs",
     "                                tokio::time::sleep(how_long).await;\n                                continue;",
     "                                let _ = how_long;\n                                return Err(error);",
     "sankhya-server", 1, "accepting"),

    # And the cap that stops the shortage being reachable in the first place.
    ("pg: accept every caller who asks, whatever it costs in descriptors",
     "crates/sankhya-api-pg/src/listener.rs",
     "                accepted = self.listener.accept(), if connections.len() < self.limit => {",
     "                accepted = self.listener.accept() => {",
     "sankhya-api-pg", 1, "accepting"),

    # --- Phase 5.3: "I could not look" is not "there is nothing" ----------------------------------

    # `OPS-12`. An unreadable warehouse returned no tables and no complaints, so the server
    # served an empty catalogue and `doctor` --- the tool for exactly that moment --- gave it
    # a clean bill of health.
    ("server: report an unreadable warehouse as an empty one",
     "crates/sankhya-server/src/warehouse.rs",
     "        Err(error) => {\n            refused.push((warehouse.to_path_buf(), error.to_string()));\n            return (found, refused);\n        }",
     "        Err(_) => {\n            return (found, refused);\n        }",
     "sankhya-server", 1, "unreadable"),

    # The same claim where it decides a deletion: a snapshot pins files only if its table
    # resolves, so "absent" on an unmounted export drops the pin under a reader.
    ("server: answer that a table is absent when nobody could look",
     "crates/sankhya-server/src/warehouse.rs",
     "        Err(error) => return Resolved::Unreadable(error.to_string()),",
     "        Err(_) => return Resolved::Absent,",
     "sankhya-server", 1, "wiring"),

    ("server: report an unreadable feed directory as a deployment with no feeds",
     "crates/sankhya-server/src/feeds.rs",
     "        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {\n            return (Vec::new(), Vec::new())\n        }",
     "        Err(_) => {\n            return (Vec::new(), Vec::new())\n        }\n        #[allow(unreachable_patterns)]",
     "sankhya-server", 1, "wiring"),

    ("server: report an unreadable aggregation store as a warehouse with none",
     "crates/sankhya-server/src/aggregations.rs",
     "        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (out, Vec::new()),\n        Err(error) => {",
     "        Err(_) => return (out, Vec::new()),\n        #[allow(unreachable_patterns)]\n        Err(error) => {",
     "sankhya-server", 1, "wiring"),

    ("cube: report an unreadable catalogue as a warehouse with no cubes",
     "crates/sankhya-cube/src/catalogue.rs",
     "        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),",
     "        Err(_) => return Ok(Vec::new()),",
     "sankhya-cube", 1, "catalogue"),

    # --- Phase 5.4: maintenance that can be watched -----------------------------------------------

    # `OPS-10`. The table list was a startup snapshot, so a table created afterwards was
    # maintained by nobody for ever --- and the aggregate reclaimed-bytes figure kept rising
    # from the others, so the warehouse looked healthy.
    ("maintenance: freeze the table list at startup, as it used to",
     "crates/sankhya-maintenance/src/service.rs",
     "            Self::Everything(warehouse) => tables_under_reporting(warehouse),",
     "            Self::Everything(_) => (Vec::new(), Vec::new()),",
     "sankhya-maintenance", 1, "adoption"),

    ("maintenance: keep ticking a table that has gone",
     "crates/sankhya-maintenance/src/service.rs",
     "                        .filter(|table| !present.contains(table))",
     "                        .filter(|_| false)",
     "sankhya-maintenance", 1, "adoption"),

    # `OPS-11`. The tick's error was discarded with `Err(_) => continue`, so a table whose
    # compaction failed every thirty seconds failed silently for ever.
    ("maintenance: discard the error from a tick that failed",
     "crates/sankhya-maintenance/src/service.rs",
     "                                failed.fetch_add(1, Ordering::Relaxed);",
     "                                let _ = &failed;",
     "sankhya-maintenance", 1, "adoption"),

    ("maintenance: read an unlistable warehouse as one with no tables",
     "crates/sankhya-maintenance/src/service.rs",
     "                unlisted.push(format!(\"{}: {error}\", directory.display()));",
     "                let _ = &error;",
     "sankhya-maintenance", 1, "adoption"),

    # `OPS-12` where it costs the most: `Path::exists` answers false for every failure, so a
    # log nobody could read replayed as an empty table and a query returned no rows and
    # succeeded.
    ("delta: end the commit walk when a log cannot be read, as `exists` did",
     "crates/sankhya-table-delta/src/log.rs",
     "        match path.try_exists() {\n            Ok(true) => {}\n            Ok(false) => break,",
     "        match path.try_exists().map_err(|_| ()).or(Ok::<bool, ()>(false)) {\n            Ok(true) => {}\n            Ok(false) => break,",
     "sankhya-table-delta", 1, "log"),

    # --- Phase 5.5: a diagnostic that looks, and a catalogue that tells the truth ------------------

    # `OPS-26`. `check::storage_headroom` was written, tested, exported --- and called by
    # nothing, so `doctor` could never warn about a filling disk however full it got.
    ("server: let the doctor stop measuring free space, as it never did",
     "crates/sankhya-server/src/doctor.rs",
     "    match free_bytes(data_dir) {\n        Some(free) => {",
     "    match Option::<f64>::None {\n        Some(free) => {",
     "sankhya-server", 1, "unreadable"),

    # The remediation on the only alert that can page named `sankhya maintenance compact`,
    # and there is no `sankhya` binary.
    ("diagnostic: name a binary this build does not ship as the remediation",
     "crates/sankhya-diagnostic/src/check.rs",
     "            \"Raise the maintenance duty cycle: lower `maintenance.compact_every` (or \\",
     "            \"Compact it: `sankhya maintenance compact --table {table}`. Or \\",
     "sankhya-diagnostic", 1, "check"),

    # --- Phase 5.6: what a statement costs before it reads a row ----------------------------------

    # The bound `SANKHYA_QUERY_MEMORY_BYTES` names is *between* a server's queries. The first
    # version built a fresh pool per statement, so ten concurrent statements got ten
    # gibibytes and the setting read as solved while the machine died as before.
    ("server: give every statement its own memory pool, as 5.1b did",
     "crates/sankhya-server/src/execute.rs",
     "    RUNTIME.get_or_init(built).clone()",
     "    built()",
     "sankhya-server", 1, "wiring"),

    # `OPS-22`. The freshness probe replayed every table's log from version zero on every
    # statement, while holding the cache built to make it incremental.
    ("server: replay every log from the beginning on every statement",
     "crates/sankhya-server/src/warehouse.rs",
     "        let now = cache\n            .live_files(&table.root)\n            .ok()\n            .and_then(|(live, _)| live.version)",
     "        let now = sankhya_table_delta::live_files(&table.root)\n            .ok()\n            .and_then(|live| live.version)",
     "sankhya-server", 1, "wiring"),

    # `OPS-21`. Checkpoints were written only by tests, so every replay in the system ran
    # from version zero for the life of the warehouse.
    ("maintenance: leave checkpointing to the tests, as it used to",
     "crates/sankhya-maintenance/src/service.rs",
     "                crate::driver::checkpoint_if_due(\n                    table_root,",
     "                #[allow(unreachable_code)] crate::driver::checkpoint_if_due(\n                    return Ok(report),",
     "sankhya-maintenance", 1, "adoption"),

    # --- Phase 5.7: a door that can be moved, and a manifest that can be applied ------------------

    # `RUN-10`. `SANKHYA_LISTEN` and `SANKHYA_METRICS_LISTEN` existed and this did not, so the
    # columnar door could be moved only by writing a file --- and an image is configured by
    # environment. Two instances on one host always collided on 5434.
    ("server: leave the columnar door unconfigurable from the environment",
     "crates/sankhya-server/src/main.rs",
     "        (\"SANKHYA_FLIGHT_LISTEN\", \"server.flight_listen\"),",
     "        // the columnar door has no environment override",
     "sankhya-server", 1, "accepting"),

    # `RUN-11`. The manifest named an image at a version and no Dockerfile existed anywhere,
    # so it could not be applied by anybody --- and nothing said so.
    ("xtask: accept a manifest naming an image nothing builds",
     "xtask/src/package.rs",
     "            if !dockerfile.is_file() {",
     "            if false {",
     "xtask", 1),

    ("xtask: accept a manifest deploying a version this build is not",
     "xtask/src/package.rs",
     "            if tag != version {",
     "            if false {",
     "xtask", 1),

    # --- Phase 5.8: a query log, and events with a time on them -----------------------------------

    # `OPS-24`. There was no query log at all: the audit records that a statement happened
    # and has no idea how long it took, so "which statements are slow?" had no answer.
    ("server: run a statement without leaving a line an operator can read",
     "crates/sankhya-server/src/audit.rs",
     "    log_statement(principal, sql, touched, rows, took, answer.is_none());",
     "    let _ = &took;",
     "sankhya-server", 1, "query_log"),

    # And the shape that keeps the caller's own text out of it. `select nosuchcolumn` puts a
    # column the caller chose into a durable log; `select 'a-secret'` puts a value.
    ("server: put the word after the verb in the log whatever it is",
     "crates/sankhya-server/src/audit.rs",
     "        Some(second) if KEYWORDS.contains(&second.as_str()) => format!(\"{first} {second}\"),",
     "        Some(second) => format!(\"{first} {second}\"),",
     "sankhya-server", 1, "query_log"),

    # Colour in a log that is not going to a terminal: escape sequences in every line, and a
    # field an operator filters on reads as `\x1b[3mfeed\x1b[0m\x1b[2m=\x1b[0mpostings`.
    ("server: colour output that is not going to a terminal",
     "crates/sankhya-server/src/main.rs",
     "        .with_ansi(to_a_terminal)",
     "        .with_ansi(true)",
     "sankhya-server", 1, "query_log"),

    # `OPS-24`, the line the audit named: an operator could not determine *when* a feed
    # halted, because a `println!` carries no timestamp --- and "when" bounds what is missing.
    ("server: report a halted feed without a time on it",
     "crates/sankhya-server/src/main.rs",
     "                                tracing::error!(\n                                    feed = %name,\n                                    detail = %reason,\n                                    \"a feed stopped and will not run again until RESUME FEED\"\n                                );",
     "                                eprintln!(\"  feed `{name}` STOPPED --- {reason}\");",
     "sankhya-server", 1, "feed_exit"),

    # --- Phase 6.4 and 6.6: the price of a guarantee, and a declaration honoured ------------------

    # `FMT-02`. `Action::Protocol` was parsed and discarded, and no ceiling existed anywhere.
    # Reader version 2 is column mapping --- every column reads null; version 3 is deletion
    # vectors --- deleted rows are served as live. Both are answers rather than errors.
    ("delta: serve a table declaring a reader version this build cannot honour",
     "crates/sankhya-table-delta/src/log.rs",
     "                if *min_reader_version > SUPPORTED_READER_VERSION {",
     "                if false && *min_reader_version > SUPPORTED_READER_VERSION {",
     "sankhya-table-delta", 1, "log"),

    # --- The adversarial review: a silent wrong answer, and a door with no lock -------------------

    # A clone read under `SET SNAPSHOT` or `SET VERSION OF` answered **zero rows and
    # succeeded**. `resolve_as_of` passes no `inherited`, and a clone's own log names none of
    # its origin's files --- so it resolved a log naming nothing. The `inherited` field was
    # copied into the table built from it and never used to build it.
    ("server: resolve a clone under a snapshot without its origin",
     "crates/sankhya-server/src/snapshots.rs",
     "        let resolved = match table.inherited.as_ref() {\n            Some(inherited) => sankhya_readpath::resolve_clone_as_of(\n                std::sync::Arc::clone(&table.schema),\n                &table.root,\n                inherited,\n                at.version,",
     "        let resolved = match Option::<&sankhya_readpath::Inherited>::None {\n            Some(inherited) => sankhya_readpath::resolve_clone_as_of(\n                std::sync::Arc::clone(&table.schema),\n                &table.root,\n                inherited,\n                at.version,",
     "sankhya-server", 1, "snapshots"),

    # The columnar door read a `sankhya-user` header, checked it was non-empty, and served
    # that user --- with no credential of any kind, while the wire door refuses without one
    # and verifies it. Both doors call the same `authenticate` now.
    ("server: serve the columnar door to anyone who names a user",
     "crates/sankhya-server/src/flight.rs",
     "        self.authenticated(&user, request_metadata)?;",
     "        let _ = &request_metadata;",
     "sankhya-server", 1, "wiring"),

    # The ceiling was enforced on the JSON path only. A table an external engine upgraded and
    # then checkpointed was refused by a query and **served by a compaction** --- which reads
    # the raw Parquet, ignores the deletion vectors, and commits deleted rows back as live.
    ("delta: read a checkpoint without asking what protocol it declares",
     "crates/sankhya-table-delta/src/checkpoint.rs",
     "                    if required > crate::log::SUPPORTED_READER_VERSION {",
     "                    if false && required > crate::log::SUPPORTED_READER_VERSION {",
     "sankhya-table-delta", 1, "checkpoint"),

    # And the fallback that masked the fix: every checkpoint failure fell back to a full
    # replay, which is right for a corrupt checkpoint and wrong for an unsupported protocol.
    ("delta: fall back to a replay when a checkpoint declares a protocol we cannot honour",
     "crates/sankhya-table-delta/src/log.rs",
     "            Err(refusal @ CommitError::Unsupported { .. }) => return Err(refusal),",
     "            Err(CommitError::Unsupported { .. }) => {}",
     "sankhya-table-delta", 1, "checkpoint"),

]


def running(pid):
    """Whether a process is alive, without signalling it."""
    try:
        os.kill(pid, 0)
    except (ProcessLookupError, ValueError):
        return False
    except PermissionError:
        return True
    return True


def take_lock():
    """Claim the working tree, or explain who has it."""
    if os.path.exists(LOCK):
        try:
            with open(LOCK) as handle:
                owner = int(handle.read().strip())
        except (ValueError, OSError):
            owner = None
        # A lock whose owner is gone is stale. Respecting it would let one crashed run
        # block every later one, which is a worse failure than the one it prevents.
        if owner is not None and owner != os.getpid() and running(owner):
            print(f"another run (pid {owner}) is already mutating this tree; two runs "
                  f"restore each other's originals and leave several deliberate defects "
                  f"behind at once")
            return False
        print("taking over a lock left by a run that is no longer alive")

    with open(LOCK, "w") as handle:
        handle.write(str(os.getpid()))
    return True


def release_lock():
    if os.path.exists(LOCK):
        try:
            with open(LOCK) as handle:
                if int(handle.read().strip()) != os.getpid():
                    return
        except (ValueError, OSError):
            pass
        os.remove(LOCK)


def begin(path, original):
    """Record what is about to be mutated, so an interrupted run can be undone."""
    with open(IN_FLIGHT, "w") as handle:
        handle.write(path + "\n")
        handle.write(original)


def finish(path, original):
    """Restore the file and clear the record."""
    with open(path, "w") as handle:
        handle.write(original)
    if os.path.exists(IN_FLIGHT):
        os.remove(IN_FLIGHT)


def recover():
    """Undo a mutation left behind by an interrupted run."""
    if not os.path.exists(IN_FLIGHT):
        return
    with open(IN_FLIGHT) as handle:
        path = handle.readline().rstrip("\n")
        original = handle.read()
    if path and os.path.exists(path):
        with open(path, "w") as handle:
            handle.write(original)
        print(f"recovered {os.path.relpath(path, ROOT)} from an interrupted run")
    os.remove(IN_FLIGHT)


def digest(path):
    import hashlib

    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def regression_files():
    return set(glob.glob(os.path.join(ROOT, "crates", "*", "tests",
                                      "*.proptest-regressions")))


def judge(crate, hint):
    """Run the tests and say whether the mutation was caught.

    `hint` is an optional test target --- the seventh field of an entry --- and it exists for
    one reason: `cargo test -p sankhya-server` builds sixteen test binaries, and most mutations
    are only reachable from one of them. Running the named target first turns a five-minute
    verdict into a thirty-second one.

    A hint that is **wrong** costs a re-run and never a wrong answer. `caught` from the named
    target is caught, whatever the rest of the crate would have said. `SURVIVED` from it is not
    trusted: the whole crate is run before that verdict is reported, because a hint that named
    the wrong target would otherwise turn a covered mutation into a false alarm --- and a list
    with false alarms on it is a list people stop reading, which is the failure this whole file
    exists to prevent.
    """
    def once(arguments):
        try:
            p = subprocess.run(["cargo", "test", "-p", crate, "--quiet"] + arguments,
                               cwd=ROOT, capture_output=True, text=True, timeout=1800)
        except subprocess.TimeoutExpired:
            # A mutation that hangs is worse than one that survives: it takes the whole run
            # with it and reports nothing. This used to raise out of `main`, thirty minutes
            # in, leaving the defect applied. Reported as its own verdict so the entry can be
            # given a narrower test target or removed with the reasoning recorded.
            return "HUNG", False
        out = p.stdout + p.stderr
        if "error[E" in out or "could not compile" in out:
            # Not caught. The compiler refusing a mutation says nothing about the tests.
            #
            # This returned `True`, which `main` reads as "not a survivor", and the run then
            # printed *"all N mutations were caught"*. A catalogue in which every single entry
            # failed to compile reported total success --- which is the failure the comment
            # eighteen lines below warns about, inside the mechanism built to detect it.
            #
            # Reported as its own verdict rather than as a survivor: a mutation the compiler
            # rejects is an entry that needs rewriting, not a hole in the tests, and calling it
            # a survivor would send somebody to look for coverage that is not the problem.
            return "no compile", False

        # A non-zero exit is not a caught mutation.
        #
        # This was `if p.returncode == 0: SURVIVED else: caught`, and cargo exits non-zero for
        # reasons that have nothing to do with the defect. The one that was live: a seventh
        # field naming a test target that does not exist --- `cargo test -p xtask --test
        # package` prints *"no test target named `package`"* and exits 101, which read as
        # **caught**. Two entries were passing on it, and both would have survived their real
        # suite.
        #
        # So the verdict now requires libtest to say so. `test result: FAILED` is printed by
        # the harness only when a test actually ran and actually failed, which is the thing
        # this whole file is trying to establish.
        #
        # This is `R3` --- *gates report green when they measure nothing* --- inside the
        # mechanism built to detect exactly that.
        if "test result: FAILED" in out:
            return "caught", True
        if p.returncode == 0:
            return "SURVIVED", False
        # Non-zero, no compile error, and no failing test. Cargo could not run the suite ---
        # a missing target, a lock it could not take, a signal. Never silently a pass.
        return "DID NOT RUN", False

    if hint:
        verdict, ok = once(["--test", hint])
        # A named target that caught it, or would not compile, is the answer. A survival ---
        # and now a target that could not be run at all --- is checked against everything else,
        # because a hint naming the wrong target must cost a re-run and never a wrong answer.
        if verdict not in ("SURVIVED", "DID NOT RUN"):
            return verdict, ok
    return once([])


def declared_test_targets(crate):
    """Every `--test` target `crate` actually has.

    A `--test NAME` target comes from `tests/NAME.rs` or an explicit `[[test]]` in the
    manifest, and nothing else --- so an empty set means the crate has none, which is a real
    answer rather than a failure to look. Returning it as such is the point: the entries this
    check exists to catch name a target in a crate with no `tests/` directory at all.
    """
    targets = set()
    for base in (os.path.join(ROOT, "crates", crate), os.path.join(ROOT, crate)):
        for path in glob.glob(os.path.join(base, "tests", "*.rs")):
            targets.add(os.path.splitext(os.path.basename(path))[0])
        manifest = os.path.join(base, "Cargo.toml")
        if os.path.isfile(manifest):
            text = io.open(manifest, encoding="utf-8").read()
            for block in text.split("[[test]]")[1:]:
                match = re.search(r'name\s*=\s*"([^"]+)"', block)
                if match:
                    targets.add(match.group(1))
    return targets


def check_hints_and_duplicates():
    """Two failures `--check` could not see, both of which made an entry prove nothing.

    **A hint naming a target that does not exist.** The seventh field names a test binary so a
    verdict costs thirty seconds instead of five minutes. Nothing validated it, and
    `cargo test -p xtask --test package` --- for a crate with no `tests/` directory at all ---
    prints *"no test target named `package`"* and exits non-zero, which the runner read as
    **caught**. Two entries were passing on that.

    **Two entries naming one site.** `--check` counts occurrences of each entry's find-text in
    the source, independently, so a duplicated tuple sees one occurrence and one declaration
    and agrees with itself. The headline count is then larger than the number of distinct
    defects, which is the one number this file exists to be trusted about.
    """
    problems = []
    seen = {}
    for entry in CATALOGUE:
        label, relpath, find, replace, crate = entry[0], entry[1], entry[2], entry[3], entry[4]
        hint = entry[6] if len(entry) > 6 else None
        if hint:
            available = declared_test_targets(crate)
            if hint not in available:
                problems.append(
                    "BAD HINT   %s names test target '%s', which %s does not have. A hint that "
                    "cannot run exits non-zero and used to read as caught" % (label, hint, crate))
        key = (relpath, find, replace)
        if key in seen:
            problems.append(
                "DUPLICATE  %s and %s are the same mutation of the same site, so the catalogue "
                "counts one defect twice" % (seen[key], label))
        else:
            seen[key] = label
    return problems


def check_only():
    """Verify every catalogue entry still matches its source, without running anything.

    Two different failures land here, and both are silent otherwise. A refactor moves the
    code an entry names, and the entry then proves nothing while still reporting a pass.
    Or a run was killed hard enough to defeat the in-flight record -- `kill -9`, a lost
    machine -- and a deliberate defect is still sitting in the tree, ready to be
    committed. Neither shows up in a diff anyone reads. This costs milliseconds and no
    compilation, so it can gate every build rather than only a full audit.
    """
    # A missing comma between two entries is not a syntax error. Python reads the second
    # tuple as an element of the first, and the catalogue then holds one malformed entry
    # where three should be: the outer one runs `cargo test -p <tuple>`, and the two it
    # swallowed never run at all. That happened here and survived because every check in
    # this file only ever looked at the first three fields, which are strings either way.
    #
    # Seven fields now: the seventh is an optional test target, which lets a mutation run one
    # test binary instead of a crate's sixteen. It is checked here too, because the whole point
    # of this list is that a shape nobody validates is a shape that silently means something
    # else.
    malformed = [
        entry
        for entry in CATALOGUE
        if len(entry) not in (5, 6, 7)
        or not all(isinstance(field, str) for field in entry[:5])
        or (len(entry) > 5 and not isinstance(entry[5], int))
        or (len(entry) > 6 and not isinstance(entry[6], str))
    ]
    for entry in malformed:
        print(f"{'MALFORMED':10} {entry[0]}\n{'':10} an entry of {len(entry)} field(s); a "
              f"missing comma has swallowed the entries that follow it")
    if malformed:
        return 1

    absent = []
    # A find string that appears more often than the entry says it should mutates the
    # **first** occurrence, which need not be the one the entry is about.
    #
    # The sixth field exists for the case where an entry genuinely means every site --- a rule
    # applied at a lower bound and an upper one, say --- and where it is absent the entry means
    # exactly one. So the check is not *"does this appear twice?"* but *"does this appear more
    # times than the entry accounts for?"*.
    #
    # `math: hide a non-finite value inside an exact sum` named `if !value.is_finite() {` with
    # no count, and there are two of those in `reduce.rs`. It matched the early scan in
    # `exact_sum`, where a second guard on the scaled value masks it, instead of the
    # expansion's --- so it survived every run as a mutation of a line nobody meant, and the
    # survivor list carried it as "the tests do not cover this" when the entry did not point at
    # the code. That is the same shape the audit found in `COR-08`: an entry protecting a
    # constant rather than the property. It is mechanically checkable, so it is checked.
    ambiguous = []
    for entry in CATALOGUE:
        label, relpath, find = entry[0], entry[1], entry[2]
        declared = entry[5] if len(entry) > 5 else 1
        path = os.path.join(ROOT, relpath)
        try:
            with open(path) as handle:
                source = handle.read()
        except OSError:
            absent.append((label, relpath, "file is missing"))
            continue
        occurrences = source.count(find)
        if occurrences == 0:
            absent.append((label, relpath, "the text it names is not there"))
        elif occurrences != declared:
            ambiguous.append((label, relpath, occurrences, declared))
    for label, relpath, why in absent:
        print(f"{'UNMATCHED':10} {label}\n{'':10} {relpath}: {why}")
    for label, relpath, occurrences, declared in ambiguous:
        print(f"{'AMBIGUOUS':10} {label}\n{'':10} {relpath}: the text it names appears "
              f"{occurrences} time(s) and this entry mutates {declared}. Widen the text with "
              f"surrounding lines until it names the one site the entry is about, or declare "
              f"the count as a sixth field if it genuinely means all of them")
    if absent or ambiguous:
        print(f"\n{len(absent) + len(ambiguous)} of {len(CATALOGUE)} catalogue entries do not "
              f"name one site in the source. Either the code moved and the entry needs "
              f"updating, the entry is ambiguous, or a killed run left its mutation applied "
              f"-- check `git diff` before anything else.")
        return 1

    # Matching the source is necessary and was never sufficient. An entry can name real code,
    # apply cleanly, and still prove nothing: because the test target it names does not exist,
    # or because another entry already mutates the same site the same way.
    problems = check_hints_and_duplicates()
    if problems:
        for problem in problems:
            print("  " + problem)
        print(f"\n{len(problems)} entr(ies) match the source and cannot prove anything.")
        return 1

    print(f"all {len(CATALOGUE)} catalogue entries match the source, name a test target that "
          f"exists, and describe distinct defects")
    return 0


def main():
    if "--check" in sys.argv[1:]:
        return check_only()

    if not take_lock():
        return 2

    # Anything a previous run left behind, before deciding what to do next.
    recover()

    # A kill does not run `finally`. These do.
    def restore_and_exit(signum, _frame):
        recover()
        release_lock()
        sys.exit(128 + signum)

    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, restore_and_exit)

    pattern = sys.argv[1] if len(sys.argv) > 1 else ""
    entries = [e for e in CATALOGUE if pattern in e[0]]
    if not entries:
        print(f"no catalogue entry matches {pattern!r}")
        release_lock()
        return 2

    # A failing property test writes a regression seed, and every mutation that works is
    # *meant* to make property tests fail. Those seeds record cases that were only ever
    # interesting under deliberately broken code, so they are removed afterwards --
    # committing them would make every future run replay a case that proves nothing, and
    # would quietly dilute the real regressions in the same files.
    pre_existing = regression_files()

    # Every file this run will edit, as it was before the run.
    before = {}
    for entry in entries:
        path = os.path.join(ROOT, entry[1])
        if os.path.exists(path):
            before[path] = digest(path)


    survivors, missing, unproven = [], [], []
    for entry in entries:
        label, relpath, find, repl, crate = entry[:5]
        count = entry[5] if len(entry) > 5 else 1
        path = os.path.join(ROOT, relpath)
        original = open(path).read()
        if find not in original:
            # The code moved. The entry is stale and is silently proving nothing, which
            # is the exact failure this script exists to prevent.
            print(f"{'STALE ENTRY':10} {label}")
            missing.append(label)
            continue

        begin(path, original)
        open(path, "w").write(original.replace(find, repl, count))
        try:
            verdict, ok = judge(crate, entry[6] if len(entry) > 6 else None)
        finally:
            finish(path, original)

        print(f"{verdict:10} {label}")
        if verdict in ("no compile", "HUNG"):
            # Neither caught nor survived: the run never got far enough to ask.
            unproven.append((verdict, label))
        elif not ok:
            survivors.append(label)

    for path in regression_files() - pre_existing:
        os.remove(path)

    changed = [p for p, d in before.items() if digest(p) != d]
    if changed:
        release_lock()
        print("\nthese files were not restored and the results below cannot be trusted:")
        for path in changed:
            print(f"  {os.path.relpath(path, ROOT)}")
        return 2

    release_lock()

    print()
    if survivors:
        print(f"{len(survivors)} mutation(s) survived — the tests covering them do not "
              f"actually cover them:")
        for s in survivors:
            print(f"  - {s}")
    if unproven:
        print(f"{len(unproven)} mutation(s) never reached a verdict, so nothing was proved "
              f"about them either way:")
        for verdict, label in unproven:
            print(f"  - [{verdict}] {label}")
    if missing:
        print(f"{len(missing)} catalogue entr(ies) no longer match the source and are "
              f"proving nothing; update or remove them.")
    if not survivors and not missing and not unproven:
        print(f"all {len(entries)} mutations were caught")
    return 1 if (survivors or missing or unproven) else 0


if __name__ == "__main__":
    sys.exit(main())
