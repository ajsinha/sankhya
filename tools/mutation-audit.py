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
     "        live.retain(|f| !superseded.contains(std::ffi::OsStr::new(f.name.as_str())));",
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
     'let name = format!("compacted-{sequence:06}-{index:04}.parquet");',
     'let name = format!("compacted-{index:04}.parquet");',
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
    ("log: allow a second commit to overwrite an existing version",
     "crates/sankhya-table-delta/src/log.rs",
     "    if path.exists() {",
     "    if false {",
     "sankhya-table-delta",
     2),

    ("log: skip a malformed line instead of reporting it",
     "crates/sankhya-table-delta/src/log.rs",
     "            let action: Action =\n                serde_json::from_str(line).map_err(|e| CommitError::Malformed {\n                    version,\n                    detail: e.to_string(),\n                })?;\n            out.push((version, action));",
     "            if let Ok(action) = serde_json::from_str::<Action>(line) {\n                out.push((version, action));\n            }",
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
     "        actions.push(Action::Add(AddFile::with_statistics(\n            name(&outcome.output),\n            outcome.bytes,\n            now,\n            &statistics,\n        )));",
     "        actions.push(Action::Add(AddFile::with_rows(\n            name(&outcome.output),\n            outcome.bytes,\n            now,\n            outcome.rows,\n        )));",
     "sankhya-maintenance"),

    ("ingest: publish a file without the statistics it could have carried",
     "crates/sankhya-ingest/src/pipeline.rs",
     "            let action = DeltaAction::Add(DeltaAdd::with_statistics(\n                file_name.clone(),\n                report.bytes,\n                0,\n                &statistics,\n            ));",
     "            let action = DeltaAction::Add(DeltaAdd::with_rows(\n                file_name.clone(),\n                report.bytes,\n                0,\n                u64::try_from(report.rows).unwrap_or(0),\n            ));",
     "sankhya-ingest"),

    ("log: replay by scanning the file list instead of indexing it",
     "crates/sankhya-table-delta/src/log.rs",
     "            Action::Add(add) => match self.position.get(&add.path).copied() {\n                Some(index) => {\n                    if let Some(slot) = self.files.get_mut(index) {\n                        *slot = Some(add);\n                    }\n                }\n                None => {\n                    self.position.insert(add.path.clone(), self.files.len());\n                    self.files.push(Some(add));\n                }\n            },",
     "            Action::Add(add) => {\n                if let Some(existing) =\n                    self.files.iter_mut().flatten().find(|f| f.path == add.path)\n                {\n                    *existing = add;\n                } else {\n                    self.files.push(Some(add));\n                }\n            }",
     "sankhya-table-delta"),

    ("cache: trust the cached version instead of asking the log",
     "crates/sankhya-table-delta/src/cache.rs",
     "        let newest = newest_after(table_root, cached.flatten());",
     "        let newest = cached.flatten();",
     "sankhya-table-delta"),

    ("cache: resume from a stale base after the table was rebuilt",
     "crates/sankhya-table-delta/src/cache.rs",
     "        if rebuilt {\n            entries.remove(table_root);\n        }",
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

    ("ingest: fail a publish instead of rebasing on a version conflict",
     "crates/sankhya-ingest/src/pipeline.rs",
     "            Err(sankhya_table_delta::CommitError::VersionTaken(_)) => {\n                version = newest().map_or(version.saturating_add(1), |v| v.saturating_add(1));\n            }",
     "            Err(sankhya_table_delta::CommitError::VersionTaken(v)) => {\n                return Err(Error::StorageUnavailable(format!(\"version {v} is taken\")));\n            }",
     "sankhya-ingest"),

    ("ingest: retry a failure that is not a version race",
     "crates/sankhya-ingest/src/pipeline.rs",
     "            Err(e) => return Err(Error::StorageUnavailable(e.to_string())),\n        }\n    }\n\n    Err(Error::StorageUnavailable(format!(",
     "            Err(_) => continue,\n        }\n    }\n\n    Err(Error::StorageUnavailable(format!(",
     "sankhya-ingest"),

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
     "        if fit.r_squared < LINEAR_ENOUGH && self.observations.len() > MINIMUM_OBSERVATIONS {",
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

    ("math: call a constant series a bad linear fit",
     "crates/sankhya-math/src/stats.rs",
     "        Err(VectorError::ZeroMagnitude) => 1.0,",
     "        Err(VectorError::ZeroMagnitude) => 0.0,",
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
     '                let _ = writeln!(out, "# TYPE {} {}", metric.name, metric.kind.as_str());\n                continue;',
     "                continue;",
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
     "    let plan = context\n        .state()\n        .create_logical_plan(sql)\n        .await\n        .map_err(|error| plan_failure(&error))?;\n    refuse_if_not_a_read(&plan)?;\n\n    let frame = context\n        .execute_logical_plan(plan)\n        .await\n        .map_err(|error| plan_failure(&error))?;",
     "    let frame = context.sql(sql).await.map_err(|error| plan_failure(&error))?;\n    refuse_if_not_a_read(frame.logical_plan())?;",
     "sankhya-server"),

    ("server: stop unwrapping the engine's diagnostic wrapper",
     "crates/sankhya-server/src/execute.rs",
     "        E::Diagnostic(_, inner) | E::Context(_, inner) => classify(inner),",
     "        E::Context(_, inner) => classify(inner),",
     "sankhya-server"),

    ("server: send the engine's message with no catalogue code",
     "crates/sankhya-server/src/execute.rs",
     '        message: format!("[{}] {}", classified.code(), error),',
     "        message: error.to_string(),",
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
     "    let Some(last) = last_pass else {",
     "    let Some(last) = last_pass.or(Some(now)) else {",
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


def check_only():
    """Verify every catalogue entry still matches its source, without running anything.

    Two different failures land here, and both are silent otherwise. A refactor moves the
    code an entry names, and the entry then proves nothing while still reporting a pass.
    Or a run was killed hard enough to defeat the in-flight record -- `kill -9`, a lost
    machine -- and a deliberate defect is still sitting in the tree, ready to be
    committed. Neither shows up in a diff anyone reads. This costs milliseconds and no
    compilation, so it can gate every build rather than only a full audit.
    """
    absent = []
    for entry in CATALOGUE:
        label, relpath, find = entry[0], entry[1], entry[2]
        path = os.path.join(ROOT, relpath)
        try:
            with open(path) as handle:
                source = handle.read()
        except OSError:
            absent.append((label, relpath, "file is missing"))
            continue
        if find not in source:
            absent.append((label, relpath, "the text it names is not there"))
    for label, relpath, why in absent:
        print(f"{'UNMATCHED':10} {label}\n{'':10} {relpath}: {why}")
    if absent:
        print(f"\n{len(absent)} of {len(CATALOGUE)} catalogue entries do not match the "
              f"source. Either the code moved and the entry needs updating, or a killed "
              f"run left its mutation applied -- check `git diff` before anything else.")
        return 1
    print(f"all {len(CATALOGUE)} catalogue entries match the source")
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

    survivors, missing = [], []
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
            p = subprocess.run(["cargo", "test", "-p", crate, "--quiet"],
                               cwd=ROOT, capture_output=True, text=True, timeout=1800)
            out = p.stdout + p.stderr
            if p.returncode == 0:
                verdict, ok = "SURVIVED", False
            elif "error[E" in out or "could not compile" in out:
                verdict, ok = "no compile", True
            else:
                verdict, ok = "caught", True
        finally:
            finish(path, original)

        print(f"{verdict:10} {label}")
        if not ok:
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
    if missing:
        print(f"{len(missing)} catalogue entr(ies) no longer match the source and are "
              f"proving nothing; update or remove them.")
    if not survivors and not missing:
        print(f"all {len(entries)} mutations were caught")
    return 1 if (survivors or missing) else 0


if __name__ == "__main__":
    sys.exit(main())
