//! What the log carries a bound as, and what comes back.
//!
//! The protocol's statistics document is **schemaless** — its keys are column names and its
//! values are bare JSON — so `"2026-09-14"` and `"paris"` are the same shape. `M24` made the
//! decoder read against the column's declared type for exactly that reason, and every test
//! here is a case where reading by JSON shape alone gives a bound that is never comparable
//! with anything, or worse, is comparable with the wrong thing.
//!
//! # Why dates go out as strings
//!
//! Because the protocol says so, and because a log is read by engines that are not this one.
//! Writing the day count would round-trip perfectly through this system's own reader and be
//! meaningless to every other — the failure this crate's header calls out as the worse kind,
//! since it cannot be fixed from here.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_schema::{DataType, Field, Schema, TimeUnit as ArrowUnit};
use proptest::prelude::*;
use sankhya_stats::{Bound, TimeUnit};
use sankhya_table_delta::{decode_bound, encode_bound};

fn round_trip(bound: &Bound, data_type: &DataType) -> Option<Bound> {
    decode_bound(&encode_bound(bound)?, Some(data_type))
}

#[test]
fn a_date_goes_out_as_the_protocol_spells_it() {
    let written = encode_bound(&Bound::Date(20_710)).expect("a date is representable");
    assert_eq!(
        written,
        serde_json::Value::String("2026-09-14".to_string()),
        "the protocol's date format, not the day count --- a reader that is not this system \
         has to be able to use it"
    );
    assert_eq!(
        decode_bound(&written, Some(&DataType::Date32)),
        Some(Bound::Date(20_710))
    );
}

#[test]
fn a_date_read_without_a_type_is_a_string_and_prunes_nothing() {
    // The behaviour that made this change necessary, pinned so it cannot come back by
    // accident. Decoded by JSON shape alone a date is bytes, and bytes are incomparable with
    // the `Bound::Date` a query's literal produces --- so the bound is written, read, and
    // never matches. A scan rather than a wrong answer, and exactly the state `M24` set out
    // to leave: statistics recorded and pruning that never happens.
    let written = encode_bound(&Bound::Date(20_710)).expect("representable");
    let blind = decode_bound(&written, None).expect("a value comes back");
    assert!(matches!(blind, Bound::Bytes(_)), "{blind:?}");
    assert_eq!(blind.compare(&Bound::Date(20_710)), None);
}

#[test]
fn an_instant_keeps_its_unit_across_the_log() {
    for (unit, arrow, value) in [
        (TimeUnit::Second, ArrowUnit::Second, 1_788_912_000_i64),
        (TimeUnit::Millisecond, ArrowUnit::Millisecond, 1_788_912_000_123),
        (TimeUnit::Microsecond, ArrowUnit::Microsecond, 1_788_912_000_123_456),
        (TimeUnit::Nanosecond, ArrowUnit::Nanosecond, 1_788_912_000_123_456_789),
    ] {
        let bound = Bound::Timestamp { value, unit };
        assert_eq!(
            round_trip(&bound, &DataType::Timestamp(arrow, None)),
            Some(bound.clone()),
            "{bound:?} must survive the log unchanged"
        );
    }
}

#[test]
fn an_instant_carrying_an_offset_is_refused_rather_than_shifted() {
    // A stats document is specified as UTC. A value written with `+05:30` is one whose
    // writer meant something this cannot check, and applying the offset on a guess moves
    // every bound in the column by five and a half hours --- consistently, so nothing looks
    // wrong until somebody reconciles against the source.
    let offset = serde_json::Value::String("2026-09-14T00:00:00.000000+05:30".to_string());
    assert_eq!(
        decode_bound(&offset, Some(&DataType::Timestamp(ArrowUnit::Microsecond, None))),
        None
    );

    // And a fraction that is not digits but that Rust's own integer parser would take.
    //
    // This is the case that makes "these must be digits" falsifiable. Every offset form ---
    // `+05:30`, `+0530`, a short one against a wide unit --- is already refused by the length
    // check or by the pad-and-parse below it, so deleting the digit check left every test
    // passing. `i64::from_str` accepts a leading sign, though, so `.+12` pads to `+12000000`
    // and parses to twelve million: a bound read out of text that is not a fraction at all.
    let signed = serde_json::Value::String("2026-09-14T00:00:00.+12Z".to_string());
    assert_eq!(
        decode_bound(&signed, Some(&DataType::Timestamp(ArrowUnit::Nanosecond, None))),
        None,
        "a fraction is digits, and a sign the integer parser happens to accept is not one"
    );
}

#[test]
fn a_finer_fraction_than_the_unit_holds_is_refused() {
    // Truncating loses precision the bound had, which moves a **maximum down** --- the one
    // direction that skips a file holding rows.
    let finer = serde_json::Value::String("2026-09-14T00:00:00.123456789Z".to_string());
    assert_eq!(
        decode_bound(&finer, Some(&DataType::Timestamp(ArrowUnit::Millisecond, None))),
        None
    );
}

#[test]
fn money_survives_the_log_exactly() {
    let bound = Bound::Decimal { unscaled: 123_456, scale: 2 };
    assert_eq!(
        encode_bound(&bound),
        Some(serde_json::json!(1234.56)),
        "the protocol carries a decimal as a number"
    );
    assert_eq!(
        round_trip(&bound, &DataType::Decimal128(18, 2)),
        Some(bound),
        "and it comes back as the same amount, not the nearest float to it"
    );
}

#[test]
fn a_decimal_that_will_not_round_trip_is_written_as_no_bound() {
    // Exact or nothing. This crate is built without arbitrary-precision JSON, so a value
    // goes through `f64`; one that does not survive that is not written, rather than written
    // slightly wrong. An absent bound costs a scan.
    let too_precise = Bound::Decimal {
        unscaled: 123_456_789_012_345_678_901_234_567_890,
        scale: 10,
    };
    assert_eq!(encode_bound(&too_precise), None);
}

#[test]
fn what_the_log_already_holds_still_reads() {
    // Every bound this system wrote before `M24` was an integer, a float or a string under
    // exactly the old rules. A schema-aware decoder that changed any of them would make an
    // existing warehouse's statistics disagree with the data they describe.
    let schema = Schema::new(vec![
        Field::new("n", DataType::Int64, false),
        Field::new("f", DataType::Float64, false),
        Field::new("s", DataType::Utf8, false),
    ]);
    let cases: [(&str, Bound); 3] = [
        ("n", Bound::Int(42)),
        ("f", Bound::Float(1.5)),
        ("s", Bound::Bytes(b"paris".to_vec())),
    ];
    for (name, bound) in cases {
        let (_, field) = schema.column_with_name(name).expect("declared");
        assert_eq!(round_trip(&bound, field.data_type()), Some(bound.clone()), "{name}");
    }
}

proptest! {
    /// Every representable date survives the log.
    ///
    /// Over the whole range `Date32` can hold, because the rendering goes through a civil
    /// calendar and the leap-day irregularity is where a hand-written conversion goes wrong.
    #[test]
    fn a_date_round_trips(days in -700_000i32..2_900_000) {
        let bound = Bound::Date(days);
        prop_assert_eq!(round_trip(&bound, &DataType::Date32), Some(bound));
    }

    /// And every instant a microsecond column can hold, either side of the epoch.
    #[test]
    fn an_instant_round_trips(micros in -60_000_000_000_000_000i64..60_000_000_000_000_000) {
        let bound = Bound::Timestamp { value: micros, unit: TimeUnit::Microsecond };
        prop_assert_eq!(
            round_trip(&bound, &DataType::Timestamp(ArrowUnit::Microsecond, None)),
            Some(bound)
        );
    }
}
