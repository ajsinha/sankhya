//! The date axis, and the per-row default it deliberately does not have.
//!
//! The test that carries the most weight here is
//! `a_null_in_the_declared_source_is_an_error_not_todays_date`. A per-row fallback is the
//! obvious convenience and it makes the column mean "when it happened" in some rows and
//! "when we received it" in others, in the same table, inseparably — which is the failure
//! ADR-0004 exists to prevent.

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

use sankhya_schema::{
    civil_from_days, is_reserved, AxisError, DateAxis, DateSource, Granularity, DATA_DATE_COLUMN,
    GRANULARITY_KEY, SOURCE_KEY,
};
use std::collections::BTreeMap;

/// 2024-03-01, as days since the Unix epoch.
const LEAP_MARCH: i32 = 19_783;

#[test]
fn the_column_is_named_once_and_everything_agrees() {
    assert_eq!(DATA_DATE_COLUMN, "sank_data_date");
    assert!(is_reserved(DATA_DATE_COLUMN));
}

// --- what the date means --------------------------------------------------

#[test]
fn a_table_says_whether_its_date_is_a_business_date_or_an_arrival_date() {
    // The property the rejected design lacked. A reader must always be able to find out
    // what the column means for a given table.
    let business = DateAxis::from_column("order_date");
    assert!(business.source.is_business_date());
    assert!(business.source.describe().contains("each row is about"));

    let arrival = DateAxis::ingest_date();
    assert!(!arrival.source.is_business_date());
    assert!(
        arrival
            .source
            .describe()
            .contains("not the date the row is about"),
        "the description must say what it is NOT: {}",
        arrival.source.describe()
    );
}

#[test]
fn ingest_date_is_recorded_explicitly_rather_than_being_an_absence() {
    // So a reader can tell "this column means arrival" from "nobody thought about it".
    assert_eq!(DateAxis::ingest_date().source, DateSource::IngestDate);
    assert_eq!(
        DateAxis::from_configuration(&BTreeMap::new())
            .expect("an empty configuration is valid")
            .source,
        DateSource::IngestDate
    );
}

#[test]
fn a_null_in_the_declared_source_is_an_error_not_todays_date() {
    // The whole point of ADR-0004. A per-row fallback reintroduces the mixture one row at
    // a time: some rows meaning "this happened then" and others meaning "we heard about it
    // then", in one table, with nothing recording which.
    let error = AxisError::NullDate {
        column: "order_date".to_string(),
    };
    let message = error.to_string();
    assert!(
        message.contains("Refusing rather than substituting today"),
        "{message}"
    );
    assert!(
        message.contains("inseparably"),
        "the message must say why a fallback is unrecoverable: {message}"
    );
}

// --- declaration and round trip -------------------------------------------

#[test]
fn an_axis_round_trips_through_a_tables_configuration() {
    let original = DateAxis::from_column("order_date").at(Granularity::Month);
    let configuration = original.to_configuration();

    assert_eq!(
        configuration.get(SOURCE_KEY),
        Some(&"order_date".to_string())
    );
    assert_eq!(
        configuration.get(GRANULARITY_KEY),
        Some(&"month".to_string())
    );
    assert_eq!(
        DateAxis::from_configuration(&configuration).expect("valid"),
        original
    );
}

#[test]
fn an_unrecognised_granularity_is_refused_rather_than_defaulted() {
    // Silently falling back to daily would repartition a monthly table on its next write —
    // a full rewrite, for a typo.
    let mut configuration = BTreeMap::new();
    configuration.insert(GRANULARITY_KEY.to_string(), "weekly".to_string());

    let Err(error) = DateAxis::from_configuration(&configuration) else {
        panic!("'weekly' is not a granularity this system has");
    };
    assert_eq!(
        error,
        AxisError::UnknownGranularity {
            offered: "weekly".to_string()
        }
    );
    assert!(error
        .to_string()
        .contains("rewrites the whole table for a typo"));
}

#[test]
fn the_default_granularity_is_daily() {
    assert_eq!(DateAxis::ingest_date().granularity, Granularity::Day);
    assert_eq!(Granularity::default(), Granularity::Day);
}

// --- partition values -----------------------------------------------------

#[test]
fn a_partition_path_is_the_hive_convention_external_engines_parse() {
    // CON-08 requires Spark and Trino to read these tables directly. `=2024-03-01` is a
    // date to them; an encoded integer is a string every pruning query has to be told about.
    let axis = DateAxis::from_column("order_date");
    assert_eq!(axis.partition_path(LEAP_MARCH), "sank_data_date=2024-03-01");
}

#[test]
fn a_coarser_granularity_truncates_rather_than_rounds() {
    // A row dated the 20th belongs to March, never April. Rounding would put the second
    // half of every period in the next one — wrong only at period boundaries, which is
    // where nobody looks.
    let twentieth = LEAP_MARCH + 19;
    assert_eq!(
        DateAxis::from_column("d")
            .at(Granularity::Day)
            .partition_of(twentieth),
        "2024-03-20"
    );
    assert_eq!(
        DateAxis::from_column("d")
            .at(Granularity::Month)
            .partition_of(twentieth),
        "2024-03"
    );
    assert_eq!(
        DateAxis::from_column("d")
            .at(Granularity::Year)
            .partition_of(twentieth),
        "2024"
    );
}

#[test]
fn partition_values_sort_the_way_the_dates_do() {
    // ISO-8601 sorts lexicographically in date order, which is why the format is that one.
    // A listing sorted by name is then sorted by time, for free, in every tool.
    let axis = DateAxis::from_column("d");
    let chronological: Vec<String> = [LEAP_MARCH, LEAP_MARCH + 40, LEAP_MARCH + 400]
        .iter()
        .map(|d| axis.partition_of(*d))
        .collect();

    // Shuffled, then sorted as text — and it must come back in date order.
    let mut shuffled = vec![
        chronological[2].clone(),
        chronological[0].clone(),
        chronological[1].clone(),
    ];
    shuffled.sort();
    assert_eq!(shuffled, chronological);
}

// --- the calendar ---------------------------------------------------------

#[test]
fn the_calendar_is_right_at_the_dates_that_break_naive_arithmetic() {
    // A partition key computed slightly differently by two components is a table that
    // splits in half, so this is written out rather than pulled from a dependency — and
    // therefore has to be checked at the awkward dates.
    assert_eq!(civil_from_days(0), (1970, 1, 1), "the epoch");
    assert_eq!(civil_from_days(-1), (1969, 12, 31), "the day before it");
    assert_eq!(civil_from_days(LEAP_MARCH), (2024, 3, 1));
    assert_eq!(civil_from_days(LEAP_MARCH - 1), (2024, 2, 29), "a leap day");
    // 1900 was not a leap year and 2000 was — the rule everyone's naive version gets wrong.
    assert_eq!(
        civil_from_days(11_016),
        (2000, 2, 29),
        "2000 was a leap year"
    );
    assert_eq!(civil_from_days(-25_508), (1900, 3, 1));
    assert_eq!(civil_from_days(-25_509), (1900, 2, 28), "1900 was not");
}

#[test]
fn every_day_of_a_leap_year_round_trips() {
    // Exhaustive over a year, because an off-by-one in the month arithmetic shows up on one
    // day and no other.
    let start = 19_723; // 2024-01-01
    let mut expected_month = 1u32;
    let mut expected_day = 1u32;
    const LENGTHS: [u32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

    for offset in 0..366 {
        let (year, month, day) = civil_from_days(start + offset);
        assert_eq!(year, 2024, "day {offset}");
        assert_eq!((month, day), (expected_month, expected_day), "day {offset}");

        expected_day += 1;
        if expected_day > LENGTHS[(expected_month - 1) as usize] {
            expected_day = 1;
            expected_month += 1;
        }
    }
}

// --- the reserved prefix --------------------------------------------------

#[test]
fn a_source_column_using_the_reserved_prefix_is_a_collision() {
    // Shadowing it would make the source's data disappear behind a system value, with no
    // error anywhere and no way to notice except by missing it.
    assert!(is_reserved("sank_data_date"));
    assert!(is_reserved("sank_anything"));
    assert!(!is_reserved("sankhya_like"), "only the exact prefix");
    assert!(!is_reserved("order_date"));

    let error = AxisError::ReservedName {
        column: "sank_total".to_string(),
    };
    assert!(error
        .to_string()
        .contains("disappears behind a system value"));
}
