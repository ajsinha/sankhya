//! Publishing a table, and checking one written by something else.
//!
//! Every test here corresponds to a failure that is **silent**. That is the whole reason
//! the library exists rather than a page of instructions: none of these mistakes announces
//! itself, and several are only visible to an implementation that did not make them.

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
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use sankhya_publish::class::{TableClass, CLASS_KEY};
use sankhya_publish::publish::{publish_table, Publication, PublishError};
use sankhya_publish::verify::{verify, Finding};
use std::sync::Arc;

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, true),
    ]))
}

fn batch(from: i64, rows: i64) -> RecordBatch {
    let ids: Vec<i64> = (from..from + rows).collect();
    let labels: Vec<Option<String>> = ids
        .iter()
        .map(|i| (i % 3 != 0).then(|| format!("row-{i}")))
        .collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(labels)),
        ],
    )
    .expect("a valid batch")
}

fn published() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("orders");
    let publication = Publication::external(&root, "orders");
    publish_table(&publication, &schema(), &[batch(0, 100), batch(100, 100)])
        .expect("publishing a well-formed table");
    (dir, root)
}

// --- what the library guarantees ------------------------------------------

#[test]
fn a_published_table_verifies_clean() {
    let (_dir, root) = published();
    let report = verify(&root);

    assert!(report.is_clean(), "{:?}", report.findings);
    assert_eq!(report.files, 2);
    assert_eq!(
        report.files_with_statistics, 2,
        "every file gets statistics; there is no option to skip them"
    );
    assert!(report.summary().contains("nothing to report"));
}

#[test]
fn every_published_file_carries_statistics() {
    // A file with no statistics cannot be pruned, so every query reads it. The answers stay
    // correct and the table gets slower and slower, with nothing anywhere saying why.
    let (_dir, root) = published();
    let live = sankhya_table_delta::live_files(&root).expect("readable");

    for file in &live.files {
        let stats = file
            .stats
            .as_ref()
            .unwrap_or_else(|| panic!("{} was published without statistics", file.path));
        assert!(stats.contains("numRecords"), "{stats}");
        assert!(
            stats.contains("minValues") || stats.contains("maxValues"),
            "bounds are what make pruning possible: {stats}"
        );
    }
}

#[test]
fn the_add_action_carries_the_field_this_system_once_omitted() {
    // The specific defect that motivated the library. `partitionValues` is non-nullable;
    // omitting it produced a log this system's own reader accepted happily — because a
    // reader ignores a field it never writes — and that an independent implementation
    // rejected on the first read.
    let (_dir, root) = published();
    let log = std::fs::read_to_string(root.join("_delta_log/00000000000000000001.json"))
        .expect("the commit exists");

    assert!(
        log.contains("partitionValues"),
        "the field an independent reader requires is absent again: {log}"
    );
}

#[test]
fn a_table_declares_its_class_in_its_own_log() {
    // In the log rather than in any server's configuration, so two nodes reading one
    // warehouse cannot disagree and a restart cannot forget.
    let (_dir, root) = published();
    let log = std::fs::read_to_string(root.join("_delta_log/00000000000000000000.json"))
        .expect("the creation commit exists");

    assert!(log.contains(CLASS_KEY), "{log}");
    assert!(log.contains("external"));
    assert_eq!(verify(&root).class, TableClass::External);
}

#[test]
fn a_mutable_table_declares_the_columns_that_identify_a_row() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("accounts");
    let publication = Publication::external(&root, "accounts").keyed_by(["id"]);
    publish_table(&publication, &schema(), &[batch(0, 10)]).expect("publishing");

    let report = verify(&root);
    assert_eq!(report.key_columns, vec!["id".to_string()]);
    assert!(report.is_clean());
}

// --- what the library refuses ---------------------------------------------

#[test]
fn a_type_that_does_not_round_trip_is_refused_rather_than_approximated() {
    // Publishing something merely similar produces a table other engines read confidently
    // and wrongly.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let unsupported = Schema::new(vec![Field::new(
        "when",
        DataType::Timestamp(TimeUnit::Nanosecond, None),
        true,
    )]);
    let publication = Publication::external(dir.path().join("t"), "t");

    let Err(error) = publication.create(&unsupported) else {
        panic!("a nanosecond timestamp cannot round-trip and must be refused");
    };
    assert!(matches!(error, PublishError::UnrepresentableSchema { .. }));
    assert!(error.to_string().contains("merely similar"));
}

#[test]
fn a_key_column_that_is_not_in_the_schema_is_refused_at_creation() {
    // A key naming a column that does not exist makes every merge return nothing for that
    // key — silently, and far from the typo that caused it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let publication = Publication::external(dir.path().join("t"), "t").keyed_by(["idd"]);

    let Err(error) = publication.create(&schema()) else {
        panic!("a key column that is not in the schema must be refused");
    };
    let PublishError::NoSuchKeyColumn { column, available } = &error else {
        panic!("expected a missing key column");
    };
    assert_eq!(column, "idd");
    assert!(
        available.contains(&"id".to_string()),
        "the message says what is there"
    );
}

#[test]
fn a_key_column_containing_a_comma_is_refused() {
    // The key list is comma-separated in the log, so this would split one column into two
    // on the way back.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let publication = Publication::external(dir.path().join("t"), "t").keyed_by(["a,b"]);
    assert!(matches!(
        publication.create(&schema()),
        Err(PublishError::CommaInKeyColumn { .. })
    ));
}

#[test]
fn a_file_name_that_would_escape_the_table_is_refused() {
    // The name becomes a path relative to the table root. A separator writes outside the
    // table; `..` writes outside the warehouse.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("t");
    let publication = Publication::external(&root, "t");
    publication.create(&schema()).expect("creating");

    for name in ["../escape.parquet", "sub/dir.parquet", "..", "a/../../b"] {
        assert!(
            matches!(
                publication.append(1, name, &batch(0, 1), sankhya_types::Lsn::new(1)),
                Err(PublishError::UnsafeFileName { .. })
            ),
            "'{name}' must be refused"
        );
    }
}

#[test]
fn a_batch_whose_schema_differs_from_the_table_is_refused() {
    // Publishing it produces files a reader cannot combine, and the failure appears at
    // query time rather than here.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let other = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
    let wrong = RecordBatch::try_new(
        Arc::clone(&other),
        vec![Arc::new(Int64Array::from(vec![1i64]))],
    )
    .expect("valid");

    let publication = Publication::external(dir.path().join("t"), "t");
    assert!(matches!(
        publish_table(&publication, &schema(), &[batch(0, 1), wrong]),
        Err(PublishError::SchemaMismatch { batch: 1 })
    ));
}

// --- verifying a table written by something else --------------------------

#[test]
fn a_file_published_without_statistics_is_reported_as_slow_not_wrong() {
    // The distinction operators need most. A table that is merely slow can wait until
    // Monday; one that returns wrong answers cannot.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("hand-written");
    std::fs::create_dir_all(&root).expect("creating");

    let json = sankhya_table_delta::schema_string(&schema()).expect("representable");
    sankhya_table_delta::commit(
        &root,
        0,
        &sankhya_table_delta::create(sankhya_table_delta::Metadata::new("t", json, 0)),
    )
    .expect("creating");
    sankhya_table_delta::commit(
        &root,
        1,
        &[sankhya_table_delta::Action::Add(
            // `AddFile::new` carries no statistics — which is exactly what a hand-written
            // publisher produces, and exactly what this exists to notice.
            sankhya_table_delta::AddFile::new("part-0000.parquet", 1_024, 0),
        )],
    )
    .expect("adding");

    let report = verify(&root);
    assert!(!report.is_clean());
    assert!(
        !report.has_correctness_findings(),
        "missing statistics makes queries slow, not wrong"
    );
    let finding = report.findings.first().expect("one finding");
    assert!(matches!(finding, Finding::NoStatistics { .. }));
    assert!(finding.to_string().starts_with("[slow]"));
    assert!(finding.remediation().contains("every query reads it"));
}

#[test]
fn a_table_with_no_metadata_is_reported_as_a_correctness_problem() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("empty");
    std::fs::create_dir_all(root.join("_delta_log")).expect("creating");

    let report = verify(&root);
    assert_eq!(report.findings, vec![Finding::NoMetadata]);
    assert!(report.has_correctness_findings());
}

#[test]
fn a_directory_that_is_not_a_table_says_so_rather_than_looking_empty() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let report = verify(&dir.path().join("nothing-here"));
    assert!(matches!(
        report.findings.first(),
        Some(Finding::Unreadable { .. })
    ));
}

#[test]
fn a_table_declaring_a_class_this_version_does_not_know_is_treated_as_external() {
    // A future version might add a class this one does not recognise. Treating an unknown
    // class as managed would grant it guarantees nobody here can honour.
    use std::collections::BTreeMap;
    let mut configuration = BTreeMap::new();
    configuration.insert(CLASS_KEY.to_string(), "federated".to_string());
    assert_eq!(
        TableClass::from_configuration(&configuration),
        TableClass::External
    );

    // And an absent declaration means the same thing, for the same reason.
    assert_eq!(
        TableClass::from_configuration(&BTreeMap::new()),
        TableClass::External
    );
}

#[test]
fn an_external_table_refuses_the_guarantees_it_cannot_offer() {
    assert!(!TableClass::External.supports_strong_reads());
    assert!(!TableClass::External.is_writable_here());
    assert!(TableClass::Managed.supports_strong_reads());
    assert!(TableClass::Managed.is_writable_here());
}

#[test]
fn the_report_distinguishes_slow_findings_from_wrong_ones_in_its_summary() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("empty");
    std::fs::create_dir_all(root.join("_delta_log")).expect("creating");

    let summary = verify(&root).summary();
    assert!(summary.contains("affecting correctness"), "{summary}");
}
