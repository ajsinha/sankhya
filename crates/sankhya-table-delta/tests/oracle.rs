//! The kernel reads what this crate writes.
//!
//! This is the test that makes the open-storage claim real rather than aspirational.
//! Writing the log by hand is only defensible if the result is actually valid Delta, and
//! "we implemented the spec carefully" is not evidence. So an independent implementation
//! reads the log and must agree about the table's schema, version and live files.
//!
//! The kernel is a dev-dependency for exactly this reason: it supplies a definition of
//! correctness, not an I/O layer.

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

use delta_kernel::object_store::local::LocalFileSystem;
use delta_kernel::scan::state::ScanFile;
use delta_kernel::Snapshot;
use delta_kernel_default_engine::DefaultEngineBuilder;
use sankhya_table_delta::{commit, create, live_files, Action, AddFile, Metadata, RemoveFile};
use std::sync::Arc;
use url::Url;

// # What this file cannot check, and where that is checked instead
//
// This crate *defines* the log, so the crates that *write* one --- `sankhya-publish` and
// `sankhya-maintenance` --- are above it in the dependency graph and cannot be reached from
// here. Everything below is therefore hand-assembled: the schema is the constant on the next
// line rather than anything `schema_string()` produced, the actions are built by the test,
// and the data files are zero bytes.
//
// So these tests prove that a log this crate can *describe* is one the kernel can parse. They
// cannot prove that the logs this workspace actually writes are readable, and for a while the
// README said they did. When an auditor pointed the kernel at a real table, compaction was
// committing `partitionValues: {}` inside a partition directory and the kernel stopped
// mid-scan --- a defect no test here could see, because no test here has a partitioned table,
// a real writer, or a row to read.
//
// `crates/sankhya-maintenance/tests/kernel_oracle.rs` is the test that can, and it lives
// there because that is where the writers are.
const SCHEMA: &str = r#"{"type":"struct","fields":[{"name":"id","type":"long","nullable":false,"metadata":{}},{"name":"amount","type":"long","nullable":false,"metadata":{}}]}"#;

/// A table root with the log written, and one empty Parquet-shaped file per add so the
/// kernel has something to resolve paths against.
fn table(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).expect("creating the table root");
}

fn touch(dir: &std::path::Path, name: &str) {
    std::fs::write(dir.join(name), b"").expect("touching a file");
}

fn kernel_files(root: &std::path::Path) -> (u64, Vec<String>) {
    let engine = Arc::new(DefaultEngineBuilder::new(Arc::new(LocalFileSystem::new())).build());
    let url = Url::from_directory_path(root).expect("a file url");
    let snapshot = Snapshot::builder_for(url.to_string())
        .build(engine.as_ref())
        .expect("the kernel must be able to build a snapshot of this log");

    let version = snapshot.version();
    let scan = snapshot.scan_builder().build().expect("building a scan");

    let mut paths = Vec::new();
    for result in scan.scan_metadata(engine.as_ref()).expect("scan metadata") {
        let metadata = result.expect("a scan metadata batch");
        paths = metadata
            .visit_scan_files(paths, |acc: &mut Vec<String>, file: ScanFile| {
                acc.push(file.path.clone());
            })
            .expect("visiting scan files");
    }
    paths.sort();
    (version, paths)
}

#[test]
fn the_kernel_reads_a_table_this_crate_created() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    table(root);
    touch(root, "part-0000.parquet");

    commit(
        root,
        0,
        &[
            create(Metadata::new("t1", SCHEMA, 0))
                .into_iter()
                .next()
                .expect("the protocol action"),
            Action::Metadata(Metadata::new("t1", SCHEMA, 0)),
            Action::Add(AddFile::new("part-0000.parquet", 10, 0)),
        ],
    )
    .expect("committing");

    let ours = live_files(root).expect("our reader");
    assert_eq!(ours.paths(), vec!["part-0000.parquet"]);
    assert_eq!(ours.version, Some(0));

    let (version, paths) = kernel_files(root);
    assert_eq!(version, 0);
    assert_eq!(paths, vec!["part-0000.parquet".to_string()]);
}

#[test]
fn the_kernel_agrees_about_a_compaction() {
    // The case that motivated the log. Four fragments are replaced by one file, and
    // both readers must see one file rather than five -- even though all five are still
    // on disk, as the add-only rule requires.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    table(root);

    let fragments: Vec<String> = (0..4).map(|i| format!("part-{i:04}.parquet")).collect();
    for name in &fragments {
        touch(root, name);
    }
    touch(root, "compacted-0000.parquet");

    let mut actions = create(Metadata::new("t2", SCHEMA, 0));
    actions.extend(
        fragments
            .iter()
            .map(|n| Action::Add(AddFile::new(n.clone(), 10, 0))),
    );
    commit(root, 0, &actions).expect("the creating commit");

    // The compaction: one add, four removes, in one atomic commit.
    let mut compaction = vec![Action::Add(AddFile::new("compacted-0000.parquet", 40, 1))];
    compaction.extend(
        fragments
            .iter()
            .map(|n| Action::Remove(RemoveFile::rewritten(n.clone(), 1))),
    );
    commit(root, 1, &compaction).expect("the compaction commit");

    let ours = live_files(root).expect("our reader");
    assert_eq!(ours.paths(), vec!["compacted-0000.parquet"]);
    assert_eq!(ours.version, Some(1));

    let (version, paths) = kernel_files(root);
    assert_eq!(version, 1);
    assert_eq!(
        paths,
        vec!["compacted-0000.parquet".to_string()],
        "the kernel saw the superseded fragments, which are still on disk by design"
    );

    // All five files really are still there.
    assert_eq!(
        std::fs::read_dir(root)
            .expect("listing")
            .filter(|e| {
                e.as_ref()
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".parquet")
            })
            .count(),
        5
    );
}

#[test]
fn a_compaction_removal_does_not_look_like_a_deletion() {
    // Compaction rewrites files without changing rows. A reader streaming changes from
    // this table must not see every compacted row as a delete followed by an insert --
    // a flood of spurious changes proportional to how well maintenance is working,
    // which would be a perverse thing to punish.
    let removal = RemoveFile::rewritten("part-0000.parquet", 1);
    assert!(!removal.data_change);

    let deletion = RemoveFile::deleted("part-0000.parquet", 1);
    assert!(deletion.data_change);
}

#[test]
fn the_kernel_accepts_the_statistics_this_crate_writes() {
    // The statistics field is an encoded JSON string inside a JSON object, which is an
    // easy shape to get subtly wrong -- and a reader that rejects the log over it
    // rejects the whole table, not just the statistic.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    table(root);
    touch(root, "part-0000.parquet");

    let mut actions = create(Metadata::new("t3", SCHEMA, 0));
    actions.push(Action::Add(AddFile::with_rows(
        "part-0000.parquet",
        10,
        0,
        1_234,
    )));
    commit(root, 0, &actions).expect("committing");

    let ours = live_files(root).expect("our reader");
    assert_eq!(ours.files[0].rows(), Some(1_234));

    let (version, paths) = kernel_files(root);
    assert_eq!(version, 0);
    assert_eq!(paths, vec!["part-0000.parquet".to_string()]);
}

#[test]
fn an_absent_row_count_reads_as_unknown_not_as_zero() {
    // Zero and unknown are different, and conflating them is how a compaction plan
    // claims to merge nothing and then merges everything.
    let add = AddFile::new("part-0000.parquet", 10, 0);
    assert_eq!(add.rows(), None);
}

#[test]
fn the_kernel_accepts_bounds_and_null_counts() {
    // Bounds were withheld from the log originally on the grounds that a wrong bound
    // silently drops rows. Now that they are written, they are read by another engine
    // rather than only by this one -- so a malformed statistics document costs *other
    // people* answers, which is a stronger reason to check than the original was to
    // abstain.
    use sankhya_stats::{Bound, ColumnStats};
    use std::collections::BTreeMap;

    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    table(root);
    touch(root, "part-0000.parquet");

    let mut columns: BTreeMap<String, ColumnStats> = BTreeMap::new();
    columns.insert(
        "id".to_string(),
        ColumnStats {
            rows: 100,
            nulls: 3,
            min: Some(Bound::Int(-5)),
            max: Some(Bound::Int(900)),
            ..ColumnStats::default()
        },
    );
    columns.insert(
        "amount".to_string(),
        ColumnStats {
            rows: 100,
            nulls: 0,
            min: Some(Bound::Float(0.5)),
            max: Some(Bound::Float(99.25)),
            ..ColumnStats::default()
        },
    );

    let statistics = sankhya_table_delta::from_column_stats(100, &columns);
    let mut actions = create(Metadata::new("t4", SCHEMA, 0));
    actions.push(Action::Add(AddFile::with_statistics(
        "part-0000.parquet",
        10,
        0,
        &statistics,
    )));
    commit(root, 0, &actions).expect("committing");

    // Our own reader.
    let ours = live_files(root).expect("our reader");
    let recovered =
        sankhya_table_delta::to_column_stats(&ours.files[0].statistics().expect("statistics"));
    assert_eq!(recovered["id"].min, Some(Bound::Int(-5)));
    assert_eq!(recovered["id"].nulls, 3);
    assert_eq!(recovered["amount"].max, Some(Bound::Float(99.25)));

    // And the kernel's.
    let (version, paths) = kernel_files(root);
    assert_eq!(version, 0);
    assert_eq!(paths, vec!["part-0000.parquet".to_string()]);
}

#[test]
fn a_bound_the_protocol_cannot_carry_is_omitted_rather_than_approximated() {
    // An infinity has no JSON number, and bytes that are not text have no JSON string.
    // Omitting the bound costs a scan; writing something close costs an answer, in
    // engines that cannot be fixed from here.
    use sankhya_stats::Bound;

    assert_eq!(
        sankhya_table_delta::encode_bound(&Bound::Float(f64::INFINITY)),
        None
    );
    assert_eq!(
        sankhya_table_delta::encode_bound(&Bound::Float(f64::NAN)),
        None
    );
    assert_eq!(
        sankhya_table_delta::encode_bound(&Bound::Bytes(vec![0xff, 0xfe])),
        None
    );

    assert!(sankhya_table_delta::encode_bound(&Bound::Int(7)).is_some());
    assert!(sankhya_table_delta::encode_bound(&Bound::Float(1.5)).is_some());
}

#[test]
fn the_kernel_reads_a_checkpoint_this_crate_wrote() {
    // A checkpoint is a Parquet file of nested structs, maps and lists, written by hand
    // against a protocol this crate does not own. Of everything here it is the most
    // likely to be subtly wrong, and the least likely for our own reader to notice --
    // because our own reader would be parsing exactly what our own writer produced.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    table(root);

    let metadata = Metadata::new("t5", SCHEMA, 0);
    let mut actions = create(metadata.clone());
    for i in 0..6 {
        let name = format!("part-{i:04}.parquet");
        touch(root, &name);
        actions.push(Action::Add(AddFile::with_rows(name, 100, 0, 50)));
    }
    commit(root, 0, &actions).expect("committing");

    let live = live_files(root).expect("our reader");
    assert_eq!(live.files.len(), 6);

    let report = sankhya_table_delta::write_checkpoint(root, &live, &metadata, 1, 2)
        .expect("writing a checkpoint");
    assert_eq!(report.version, 0);
    assert_eq!(report.actions, 8, "protocol, metadata, and six files");
    assert!(report.path.exists());

    // The kernel must resolve the same table through the checkpoint it now finds.
    let (version, mut paths) = kernel_files(root);
    assert_eq!(version, 0);
    let mut expected: Vec<String> = (0..6).map(|i| format!("part-{i:04}.parquet")).collect();
    expected.sort();
    paths.sort();
    assert_eq!(paths, expected);
}

#[test]
fn the_kernel_reads_commits_made_after_a_checkpoint() {
    // A checkpoint is a starting point, not an answer. A reader that stopped there would
    // serve a table frozen at the moment it was written.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    table(root);

    let metadata = Metadata::new("t6", SCHEMA, 0);
    let mut actions = create(metadata.clone());
    for i in 0..4 {
        let name = format!("part-{i:04}.parquet");
        touch(root, &name);
        actions.push(Action::Add(AddFile::with_rows(name, 100, 0, 50)));
    }
    commit(root, 0, &actions).expect("committing");

    let live = live_files(root).expect("reading");
    sankhya_table_delta::write_checkpoint(root, &live, &metadata, 1, 2).expect("checkpointing");

    // Two more commits: one adds, one compacts the originals away.
    touch(root, "part-0004.parquet");
    commit(
        root,
        1,
        &[Action::Add(AddFile::with_rows(
            "part-0004.parquet",
            100,
            0,
            50,
        ))],
    )
    .expect("committing");

    touch(root, "merged.parquet");
    let mut compaction = vec![Action::Add(AddFile::with_rows(
        "merged.parquet",
        400,
        1,
        200,
    ))];
    for i in 0..4 {
        compaction.push(Action::Remove(sankhya_table_delta::RemoveFile::rewritten(
            format!("part-{i:04}.parquet"),
            1,
        )));
    }
    commit(root, 2, &compaction).expect("committing");

    let ours = live_files(root).expect("our reader");
    let mut expected = ours
        .paths()
        .iter()
        .map(|p| (*p).to_string())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(expected, vec!["merged.parquet", "part-0004.parquet"]);

    let (version, mut paths) = kernel_files(root);
    assert_eq!(version, 2);
    paths.sort();
    assert_eq!(paths, expected);
}

#[test]
fn the_checkpoint_is_actually_used_and_not_merely_tolerated() {
    // Both checkpoint tests above pass whether the kernel reads the checkpoint or
    // ignores it and replays the log, because either route reaches the same answer.
    // That is exactly the shape of a test that proves nothing.
    //
    // So: write a checkpoint, then delete the commits it covers. The protocol permits
    // that after a checkpoint, and it leaves the checkpoint as the *only* record of
    // those files. A reader that resolves the table now has certainly read it.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    table(root);

    let metadata = Metadata::new("t7", SCHEMA, 0);
    let mut actions = create(metadata.clone());
    for i in 0..5 {
        let name = format!("part-{i:04}.parquet");
        touch(root, &name);
        actions.push(Action::Add(AddFile::with_rows(name, 100, 0, 50)));
    }
    commit(root, 0, &actions).expect("committing");

    touch(root, "part-0005.parquet");
    commit(
        root,
        1,
        &[Action::Add(AddFile::with_rows(
            "part-0005.parquet",
            100,
            0,
            50,
        ))],
    )
    .expect("committing");

    let live = live_files(root).expect("reading");
    assert_eq!(live.files.len(), 6);
    sankhya_table_delta::write_checkpoint(root, &live, &metadata, 1, 2).expect("checkpointing");

    // Remove the commits the checkpoint subsumes, keeping the one it names.
    std::fs::remove_file(root.join("_delta_log").join("00000000000000000000.json"))
        .expect("removing commit 0");

    // Without the checkpoint, version 1 alone names only one file. With it, six.
    let (version, paths) = kernel_files(root);
    assert_eq!(version, 1);
    assert_eq!(
        paths.len(),
        6,
        "the kernel resolved {} files, so it did not read the checkpoint: {paths:?}",
        paths.len()
    );
}
