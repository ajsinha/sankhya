//! The loop, over real files.
//!
//! Everything below this has been tested in isolation: the policy decides, the
//! scheduler arbitrates, the merge merges, retirement refuses. This is the test that
//! they compose — that a warehouse left fragmented by continuous capture is actually
//! cleared by ticking, and that the data survives it.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_maintenance::{
    apply, commit_tick, execute_tick, plan_tick, retire_completed, StillReferenced, Class, CompactionPolicy, CompactionUrgency,
    DriverPolicy, FileStat, PartitionState, RetentionPolicy, SystemState,
};
use sankhya_table::{read_parquet_stats, write_parquet, WriterConfig};
use sankhya_types::Lsn;
use std::collections::BTreeSet;
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("bucket", DataType::Utf8, false),
    ]))
}

fn batch(start: i64, rows: i64) -> RecordBatch {
    let ids: Vec<i64> = (start..start + rows).collect();
    let buckets: Vec<String> = ids.iter().map(|i| format!("b{}", i % 5)).collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(buckets)),
        ],
    )
    .expect("building")
}

/// A partition as continuous capture would leave it: many small files.
fn fragmented(dir: &std::path::Path, table: &str, files: i64, rows_each: i64) -> PartitionState {
    let stats: Vec<FileStat> = (0..files)
        .map(|i| {
            let name = format!("part-{i:04}.parquet");
            let lsn = Lsn::new(1000 + u64::try_from(i).expect("small"));
            let report = write_parquet(
                dir,
                &name,
                &batch(i * rows_each, rows_each),
                lsn,
                WriterConfig::default(),
            )
            .expect("writing");
            FileStat {
                name,
                bytes: report.bytes,
                rows: u64::try_from(rows_each).expect("small"),
                covers_through: lsn,
            }
        })
        .collect();

    PartitionState {
        table: table.to_string(),
        partition: "dt=2026-08-26".to_string(),
        files: stats,
        ticks_since_write: 100,
    }
}

fn quiet() -> SystemState {
    SystemState {
        in_maintenance_window: false,
        queries_running: 0,
        duty_cycle_ticks_remaining: 10_000,
    }
}

fn policy() -> DriverPolicy {
    DriverPolicy {
        compaction: CompactionPolicy {
            small_file_bytes: 1024 * 1024,
            routine_file_count: 6,
            elevated_file_count: 12,
            urgent_file_count: 40,
            ..CompactionPolicy::default()
        },
        ..DriverPolicy::default()
    }
}

#[test]
fn a_tick_clears_a_fragmented_partition_without_losing_a_row() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let partition = fragmented(dir.path(), "sales.orders", 20, 1_000);
    let rows_before: u64 = partition.files.iter().map(|f| f.rows).sum();

    let plan = plan_tick(&[partition], &policy(), &quiet());
    assert_eq!(plan.run.len(), 1, "the partition needs compacting");

    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");

    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.merged.len(), 1);

    let merged_rows: u64 = report.merged.iter().map(|o| o.rows).sum();
    assert_eq!(merged_rows, rows_before, "the tick lost rows");
    assert!(report.bytes_after < report.bytes_before);
}

#[test]
fn a_well_kept_partition_produces_no_work() {
    // The loop must be quiet when there is nothing to do. A driver that always finds
    // work burns the duty cycle on nothing and hides the partitions that need it.
    //
    // "Well kept" means files at their target size, not merely few of them. The policy
    // has two independent triggers, and a handful of tiny files legitimately fires the
    // second one -- a partition can be small in count and still be nothing but
    // fragments. So this fixture makes the files genuinely large enough by moving the
    // threshold beneath them, rather than by writing fewer.
    let dir = tempfile::tempdir().expect("a temp dir");
    let partition = fragmented(dir.path(), "sales.orders", 2, 1_000);

    let settled = DriverPolicy {
        compaction: CompactionPolicy {
            small_file_bytes: 1,
            ..policy().compaction
        },
        ..policy()
    };
    let plan = plan_tick(&[partition], &settled, &quiet());
    assert!(plan.run.is_empty());
    assert!(plan.deferred.is_empty());
    assert!(!plan.preempts_queries);
}

#[test]
fn a_degrading_partition_is_an_availability_problem_and_may_preempt() {
    // Urgent means the partition is degrading faster than it is being cleared, which is
    // self-reinforcing: more files, slower queries, less capacity to compact. That is
    // an availability problem, not a performance one, and it goes ahead of the queries
    // it is making slow.
    let dir = tempfile::tempdir().expect("a temp dir");
    let partition = fragmented(dir.path(), "sales.orders", 60, 100);

    let busy = SystemState {
        queries_running: 20,
        duty_cycle_ticks_remaining: 0,
        ..quiet()
    };
    let plan = plan_tick(&[partition], &policy(), &busy);

    assert_eq!(plan.run.len(), 1, "an exhausted budget must not stop this");
    assert_eq!(plan.run[0].plan.urgency, CompactionUrgency::Urgent);
    assert_eq!(plan.run[0].job.class, Class::Availability);
    assert!(plan.preempts_queries);
}

#[test]
fn ordinary_compaction_waits_for_the_queries_it_would_slow() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let partition = fragmented(dir.path(), "sales.orders", 8, 100);

    let busy = SystemState {
        queries_running: 20,
        duty_cycle_ticks_remaining: 0,
        ..quiet()
    };
    let plan = plan_tick(&[partition], &policy(), &busy);

    assert!(plan.run.is_empty(), "routine work must not preempt a query");
    assert_eq!(plan.deferred.len(), 1);
    assert!(!plan.preempts_queries);
}

#[test]
fn the_worst_partition_is_cleared_first() {
    // Several partitions, one degrading. Ordering by how soon each becomes visible puts
    // it first regardless of how much larger the others' backlogs are.
    let dir = tempfile::tempdir().expect("a temp dir");
    let a = fragmented(&dir.path().join("a"), "sales.orders", 7, 100);
    let b = fragmented(&dir.path().join("b"), "sales.returns", 60, 100);
    let c = fragmented(&dir.path().join("c"), "sales.items", 14, 100);

    let plan = plan_tick(&[a, b, c], &policy(), &quiet());
    let order: Vec<&str> = plan.run.iter().map(|p| p.plan.table.as_str()).collect();

    assert_eq!(order, vec!["sales.returns", "sales.items", "sales.orders"]);
}

#[test]
fn one_failing_partition_does_not_block_the_others() {
    // Partitions are independent. Stopping the tick on the first failure would let one
    // bad partition hold up maintenance for the whole warehouse -- and the failure
    // modes that reach here are exactly the ones that persist, so it would hold it up
    // permanently.
    let dir = tempfile::tempdir().expect("a temp dir");
    let good = fragmented(&dir.path().join("good"), "sales.orders", 8, 100);
    let mut bad = fragmented(&dir.path().join("bad"), "sales.returns", 8, 100);
    // A plan referring to a file that is not there.
    bad.files[0].name = "vanished.parquet".to_string();

    let plan = plan_tick(&[good, bad], &policy(), &quiet());
    assert_eq!(plan.run.len(), 2);

    // Both plans point at the same directory here, so the good one's files resolve and
    // the bad one's do not.
    let report =
        execute_tick(&plan, &dir.path().join("good"), 1, WriterConfig::default()).expect("ticking");

    assert_eq!(
        report.merged.len(),
        1,
        "the healthy partition was compacted"
    );
    assert_eq!(report.failed.len(), 1, "and the broken one was reported");
    assert!(report.failed[0].1.contains("vanished.parquet"));
}

#[test]
fn nothing_is_removed_on_the_tick_that_merged_it() {
    // The grace period exists to outlast readers that listed files before the merge.
    // Merging and retiring in one pass would make it unobservable and the safety it
    // provides theoretical.
    let dir = tempfile::tempdir().expect("a temp dir");
    let partition = fragmented(dir.path(), "sales.orders", 10, 500);
    let names: Vec<String> = partition.files.iter().map(|f| f.name.clone()).collect();

    let plan = plan_tick(&[partition], &policy(), &quiet());
    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");

    assert!(report.files_removed.is_empty());
    assert_eq!(report.bytes_reclaimed, 0);
    for name in &names {
        assert!(
            dir.path().join(name).exists(),
            "{name} was removed too early"
        );
    }

    // A later tick, once the grace period has passed, does remove them.
    let outcomes: Vec<_> = report.merged.iter().map(|o| (o.clone(), 100u64)).collect();
    let retired = retire_completed(&outcomes, &StillReferenced::nothing(), &RetentionPolicy::default())
        .expect("retiring");

    assert_eq!(retired.files_removed.len(), 10);
    assert!(retired.bytes_reclaimed > 0);
    for name in &names {
        assert!(!dir.path().join(name).exists());
    }
    assert!(report.merged[0].output.exists(), "the replacement survived");
}

#[test]
fn ticking_twice_does_not_overwrite_the_first_ticks_output() {
    // A later merge overwriting an earlier one would pull a file out from under a
    // reader still holding it -- the exact hazard the add-only rule exists to avoid.
    let dir = tempfile::tempdir().expect("a temp dir");
    let partition = fragmented(dir.path(), "sales.orders", 10, 500);

    let plan = plan_tick(&[partition.clone()], &policy(), &quiet());
    let first = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("tick 1");
    let second = execute_tick(&plan, dir.path(), 2, WriterConfig::default()).expect("tick 2");

    assert_ne!(first.merged[0].output, second.merged[0].output);
    assert!(first.merged[0].output.exists());
    assert!(second.merged[0].output.exists());

    let (rows_a, _) = read_parquet_stats(&first.merged[0].output).expect("stats");
    let (rows_b, _) = read_parquet_stats(&second.merged[0].output).expect("stats");
    assert_eq!(rows_a, rows_b, "the same inputs must yield the same rows");
}

#[test]
fn the_estimate_never_rounds_a_merge_down_to_free() {
    // A merge estimated at zero ticks costs nothing against the budget, so an unbounded
    // number of them would pass in one cycle -- which is the budget not existing.
    let dir = tempfile::tempdir().expect("a temp dir");
    let partition = fragmented(dir.path(), "sales.orders", 8, 10);

    let generous = DriverPolicy {
        bytes_per_tick: u64::MAX,
        ..policy()
    };
    let plan = plan_tick(&[partition], &generous, &quiet());

    assert_eq!(plan.run.len(), 1);
    assert!(plan.run[0].job.estimated_ticks >= 1);
}

#[test]
fn the_duty_cycle_bounds_how_much_a_tick_attempts() {
    // The reason a compaction pass is bounded in the first place: a tick must leave
    // capacity for the queries it exists to protect.
    let dir = tempfile::tempdir().expect("a temp dir");
    let partitions: Vec<PartitionState> = (0..6)
        .map(|i| {
            fragmented(
                &dir.path().join(format!("p{i}")),
                &format!("sales.t{i}"),
                8,
                20_000,
            )
        })
        .collect();

    let tight = SystemState {
        duty_cycle_ticks_remaining: 1,
        ..quiet()
    };
    let plan = plan_tick(&partitions, &policy(), &tight);

    assert!(plan.run.len() < partitions.len(), "the budget was ignored");
    assert!(!plan.deferred.is_empty());
    assert!(plan.deferred[0].1.contains("duty cycle"));
}

/// Every `.parquet` file physically present, regardless of whether it is still part of
/// the table.
///
/// Used only to demonstrate the difference between what is on disk and what is live.
fn files_on_disk(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .expect("listing")
        .filter(|e| {
            e.as_ref()
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".parquet")
        })
        .count()
}

fn partition_of(files: &[FileStat]) -> PartitionState {
    PartitionState {
        table: "sales.orders".to_string(),
        partition: "dt=2026-08-26".to_string(),
        files: files.to_vec(),
        ticks_since_write: 100,
    }
}

#[test]
fn ticking_converges_and_the_data_is_unchanged() {
    // The whole point, end to end. A warehouse left fragmented by continuous capture is
    // cleared by ticking, it stops when there is nothing left to do, and every row
    // survives exactly once.
    //
    // The live set is carried across ticks rather than re-read from the directory. That
    // is not a convenience: between a merge and the retirement of its inputs the
    // directory holds both, so a planner re-reading it would merge files that were
    // already superseded and duplicate their rows.
    let dir = tempfile::tempdir().expect("a temp dir");
    let initial = fragmented(dir.path(), "sales.orders", 60, 500);

    let mut live = initial.files.clone();
    let rows_before: u64 = live.iter().map(|f| f.rows).sum();
    let files_before = live.len();
    assert_eq!(rows_before, 30_000);

    let mut pending: Vec<(sankhya_table::CompactionOutcome, u64)> = Vec::new();
    let mut ticks = 0u64;

    loop {
        ticks += 1;
        assert!(ticks < 50, "the loop did not converge");

        let plan = plan_tick(&[partition_of(&live)], &policy(), &quiet());
        if plan.run.is_empty() {
            break;
        }

        let report =
            execute_tick(&plan, dir.path(), ticks, WriterConfig::default()).expect("ticking");
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        // The live set moves forward immediately; the files do not go anywhere yet.
        apply(&mut live, &report, dir.path());
        pending.extend(report.merged.into_iter().map(|o| (o, 0u64)));

        // Retire what earlier ticks merged, once the grace period has passed. Safe now
        // precisely because those files left the live set when they were superseded.
        let due: Vec<_> = std::mem::take(&mut pending)
            .into_iter()
            .map(|(o, age)| (o, age + 100))
            .collect();
        retire_completed(&due, &StillReferenced::nothing(), &RetentionPolicy::default()).expect("retiring");
    }

    assert!(
        live.len() < files_before,
        "{} live files became {}",
        files_before,
        live.len()
    );
    let rows_after: u64 = live.iter().map(|f| f.rows).sum();
    assert_eq!(
        rows_after, rows_before,
        "converged on the wrong number of rows"
    );

    // Genuinely settled: another tick finds nothing.
    let idle = plan_tick(&[partition_of(&live)], &policy(), &quiet());
    assert!(idle.run.is_empty());
}

#[test]
fn a_directory_listing_would_have_double_counted() {
    // The reason the live set exists, demonstrated rather than asserted. After one
    // merge the directory holds the inputs and the output together, so counting rows
    // from the directory counts every merged row twice. Nothing is wrong on disk --
    // this is the add-only rule working as designed -- but a planner or a reader that
    // trusts the listing is wrong for the whole grace period.
    let dir = tempfile::tempdir().expect("a temp dir");
    let initial = fragmented(dir.path(), "sales.orders", 10, 500);
    let mut live = initial.files.clone();
    let rows = 5_000u64;

    let plan = plan_tick(&[partition_of(&live)], &policy(), &quiet());
    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");
    apply(&mut live, &report, dir.path());

    let live_rows: u64 = live.iter().map(|f| f.rows).sum();
    assert_eq!(live_rows, rows, "the live set is correct");

    // On disk, both forms are present.
    assert_eq!(files_on_disk(dir.path()), 11);
    let mut listed_rows = 0u64;
    for entry in std::fs::read_dir(dir.path()).expect("listing") {
        let path = entry.expect("an entry").path();
        if path.extension().is_some_and(|e| e == "parquet") {
            listed_rows += read_parquet_stats(&path).expect("stats").0;
        }
    }
    assert_eq!(
        listed_rows,
        rows * 2,
        "a listing must double-count here, or this test is not demonstrating anything"
    );
}

#[test]
fn applying_a_tick_leaves_untouched_files_alone() {
    // A merge bounded by max_files_per_pass leaves the rest of the partition behind.
    // Those files must survive the update unchanged, including their declared coverage:
    // rewriting coverage a merge did not change is a chance to get it wrong for no
    // benefit.
    let dir = tempfile::tempdir().expect("a temp dir");
    let initial = fragmented(dir.path(), "sales.orders", 12, 100);
    let mut live = initial.files.clone();

    let plan = plan_tick(&[partition_of(&live)], &policy(), &quiet());
    let merged_names: Vec<String> = plan.run[0]
        .plan
        .inputs
        .iter()
        .map(|f| f.name.clone())
        .collect();

    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");
    apply(&mut live, &report, dir.path());

    for original in &initial.files {
        if merged_names.contains(&original.name) {
            assert!(
                !live.iter().any(|f| f.name == original.name),
                "{} was merged and must have left the live set",
                original.name
            );
        } else {
            let kept = live
                .iter()
                .find(|f| f.name == original.name)
                .expect("an untouched file must remain");
            assert_eq!(kept, original, "an untouched file was rewritten");
        }
    }
}

// --- partitioned paths ---------------------------------------------------
//
// At the table root a bare file name and a path relative to the root are the same string,
// so every test above passes whichever one the code uses. These distinguish them, and both
// properties were wrong until a partitioned table made them observable.

/// A table whose files live inside a partition directory.
fn partitioned_table(dir: &std::path::Path) -> Vec<FileStat> {
    let partition = dir.join("sank_data_date=2026-08-28");
    std::fs::create_dir_all(&partition).expect("creating the partition");
    let mut out = Vec::new();
    for index in 0..4u64 {
        let name = format!("sank_data_date=2026-08-28/part-{index:04}.parquet");
        let report = sankhya_table::write_parquet(
            dir,
            &name,
            &batch(index as i64 * 100, 50),
            Lsn::new(index + 1),
            WriterConfig::default(),
        )
        .expect("written");
        out.push(FileStat {
            name,
            bytes: report.bytes,
            rows: report.rows as u64,
            covers_through: Lsn::new(index + 1),
        });
    }
    out
}

#[test]
fn a_merged_file_lands_inside_the_partition_its_rows_belong_to() {
    // A merge outside the partition leaves the rows' dates disagreeing with the directory
    // holding them, and the add action carries no partition value — so an external reader
    // sees a file belonging to no partition at all.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let files = partitioned_table(dir.path());

    let plan = plan_tick(
        &[PartitionState {
            table: "t".to_string(),
            // Deliberately *not* a directory name: a partition identifier is a label, and
            // deriving the output path from it is what broke this.
            partition: "all".to_string(),
            files,
            ticks_since_write: 0,
        }],
        &DriverPolicy {
            compaction: CompactionPolicy {
                small_file_bytes: 64 * 1024 * 1024,
                ..CompactionPolicy::default()
            },
            ..DriverPolicy::default()
        },
        &quiet(),
    );
    assert!(!plan.run.is_empty(), "nothing was planned");

    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let merged = &report.merged.first().expect("one merge").output;
    assert!(
        merged.starts_with(dir.path().join("sank_data_date=2026-08-28")),
        "the merged file landed outside its partition: {}",
        merged.display()
    );
}

#[test]
fn applying_a_tick_retires_partitioned_inputs_from_the_live_set() {
    // `apply` matched on the bare file name. With partitioned paths the live set holds
    // `sank_data_date=…/part-0000.parquet` and the bare name is `part-0000.parquet`, so
    // nothing ever matched: every merged input stayed live and the set grew by one phantom
    // entry per tick.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut live = partitioned_table(dir.path());
    let before = live.len();

    let plan = plan_tick(
        &[PartitionState {
            table: "t".to_string(),
            partition: "sank_data_date=2026-08-28".to_string(),
            files: live.clone(),
            ticks_since_write: 0,
        }],
        &DriverPolicy {
            compaction: CompactionPolicy {
                small_file_bytes: 64 * 1024 * 1024,
                ..CompactionPolicy::default()
            },
            ..DriverPolicy::default()
        },
        &quiet(),
    );
    let report = execute_tick(&plan, dir.path(), 1, WriterConfig::default()).expect("ticking");
    apply(&mut live, &report, dir.path());

    assert!(
        live.len() < before,
        "no partitioned input left the live set: {} then {}",
        before,
        live.len()
    );
    for file in &live {
        assert!(
            file.name.contains('/'),
            "a file lost its partition on the way into the live set: {}",
            file.name
        );
    }
}

#[test]
fn a_compacted_file_declares_the_partition_it_was_written_into() {
    // `a_merged_file_lands_inside_the_partition_its_rows_belong_to` says in its own comment
    // that the add action "carries no partition value --- so an external reader sees a file
    // belonging to no partition at all", and then asserts only the output *path*. The half
    // it named and did not check was wrong for as long as it existed.
    //
    // A table whose `metaData` declares a partition column and whose files carry no value
    // for it is malformed. `delta_kernel` stops mid-scan. Spark reads the column as `NULL`
    // and **prunes the compacted file out of any query that filters on the partition**, so
    // the answer is short rather than refused --- and gets shorter the better maintenance
    // is working. `sankhya-publish` has supplied these values all along; compaction did not.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    let files = partitioned_table(root);

    // A log that declares the partition column, which is what makes an empty
    // `partitionValues` malformed rather than merely unhelpful.
    let mut metadata = sankhya_table_delta::Metadata::new("t", "{}", 0);
    metadata.partition_columns = vec!["sank_data_date".to_string()];
    sankhya_table_delta::commit(root, 0, &sankhya_table_delta::create(metadata))
        .expect("creating the table");
    let adds: Vec<_> = files
        .iter()
        .map(|f| {
            let mut add = sankhya_table_delta::AddFile::new(f.name.clone(), f.bytes, 0);
            add.partition_values
                .insert("sank_data_date".to_string(), "2026-08-28".to_string());
            sankhya_table_delta::Action::Add(add)
        })
        .collect();
    sankhya_table_delta::commit(root, 1, &adds).expect("publishing");

    let plan = plan_tick(
        &[PartitionState {
            table: "t".to_string(),
            partition: "sank_data_date=2026-08-28".to_string(),
            files,
            ticks_since_write: 0,
        }],
        &DriverPolicy {
            compaction: CompactionPolicy {
                small_file_bytes: 64 * 1024 * 1024,
                ..CompactionPolicy::default()
            },
            ..DriverPolicy::default()
        },
        &quiet(),
    );
    let report = execute_tick(&plan, root, 1, WriterConfig::default()).expect("ticking");
    assert!(!report.merged.is_empty(), "nothing was merged");
    commit_tick(root, 2, &report, 1).expect("committing the tick");

    // Read back what was committed, the way an external reader does.
    let actions = sankhya_table_delta::read_actions(root).expect("replaying the log");
    let compacted: Vec<_> = actions
        .iter()
        .filter_map(|(_version, action)| match action {
            sankhya_table_delta::Action::Add(add) if add.path.contains("compacted") => Some(add),
            _ => None,
        })
        .collect();
    assert!(!compacted.is_empty(), "the tick committed no compacted file");
    for add in compacted {
        assert_eq!(
            add.partition_values.get("sank_data_date").map(String::as_str),
            Some("2026-08-28"),
            "a compacted file declares no partition: {} carries {:?}",
            add.path,
            add.partition_values
        );
    }
}
