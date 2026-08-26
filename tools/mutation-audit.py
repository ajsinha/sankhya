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

Mutations edit source files in place. The script refuses to run on a dirty tree, so
`git checkout` is always a correct restore, and it restores from the in-memory original
after every mutation as well as verifying the tree is clean at the end.

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


def regression_files():
    return set(glob.glob(os.path.join(ROOT, "crates", "*", "tests",
                                      "*.proptest-regressions")))


def tree_is_clean():
    p = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT,
                       capture_output=True, text=True)
    return p.stdout.strip() == ""


def main():
    if not tree_is_clean():
        print("refusing to run on a dirty tree: mutations edit source files in place, "
              "and a clean tree is what makes the restore trustworthy")
        return 2

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

    survivors, missing = [], []
    for label, relpath, find, repl, crate in entries:
        path = os.path.join(ROOT, relpath)
        original = open(path).read()
        if find not in original:
            # The code moved. The entry is stale and is silently proving nothing, which
            # is the exact failure this script exists to prevent.
            print(f"{'STALE ENTRY':10} {label}")
            missing.append(label)
            continue

        open(path, "w").write(original.replace(find, repl, 1))
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

    if not tree_is_clean():
        print("\nthe tree is dirty after the run; restore it with `git checkout -- .` "
              "before trusting anything below")
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
