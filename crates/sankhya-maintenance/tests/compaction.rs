//! Compaction policy tests.

use proptest::prelude::*;
use sankhya_maintenance::{
    CompactionPolicy, CompactionUrgency, FileStat, PartitionState, plan_compaction,
};
use sankhya_types::Lsn;

const MIB: u64 = 1024 * 1024;

fn file(name: &str, mib: u64, at: u64) -> FileStat {
    FileStat {
        name: name.into(),
        bytes: mib * MIB,
        rows: mib * 1000,
        covers_through: Lsn::new(at),
    }
}

fn partition(files: Vec<FileStat>, quiet_for: u64) -> PartitionState {
    PartitionState {
        table: "readings".into(),
        partition: "2026-08-26".into(),
        files,
        ticks_since_write: quiet_for,
    }
}

#[test]
fn a_well_formed_partition_needs_nothing() {
    let state = partition(vec![file("a", 256, 100), file("b", 256, 200)], 0);
    assert!(plan_compaction(&CompactionPolicy::default(), &state).is_none());
}

#[test]
fn a_single_file_is_never_compacted() {
    // There is nothing to merge it with, and rewriting one file achieves nothing while
    // costing a full read and write.
    let state = partition(vec![file("only", 1, 100)], 0);
    assert!(plan_compaction(&CompactionPolicy::default(), &state).is_none());
}

#[test]
fn many_small_files_are_compacted() {
    let files: Vec<FileStat> = (0..20).map(|i| file(&format!("f{i}"), 2, 100 + i)).collect();
    let plan = plan_compaction(&CompactionPolicy::default(), &partition(files, 0))
        .expect("should compact");
    assert_eq!(plan.urgency, CompactionUrgency::Routine);
    assert!(plan.inputs.len() >= 2);
}

#[test]
fn urgency_rises_with_file_count() {
    let policy = CompactionPolicy::default();
    // Two independent triggers exist, so the sizes are chosen to isolate the count.
    //
    //   - a high FILE COUNT, regardless of size
    //   - a very small MEDIAN size, regardless of count
    //
    // At 32 MiB the median sits above the very-small rule but below the target, so
    // only the count varies across these cases.
    let cases = [
        (4usize, CompactionUrgency::None),
        (10, CompactionUrgency::Routine),
        (40, CompactionUrgency::Elevated),
        (150, CompactionUrgency::Urgent),
    ];
    for (count, expected) in cases {
        let files: Vec<FileStat> =
            (0..count).map(|i| file(&format!("f{i}"), 32, 100 + i as u64)).collect();
        let plan = plan_compaction(&policy, &partition(files, 0));
        let urgency = plan.map_or(CompactionUrgency::None, |p| p.urgency);
        assert_eq!(urgency, expected, "with {count} files of 32 MiB");
    }
}

#[test]
fn a_very_small_median_triggers_compaction_below_the_count_threshold() {
    // The second trigger. Four files of two megabytes are well below the count
    // threshold, but merging them still removes three quarters of the per-file
    // overhead for almost no rewriting — so waiting for a count that may never arrive
    // would leave an obviously improvable partition alone indefinitely.
    let files: Vec<FileStat> = (0..4).map(|i| file(&format!("f{i}"), 2, 100 + i)).collect();
    let plan = plan_compaction(&CompactionPolicy::default(), &partition(files, 0))
        .expect("a tiny-file partition should compact regardless of count");
    assert_eq!(plan.urgency, CompactionUrgency::Routine);
    assert_eq!(plan.inputs.len(), 4);
}

#[test]
fn many_well_sized_files_produce_no_plan() {
    // A partition holding many files that are ALREADY at target size has no compaction
    // problem — merging two would overshoot, reducing scan parallelism and coarsening
    // pruning for no benefit. It has a *partitioning* problem instead, and the right
    // response is to say so rather than to rewrite gigabytes to no purpose.
    let policy = CompactionPolicy::default();
    for count in [10usize, 40, 150] {
        let files: Vec<FileStat> =
            (0..count).map(|i| file(&format!("f{i}"), 256, 100 + i as u64)).collect();
        assert!(
            plan_compaction(&policy, &partition(files, 0)).is_none(),
            "{count} target-sized files should not be merged"
        );
    }
}

#[test]
fn coverage_is_preserved_exactly() {
    // A rewrite that narrowed coverage would open a gap in the read path; one that
    // widened it would claim data the file does not hold. Either is a wrong answer.
    let files = vec![file("a", 1, 100), file("b", 1, 250), file("c", 1, 175)];
    let plan = plan_compaction(&CompactionPolicy::default(), &partition(files, 0))
        .expect("should compact");
    assert_eq!(
        plan.covers_through,
        Lsn::new(250),
        "the merged output must cover exactly what its inputs did"
    );
}

#[test]
fn no_row_is_lost_in_a_merge() {
    let files: Vec<FileStat> = (0..10).map(|i| file(&format!("f{i}"), 3, 100 + i)).collect();
    let expected: u64 = files.iter().map(|f| f.rows).sum();
    let plan = plan_compaction(&CompactionPolicy::default(), &partition(files, 0))
        .expect("should compact");
    assert_eq!(plan.rows(), expected, "a merge must account for every row");
}

#[test]
fn the_smallest_files_are_merged_first() {
    // They carry the most per-file overhead per byte, so they yield the largest
    // planning improvement for the least rewriting.
    let files = vec![
        file("large", 200, 100),
        file("tiny_a", 1, 101),
        file("tiny_b", 1, 102),
        file("medium", 50, 103),
        file("tiny_c", 1, 104),
        file("m2", 40, 105),
        file("m3", 40, 106),
        file("m4", 40, 107),
        file("m5", 40, 108),
    ];
    let plan = plan_compaction(&CompactionPolicy::default(), &partition(files, 0))
        .expect("should compact");
    let names = plan.input_names();
    for tiny in ["tiny_a", "tiny_b", "tiny_c"] {
        assert!(names.contains(&tiny), "{tiny} should be merged first; got {names:?}");
    }
}

#[test]
fn a_merge_does_not_overshoot_the_target_size() {
    // A file larger than the target is worse than two files near it: it reduces scan
    // parallelism and coarsens pruning granularity.
    let policy = CompactionPolicy::default();
    let files: Vec<FileStat> = (0..12).map(|i| file(&format!("f{i}"), 100, 100 + i)).collect();
    let plan = plan_compaction(&policy, &partition(files, 0)).expect("should compact");
    assert!(
        plan.bytes() <= policy.target_bytes,
        "merged {} bytes against a {} target",
        plan.bytes(),
        policy.target_bytes
    );
}

#[test]
fn a_pass_is_bounded_so_it_cannot_monopolise_the_budget() {
    // Also bounds how much work an interrupted pass loses.
    let policy = CompactionPolicy { max_files_per_pass: 5, ..CompactionPolicy::default() };
    let files: Vec<FileStat> = (0..200).map(|i| file(&format!("f{i}"), 1, 100 + i)).collect();
    let plan = plan_compaction(&policy, &partition(files, 0)).expect("should compact");
    assert!(plan.inputs.len() <= 5);
    assert_eq!(plan.urgency, CompactionUrgency::Urgent, "200 files is urgent");
}

#[test]
fn a_settled_partition_is_marked_for_sorting_but_a_busy_one_is_not() {
    // Re-sorting a partition still receiving writes means doing it again tomorrow.
    let files: Vec<FileStat> = (0..10).map(|i| file(&format!("f{i}"), 2, 100 + i)).collect();
    let policy = CompactionPolicy::default();

    let busy = plan_compaction(&policy, &partition(files.clone(), 0)).expect("compacts");
    assert!(!busy.settled);

    let quiet = plan_compaction(&policy, &partition(files, policy.settle_ticks + 1))
        .expect("compacts");
    assert!(quiet.settled);
}

#[test]
fn a_partition_of_target_sized_files_below_the_count_threshold_is_left_alone() {
    // Rewriting well-formed files is pure write amplification for no benefit.
    let files: Vec<FileStat> = (0..4).map(|i| file(&format!("f{i}"), 256, 100 + i)).collect();
    assert!(plan_compaction(&CompactionPolicy::default(), &partition(files, 0)).is_none());
}

#[test]
fn the_reason_describes_the_partition_an_operator_would_see() {
    let files: Vec<FileStat> = (0..15).map(|i| file(&format!("f{i}"), 1, 100 + i)).collect();
    let plan = plan_compaction(&CompactionPolicy::default(), &partition(files, 0))
        .expect("compacts");
    assert!(plan.reason.contains("15 files"), "{}", plan.reason);
    assert!(plan.reason.contains("median"), "{}", plan.reason);
}

proptest! {
    /// A plan never claims more coverage than its inputs hold, and never less.
    #[test]
    fn coverage_always_matches_the_inputs(
        sizes in prop::collection::vec(1u64..300, 2..40),
        positions in prop::collection::vec(1u64..10_000, 2..40),
    ) {
        let n = sizes.len().min(positions.len());
        let files: Vec<FileStat> = (0..n)
            .map(|i| file(&format!("f{i}"), sizes[i], positions[i]))
            .collect();

        if let Some(plan) = plan_compaction(&CompactionPolicy::default(), &partition(files, 0)) {
            let expected = plan.inputs.iter().map(|f| f.covers_through).max().unwrap_or(Lsn::ZERO);
            prop_assert_eq!(plan.covers_through, expected);
        }
    }

    /// Planning never panics, and never selects a single file.
    #[test]
    fn planning_never_panics_or_selects_one_file(
        sizes in prop::collection::vec(0u64..1000, 0..60),
        quiet in 0u64..200,
    ) {
        let files: Vec<FileStat> = sizes
            .iter()
            .enumerate()
            .map(|(i, s)| file(&format!("f{i}"), *s, 100 + i as u64))
            .collect();
        if let Some(plan) = plan_compaction(&CompactionPolicy::default(), &partition(files, quiet)) {
            prop_assert!(plan.inputs.len() >= 2, "a plan must merge at least two files");
        }
    }

    /// Compacting always reduces the file count.
    ///
    /// The whole purpose. A plan that replaced N files with N or more would be pure
    /// write amplification.
    #[test]
    fn compaction_always_reduces_the_file_count(sizes in prop::collection::vec(1u64..40, 2..60)) {
        let before = sizes.len();
        let files: Vec<FileStat> = sizes
            .iter()
            .enumerate()
            .map(|(i, s)| file(&format!("f{i}"), *s, 100 + i as u64))
            .collect();
        if let Some(plan) = plan_compaction(&CompactionPolicy::default(), &partition(files, 0)) {
            let after = before - plan.inputs.len() + 1; // inputs replaced by one output
            prop_assert!(after < before, "{before} files became {after}");
        }
    }
}
