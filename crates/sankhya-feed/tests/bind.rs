//! Reading a document, and the six conversions this refuses to make.
//!
//! # Why a refusal is the interesting case here
//!
//! Binding a well-formed document is arithmetic. What decides whether a feed can be trusted
//! is what it does with a document that is *nearly* right --- because every permissive answer
//! produces a row that queries cleanly, renders in a report, and is wrong.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_feed::bind::{bind, Cell, Unfit};
use sankhya_feed::declare::{Column, Declaration, Microbatch, Missing, Quarantine, Unknown};
use sankhya_feed::validate::{validate, Feed};
use serde_json::json;

/// A feed of one column of the given type.
fn feed_of(written_type: &str, nullable: bool, missing: Missing, unknown: Unknown) -> Feed {
    validate(Declaration {
        name: "f".to_owned(),
        from: "/spool".to_owned(),
        schema: "s".to_owned(),
        table: "t".to_owned(),
        columns: vec![Column {
            name: "value".to_owned(),
            from: None,
            written_type: written_type.to_owned(),
            nullable,
            missing,
        }],
        unknown,
        microbatch: Microbatch::default(),
        quarantine: Quarantine::default(),
    })
    .expect("a sound one-column feed")
}

/// The ordinary feed: not null, a missing key refused, unknown keys refused.
fn strict(written_type: &str) -> Feed {
    feed_of(written_type, false, Missing::Refuse, Unknown::Refuse)
}

#[test]
fn a_document_that_fits_binds() {
    let feed = strict("int64");
    let row = bind(&feed, &json!({"value": 42})).expect("it fits");
    assert_eq!(row.cells, vec![Cell::Integer(42)]);
}

#[test]
fn a_string_is_not_a_number() {
    // The conversion every ingest tool offers. It works until the source emits `"4 2"`, and
    // then it works for that too, in whichever direction the parser leans.
    let feed = strict("int64");
    let refused = bind(&feed, &json!({"value": "42"})).expect_err("a string is not a number");

    match &refused {
        Unfit::WrongKind { column, found, .. } => {
            assert_eq!(column, "value");
            assert_eq!(*found, "a string");
        }
        other => panic!("expected a wrong kind, got {other:?}"),
    }
    assert!(refused.to_string().contains("forty-two"), "{refused}");
}

#[test]
fn a_float_is_not_a_whole_number() {
    // `3.0` converts and `3.5` truncates, and any rule that accepts the first has to have an
    // opinion about the second. This one declines to have one.
    let feed = strict("int32");
    for value in [json!(3.0), json!(3.5)] {
        let refused = bind(&feed, &json!({"value": value}));
        assert!(refused.is_err(), "{value} should not become a whole number");
    }
}

#[test]
fn a_whole_number_that_does_not_fit_the_column_is_refused_rather_than_wrapped() {
    let feed = strict("int16");
    let refused = bind(&feed, &json!({"value": 40_000})).expect_err("too wide for int16");

    match refused {
        Unfit::OutOfRange { column, value, .. } => {
            assert_eq!(column, "value");
            assert_eq!(value, "40000");
        }
        other => panic!("expected out of range, got {other:?}"),
    }
}

#[test]
fn a_decimal_arrives_as_a_string_because_a_json_number_is_not_exact() {
    let feed = strict("decimal(18,2)");

    let refused = bind(&feed, &json!({"value": 0.1})).expect_err("a JSON number is a double");
    assert!(refused.to_string().contains("0.1 is not 0.1"), "{refused}");

    // And as a string it survives the trip exactly: the unscaled integer, at the declared
    // scale, with no floating point anywhere in the path.
    let row = bind(&feed, &json!({"value": "0.10"})).expect("a string decimal");
    assert_eq!(row.cells, vec![Cell::Decimal(10)]);
    let row = bind(&feed, &json!({"value": "-1234.56"})).expect("a negative decimal");
    assert_eq!(row.cells, vec![Cell::Decimal(-123_456)]);
    // Fewer places than declared is padded, which loses nothing.
    let row = bind(&feed, &json!({"value": "7"})).expect("a whole decimal");
    assert_eq!(row.cells, vec![Cell::Decimal(700)]);
}

#[test]
fn a_decimal_with_more_places_than_declared_is_refused_rather_than_rounded() {
    // Rounding here is how a reconciliation fails by a penny that nobody can trace: the
    // source and the warehouse each hold a defensible number and they are not the same one.
    let feed = strict("decimal(18,2)");
    let refused = bind(&feed, &json!({"value": "1.005"})).expect_err("three places into two");
    assert!(matches!(refused, Unfit::OutOfRange { .. }), "{refused:?}");
}

#[test]
fn text_that_is_not_text_is_refused() {
    let feed = strict("utf8");
    let refused = bind(&feed, &json!({"value": 42})).expect_err("a number is not text");
    assert!(refused.to_string().contains("a decision this feed will not make"), "{refused}");
}

#[test]
fn a_json_column_carries_the_document_whole() {
    let feed = strict("json");
    let row = bind(&feed, &json!({"value": {"a": [1, 2]}})).expect("json carries anything");
    assert_eq!(row.cells, vec![Cell::Text(r#"{"a":[1,2]}"#.to_owned())]);
}

#[test]
fn a_missing_key_is_refused_unless_the_column_says_what_it_means() {
    let feed = strict("int64");
    let refused = bind(&feed, &json!({})).expect_err("the key is absent");
    assert!(matches!(refused, Unfit::MissingKey { .. }), "{refused:?}");
    assert!(refused.to_string().contains("indistinguishable"), "{refused}");

    let told = feed_of("int64", true, Missing::Null, Unknown::Refuse);
    let row = bind(&told, &json!({})).expect("said by name");
    assert_eq!(row.cells, vec![Cell::Null]);
}

#[test]
fn an_explicit_null_needs_a_column_that_accepts_one() {
    let feed = strict("int64");
    let refused = bind(&feed, &json!({"value": null})).expect_err("null into not-null");
    assert!(matches!(refused, Unfit::NullIntoNotNull { .. }), "{refused:?}");

    let nullable = feed_of("int64", true, Missing::Refuse, Unknown::Refuse);
    assert_eq!(
        bind(&nullable, &json!({"value": null})).expect("null is fine here").cells,
        vec![Cell::Null]
    );
}

#[test]
fn a_key_nobody_claimed_is_news_until_told_otherwise() {
    let feed = strict("int64");
    let refused = bind(&feed, &json!({"value": 1, "grew_a_field": true}))
        .expect_err("the source grew a field");
    assert_eq!(refused, Unfit::UnknownKey { key: "grew_a_field".to_owned() });

    let told = feed_of("int64", false, Missing::Refuse, Unknown::Ignore);
    let row = bind(&told, &json!({"value": 1, "grew_a_field": true})).expect("told to ignore");
    assert_eq!(row.cells, vec![Cell::Integer(1)]);
}

#[test]
fn something_that_is_not_a_dictionary_is_not_a_record() {
    let feed = strict("int64");
    for value in [json!([1, 2, 3]), json!("a line of text"), json!(7)] {
        assert_eq!(
            bind(&feed, &value).expect_err("not a dictionary"),
            Unfit::NotADocument
        );
    }
}
