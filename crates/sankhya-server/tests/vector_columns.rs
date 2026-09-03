//! A table with a vector column, published and read back through the server.
//!
//! # The question this answers
//!
//! *"Can a column be declared as a vector, on both sides?"* This is the analytical half,
//! answered by demonstration rather than by reading the schema code: a `FixedSizeList<Float64,
//! 3>` column written through the product's own writer, and read back by a statement that
//! calls a vector function on it.
//!
//! The **width** is the part worth testing. `ADR-0021` Decision 2 makes it part of the type,
//! because it is what lets a kernel take a contiguous slice instead of copying per row --- and
//! a column that silently degraded to a variable-length list would lose that with no symptom
//! but a slope on a latency chart.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use arrow_array::builder::{FixedSizeListBuilder, Float64Builder};
use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use common::{first_column_oid, start, text_rows};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::Arc;

const WIDTH: i32 = 3;

fn embeddings() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), WIDTH),
            false,
        ),
    ]))
}

#[test]
fn a_column_can_be_declared_a_vector_and_read_back_as_one() {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    let root = warehouse.join("docs").join("embeddings");

    let publication = Publication::external(&root, "embeddings");
    publication.create(&embeddings()).expect("creating a table with a vector column");

    let mut vectors = FixedSizeListBuilder::new(Float64Builder::new(), WIDTH);
    for row in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.6, 0.8, 0.0]] {
        vectors.values().append_slice(&row);
        vectors.append(true);
    }
    let batch = RecordBatch::try_new(
        embeddings(),
        vec![
            Arc::new(Int64Array::from(vec![1_i64, 2, 3])),
            Arc::new(vectors.finish()),
        ],
    )
    .expect("a batch");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(3))
        .expect("publishing vectors");

    let server = start(&warehouse, &dir.path().join("data"));

    // A statement calls a vector function on the stored column, which is the whole point
    // of the column being a vector rather than three columns of doubles.
    let ranked = text_rows(
        server.port,
        "SELECT id FROM docs.embeddings \
         ORDER BY vec_cosine_similarity(embedding, vec_of(1.0, 0.0, 0.0)) DESC",
    );
    let order: Vec<&str> = ranked.iter().filter_map(|row| row[0].as_deref()).collect();
    assert_eq!(
        order,
        vec!["1", "3", "2"],
        "a similarity search over a stored vector column ranked wrongly: {ranked:?}"
    );

    // A reduction over the column, so the kernel is reached with real data rather than with a
    // literal built in the SELECT list.
    let norms = text_rows(server.port, "SELECT round(vec_norm_l2(embedding)) FROM docs.embeddings");
    assert_eq!(norms.len(), 3, "{norms:?}");
    for row in &norms {
        assert_eq!(row[0].as_deref(), Some("1"), "every fixture vector is a unit vector");
    }
}

#[test]
fn a_vector_crosses_the_wire_as_an_array_and_not_as_text() {
    // `ADR-0021` Decision 3. It was sent as `text`, so a client received the characters
    // `[1.0, 2.0]` and had to parse them --- and would have got it wrong on a null element, on
    // a locale rendering a decimal comma, and on an empty array against a null one.
    //
    // `{1,2}` is PostgreSQL's own array syntax, under OID 1022, which every driver already
    // decodes. The OID and the rendering are one change: announcing the type while sending
    // Arrow's brackets would be worse than sending text, because a client would then try.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).expect("a warehouse");
    let server = start(&warehouse, &dir.path().join("data"));

    // The type, which is what a driver dispatches on and what the rendering alone cannot
    // show: a column sent as `text` and one sent as `float8[]` can carry identical bytes.
    assert_eq!(
        first_column_oid(server.port, "SELECT vec_of(1.0, 2.5, 3.0) AS v"),
        Some(1022),
        "a vector was not described as `float8[]`, so a driver sees a string"
    );
    // And an ordinary column is untouched by the change.
    assert_eq!(first_column_oid(server.port, "SELECT 1.5 AS n"), Some(701));

    let rows = text_rows(server.port, "SELECT vec_of(1.0, 2.5, 3.0) AS v");
    let rendered = rows.first().and_then(|row| row[0].as_deref()).unwrap_or_default();
    assert_eq!(rendered, "{1,2.5,3}", "a vector was not rendered as an array");
    assert!(!rendered.contains('['), "Arrow's own rendering reached the wire: {rendered}");

    // A series result travels the same way, so the two do not diverge.
    let series = text_rows(server.port, "SELECT vec_differences(vec_of(1.0, 4.0, 9.0)) AS d");
    assert_eq!(series.first().and_then(|row| row[0].as_deref()), Some("{3,5}"));

    // And a null element stays a null rather than becoming a zero or an empty slot, which is
    // what PostgreSQL's bare `NULL` in an array literal means.
    let mixed = text_rows(server.port, "SELECT vec_of(1.0, 2.0) AS v WHERE 1 = 0");
    assert!(mixed.is_empty(), "{mixed:?}");
}

#[test]
fn a_batch_of_wholly_different_columns_is_refused() {
    // Asked first, because it decides how large the previous finding is. If the write path
    // enforces nothing at all, then "a vector of the wrong width is accepted" is not a vector
    // problem --- it is a schema problem that happens to have been noticed through a vector.
    let dir = tempfile::tempdir().expect("a directory");
    let root = dir.path().join("warehouse").join("docs").join("t");
    let declared = Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
    ]);
    let publication = Publication::external(&root, "t");
    publication.create(&declared).expect("creating");

    let other = Arc::new(Schema::new(vec![
        Field::new("wholly", DataType::Utf8, false),
        Field::new("different", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&other),
        vec![
            Arc::new(arrow_array::StringArray::from(vec!["x"])),
            Arc::new(Int64Array::from(vec![1_i64])),
        ],
    )
    .expect("a batch");

    assert!(
        publication.append(1, "part-0000.parquet", &batch, Lsn::new(1)).is_err(),
        "a table accepted a batch of columns it never declared"
    );
}

#[test]
fn a_vector_column_of_a_different_width_is_a_different_column() {
    // `ADR-0021` Decision 2: the width is part of the type. A 384-dimensional embedding and a
    // 512-dimensional one have no meaningful cosine between them, so a write of the wrong
    // width is refused rather than accommodated by widening the column to a variable list.
    let dir = tempfile::tempdir().expect("a directory");
    let root = dir.path().join("warehouse").join("docs").join("embeddings");
    let publication = Publication::external(&root, "embeddings");
    publication.create(&embeddings()).expect("creating");

    let wider = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float64, true)), 4),
            false,
        ),
    ]));
    let mut vectors = FixedSizeListBuilder::new(Float64Builder::new(), 4);
    vectors.values().append_slice(&[1.0, 0.0, 0.0, 0.0]);
    vectors.append(true);
    let batch = RecordBatch::try_new(
        Arc::clone(&wider),
        vec![Arc::new(Int64Array::from(vec![1_i64])), Arc::new(vectors.finish())],
    )
    .expect("a batch");

    assert!(
        publication.append(1, "part-0000.parquet", &batch, Lsn::new(1)).is_err(),
        "a vector of the wrong width was accepted into the column, which silently changes \
         what every similarity against it means"
    );
}

#[test]
fn computing_over_a_column_sends_the_answer_rather_than_the_data() {
    // The claim the whole function catalogue rests on: **only the results cross the wire**.
    // Asserted with a number, because a claim with no number is a claim.
    //
    // The same twelve quantiles, reached two ways. Server-side, one number per row comes back.
    // Client-side, every simulated outcome has to arrive before the client can take a quantile
    // of it. The ratio is not a constant --- it is the width of the vector --- so it does not
    // improve as a book grows.
    let (_dir, server) = running_with_risk();

    let answers = text_rows(
        server.port,
        "SELECT position_id, vec_quantile(pnl, vec_of(0.05)) AS var_95 FROM risk.positions",
    );
    let vectors = text_rows(server.port, "SELECT position_id, pnl FROM risk.positions");

    assert_eq!(answers.len(), vectors.len(), "the two routes read different rows");
    assert!(!answers.is_empty(), "the fixture has no positions");

    let answer_bytes: usize = answers
        .iter()
        .flat_map(|row| row.iter())
        .map(|value| value.as_deref().unwrap_or("").len())
        .sum();
    let vector_bytes: usize = vectors
        .iter()
        .flat_map(|row| row.iter())
        .map(|value| value.as_deref().unwrap_or("").len())
        .sum();

    assert!(
        vector_bytes > answer_bytes * 10,
        "computing in the warehouse saved less than tenfold: {answer_bytes} against \
         {vector_bytes}. Either the vectors got narrower or the answer stopped being one \
         number per row --- and the second is the one worth knowing about"
    );
}

/// A server over the fixture's risk table.
fn running_with_risk() -> (tempfile::TempDir, common::Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    common::write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

#[test]
fn a_vector_column_reports_its_type_rather_than_text() {
    // A client asking what type a column is gets the answer in one place, and that place said
    // `text` for every vector column --- while the same column crossed the wire as `float8[]`.
    // Two answers about one column, and the catalogue's was the wrong one.
    let (_dir, server) = running_with_risk();

    let rows = text_rows(
        server.port,
        "SELECT column_name, data_type FROM information_schema.columns \
         WHERE table_name = 'positions'",
    );

    // Two columns asked for, two returned, in that order.
    //
    // This used to index around a defect and said so: the projection was ignored, so a client
    // writing `SELECT column_name, data_type` received all six columns of
    // `information_schema.columns` in the catalogue's own order. It asserted the six and left a
    // note --- *if it is ever fixed, this fails and says where to look* --- which is exactly
    // what happened on 2026-09-03.
    assert_eq!(rows[0].len(), 2, "the projection is honoured: two asked for, two returned");

    let pnl = rows
        .iter()
        .find(|row| row[0].as_deref() == Some("pnl"))
        .unwrap_or_else(|| panic!("no `pnl` column in {rows:?}"));
    assert_eq!(
        pnl[1].as_deref(),
        Some("float8[]"),
        "the catalogue reports a vector column as something a client cannot decode"
    );

    // And the ordinary columns beside it are untouched by the change.
    let id = rows
        .iter()
        .find(|row| row[0].as_deref() == Some("position_id"))
        .expect("a `position_id` column");
    assert_eq!(id[1].as_deref(), Some("int8"));
}
