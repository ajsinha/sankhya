//! Reading a clone: the origin's live set at the cloned version, then the clone's own log.
//!
//! # The failure these exist to close
//!
//! `ADR-0016`'s Decision 1a says a clone's log names none of its origin's files. That decision
//! is right — it keeps a warehouse portable and makes the reclamation question answerable — and
//! its unstated cost was this: **until the read path splices, a clone reads as empty.** Present,
//! readable, and containing nothing, which is precisely the shape the backup check refuses and
//! the live behaviour permitted.
//!
//! So the first test here is the one that would have failed before any of this existed.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_publish::Publication;
use sankhya_readpath::{resolve_as_of, resolve_cached, resolve_clone_cached, Inherited};
use sankhya_table_delta::LogCache;
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

const ROWS: u64 = 100;

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]))
}

fn rows(from: u64, to: u64) -> RecordBatch {
    let ids: Vec<i64> = (from..to).map(|i| i64::try_from(i).unwrap_or(0)).collect();
    RecordBatch::try_new(schema(), vec![Arc::new(Int64Array::from(ids))]).expect("a batch")
}

/// An origin of `files` published fragments, written through the shipping write path.
fn origin(root: &std::path::Path, files: u64) -> Publication {
    let publication = Publication::external(root.join("entries"), "entries");
    publication.create(&schema()).expect("creating");
    for file in 0..files {
        let from = file * ROWS;
        publication
            .append(
                file + 1,
                &format!("part-{file:04}.parquet"),
                &rows(from, from + ROWS),
                Lsn::new(from + ROWS),
            )
            .expect("publishing");
    }
    publication
}

/// A clone's own table: a log with lineage and no files of its own.
fn clone_of(root: &std::path::Path, name: &str, origin_version: u64) -> Publication {
    let publication = Publication::external(root.join(name), name);
    let lineage = sankhya_clone::Lineage::new("entries", origin_version, 0);
    publication
        .create_clone(
            &sankhya_table_delta::schema_string(&schema()).expect("a schema string"),
            &lineage.to_properties(),
        )
        .expect("creating the clone");
    publication
}

fn inherited(root: &std::path::Path, version: u64) -> Inherited {
    Inherited { origin_root: root.join("entries"), version }
}

fn rows_readable(root: &std::path::Path, name: &str, at: Option<&Inherited>) -> usize {
    let cache = LogCache::new();
    let coverage = Some(LsnRange::up_to(Lsn::new(u64::MAX)));
    let table = match at {
        Some(inherited) => resolve_clone_cached(
            schema(),
            &root.join(name),
            inherited,
            coverage,
            Lsn::new(u64::MAX),
            &cache,
        ),
        None => resolve_cached(
            schema(),
            &root.join(name),
            coverage,
            None,
            Lsn::new(u64::MAX),
            &cache,
        ),
    }
    .expect("the table resolves");
    // Every planned file must be openable where the plan says it is.
    //
    // A row count comes from the log, so a file resolved against the wrong root still reports
    // the right number --- a mutation that resolved inherited files against the clone's own root
    // survived a version of this file that only counted rows. The path check is what makes the
    // count mean the rows are reachable.
    for file in table.published_files() {
        assert!(
            std::path::Path::new(&file.path).exists(),
            "the plan resolved `{}`, which is not there",
            file.path
        );
    }

    usize::try_from(table.declared_rows()).unwrap_or(usize::MAX)
}

#[test]
fn a_clone_read_without_the_splice_is_empty_and_with_it_is_not() {
    // Both halves in one test, because the second means nothing without the first. A clone
    // resolved as an ordinary table is a table of no files — that is not a bug in the log, it is
    // Decision 1a working — and it is why the read path had to learn to splice.
    let dir = tempfile::tempdir().expect("a directory");
    origin(dir.path(), 4);
    clone_of(dir.path(), "staging", 4);

    assert_eq!(
        rows_readable(dir.path(), "staging", None),
        0,
        "a clone's own log names nothing, which is the decision rather than a defect"
    );
    assert_eq!(
        rows_readable(dir.path(), "staging", Some(&inherited(dir.path(), 4))),
        400,
        "and spliced, it reads every row the origin had at the cloned version"
    );
}

#[test]
fn a_clone_reads_the_version_it_was_taken_at_and_not_the_origin_as_it_stands() {
    // The property that makes a clone a clone. The origin has moved on; the clone has not.
    let dir = tempfile::tempdir().expect("a directory");
    let publication = origin(dir.path(), 2);
    clone_of(dir.path(), "staging", 2);

    // The origin gains two more files after the clone was taken.
    for file in 2..4u64 {
        let from = file * ROWS;
        publication
            .append(
                file + 1,
                &format!("part-{file:04}.parquet"),
                &rows(from, from + ROWS),
                Lsn::new(from + ROWS),
            )
            .expect("publishing");
    }

    assert_eq!(rows_readable(dir.path(), "entries", None), 400, "the origin moved on");
    assert_eq!(
        rows_readable(dir.path(), "staging", Some(&inherited(dir.path(), 2))),
        200,
        "and the clone still reads what it was taken at"
    );
}

#[test]
fn a_clones_own_writes_are_read_alongside_what_it_inherited() {
    // Divergence, which is the whole point of a clone: each side commits to its own log and
    // neither observes the other.
    let dir = tempfile::tempdir().expect("a directory");
    origin(dir.path(), 3);
    let clone = clone_of(dir.path(), "staging", 3);

    clone
        .append(1, "part-9000.parquet", &rows(9_000, 9_100), Lsn::new(9_100))
        .expect("the clone writes for itself");

    assert_eq!(
        rows_readable(dir.path(), "staging", Some(&inherited(dir.path(), 3))),
        400,
        "three inherited (300 rows) and one of its own (100)"
    );
    assert_eq!(
        rows_readable(dir.path(), "entries", None),
        300,
        "and the origin does not see the clone's write"
    );
}

#[test]
fn an_empty_origin_version_gives_a_clone_of_nothing_rather_than_a_failure() {
    // Version zero is the creating commit: metadata, no files. A clone taken there is empty and
    // legitimately so, and it must be distinguishable from a clone that failed to resolve.
    let dir = tempfile::tempdir().expect("a directory");
    origin(dir.path(), 3);
    clone_of(dir.path(), "staging", 0);

    assert_eq!(rows_readable(dir.path(), "staging", Some(&inherited(dir.path(), 0))), 0);
}

#[test]
fn a_clone_whose_origin_is_gone_reports_rather_than_serving_what_remains() {
    // The clone's own log still resolves, so a splice that ignored the failure would answer with
    // the clone's own writes and nothing else — a short answer that looks like a whole one.
    let dir = tempfile::tempdir().expect("a directory");
    origin(dir.path(), 2);
    let clone = clone_of(dir.path(), "staging", 2);
    clone
        .append(1, "part-9000.parquet", &rows(9_000, 9_100), Lsn::new(9_100))
        .expect("its own write");

    std::fs::remove_dir_all(dir.path().join("entries")).expect("the origin goes away");

    let cache = LogCache::new();
    let outcome = resolve_clone_cached(
        schema(),
        &dir.path().join("staging"),
        &inherited(dir.path(), 2),
        Some(LsnRange::up_to(Lsn::new(u64::MAX))),
        Lsn::new(u64::MAX),
        &cache,
    );
    assert!(
        outcome.is_err(),
        "a clone whose origin has gone must say so, not serve the rows it happens to still have"
    );
}

#[test]
fn a_table_resolved_at_a_version_reads_that_version_and_not_the_present() {
    // What reading as of a named snapshot needs (`ADR-0019`): a snapshot records a version per
    // table, and answering as of one means resolving each table at the version it recorded --
    // so four tables read at four moments become four tables read at one.
    //
    // Not the clone path. That resolves *two* logs, so using it here would add the table's
    // current files to its historical ones and answer with both: a superset presented as a
    // snapshot, which only reveals itself once the table has moved.
    let dir = tempfile::tempdir().expect("a directory");
    origin(dir.path(), 3);
    let root = dir.path().join("entries");

    let at_one = resolve_as_of(
        schema(),
        &root,
        1,
        LsnRange::new(Lsn::new(0), Lsn::new(u64::MAX)),
        Lsn::new(u64::MAX),
    )
    .expect("version one resolves");
    let now = resolve_cached(
        schema(),
        &root,
        LsnRange::new(Lsn::new(0), Lsn::new(u64::MAX)),
        None,
        Lsn::new(u64::MAX),
        &LogCache::new(),
    )
    .expect("the present resolves");

    assert!(
        at_one.published_files().len() < now.published_files().len(),
        "version one saw as much as the present: {} against {}",
        at_one.published_files().len(),
        now.published_files().len()
    );
    assert_eq!(at_one.published_files().len(), 1, "one commit, one file");
}
