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
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

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
     "                if let Some(existing) = files.iter_mut().find(|f| f.path == add.path) {\n                    *existing = add;\n                } else {\n                    files.push(add);\n                }",
     "                files.push(add);",
     "sankhya-table-delta"),

    ("log: replay commits in directory order rather than version order",
     "crates/sankhya-table-delta/src/log.rs",
     "    commits.sort_by_key(|(v, _)| *v);",
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

    ("provider: fall back to the whole live set when the log will not replay",
     "crates/sankhya-readpath/src/provider.rs",
     "                let live = live_files(table_root)?;",
     "                let live = live_files(table_root).unwrap_or_default();",
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
     "                &[DeltaAction::Add(DeltaAdd::with_statistics(\n                    file_name.clone(),\n                    report.bytes,\n                    0,\n                    &statistics,\n                ))],",
     "                &[DeltaAction::Add(DeltaAdd::with_rows(\n                    file_name.clone(),\n                    report.bytes,\n                    0,\n                    u64::try_from(report.rows).unwrap_or(0),\n                ))],",
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


def digest(path):
    import hashlib

    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def regression_files():
    return set(glob.glob(os.path.join(ROOT, "crates", "*", "tests",
                                      "*.proptest-regressions")))


def main():
    pattern = sys.argv[1] if len(sys.argv) > 1 else ""
    entries = [e for e in CATALOGUE if pattern in e[0]]
    if not entries:
        print(f"no catalogue entry matches {pattern!r}")
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
            open(path, "w").write(original)

        print(f"{verdict:10} {label}")
        if not ok:
            survivors.append(label)

    for path in regression_files() - pre_existing:
        os.remove(path)

    changed = [p for p, d in before.items() if digest(p) != d]
    if changed:
        print("\nthese files were not restored and the results below cannot be trusted:")
        for path in changed:
            print(f"  {os.path.relpath(path, ROOT)}")
        return 2

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
