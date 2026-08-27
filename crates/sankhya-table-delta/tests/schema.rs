//! The schema string, and what it refuses to say.
//!
//! The schema in the log is what another engine believes the table contains. Every
//! assertion here about a *refusal* is an assertion that a wrong belief is impossible to
//! publish.

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

use arrow_schema::{DataType, Field, Schema, TimeUnit};
use sankhya_table_delta::schema_string;

fn one(name: &str, data_type: DataType, nullable: bool) -> Schema {
    Schema::new(vec![Field::new(name, data_type, nullable)])
}

#[test]
fn the_types_the_mapping_produces_all_translate() {
    // Every Arrow type the source type mapping can emit. If this list falls behind the
    // mapping, tables stop publishing rather than publishing wrongly — but they stop,
    // so it is worth keeping in step.
    let schema = Schema::new(vec![
        Field::new("b", DataType::Boolean, false),
        Field::new("i16", DataType::Int16, false),
        Field::new("i32", DataType::Int32, false),
        Field::new("i64", DataType::Int64, false),
        Field::new("f32", DataType::Float32, true),
        Field::new("f64", DataType::Float64, true),
        Field::new("dec", DataType::Decimal128(38, 9), true),
        Field::new("s", DataType::Utf8, true),
        Field::new("bin", DataType::Binary, true),
        Field::new("uuid", DataType::FixedSizeBinary(16), true),
        Field::new("d", DataType::Date32, true),
        Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            true,
        ),
        Field::new("lsn", DataType::UInt64, false),
    ]);

    let json = schema_string(&schema).expect("every mapped type must translate");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed["type"], "struct");
    assert_eq!(
        parsed["fields"].as_array().expect("fields").len(),
        schema.fields().len()
    );
}

#[test]
fn a_decimal_keeps_its_precision_and_scale() {
    // The one type where a nearly-right answer is worst. A decimal published as a double
    // is read confidently and is silently inexact.
    let json = schema_string(&one("amount", DataType::Decimal128(18, 4), false)).expect("ok");
    assert!(json.contains(r#""type":"decimal(18,4)""#), "{json}");
}

#[test]
fn nullability_is_carried_exactly() {
    let required = schema_string(&one("id", DataType::Int64, false)).expect("ok");
    assert!(required.contains(r#""nullable":false"#));

    let optional = schema_string(&one("id", DataType::Int64, true)).expect("ok");
    assert!(optional.contains(r#""nullable":true"#));
}

#[test]
fn a_nanosecond_timestamp_is_refused() {
    // The protocol's timestamp is microsecond-precision. Publishing a nanosecond column
    // as one would drop three digits from every value, with nothing to indicate it.
    let err = schema_string(&one(
        "t",
        DataType::Timestamp(TimeUnit::Nanosecond, None),
        true,
    ))
    .expect_err("a nanosecond timestamp must be refused");

    assert_eq!(err.field, "t");
    assert!(format!("{err}").contains("confidently and wrongly"));
}

#[test]
fn a_millisecond_timestamp_is_refused() {
    // The other direction, and the more tempting one: it fits, so it looks safe. It
    // would add three zeroes of precision the source never had.
    assert!(schema_string(&one(
        "t",
        DataType::Timestamp(TimeUnit::Millisecond, None),
        true
    ))
    .is_err());
}

#[test]
fn a_negative_decimal_scale_is_refused() {
    // Clamping it to zero moves the decimal point, which is a wrong number rather than
    // a lost one.
    assert!(schema_string(&one("x", DataType::Decimal128(10, -2), true)).is_err());
}

#[test]
fn an_unrepresentable_type_names_the_column() {
    // "the schema is unsupported" is not something an operator can act on.
    let schema = Schema::new(vec![
        Field::new("fine", DataType::Int64, false),
        Field::new("trouble", DataType::Duration(TimeUnit::Microsecond), true),
    ]);
    let err = schema_string(&schema).expect_err("a duration has no representation here");
    assert_eq!(err.field, "trouble");
    assert!(err.arrow_type.contains("Duration"));
}

#[test]
fn column_names_needing_escaping_survive_it() {
    // A name with a quote in it would otherwise produce a log that is not valid JSON,
    // which fails at the reader rather than at the writer.
    let json = schema_string(&one(r#"od"d"#, DataType::Int64, false)).expect("ok");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("still valid json");
    assert_eq!(parsed["fields"][0]["name"], r#"od"d"#);
}

// --- reading a schema back out of a log -----------------------------------

#[test]
fn a_schema_round_trips_through_the_log() {
    // A server reads tables it did not write — on restart, or written by another node — so
    // the schema has to come from the log rather than from whoever created the table.
    let original = Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, true),
        Field::new("ratio", DataType::Float64, true),
        Field::new("flag", DataType::Boolean, false),
        Field::new("when", DataType::Timestamp(TimeUnit::Microsecond, None), true),
        Field::new("day", DataType::Date32, true),
        Field::new("amount", DataType::Decimal128(38, 9), false),
    ]);

    let json = schema_string(&original).expect("every type here is representable");
    let read_back = sankhya_table_delta::schema_from_string(&json).expect("and readable");

    assert_eq!(read_back, original, "the round trip must be exact");
}

#[test]
fn nullability_survives_the_round_trip() {
    // A column read back as nullable when it is not lets a null through a constraint the
    // writer was enforcing; the other way round refuses data the table legitimately holds.
    let original = Schema::new(vec![
        Field::new("required", DataType::Int64, false),
        Field::new("optional", DataType::Int64, true),
    ]);
    let json = schema_string(&original).expect("representable");
    let read_back = sankhya_table_delta::schema_from_string(&json).expect("readable");

    assert!(!read_back.field(0).is_nullable());
    assert!(read_back.field(1).is_nullable());
}

#[test]
fn an_unsigned_column_reads_back_as_signed_and_that_is_written_down() {
    // The log writes both `Int64` and `UInt64` as `long`, so a reader cannot tell them
    // apart and has to pick. Signed is the choice that preserves every value the log can
    // hold, and this test exists so the asymmetry is a decision rather than a surprise.
    let original = Schema::new(vec![Field::new("lsn", DataType::UInt64, false)]);
    let json = schema_string(&original).expect("representable");
    let read_back = sankhya_table_delta::schema_from_string(&json).expect("readable");

    assert_eq!(read_back.field(0).data_type(), &DataType::Int64);
    assert_ne!(read_back, original, "and the round trip is not exact for it");
}

#[test]
fn a_type_this_reader_does_not_know_is_named_rather_than_skipped() {
    // Skipping it would produce a table quietly missing a column, and every query against
    // that column would fail somewhere far from the cause.
    let json = r#"{"type":"struct","fields":[
        {"name":"ok","type":"long","nullable":false,"metadata":{}},
        {"name":"exotic","type":"interval","nullable":true,"metadata":{}}
    ]}"#;
    let Err(unsupported) = sankhya_table_delta::schema_from_string(json) else {
        panic!("an unknown type must be refused");
    };
    assert_eq!(unsupported.field, "exotic");
    assert!(unsupported.arrow_type.contains("interval"));
}

#[test]
fn a_nested_type_is_refused_by_name_rather_than_guessed_at() {
    // A nested type arrives as an object rather than a string. Guessing a flat type for it
    // produces a table other engines read confidently and wrongly.
    let json = r#"{"type":"struct","fields":[
        {"name":"nested","type":{"type":"array","elementType":"long"},"nullable":true,"metadata":{}}
    ]}"#;
    let Err(unsupported) = sankhya_table_delta::schema_from_string(json) else {
        panic!("a nested type must be refused");
    };
    assert_eq!(unsupported.field, "nested");
}

#[test]
fn a_schema_that_is_not_valid_json_is_refused_with_what_the_parser_said() {
    let Err(unsupported) = sankhya_table_delta::schema_from_string("{not json") else {
        panic!("malformed JSON must be refused");
    };
    assert!(unsupported.field.contains("schema itself"));
}
