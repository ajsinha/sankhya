//! What a value looks like on the wire, in the format a PostgreSQL driver parses.
//
//! # Why this file exists
//
//! **Zero tests touched a timestamp.** Microsecond timestamps — this project's own canonical
//! unit — were rendered with `.to_string()` on the raw `i64` under OID 1114/1184, so `psql`
//! printed `1756545242000000` and JDBC and psycopg raised on it. The other three units fell
//! through to Arrow's display, which writes `T` between the date and the time and `Z` for the
//! zone where PostgreSQL writes a space and a numeric offset. `bytea` went out as bare hex
//! with no `\x`, which a driver decodes as the characters rather than the bytes.
//
//! That is `CLI-01`, and none of it is visible from inside the engine: the value is right, the
//! rendering is not, and the only place a rendering is wrong is over a socket.
//!
//! # What is deliberately not asserted here
//!
//! `CLI-01` also records that the catalogue and the result set disagree about a **zone-aware**
//! column: the result set maps it to `TIMESTAMPTZ` and the catalogue said `TIMESTAMP`.
//!
//! The catalogue's mapping is corrected, and it is not enough, because the two read different
//! sources. The result set types a column from the Arrow schema the scan produces, which comes
//! from the Parquet footer and keeps the timezone. The catalogue types it from the table's
//! declared schema in the log, and `schemaString` writes `"timestamp"` for both zone-aware and
//! naive — the Delta protocol has one timestamp type — so the zone is gone by the time the
//! catalogue sees it.
//!
//! Making them agree means either recording the zone in the log or normalising the read path to
//! the declaration. That is a format decision rather than a rendering fix, and writing a test
//! that passes against the present behaviour would pin the disagreement instead of the
//! property. It is recorded in `REMEDIATION.md` and left open.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use arrow_array::{BinaryArray, Int64Array, RecordBatch, TimestampMicrosecondArray};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use common::{start, text_rows, Running};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::sync::Arc;

/// `2026-08-23 09:14:02.000123` UTC, in microseconds since the epoch.
const AT: i64 = 1_787_476_442_000_123;

fn events() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("at", DataType::Timestamp(TimeUnit::Microsecond, None), false),
    ]))
}

/// The server, **returned** rather than its port: dropping the handle stops the thread,
/// and a port with nothing behind it refuses the connection.
fn served(dir: &tempfile::TempDir, rows: &[i64]) -> Running {
    let warehouse = dir.path().join("warehouse");
    let root = warehouse.join("logs").join("events");
    let publication = Publication::external(&root, "events");
    publication.create(&events()).expect("creating");

    let batch = RecordBatch::try_new(
        events(),
        vec![
            Arc::new(Int64Array::from((0..rows.len() as i64).collect::<Vec<i64>>())),
            Arc::new(TimestampMicrosecondArray::from(rows.to_vec())),
        ],
    )
    .expect("a batch");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect("publishing");

    start(&warehouse, &dir.path().join("data"))
}

#[test]
fn a_microsecond_timestamp_arrives_as_a_timestamp_and_not_as_a_number() {
    // The finding, exactly: `psql` printed `1756545242000000`.
    let dir = tempfile::tempdir().expect("a directory");
    let server = served(&dir, &[AT]);

    let rows = text_rows(server.port, "SELECT at FROM logs.events");
    let value = rows[0][0].as_deref().expect("a value");
    assert_eq!(
        value, "2026-08-23 09:14:02.000123",
        "a timestamp went out as {value}, which is not something a driver can parse"
    );
}

#[test]
fn a_whole_second_carries_no_fractional_part() {
    // PostgreSQL omits it, and a round-trip comparison against PostgreSQL's own output is
    // what a driver's test suite does.
    let dir = tempfile::tempdir().expect("a directory");
    let server = served(&dir, &[1_787_476_442_000_000]);

    let rows = text_rows(server.port, "SELECT at FROM logs.events");
    assert_eq!(rows[0][0].as_deref(), Some("2026-08-23 09:14:02"));
}

#[test]
fn an_instant_before_the_epoch_borrows_from_the_day_rather_than_truncating_toward_zero() {
    // The arithmetic that is wrong on exactly one side of one boundary, and therefore never
    // noticed: `-1` microsecond is the last microsecond of 1969, not the first of 1970.
    let dir = tempfile::tempdir().expect("a directory");
    let server = served(&dir, &[-1]);

    let rows = text_rows(server.port, "SELECT at FROM logs.events");
    assert_eq!(rows[0][0].as_deref(), Some("1969-12-31 23:59:59.999999"));
}

#[test]
fn the_catalogue_and_the_result_set_agree_about_a_timestamp() {
    // They did not: `information_schema` mapped every timestamp to `TIMESTAMP` while the
    // result set mapped a zone-aware one to `TIMESTAMPTZ`. A client that describes a column
    // and then reads it must get the same answer twice.
    let dir = tempfile::tempdir().expect("a directory");
    let server = served(&dir, &[AT]);

    // Both columns, and the row picked by name here rather than by a `WHERE` clause: the
    // catalogue is answered by a recogniser that matches the statement's shape, and a
    // predicate it does not implement would silently select the wrong row for this assertion.
    let described = text_rows(
        server.port,
        "SELECT column_name, data_type FROM information_schema.columns",
    );
    let named = described
        .iter()
        .find(|row| row[0].as_deref() == Some("at"))
        .and_then(|row| row[1].clone())
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        named.contains("timestamp"),
        "the catalogue calls `at` {named:?}, and the result set renders it as a timestamp: \
         {described:?}"
    );
}

#[test]
fn bytea_carries_the_marker_a_driver_strips_before_decoding() {
    // `bytea` went out as bare hex. PostgreSQL's `bytea_output = hex` writes `\x` first and
    // every driver strips it; given bare hex a driver decodes the **characters**, so every
    // byte comes back wrong and nothing errors anywhere.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    let root = warehouse.join("logs").join("blobs");

    let schema = Arc::new(Schema::new(vec![Field::new("payload", DataType::Binary, false)]));
    let publication = Publication::external(&root, "blobs");
    publication.create(&schema).expect("creating");

    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(BinaryArray::from_vec(vec![&[0x00, 0xde, 0xad, 0xff]]))],
    )
    .expect("a batch");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect("publishing");

    let server = start(&warehouse, &dir.path().join("data"));
    let rows = text_rows(server.port, "SELECT payload FROM logs.blobs");
    assert_eq!(
        rows[0][0].as_deref(),
        Some("\\x00deadff"),
        "bytea went out in a form a driver cannot decode: {rows:?}"
    );
}
