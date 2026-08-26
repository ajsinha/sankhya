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
     "            if front.coverage.end_inclusive() <= self.durable_through {",
     "            if true {",
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
     "                Action::Add(add) => match self.position.get(&add.path) {\n                    Some(index) => self.files[*index] = Some(add),\n                    None => {\n                        self.position.insert(add.path.clone(), self.files.len());\n                        self.files.push(Some(add));\n                    }\n                },",
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
     "crates/sankhya-numeric/src/quantile.rs",
     "    if let Some(at) = values.iter().position(|v| v.is_nan()) {\n        return Err(QuantileError::NotOrderable { at });\n    }",
     "",
     "sankhya-numeric"),

    ("quantile: answer an empty input with zero",
     "crates/sankhya-numeric/src/quantile.rs",
     "    if values.is_empty() {\n        return Err(QuantileError::Empty);\n    }",
     "    if values.is_empty() {\n        return Ok(0.0);\n    }",
     "sankhya-numeric"),

    ("quantile: collapse linear interpolation onto the lower observation",
     "crates/sankhya-numeric/src/quantile.rs",
     "            Ok(lower + (upper - lower) * fraction)",
     "            let _ = upper;\n            Ok(lower)",
     "sankhya-numeric"),

    ("quantile: sum across the wrong element, so vectors stop lining up by scenario",
     "crates/sankhya-numeric/src/quantile.rs",
     "vectors.iter().map(|v| v[element]).collect()",
     "vectors.iter().map(|v| v[0]).collect()",
     "sankhya-numeric"),

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
     "                Action::Add(add) => match self.position.get(&add.path) {\n                    Some(index) => self.files[*index] = Some(add),\n                    None => {\n                        self.position.insert(add.path.clone(), self.files.len());\n                        self.files.push(Some(add));\n                    }\n                },",
     "                Action::Add(add) => {\n                    if let Some(existing) =\n                        self.files.iter_mut().flatten().find(|f| f.path == add.path)\n                    {\n                        *existing = add;\n                    } else {\n                        self.files.push(Some(add));\n                    }\n                }",
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


def main():
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
