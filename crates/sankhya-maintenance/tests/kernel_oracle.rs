//! What an independent implementation of the Delta protocol makes of the tables this
//! workspace actually writes.
//!
//! # Why this exists when `sankhya-table-delta/tests/oracle.rs` already used the kernel
//!
//! Because that one could not reach the writers. It lives in the crate that *defines* the
//! log, so the crates that *produce* logs --- `sankhya-publish` and `sankhya-maintenance` ---
//! are below it in the dependency graph and unavailable to it. What it tested was therefore
//! hand-assembled: a schema string written out as a constant, actions built by the test, and
//! `touch`ed zero-byte files standing in for Parquet.
//!
//! Four things followed, and a production-readiness audit found all four at once:
//!
//! - `schema_string()` --- the entire Arrow-to-protocol type mapping --- was never called.
//! - Every data file was empty, so the kernel never read a row and no test could compare the
//!   schema against the data.
//! - **No test used a partitioned table.**
//! - Neither production writer was under test.
//!
//! So when an auditor finally pointed `delta_kernel` at a table this server had written and
//! maintained, what broke was precisely the set of things nothing had ever looked at:
//! compaction was committing `partitionValues: {}` for files it had just written *inside* a
//! partition directory, on a table whose `metaData` declares that column. Strict readers stop
//! mid-scan. Spark reads the column as `NULL` and prunes the file out of every query that
//! filters on the partition --- a short answer rather than an error, growing worse the better
//! maintenance works.
//!
//! This test is the one that would have caught it: a real table, written by the real writer,
//! compacted by the real driver, and read --- rows and all --- by somebody else's code.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![allow(clippy::print_stdout)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use delta_kernel::engine::arrow_data::ArrowEngineData;
use delta_kernel::object_store::local::LocalFileSystem;
use delta_kernel::scan::state::ScanFile;
use delta_kernel::Snapshot;
use delta_kernel_default_engine::DefaultEngineBuilder;
use sankhya_maintenance::{
    commit_tick, execute_tick, next_compaction_sequence, plan_tick, CompactionPolicy,
    DriverPolicy, FileStat, PartitionState, SystemState,
};
use sankhya_publish::Publication;
use sankhya_table_delta::live_files;
use sankhya_table::WriterConfig;
use sankhya_types::Lsn;
use std::sync::Arc;
use url::Url;

fn quiet() -> SystemState {
    SystemState {
        in_maintenance_window: true,
        queries_running: 0,
        duty_cycle_ticks_remaining: 10_000,
    }
}

/// A table of the shape a real one has: several types, and a date column to partition on.
///
/// Types chosen so the mapping is exercised rather than asserted --- `long` alone would pass
/// against a constant, which is what the old oracle did.
fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, false),
    ]))
}

fn rows(from: i64, count: i64) -> RecordBatch {
    let ids: Vec<i64> = (from..from + count).collect();
    let regions: Vec<Option<&str>> = ids
        .iter()
        .map(|i| if i % 2 == 0 { Some("north") } else { Some("south") })
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let amounts: Vec<f64> = ids.iter().map(|i| *i as f64 * 1.5).collect();
    // No date column: the publisher stamps `sank_data_date` itself, which is the whole of
    // the date axis and the reason these tables are partitioned at all.
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(regions)),
            Arc::new(Float64Array::from(amounts)),
        ],
    )
    .expect("a valid batch")
}

/// Everything the kernel can tell us about a table: version, files, and the **rows**.
///
/// Reading the rows is the part the old oracle could not do. Listing scan files proves the
/// log parses; reading them proves the log describes the data that is actually there.
fn kernel_reads(root: &std::path::Path) -> (u64, Vec<String>, usize) {
    let engine = Arc::new(DefaultEngineBuilder::new(Arc::new(LocalFileSystem::new())).build());
    let url = Url::from_directory_path(root).expect("a file url");
    let snapshot = Snapshot::builder_for(url.to_string())
        .build(engine.as_ref())
        .expect("the kernel must be able to build a snapshot of this log");
    let version = snapshot.version();

    let scan = snapshot.scan_builder().build().expect("building a scan");
    let mut paths: Vec<String> = Vec::new();
    for result in scan.scan_metadata(engine.as_ref()).expect("scan metadata") {
        let metadata = result.expect("a scan metadata batch");
        paths = metadata
            .visit_scan_files(paths, |acc: &mut Vec<String>, file: ScanFile| {
                acc.push(file.path.clone());
            })
            .expect("visiting scan files");
    }
    paths.sort();

    // The rows themselves. A kernel that cannot resolve a file's partition values fails
    // here, which is the point: `partitionValues: {}` on a partitioned table is not a
    // cosmetic defect, it stops the scan.
    let mut read = 0usize;
    for batch in scan.execute(engine.clone()).expect("executing the scan") {
        let data = batch.expect("a scan result batch");
        let records: RecordBatch = data
            .into_any()
            .downcast::<ArrowEngineData>()
            .expect("arrow engine data")
            .into();
        read += records.num_rows();
    }
    (version, paths, read)
}

#[test]
fn the_kernel_reads_a_partitioned_table_this_workspace_wrote_and_compacted() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();

    // The real writer, partitioning on the real date axis.
    let publication = Publication::external(root, "orders");
    publication.create(&schema()).expect("creating the table");

    let mut files = Vec::new();
    let mut partition = String::new();
    for file in 0..4u64 {
        let published = publication
            .append(
                file + 1,
                &format!("part-{file:04}.parquet"),
                &rows(i64::from(u32::try_from(file).expect("small")) * 250, 250),
                Lsn::new(file + 1),
            )
            .expect("publishing");
        for one in published {
            if partition.is_empty() {
                partition = one
                    .file
                    .split('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
            }
            files.push(FileStat {
                name: one.file.clone(),
                bytes: one.bytes,
                rows: u64::try_from(one.rows).expect("small"),
                covers_through: Lsn::new(file + 1),
            });
        }
    }

    // Before compaction: the kernel must already agree, or nothing after this means anything.
    let (version, paths, read) = kernel_reads(root);
    assert_eq!(version, 4, "the kernel disagrees about the version");
    assert_eq!(paths.len(), 4, "the kernel sees {paths:?}");
    assert_eq!(read, 1000, "the kernel read {read} rows of a thousand");
    assert!(
        paths.iter().all(|p| p.contains("sank_data_date=")),
        "the writer did not partition: {paths:?}"
    );

    // The real driver, doing what it does on a tick.
    let plan = plan_tick(
        &[PartitionState {
            table: "orders".to_string(),
            partition: partition.clone(),
            files,
            ticks_since_write: 100,
        }],
        &DriverPolicy {
            compaction: CompactionPolicy {
                small_file_bytes: 64 * 1024 * 1024,
                routine_file_count: 2,
                ..CompactionPolicy::default()
            },
            ..DriverPolicy::default()
        },
        &quiet(),
    );
    assert!(!plan.run.is_empty(), "the driver planned no compaction");
    let report = execute_tick(&plan, root, 5, WriterConfig::default()).expect("ticking");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(!report.merged.is_empty(), "nothing was merged");
    commit_tick(root, 5, &report, 1).expect("committing the tick");

    // After compaction: the same thousand rows, still readable by somebody else's code.
    //
    // This is the assertion that was missing. Compaction wrote `partitionValues: {}` for a
    // file inside `sank_data_date=2026-08-28/`, and every existing test looked only at paths
    // and counts --- which are unaffected. A reader that resolves partitions is not.
    let (version, paths, read) = kernel_reads(root);
    assert_eq!(version, 5, "the tick is not one version");
    assert!(
        paths.iter().any(|p| p.contains("compacted")),
        "the compacted file is not in the kernel's scan: {paths:?}"
    );
    assert_eq!(
        read, 1000,
        "the kernel read {read} rows after compaction, not the thousand that are there --- \
         a file whose partition values are missing is pruned away rather than refused"
    );
}

/// A removal says when it happened, in the units the protocol defines.
///
/// # Why this is worth a test of its own
///
/// `deletionTimestamp` was a tick counter. A removal recorded `deletionTimestamp: 3` --- three
/// milliseconds after 1970 --- so **every superseded file was instantly older than any
/// retention interval**. A conformant `VACUUM RETAIN 168 HOURS` run by any other engine would
/// have deleted all of them at once, out from under SANKHYA readers holding leases, and
/// `DRY RUN` would have reported them safe first.
///
/// The two retention mechanisms could not see each other because one of them was reading a
/// counter as a clock. Nothing caught it because nothing looked at the value: this system's
/// own retention parses the *path*, not the timestamp.
///
/// Asserted as a range rather than an equality: the property is that it is a real wall-clock
/// millisecond, and any test that pins it exactly would pin the clock instead.
#[test]
fn a_removal_carries_a_wall_clock_millisecond_not_a_tick() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();

    let publication = Publication::external(root, "orders");
    publication.create(&schema()).expect("creating the table");
    let mut files = Vec::new();
    let mut partition = String::new();
    for file in 0..4u64 {
        for one in publication
            .append(
                file + 1,
                &format!("part-{file:04}.parquet"),
                &rows(i64::from(u32::try_from(file).expect("small")) * 100, 100),
                Lsn::new(file + 1),
            )
            .expect("publishing")
        {
            if partition.is_empty() {
                partition = one.file.split('/').next().unwrap_or_default().to_string();
            }
            files.push(FileStat {
                name: one.file.clone(),
                bytes: one.bytes,
                rows: u64::try_from(one.rows).expect("small"),
                covers_through: Lsn::new(file + 1),
            });
        }
    }

    let plan = plan_tick(
        &[PartitionState {
            table: "orders".to_string(),
            partition,
            files,
            ticks_since_write: 100,
        }],
        &DriverPolicy {
            compaction: CompactionPolicy {
                small_file_bytes: 64 * 1024 * 1024,
                routine_file_count: 2,
                ..CompactionPolicy::default()
            },
            ..DriverPolicy::default()
        },
        &quiet(),
    );
    let report = execute_tick(&plan, root, 5, WriterConfig::default()).expect("ticking");
    assert!(!report.merged.is_empty(), "nothing was merged");

    // The service's own clock, not a tick, is what reaches the log.
    let now_millis = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_millis(),
    )
    .expect("fits");
    commit_tick(root, 5, &report, now_millis).expect("committing the tick");

    // 2020-01-01 and 2100-01-01 in epoch milliseconds. A tick counter fails the lower bound
    // by fifty years; microseconds fail the upper one by fifty thousand.
    const YEAR_2020: i64 = 1_577_836_800_000;
    const YEAR_2100: i64 = 4_102_444_800_000;

    let actions = sankhya_table_delta::read_actions(root).expect("replaying the log");
    let removals: Vec<i64> = actions
        .iter()
        .filter_map(|(_version, action)| match action {
            sankhya_table_delta::Action::Remove(remove) => Some(remove.deletion_timestamp),
            _ => None,
        })
        .collect();
    assert!(!removals.is_empty(), "the tick removed nothing");
    for stamp in removals {
        assert!(
            (YEAR_2020..YEAR_2100).contains(&stamp),
            "a removal is dated {stamp}, which is not a plausible epoch millisecond --- a \
             tick counter reads as 1970 and a microsecond clock reads as the year 57000, and \
             an external VACUUM believes whichever it is told"
        );
    }
}

/// A restart does not reissue a compaction output name.
///
/// # The failure this closes
///
/// The output sequence was the maintainer's own tick counter, which starts at zero every time
/// the server starts. After a restart, tick 1 recomputed a name tick 1 had already used ---
/// and the planner selects any live file under the target size, so the previous output could
/// be chosen as **its own input**. The writer was a truncating `File::create`, so it was
/// emptied in place, and the commit that followed added and removed the same path, taking the
/// merged partition out of the live set altogether.
///
/// # Why this tests the sequence and not a whole restart
///
/// The first version of this test drove two `Maintainer`s across a simulated restart, and it
/// was **vacuous**: the service passes `ticks_since_write: 0`, so routine compaction never
/// fires on a freshly-written partition and neither maintainer merged anything. It asserted
/// that nothing was lost by a process that did nothing.
///
/// The decision is what matters, so the decision is what is tested. A sequence recovered from
/// the log is higher than every sequence the log already contains; a counter in memory is not.
#[test]
fn a_compaction_sequence_is_recovered_from_the_log_not_counted_in_memory() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();

    let publication = Publication::external(root, "orders");
    publication.create(&schema()).expect("creating the table");
    publication
        .append(1, "part-0000.parquet", &rows(0, 50), Lsn::new(1))
        .expect("publishing");

    // Nothing compacted yet: the first sequence must still not be zero-colliding, and it is
    // whatever one past "no compactions" is.
    let first = next_compaction_sequence(root);

    // A compaction lands, named with that sequence, exactly as the driver names it.
    let files = live_files(root).expect("reading").files;
    let partition = files
        .first()
        .and_then(|f| f.path.split('/').next())
        .unwrap_or_default()
        .to_string();
    let merged = if partition.is_empty() {
        format!("compacted-{first:06}-0000.parquet")
    } else {
        format!("{partition}/compacted-{first:06}-0000.parquet")
    };
    let mut add = sankhya_table_delta::AddFile::new(merged.clone(), 100, 0);
    add.partition_values = sankhya_table_delta::partition_values_from(&merged);
    sankhya_table_delta::commit(root, 2, &[sankhya_table_delta::Action::Add(add)])
        .expect("committing a compaction");

    // A restart is exactly this: ask again, with no memory of the first answer.
    let second = next_compaction_sequence(root);
    assert!(
        second > first,
        "the sequence went from {first} to {second}. A restart would reissue a name the log \
         already holds --- and the planner can select that file as its own input, truncate \
         it, and commit an add and a remove of the same path"
    );
    assert!(
        !merged.contains(&format!("compacted-{second:06}")),
        "the next sequence names the file that already exists"
    );
}
