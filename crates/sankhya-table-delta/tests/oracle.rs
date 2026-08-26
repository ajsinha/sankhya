//! The kernel reads what this crate writes.
//!
//! This is the test that makes the open-storage claim real rather than aspirational.
//! Writing the log by hand is only defensible if the result is actually valid Delta, and
//! "we implemented the spec carefully" is not evidence. So an independent implementation
//! reads the log and must agree about the table's schema, version and live files.
//!
//! The kernel is a dev-dependency for exactly this reason: it supplies a definition of
//! correctness, not an I/O layer.

use delta_kernel::object_store::local::LocalFileSystem;
use delta_kernel::scan::state::ScanFile;
use delta_kernel::Snapshot;
use delta_kernel_default_engine::DefaultEngineBuilder;
use sankhya_table_delta::{commit, create, live_files, Action, AddFile, Metadata, RemoveFile};
use std::sync::Arc;
use url::Url;

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
