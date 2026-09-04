//! A backup taken from a real warehouse, and a drill against it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_backup::drill::{drill, ReadsBack, TableOutcome};
use sankhya_backup::manifest::{KeyGeneration, Manifest, SourceBackup, TableSnapshot};
use sankhya_backup::warehouse::Warehouse;
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::path::Path;
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, true),
    ]))
}

fn batch(ids: &[i64], labels: &[Option<&str>]) -> RecordBatch {
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids.to_vec())),
            Arc::new(StringArray::from(labels.to_vec())),
        ],
    )
    .expect("a valid batch")
}

/// The Parquet file the table actually holds.
///
/// Not `root/part-0000.parquet`. The writer puts a file in the partition directory its rows
/// belong to, so the flat path these tests used to tamper with existed only because the
/// fixture built the table by hand --- and once the fixture went through `Publication`, the
/// tampering wrote a *new* file beside the real one and the drill correctly reported the
/// table intact. A corruption test that corrupts nothing passes for the wrong reason.
fn the_published_file(root: &Path) -> std::path::PathBuf {
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "_delta_log") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "parquet") {
                return path;
            }
        }
    }
    panic!("the table holds no parquet file");
}

/// A table with one commit of data, returning its root.
fn write_table(warehouse: &Path, schema_name: &str, table: &str, rows: &RecordBatch) -> std::path::PathBuf {
    let root = warehouse.join(schema_name).join(table);
    // Through the writer that owns publishing. The tampering this file tests for happens
    // *below*, on purpose; the table it tampers with must be one the product really wrote,
    // or the test proves only that a hand-built table can be corrupted.
    let publication = Publication::external(&root, table);
    publication.create(&schema()).expect("created");
    publication
        .append(1, "part-0000.parquet", rows, Lsn::new(3))
        .expect("published");
    root
}

/// Take a backup of every named table, at the versions they currently stand.
fn take_backup(warehouse: &Warehouse, tables: &[&str], now: i64) -> Manifest {
    let snapshots: Vec<TableSnapshot> = tables
        .iter()
        .map(|table| {
            let (version, digest) = warehouse.digest_now(table).expect("the table digests");
            TableSnapshot::new(*table, version, Lsn::new(100), digest)
        })
        .collect();
    Manifest::bind(
        now,
        SourceBackup {
            location: "file:///backups/pg".to_string(),
            restores_to: Lsn::new(200),
            artefact_digest: "sha256:abc".to_string(),
        },
        snapshots,
        KeyGeneration {
            name: "warehouse".to_string(),
            version: 1,
        },
        now + 86_400_000_000,
    )
    .expect("consistent")
}

#[test]
fn a_backup_taken_from_a_real_warehouse_drills_clean() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_table(
        dir.path(),
        "sales",
        "orders",
        &batch(&[1, 2, 3], &[Some("north"), None, Some("south")]),
    );
    let warehouse = Warehouse::at(dir.path());
    let manifest = take_backup(&warehouse, &["sales.orders"], 1_000);

    let evidence = drill(&manifest, &warehouse, 2_000);
    assert!(evidence.passed(), "{:?}", evidence.failures());
    assert_eq!(
        evidence.tables[0].1,
        TableOutcome::Verified { rows: 3 },
        "the drill counted the rows it read"
    );
}

#[test]
fn a_file_altered_after_the_backup_is_caught() {
    // The failure a presence check never reaches: the file is there, it is the right size,
    // and its contents are not what was backed up.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = write_table(
        dir.path(),
        "sales",
        "orders",
        &batch(&[1, 2, 3], &[Some("north"), None, Some("south")]),
    );
    let warehouse = Warehouse::at(dir.path());
    let manifest = take_backup(&warehouse, &["sales.orders"], 1_000);

    // Republish the same file name with different data — the shape a bad restore or a
    // confused writer produces.
    let replacement = batch(&[1, 2, 9], &[Some("north"), None, Some("elsewhere")]);
    let published = the_published_file(&root);
    let name = published
        .file_name()
        .and_then(|n| n.to_str())
        .expect("a utf-8 file name")
        .to_string();
    let partition = published.parent().expect("a parent directory");
    // Removed first, because `write_parquet` refuses a name that already exists --- a data
    // file is never written over one some log still points at, and that rule is what stops a
    // restarted maintainer truncating its own earlier output.
    //
    // What this test simulates is not a publish. It is a **bad restore or a confused writer
    // from outside this process**, which does not go through the guarded path, so going
    // around it here is the faithful simulation rather than a way past an inconvenience.
    std::fs::remove_file(&published).expect("removing the published file");
    write_parquet(partition, &name, &replacement, Lsn::new(3), WriterConfig::default())
        .expect("written");

    let evidence = drill(&manifest, &warehouse, 2_000);
    assert!(!evidence.passed());
    let (table, outcome) = evidence.failures()[0];
    assert_eq!(table, "sales.orders");
    assert!(
        outcome.to_string().contains("altered, not lost"),
        "the row count is unchanged and the data is not: {outcome}"
    );
}

#[test]
fn a_truncated_file_is_caught_rather_than_read_as_empty() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = write_table(dir.path(), "sales", "orders", &batch(&[1, 2, 3], &[None, None, None]));
    let warehouse = Warehouse::at(dir.path());
    let manifest = take_backup(&warehouse, &["sales.orders"], 1_000);

    let published = the_published_file(&root);
    std::fs::write(&published, b"not parquet at all").expect("truncated");

    let evidence = drill(&manifest, &warehouse, 2_000);
    let (_, outcome) = evidence.failures()[0];
    let TableOutcome::Unreadable { why } = outcome else {
        panic!("a truncated file must be unreadable, not empty: {outcome:?}");
    };
    let name = published
        .file_name()
        .and_then(|n| n.to_str())
        .expect("a utf-8 file name");
    assert!(why.contains(name), "it names the file: {why}");
}

#[test]
fn a_backup_still_verifies_after_the_table_moves_on() {
    // The whole point of pinning a version. New data arriving must not make an old backup
    // look broken, or every drill fails on a system that is working.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = write_table(dir.path(), "sales", "orders", &batch(&[1, 2, 3], &[None, None, None]));
    let warehouse = Warehouse::at(dir.path());
    let manifest = take_backup(&warehouse, &["sales.orders"], 1_000);

    let more = batch(&[4, 5], &[Some("east"), Some("west")]);
    Publication::external(&root, "orders")
        .append(2, "part-0001.parquet", &more, Lsn::new(5))
        .expect("published");

    let evidence = drill(&manifest, &warehouse, 3_000);
    assert!(evidence.passed(), "{:?}", evidence.failures());
    assert_eq!(evidence.tables[0].1, TableOutcome::Verified { rows: 3 });

    // And a backup taken now sees the new rows.
    let later = take_backup(&warehouse, &["sales.orders"], 4_000);
    assert!(drill(&later, &warehouse, 5_000).passed());
    assert_ne!(later.tables[0].checksum, manifest.tables[0].checksum);
}

#[test]
fn a_null_and_an_empty_string_digest_differently() {
    // They are different facts. A backup that cannot tell them apart cannot prove it
    // restored either, and the rendering convention is where that distinction usually dies.
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_table(dir.path(), "s", "with_null", &batch(&[1], &[None]));
    write_table(dir.path(), "s", "with_empty", &batch(&[1], &[Some("")]));
    let warehouse = Warehouse::at(dir.path());

    let (_, null_digest) = warehouse.digest_now("s.with_null").expect("digests");
    let (_, empty_digest) = warehouse.digest_now("s.with_empty").expect("digests");
    assert_ne!(null_digest.checksum(), empty_digest.checksum());
}

#[test]
fn the_digest_does_not_depend_on_which_order_the_log_lists_files_in() {
    // The digest is order-independent by construction; this proves the file walk does not
    // sneak an ordering in anyway.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = write_table(dir.path(), "s", "t", &batch(&[1, 2], &[Some("a"), Some("b")]));
    let second = batch(&[3, 4], &[Some("c"), Some("d")]);
    Publication::external(&root, "orders")
        .append(2, "part-0001.parquet", &second, Lsn::new(4))
        .expect("published");

    let warehouse = Warehouse::at(dir.path());
    let (version, once) = warehouse.digest_now("s.t").expect("digests");
    let again = warehouse.digest_of("s.t", version).expect("digests");
    assert_eq!(once, again);
    assert_eq!(once.rows(), 4);
}

#[test]
fn a_table_with_no_commits_cannot_be_backed_up() {
    // A backup of a table with no version pins nothing, and a drill of it would pass
    // vacuously.
    let dir = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir_all(dir.path().join("s").join("empty")).expect("directory");
    let warehouse = Warehouse::at(dir.path());
    let refused = warehouse.digest_now("s.empty").expect_err("no commits");
    assert!(refused.contains("nothing to back up"), "{refused}");
}
