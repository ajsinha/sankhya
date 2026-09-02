//! A batch is checked against the table's own schema before it is written.
//!
//! # What was missing
//!
//! `publish_table` compared a batch against the schema **it was handed**, which is the easy
//! case: the caller already had it. `append` is the path everything actually uses --- feeds,
//! compaction, every test --- and it read the batch's schema from the batch and never looked
//! at the table's.
//!
//! So a table declaring `id, amount` accepted a batch of two entirely different columns, and a
//! vector column of width three accepted a vector of width four. The second is the one that
//! does quiet damage: a cosine similarity between a 384-dimensional embedding and a
//! 512-dimensional one is not a near miss, it is a different question, and a column holding
//! both would answer it.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use arrow_array::builder::{FixedSizeListBuilder, Float64Builder};
use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::Arc;

fn table(fields: Vec<Field>) -> Schema {
    Schema::new(fields)
}

fn declared() -> Schema {
    table(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
        Field::new("note", DataType::Utf8, true),
    ])
}

fn published(root: &std::path::Path) -> Publication {
    let publication = Publication::external(root.join("t"), "t");
    publication.create(&declared()).expect("creating");
    publication
}

#[test]
fn a_batch_matching_the_declaration_is_written() {
    let dir = tempfile::tempdir().expect("a directory");
    let publication = published(dir.path());

    let batch = RecordBatch::try_new(
        Arc::new(declared()),
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(Float64Array::from(vec![10.0])),
            Arc::new(StringArray::from(vec![Some("ok")])),
        ],
    )
    .expect("a batch");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect("a batch that agrees with the table is written");
}

#[test]
fn a_batch_of_columns_the_table_never_declared_is_refused() {
    let dir = tempfile::tempdir().expect("a directory");
    let publication = published(dir.path());

    let other = Arc::new(table(vec![
        Field::new("wholly", DataType::Utf8, false),
        Field::new("different", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        other,
        vec![
            Arc::new(StringArray::from(vec!["x"])),
            Arc::new(Int64Array::from(vec![1_i64])),
        ],
    )
    .expect("a batch");

    let refused = publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect_err("a table accepted columns it never declared");
    let said = refused.to_string();
    assert!(said.contains("cannot be null"), "{said}");
    assert!(
        said.contains("not adding to the table"),
        "the refusal says why this is not evolution: {said}"
    );
}

#[test]
fn a_column_that_changes_type_is_refused_and_both_types_are_named() {
    let dir = tempfile::tempdir().expect("a directory");
    let publication = published(dir.path());

    let changed = Arc::new(table(vec![
        Field::new("id", DataType::Int64, false),
        // Was `Float64`.
        Field::new("amount", DataType::Utf8, false),
        Field::new("note", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        changed,
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(StringArray::from(vec!["ten"])),
            Arc::new(StringArray::from(vec![Some("n")])),
        ],
    )
    .expect("a batch");

    let said = publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect_err("a column changed type")
        .to_string();
    assert!(said.contains("`amount`"), "{said}");
    assert!(said.contains("two types"), "{said}");
}

#[test]
fn a_new_column_is_additive_and_is_written() {
    // `ARCHITECTURE` §6.6: additive and compatible changes apply automatically. A check that
    // refused this would have turned schema evolution off, which is a larger bug than the one
    // it was closing.
    let dir = tempfile::tempdir().expect("a directory");
    let publication = published(dir.path());

    let wider = Arc::new(table(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
        Field::new("note", DataType::Utf8, true),
        Field::new("added_later", DataType::Int64, true),
    ]));
    let batch = RecordBatch::try_new(
        wider,
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(Float64Array::from(vec![10.0])),
            Arc::new(StringArray::from(vec![Some("n")])),
            Arc::new(Int64Array::from(vec![7_i64])),
        ],
    )
    .expect("a batch");

    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect("adding a column is evolution, not a contradiction");
}

#[test]
fn a_nullable_column_may_be_left_out() {
    // Omitting a column that *can* be null produces rows the schema permits. Omitting one that
    // cannot is the case the previous test covers, and the two must not be collapsed.
    let dir = tempfile::tempdir().expect("a directory");
    let publication = published(dir.path());

    let without = Arc::new(table(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        without,
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(Float64Array::from(vec![10.0])),
        ],
    )
    .expect("a batch");

    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect("a nullable column may be absent");
}

#[test]
fn a_vector_of_the_wrong_width_is_refused() {
    // The width is part of the type --- `ADR-0021` Decision 2 --- because it is what lets a
    // kernel take a contiguous slice instead of copying per row, and because a similarity
    // between vectors of different widths is not a near miss but a different question.
    let dir = tempfile::tempdir().expect("a directory");
    let root = dir.path().join("embeddings");
    let three = table(vec![Field::new(
        "embedding",
        DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), 3),
        false,
    )]);
    let publication = Publication::external(&root, "embeddings");
    publication.create(&three).expect("creating");

    let mut wider = FixedSizeListBuilder::new(Float64Builder::new(), 4);
    wider.values().append_slice(&[1.0, 0.0, 0.0, 0.0]);
    wider.append(true);
    let batch = RecordBatch::try_new(
        Arc::new(table(vec![Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), 4),
            false,
        )])),
        vec![Arc::new(wider.finish())],
    )
    .expect("a batch");

    let said = publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect_err("a vector of the wrong width was accepted")
        .to_string();
    assert!(said.contains("`embedding`"), "{said}");
    assert!(
        said.contains("change of question"),
        "the refusal says why a width is not a detail: {said}"
    );
}

#[test]
fn a_timestamp_the_format_cannot_distinguish_is_not_a_contradiction() {
    // The first thing the check caught was the quarantine table writing UTC-aware timestamps
    // into a column its own metadata calls naive --- and that is *not* a contradiction, because
    // the format's timestamps are UTC-normalised and the round trip erases the zone before any
    // reader sees it. Comparing raw Arrow types reported it; comparing what the format records
    // does not.
    let dir = tempfile::tempdir().expect("a directory");
    let root = dir.path().join("events");
    let naive = table(vec![Field::new(
        "arrived_at",
        DataType::Timestamp(arrow_schema::TimeUnit::Microsecond, None),
        false,
    )]);
    let publication = Publication::external(&root, "events");
    publication.create(&naive).expect("creating");

    let aware = Arc::new(table(vec![Field::new(
        "arrived_at",
        DataType::Timestamp(arrow_schema::TimeUnit::Microsecond, Some("UTC".into())),
        false,
    )]));
    let batch = RecordBatch::try_new(
        aware,
        vec![Arc::new(
            arrow_array::TimestampMicrosecondArray::from(vec![1_700_000_000_000_000_i64])
                .with_timezone("UTC"),
        )],
    )
    .expect("a batch");

    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect("a difference no reader can observe is not a contradiction");
}
